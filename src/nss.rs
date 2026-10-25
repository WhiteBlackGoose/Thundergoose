//! Decrypts the secrets Thunderbird keeps in `logins.json`, using the NSS key database `key4.db`.
//!
//! Only the bits of NSS that Mozilla profiles actually use are implemented: PBES2
//! (PBKDF2-HMAC-SHA256 + AES-256-CBC) and the legacy SHA1/3DES PBE for the key database,
//! and 3DES-CBC / AES-256-CBC for individual login fields.

use std::path::Path;

use base64::Engine as _;
use cbc::cipher::{block_padding::NoPadding, BlockDecryptMut, KeyIvInit};
use hmac::{Hmac, Mac};
use sha1::{Digest, Sha1};

const OID_PBES2: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x05, 0x0d];
const OID_PBE_SHA1_3DES: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x0c, 0x05, 0x01, 0x03];
const OID_DES_EDE3_CBC: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x03, 0x07];
const OID_AES256_CBC: &[u8] = &[0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x01, 0x2a];

/// One DER TLV: (tag, contents, rest of input).
fn tlv(input: &[u8]) -> Option<(u8, &[u8], &[u8])> {
    let (&tag, rest) = input.split_first()?;
    let (&first, mut rest) = rest.split_first()?;
    let len = if first < 0x80 {
        first as usize
    } else {
        let n = (first & 0x7f) as usize;
        if n == 0 || n > 4 || rest.len() < n {
            return None;
        }
        let len = rest[..n].iter().fold(0usize, |acc, &b| (acc << 8) | b as usize);
        rest = &rest[n..];
        len
    };
    if rest.len() < len {
        return None;
    }
    Some((tag, &rest[..len], &rest[len..]))
}

fn children(mut content: &[u8]) -> Vec<(u8, &[u8])> {
    let mut out = Vec::new();
    while let Some((tag, value, rest)) = tlv(content) {
        out.push((tag, value));
        content = rest;
    }
    out
}

fn int(bytes: &[u8]) -> u32 {
    bytes.iter().fold(0u32, |acc, &b| (acc << 8) | b as u32)
}

fn unpad(mut data: Vec<u8>, block: usize) -> Option<Vec<u8>> {
    let n = *data.last()? as usize;
    if n == 0 || n > block || n > data.len() || !data[data.len() - n..].iter().all(|&b| b as usize == n) {
        return None;
    }
    data.truncate(data.len() - n);
    Some(data)
}

fn aes256_cbc(key: &[u8], iv: &[u8], ct: &[u8]) -> Option<Vec<u8>> {
    let dec = cbc::Decryptor::<aes::Aes256>::new_from_slices(key, iv).ok()?;
    let plain = dec.decrypt_padded_vec_mut::<NoPadding>(ct).ok()?;
    unpad(plain, 16)
}

fn des3_cbc(key: &[u8], iv: &[u8], ct: &[u8]) -> Option<Vec<u8>> {
    let dec = cbc::Decryptor::<des::TdesEde3>::new_from_slices(key.get(..24)?, iv).ok()?;
    let plain = dec.decrypt_padded_vec_mut::<NoPadding>(ct).ok()?;
    unpad(plain, 8)
}

fn hmac_sha1(key: &[u8], parts: &[&[u8]]) -> Vec<u8> {
    let mut mac = Hmac::<Sha1>::new_from_slice(key).expect("hmac accepts any key");
    for p in parts {
        mac.update(p);
    }
    mac.finalize().into_bytes().to_vec()
}

/// Decrypts a password-based-encryption blob from key4.db (`SEQUENCE { AlgorithmIdentifier, OCTET STRING }`).
fn decrypt_pbe(global_salt: &[u8], password: &[u8], blob: &[u8]) -> Option<Vec<u8>> {
    let (_, top, _) = tlv(blob)?;
    let top = children(top);
    let (_, alg) = top.first()?;
    let (_, ct) = top.get(1)?;
    let alg = children(alg);
    let (_, oid) = alg.first()?;
    let (_, params) = alg.get(1)?;
    let hp = Sha1::new().chain_update(global_salt).chain_update(password).finalize();

    if *oid == OID_PBES2 {
        let params = children(params);
        let kdf = children(params.first()?.1);
        let kdf_params = children(kdf.get(1)?.1);
        let salt = kdf_params.first()?.1;
        let iterations = int(kdf_params.get(1)?.1);
        let key_len = kdf_params.get(2).map(|p| int(p.1) as usize).unwrap_or(32);
        let enc = children(params.get(1)?.1);
        if enc.first()?.1 != OID_AES256_CBC {
            return None;
        }
        let iv_raw = enc.get(1)?.1;
        // NSS stores a truncated IV and prepends the DER header of an OCTET STRING of length 14.
        let iv: Vec<u8> = if iv_raw.len() == 14 { [&[0x04, 0x0e][..], iv_raw].concat() } else { iv_raw.to_vec() };
        let mut key = vec![0u8; key_len];
        pbkdf2::pbkdf2_hmac::<sha2::Sha256>(&hp, salt, iterations, &mut key);
        aes256_cbc(&key, &iv, ct)
    } else if *oid == OID_PBE_SHA1_3DES {
        let params = children(params);
        let entry_salt = params.first()?.1;
        let mut pes = entry_salt.to_vec();
        pes.resize(20.max(entry_salt.len()), 0);
        let chp = Sha1::new().chain_update(hp).chain_update(entry_salt).finalize();
        let k1 = hmac_sha1(&chp, &[&pes, entry_salt]);
        let tk = hmac_sha1(&chp, &[&pes]);
        let k2 = hmac_sha1(&chp, &[&tk, entry_salt]);
        let k = [k1, k2].concat();
        des3_cbc(&k[..24], &k[k.len() - 8..], ct)
    } else {
        None
    }
}

#[derive(Debug)]
pub enum NssError {
    Missing,
    PrimaryPassword,
    Corrupt(String),
}

impl std::fmt::Display for NssError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NssError::Missing => write!(f, "key4.db not found"),
            NssError::PrimaryPassword => write!(f, "Thunderbird's primary password is wrong or required"),
            NssError::Corrupt(e) => write!(f, "cannot read key4.db: {e}"),
        }
    }
}

pub struct KeyStore {
    keys: Vec<Vec<u8>>,
}

impl KeyStore {
    pub fn open(profile: &Path, primary_password: &str) -> Result<Self, NssError> {
        let path = profile.join("key4.db");
        if !path.exists() {
            return Err(NssError::Missing);
        }
        let conn = crate::tb::open_sqlite_copy(&path).map_err(|e| NssError::Corrupt(e.to_string()))?;
        let (salt, check): (Vec<u8>, Vec<u8>) = conn
            .query_row("SELECT item1, item2 FROM metadata WHERE id = 'password'", [], |r| Ok((r.get(0)?, r.get(1)?)))
            .map_err(|e| NssError::Corrupt(e.to_string()))?;
        match decrypt_pbe(&salt, primary_password.as_bytes(), &check) {
            Some(p) if p.starts_with(b"password-check") => {}
            _ => return Err(NssError::PrimaryPassword),
        }
        let mut stmt = conn.prepare("SELECT a11 FROM nssPrivate").map_err(|e| NssError::Corrupt(e.to_string()))?;
        let keys = stmt
            .query_map([], |r| r.get::<_, Vec<u8>>(0))
            .map_err(|e| NssError::Corrupt(e.to_string()))?
            .filter_map(Result::ok)
            .filter_map(|a11| decrypt_pbe(&salt, primary_password.as_bytes(), &a11))
            .collect();
        Ok(KeyStore { keys })
    }

    /// Decrypts a base64 `encryptedUsername` / `encryptedPassword` field of logins.json.
    pub fn decrypt(&self, b64: &str) -> Option<String> {
        let blob = base64::engine::general_purpose::STANDARD.decode(b64.trim()).ok()?;
        let (_, top, _) = tlv(&blob)?;
        let top = children(top);
        let alg = children(top.get(1)?.1);
        let oid = alg.first()?.1;
        let iv = alg.get(1)?.1;
        let ct = top.get(2)?.1;
        self.keys.iter().find_map(|key| {
            let plain = if oid == OID_AES256_CBC && key.len() >= 32 {
                aes256_cbc(&key[..32], iv, ct)
            } else if oid == OID_DES_EDE3_CBC && key.len() >= 24 {
                des3_cbc(key, iv, ct)
            } else {
                None
            }?;
            String::from_utf8(plain).ok()
        })
    }
}
