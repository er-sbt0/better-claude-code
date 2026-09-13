use agent_protocol::RunId;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

/// The document schema version (design §7: "every document carries a
/// `version`").
///
/// It is not the event schema and does not move with it. `agent_protocol::
/// SCHEMA_VERSION` says what a stored *event* looks like and is a promise about
/// the log, which is forever; this says what a *projection* of those events
/// looks like and may be rewritten whenever the projection improves (§8: a
/// projection is recomputable and disposable). A consumer that persists a
/// document — §9.1's derived runs will — reads this to know what it is holding.
pub const DOCUMENT_VERSION: u32 = 1;

/// One run's feed: §7's blocks, in stream order.
///
/// A projection, exactly like `event-log`'s `RunMetrics` and `ForkPoints` and
/// for the same reason: it is recomputed from the log and never stored, so
/// improving the derivation costs a recompute rather than a migration. It lives
/// in a crate rather than in the webview because **deriving is deciding** — a
/// block is a decision about which events belong together, and the shell owns
/// no logic (roadmap §8).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Feed {
    pub run_id: RunId,
    /// [`DOCUMENT_VERSION`] at the moment this was projected.
    pub version: u32,
    pub blocks: Vec<Block>,
}

/// One block of the feed: what it is, and which raw events it came from.
///
/// ## The address
///
/// §7: "Every block carries the provenance of the raw events it was derived
/// from", and calls it a V1 foundation rather than a feature. §9.1 is what
/// spends it — `(runId, seqRange, actionId) → artifact` — and §3 names it the
/// address that collapse, fork and compare all resolve against.
///
/// §7 spells the address `{runId, seqStart, seqEnd}` and this stores
/// [`seqs`](Self::seqs), the exact events, instead. That is a change earned by
/// building it: **spans overlap**. One assistant message may open two tool
/// calls before either returns, so A covers `seq` 11 and 13 while B covers 12
/// and 14, and a span alone cannot say which of them `seq` 12 belongs to. The
/// span is still available — [`seq_start`](Self::seq_start) and
/// [`seq_end`](Self::seq_end) are its ends — and it is still what a range
/// action selects; what the list buys is that "no event is dropped" is
/// *checkable on the document* rather than asserted about the code that made
/// it.
///
/// The reverse also happens and is why one `seq` can appear in two blocks: a
/// single `assistant.message` carrying prose around a fenced example is one
/// chain entry and two blocks (§7's `Text` and `Code`). Both address the same
/// message, which is correct — they are one thing the provider said.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Block {
    pub run_id: RunId,
    /// Every raw event this block is a projection of, ascending. Never empty.
    #[ts(type = "number[]")]
    pub seqs: Vec<u64>,
    /// The `seq` a fork anchored at this block resolves to, or `None` where the
    /// provider gave this block no address.
    ///
    /// **Not a second copy of the rule.** It is
    /// [`normalizer::claude_code::fork_address`] applied to the events in
    /// [`seqs`](Self::seqs) — the same function `EventLog::fork_points` walks
    /// and `driver::ForkPoint::resolve` re-addresses through, so the three
    /// cannot drift. A key is necessary and not sufficient: the provider keys
    /// its `system`, `result` and `rate_limit_event` lines too and refuses a
    /// fork at every one of them
    /// (`docs/verification/claude-code-fork-anchor-types.md`).
    ///
    /// Where a block covers several addressable events — a tool call opens on
    /// an `assistant` line and closes on a `user` one, and both resolve — this
    /// is the **last** of them. "Fork here" means a child that has this block,
    /// which is also how the inherited-message count beside it is computed
    /// (`ForkBlock::inherited_messages` counts messages *at or before* the
    /// `seq`). Anchoring at the first would offer a fork that silently excluded
    /// the result of the call the user pointed at.
    #[ts(type = "number | null")]
    pub fork_seq: Option<u64>,
    /// One line, for the collapsed state.
    ///
    /// Derived here rather than in the renderer for the reason everything else
    /// in this file is: summarizing is deciding. This replaced
    /// `apps/desktop/src/lib/describe.ts`, which was the same job done in
    /// TypeScript where no test in this workspace could reach it.
    pub summary: String,
    #[serde(flatten)]
    pub body: Body,
}

impl Block {
    /// The first event of the block. Panics only on an empty `seqs`, which the
    /// projection never produces.
    pub fn seq_start(&self) -> u64 {
        *self
            .seqs
            .first()
            .expect("a block covers at least one event")
    }

    /// The last event of the block.
    pub fn seq_end(&self) -> u64 {
        *self.seqs.last().expect("a block covers at least one event")
    }

    /// The block's `type` on the wire, which is what a renderer switches on.
    pub fn type_name(&self) -> &'static str {
        self.body.type_name()
    }
}

/// §7's six block types, and no seventh.
///
/// The list is short **on purpose**: "a list designed up front encodes guesses
/// as commitments, and every unused type is schema we are stuck supporting."
/// `Finding`, `Plan`, `TestResult` and `PermissionRequest` are named there as
/// *likely next* and are deliberately absent — three of them arrive through §6
/// source 3, whose other four tools are unbuilt, and the fourth arrives with
/// §12's dialog.
///
/// The consequence lands on run and turn lifecycle, which has no type of its
/// own here. `run.started`, `turn.finished` and their siblings are preserved as
/// [`Body::Raw`] — which is not a shortfall but the invariant working: nothing
/// is dropped, the block says what it is, and the day lifecycle earns a block
/// type it gets one instead of inheriting a guess made today.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "type", content = "block", rename_all = "snake_case")]
#[ts(export)]
pub enum Body {
    /// Prose — the agent's, the user's, or a disclosed thinking block.
    Text(Text),
    /// A fenced, language-tagged code span lifted out of prose.
    Code(Code),
    /// A tool call: name, typed args, result, status, duration.
    ToolCall(ToolCall),
    /// A tool call that edited files, read as the change it made.
    Diff(Diff),
    /// A tool call that ran a command, read as the command and its output.
    Terminal(Terminal),
    /// Anything this projection does not interpret, kept verbatim (§7's
    /// degradation invariant).
    Raw(Raw),
}

impl Body {
    pub fn type_name(&self) -> &'static str {
        match self {
            Body::Text(_) => "text",
            Body::Code(_) => "code",
            Body::ToolCall(_) => "tool_call",
            Body::Diff(_) => "diff",
            Body::Terminal(_) => "terminal",
            Body::Raw(_) => "raw",
        }
    }
}

/// Who said it.
///
/// Read off the provider's own line type and never invented — which is the
/// distinction that makes this legitimate where `role` was not
/// (`docs/branching-surface.md`: a run's `role` was a label typed into a mock
/// and there was no such concept anywhere under the window). An `assistant`
/// line, a `user` line and a `thinking` content block are three different
/// things on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum Speaker {
    User,
    Assistant,
    /// Reasoning the provider disclosed. Empty text with
    /// [`Text::disclosed`] false means "not disclosed", never "did not
    /// think" (§5).
    Thinking,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Text {
    pub speaker: Speaker,
    pub text: String,
    /// Only meaningful for [`Speaker::Thinking`]: false means the run was not
    /// launched with `--thinking-display summarized`, so the block arrived
    /// carrying a signature and no text.
    pub disclosed: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Code {
    /// The fence's info string, lower-cased, or `None` for a bare fence.
    pub language: Option<String>,
    pub code: String,
}

/// What became of a tool call. Shared by [`ToolCall`], [`Diff`] and
/// [`Terminal`], which are three readings of the same three events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ToolStatus {
    /// Started and not yet answered — the live case, and the reason the feed is
    /// projected from the whole log rather than appended to.
    Running,
    Ok,
    /// Ran and failed.
    Failed,
    /// Never ran. `tool.completed.non_execution` is the only machine-readable
    /// way to tell this from a failure (§5), and a run whose every call was
    /// refused still reports `subtype: success`.
    Denied,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ToolCall {
    /// `Read`, `Grep`, `mcp__agent-workspace__report_finding`, …
    pub name: String,
    /// Typed arguments, verbatim (§6 source 1).
    pub input: Value,
    pub status: ToolStatus,
    /// What the tool returned to the model, when it returned.
    pub output: Option<String>,
    /// Why it did not run, when it did not (`permission-rule`, …).
    pub non_execution: Option<String>,
    /// Wall time between `tool.started` and `tool.completed`, from our own
    /// timestamps (§8: `ts` comes from the runtime, never from the agent).
    #[ts(type = "number | null")]
    pub duration_ms: Option<u64>,
    /// Set when a subagent made the call rather than the main loop.
    pub parent_tool_use_id: Option<String>,
}

/// A file change, read off the tool call that made it.
///
/// §6 source 1 is why this is exact rather than inferred: `Edit` carries
/// `{file_path, old_string, new_string}` with an absolute path, so the hunk is
/// the arguments and not a guess. It is **not** §13.1's attribution, which
/// blames a shadow history and catches `Bash` and formatters; this is what the
/// agent said it was doing, which is the right thing to show beside what it
/// said.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Diff {
    /// `Edit`, `Write`, `MultiEdit`.
    pub tool: String,
    pub files: Vec<String>,
    pub hunks: Vec<Hunk>,
    pub stats: DiffStats,
    pub status: ToolStatus,
    #[ts(type = "number | null")]
    pub duration_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Hunk {
    pub file: String,
    pub lines: Vec<DiffLine>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct DiffLine {
    pub kind: DiffLineKind,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum DiffLineKind {
    Context,
    Added,
    Removed,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct DiffStats {
    pub additions: u32,
    pub deletions: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Terminal {
    pub command: String,
    /// The `description` argument, which Claude Code's `Bash` tool carries.
    pub description: Option<String>,
    pub output: String,
    /// **Always `None`, and the field is here to say so.** §7 specifies
    /// "command + output + exit code"; §6's verification found the exit code is
    /// not on the wire in either shape of `tool_use_result`. So the status
    /// below is what we actually know, and this stays an honest hole rather
    /// than a number invented from `is_error`. It fills in the day a tier 3 PTY
    /// agent (§5) or a richer result gives us one.
    pub exit_code: Option<i32>,
    pub status: ToolStatus,
    #[ts(type = "number | null")]
    pub duration_ms: Option<u64>,
}

/// The degradation invariant, as a type.
///
/// §7: "No event is ever dropped. Anything we cannot classify renders as a raw
/// block, and the unprocessed transcript is always one click away." Two
/// different things land here and the field says which:
///
/// - a `raw.unclassified` event, where the *normalizer* could not classify the
///   provider's line — [`line`](Self::line) is then that line, byte for byte;
/// - an event this *projection* has no block type for — run and turn
///   lifecycle, and any event type added to the protocol after this code was
///   written. [`payload`](Self::payload) is the event's own payload, so
///   nothing is lost and the renderer has something true to show.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Raw {
    /// The raw event's dotted type name, e.g. `turn.finished`.
    pub event_type: String,
    /// The event payload, verbatim.
    pub payload: Value,
    /// The provider's original line, for a `raw.unclassified` event only.
    pub line: Option<String>,
}
