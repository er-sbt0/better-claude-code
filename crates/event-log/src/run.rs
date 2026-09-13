use agent_protocol::{ModelUsage, RunId, SessionId};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::{Error, Result};

/// Identity and lifecycle of a run.
///
/// Note what is absent: the token and cost columns §14 lists on the run entity.
/// They are recomputable from `turn.finished`, which by §8's rule makes them a
/// projection — see [`RunMetrics`]. Caching them back onto this row is a
/// legitimate optimization later; making them the source of truth is not.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Run {
    pub id: RunId,
    pub agent: String,
    pub agent_version: Option<String>,
    pub provider_session_id: Option<SessionId>,
    pub workspace_path: String,
    pub worktree_path: Option<String>,
    pub branch: Option<String>,
    /// `refs/agent-workspace/runs/<id>` (design §13.1). `None` means Review has
    /// no attribution for this run and must say so.
    pub shadow_ref: Option<String>,
    pub prompt: Option<String>,
    /// The model the run was launched with (`LaunchSpec::model`), or `None`
    /// when the launch named none and the provider used its own default.
    ///
    /// Stored rather than derived, and `schema.rs`'s V3 comment carries the
    /// argument: the model is a fact about the *launch*, which the log never
    /// sees. A fork inherits its parent's at `driver::fork`, so the column is
    /// per-run and a later surface can change one branch's without touching
    /// what the parent was launched with.
    pub model: Option<String>,
    /// The rest of what the run was launched with, so a fork or a resume can
    /// restore it (`schema.rs`, V4). Empty for a run recorded before the
    /// column existed, and for one launched with nothing — the same state,
    /// deliberately.
    pub launch_flags: LaunchFlags,
    pub status: RunStatus,
    pub started_at: String,
    pub finished_at: Option<String>,
    /// Which run this one was forked from (§19). `None` for a root run.
    pub parent_run_id: Option<RunId>,
    /// *Where* in the parent it was forked from: a `seq` in the **parent's**
    /// log, naming the event the fork was anchored at.
    ///
    /// This and [`Run::parent_run_id`] are one fact in two columns, and they
    /// are the only record the graph edge will ever have. The provider records
    /// no parentage — a forked transcript is a full copy re-stamped with its
    /// own session id
    /// (`docs/verification/claude-code-session-forking.md`) — so unlike
    /// [`RunMetrics`], which §8 makes a projection because it is recomputable,
    /// this is recomputable from nothing at all. It is written once, at the
    /// fork, or it is lost.
    ///
    /// The corresponding `raw_events.idempotency_key` is the chain-entry uuid
    /// `--resume-session-at` accepts, which is what makes a stored `seq` enough
    /// to re-address the branch point later
    /// (`docs/verification/claude-code-fork-address.md`).
    #[ts(type = "number | null")]
    pub branch_point_seq: Option<u64>,
}

impl Run {
    /// A root run has neither a parent nor a branch point; a fork has both.
    ///
    /// Enforced here rather than as a SQL `CHECK`, for two reasons that agree.
    /// SQLite cannot add a `CHECK` through `ALTER TABLE` without rebuilding the
    /// table, and the repo's standing rule is that anything which starts
    /// deciding belongs where a test can reach it — which here means without a
    /// database at all.
    ///
    /// Half an edge is worse than none: a parent with no branch point draws an
    /// arrow the UI cannot anchor, and a branch point with no parent names a
    /// `seq` in a log nobody can name. Both are silent if they are allowed to
    /// be stored, and both are unrecoverable afterwards, since nothing else
    /// remembers the fork.
    pub fn check_fork_pairing(&self) -> Result<()> {
        match (&self.parent_run_id, self.branch_point_seq) {
            (Some(_), Some(_)) | (None, None) => Ok(()),
            _ => Err(Error::IncompleteForkEdge {
                run: self.id.clone(),
                parent_run_id: self.parent_run_id.clone(),
                branch_point_seq: self.branch_point_seq,
            }),
        }
    }
}

/// What a run was launched with, beyond [`Run::model`]: the policy it runs
/// under and the bounds it runs inside.
///
/// **Why this is stored at all.** §8's rule is that anything recomputable from
/// the log is a projection and must not be the source of truth. None of this is
/// recomputable: the log records what the agent *did*, never what it was
/// *permitted* to do, and a denial reaches the log as a refused tool call with
/// no statement of the allow-list it failed. So this is the same exception
/// [`Run::branch_point_seq`] is — written once, at launch, or lost — and the
/// consequence of losing it is concrete: a fork or a resume that pre-grants
/// nothing is refused whatever its parent had, under §12's `DenyPrompts`
/// posture where nothing may prompt and anything not pre-granted is denied
/// outright. The agent then works around it silently.
///
/// **Why one column of JSON rather than seven columns.** §14 keeps `runs`
/// deliberately thin, and the cost of a column here is not the column: every
/// query in `log.rs` names the whole column list and `row_to_run` reads it
/// back *positionally*, so each new column is an edit in five places and a
/// mis-ordering hazard the V2→V3 migration test exists to catch. That is the
/// price of a column that is *queried*, and none of these ever will be — nobody
/// asks the log for "runs with `Edit` pre-granted". This is one opaque value
/// read and written whole, which is what `raw_events.payload` already is and
/// for the same reason.
///
/// **The blob is a wire format, and this is how it survives the struct
/// changing.** `#[serde(default)]` on the container, not on each field: a field
/// added later is absent from every blob already on disk and decodes to its
/// default without a line of migration, and there is one attribute to not
/// forget rather than one per field. A field *removed* later is ignored on
/// read, which serde does by default. There is deliberately no version tag —
/// a tag is only worth its weight when a reader has to branch on it, and a
/// defaulted field is the same migration that a version bump would have had to
/// spell out anyway.
///
/// **What is not here, and why.** The excluded half of `driver::LaunchSpec` is
/// not an oversight; each field is excluded for a reason of its own:
///
/// - **`model`** is already [`Run::model`] (V3). Storing it twice is the one
///   thing §8 forbids more firmly than storing a projection.
/// - **`binary`** is a property of *this machine*. A run resumed next year
///   against a path that no longer exists is worse than one resumed against
///   whatever `claude` is now.
/// - **`cwd`** is [`Run::worktree_path`], and §13 overwrites it at every launch
///   anyway: the agent's working directory is the tree its run owns, and that is
///   not a caller's to choose.
/// - **`session_id`** is [`Run::provider_session_id`] (§14: we assign it).
/// - **`resume` and `fork_at`** are not properties of the run — they *are* the
///   resume and the fork, computed by the driver from the row and the branch
///   point.
/// - **`mcp_config`** names a socket this process created; it dies with the
///   process that made it.
/// - **`permission`** records which of §12's postures *this process* arranged,
///   and `driver::wire_workspace_mcp` overwrites it at every launch depending
///   on whether a workspace MCP server is hosted. Storing it would file one
///   session's hosting arrangement as a fact about the run.
/// - **`thinking` and `partial_messages`** are how the watcher watches, not what
///   the run may do. Neither changes what the agent is permitted or how far it
///   goes, and both are already whatever the driver defaults them to unless a
///   caller opts out — so inheriting them would mostly be inheriting a default.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[serde(default)]
#[ts(export)]
pub struct LaunchFlags {
    /// Pre-granted tools. Under `DenyPrompts` this is the whole of what the run
    /// may do, which is why losing it is the bug this record exists to fix.
    pub allowed_tools: Vec<String>,
    pub disallowed_tools: Vec<String>,
    pub permission_mode: Option<String>,
    #[ts(type = "number | null")]
    pub max_turns: Option<u32>,
    pub max_budget_usd: Option<f64>,
    /// §6: a tool the agent is never told about is a tool it never calls, so
    /// this is instruction rather than observation — what the run may do, said
    /// in prose.
    ///
    /// Recorded as the *caller* set it, before the driver appends the workspace
    /// MCP server's own paragraph to it. Storing the appended value would
    /// append it a second time on every resume.
    pub append_system_prompt: Option<String>,
    /// `LaunchSpec::extra`, verbatim.
    ///
    /// Kept even though we have no opinion about what is in it, and *because*
    /// we have none: a flag this struct cannot classify is a flag we cannot
    /// argue is safe to drop, and dropping it silently is the same class of bug
    /// as dropping the allow-list.
    pub extra: Vec<String>,
}

impl LaunchFlags {
    /// Nothing was recorded — an old row, or a run launched with no flags.
    ///
    /// One predicate for both on purpose. A V3-era row *may* have had an
    /// allow-list that nothing wrote down, and there is no way to recover it;
    /// pre-granting tools we cannot show were granted would be a guess stored
    /// as a fact, and the empty reading is the one that fails closed.
    pub fn is_empty(&self) -> bool {
        self == &LaunchFlags::default()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum RunStatus {
    Running,
    Finished,
    Failed,
    /// Still `Running` in the log when the app restarted, so its process is
    /// gone. The log cannot detect this on its own — it has no processes. The
    /// driver reconciles at startup from [`crate::EventLog::running_runs`];
    /// §8 calls this crash recovery.
    Abandoned,
    /// The process was killed rather than allowed to end: the user stopped the
    /// run, or the app exited under it and `Registry::shutdown` interrupted
    /// every live run.
    ///
    /// **An interrupt is not a failure and it is not a success**, and this
    /// variant exists because the four statuses before it forced a run that
    /// was killed to be one or the other. Nothing closes a run's stdin on its
    /// behalf — a run is a conversation and the user decides when it is over —
    /// so *every* live run is interrupted at app exit, and a status derived
    /// from the exit code alone filed every one of them as `Failed`, over logs
    /// that said `turn.finished`, `run.finished`, and no `run.failed`
    /// anywhere. `Failed` is a claim about the agent's work; the canvas, the
    /// runs list and the inspector all repeat it.
    ///
    /// Not [`RunStatus::Abandoned`], which is the neighbouring case and a
    /// different fact. Abandoned means *we lost track of it*: the row still
    /// said `Running` at boot, nobody watched the process go, and the log may
    /// stop anywhere. Interrupted means *we ended it*, at a moment we know,
    /// with the run's last events already written. One is reconciled from
    /// ignorance at startup, the other is recorded by the thread holding the
    /// session — and telling a user who pressed stop that their run was
    /// "abandoned" describes neglect that did not happen.
    Interrupted,
}

impl RunStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            RunStatus::Running => "running",
            RunStatus::Finished => "finished",
            RunStatus::Failed => "failed",
            RunStatus::Abandoned => "abandoned",
            RunStatus::Interrupted => "interrupted",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "running" => RunStatus::Running,
            "finished" => RunStatus::Finished,
            "failed" => RunStatus::Failed,
            "abandoned" => RunStatus::Abandoned,
            "interrupted" => RunStatus::Interrupted,
            _ => return None,
        })
    }
}

/// What a run cost and did, recomputed from the log.
///
/// This is a projection (§8): drop it, recompute it, it is never stored. It is
/// also the answer to "are the columns users check first actually derivable?" —
/// they are, from `turn.finished` alone.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct RunMetrics {
    pub turns: u32,
    /// Summed over turns. §5: `result` is per turn, not per run, so a run's
    /// cost is a sum and never the last value seen.
    pub cost_usd: f64,
    #[ts(type = "number")]
    pub input_tokens: u64,
    #[ts(type = "number")]
    pub output_tokens: u64,
    #[ts(type = "number")]
    pub cache_creation_tokens: u64,
    #[ts(type = "number")]
    pub cache_read_tokens: u64,
    #[ts(type = "number")]
    pub thinking_tokens: u64,
    /// Merged across turns and kept so the totals can be audited (§14).
    pub by_model: Vec<ModelUsage>,
    pub tool_calls: u32,
    pub permission_denials: u32,
    /// Summed over turns, when the provider reported it.
    #[ts(type = "number | null")]
    pub duration_ms: Option<u64>,
}

impl RunMetrics {
    pub(crate) fn add_model_usage(&mut self, m: &ModelUsage) {
        self.input_tokens += m.input_tokens;
        self.output_tokens += m.output_tokens;
        self.cache_creation_tokens += m.cache_creation_tokens;
        self.cache_read_tokens += m.cache_read_tokens;
        self.thinking_tokens += m.thinking_tokens;
        match self.by_model.iter_mut().find(|e| e.model == m.model) {
            Some(e) => {
                e.input_tokens += m.input_tokens;
                e.output_tokens += m.output_tokens;
                e.cache_creation_tokens += m.cache_creation_tokens;
                e.cache_read_tokens += m.cache_read_tokens;
                e.thinking_tokens += m.thinking_tokens;
                e.cost_usd += m.cost_usd;
            }
            None => self.by_model.push(m.clone()),
        }
    }
}

/// The three-word status vocabulary, and the message count beside it.
///
/// A projection like [`RunMetrics`] and for the same reason: recomputed from
/// the log, never stored. It lives here rather than in the shell because
/// deriving it is *deciding*, and the shell owns no logic — "anything there
/// that starts deciding belongs in a crate, where a test can reach it without a
/// window" (roadmap §8). What the shell is left with is a map from three values
/// to three colours.
///
/// `docs/branching-surface.md` fixes the vocabulary at three words and
/// propagates them to border, pill, sliver and card, so this is what a node's
/// header says about itself.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct RunActivity {
    pub run_id: RunId,
    pub status: Activity,
    /// **Messages, not events.** A run with 200 events does not have 200
    /// messages: most of a log is tool traffic, permission decisions and
    /// lifecycle, and one assistant message fans out into one event per content
    /// block. [`EventLog::recent_activity`](crate::EventLog::recent_activity)
    /// carries the counting rule; what it counts is what a reader means by "how
    /// long is this conversation", and what §10's card spells `N msgs`.
    ///
    /// Thinking is deliberately not counted. It belongs to the message it
    /// precedes, and a run that thought four times before answering once has
    /// had one exchange with you, not five.
    pub messages: u32,
}

/// Idle, running, or needs you.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum Activity {
    Idle,
    Running,
    NeedsYou,
}

impl Activity {
    /// Collapse a [`RunStatus`] and the log into the three words the surface
    /// has.
    ///
    /// **Five states, three words, and every ending lands on `Idle`.** The
    /// vocabulary is not an outcome vocabulary — it answers "does this node
    /// want me *now*", which is the only question a canvas of a dozen columns
    /// can answer at a glance. On that axis a failed run, an abandoned one and
    /// an interrupted one are the same as a finished one: nothing is working
    /// and nothing is blocked on a decision. Outcome is a second axis and it
    /// has not moved — `RunStatus` is still on the row, and the inspector, the
    /// runs list and the status bar spell every word of it. What this must not
    /// do is smuggle outcome in by routing `Failed` to `NeedsYou`, which would
    /// make the pill mean two unrelated things and destroy the one state whose
    /// derivation the record actually specifies.
    ///
    /// [`RunStatus::Interrupted`] is the newest ending and it did not buy a
    /// fourth word. A run the user stopped, or that app exit stopped, is
    /// exactly as idle as one that ran out of things to say: the process is
    /// gone either way and nobody is waiting on the reader. The reason it is a
    /// separate *status* is the second axis, where "we ended it" and "it
    /// failed" are not the same sentence.
    ///
    /// `NeedsYou` is that state: a `permission.requested` with no matching
    /// `permission.resolved`. It is scoped to a *running* run on purpose — a
    /// dead process is not waiting for an answer, so an unresolved request on a
    /// finished run is a record of how it ended, not a demand on the reader.
    pub fn of(status: RunStatus, awaiting_permission: bool) -> Self {
        match status {
            RunStatus::Running if awaiting_permission => Activity::NeedsYou,
            RunStatus::Running => Activity::Running,
            RunStatus::Finished
            | RunStatus::Failed
            | RunStatus::Abandoned
            | RunStatus::Interrupted => Activity::Idle,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Activity::Idle => "idle",
            Activity::Running => "running",
            Activity::NeedsYou => "needs you",
        }
    }
}

/// Where a run can be forked, and what a fork there would inherit.
///
/// A projection like [`RunActivity`], and here for the same reason: **which
/// blocks are forkable is a decision, and the shell owns no logic** (roadmap
/// §8). The rule itself is not re-implemented here either — it is
/// [`normalizer::claude_code::fork_address`], the one reader of the format the
/// normalizer mints into `raw_events.idempotency_key`, and this walks a run's
/// blocks past that function. A second copy of "strip at the first `#`, a key is
/// only an address on an `assistant` or `user` line, null is not an address at
/// all" in TypeScript would drift from it silently, and the surface that drifted
/// would be the one offering forks at addresses the driver then refuses.
///
/// `RawEvent.idempotency_key` does reach the webview, which makes re-deriving
/// it there look free. It is not free: it is the same rule in a second
/// language, out of reach of every test in this workspace.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ForkPoints {
    pub run_id: RunId,
    /// Every block of the run that carries a provider address, oldest first.
    ///
    /// A run with none is a run nothing can be forked from — which is an
    /// ordinary state, not an error: out-of-band events carry no key at all,
    /// the provider's own `system`, `result` and `rate_limit_event` lines are
    /// keyed but are not chain entries, and a run that has started, run a hook
    /// and asked for a permission has nothing addressable in it yet.
    pub blocks: Vec<ForkBlock>,
}

/// One forkable block: where it is, and how much conversation a fork there
/// would start with.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ForkBlock {
    /// The `seq` in this run's log. What `Run::branch_point_seq` stores, and
    /// what `driver::ForkPoint::resolve` is handed to re-address the block.
    #[ts(type = "number")]
    pub seq: u64,
    /// Messages in this run at or before this block — what the child's
    /// inherited-context strip states out loud.
    ///
    /// Counted by [`RunActivity::messages`]'s rule, which is why this is
    /// computed here rather than by whatever draws the strip: the two numbers
    /// appear beside each other on the canvas (a node's header says `N msgs`,
    /// its child says it inherited M of them) and two counting rules would make
    /// one of them wrong. `branching-surface.md` rejected snapping to a nearby
    /// address precisely because this number is stated out loud, so it is the
    /// number the fork decision is made against.
    pub inherited_messages: u32,
}
