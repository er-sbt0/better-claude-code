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
}

impl RunStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            RunStatus::Running => "running",
            RunStatus::Finished => "finished",
            RunStatus::Failed => "failed",
            RunStatus::Abandoned => "abandoned",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "running" => RunStatus::Running,
            "finished" => RunStatus::Finished,
            "failed" => RunStatus::Failed,
            "abandoned" => RunStatus::Abandoned,
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
    /// **Four states, three words, and `failed` and `abandoned` land on
    /// `Idle`.** The vocabulary is not an outcome vocabulary — it answers "does
    /// this node want me *now*", which is the only question a canvas of a dozen
    /// columns can answer at a glance. On that axis a failed run and an
    /// abandoned one are the same as a finished one: nothing is working and
    /// nothing is blocked on a decision. Outcome is a second axis and it has
    /// not moved — `RunStatus` is still on the row, and the inspector, the runs
    /// list and the status bar still spell all four words. What this must not
    /// do is smuggle outcome in by routing `Failed` to `NeedsYou`, which would
    /// make the pill mean two unrelated things and destroy the one state whose
    /// derivation the record actually specifies.
    ///
    /// `NeedsYou` is that state: a `permission.requested` with no matching
    /// `permission.resolved`. It is scoped to a *running* run on purpose — a
    /// dead process is not waiting for an answer, so an unresolved request on a
    /// finished run is a record of how it ended, not a demand on the reader.
    pub fn of(status: RunStatus, awaiting_permission: bool) -> Self {
        match status {
            RunStatus::Running if awaiting_permission => Activity::NeedsYou,
            RunStatus::Running => Activity::Running,
            RunStatus::Finished | RunStatus::Failed | RunStatus::Abandoned => Activity::Idle,
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
/// blocks past that function. A second copy of "strip at the first `#`, null is
/// not an address" in TypeScript would drift from it silently, and the surface
/// that drifted would be the one offering forks at addresses the driver then
/// refuses.
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
    /// and a run that has only asked for a permission has nothing addressable
    /// in it yet.
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
