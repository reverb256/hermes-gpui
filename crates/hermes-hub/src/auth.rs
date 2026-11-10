//! Token generation and verification.
//!
//! Long-lived tokens are returned exactly once and stored only as SHA-256
//! hashes. A database leak therefore does not hand out working credentials.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::Rng;
use sha2::{Digest, Sha256};

/// 32 random bytes, URL-safe, no padding.
pub fn random_token() -> String {
    let mut bytes = [0u8; 32];
    rand::rng().fill(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

/// 16 random bytes, used for one-time enrollment codes and prompt challenges.
pub fn random_challenge() -> String {
    let mut bytes = [0u8; 32];
    rand::rng().fill(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

pub fn hash_token(token: &str) -> String {
    let digest = Sha256::digest(token.as_bytes());
    hex::encode(digest)
}

/// Compare two byte strings without an early exit on the first mismatch.
pub fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    let mut diff = left.len() ^ right.len();
    let len = left.len().max(right.len());
    for index in 0..len {
        let a = left.get(index).copied().unwrap_or(0);
        let b = right.get(index).copied().unwrap_or(0);
        diff |= usize::from(a ^ b);
    }
    diff == 0
}

/// Extract a bearer token from an `Authorization` header.
pub fn bearer(header: Option<&str>) -> Option<&str> {
    let value = header?.trim();
    let token = value
        .strip_prefix("Bearer ")
        .or_else(|| value.strip_prefix("bearer "))?;
    if token.is_empty() { None } else { Some(token) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_tokens_differ_and_are_url_safe() {
        let first = random_token();
        let second = random_token();
        assert_ne!(first, second);
        assert_eq!(first.len(), 43);
        assert!(
            first
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        );
    }

    #[test]
    fn hashing_is_stable_and_does_not_contain_the_token() {
        let token = "a-very-secret-token";
        let hash = hash_token(token);
        assert_eq!(hash, hash_token(token));
        assert!(!hash.contains(token));
        assert_eq!(hash.len(), 64);
    }

    #[test]
    fn comparison_rejects_prefixes_and_length_changes() {
        assert!(constant_time_eq(b"abcdef", b"abcdef"));
        assert!(!constant_time_eq(b"abcdef", b"abcdeg"));
        assert!(!constant_time_eq(b"abcdef", b"abcde"));
        assert!(!constant_time_eq(b"", b"a"));
    }

    #[test]
    fn bearer_parsing_is_strict() {
        assert_eq!(bearer(Some("Bearer abc")), Some("abc"));
        assert_eq!(bearer(Some("bearer abc")), Some("abc"));
        assert_eq!(bearer(Some("Basic abc")), None);
        assert_eq!(bearer(Some("Bearer ")), None);
        assert_eq!(bearer(None), None);
    }
}
