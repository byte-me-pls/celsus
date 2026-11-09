//! Random tokens, hashes, password hashing and encryption at rest.

use std::sync::OnceLock;

use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::{Algorithm, Argon2, Params, Version};
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

/// Bytes from the operating system's secure random source.
pub fn random<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    getrandom::fill(&mut b).expect("the system random source failed");
    b
}

/// URL-safe base64 without padding.
pub fn b64url(data: &[u8]) -> String {
    celsus_core::base64::encode(data)
        .trim_end_matches('=')
        .replace('+', "-")
        .replace('/', "_")
}

/// A fresh random token with 256 bits of entropy, safe in URLs and cookies.
pub fn token() -> String {
    b64url(&random::<32>())
}

pub fn sha256(data: &[u8]) -> Vec<u8> {
    Sha256::digest(data).to_vec()
}

/// Compare secrets without leaking where they differ through timing.
pub fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && bool::from(a.ct_eq(b))
}

/// Argon2id with the OWASP baseline: 19 MiB, 2 passes, 1 lane.
fn argon2() -> Argon2<'static> {
    let params = Params::new(19 * 1024, 2, 1, None).expect("valid Argon2 parameters");
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
}

pub fn hash_password(password: &str) -> String {
    let salt = SaltString::encode_b64(&random::<16>()).expect("16 bytes is a valid salt");
    argon2()
        .hash_password(password.as_bytes(), &salt)
        .expect("hashing cannot fail with valid parameters")
        .to_string()
}

/// Whether `password` matches `hash`, and whether the hash should be redone with the current parameters.
pub fn verify_password(password: &str, hash: &str) -> (bool, bool) {
    let Ok(parsed) = PasswordHash::new(hash) else {
        return (false, false);
    };
    let ok = argon2()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok();
    let current = argon2().params().clone();
    let outdated = parsed.algorithm.as_str() != "argon2id"
        || Params::try_from(&parsed).map_or(true, |p| {
            (p.m_cost(), p.t_cost(), p.p_cost())
                != (current.m_cost(), current.t_cost(), current.p_cost())
        });
    (ok, ok && outdated)
}

/// Check a password against a hash nobody can match, so unknown accounts cost the same time as known ones.
pub fn burn_password_check(password: &str) {
    static DUMMY: OnceLock<String> = OnceLock::new();
    let hash = DUMMY.get_or_init(|| hash_password(&token()));
    let _ = verify_password(password, hash);
}

/// Encryption at rest with the server key (XChaCha20-Poly1305, random 192-bit nonce per value).
pub struct SecretBox {
    cipher: XChaCha20Poly1305,
}

impl SecretBox {
    pub fn new(key: &[u8; 32]) -> Self {
        SecretBox {
            cipher: XChaCha20Poly1305::new(key.into()),
        }
    }

    /// Nonce followed by the ciphertext. `context` binds the value to its place (for example `totp:<user id>`),
    /// so a value copied into another row does not decrypt.
    pub fn seal(&self, plain: &[u8], context: &str) -> Vec<u8> {
        let nonce = random::<24>();
        let ct = self
            .cipher
            .encrypt(
                XNonce::from_slice(&nonce),
                chacha20poly1305::aead::Payload {
                    msg: plain,
                    aad: context.as_bytes(),
                },
            )
            .expect("encryption cannot fail");
        let mut out = nonce.to_vec();
        out.extend_from_slice(&ct);
        out
    }

    pub fn open(&self, sealed: &[u8], context: &str) -> Option<Vec<u8>> {
        if sealed.len() < 24 + 16 {
            return None;
        }
        let (nonce, ct) = sealed.split_at(24);
        self.cipher
            .decrypt(
                XNonce::from_slice(nonce),
                chacha20poly1305::aead::Payload {
                    msg: ct,
                    aad: context.as_bytes(),
                },
            )
            .ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passwords_hash_and_verify() {
        let h = hash_password("correct horse battery");
        assert!(h.starts_with("$argon2id$v=19$m=19456,t=2,p=1$"));
        assert_eq!(verify_password("correct horse battery", &h), (true, false));
        assert_eq!(verify_password("wrong", &h), (false, false));
        assert_ne!(
            h,
            hash_password("correct horse battery"),
            "every hash has its own salt"
        );
        assert_eq!(verify_password("x", "not a hash"), (false, false));
    }

    #[test]
    fn weaker_hashes_are_flagged_for_upgrade() {
        let weak = Argon2::new(
            Algorithm::Argon2id,
            Version::V0x13,
            Params::new(8 * 1024, 1, 1, None).unwrap(),
        );
        let salt = SaltString::encode_b64(&random::<16>()).unwrap();
        let h = weak
            .hash_password(b"pw-long-enough", &salt)
            .unwrap()
            .to_string();
        assert_eq!(verify_password("pw-long-enough", &h), (true, true));
    }

    #[test]
    fn sealed_values_need_the_key_and_the_context() {
        let a = SecretBox::new(&[7; 32]);
        let b = SecretBox::new(&[8; 32]);
        let sealed = a.seal(b"secret", "totp:1");
        assert_eq!(a.open(&sealed, "totp:1").as_deref(), Some(&b"secret"[..]));
        assert!(a.open(&sealed, "totp:2").is_none(), "moved to another row");
        assert!(b.open(&sealed, "totp:1").is_none(), "another key");
        let mut tampered = sealed.clone();
        *tampered.last_mut().unwrap() ^= 1;
        assert!(a.open(&tampered, "totp:1").is_none());
        assert_ne!(a.seal(b"secret", "totp:1"), sealed, "fresh nonce each time");
    }

    #[test]
    fn tokens_are_random_and_url_safe() {
        let t = token();
        assert_eq!(t.len(), 43);
        assert!(
            t.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        );
        assert_ne!(t, token());
        assert!(same(b"abc", b"abc") && !same(b"abc", b"abd") && !same(b"abc", b"ab"));
    }
}
