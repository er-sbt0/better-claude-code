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
