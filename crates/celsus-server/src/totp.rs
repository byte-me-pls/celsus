//! Time-based one-time codes (RFC 6238) for two-step sign-in, as authenticator apps make them: HMAC-SHA1,
//! 30-second steps, 6 digits.

use hmac::{Hmac, Mac};
use sha1::Sha1;

pub const STEP: i64 = 30;

/// A new 160-bit secret.
pub fn new_secret() -> [u8; 20] {
    crate::crypto::random()
}

/// RFC 4648 base32 without padding, as authenticator apps expect.
pub fn base32(data: &[u8]) -> String {
    const A: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    let mut out = String::new();
    let (mut acc, mut bits) = (0u32, 0);
    for &b in data {
        acc = (acc << 8) | u32::from(b);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(A[((acc >> bits) & 31) as usize] as char);
        }
    }
    if bits > 0 {
        out.push(A[((acc << (5 - bits)) & 31) as usize] as char);
    }
    out
}

/// The link an authenticator app reads from a QR code.
pub fn otpauth_url(secret: &[u8], account: &str) -> String {
    let label: String = account
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'.' | b'-' | b'_' | b'@' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect();
    format!(
        "otpauth://totp/Celsus:{label}?secret={}&issuer=Celsus&algorithm=SHA1&digits=6&period=30",
        base32(secret)
    )
}

/// The setup link as a QR code, an SVG image an authenticator app can scan.
pub fn qr_svg(text: &str) -> String {
    use qrcodegen::{QrCode, QrCodeEcc};
    let Ok(qr) = QrCode::encode_text(text, QrCodeEcc::Medium) else {
        return String::new();
    };
    let size = qr.size();
    let border = 4;
    let mut path = String::new();
    for y in 0..size {
        for x in 0..size {
            if qr.get_module(x, y) {
                path.push_str(&format!("M{},{}h1v1h-1z", x + border, y + border));
            }
        }
    }
    let full = size + border * 2;
    format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {full} {full}" shape-rendering="crispEdges"><rect width="100%" height="100%" fill="#fff"/><path d="{path}" fill="#000"/></svg>"##
    )
}

/// The code for a time step, with `digits` digits.
pub fn code(secret: &[u8], step: i64, digits: u32) -> u32 {
    let mut mac = Hmac::<Sha1>::new_from_slice(secret).expect("any key length works");
    mac.update(&step.to_be_bytes());
    let h = mac.finalize().into_bytes();
    let offset = (h[19] & 0x0f) as usize;
    let bin = u32::from_be_bytes([
        h[offset] & 0x7f,
        h[offset + 1],
        h[offset + 2],
        h[offset + 3],
    ]);
    bin % 10u32.pow(digits)
}

/// The step a typed code belongs to, if it is valid now (one step of clock drift either way) and newer than
/// `last_used`, so a code works once only.
pub fn verify(secret: &[u8], typed: &str, now: i64, last_used: Option<i64>) -> Option<i64> {
    let typed: String = typed.chars().filter(|c| !c.is_whitespace()).collect();
    if typed.len() != 6 || !typed.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let value: u32 = typed.parse().ok()?;
    let current = now.div_euclid(STEP);
    let mut found = None;
    // Check every candidate so the time taken does not depend on which one matched.
    for step in [current - 1, current, current + 1] {
        let ok = crate::crypto::same(&code(secret, step, 6).to_be_bytes(), &value.to_be_bytes());
        if ok && last_used.is_none_or(|l| step > l) && found.is_none() {
            found = Some(step);
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    const RFC: &[u8] = b"12345678901234567890";

    #[test]
    fn matches_the_rfc_6238_test_vectors() {
        for (t, expected) in [
            (59, 94287082),
            (1111111109, 7081804),
            (1111111111, 14050471),
            (1234567890, 89005924),
            (2000000000, 69279037),
        ] {
            assert_eq!(code(RFC, t / STEP, 8), expected, "t={t}");
        }
    }

    #[test]
    fn accepts_drift_of_one_step_and_each_code_once() {
        let now = 1_111_111_109;
        let c = format!("{:06}", code(RFC, now / STEP, 6));
        let step = verify(RFC, &c, now, None).unwrap();
        assert_eq!(step, now / STEP);
        assert_eq!(verify(RFC, &c, now, Some(step)), None, "replay");
        let prev = format!("{:06}", code(RFC, now / STEP - 1, 6));
        assert!(
            verify(RFC, &prev, now, None).is_some(),
            "30 seconds late is fine"
        );
        let old = format!("{:06}", code(RFC, now / STEP - 2, 6));
        assert!(
            verify(RFC, &old, now, None).is_none(),
            "a minute late is not"
        );
        assert!(verify(RFC, "12345", now, None).is_none());
        assert!(verify(RFC, "abcdef", now, None).is_none());
        assert!(
            verify(RFC, &format!("{} {}", &c[..3], &c[3..]), now, None).is_some(),
            "spaces are ignored"
        );
    }

    #[test]
    fn qr_codes_are_svg() {
        let svg = qr_svg("otpauth://totp/Celsus:a@b.c?secret=ABC");
        assert!(svg.starts_with("<svg") && svg.contains(r#"<path d="M"#));
    }

    #[test]
    fn base32_and_link() {
        assert_eq!(base32(b"foobar"), "MZXW6YTBOI");
        assert_eq!(base32(b"f"), "MY");
        let url = otpauth_url(b"12345678901234567890", "ali veli@example.com");
        assert!(url.starts_with("otpauth://totp/Celsus:ali%20veli@example.com?secret=GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ&issuer=Celsus"));
    }
}
