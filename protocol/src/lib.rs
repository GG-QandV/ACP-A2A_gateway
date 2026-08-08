//! protocol/src/lib.rs
//! Типы и (de)serialize для ACP и A2A. Не знает о Reply<T>, о стриминге,
//! о конвертации — только протокольные структуры "как в каноне".

pub mod acp;
pub mod a2a;

pub use acp::*;
pub use a2a::*;

#[cfg(test)]
mod tests {
    use crate::acp::{ContentBlock, PromptResponse, StopReason};

    /// Тест из docs/03-dev-guide-testing.md — в репо отсутствовал.
    #[test]
    fn content_block_roundtrip() {
        let cb = ContentBlock::Text { text: "hello".into() };
        let json = serde_json::to_string(&cb).unwrap();
        let restored: ContentBlock = serde_json::from_str(&json).unwrap();
        assert!(matches!(restored, ContentBlock::Text { text } if text == "hello"));
    }

    /// Новое поле content опционально: ответы старых агентов без него
    /// продолжают десериализоваться.
    #[test]
    fn prompt_response_content_is_optional() {
        let resp: PromptResponse = serde_json::from_str(r#"{"stopReason":"end_turn"}"#).unwrap();
        assert!(matches!(resp.stop_reason, StopReason::EndTurn));
        assert!(resp.content.is_empty());
    }
}
