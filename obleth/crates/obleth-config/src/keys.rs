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

/// Prefix new keys start with unless an operator chose another (see
/// [`is_valid_key_prefix`]). Keys minted before checksummed keys existed
/// (`sk_…`) and any other format an instance imported keep authenticating:
/// only minting depends on the prefix.
pub const DEFAULT_KEY_PREFIX: &str = "obl";

/// Most characters a key prefix may have.
pub const KEY_PREFIX_MAX_CHARS: usize = 3;

/// Hex characters of random secret after the prefix and `_` (24 bytes).
const KEY_RANDOM_HEX: usize = 48;

/// Hex characters of the CRC-32 checksum that ends a key.
const KEY_CHECKSUM_HEX: usize = 8;

/// Hex characters after the prefix and `_`.
const KEY_TAIL_HEX: usize = KEY_RANDOM_HEX + KEY_CHECKSUM_HEX;

/// Length of a well-formed key with the default prefix.
pub const KEY_LEN: usize = DEFAULT_KEY_PREFIX.len() + 1 + KEY_TAIL_HEX;

/// True when `prefix` may start new keys: 1 to [`KEY_PREFIX_MAX_CHARS`]
/// lowercase letters or digits, the first a letter. Kept short and plain so
/// a key stays one token in a URL, a shell and a secret scanner's pattern.
pub fn is_valid_key_prefix(prefix: &str) -> bool {
    let bytes = prefix.as_bytes();
    (1..=KEY_PREFIX_MAX_CHARS).contains(&bytes.len())
        && bytes[0].is_ascii_lowercase()
        && bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
}

/// The prefix `OBLETH_KEY_PREFIX` names, when it is a valid one. The saved
/// setting wins over it; it only sets the starting value.
pub fn env_key_prefix() -> Option<&'static str> {
    static PREFIX: OnceLock<Option<String>> = OnceLock::new();
    PREFIX
        .get_or_init(|| {
            std::env::var("OBLETH_KEY_PREFIX")
                .ok()
                .map(|v| v.trim().to_string())
                .filter(|v| is_valid_key_prefix(v))
        })
        .as_deref()
}

/// The prefix to mint with when no setting is saved.
pub fn default_key_prefix() -> &'static str {
    env_key_prefix().unwrap_or(DEFAULT_KEY_PREFIX)
}

/// Characters of a key kept for display (`ApiKey::key_prefix`).
const DISPLAY_PREFIX_CHARS: usize = 18;

/// CRC-32 (IEEE 802.3, the zlib/PNG polynomial) of `data`. Bitwise rather
/// than table-driven: it runs once per minted key and once per `obl_`
/// credential presented, on under 60 bytes.
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

/// Generate a new API key with the default prefix. See
/// [`generate_api_key_with_prefix`].
pub fn generate_api_key() -> GeneratedKey {
    generate_api_key_with_prefix(DEFAULT_KEY_PREFIX)
}

/// Generate a new API key: `prefix`, `_`, 48 lowercase hex characters of
/// random secret, then 8 lowercase hex characters of CRC-32 over everything
/// before them. The checksum lets the gateway refuse a mistyped or truncated
/// key without a lookup, and lets secret scanners recognise a real key with
/// few false positives. An invalid `prefix` falls back to the default.
pub fn generate_api_key_with_prefix(prefix: &str) -> GeneratedKey {
    let prefix = if is_valid_key_prefix(prefix) {
        prefix
    } else {
        DEFAULT_KEY_PREFIX
    };
    let mut bytes = [0u8; KEY_RANDOM_HEX / 2];
    rand::thread_rng().fill_bytes(&mut bytes);
    let body = format!("{prefix}_{}", hex::encode(bytes));
    let secret = format!("{body}{:08x}", crc32(body.as_bytes()));
    let prefix = secret
        .chars()
        .take(DISPLAY_PREFIX_CHARS)
        .collect::<String>();
    let hash = hash_api_key(&secret);
    GeneratedKey {
        secret,
        prefix,
        hash,
    }
}

/// The key's checksummed body and checksum when `secret` has the shape of a
/// checksummed key: a valid prefix, `_`, and 56 lowercase hex characters.
fn checksummed_parts(secret: &str) -> Option<(&str, &str)> {
    let (prefix, tail) = secret.split_once('_')?;
    if !is_valid_key_prefix(prefix)
        || tail.len() != KEY_TAIL_HEX
        || !tail.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    {
        return None;
    }
    Some(secret.split_at(secret.len() - KEY_CHECKSUM_HEX))
}

/// True when `secret` is a well-formed checksummed key, with any valid
/// prefix: the prefix, `_`, 56 lowercase hex characters, and a checksum that
/// matches. Says nothing about whether the key exists.
pub fn is_well_formed_key(secret: &str) -> bool {
    checksummed_parts(secret).is_some_and(|(body, checksum)| {
        u32::from_str_radix(checksum, 16).is_ok_and(|c| c == crc32(body.as_bytes()))
    })
}

/// True when a presented credential claims to be a checksummed key but is
/// not a well-formed one, so it can be refused before it is hashed or looked
/// up: anything starting with the default prefix, or a key of the full shape
/// with any prefix whose checksum fails. A key in another format (`sk_…` and
/// the rest) is never refused here, and neither is a cut-off key with
/// another prefix: those go to the lookup and are refused there.
pub fn is_malformed_key(secret: &str) -> bool {
    if is_well_formed_key(secret) {
        return false;
    }
    secret
        .strip_prefix(DEFAULT_KEY_PREFIX)
        .is_some_and(|rest| rest.starts_with('_'))
        || checksummed_parts(secret).is_some()
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
        assert!(k.secret.starts_with("obl_"));
        assert_eq!(k.secret.len(), KEY_LEN);
        assert_eq!(KEY_LEN, 60);
        assert!(k.secret[4..]
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')));
        assert!(is_well_formed_key(&k.secret));
        assert!(!is_malformed_key(&k.secret));
        assert_eq!(k.prefix, k.secret[..18]);
        assert_eq!(k.hash.len(), 64);
        assert_eq!(hash_api_key(&k.secret), k.hash);
        assert_ne!(generate_api_key().secret, k.secret);
    }

    #[test]
    fn crc32_matches_the_standard_check_value() {
        // The CRC-32/ISO-HDLC check value every implementation agrees on.
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b""), 0);
    }

    #[test]
    fn checksum_covers_the_prefix_and_the_random_part() {
        let body = format!("obl_{}", "0".repeat(48));
        let good = format!("{body}{:08x}", crc32(body.as_bytes()));
        assert!(is_well_formed_key(&good));

        // One flipped hex digit anywhere fails the checksum.
        let mut flipped = good.clone().into_bytes();
        flipped[10] = b'1';
        let flipped = String::from_utf8(flipped).unwrap();
        assert!(is_malformed_key(&flipped));

        let mut bad_sum = good.clone();
        bad_sum.replace_range(56.., "00000000");
        assert!(is_malformed_key(&bad_sum));

        // Wrong length, upper case, non-hex: all malformed.
        assert!(is_malformed_key(&good[..59]));
        assert!(is_malformed_key(&format!("{good}0")));
        assert!(is_malformed_key(
            &good.to_uppercase().replace("OBL_", "obl_")
        ));
        assert!(is_malformed_key(&good.replace('0', "g")));
        assert!(is_malformed_key("obl_"));
    }

    #[test]
    fn other_formats_are_never_called_malformed() {
        // Keys minted before `obl_` keep authenticating exactly as before.
        assert!(!is_malformed_key(
            "sk_0123456789abcdef0123456789abcdef0123456789abcdef"
        ));
        assert!(!is_malformed_key("sk_short"));
        assert!(!is_malformed_key(
            "eyJhbGciOiJFUzI1NiJ9.eyJzdWIiOiJhIn0.sig"
        ));
        assert!(!is_malformed_key("OBL_upper-case-is-another-format"));
        assert!(!is_malformed_key(""));
        assert!(!is_well_formed_key("sk_0123"));
    }

    #[test]
    fn a_prefix_is_one_to_three_lowercase_letters_or_digits() {
        for ok in ["obl", "sk", "rc", "air", "a", "x9", "ab1"] {
            assert!(is_valid_key_prefix(ok), "{ok}");
        }
        for bad in ["", "abcd", "Obl", "9ab", "a_b", "a-b", "é", " rc", "rc "] {
            assert!(!is_valid_key_prefix(bad), "{bad}");
        }
    }

    #[test]
    fn keys_mint_with_the_prefix_they_are_given() {
        for prefix in ["rc", "air", "sk", "a"] {
            let k = generate_api_key_with_prefix(prefix);
            assert!(k.secret.starts_with(&format!("{prefix}_")));
            assert_eq!(k.secret.len(), prefix.len() + 1 + 56);
            assert!(is_well_formed_key(&k.secret));
            assert!(!is_malformed_key(&k.secret));
            assert_eq!(k.prefix, k.secret[..18]);
        }
        // An invalid prefix never reaches a key.
        assert!(generate_api_key_with_prefix("toolong")
            .secret
            .starts_with("obl_"));
        assert!(generate_api_key_with_prefix("").secret.starts_with("obl_"));
    }

    #[test]
    fn a_checksum_failure_is_caught_whatever_the_prefix() {
        let k = generate_api_key_with_prefix("rc").secret;
        let mut flipped = k.clone().into_bytes();
        flipped[5] = if flipped[5] == b'0' { b'1' } else { b'0' };
        assert!(is_malformed_key(&String::from_utf8(flipped).unwrap()));
        // A cut-off key with another prefix goes to the lookup instead.
        assert!(!is_malformed_key(&k[..40]));
        assert!(!is_well_formed_key(&k[..40]));
        // A new-format `sk_` key is told apart from a 48-hex legacy one by length.
        let sk = generate_api_key_with_prefix("sk").secret;
        assert!(is_well_formed_key(&sk));
        assert!(!is_malformed_key(
            "sk_0123456789abcdef0123456789abcdef0123456789abcdef"
        ));
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
