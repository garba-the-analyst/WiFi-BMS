//! Encrypting tenant payment keys before they reach the database.
//!
//! Format stored in `tenant_secrets.paystack_secret_enc`:
//! `base64( 12-byte random nonce || AES-256-GCM ciphertext+tag )`.
//!
//! The master key lives only in the API's environment (`PLATFORM_MASTER_KEY`,
//! 64 hex characters = 32 bytes). Lose it and every stored key is unreadable,
//! so back it up separately from the database.

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use rand::RngCore;

use crate::error::{AppError, Result};

const NONCE_LEN: usize = 12;

/// The 32-byte key that protects tenant secrets.
#[derive(Clone)]
pub struct MasterKey([u8; 32]);

impl MasterKey {
    /// Parse 64 hex characters.
    pub fn from_hex(s: &str) -> Result<Self> {
        let bytes = hex::decode(s.trim()).map_err(|_| AppError::Internal("master key is not valid hex".into()))?;
        let arr: [u8; 32] = bytes
            .try_into()
            .map_err(|_| AppError::Internal("master key must be 32 bytes (64 hex characters)".into()))?;
        Ok(MasterKey(arr))
    }

    pub fn encrypt(&self, plaintext: &str) -> Result<String> {
        let cipher = Aes256Gcm::new_from_slice(&self.0).map_err(|e| AppError::Internal(e.to_string()))?;
        let mut nonce_bytes = [0u8; NONCE_LEN];
        rand::rngs::OsRng.fill_bytes(&mut nonce_bytes);
        let ct = cipher
            .encrypt(Nonce::from_slice(&nonce_bytes), plaintext.as_bytes())
            .map_err(|_| AppError::Internal("encryption failed".into()))?;
        let mut out = nonce_bytes.to_vec();
        out.extend_from_slice(&ct);
        Ok(B64.encode(out))
    }

    pub fn decrypt(&self, stored: &str) -> Result<String> {
        let raw = B64.decode(stored).map_err(|_| AppError::Internal("stored secret is not base64".into()))?;
        if raw.len() <= NONCE_LEN {
            return Err(AppError::Internal("stored secret is too short".into()));
        }
        let (nonce, ct) = raw.split_at(NONCE_LEN);
        let cipher = Aes256Gcm::new_from_slice(&self.0).map_err(|e| AppError::Internal(e.to_string()))?;
        let pt = cipher
            .decrypt(Nonce::from_slice(nonce), ct)
            .map_err(|_| AppError::Internal("cannot decrypt stored secret (wrong master key or tampered data)".into()))?;
        String::from_utf8(pt).map_err(|_| AppError::Internal("decrypted secret is not UTF-8".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";

    #[test]
    fn round_trip() {
        let k = MasterKey::from_hex(KEY).unwrap();
        let enc = k.encrypt("sk_test_abc123").unwrap();
        assert_ne!(enc, "sk_test_abc123");
        assert_eq!(k.decrypt(&enc).unwrap(), "sk_test_abc123");
    }

    #[test]
    fn same_text_encrypts_differently_each_time() {
        let k = MasterKey::from_hex(KEY).unwrap();
        assert_ne!(k.encrypt("x").unwrap(), k.encrypt("x").unwrap());
    }

    #[test]
    fn wrong_key_or_tampering_fails() {
        let k = MasterKey::from_hex(KEY).unwrap();
        let other = MasterKey::from_hex(&"ff".repeat(32)).unwrap();
        let enc = k.encrypt("secret").unwrap();
        assert!(other.decrypt(&enc).is_err());
        let mut raw = B64.decode(&enc).unwrap();
        *raw.last_mut().unwrap() ^= 1;
        assert!(k.decrypt(&B64.encode(raw)).is_err());
    }

    #[test]
    fn rejects_bad_key_length() {
        assert!(MasterKey::from_hex("abcd").is_err());
        assert!(MasterKey::from_hex("zz").is_err());
    }
}
