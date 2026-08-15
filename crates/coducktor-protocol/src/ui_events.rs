use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The backend that produced a session, from `packages/api-client/src/protocol/ui-events.ts`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UiBackend {
    Claude,
    Codex,
    #[serde(rename = "opencode")]
    OpenCode,
    Pi,
}

/// Tool lifecycle status from the v2 UI protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolStatus {
    Pending,
    Running,
    Completed,
    Failed,
    Declined,
}

/// Tool icon/verb hint from the v2 UI protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolKind {
    Read,
    Edit,
    Delete,
    Move,
    Search,
    Execute,
    Think,
    Fetch,
    Task,
    Plan,
    Other,
}

/// Why a turn or session stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    EndTurn,
    MaxTokens,
    Refusal,
    Cancelled,
    Timeout,
    Error,
}

/// Status of one plan entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanStatus {
    Pending,
    InProgress,
    Completed,
    Cancelled,
}

/// One session plan entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanEntry {
    pub content: String,
    pub status: PlanStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<PlanPriority>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_form: Option<String>,
}

/// Plan priority from the v2 UI protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PlanPriority {
    High,
    Medium,
    Low,
}

/// Raw token counts from the v2 UI protocol.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenUsage {
    pub input: f64,
    pub output: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<f64>,
    pub total: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<f64>,
}

/// One file change carried by an edit item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileDiff {
    pub path: String,
    pub old_text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unified: Option<String>,
}

/// A file location touched by a tool.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolLocation {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<f64>,
}

/// Permission option kind from the v2 UI protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionOptionKind {
    AllowOnce,
    AllowAlways,
    RejectOnce,
    RejectAlways,
}

/// One permission option.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionOption {
    pub id: String,
    pub label: String,
    pub kind: PermissionOptionKind,
}

/// A chat message item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiMessageItem {
    pub id: String,
    pub role: MessageRole,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<MessagePhase>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_item_id: Option<String>,
}

/// Message role from the v2 UI protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageRole {
    Assistant,
    User,
}

/// Message phase from the v2 UI protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessagePhase {
    Commentary,
    Final,
}

/// An extended-thinking item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiReasoningItem {
    pub id: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_item_id: Option<String>,
}

/// A tool invocation with its full lifecycle.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiToolItem {
    pub id: String,
    pub name: String,
    pub tool_kind: ToolKind,
    pub title: String,
    pub status: ToolStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diffs: Option<Vec<FileDiff>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locations: Option<Vec<ToolLocation>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_item_id: Option<String>,
}

/// The id-keyed item union from the v2 UI protocol.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum UiItem {
    #[serde(rename = "message")]
    Message(UiMessageItem),
    #[serde(rename = "reasoning")]
    Reasoning(UiReasoningItem),
    #[serde(rename = "tool")]
    Tool(UiToolItem),
}

/// A structured AskUser option.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UiAskOption {
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// A structured AskUser question.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiAskQuestion {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub header: String,
    pub question: String,
    pub options: Vec<UiAskOption>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub multi_select: Option<bool>,
}

/// Normalized agent-event protocol v2.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum UiEvent {
    #[serde(rename = "session.started")]
    SessionStarted {
        #[serde(rename = "sessionId")]
        session_id: String,
        backend: UiBackend,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tools: Option<Vec<String>>,
    },
    #[serde(rename = "session.ended")]
    SessionEnded {
        reason: StopReason,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
    #[serde(rename = "session.error")]
    SessionError { message: String, fatal: bool },
    #[serde(rename = "turn.started")]
    TurnStarted {
        #[serde(rename = "turnId")]
        turn_id: String,
    },
    #[serde(rename = "turn.completed")]
    TurnCompleted {
        #[serde(rename = "turnId")]
        turn_id: String,
        #[serde(rename = "stopReason")]
        stop_reason: StopReason,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        usage: Option<TokenUsage>,
        #[serde(rename = "costUsd", default, skip_serializing_if = "Option::is_none")]
        cost_usd: Option<f64>,
    },
    #[serde(rename = "item.started")]
    ItemStarted { item: UiItem },
    #[serde(rename = "item.delta")]
    ItemDelta {
        #[serde(rename = "itemId")]
        item_id: String,
        field: ItemDeltaField,
        delta: String,
    },
    #[serde(rename = "item.updated")]
    ItemUpdated { item: UiItem },
    #[serde(rename = "item.completed")]
    ItemCompleted { item: UiItem },
    #[serde(rename = "plan.updated")]
    PlanUpdated { entries: Vec<PlanEntry> },
    #[serde(rename = "permission.requested")]
    PermissionRequested {
        #[serde(rename = "requestId")]
        request_id: String,
        #[serde(rename = "itemId", default, skip_serializing_if = "Option::is_none")]
        item_id: Option<String>,
        title: String,
        options: Vec<PermissionOption>,
    },
    #[serde(rename = "permission.resolved")]
    PermissionResolved {
        #[serde(rename = "requestId")]
        request_id: String,
        #[serde(rename = "optionId")]
        option_id: String,
    },
    #[serde(rename = "ask.requested")]
    AskRequested {
        #[serde(rename = "requestId")]
        request_id: String,
        questions: Vec<UiAskQuestion>,
    },
    #[serde(rename = "usage.updated")]
    UsageUpdated {
        usage: TokenUsage,
        #[serde(rename = "costUsd", default, skip_serializing_if = "Option::is_none")]
        cost_usd: Option<f64>,
    },
    #[serde(rename = "image")]
    Image {
        #[serde(rename = "itemId", default, skip_serializing_if = "Option::is_none")]
        item_id: Option<String>,
        #[serde(rename = "mediaType")]
        media_type: String,
        data: String,
    },
}

/// The streamed field that an item delta appends to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ItemDeltaField {
    Text,
    Reasoning,
    Output,
}

/// All v2 event discriminator values.
pub const UI_EVENT_TYPES: &[&str] = &[
    "session.started",
    "session.ended",
    "session.error",
    "turn.started",
    "turn.completed",
    "item.started",
    "item.delta",
    "item.updated",
    "item.completed",
    "plan.updated",
    "permission.requested",
    "permission.resolved",
    "ask.requested",
    "usage.updated",
    "image",
];
