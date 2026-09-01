//! core/src/owner.rs
//!
//! Owner of a conversation and a task. Extracted from convert.rs into a
//! separate module because after closing audit P1-2 it is used by task_store
//! as well: the owner must survive session eviction, otherwise the
//! "whose task?" check works only while the conversation is alive.
//!
//! The token hash is stored, not the token itself: equality is enough to
//! answer the "same client?" question, and there is no reason to keep a
//! secret in memory and on disk longer than necessary.

use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// Default key — ONLY for local development. In prod, setting
/// GATEWAY_HMAC_KEY via the environment is mandatory.
const DEFAULT_DEV_KEY: &str = "default-dev-key-do-not-use-in-prod";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Owner {
    /// Calls through the bare `A2aAgent` trait, without transport context.
    /// A separate bucket: anonymous calls are isolated from token-bearing ones.
    Anonymous,
    Token {
        hash: u64,
    },
}

impl Owner {
    pub fn from_token(token: &str) -> Self {
        // FIXED (TECH_DEBT: token hash — HMAC): RandomState replaced
        // with HMAC-SHA256 keyed from {env:GATEWAY_HMAC_KEY}. This is
        // a cryptographic hash, not just SipHash with a random seed.
        // The first 8 HMAC bytes go into hash: u64 — the Owner::Token format
        // is unchanged, StoredTask requires no migration.
        let key = std::env::var("GATEWAY_HMAC_KEY").unwrap_or_else(|_| DEFAULT_DEV_KEY.to_string());
        let mut mac =
            HmacSha256::new_from_slice(key.as_bytes()).expect("HMAC accepts any key length");
        mac.update(token.as_bytes());
        let result = mac.finalize();
        let hash_bytes: [u8; 8] = result.into_bytes()[..8].try_into().unwrap();
        Owner::Token {
            hash: u64::from_le_bytes(hash_bytes),
        }
    }

    /// The hash is not cryptographic and is intended only for
    /// equality comparison. The token cannot be recovered from it,
    /// but it should not be relied upon as a secret either.
    pub fn is_anonymous(&self) -> bool {
        matches!(self, Owner::Anonymous)
    }

    /// Returns true if the token yields the same Owner as this one.
    /// Needed for tests to verify that HMAC is deterministic.
    #[cfg(test)]
    pub fn same_token_as(&self, token: &str) -> bool {
        *self == Owner::from_token(token)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_token_gives_same_owner() {
        assert_eq!(Owner::from_token("t-1"), Owner::from_token("t-1"));
        // With HMAC this remains true: one token → one hash (determinism).
    }

    #[test]
    fn different_tokens_give_different_owners() {
        assert_ne!(Owner::from_token("t-1"), Owner::from_token("t-2"));
        // With HMAC this also remains true: different tokens → different hashes.
    }

    #[test]
    fn hmac_is_deterministic_for_same_token() {
        // Check that from_token is deterministic (one token → one Owner)
        // even with HMAC. This is not a test of cryptographic strength — only
        // of implementation correctness.
        let owner = Owner::from_token("test-token");
        assert!(owner.same_token_as("test-token"));
        assert!(!owner.same_token_as("other-token"));
    }

    #[test]
    fn anonymous_never_equals_token_owner() {
        assert_ne!(Owner::Anonymous, Owner::from_token("t-1"));
    }

    #[test]
    fn owner_survives_serde_roundtrip() {
        let owner = Owner::from_token("t-1");
        let json = serde_json::to_string(&owner).unwrap();
        let restored: Owner = serde_json::from_str(&json).unwrap();
        assert_eq!(owner, restored);
    }
}
