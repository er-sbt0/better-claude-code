//! The append-only run event log, on SQLite (design §8, §14).
//!
//! Two tables and one rule. `raw_events` holds observed facts and is immutable
//! — protected by triggers, so a projection bug that reaches for `UPDATE` gets
//! an error rather than a corrupted log. `runs` holds identity and lifecycle,
//! and deliberately not metrics: anything recomputable from the log is a
//! projection and must not be the source of truth, so a run's cost and token
//! counts are a query ([`EventLog::metrics`]), not columns.
//!
//! The log is local-first and single-writer: the process driver writes, the UI
//! reads. WAL mode is what makes reading a run while it is still being written
//! safe.

mod error;
mod log;
mod run;
mod schema;

pub use error::{Error, Result};
pub use log::{Appended, EventLog, Health};
pub use run::{Run, RunMetrics, RunStatus};
pub use schema::SCHEMA_VERSION;
