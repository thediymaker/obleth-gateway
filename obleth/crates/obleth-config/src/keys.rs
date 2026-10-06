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

/// Exact-match response cache key: SHA-256 over the tenant id, the
/// client-facing model name and the raw request body. Identical requests from
/// the same tenant for the same model collide (a cache hit); anything
/// different misses. The tenant is part of the key so one tenant's cached
/// answer is never replayed to another.
pub fn cache_key(tenant_id: &str, model: &str, body: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(tenant_id.as_bytes());
    hasher.update([0u8]);
    hasher.update(model.as_bytes());
    hasher.update([0u8]);
    hasher.update(body);
    hex::encode(hasher.finalize())
}

/// Longest end-user id kept, in characters. A longer one is cut, so two ids
/// that differ only past this point share one place in the queue.
pub const END_USER_MAX_CHARS: usize = 128;

/// The end-user id a request names, cleaned up: surrounding whitespace
/// trimmed, cut to [`END_USER_MAX_CHARS`], `None` when nothing is left.
pub fn normalize_end_user(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed.chars().take(END_USER_MAX_CHARS).collect())
}

/// The scheduler identity of one end user of a key that has
/// `end_user_fairshare` on. Derived, not stored: the same (key, end user)
/// always yields the same id, so a user keeps their place across requests
/// and gateway replicas, and two keys' users never collide. Version-8 UUID
/// (custom), so it can never equal a real key id, which is version 4.
pub fn end_user_key_id(key: uuid::Uuid, end_user: &str) -> uuid::Uuid {
    let mut hasher = Sha256::new();
    hasher.update(b"obleth-end-user\0");
    hasher.update(key.as_bytes());
    hasher.update([0u8]);
    hasher.update(end_user.as_bytes());
    let digest = hasher.finalize();
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x80; // version 8
    bytes[8] = (bytes[8] & 0x3f) | 0x80; // RFC 4122 variant
    uuid::Uuid::from_bytes(bytes)
}

/// SHA-256 hex digest of an arbitrary content string. Used by the compression
/// boon as the reversibility store key: identical content hashes to one entry.
pub fn content_hash(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
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

    #[test]
    fn end_user_ids_are_trimmed_and_capped() {
        assert_eq!(normalize_end_user("  alice \n").as_deref(), Some("alice"));
        assert_eq!(normalize_end_user("   "), None);
        assert_eq!(normalize_end_user(""), None);
        let long = "é".repeat(END_USER_MAX_CHARS + 10);
        let cut = normalize_end_user(&long).unwrap();
        assert_eq!(cut.chars().count(), END_USER_MAX_CHARS);
    }

    #[test]
    fn end_user_key_id_is_stable_and_scoped_to_the_key() {
        let key = uuid::Uuid::new_v4();
        let other = uuid::Uuid::new_v4();
        let alice = end_user_key_id(key, "alice");
        assert_eq!(alice, end_user_key_id(key, "alice"));
        assert_ne!(alice, end_user_key_id(key, "bob"));
        assert_ne!(alice, end_user_key_id(other, "alice"));
        // Never mistakable for a real (v4) key id.
        assert_eq!(alice.get_version_num(), 8);
        assert_ne!(alice, key);
    }

    #[test]
    fn content_hash_is_stable_and_distinct() {
        assert_eq!(content_hash("hello"), content_hash("hello"));
        assert_ne!(content_hash("hello"), content_hash("world"));
        assert_eq!(content_hash("hello").len(), 64); // SHA-256 hex
    }

    #[test]
    fn cache_key_is_scoped_to_the_tenant() {
        let body = br#"{"model":"m","messages":[]}"#;
        assert_eq!(
            cache_key("tenant-a", "m", body),
            cache_key("tenant-a", "m", body)
        );
        assert_ne!(
            cache_key("tenant-a", "m", body),
            cache_key("tenant-b", "m", body),
            "identical requests from two tenants must never share an entry"
        );
        assert_ne!(cache_key("t", "m", body), cache_key("t", "n", body));
    }

    #[test]
    fn cache_key_fields_cannot_bleed_into_each_other() {
        // The NUL separators keep ("ab", "c") and ("a", "bc") distinct.
        assert_ne!(cache_key("ab", "c", b"x"), cache_key("a", "bc", b"x"));
        assert_ne!(cache_key("t", "mx", b""), cache_key("t", "m", b"x"));
    }
}
