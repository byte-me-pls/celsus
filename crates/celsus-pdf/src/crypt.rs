//! Opening files encrypted with the standard security handler.
//!
//! Many books are encrypted with only an owner password: it asks readers not to allow printing or copying, but
//! the password needed to open the file is empty and every reader opens it. Such a file is decrypted as its
//! objects are read, so the rest of the library only ever sees plain objects, and a copy saved from it is written
//! without encryption. A file that needs a password to open is refused with [`Error::PasswordProtected`].
//!
//! Supported: RC4 with 40 to 128 bit keys (`/V 1` and `2`, revisions 2 and 3), crypt filters with RC4 or AES-128
//! (`/V 4`, revision 4) and AES-256 (`/V 5`, revisions 5 and 6).

use aes::cipher::generic_array::GenericArray;
use aes::cipher::{BlockDecrypt, BlockEncrypt, KeyInit};
use aes::{Aes128, Aes192, Aes256};
use md5::{Digest, Md5};
use sha2::{Sha256, Sha384, Sha512};

use crate::object::{Dict, ObjRef, Object, Stream, get};
use crate::{Error, Result};

/// Pads a password to 32 bytes for revisions 2 to 4.
const PADDING: [u8; 32] = [
    0x28, 0xBF, 0x4E, 0x5E, 0x4E, 0x75, 0x8A, 0x41, 0x64, 0x00, 0x4E, 0x56, 0xFF, 0xFA, 0x01, 0x08,
    0x2E, 0x2E, 0x00, 0xB6, 0xD0, 0x68, 0x3E, 0x80, 0x2F, 0x0C, 0xA9, 0xFE, 0x64, 0x53, 0x69, 0x7A,
];

/// How strings or streams are encrypted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Method {
    /// Not encrypted at all.
    Identity,
    Rc4,
    Aes128,
    Aes256,
}

/// The file key and the methods an encrypted file uses.
pub(crate) struct Crypt {
    key: Vec<u8>,
    strings: Method,
    streams: Method,
    /// `false` when the document's XMP metadata stream was left unencrypted.
    encrypt_metadata: bool,
    /// The encryption dictionary's own object, which is never encrypted.
    dict_num: Option<u32>,
}

impl Crypt {
    /// Work out the file key from the encryption dictionary (its references already resolved) with the empty
    /// user password, or with the empty owner password if that is what the file was given instead.
    pub(crate) fn open(encrypt: &Dict, dict_num: Option<u32>, id0: &[u8]) -> Result<Self> {
        let handler = get(encrypt, "Filter").and_then(Object::as_name);
        if handler != Some(b"Standard") {
            return Err(Error::Encrypted);
        }
        let int = |key: &str| get(encrypt, key).and_then(Object::as_i64);
        let bytes = |key: &str| {
            get(encrypt, key)
                .and_then(Object::as_str_bytes)
                .unwrap_or_default()
        };
        let v = int("V").unwrap_or(0);
        let r = int("R").unwrap_or(2);
        let encrypt_metadata =
            !matches!(get(encrypt, "EncryptMetadata"), Some(Object::Bool(false)));
        // The key length in bytes; `/Length` gives it in bits.
        let given = int("Length")
            .and_then(|n| usize::try_from(n / 8).ok())
            .filter(|n| *n > 0);
        let (strings, streams, length) = match v {
            1 => (Method::Rc4, Method::Rc4, 5),
            2 | 3 => (Method::Rc4, Method::Rc4, given.unwrap_or(5)),
            4 | 5 => {
                let filters = get(encrypt, "CF").and_then(Object::as_dict);
                let method = |key: &str| -> Result<(Method, Option<usize>)> {
                    let name = get(encrypt, key)
                        .and_then(Object::as_name)
                        .unwrap_or(b"Identity");
                    if name == b"Identity" {
                        return Ok((Method::Identity, None));
                    }
                    let filter = filters
                        .and_then(|f| f.get(name))
                        .and_then(Object::as_dict)
                        .ok_or_else(|| Error::Corrupt("missing crypt filter".into()))?;
                    // Writers disagree on whether a crypt filter's length is in bits or bytes.
                    let length = get(filter, "Length")
                        .and_then(Object::as_i64)
                        .and_then(|n| usize::try_from(n).ok())
                        .map(|n| if n > 32 { n / 8 } else { n });
                    let method = match get(filter, "CFM").and_then(Object::as_name) {
                        None | Some(b"None") => Method::Identity,
                        Some(b"V2") => Method::Rc4,
                        Some(b"AESV2") => Method::Aes128,
                        Some(b"AESV3") => Method::Aes256,
                        Some(other) => {
                            return Err(Error::Unsupported(format!(
                                "crypt filter {}",
                                String::from_utf8_lossy(other)
                            )));
                        }
                    };
                    Ok((method, length))
                };
                let (strings, a) = method("StrF")?;
                let (streams, b) = method("StmF")?;
                let length = if strings == Method::Aes128 || streams == Method::Aes128 {
                    16
                } else {
                    given.or(a).or(b).unwrap_or(16)
                };
                (strings, streams, length)
            }
            _ => return Err(Error::Unsupported(format!("encryption version {v}"))),
        };
        let length = length.clamp(5, 16);

        let o = bytes("O");
        let u = bytes("U");
        let key = match r {
            2..=4 => {
                let p = int("P").unwrap_or(0) as u32;
                legacy_key(o, u, p, id0, r, length, encrypt_metadata)?
            }
            5 | 6 => modern_key(encrypt, o, u, r)?,
            _ => return Err(Error::Unsupported(format!("encryption revision {r}"))),
        };
        Ok(Crypt {
            key,
            strings,
            streams,
            encrypt_metadata,
            dict_num,
        })
    }

    /// Decrypt the strings and stream data of an object read from the file. Objects inside an object stream are
    /// not passed here: the object stream itself was decrypted as a whole.
    pub(crate) fn decrypt_object(&self, id: ObjRef, obj: Object) -> Object {
        if Some(id.num) == self.dict_num {
            return obj;
        }
        match obj {
            Object::Stream(stream) => self.decrypt_stream(id, stream),
            other => self.decrypt_strings(id, other),
        }
    }

    fn decrypt_stream(&self, id: ObjRef, stream: Stream) -> Object {
        let kind = get(&stream.dict, "Type").and_then(Object::as_name);
        // Cross-reference streams are never encrypted, so a reader can find the objects before decrypting them.
        if kind == Some(b"XRef") {
            return Object::Stream(stream);
        }
        let mut method = self.streams;
        if kind == Some(b"Metadata") && !self.encrypt_metadata {
            method = Method::Identity;
        }
        let Stream { dict, data } = stream;
        let mut dict = match self.decrypt_strings(id, Object::Dict(dict)) {
            Object::Dict(d) => d,
            _ => unreachable!(),
        };
        // A stream can name its own crypt filter as its first filter; it is taken out, since the copy that is
        // saved is not encrypted. Only the Identity filter differs from the file's default for streams.
        if let Some(name) = take_crypt_filter(&mut dict)
            && name == b"Identity"
        {
            method = Method::Identity;
        }
        let data = self.decrypt_bytes(method, id, &data);
        Object::Stream(Stream { dict, data })
    }

    fn decrypt_strings(&self, id: ObjRef, obj: Object) -> Object {
        match obj {
            Object::String(s) => Object::String(self.decrypt_bytes(self.strings, id, &s)),
            Object::Array(a) => {
                Object::Array(a.into_iter().map(|o| self.decrypt_strings(id, o)).collect())
            }
            Object::Dict(d) => {
                // The signature value in a signature dictionary is stored in the clear.
                let signature = matches!(
                    get(&d, "Type").and_then(Object::as_name),
                    Some(b"Sig" | b"DocTimeStamp")
                );
                Object::Dict(
                    d.into_iter()
                        .map(|(k, v)| {
                            if signature && k == b"Contents" {
                                (k, v)
                            } else {
                                (k, self.decrypt_strings(id, v))
                            }
                        })
                        .collect(),
                )
            }
            other => other,
        }
    }

    fn decrypt_bytes(&self, method: Method, id: ObjRef, data: &[u8]) -> Vec<u8> {
        match method {
            Method::Identity => data.to_vec(),
            Method::Rc4 => rc4(&self.object_key(id, false), data),
            Method::Aes128 => aes_cbc_decrypt(&self.object_key(id, true), data),
            Method::Aes256 => aes_cbc_decrypt(&self.key, data),
        }
    }

    /// The key for one object: the file key mixed with the object's number and generation.
    fn object_key(&self, id: ObjRef, aes: bool) -> Vec<u8> {
        let mut h = Md5::new();
        h.update(&self.key);
        h.update(&id.num.to_le_bytes()[..3]);
        h.update(id.generation.to_le_bytes());
        if aes {
            h.update(b"sAlT");
        }
        let mut key = h.finalize().to_vec();
        key.truncate((self.key.len() + 5).min(16));
        key
    }
}

/// Remove a leading `/Crypt` filter from a stream dictionary and return the crypt filter it names.
fn take_crypt_filter(dict: &mut Dict) -> Option<Vec<u8>> {
    let name_of = |params: Option<&Object>| {
        params
            .and_then(Object::as_dict)
            .and_then(|p| get(p, "Name"))
            .and_then(Object::as_name)
            .unwrap_or(b"Identity")
            .to_vec()
    };
    match dict.get(&b"Filter"[..]) {
        Some(Object::Name(n)) if n == b"Crypt" => {
            let name = name_of(dict.get(&b"DecodeParms"[..]));
            dict.remove(&b"Filter"[..]);
            dict.remove(&b"DecodeParms"[..]);
            Some(name)
        }
        Some(Object::Array(a)) if a.first().and_then(Object::as_name) == Some(b"Crypt") => {
            let mut filters = a.clone();
            filters.remove(0);
            let name = match dict.get_mut(&b"DecodeParms"[..]) {
                Some(Object::Array(p)) if !p.is_empty() => {
                    let first = p.remove(0);
                    name_of(Some(&first))
                }
                _ => b"Identity".to_vec(),
            };
            dict.insert(b"Filter".to_vec(), Object::Array(filters));
            Some(name)
        }
        _ => None,
    }
}

/// The file key for revisions 2 to 4, trying the empty password first as the user password and then as the
/// owner password.
fn legacy_key(
    o: &[u8],
    u: &[u8],
    p: u32,
    id0: &[u8],
    r: i64,
    length: usize,
    encrypt_metadata: bool,
) -> Result<Vec<u8>> {
    if o.len() < 32 || u.len() < 16 {
        return Err(Error::Corrupt("encryption dictionary".into()));
    }
    let key = legacy_user_key(b"", o, p, id0, r, length, encrypt_metadata);
    if legacy_user_value(&key, r, id0)[..16] == u[..16] {
        return Ok(key);
    }
    // The empty password may be the owner password: the user password is then recovered from /O.
    let user = legacy_owner_unlock(b"", o, r, length);
    let key = legacy_user_key(&user, o, p, id0, r, length, encrypt_metadata);
    if legacy_user_value(&key, r, id0)[..16] == u[..16] {
        return Ok(key);
    }
    Err(Error::PasswordProtected)
}

fn padded(password: &[u8]) -> [u8; 32] {
    let mut out = PADDING;
    let n = password.len().min(32);
    out[..n].copy_from_slice(&password[..n]);
    out[n..].copy_from_slice(&PADDING[..32 - n]);
    out
}

/// Algorithm 2 of the specification: the file key from a user password.
fn legacy_user_key(
    password: &[u8],
    o: &[u8],
    p: u32,
    id0: &[u8],
    r: i64,
    length: usize,
    encrypt_metadata: bool,
) -> Vec<u8> {
    let mut h = Md5::new();
    h.update(padded(password));
    h.update(&o[..32]);
    h.update(p.to_le_bytes());
    h.update(id0);
    if r >= 4 && !encrypt_metadata {
        h.update([0xFF; 4]);
    }
    let mut key = h.finalize().to_vec();
    if r >= 3 {
        for _ in 0..50 {
            key = Md5::digest(&key[..length]).to_vec();
        }
    }
    key.truncate(length);
    key
}

/// Algorithms 4 and 5: the `/U` value a file key produces (only its first 16 bytes count from revision 3).
fn legacy_user_value(key: &[u8], r: i64, id0: &[u8]) -> Vec<u8> {
    if r == 2 {
        return rc4(key, &PADDING);
    }
    let mut h = Md5::new();
    h.update(PADDING);
    h.update(id0);
    let mut value = rc4(key, &h.finalize());
    for i in 1..=19u8 {
        let round: Vec<u8> = key.iter().map(|b| b ^ i).collect();
        value = rc4(&round, &value);
    }
    value
}

/// The key that `/O` is encrypted with, from an owner password (algorithm 3, steps a to d).
fn legacy_owner_key(password: &[u8], r: i64, length: usize) -> Vec<u8> {
    let mut key = Md5::digest(padded(password)).to_vec();
    if r >= 3 {
        for _ in 0..50 {
            key = Md5::digest(&key).to_vec();
        }
    }
    key.truncate(if r == 2 { 5 } else { length });
    key
}

/// Algorithm 7: the user password, recovered from `/O` with the owner password.
fn legacy_owner_unlock(password: &[u8], o: &[u8], r: i64, length: usize) -> Vec<u8> {
    let key = legacy_owner_key(password, r, length);
    if r == 2 {
        return rc4(&key, &o[..32]);
    }
    let mut value = o[..32].to_vec();
    for i in (0..=19u8).rev() {
        let round: Vec<u8> = key.iter().map(|b| b ^ i).collect();
        value = rc4(&round, &value);
    }
    value
}

/// The file key for revisions 5 and 6 (AES-256), with the empty password as user password or else as owner
/// password.
fn modern_key(encrypt: &Dict, o: &[u8], u: &[u8], r: i64) -> Result<Vec<u8>> {
    let bytes = |key: &str| {
        get(encrypt, key)
            .and_then(Object::as_str_bytes)
            .unwrap_or_default()
    };
    if o.len() < 48 || u.len() < 48 {
        return Err(Error::Corrupt("encryption dictionary".into()));
    }
    let (ue, oe) = (bytes("UE"), bytes("OE"));
    if modern_hash(b"", &u[32..40], &[], r)[..] == u[..32] && ue.len() >= 32 {
        let k = modern_hash(b"", &u[40..48], &[], r);
        return Ok(aes256_cbc_decrypt_raw(&k, &ue[..32]));
    }
    if modern_hash(b"", &o[32..40], &u[..48], r)[..] == o[..32] && oe.len() >= 32 {
        let k = modern_hash(b"", &o[40..48], &u[..48], r);
        return Ok(aes256_cbc_decrypt_raw(&k, &oe[..32]));
    }
    Err(Error::PasswordProtected)
}

/// Algorithm 2.A's hash (revision 5) and algorithm 2.B (revision 6).
fn modern_hash(password: &[u8], salt: &[u8], user: &[u8], r: i64) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(password);
    h.update(salt);
    h.update(user);
    let first: [u8; 32] = h.finalize().into();
    if r == 5 {
        return first;
    }
    let mut k = first.to_vec();
    let mut round = 0u32;
    loop {
        let mut block = Vec::with_capacity(64 * (password.len() + k.len() + user.len()));
        for _ in 0..64 {
            block.extend_from_slice(password);
            block.extend_from_slice(&k);
            block.extend_from_slice(user);
        }
        let e = aes128_cbc_encrypt_raw(&k[..16], &k[16..32], &block);
        let choice = e[..16].iter().map(|&b| u32::from(b)).sum::<u32>() % 3;
        k = match choice {
            0 => Sha256::digest(&e).to_vec(),
            1 => Sha384::digest(&e).to_vec(),
            _ => Sha512::digest(&e).to_vec(),
        };
        round += 1;
        if round >= 64 && u32::from(*e.last().unwrap()) <= round - 32 {
            break;
        }
    }
    let mut out = [0; 32];
    out.copy_from_slice(&k[..32]);
    out
}

pub(crate) fn rc4(key: &[u8], data: &[u8]) -> Vec<u8> {
    if key.is_empty() {
        return data.to_vec();
    }
    let mut s: [u8; 256] = std::array::from_fn(|i| i as u8);
    let mut j = 0u8;
    for i in 0..256 {
        j = j.wrapping_add(s[i]).wrapping_add(key[i % key.len()]);
        s.swap(i, j as usize);
    }
    let (mut i, mut j) = (0u8, 0u8);
    data.iter()
        .map(|&b| {
            i = i.wrapping_add(1);
            j = j.wrapping_add(s[i as usize]);
            s.swap(i as usize, j as usize);
            b ^ s[s[i as usize].wrapping_add(s[j as usize]) as usize]
        })
        .collect()
}

/// One AES block cipher, by key length.
enum Aes {
    K128(Box<Aes128>),
    K192(Box<Aes192>),
    K256(Box<Aes256>),
}

impl Aes {
    fn new(key: &[u8]) -> Option<Self> {
        Some(match key.len() {
            16 => Aes::K128(Box::new(Aes128::new_from_slice(key).ok()?)),
            24 => Aes::K192(Box::new(Aes192::new_from_slice(key).ok()?)),
            32 => Aes::K256(Box::new(Aes256::new_from_slice(key).ok()?)),
            _ => return None,
        })
    }

    fn decrypt(&self, block: &mut [u8]) {
        let block = GenericArray::from_mut_slice(block);
        match self {
            Aes::K128(c) => c.decrypt_block(block),
            Aes::K192(c) => c.decrypt_block(block),
            Aes::K256(c) => c.decrypt_block(block),
        }
    }

    fn encrypt(&self, block: &mut [u8]) {
        let block = GenericArray::from_mut_slice(block);
        match self {
            Aes::K128(c) => c.encrypt_block(block),
            Aes::K192(c) => c.encrypt_block(block),
            Aes::K256(c) => c.encrypt_block(block),
        }
    }
}

/// Decrypt AES in CBC mode as PDF stores it: a 16-byte initialisation vector, then the data with PKCS#7
/// padding. Data too short to hold a vector decrypts to nothing.
pub(crate) fn aes_cbc_decrypt(key: &[u8], data: &[u8]) -> Vec<u8> {
    let Some(cipher) = Aes::new(key) else {
        return Vec::new();
    };
    if data.len() < 16 {
        return Vec::new();
    }
    let (iv, body) = data.split_at(16);
    let mut out = cbc_decrypt(&cipher, iv, body);
    if let Some(&pad) = out.last() {
        let pad = usize::from(pad);
        if (1..=16).contains(&pad)
            && pad <= out.len()
            && out[out.len() - pad..]
                .iter()
                .all(|&b| usize::from(b) == pad)
        {
            out.truncate(out.len() - pad);
        }
    }
    out
}

/// CBC decryption of whole blocks; a broken last partial block is dropped.
fn cbc_decrypt(cipher: &Aes, iv: &[u8], body: &[u8]) -> Vec<u8> {
    let mut previous = [0u8; 16];
    previous.copy_from_slice(&iv[..16]);
    let mut out = Vec::with_capacity(body.len());
    for chunk in body.chunks_exact(16) {
        let mut block = [0u8; 16];
        block.copy_from_slice(chunk);
        cipher.decrypt(&mut block);
        for (b, p) in block.iter_mut().zip(previous) {
            *b ^= p;
        }
        out.extend_from_slice(&block);
        previous.copy_from_slice(chunk);
    }
    out
}

/// AES-256 in CBC mode with a zero vector and no padding, as `/UE` and `/OE` are stored.
fn aes256_cbc_decrypt_raw(key: &[u8], data: &[u8]) -> Vec<u8> {
    match Aes::new(key) {
        Some(cipher) => cbc_decrypt(&cipher, &[0; 16], data),
        None => Vec::new(),
    }
}

/// AES-128 in CBC mode with no padding, used by the revision 6 hash.
fn aes128_cbc_encrypt_raw(key: &[u8], iv: &[u8], data: &[u8]) -> Vec<u8> {
    let Some(cipher) = Aes::new(key) else {
        return Vec::new();
    };
    cbc_encrypt(&cipher, iv, data)
}

fn cbc_encrypt(cipher: &Aes, iv: &[u8], data: &[u8]) -> Vec<u8> {
    let mut previous = [0u8; 16];
    previous.copy_from_slice(&iv[..16]);
    let mut out = Vec::with_capacity(data.len());
    for chunk in data.chunks_exact(16) {
        let mut block = [0u8; 16];
        for (b, (c, p)) in block.iter_mut().zip(chunk.iter().zip(previous)) {
            *b = c ^ p;
        }
        cipher.encrypt(&mut block);
        out.extend_from_slice(&block);
        previous = block;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::Lexer;
    use crate::write::{Saver, serialize};
    use crate::{Document, interp};

    fn dict(text: &str) -> Dict {
        Lexer::new(text.as_bytes())
            .parse_object(true)
            .unwrap()
            .as_dict()
            .unwrap()
            .clone()
    }

    fn hex(text: &str) -> Vec<u8> {
        (0..text.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
            .collect()
    }

    fn hex_string(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02X}")).collect()
    }

    // Encryption dictionaries of files saved by another PDF library with an owner password and no user
    // password, so that the key derivation is checked against files this code did not write.

    #[test]
    fn opens_rc4_files_written_elsewhere() {
        let rc4_40 = dict(
            "<</Filter/Standard/R 2/V 1/Length 40/P -4/EncryptMetadata true\
             /O<D8A81F2F756B03F0E776376607DC67C098531AC3E51F6E5CF0FC1473B1CE0975>\
             /U<FF3B3B2CBC068719326931B5A860EFC836106059B35185D48E6EF518B3BB4727>>>",
        );
        let crypt = Crypt::open(&rc4_40, None, &hex("3B091FC2826DC38BC38EC2AF271DC3BD")).unwrap();
        assert_eq!((crypt.key.len(), crypt.strings), (5, Method::Rc4));

        let rc4_128 = dict(
            "<</Filter/Standard/R 3/V 2/Length 128/P -4/EncryptMetadata true\
             /O<77B8FB098022D3AB34237EA5643C08710EA5123FC5F88BF993A68CCA5F12B40F>\
             /U<DAA254352B60017C1BBCCC64E2ADA6FD28BF4E5E4E758A4164004E56FFFA0108>>>",
        );
        let crypt = Crypt::open(&rc4_128, None, &hex("1A5803150A1233C2AE08C2A6C38F38C3")).unwrap();
        assert_eq!((crypt.key.len(), crypt.streams), (16, Method::Rc4));
        // The same dictionary with another file's /ID does not open.
        assert!(matches!(
            Crypt::open(&rc4_128, None, &hex("3B091FC2826DC38BC38EC2AF271DC3BD")),
            Err(Error::PasswordProtected)
        ));
    }

    #[test]
    fn opens_aes_files_written_elsewhere() {
        let aes_128 = dict(
            "<</Filter/Standard/R 4/V 4/Length 128/P -4/EncryptMetadata true/StmF/StdCF/StrF/StdCF\
             /CF<</StdCF<</AuthEvent/DocOpen/CFM/AESV2/Length 16>>>>\
             /O<77B8FB098022D3AB34237EA5643C08710EA5123FC5F88BF993A68CCA5F12B40F>\
             /U<13CA32CCF6956DA987360BA4F8C10C8628BF4E5E4E758A4164004E56FFFA0108>>>",
        );
        let crypt = Crypt::open(&aes_128, None, &hex("C3AC4C64C2AB571C3760C380580B11C2")).unwrap();
        assert_eq!((crypt.key.len(), crypt.streams), (16, Method::Aes128));

        let aes_256 = dict(
            "<</Filter/Standard/R 6/V 5/Length 256/P -4/EncryptMetadata true/StmF/StdCF/StrF/StdCF\
             /CF<</StdCF<</AuthEvent/DocOpen/CFM/AESV3/Length 32>>>>\
             /O<D9967CFFEF71A8DA87C5DB4A04A3E58BEA6DF66506B1062CFA0EDA40988AF8E682F8594E7D253BA69096B2CCAF393ED8>\
             /U<C50E2869E3F5628A6D0FA2623836DEA23730BD4A4D8E78AD2C68E45630656CEB5B5F07971642F4C0162CE4F90A076E81>\
             /OE<83438105595869818382F1BA51C6AD1C0BA28020D5AE3C02C8EE3F1C3AF367E5>\
             /UE<2433FC87B14530C9924511A2D117CEFDAE54586FA19FD2AC59112C4E2C5CBC2E>\
             /Perms<DAC6F5AF9F3AF9A75755C6D93D720380>>>",
        );
        let crypt = Crypt::open(&aes_256, None, &[]).unwrap();
        assert_eq!(crypt.strings, Method::Aes256);
        // /Perms is the permissions encrypted with the file key; bytes 9 to 11 read "adb" when the key is right.
        let mut perms = hex("DAC6F5AF9F3AF9A75755C6D93D720380");
        Aes::new(&crypt.key).unwrap().decrypt(&mut perms);
        assert_eq!(&perms[9..12], b"adb");
    }

    #[test]
    fn files_that_cannot_be_opened_say_why() {
        let user_password = dict(
            "<</Filter/Standard/R 3/V 2/Length 128/P -4/EncryptMetadata true\
             /O<2A2F0A1990192C60114730BDCD39F37828A53C89A340DD473C85299DC5258E1C>\
             /U<5DF1EAFABD82E28D2CF5EFAF0262D77C28BF4E5E4E758A4164004E56FFFA0108>>>",
        );
        assert!(matches!(
            Crypt::open(
                &user_password,
                None,
                &hex("C391C29EC3B1793EC3B9756D31C39CC2")
            ),
            Err(Error::PasswordProtected)
        ));
        assert!(matches!(
            Crypt::open(&dict("<</Filter/FOPN_fLock/V 1>>"), None, &[]),
            Err(Error::Encrypted)
        ));
    }

    #[test]
    fn rc4_and_aes_known_answers() {
        // The RC4 test vector from its original publication.
        assert_eq!(rc4(b"Key", b"Plaintext"), hex("BBF316E8D940AF0AD3"));
        // AES-128 from FIPS-197, appendix C.1, as one CBC block with a zero vector.
        let key = hex("000102030405060708090A0B0C0D0E0F");
        let plain = hex("00112233445566778899AABBCCDDEEFF");
        let cipher = hex("69C4E0D86A7B0430D8CDB78070B4C55A");
        assert_eq!(aes128_cbc_encrypt_raw(&key, &[0; 16], &plain), cipher);
        let mut stored = vec![0; 16];
        stored.extend_from_slice(&cipher);
        // Without valid padding the data is kept whole.
        assert_eq!(aes_cbc_decrypt(&key, &stored), plain);
        assert!(aes_cbc_decrypt(&key, b"short").is_empty());
    }

    /// How a test file is locked.
    #[derive(Clone, Copy)]
    enum Lock {
        Rc4 { r: i64, bits: usize },
        Aes128 { encrypt_metadata: bool },
        Aes256 { r: i64 },
    }

    const ID: &[u8] = b"celsus-test-id-0";

    fn crypt(key: Vec<u8>, method: Method, encrypt_metadata: bool) -> Crypt {
        Crypt {
            key,
            strings: method,
            streams: method,
            encrypt_metadata,
            dict_num: None,
        }
    }

    /// The encryption dictionary for a lock with `password` as the user password, and the key it makes.
    fn lock(how: Lock, password: &[u8]) -> (String, Crypt) {
        let (r, bits, aes, encrypt_metadata) = match how {
            Lock::Rc4 { r, bits } => (r, bits, false, true),
            Lock::Aes128 { encrypt_metadata } => (4, 128, true, encrypt_metadata),
            Lock::Aes256 { r } => {
                let key = [0x5Au8; 32];
                let (validation, salt) = ([1u8; 8], [2u8; 8]);
                let mut u = modern_hash(password, &validation, &[], r).to_vec();
                u.extend_from_slice(&validation);
                u.extend_from_slice(&salt);
                let wrap = modern_hash(password, &salt, &[], r);
                let ue = cbc_encrypt(&Aes::new(&wrap).unwrap(), &[0; 16], &key);
                let text = format!(
                    "<< /Filter /Standard /V 5 /R {r} /Length 256 /P -4 /O <{}> /U <{}> /OE <{}> /UE <{}> \
                     /CF << /StdCF << /CFM /AESV3 /Length 32 >> >> /StmF /StdCF /StrF /StdCF >>",
                    hex_string(&[0x22; 48]),
                    hex_string(&u),
                    hex_string(&[0; 32]),
                    hex_string(&ue)
                );
                return (text, crypt(key.to_vec(), Method::Aes256, true));
            }
        };
        let o = [0x11u8; 32];
        let key = legacy_user_key(
            password,
            &o,
            -4i32 as u32,
            ID,
            r,
            bits / 8,
            encrypt_metadata,
        );
        let mut u = legacy_user_value(&key, r, ID);
        u.resize(32, 0);
        let v = match (r, aes) {
            (2, _) => 1,
            (_, false) => 2,
            (_, true) => 4,
        };
        let mut text = format!(
            "<< /Filter /Standard /V {v} /R {r} /Length {bits} /P -4 /O <{}> /U <{}>",
            hex_string(&o),
            hex_string(&u)
        );
        if aes {
            text.push_str(&format!(
                " /CF << /StdCF << /CFM /AESV2 /Length 16 >> >> /StmF /StdCF /StrF /StdCF \
                 /EncryptMetadata {encrypt_metadata}"
            ));
        }
        text.push_str(" >>");
        let method = if aes { Method::Aes128 } else { Method::Rc4 };
        (text, crypt(key, method, encrypt_metadata))
    }

    impl Crypt {
        fn encrypt_bytes(&self, method: Method, id: ObjRef, data: &[u8]) -> Vec<u8> {
            let aes = |key: &[u8]| {
                let mut padded = data.to_vec();
                let pad = 16 - padded.len() % 16;
                padded.extend(std::iter::repeat_n(pad as u8, pad));
                let iv = [3u8; 16];
                let mut out = iv.to_vec();
                out.extend(cbc_encrypt(&Aes::new(key).unwrap(), &iv, &padded));
                out
            };
            match method {
                Method::Identity => data.to_vec(),
                Method::Rc4 => rc4(&self.object_key(id, false), data),
                Method::Aes128 => aes(&self.object_key(id, true)),
                Method::Aes256 => aes(&self.key),
            }
        }

        fn encrypt_object(&self, id: ObjRef, obj: Object) -> Object {
            match obj {
                Object::String(s) => Object::String(self.encrypt_bytes(self.strings, id, &s)),
                Object::Array(a) => {
                    Object::Array(a.into_iter().map(|o| self.encrypt_object(id, o)).collect())
                }
                Object::Dict(d) => Object::Dict(
                    d.into_iter()
                        .map(|(k, v)| (k, self.encrypt_object(id, v)))
                        .collect(),
                ),
                Object::Stream(s) => {
                    let plain = get(&s.dict, "Type").and_then(Object::as_name) == Some(b"Metadata")
                        && !self.encrypt_metadata;
                    let method = if plain {
                        Method::Identity
                    } else {
                        self.streams
                    };
                    let data = self.encrypt_bytes(method, id, &s.data);
                    let Object::Dict(dict) = self.encrypt_object(id, Object::Dict(s.dict)) else {
                        unreachable!()
                    };
                    Object::Stream(Stream { dict, data })
                }
                other => other,
            }
        }
    }

    const TEXT: &str = "Hello encrypted world";
    const XMP: &[u8] = b"<x:xmpmeta xmlns:x='adobe:ns:meta/'/>";

    /// A one-page book encrypted with `crypt`. With `packed`, the catalog, page tree and page sit in an object
    /// stream and the file has no cross-reference table, so it has to be rebuilt.
    fn book(encrypt: &str, crypt: &Crypt, packed: bool) -> Vec<u8> {
        let content = format!("BT /F1 12 Tf 72 720 Td ({TEXT}) Tj ET");
        let objects: Vec<(u32, Object)> = [
            (1, "<< /Type /Catalog /Pages 2 0 R /Metadata 6 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R \
                 /Resources << /Font << /F1 5 0 R >> >> >>",
            ),
            (4, "<< >>"),
            (5, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>"),
            (6, "<< /Type /Metadata /Subtype /XML >>"),
            (7, "<< /Title (Secret Title) /Producer (tests) >>"),
        ]
        .into_iter()
        .map(|(num, text)| {
            let d = dict(text);
            let obj = match num {
                4 => Object::Stream(Stream {
                    dict: d,
                    data: content.as_bytes().to_vec(),
                }),
                6 => Object::Stream(Stream {
                    dict: d,
                    data: XMP.to_vec(),
                }),
                _ => Object::Dict(d),
            };
            (num, obj)
        })
        .collect();
        let id = |num| ObjRef { num, generation: 0 };
        let mut pdf = b"%PDF-1.7\n".to_vec();
        let mut offsets = Vec::new();
        let mut write = |pdf: &mut Vec<u8>, num: u32, obj: &Object| {
            offsets.push(pdf.len());
            pdf.extend_from_slice(format!("{num} 0 obj\n").as_bytes());
            serialize(&crypt.encrypt_object(id(num), obj.clone()), pdf);
            pdf.extend_from_slice(b"\nendobj\n");
        };
        let loose = if packed {
            // Objects in an object stream are plain inside it; the stream is encrypted as a whole.
            let (mut header, mut body) = (String::new(), Vec::new());
            for (num, obj) in &objects[..3] {
                header.push_str(&format!("{num} {} ", body.len()));
                serialize(obj, &mut body);
                body.push(b'\n');
            }
            let mut data = header.clone().into_bytes();
            data.extend_from_slice(&body);
            let stm = Object::Stream(Stream {
                dict: dict(&format!("<< /Type /ObjStm /N 3 /First {} >>", header.len())),
                data,
            });
            write(&mut pdf, 10, &stm);
            &objects[3..]
        } else {
            &objects[..]
        };
        for (num, obj) in loose {
            write(&mut pdf, *num, obj);
        }
        let trailer = format!(
            "<< /Size 11 /Root 1 0 R /Info 7 0 R /ID [<{0}> <{0}>] /Encrypt {encrypt} >>",
            hex_string(ID)
        );
        if packed {
            pdf.extend_from_slice(format!("trailer\n{trailer}\n%%EOF\n").as_bytes());
            return pdf;
        }
        let at = pdf.len();
        let mut xref = String::from("xref\n0 8\n0000000000 65535 f \n");
        for offset in &offsets {
            xref.push_str(&format!("{offset:010} 00000 n \n"));
        }
        pdf.extend_from_slice(
            format!("{xref}trailer\n{trailer}\nstartxref\n{at}\n%%EOF\n").as_bytes(),
        );
        pdf
    }

    fn page_text(doc: &Document) -> String {
        let pages = doc.pages().unwrap();
        interp::interpret(doc, &pages[0])
            .unwrap()
            .runs
            .iter()
            .flat_map(|r| r.glyphs.iter().map(|g| g.text.clone()))
            .collect()
    }

    fn title(doc: &Document) -> Vec<u8> {
        let info = doc.get_in(&doc.trailer, "Info").unwrap();
        doc.get_in(info.as_dict().unwrap(), "Title")
            .unwrap()
            .as_str_bytes()
            .unwrap()
            .to_vec()
    }

    fn check(how: Lock, packed: bool) {
        let (encrypt, crypt) = lock(how, b"");
        let doc = Document::load(book(&encrypt, &crypt, packed)).unwrap();
        assert!(doc.was_encrypted());
        assert_eq!(doc.was_repaired(), packed);
        assert_eq!(page_text(&doc).replace(' ', ""), TEXT.replace(' ', ""));
        assert_eq!(title(&doc), b"Secret Title");
        let catalog = doc.catalog().unwrap();
        let metadata = doc.get_in(catalog.as_dict().unwrap(), "Metadata").unwrap();
        assert_eq!(doc.stream_data(metadata.as_stream().unwrap()).unwrap(), XMP);

        // A saved copy is not encrypted and reads the same.
        let saved = Saver::new(&doc).save().unwrap();
        assert!(crate::document::find(&saved, b"/Encrypt").is_none());
        let copy = Document::load(saved).unwrap();
        assert!(!copy.was_encrypted() && !copy.was_repaired());
        assert_eq!(page_text(&copy), page_text(&doc));
        assert_eq!(title(&copy), b"Secret Title");
    }

    #[test]
    fn rc4_40() {
        check(Lock::Rc4 { r: 2, bits: 40 }, false);
    }

    #[test]
    fn rc4_128() {
        check(Lock::Rc4 { r: 3, bits: 128 }, false);
        check(Lock::Rc4 { r: 3, bits: 128 }, true);
    }

    #[test]
    fn aes_128_with_and_without_encrypted_metadata() {
        check(
            Lock::Aes128 {
                encrypt_metadata: true,
            },
            false,
        );
        check(
            Lock::Aes128 {
                encrypt_metadata: false,
            },
            false,
        );
        check(
            Lock::Aes128 {
                encrypt_metadata: false,
            },
            true,
        );
    }

    #[test]
    fn aes_256() {
        check(Lock::Aes256 { r: 5 }, false);
        check(Lock::Aes256 { r: 6 }, false);
        check(Lock::Aes256 { r: 6 }, true);
    }

    #[test]
    fn books_that_need_a_password_are_refused() {
        for how in [
            Lock::Rc4 { r: 3, bits: 128 },
            Lock::Aes128 {
                encrypt_metadata: true,
            },
            Lock::Aes256 { r: 6 },
        ] {
            for packed in [false, true] {
                let (encrypt, crypt) = lock(how, b"secret");
                let refused = Document::load(book(&encrypt, &crypt, packed));
                assert!(matches!(refused, Err(Error::PasswordProtected)));
            }
        }
    }

    #[test]
    fn an_empty_owner_password_opens_the_file() {
        // The user password is "secret" and the owner password is empty: /O holds the user password encrypted
        // with a key made from the empty owner password (algorithm 3).
        let (r, n) = (3, 16);
        let owner_key = legacy_owner_key(b"", r, n);
        let mut o = rc4(&owner_key, &padded(b"secret"));
        for i in 1..=19u8 {
            let round: Vec<u8> = owner_key.iter().map(|b| b ^ i).collect();
            o = rc4(&round, &o);
        }
        let key = legacy_user_key(b"secret", &o, -4i32 as u32, ID, r, n, true);
        let mut u = legacy_user_value(&key, r, ID);
        u.resize(32, 0);
        let encrypt = format!(
            "<< /Filter /Standard /V 2 /R 3 /Length 128 /P -4 /O <{}> /U <{}> >>",
            hex_string(&o),
            hex_string(&u)
        );
        let doc = Document::load(book(&encrypt, &crypt(key, Method::Rc4, true), false)).unwrap();
        assert_eq!(page_text(&doc).replace(' ', ""), TEXT.replace(' ', ""));
    }
}
