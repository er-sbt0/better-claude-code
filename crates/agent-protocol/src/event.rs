use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

use crate::ids::{RunId, SessionId, ToolUseId};
use crate::usage::Usage;

/// Version of the event payload schema. The log outlives the code, so every
/// event records which shape it was written in. Bump on any breaking payload
/// change; never repurpose a variant name (design §7).
pub const SCHEMA_VERSION: u32 = 1;

/// One observed fact about what a provider did.
///
/// Raw events are append-only and immutable. Anything we *concluded* rather
/// than observed is a projection and belongs somewhere else (design §8).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct RawEvent {
    pub run_id: RunId,
    /// Monotonic within a run. Ordering and gap detection.
    #[ts(type = "number")]
    pub seq: u64,
    /// ISO 8601, from our runtime — never from the agent.
    pub ts: String,
    pub schema: u32,
    /// Stable across a replay of the same provider message, so a resumed or
    /// re-read stream deduplicates instead of duplicating.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
    #[serde(flatten)]
    pub event: Event,
}

impl RawEvent {
    /// The dotted, lowercase type name — the value that goes in the `type`
    /// column and appears on the wire.
    pub fn type_name(&self) -> &'static str {
        self.event.type_name()
    }
}

/// The raw event vocabulary.
///
/// Serializes as `{"type": "...", "payload": {...}}`, flattened into
/// [`RawEvent`]'s envelope.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "type", content = "payload")]
#[ts(export)]
pub enum Event {
    #[serde(rename = "run.started")]
    RunStarted(RunStarted),
    #[serde(rename = "run.finished")]
    RunFinished(RunFinished),
    #[serde(rename = "run.failed")]
    RunFailed(RunFailed),
    /// Provider-side throttling. Run state, not conversation content.
    #[serde(rename = "run.rate_limited")]
    RunRateLimited(RunRateLimited),

    /// A turn began. Not in design §8's original list: recording showed that a
    /// run and a turn are different lifetimes, and conflating them was the
    /// single most common mapping error (§5).
    #[serde(rename = "turn.started")]
    TurnStarted(TurnStarted),
    /// A turn ended, with the usage it cost. Cost lives here, per turn, because
    /// that is the granularity the provider reports it at.
    #[serde(rename = "turn.finished")]
    TurnFinished(TurnFinished),

    #[serde(rename = "user.message")]
    UserMessage(UserMessage),
    #[serde(rename = "assistant.message")]
    AssistantMessage(AssistantMessage),
    #[serde(rename = "assistant.thinking")]
    AssistantThinking(AssistantThinking),

    #[serde(rename = "tool.started")]
    ToolStarted(ToolStarted),
    #[serde(rename = "tool.completed")]
    ToolCompleted(ToolCompleted),

    #[serde(rename = "permission.requested")]
    PermissionRequested(PermissionRequested),
    #[serde(rename = "permission.resolved")]
    PermissionResolved(PermissionResolved),

    /// A call to our own workspace MCP server (design §6 source 3).
    #[serde(rename = "workspace.report")]
    WorkspaceReport(WorkspaceReport),

    /// PTY output from a tier 3 agent.
    #[serde(rename = "process.output")]
    ProcessOutput(ProcessOutput),

    /// Something we could not classify, kept verbatim.
    ///
    /// This variant is what makes the degradation invariant true: a normalizer
    /// bug costs fidelity, never information (design §7). It is expected to be
    /// non-empty in normal operation — provider messages we have no product
    /// use for land here rather than being dropped.
    #[serde(rename = "raw.unclassified")]
    Unclassified(Unclassified),
}

impl Event {
    pub fn type_name(&self) -> &'static str {
        match self {
            Event::RunStarted(_) => "run.started",
            Event::RunFinished(_) => "run.finished",
            Event::RunFailed(_) => "run.failed",
            Event::RunRateLimited(_) => "run.rate_limited",
            Event::TurnStarted(_) => "turn.started",
            Event::TurnFinished(_) => "turn.finished",
            Event::UserMessage(_) => "user.message",
            Event::AssistantMessage(_) => "assistant.message",
            Event::AssistantThinking(_) => "assistant.thinking",
            Event::ToolStarted(_) => "tool.started",
            Event::ToolCompleted(_) => "tool.completed",
            Event::PermissionRequested(_) => "permission.requested",
            Event::PermissionResolved(_) => "permission.resolved",
            Event::WorkspaceReport(_) => "workspace.report",
            Event::ProcessOutput(_) => "process.output",
            Event::Unclassified(_) => "raw.unclassified",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct RunStarted {
    /// Adapter id, e.g. `claude-code`.
    pub agent: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_version: Option<String>,
    pub session_id: SessionId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub cwd: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permission_mode: Option<String>,
    pub tools: Vec<String>,
    pub mcp_servers: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct RunFinished {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    pub turns: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct RunFailed {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    pub turns: u32,
    /// Why we consider this failed, in words we chose — not the provider's.
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct RunRateLimited {
    pub detail: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct TurnStarted {
    /// Zero-based within the run.
    pub index: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct TurnFinished {
    pub index: u32,
    /// The provider's own `subtype`, e.g. `success`, `error_max_turns`.
    ///
    /// This is the failure signal, **not** `is_error`: a run whose every tool
    /// call was permission-denied still reports `is_error: false` with
    /// subtype `success` (design §5).
    pub subtype: String,
    pub ok: bool,
    pub usage: Usage,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(type = "number | null")]
    pub duration_ms: Option<u64>,
    /// Final assistant text for the turn, as the provider summarized it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// How many tool uses were refused during this turn.
    pub permission_denials: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct UserMessage {
    pub text: String,
    /// True when this is the provider echoing our own stdin back at us under
    /// `--replay-user-messages`. It is an acknowledgement, not new content, and
    /// a UI that renders it as a message shows the user their prompt twice.
    pub is_replay: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct AssistantMessage {
    pub text: String,
    /// Provider message id. One API message fans out into several stream lines
    /// that share it (design §5) — anything aggregating per message must group
    /// on this, not on line order.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    /// Index of this content block within its message.
    pub block_index: u32,
    /// Set when this came from a subagent rather than the main loop.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_tool_use_id: Option<ToolUseId>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct AssistantThinking {
    /// Empty unless the agent was launched with `--thinking-display summarized`.
    /// The block still arrives either way, carrying only a signature, so an
    /// empty string here means "not disclosed", never "did not think" (§5).
    pub text: String,
    pub disclosed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    pub block_index: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_tool_use_id: Option<ToolUseId>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ToolStarted {
    pub tool_use_id: ToolUseId,
    pub name: String,
    /// Typed arguments, verbatim. This is design §6 source 1: `Edit` gives
    /// `{replace_all, file_path, old_string, new_string}` with an absolute
    /// path, so changed files are exact and need no inference.
    pub input: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_tool_use_id: Option<ToolUseId>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ToolCompleted {
    pub tool_use_id: ToolUseId,
    pub is_error: bool,
    /// What the tool returned to the model.
    pub content: String,
    /// The richer result, when the provider sent one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<ToolResult>,
    /// Present when the tool never actually ran. Machine-readable, unlike the
    /// prose in `content`, and the only reliable way to tell a refusal from a
    /// failure (design §5).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub non_execution: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_tool_use_id: Option<ToolUseId>,
}

/// Claude Code's `tool_use_result` is an object on success and a bare string on
/// failure — the type itself varies with the outcome. Modeling that honestly
/// costs one enum; assuming it away costs a parser that panics the first time a
/// command exits non-zero.
///
/// Note what is *not* here: an exit code. It is not on the wire in either shape
/// (design §6).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(untagged)]
#[ts(export)]
pub enum ToolResult {
    // Order matters: `Value` matches anything, so it has to come last or every
    // string result deserializes as `Structured`.
    Text(String),
    Structured(Value),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PermissionRequested {
    pub tool_use_id: ToolUseId,
    pub tool_name: String,
    pub input: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PermissionResolved {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_use_id: Option<ToolUseId>,
    pub tool_name: String,
    pub allowed: bool,
    /// Provider's machine-readable reason code, e.g. `asyncAgent`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct WorkspaceReport {
    /// Which workspace MCP tool the agent called, e.g. `report_finding`.
    pub tool: String,
    pub payload: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ProcessOutput {
    pub stream: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Unclassified {
    /// The provider's own `type`, when the line was at least valid JSON.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_subtype: Option<String>,
    /// Why we kept it rather than interpreting it.
    pub reason: UnclassifiedReason,
    /// The original line, byte for byte.
    pub line: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum UnclassifiedReason {
    /// The line was not valid JSON.
    NotJson,
    /// Valid JSON, but no `type` we recognize.
    UnknownType,
    /// A type we recognize but deliberately do not model yet — hook lifecycle,
    /// for instance. Preserved rather than interpreted.
    NotModeled,
    /// The shape was recognized but a field we needed was missing or the wrong
    /// type. The interesting one: a spike here means the provider changed
    /// something (design §16).
    MalformedShape,
}
