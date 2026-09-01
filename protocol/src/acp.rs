//! protocol/src/acp.rs — exact schema per agentclientprotocol.com/protocol/v1/schema

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SessionId(pub String);

// --- initialize ---

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeRequest {
    /// CHANGED: was String. Per ACP the protocol version is a number, and the
    /// gateway must send it as a number; tolerance is appropriate at ingestion,
    /// not in our own representation (Postel's law: liberal in what you accept,
    /// strict in what you emit). Previously the internal type did not match
    /// the protocol, and the gateway sent the agent the string "1" — claurst
    /// swallowed it, a strict parser would have rejected it.
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
    /// Protocol version. claurst answers with a number (1), some
    /// implementations send a string — we accept both, store and return a number.
    // default = a function, not Default::default(): for u32 it equals 0,
    // while an absent field means version 1, not a zero one.
    #[serde(default = "default_protocol_version", deserialize_with = "de_protocol_version")]
    pub protocol_version: ProtocolVersion,
    #[serde(default)]
    pub agent_capabilities: AgentCapabilities,
    #[serde(default)]
    pub agent_info: Option<Implementation>,
    #[serde(default)]
    pub auth_methods: Vec<AuthMethod>,
}

/// ACP version. Always serialized as a number.
pub type ProtocolVersion = u32;

/// Default version when the agent did not send the field.
pub const DEFAULT_PROTOCOL_VERSION: ProtocolVersion = 1;

fn default_protocol_version() -> ProtocolVersion {
    DEFAULT_PROTOCOL_VERSION
}

/// Lenient ingestion: a number, a numeric string, or a string like "1.0"
/// (we take the major part). Anything unparsable is an error at the boundary,
/// not garbage that reached the logic.
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
    #[serde(default, deserialize_with = "de_bool_lenient")]
    pub close: bool,
    #[serde(default, deserialize_with = "de_bool_lenient")]
    pub delete: bool,
    #[serde(default, deserialize_with = "de_bool_lenient")]
    pub list: bool,
    #[serde(default, deserialize_with = "de_bool_lenient")]
    pub resume: bool,
}

/// ADDED (hermes integration, T-002): sessionCapabilities is parsed
/// leniently — both a bool (claurst) and any other JSON node (hermes sends
/// objects: `list: {}`, `resume: {}`). The field value is not read anywhere,
/// so foreign shapes must not break initialize deserialization.
/// Not a replacement of bool with another type, but adding tolerance at input.
fn de_bool_lenient<'de, D>(d: D) -> Result<bool, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Lenient {
        B(bool),
        Any(serde::de::IgnoredAny),
    }

    Ok(match Deserialize::deserialize(d)? {
        Lenient::B(b) => b,
        Lenient::Any(_) => false,
    })
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
    /// ADDED (audit P2-1): the agent's substantive response. It arrives from
    /// the agent via session/update notifications (AgentMessageChunk) —
    /// previously there was nowhere to put them and they were discarded, so
    /// the A2A client got a Task with no Parts at all.
    /// The field is optional: agents that put content directly into the
    /// session/prompt result work too.
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
