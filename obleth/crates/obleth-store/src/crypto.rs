//! Envelope encryption for upstream secrets stored in Postgres.
//!
//! The `models.api_key` (upstream provider key) and `mcp_servers.auth_header`
//! columns hold credentials that let the gateway authenticate *to* third-party
//! services. Storing them in plaintext means anyone with a database dump or
//! read access to the config DB gets the provider keys. We encrypt them at rest
//! with AES-256-GCM using a key supplied out of band via `OBLETH_ENCRYPTION_KEY`
//! (base64-encoded 32 bytes).
//!
//! Encryption is transparent to the rest of the store: values are encrypted on
//! write and decrypted on read. Stored ciphertext is tagged with an `enc:v1:`
//! prefix so we can:
//!   * tell encrypted values apart from legacy plaintext (decrypt passes
//!     untagged values through unchanged, easing migration), and
//!   * version the scheme for future rotation.
//!
//! If `OBLETH_ENCRYPTION_KEY` is unset the cipher is disabled (values stored as
//! plaintext) and a warning is logged — convenient for local dev, but operators
//! running in production should always set it.

use aes_gcm::aead::rand_core::RngCore;
use aes_gcm::aead::{Aead, KeyInit, OsRng};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;

const PREFIX: &str = "enc:v1:";
const NONCE_LEN: usize = 12;

#[derive(Debug, thiserror::Error)]
pub enum CryptoError {
    #[error("encryption key not configured but encrypted data was read")]
    KeyMissing,
    #[error("stored ciphertext is malformed")]
    Malformed,
    #[error("decryption failed (wrong key or tampered data)")]
    Decrypt,
}

/// Symmetric cipher for secret columns. Cheap to clone (the key schedule is
/// reference-counted internally by `Aes256Gcm`'s `Clone`).
#[derive(Clone)]
pub enum Cipher {
    Disabled,
    Enabled(Box<Aes256Gcm>),
}

impl Cipher {
    /// Build from `OBLETH_ENCRYPTION_KEY`. Panics (fail-fast at boot) if the key
    /// is present but not a valid base64-encoded 32-byte value.
    pub fn from_env() -> Self {
        match std::env::var("OBLETH_ENCRYPTION_KEY") {
            Ok(v) if !v.trim().is_empty() => {
                let raw = B64
                    .decode(v.trim())
                    .expect("OBLETH_ENCRYPTION_KEY must be valid base64");
                assert_eq!(
                    raw.len(),
                    32,
                    "OBLETH_ENCRYPTION_KEY must decode to 32 bytes (a 256-bit key)"
                );
                let key = Key::<Aes256Gcm>::from_slice(&raw);
                Cipher::Enabled(Box::new(Aes256Gcm::new(key)))
            }
            _ => {
                tracing::warn!(
                    "OBLETH_ENCRYPTION_KEY not set; upstream provider keys and MCP auth headers \
                     are stored in plaintext"
                );
                Cipher::Disabled
            }
        }
    }

    /// Encrypt a value for storage. When disabled, returns the plaintext.
    pub fn encrypt(&self, plaintext: &str) -> String {
        match self {
            Cipher::Disabled => plaintext.to_string(),
            Cipher::Enabled(cipher) => {
                let mut nonce_bytes = [0u8; NONCE_LEN];
                OsRng.fill_bytes(&mut nonce_bytes);
                let nonce = Nonce::from_slice(&nonce_bytes);
                let ciphertext = cipher
                    .encrypt(nonce, plaintext.as_bytes())
                    .expect("AES-GCM encryption is infallible for valid keys");
                let mut blob = Vec::with_capacity(NONCE_LEN + ciphertext.len());
                blob.extend_from_slice(&nonce_bytes);
                blob.extend_from_slice(&ciphertext);
                format!("{PREFIX}{}", B64.encode(blob))
            }
        }
    }

    /// Decrypt a stored value. Untagged (legacy plaintext) values pass through.
    pub fn decrypt(&self, stored: &str) -> Result<String, CryptoError> {
        let Some(b64) = stored.strip_prefix(PREFIX) else {
            return Ok(stored.to_string());
        };
        let cipher = match self {
            Cipher::Enabled(c) => c,
            Cipher::Disabled => return Err(CryptoError::KeyMissing),
        };
        let blob = B64.decode(b64).map_err(|_| CryptoError::Malformed)?;
        if blob.len() <= NONCE_LEN {
            return Err(CryptoError::Malformed);
        }
        let (nonce_bytes, ciphertext) = blob.split_at(NONCE_LEN);
        let nonce = Nonce::from_slice(nonce_bytes);
        let plaintext = cipher
            .decrypt(nonce, ciphertext)
            .map_err(|_| CryptoError::Decrypt)?;
        String::from_utf8(plaintext).map_err(|_| CryptoError::Malformed)
    }

    /// Encrypt an optional value (e.g. a nullable column).
    pub fn encrypt_opt(&self, value: Option<&str>) -> Option<String> {
        value.map(|s| self.encrypt(s))
    }

    /// Decrypt an optional value read from a nullable column.
    pub fn decrypt_opt(&self, value: Option<String>) -> Result<Option<String>, CryptoError> {
        value.map(|s| self.decrypt(&s)).transpose()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_cipher() -> Cipher {
        Cipher::Enabled(Box::new(Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(
            &[7u8; 32],
        ))))
    }

    #[test]
    fn round_trips() {
        let c = test_cipher();
        let ct = c.encrypt("sk-secret-value");
        assert!(ct.starts_with(PREFIX));
        assert_ne!(ct, "sk-secret-value");
        assert_eq!(c.decrypt(&ct).unwrap(), "sk-secret-value");
    }

    #[test]
    fn nonce_is_randomized() {
        let c = test_cipher();
        assert_ne!(c.encrypt("same"), c.encrypt("same"));
    }

    #[test]
    fn disabled_is_passthrough() {
        let c = Cipher::Disabled;
        assert_eq!(c.encrypt("x"), "x");
        assert_eq!(c.decrypt("x").unwrap(), "x");
    }

    #[test]
    fn legacy_plaintext_passes_through() {
        let c = test_cipher();
        assert_eq!(c.decrypt("plain-legacy-key").unwrap(), "plain-legacy-key");
    }

    #[test]
    fn wrong_key_fails() {
        let ct = test_cipher().encrypt("secret");
        let other = Cipher::Enabled(Box::new(Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(
            &[9u8; 32],
        ))));
        assert!(matches!(other.decrypt(&ct), Err(CryptoError::Decrypt)));
    }

    #[test]
    fn disabled_cannot_read_ciphertext() {
        let ct = test_cipher().encrypt("secret");
        assert!(matches!(
            Cipher::Disabled.decrypt(&ct),
            Err(CryptoError::KeyMissing)
        ));
    }

    #[test]
    fn opt_helpers() {
        let c = test_cipher();
        assert_eq!(c.encrypt_opt(None), None);
        let enc = c.encrypt_opt(Some("v"));
        assert_eq!(c.decrypt_opt(enc).unwrap(), Some("v".to_string()));
        assert_eq!(c.decrypt_opt(None).unwrap(), None);
    }
}
