use std::collections::BTreeMap;

/// Per-run normalization counters.
///
/// Design §16 asks for unclassified counts per adapter per version as telemetry,
/// on the grounds that a spike is the earliest warning that a provider changed
/// its format. Normalizer trouble is a number to watch, not a crash.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Stats {
    /// Non-blank lines seen.
    pub lines: u64,
    /// Lines that produced at least one classified event.
    pub classified: u64,
    /// Lines preserved as `raw.unclassified`.
    pub unclassified: u64,
    /// Lines deliberately dropped as transient progress (design §8).
    pub transient_dropped: u64,
    /// Counts keyed by the provider's own `type` / `type.subtype`.
    pub by_source: BTreeMap<String, u64>,
}

impl Stats {
    pub(crate) fn see(&mut self, source: &str) {
        self.lines += 1;
        *self.by_source.entry(source.to_owned()).or_default() += 1;
    }

    /// Share of lines we could not interpret, in `0.0..=1.0`. The number worth
    /// alerting on.
    pub fn unclassified_ratio(&self) -> f64 {
        if self.lines == 0 {
            return 0.0;
        }
        self.unclassified as f64 / self.lines as f64
    }

    /// Every line is accounted for exactly once. If this is ever false the
    /// degradation invariant is broken and something was silently dropped.
    pub fn balances(&self) -> bool {
        self.classified + self.unclassified + self.transient_dropped == self.lines
    }
}
