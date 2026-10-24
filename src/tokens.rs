//! Guest link tokens and other secrets.
//!
//! A household's link token is HMAC-SHA256(secret, "household:{id}:{version}"),
//! truncated to 128 bits and base32-encoded. Tokens are derived, never stored:
//! the database only keeps SHA-256(token) for lookup, so a leaked database
//! does not leak working links. Bumping `token_version` revokes a link.

use data_encoding::BASE32_NOPAD;
use hmac::{Hmac, Mac};
use rand::RngCore;
use sha2::{Digest, Sha256};

type HmacSha256 = Hmac<Sha256>;

pub fn household_token(secret: &[u8], id: i64, version: i64) -> String {
    let mut mac = HmacSha256::new_from_slice(secret).expect("hmac accepts any key length");
    mac.update(format!("household:{id}:{version}").as_bytes());
    let out = mac.finalize().into_bytes();
    BASE32_NOPAD.encode(&out[..16]).to_ascii_lowercase()
}

/// Lookup key stored in the database.
pub fn hash_token(token: &str) -> String {
    hex::encode(Sha256::digest(token.trim().to_ascii_lowercase().as_bytes()))
}

/// Cheap shape check before touching the database: 26 chars of base32.
pub fn plausible_token(token: &str) -> bool {
    token.len() == 26 && token.bytes().all(|b| matches!(b, b'a'..=b'z' | b'2'..=b'7'))
}

/// Random URL-safe secret (sessions, invite codes, CSRF).
pub fn random_secret(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    rand::rng().fill_bytes(&mut buf);
    BASE32_NOPAD.encode(&buf).to_ascii_lowercase()
}

pub fn sha256_hex(s: &str) -> String {
    hex::encode(Sha256::digest(s.as_bytes()))
}

/// Keyed hash of an IP for abuse correlation without storing raw addresses.
pub fn ip_hash(secret: &[u8], ip: &str) -> String {
    let mut mac = HmacSha256::new_from_slice(secret).expect("hmac");
    mac.update(b"ip:");
    mac.update(ip.as_bytes());
    hex::encode(&mac.finalize().into_bytes()[..8])
}

pub fn constant_time_eq(a: &str, b: &str) -> bool {
    use subtle::ConstantTimeEq;
    a.len() == b.len() && bool::from(a.as_bytes().ct_eq(b.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_stable_and_versioned() {
        let s = [7u8; 32];
        let t1 = household_token(&s, 1, 1);
        assert_eq!(t1, household_token(&s, 1, 1));
        assert_ne!(t1, household_token(&s, 1, 2));
        assert_ne!(t1, household_token(&s, 2, 1));
        assert!(plausible_token(&t1), "{t1}");
        assert_eq!(hash_token(&t1), hash_token(&t1.to_ascii_uppercase()));
    }
}
