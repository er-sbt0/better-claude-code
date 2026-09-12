//! Schema and migrations.
//!
//! The log outlives the code (design §8), so the schema is versioned in
//! SQLite's own `user_version` and migrations are forward-only.

use rusqlite::Connection;

use crate::Result;

/// Bump when adding a migration; `migrate` applies everything above the
/// database's current `user_version`.
pub const SCHEMA_VERSION: u32 = 1;

/// Apply every migration the database has not seen.
pub fn migrate(conn: &Connection) -> Result<()> {
    let current: u32 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if current >= SCHEMA_VERSION {
        return Ok(());
    }
    if current < 1 {
        conn.execute_batch(V1)?;
    }
    conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    Ok(())
}

const V1: &str = r#"
-- Identity and lifecycle of a run. Deliberately NOT its metrics: §8's rule is
-- that anything recomputable from the log is a projection and must not be the
-- source of truth, and token counts and cost are recomputable from
-- `turn.finished`. See `EventLog::metrics`.
CREATE TABLE runs (
    id                  TEXT PRIMARY KEY,
    -- Adapter id, e.g. `claude-code`. Not a foreign key: an agent the user has
    -- since removed must not take its history with it.
    agent               TEXT NOT NULL,
    agent_version       TEXT,
    -- §14: the provider owns session state. We hold its id so we can ask for a
    -- resume; we never claim to be able to rebuild the session ourselves.
    provider_session_id TEXT,
    workspace_path      TEXT NOT NULL,
    -- §13: a run is created *with* its worktree, so cwd is never the user's own
    -- checkout.
    worktree_path       TEXT,
    branch              TEXT,
    -- §13.1: refs/agent-workspace/runs/<id>. Review's attribution is blame
    -- against this ref; if it is gone, attribution is unavailable and must be
    -- shown as unavailable rather than silently omitted.
    shadow_ref          TEXT,
    prompt              TEXT,
    status              TEXT NOT NULL,
    started_at          TEXT NOT NULL,
    finished_at         TEXT,
    parent_run_id       TEXT REFERENCES runs(id)
) STRICT;

CREATE INDEX runs_started_at ON runs(started_at DESC);

-- The log. Observed, immutable, append-only (§8).
CREATE TABLE raw_events (
    run_id          TEXT    NOT NULL REFERENCES runs(id),
    -- Assigned by the writer, not by this table: an event is addressable (§7)
    -- the moment the normalizer produces it, before it is durable, which is
    -- what lets the UI render a streaming run. Strictly increasing per run --
    -- but NOT contiguous; see `EventLog::append`.
    seq             INTEGER NOT NULL,
    ts              TEXT    NOT NULL,
    schema          INTEGER NOT NULL,
    idempotency_key TEXT,
    type            TEXT    NOT NULL,
    -- The `payload` object, as JSON. Stored whole rather than shredded into
    -- columns: the payload is versioned by `schema` and the log has to be
    -- readable by code that predates the shape it is reading.
    payload         TEXT    NOT NULL,
    PRIMARY KEY (run_id, seq)
) STRICT;

-- What makes replaying a resumed stream safe: the second copy of an event
-- collides here instead of being stored twice (§14).
CREATE UNIQUE INDEX raw_events_idempotency
    ON raw_events(run_id, idempotency_key)
    WHERE idempotency_key IS NOT NULL;

CREATE INDEX raw_events_type ON raw_events(run_id, type);

-- §8 says raw events are "never edited, never deleted". Triggers make that
-- true rather than aspirational: a projection bug that reaches for UPDATE gets
-- an error, not a corrupted log.
CREATE TRIGGER raw_events_are_append_only_update
BEFORE UPDATE ON raw_events
BEGIN
    SELECT RAISE(ABORT, 'raw_events is append-only (design §8)');
END;

CREATE TRIGGER raw_events_are_append_only_delete
BEFORE DELETE ON raw_events
BEGIN
    SELECT RAISE(ABORT, 'raw_events is append-only (design §8)');
END;
"#;
