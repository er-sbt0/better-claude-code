use agent_protocol::RunId;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),

    #[error("event payload is not valid JSON: {0}")]
    Json(#[from] serde_json::Error),

    #[error("no such run: {0}")]
    NoSuchRun(RunId),

    /// Written by a build that knows a status this one does not.
    #[error("unknown run status {0:?}: the database was written by a newer build")]
    UnknownRunStatus(String),

    /// The writer handed us a `seq` that does not advance.
    ///
    /// Almost always one specific bug: a run was resumed with a fresh
    /// normalizer, which starts numbering at 1 again. Failing loudly here is
    /// the point — the alternative is a log whose ordering silently stops
    /// meaning anything. Seed the normalizer from [`crate::EventLog::last_seq`].
    #[error(
        "run {run}: seq {got} does not advance past {last}; \
             a resumed run must seed its normalizer from last_seq"
    )]
    SeqNotMonotonic { run: RunId, got: u64, last: u64 },

    /// Two different events claimed the same idempotency key.
    #[error("run {run}: idempotency key {key:?} is already held by a {existing} at seq {seq}")]
    IdempotencyConflict {
        run: RunId,
        key: String,
        existing: String,
        seq: u64,
    },
}

pub type Result<T> = std::result::Result<T, Error>;
