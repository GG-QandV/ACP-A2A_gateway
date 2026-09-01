//! protocol/src/lib.rs
//! Types and (de)serialization for ACP and A2A. Knows nothing about Reply<T>,
//! streaming or conversion — protocol structures only, "as in the canon".

pub mod acp;
pub mod a2a;
pub mod a2a_sdk_compat;

pub use acp::*;
pub use a2a::*;

#[cfg(test)]
mod tests {
    use crate::acp::{ContentBlock, InitializeResponse as PromptResponseless, PromptResponse, StopReason};

    /// Test from docs/03-dev-guide-testing.md — missing in the repo.
    #[test]
    fn content_block_roundtrip() {
        let cb = ContentBlock::Text { text: "hello".into() };
        let json = serde_json::to_string(&cb).unwrap();
        let restored: ContentBlock = serde_json::from_str(&json).unwrap();
        assert!(matches!(restored, ContentBlock::Text { text } if text == "hello"));
    }


    /// Regression for a live bug: claurst answers protocolVersion as a number.
    /// The field used to be String, and the handshake failed on every spawn.
    #[test]
    fn protocol_version_accepts_number() {
        let resp: PromptResponseless = serde_json::from_str(r#"{"protocolVersion":1}"#).unwrap();
        assert_eq!(resp.protocol_version, 1);
    }

    /// Some implementations use a string — accept that form too.
    #[test]
    fn protocol_version_accepts_string_forms() {
        let a: PromptResponseless = serde_json::from_str(r#"{"protocolVersion":"1"}"#).unwrap();
        assert_eq!(a.protocol_version, 1);

        let b: PromptResponseless = serde_json::from_str(r#"{"protocolVersion":"2.1"}"#).unwrap();
        assert_eq!(b.protocol_version, 2, "берётся мажорная часть");
    }

    /// A missing field — default value, not an error.
    #[test]
    fn protocol_version_defaults_when_absent() {
        let resp: PromptResponseless = serde_json::from_str("{}").unwrap();
        assert_eq!(resp.protocol_version, crate::acp::DEFAULT_PROTOCOL_VERSION);
    }

    /// Garbage is rejected at the boundary, not let through to the logic.
    #[test]
    fn protocol_version_rejects_garbage() {
        let parsed: Result<PromptResponseless, _> =
            serde_json::from_str(r#"{"protocolVersion":"не версия"}"#);
        assert!(parsed.is_err());
    }

    /// Strict on output: the gateway always sends a number, regardless of
    /// what form the agent sent the version in.
    #[test]
    fn protocol_version_always_serializes_as_number() {
        let req = crate::acp::InitializeRequest {
            protocol_version: 1,
            client_capabilities: Default::default(),
            client_info: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains(r#""protocolVersion":1"#), "версия должна уходить числом: {json}");
        assert!(!json.contains(r#""protocolVersion":"1""#));
    }

    /// The new content field is optional: replies of old agents without it
    /// still deserialize.
    #[test]
    fn prompt_response_content_is_optional() {
        let resp: PromptResponse = serde_json::from_str(r#"{"stopReason":"end_turn"}"#).unwrap();
        assert!(matches!(resp.stop_reason, StopReason::EndTurn));
        assert!(resp.content.is_empty());
    }
}
