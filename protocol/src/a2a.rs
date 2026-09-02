//! protocol/src/a2a.rs — exact A2A v1.0 schema (a2a-protocol.org/specification)

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TaskId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ContextId(pub String);

// --- AgentCard (discovery) ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentCard {
    pub name: String,
    pub description: Option<String>,
    pub version: String,
    pub url: String,
    #[serde(default)]
    pub capabilities: AgentCardCapabilities,
    #[serde(default)]
    pub skills: Vec<AgentSkill>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentCardCapabilities {
    #[serde(default)]
    pub streaming: bool,
    #[serde(default)]
    pub push_notifications: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSkill {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
}

// --- TaskState — 8 states ---

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TaskState {
    Unspecified,
    Submitted,
    Working,
    InputRequired,
    AuthRequired,
    Completed,
    Failed,
    Canceled,
    Rejected,
}

impl TaskState {
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::Canceled | Self::Rejected
        )
    }

    pub fn is_interrupted(&self) -> bool {
        matches!(self, Self::InputRequired | Self::AuthRequired)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskStatus {
    pub state: TaskState,
    pub message: Option<Message>,
    pub timestamp: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: TaskId,
    pub context_id: ContextId,
    pub status: TaskStatus,
    #[serde(default)]
    pub history: Option<Vec<Message>>,
    #[serde(default)]
    pub artifacts: Option<Vec<Artifact>>,
    #[serde(default)]
    pub metadata: Option<serde_json::Value>,
}

// --- Message / Part ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: MessageRole,
    pub parts: Vec<Part>,
    #[serde(default)]
    pub message_id: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageRole {
    User,
    Agent,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Part {
    Text { text: String },
    File { file: FilePart },
    Data { data: serde_json::Value },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FilePart {
    pub uri: Option<String>,
    pub bytes: Option<String>,
    pub mime_type: Option<String>,
}

// --- Artifact ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Artifact {
    pub artifact_id: String,
    pub name: Option<String>,
    pub description: Option<String>,
    pub parts: Vec<Part>,
    #[serde(default)]
    pub metadata: Option<serde_json::Value>,
}

// --- message/send params ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SendMessageParams {
    pub message: Message,
    #[serde(default)]
    pub configuration: Option<MessageSendConfiguration>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageSendConfiguration {
    #[serde(default)]
    pub blocking: bool,
    #[serde(default)]
    pub history_length: Option<u32>,
}

// --- push notifications ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PushNotificationConfig {
    pub url: String,
    pub token: Option<String>,
}

// --- SSE events ---

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum A2aEvent {
    TaskStatusUpdate {
        task_id: TaskId,
        status: TaskStatus,
        r#final: bool,
    },
    TaskArtifactUpdate {
        task_id: TaskId,
        artifact: Artifact,
        append: Option<bool>,
    },
    Message(Message),
}
