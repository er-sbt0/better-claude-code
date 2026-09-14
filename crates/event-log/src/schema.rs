//! Schema and migrations.
//!
//! The log outlives the code (design §8), so the schema is versioned in
//! SQLite's own `user_version` and migrations are forward-only.

use rusqlite::{Connection, TransactionBehavior};

use crate::Result;

/// Bump when adding a migration; `migrate` applies everything above the
/// database's current `user_version`.
pub const SCHEMA_VERSION: u32 = 4;

/// Apply every migration the database has not seen.
///
/// **All of it in one transaction, including the `user_version` bump**, because
/// the alternative is a log that can be bricked. V1 alone is a dozen statements
/// — two tables, three indexes, two triggers — and applied bare, a failure or a
/// crash part-way through leaves those objects on disk while `user_version` is
/// still what it was. The next [`EventLog::open`](crate::EventLog::open) then
/// re-runs the same batch and dies on `table runs already exists`, every time,
/// with no way for the user to tell what happened and nothing in the crate to
/// undo it. §8 says the log outlives the code; a migration with a state that
/// only a deleted database recovers from does not honour that.
///
/// SQLite makes this available because its DDL is transactional, so a rollback
/// takes the `CREATE TABLE`s with it. **`PRAGMA user_version` is transactional
/// too** — it writes the database header inside the open transaction rather
/// than out of band, so a failed migration rolls back to exactly the version it
/// started at, schema and version together. That is not assumed: see
/// `a_failed_migration_leaves_the_database_at_its_old_version` in
/// `tests/log.rs`, which plants a collision, watches the open fail, and asserts
/// both halves reverted.
///
/// Forward-only stays forward-only (§8). This is a rollback of a migration that
/// never committed, not a down-migration: nothing here can move a database to a
/// version it has already left.
///
/// `Immediate` for [`append_all`](crate::EventLog::append_all)'s reason, which
/// applies here more sharply. This reads `user_version` and then writes, and a
/// deferred transaction that reads first cannot upgrade to a write while
/// another connection is writing — SQLite refuses without consulting the busy
/// handler, so `log.rs`'s `BUSY_TIMEOUT` would not apply and the loser would
/// fail instantly with "database is locked". A migration *is* a write, it
/// happens at open, and `from_conn` runs it on whichever connection opens the
/// file next —
/// which, with the shell holding one `EventLog` and the driver taking another,
/// is routinely the second one while the first is live. Taking the write lock
/// up front is the case the busy handler covers.
///
/// Takes `&mut Connection` because a transaction borrows the connection
/// exclusively; `from_conn` owns it at this point, so the cost is a keyword.
pub fn migrate(conn: &mut Connection) -> Result<()> {
    // Outside the transaction on purpose: the overwhelmingly common open is of
    // an up-to-date log, and starting a write transaction on every one of those
    // would take the database's write lock — blocking a live run's appends for
    // as long as the open takes — to discover there is nothing to do.
    if user_version(conn)? >= SCHEMA_VERSION {
        return Ok(());
    }

    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;

    // Read again, now that the write lock makes the answer stable. Two
    // processes can open a stale log at the same moment — the shell and a
    // driver at startup — and the loser waits out `BUSY_TIMEOUT` here, by
    // which time the winner has committed. Without this it would then apply
    // migrations the database already has and fail on the first `ALTER`.
    let current = user_version(&tx)?;
    if current >= SCHEMA_VERSION {
        return Ok(());
    }
    if current < 1 {
        tx.execute_batch(V1)?;
    }
    if current < 2 {
        tx.execute_batch(V2)?;
    }
    if current < 3 {
        tx.execute_batch(V3)?;
    }
    if current < 4 {
        tx.execute_batch(V4)?;
    }
    tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    tx.commit()?;
    Ok(())
}

/// The schema version this database is at, per SQLite's own header field.
///
/// Takes `&Connection` so it serves both reads above: a `Transaction` derefs to
/// one, and the answer inside the transaction is the one that decides.
fn user_version(conn: &Connection) -> Result<u32> {
    Ok(conn.query_row("PRAGMA user_version", [], |r| r.get(0))?)
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
    -- §13: a run is created *with* its worktree — forks included, since a
    -- process's cwd is fixed at spawn — so cwd is never the user's own
    -- checkout. Nullable anyway, and not by oversight: a worktree can be
    -- removed after the run ends (§13 makes cleanup ordinary) while the run
    -- and its shadow ref survive, and §13.1 needs that state representable so
    -- Review can say attribution is unavailable rather than silently omit it.
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

const V2: &str = r#"
-- The other half of the graph edge (§19, `docs/branching-surface.md`).
--
-- `parent_run_id` shipped in V1 and says *which* run a fork descends from;
-- this says *where*. Together they are one fact, and the reason it is a column
-- rather than a projection is the finding in
-- `docs/verification/claude-code-session-forking.md`: **the provider records
-- no parentage.** A forked transcript is a full copy of the parent's,
-- re-stamped with its own session id, so nothing on disk outside this table
-- remembers that the two runs are related or where they diverged. §8's
-- standing rule is that anything recomputable from the log is a projection and
-- must not be stored; this is the exception that proves it, because it is not
-- recomputable from anything — not from the log, not from the provider, not
-- from git. Fail to write it at fork time and the edge is gone for good.
--
-- It is a `seq` in the PARENT's log: the event the child was anchored at, whose
-- `raw_events.idempotency_key` is the chain-entry uuid handed to
-- `--resume-session-at` (`docs/verification/claude-code-fork-address.md` —
-- and note the `#i` fan-out suffix is ours, so the address is the part before
-- it). NULL for a root run.
--
-- The pairing invariant — a root run has neither a parent nor a branch point, a
-- fork has both — is enforced in `Run::check_fork_pairing` rather than by a
-- CHECK constraint. SQLite cannot add one via `ALTER TABLE` without rebuilding
-- the table, a rebuild is not worth it for this, and §8's rule that anything
-- which starts deciding belongs where a test can reach it without a database
-- points the same way.
ALTER TABLE runs ADD COLUMN branch_point_seq INTEGER;
"#;

const V3: &str = r#"
-- Which model the run was launched with (§5, `LaunchSpec::model`).
--
-- The driver has always been able to vary this per run and nothing ever stored
-- it, so the model was knowable only while the process was alive. That was
-- tolerable while the surface had no header; `docs/branching-surface.md` gives
-- every node one, and a header that states a model it cannot read is
-- decoration. This is the column that makes it real.
--
-- Not a projection, and the §8 rule is why rather than why not: the model is
-- *not* recomputable from the log. `run.started` carries one when the provider
-- announces it and `turn.finished` carries the models a turn actually billed,
-- but neither is what we asked for — a run that failed before its first turn
-- billed nothing and announced nothing, and a turn that fell back to a
-- different model would rewrite history if it were read as the request. This
-- records the launch, which is a fact only the launcher has.
--
-- Nullable, because `LaunchSpec::model` is: a run launched without `--model`
-- gets the provider's own default, and naming a model we did not choose would
-- be a guess stored as a fact.
--
-- A fork inherits its parent's value at `driver::fork` rather than by reading
-- through the edge at query time, so a later UI can change one branch's model
-- without rewriting what the parent was launched with.
ALTER TABLE runs ADD COLUMN model TEXT;
"#;

const V4: &str = r#"
-- The rest of what the run was launched with (`event_log::LaunchFlags`).
--
-- V3 stored the model and nothing else, and two features recorded the hole it
-- left as a known gap before this closed it: `driver::fork` could only
-- pre-grant what the *shell* remembered launching the parent with, in this
-- session, so forking a run from an earlier session pre-granted nothing; and
-- `Registry::resume` woke a run with its model and an empty allow-list. Under
-- §12's posture — `DenyPrompts`, nothing may prompt — "not pre-granted" is not
-- "ask the user", it is denied outright, so the visible symptom was a branch
-- refused the `Read` its parent had for no reason on the screen, working
-- around it silently.
--
-- Not a projection, by the same §8 argument V3 makes for the model and V2
-- makes for the branch point: the log records what the agent *did*, never what
-- it was *permitted* to do. A denial arrives as a refused tool call that does
-- not state the allow-list it failed, so nothing in `raw_events` can
-- reconstruct this. It is written at launch or it is lost.
--
-- **One TEXT column of JSON, not seven columns**, and the reason is what a
-- column here actually costs. Every `SELECT` in `log.rs` names the whole
-- column list and `row_to_run` reads it back positionally, so a column is an
-- edit in five places plus a mis-ordering hazard — which is the price of a
-- column you can *query*, and nothing will ever ask this table for "runs with
-- `Edit` pre-granted". Read and written whole, this is the same kind of value
-- `raw_events.payload` is, for the same reason. §14 wanted `runs` thin and
-- this keeps it thin: one column for a set that will grow.
--
-- The blob survives the struct changing by `#[serde(default)]` on the
-- container rather than by a version tag: a field added later is absent from
-- every blob on disk and decodes to its default, a field removed is ignored on
-- read, and there is one attribute to not forget instead of one per field.
--
-- Nullable, and NULL is not a third state. A row written before this column
-- existed and a run launched with no flags at all read back identically, as
-- empty. The V3-era row may in truth have had an allow-list nobody recorded;
-- there is no way to recover it, and pre-granting tools we cannot show were
-- granted would be a guess stored as a fact. Empty fails closed, which is the
-- behaviour those rows already have.
ALTER TABLE runs ADD COLUMN launch_flags TEXT;
"#;
