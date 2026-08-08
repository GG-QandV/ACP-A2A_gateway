//! protocol/src/lib.rs
//! Типы и (de)serialize для ACP и A2A. Не знает о Reply<T>, о стриминге,
//! о конвертации — только протокольные структуры "как в каноне".

pub mod acp;
pub mod a2a;

pub use acp::*;
pub use a2a::*;

#[cfg(test)]
mod tests {
    use crate::acp::{ContentBlock, InitializeResponse as PromptResponseless, PromptResponse, StopReason};

    /// Тест из docs/03-dev-guide-testing.md — в репо отсутствовал.
    #[test]
    fn content_block_roundtrip() {
        let cb = ContentBlock::Text { text: "hello".into() };
        let json = serde_json::to_string(&cb).unwrap();
        let restored: ContentBlock = serde_json::from_str(&json).unwrap();
        assert!(matches!(restored, ContentBlock::Text { text } if text == "hello"));
    }


    /// Регрессия на live-баг: claurst отвечает protocolVersion числом.
    /// Раньше поле было String и рукопожатие падало на каждом спавне.
    #[test]
    fn protocol_version_accepts_number() {
        let resp: PromptResponseless = serde_json::from_str(r#"{"protocolVersion":1}"#).unwrap();
        assert_eq!(resp.protocol_version, 1);
    }

    /// Встречаются реализации со строкой — принимаем и её.
    #[test]
    fn protocol_version_accepts_string_forms() {
        let a: PromptResponseless = serde_json::from_str(r#"{"protocolVersion":"1"}"#).unwrap();
        assert_eq!(a.protocol_version, 1);

        let b: PromptResponseless = serde_json::from_str(r#"{"protocolVersion":"2.1"}"#).unwrap();
        assert_eq!(b.protocol_version, 2, "берётся мажорная часть");
    }

    /// Отсутствующее поле — значение по умолчанию, а не ошибка.
    #[test]
    fn protocol_version_defaults_when_absent() {
        let resp: PromptResponseless = serde_json::from_str("{}").unwrap();
        assert_eq!(resp.protocol_version, crate::acp::DEFAULT_PROTOCOL_VERSION);
    }

    /// Мусор отклоняется на границе, а не доезжает до логики.
    #[test]
    fn protocol_version_rejects_garbage() {
        let parsed: Result<PromptResponseless, _> =
            serde_json::from_str(r#"{"protocolVersion":"не версия"}"#);
        assert!(parsed.is_err());
    }

    /// Строго на выходе: шлюз всегда отправляет число, независимо от
    /// того, в каком виде версию прислал агент.
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

    /// Новое поле content опционально: ответы старых агентов без него
    /// продолжают десериализоваться.
    #[test]
    fn prompt_response_content_is_optional() {
        let resp: PromptResponse = serde_json::from_str(r#"{"stopReason":"end_turn"}"#).unwrap();
        assert!(matches!(resp.stop_reason, StopReason::EndTurn));
        assert!(resp.content.is_empty());
    }
}
