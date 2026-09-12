/// Timestamps come from our runtime, never from the agent (design §8).
///
/// Injectable so that fixture replay is byte-deterministic; a normalizer that
/// can only be tested against wall-clock time cannot have golden output.
pub trait Clock: Send + Sync {
    fn now(&self) -> String;
}

pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> String {
        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
    }
}

/// Always returns the same instant. For tests.
pub struct FixedClock(pub String);

impl Default for FixedClock {
    fn default() -> Self {
        FixedClock("2026-09-12T00:00:00.000Z".to_owned())
    }
}

impl Clock for FixedClock {
    fn now(&self) -> String {
        self.0.clone()
    }
}
