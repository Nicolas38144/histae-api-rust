use std::fmt;

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use hmac::{Hmac, Mac};
use sha2::Sha256;

use crate::config::SecretString;

const AES_KEY_BYTES: usize = 32;
const GCM_NONCE_BYTES: usize = 12;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CryptoError {
    InvalidKey,
    RandomUnavailable,
    EncryptionFailed,
}

impl CryptoError {
    pub fn safe_code(self) -> &'static str {
        match self {
            Self::InvalidKey => "crypto_invalid_key",
            Self::RandomUnavailable => "crypto_random_unavailable",
            Self::EncryptionFailed => "crypto_encryption_failed",
        }
    }
}

impl fmt::Display for CryptoError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.safe_code())
    }
}

impl std::error::Error for CryptoError {}

pub fn parse_phone_key(value: &SecretString) -> Result<[u8; AES_KEY_BYTES], CryptoError> {
    parse_phone_key_bytes(value.expose_secret())
}

pub fn encrypt_phone(phone: &str, key: &SecretString) -> Result<Vec<u8>, CryptoError> {
    let mut nonce = [0_u8; GCM_NONCE_BYTES];
    getrandom::fill(&mut nonce).map_err(|_| CryptoError::RandomUnavailable)?;
    encrypt_phone_with_nonce(phone, key, nonce)
}

pub fn encrypt_phone_with_nonce(
    phone: &str,
    key: &SecretString,
    nonce: [u8; GCM_NONCE_BYTES],
) -> Result<Vec<u8>, CryptoError> {
    let key = parse_phone_key(key)?;
    let cipher = Aes256Gcm::new_from_slice(&key).map_err(|_| CryptoError::InvalidKey)?;
    let ciphertext = cipher
        .encrypt(Nonce::from_slice(&nonce), phone.as_bytes())
        .map_err(|_| CryptoError::EncryptionFailed)?;
    let mut output = Vec::with_capacity(nonce.len() + ciphertext.len());
    output.extend_from_slice(&nonce);
    output.extend_from_slice(&ciphertext);
    Ok(output)
}

pub fn hmac_sha256(value: &str, key: &SecretString) -> Result<String, CryptoError> {
    let key = parse_phone_key(key)?;
    let mut hmac =
        <Hmac<Sha256> as Mac>::new_from_slice(&key).map_err(|_| CryptoError::InvalidKey)?;
    hmac.update(value.as_bytes());
    Ok(hex_lower(&hmac.finalize().into_bytes()))
}

pub fn sha256_hex(value: &[u8]) -> String {
    use sha2::Digest as _;
    hex_lower(&Sha256::digest(value))
}

fn parse_phone_key_bytes(value: &str) -> Result<[u8; AES_KEY_BYTES], CryptoError> {
    if value.len() == AES_KEY_BYTES * 2 && value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        let mut decoded = [0_u8; AES_KEY_BYTES];
        for (index, slot) in decoded.iter_mut().enumerate() {
            let offset = index * 2;
            *slot = u8::from_str_radix(&value[offset..offset + 2], 16)
                .map_err(|_| CryptoError::InvalidKey)?;
        }
        return Ok(decoded);
    }
    value
        .as_bytes()
        .try_into()
        .map_err(|_| CryptoError::InvalidKey)
}

fn hex_lower(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(&mut output, "{byte:02x}");
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vector_key() -> SecretString {
        SecretString::new("0123456789abcdef0123456789abcdef".to_owned())
    }

    #[test]
    fn matches_the_node_aes_gcm_and_hmac_vectors() {
        let encrypted = encrypt_phone_with_nonce(
            "+33612345678",
            &vector_key(),
            [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11],
        )
        .expect("valid vector");
        assert_eq!(
            hex_lower(&encrypted),
            "000102030405060708090a0b06d6884ae82ae1989dea4686e0811700184cb36989c103eb58b6a3b5"
        );
        assert_eq!(
            hmac_sha256("+33612345678", &vector_key()).as_deref(),
            Ok("08a3ca6b31cfb0ff0370784f7851bd0cd87000c5f0b92c408fc7657bf8b00cc7")
        );
    }

    #[test]
    fn preserves_raw_and_hex_phone_key_rules() {
        assert_eq!(
            parse_phone_key(&SecretString::new("a".repeat(32))),
            Ok([b'a'; 32])
        );
        assert_eq!(
            parse_phone_key(&SecretString::new("ab".repeat(32))),
            Ok([0xab; 32])
        );
        assert_eq!(
            parse_phone_key(&SecretString::new("short".to_owned())),
            Err(CryptoError::InvalidKey)
        );
    }
}
