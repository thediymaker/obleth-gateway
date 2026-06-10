//! API key generation and hashing.
//!
//! The raw secret is shown to the operator exactly once at creation. We persist
//! only the SHA-256 hash (for lookup) and a short display prefix, so a database
//! or cache leak never exposes usable credentials.

use rand::RngCore;
use sha2::{Digest, Sha256};
use std::sync::OnceLock;

/// Optional server-side pepper mixed into key hashes. Unlike a per-key salt
/// (which would have to be stored alongside the hash and so leaks with it), the
/// pepper is held out of band via `OBLETH_API_KEY_PEPPER` (env / secret
/// manager). A database leak then yields hashes that can't be confirmed against
/// guessed keys without also stealing the pepper.
///
/// When unset, hashing is byte-for-byte identical to the unpeppered scheme, so
/// existing keys keep working. Changing or adding a pepper invalidates
/// previously issued keys (they must be rotated).
fn pepper() -> &'static [u8] {
    static PEPPER: OnceLock<Vec<u8>> = OnceLock::new();
    PEPPER
        .get_or_init(|| {
            std::env::var("OBLETH_API_KEY_PEPPER")
                .map(String::into_bytes)
                .unwrap_or_default()
        })
        .as_slice()
}

/// True when a server-side pepper is configured. Exposed so config backups can
/// record the flag — restored key hashes only authenticate when the target
/// instance uses the same pepper, and the hashes themselves are opaque.
pub fn pepper_is_set() -> bool {
    !pepper().is_empty()
}

/// A freshly minted key. `secret` is returned to the caller once and never stored.
#[derive(Debug, Clone)]
pub struct GeneratedKey {
    pub secret: String,
    pub prefix: String,
    pub hash: String,
}

/// Generate a new API key: `sk_<48 hex chars>`.
pub fn generate_api_key() -> GeneratedKey {
    let mut bytes = [0u8; 24];
    rand::thread_rng().fill_bytes(&mut bytes);
    let secret = format!("sk_{}", hex::encode(bytes));
    let prefix = secret.chars().take(18).collect::<String>();
    let hash = hash_api_key(&secret);
    GeneratedKey {
        secret,
        prefix,
        hash,
    }
}

/// SHA-256 hex digest of a raw key, used as the lookup handle in Postgres + Redis.
/// Mixes in the optional server-side pepper (see [`pepper`]).
pub fn hash_api_key(secret: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(secret.as_bytes());
    let pepper = pepper();
    if !pepper.is_empty() {
        hasher.update([0u8]);
        hasher.update(pepper);
    }
    hex::encode(hasher.finalize())
}

/// Exact-match response cache key: SHA-256 over the client-facing model name
/// and the raw request body. Identical requests for the same model collide
/// (a cache hit); anything different misses.
pub fn cache_key(model: &str, body: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(model.as_bytes());
    hasher.update([0u8]);
    hasher.update(body);
    hex::encode(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_key_is_well_formed() {
        let k = generate_api_key();
        assert!(k.secret.starts_with("sk_"));
        assert_eq!(k.prefix.len(), 18);
        assert_eq!(k.hash.len(), 64);
        assert_eq!(hash_api_key(&k.secret), k.hash);
    }

    #[test]
    fn hash_is_stable_and_distinct() {
        assert_eq!(hash_api_key("a"), hash_api_key("a"));
        assert_ne!(hash_api_key("a"), hash_api_key("b"));
    }
}
