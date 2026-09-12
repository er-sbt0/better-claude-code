use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// What one model cost during one turn.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ModelUsage {
    /// Full model id as the provider reported it, e.g. `claude-sonnet-5`.
    pub model: String,
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
    pub cost_usd: f64,
}

/// Usage for a single turn.
///
/// The totals here are summed over [`Usage::models`], which is the whole point.
/// Claude Code reports usage in three places that do not agree: `total_cost_usd`
/// (authoritative, per turn), `usage` (detailed, **main model only**), and
/// `modelUsage` (per model, complete). Every recorded run spends on a side model
/// that appears only in `modelUsage`, and its `costUSD` entries reconcile
/// exactly to `total_cost_usd` while `usage` does not.
///
/// Reading the token counts from the provider's `usage` field would under-report
/// silently and forever, which is why this type has no field that can hold it.
/// See design §14.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Usage {
    /// The provider's authoritative cost for this turn.
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
    /// Per-model breakdown, kept so the totals above can always be audited.
    pub models: Vec<ModelUsage>,
}

impl Usage {
    /// Build the totals by summing the per-model breakdown.
    ///
    /// `cost_usd` is taken from the provider rather than summed, so that a
    /// mismatch between the two stays visible to [`Usage::cost_reconciles`]
    /// instead of being averaged away.
    pub fn from_models(cost_usd: f64, models: Vec<ModelUsage>) -> Self {
        let mut usage = Usage {
            cost_usd,
            models,
            ..Default::default()
        };
        for m in &usage.models {
            usage.input_tokens += m.input_tokens;
            usage.output_tokens += m.output_tokens;
            usage.cache_creation_tokens += m.cache_creation_tokens;
            usage.cache_read_tokens += m.cache_read_tokens;
            usage.thinking_tokens += m.thinking_tokens;
        }
        usage
    }

    /// Whether the per-model costs add up to the reported total, within a
    /// tenth of a cent. False means the provider changed something and the
    /// §14 assumption needs re-checking — worth asserting in fixture tests
    /// rather than discovering from a user's invoice.
    pub fn cost_reconciles(&self) -> bool {
        let summed: f64 = self.models.iter().map(|m| m.cost_usd).sum();
        (summed - self.cost_usd).abs() < 0.0001
    }
}
