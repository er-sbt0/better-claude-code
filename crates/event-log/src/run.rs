use agent_protocol::{ModelUsage, RunId, SessionId};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

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
    pub parent_run_id: Option<RunId>,
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
