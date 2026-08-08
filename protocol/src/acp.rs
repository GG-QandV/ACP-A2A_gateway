//! protocol/src/acp.rs — точная схема по agentclientprotocol.com/protocol/v1/schema

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SessionId(pub String);

// --- initialize ---

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeRequest {
    /// ИЗМЕНЕНО: было String. По ACP версия протокола — число, и шлюз
    /// обязан слать её числом; толерантность уместна на приёме, но не
    /// в собственном представлении (закон Постела: либерален на входе,
    /// строг на выходе). Раньше внутренний тип не соответствовал
    /// протоколу, и шлюз отправлял агенту строку "1" — claurst это
    /// проглотил, строгий парсер отверг бы.
    #[serde(deserialize_with = "de_protocol_version")]
    pub protocol_version: ProtocolVersion,
    #[serde(default)]
    pub client_capabilities: ClientCapabilities,
    #[serde(default)]
    pub client_info: Option<Implementation>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientCapabilities {
    #[serde(default)]
    pub fs: FsCapabilities,
    #[serde(default)]
    pub terminal: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FsCapabilities {
    #[serde(default)]
    pub read_text_file: bool,
    #[serde(default)]
    pub write_text_file: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Implementation {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeResponse {
    /// Версия протокола. claurst отвечает числом (1), встречаются
    /// реализации со строкой — принимаем оба, храним и отдаём числом.
    // default = функция, а не Default::default(): у u32 он равен 0,
    // а отсутствующее поле означает версию 1, не нулевую.
    #[serde(default = "default_protocol_version", deserialize_with = "de_protocol_version")]
    pub protocol_version: ProtocolVersion,
    #[serde(default)]
    pub agent_capabilities: AgentCapabilities,
    #[serde(default)]
    pub agent_info: Option<Implementation>,
    #[serde(default)]
    pub auth_methods: Vec<AuthMethod>,
}

/// Версия ACP. Сериализуется всегда числом.
pub type ProtocolVersion = u32;

/// Версия по умолчанию, если агент поле не прислал.
pub const DEFAULT_PROTOCOL_VERSION: ProtocolVersion = 1;

fn default_protocol_version() -> ProtocolVersion {
    DEFAULT_PROTOCOL_VERSION
}

/// Либеральный приём: число, строка с числом или строка вида "1.0"
/// (берём мажорную часть). Всё, что не разбирается, — ошибка на границе,
/// а не мусор, доехавший до логики.
fn de_protocol_version<'de, D>(d: D) -> Result<ProtocolVersion, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error as _;

    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Raw {
        Number(u32),
        Text(String),
    }

    match Option::<Raw>::deserialize(d)? {
        None => Ok(DEFAULT_PROTOCOL_VERSION),
        Some(Raw::Number(n)) => Ok(n),
        Some(Raw::Text(s)) => {
            let major = s.split('.').next().unwrap_or("").trim();
            major.parse::<ProtocolVersion>().map_err(|_| {
                D::Error::custom(format!("protocolVersion: не разобрать версию из {s:?}"))
            })
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentCapabilities {
    #[serde(default)]
    pub load_session: bool,
    #[serde(default)]
    pub prompt_capabilities: PromptCapabilities,
    #[serde(default)]
    pub mcp_capabilities: McpCapabilities,
    #[serde(default)]
    pub session_capabilities: SessionCapabilities,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptCapabilities {
    #[serde(default)]
    pub image: bool,
    #[serde(default)]
    pub audio: bool,
    #[serde(default)]
    pub embedded_context: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpCapabilities {
    #[serde(default)]
    pub http: bool,
    #[serde(default)]
    pub sse: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionCapabilities {
    #[serde(default)]
    pub close: bool,
    #[serde(default)]
    pub delete: bool,
    #[serde(default)]
    pub list: bool,
    #[serde(default)]
    pub resume: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthMethod {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
}

// --- session/new ---

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewSessionRequest {
    pub cwd: String,
    #[serde(default)]
    pub mcp_servers: Vec<McpServer>,
    #[serde(default)]
    pub additional_directories: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServer {
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: std::collections::HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewSessionResponse {
    pub session_id: SessionId,
}

// --- session/prompt: ContentBlock ---

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    Image {
        #[serde(rename = "mimeType")]
        mime_type: String,
        data: String,
        uri: Option<String>,
    },
    Audio {
        #[serde(rename = "mimeType")]
        mime_type: String,
        data: String,
    },
    Resource {
        resource: EmbeddedResource,
    },
    ResourceLink {
        uri: String,
        name: String,
        #[serde(rename = "mimeType")]
        mime_type: Option<String>,
        size: Option<u64>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum EmbeddedResource {
    Text { uri: String, text: String, #[serde(rename = "mimeType")] mime_type: Option<String> },
    Blob { uri: String, blob: String, #[serde(rename = "mimeType")] mime_type: Option<String> },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptRequest {
    pub session_id: SessionId,
    pub prompt: Vec<ContentBlock>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    EndTurn,
    MaxTokens,
    MaxTurnRequests,
    Refusal,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptResponse {
    pub stop_reason: StopReason,
    /// ДОБАВЛЕНО (аудит P2-1): содержательный ответ агента. Приходит от
    /// агента через session/update-нотификации (AgentMessageChunk) —
    /// раньше их некуда было положить и они выбрасывались, из-за чего
    /// A2A-клиент получал Task вообще без Part'ов.
    /// Поле опционально: агенты, кладущие контент прямо в результат
    /// session/prompt, тоже работают.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub content: Vec<ContentBlock>,
}

// --- session/update ---

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "sessionUpdate", rename_all = "snake_case")]
pub enum SessionUpdate {
    AgentMessageChunk { message_id: Option<String>, content: ContentBlock },
    ToolCall { tool_call_id: String, title: String, kind: String, status: ToolCallStatus },
    ToolCallUpdate { tool_call_id: String, status: ToolCallStatus, content: Option<Vec<ContentBlock>> },
    Plan { entries: Vec<PlanEntry> },
    UsageUpdate { used: u64, size: u64, cost: Option<Cost> },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolCallStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanEntry {
    pub content: String,
    pub priority: String,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Cost {
    pub amount: f64,
    pub currency: String,
}

// --- session/cancel ---

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CancelNotification {
    pub session_id: SessionId,
}
