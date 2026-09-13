use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use agent_protocol::{Event, RawEvent, RunId, SessionId};
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use serde_json::Value;

/// SQLite's INTEGER is signed 64-bit, so `seq` crosses the boundary as `i64`.
/// A run would need 9.2e18 events to overflow it; the cast is total in practice
/// and `as` is the honest spelling of that.
fn to_i64(seq: u64) -> i64 {
    seq as i64
}
fn from_i64(seq: i64) -> u64 {
    seq as u64
}

use crate::error::{Error, Result};
use crate::run::{Activity, ForkBlock, ForkPoints, Run, RunActivity, RunMetrics, RunStatus};
use crate::schema;

/// What happened to an appended event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Appended {
    /// Stored, at this `seq`.
    Stored(u64),
    /// Already in the log under the same idempotency key, at this `seq`. Not an
    /// error: this is the expected outcome for every event a resumed stream
    /// replays (§14).
    Duplicate(u64),
}

impl Appended {
    pub fn seq(self) -> u64 {
        match self {
            Appended::Stored(s) | Appended::Duplicate(s) => s,
        }
    }
    pub fn is_duplicate(self) -> bool {
        matches!(self, Appended::Duplicate(_))
    }
}

/// Normalizer telemetry read back off the log (design §16).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Health {
    pub events: u64,
    pub unclassified: u64,
    pub by_type: BTreeMap<String, u64>,
}

impl Health {
    /// The number worth alerting on: a spike is the earliest warning that a
    /// provider changed its format.
    pub fn unclassified_ratio(&self) -> f64 {
        if self.events == 0 {
            return 0.0;
        }
        self.unclassified as f64 / self.events as f64
    }
}

/// How long a write waits for another connection's transaction before giving
/// up with "database is locked".
///
/// The log is single-writer per *connection*, but the app runs two of them by
/// design: the shell keeps one `EventLog` for the UI, and `driver::start`
/// takes an `EventLog` **by value** — returning it only from `Session::wait()`
/// — so a run's writer is necessarily a second connection to the same file.
/// WAL (below) lets those two read past each other; it does not queue their
/// writes. The collision that actually happens is §8's boot reconcile:
/// `driver::reconcile_after_restart` writing `finish_run` for every orphan
/// while a freshly started run does `create_run` / `append` on its own
/// connection. With no busy handler the loser gets `SQLITE_BUSY` on its first
/// attempt, and a database that is busy for a millisecond is reported to the
/// user as a database that is locked.
///
/// **This is set explicitly even though rusqlite already does it.**
/// `Connection::open` installs `sqlite3_busy_timeout(db, 5000)` itself
/// (`rusqlite::inner_connection`), which is why nothing has gone wrong so far
/// — the two-connection story has been working on a library default that is
/// documented nowhere in this crate and that a version bump is free to drop or
/// change. Naming the value makes it ours: if it moves, it moves here, and
/// `tests/log.rs` fails when it is zero.
///
/// Five seconds, which is also what we were silently getting: three orders of
/// magnitude above a `synchronous = FULL` commit's 1.2ms fsync, so no honest
/// contention reaches it, and short enough that a real deadlock fails a call
/// instead of hanging the UI behind it forever.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// The append-only run event log.
///
/// One database per installation. `runs` holds identity and lifecycle;
/// `raw_events` holds the log itself and is protected against `UPDATE` and
/// `DELETE` by triggers, not by convention.
pub struct EventLog {
    conn: Connection,
}

impl EventLog {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::from_conn(Connection::open(path)?)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::from_conn(Connection::open_in_memory()?)
    }

    fn from_conn(conn: Connection) -> Result<Self> {
        // First, so that it covers the migration below as well as every later
        // write: `migrate` runs on whichever connection opens the file next,
        // which may well be the second one.
        conn.busy_timeout(BUSY_TIMEOUT)?;

        // WAL so that the UI can read a run while the driver is writing it.
        //
        // `synchronous = FULL` rather than the usual NORMAL: §8 names crash
        // recovery as a reason the log exists, and an event log that loses its
        // last few events to a power cut is not one. The cost is one fsync per
        // commit — 1.2ms on ext4/NVMe against NORMAL's 0.04ms, so 835 events/s
        // committed one at a time and 3.6k/s batched per provider line, which
        // is the real write pattern. Tool calls take hundreds of milliseconds;
        // this is not the bottleneck. Re-measure with
        // `examples/append_cost.rs` before changing it, and not on tmpfs.
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "FULL")?;
        conn.pragma_update(None, "foreign_keys", true)?;
        schema::migrate(&conn)?;
        Ok(Self { conn })
    }

    /// Trade durability for speed: the last few transactions can be lost to an
    /// OS crash or power cut, though the database stays intact. Exposed for
    /// `examples/append_cost.rs`, which is how the default was chosen; a caller
    /// reaching for this is deciding that a run's tail is expendable.
    pub fn set_synchronous_normal(&mut self) -> Result<()> {
        self.conn.pragma_update(None, "synchronous", "NORMAL")?;
        Ok(())
    }

    // ------------------------------------------------------------------ runs --

    /// Record a run. `started_at` and `status` are the caller's to set, because
    /// the log has no clock of its own (§8: timestamps come from the runtime).
    ///
    /// The graph edge is checked before it is written
    /// ([`Run::check_fork_pairing`]): half an edge cannot be repaired later,
    /// because nothing outside this table remembers the fork.
    pub fn create_run(&mut self, run: &Run) -> Result<()> {
        run.check_fork_pairing()?;
        self.conn.execute(
            "INSERT INTO runs (id, agent, agent_version, provider_session_id, workspace_path,
                               worktree_path, branch, shadow_ref, prompt, model, status,
                               started_at, finished_at, parent_run_id, branch_point_seq)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
            params![
                run.id.0,
                run.agent,
                run.agent_version,
                run.provider_session_id.as_ref().map(|s| &s.0),
                run.workspace_path,
                run.worktree_path,
                run.branch,
                run.shadow_ref,
                run.prompt,
                run.model,
                run.status.as_str(),
                run.started_at,
                run.finished_at,
                run.parent_run_id.as_ref().map(|r| &r.0),
                run.branch_point_seq.map(to_i64),
            ],
        )?;
        Ok(())
    }

    pub fn run(&self, id: &RunId) -> Result<Option<Run>> {
        self.conn
            .query_row(
                "SELECT id, agent, agent_version, provider_session_id, workspace_path,
                        worktree_path, branch, shadow_ref, prompt, model, status,
                        started_at, finished_at, parent_run_id, branch_point_seq
                 FROM runs WHERE id = ?1",
                params![id.0],
                row_to_run,
            )
            .optional()
            .map_err(Error::from)
    }

    pub fn finish_run(&mut self, id: &RunId, status: RunStatus, finished_at: &str) -> Result<()> {
        let n = self.conn.execute(
            "UPDATE runs SET status = ?2, finished_at = ?3 WHERE id = ?1",
            params![id.0, status.as_str(), finished_at],
        )?;
        if n == 0 {
            return Err(Error::NoSuchRun(id.clone()));
        }
        Ok(())
    }

    /// Record the provider's session id once we have it.
    ///
    /// §14 prefers *assigning* it at launch, in which case this is never
    /// needed; it exists for agents that only report one.
    pub fn set_provider_session_id(&mut self, id: &RunId, session: &SessionId) -> Result<()> {
        let n = self.conn.execute(
            "UPDATE runs SET provider_session_id = ?2 WHERE id = ?1",
            params![id.0, session.0],
        )?;
        if n == 0 {
            return Err(Error::NoSuchRun(id.clone()));
        }
        Ok(())
    }

    /// Runs the log still believes are running.
    ///
    /// At startup every one of these is a lie — no process of ours survived the
    /// restart — so the driver reconciles them. §8's crash recovery starts here.
    pub fn running_runs(&self) -> Result<Vec<Run>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, agent, agent_version, provider_session_id, workspace_path,
                    worktree_path, branch, shadow_ref, prompt, model, status,
                    started_at, finished_at, parent_run_id, branch_point_seq
             FROM runs WHERE status = 'running' ORDER BY started_at",
        )?;
        let rows = stmt.query_map([], row_to_run)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Error::from)
    }

    /// The most recently started runs, whatever became of them.
    ///
    /// [`running_runs`](Self::running_runs) answers a different question, and
    /// the difference is the whole reason this exists: a graph that only shows
    /// runs still in flight loses a branch the moment it finishes, which is
    /// precisely when it becomes worth looking at. §10's surface needs the
    /// dead ones too.
    ///
    /// Ordered newest first and capped, because the caller draws these and
    /// there is no workspace filter here on purpose — the desktop shell groups
    /// by `workspace_path` itself, and the same query then serves both the
    /// per-workspace graph and §10's cross-workspace index.
    pub fn recent_runs(&self, limit: u32) -> Result<Vec<Run>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, agent, agent_version, provider_session_id, workspace_path,
                    worktree_path, branch, shadow_ref, prompt, model, status,
                    started_at, finished_at, parent_run_id, branch_point_seq
             FROM runs ORDER BY started_at DESC, id DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit], row_to_run)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Error::from)
    }

    // ---------------------------------------------------------------- append --

    pub fn append(&mut self, ev: &RawEvent) -> Result<Appended> {
        Ok(self.append_all(std::slice::from_ref(ev))?.remove(0))
    }

    /// Append a batch in one transaction.
    ///
    /// The natural batch is one provider line, which can fan out into several
    /// events (§5). Committing them together means a crash never leaves half a
    /// provider message in the log.
    ///
    /// **`seq` is the writer's, not ours.** The normalizer assigns it when it
    /// produces the event, so the event is addressable (§7) before it is
    /// durable — which is what lets the UI render a run as it streams. This
    /// method enforces only that `seq` strictly increases.
    ///
    /// **It does not enforce contiguity, and gaps are not an error.** §8 calls
    /// `seq` "ordering and gap detection"; the second half does not survive
    /// resume. A resumed stream replays events we already hold, each consuming
    /// a `seq` and then deduplicating away, so holes in the sequence are the
    /// normal result of a resume rather than evidence of loss. Ordering is what
    /// `seq` is actually good for.
    pub fn append_all(&mut self, events: &[RawEvent]) -> Result<Vec<Appended>> {
        // IMMEDIATE, not the default DEFERRED, and this is the difference
        // between two concurrent runs and one. A deferred transaction that
        // reads first — `last_seq_tx` below, on every batch — holds a read
        // snapshot, and SQLite refuses to upgrade it to a write while another
        // connection is writing. It refuses *without consulting the busy
        // handler*, because waiting there could deadlock two connections
        // against each other, so [`BUSY_TIMEOUT`] does not apply and the loser
        // fails instantly with "database is locked". Taking the write lock up
        // front is the case the busy handler does cover. Found by running two
        // supervised runs at once (`driver/tests/registry.rs`), which is the
        // product case and not an exotic one.
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let mut out = Vec::with_capacity(events.len());
        let mut high: BTreeMap<&RunId, u64> = BTreeMap::new();
        for ev in events {
            let last = match high.get(&ev.run_id) {
                Some(&s) => s,
                None => last_seq_tx(&tx, &ev.run_id)?,
            };
            if ev.seq <= last {
                return Err(Error::SeqNotMonotonic {
                    run: ev.run_id.clone(),
                    got: ev.seq,
                    last,
                });
            }
            let appended = insert(&tx, ev)?;
            if let Appended::Stored(seq) = appended {
                high.insert(&ev.run_id, seq);
            }
            out.push(appended);
        }
        tx.commit()?;
        Ok(out)
    }

    /// The highest `seq` stored for a run, or 0 if it has no events.
    ///
    /// A resumed run must seed its normalizer from this, or the fresh
    /// normalizer restarts numbering at 1 and every append fails
    /// [`Error::SeqNotMonotonic`].
    pub fn last_seq(&self, run: &RunId) -> Result<u64> {
        self.conn
            .query_row(
                "SELECT COALESCE(MAX(seq), 0) FROM raw_events WHERE run_id = ?1",
                params![run.0],
                |r| r.get::<_, i64>(0).map(from_i64),
            )
            .map_err(Error::from)
    }

    // ------------------------------------------------------------------ read --

    pub fn events(&self, run: &RunId) -> Result<Vec<RawEvent>> {
        self.events_after(run, 0, u32::MAX)
    }

    /// Events after `seq`, in order, at most `limit` of them.
    ///
    /// This is the read the UI tails a live run with, and the read §20.5's run
    /// replay scrubs with.
    pub fn events_after(&self, run: &RunId, seq: u64, limit: u32) -> Result<Vec<RawEvent>> {
        let mut stmt = self.conn.prepare(
            "SELECT run_id, seq, ts, schema, idempotency_key, type, payload
             FROM raw_events WHERE run_id = ?1 AND seq > ?2 ORDER BY seq LIMIT ?3",
        )?;
        let rows = stmt.query_map(params![run.0, to_i64(seq), limit], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1).map(from_i64)?,
                r.get::<_, String>(2)?,
                r.get::<_, u32>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, String>(6)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (run_id, seq, ts, schema, idempotency_key, ty, payload) = row?;
            out.push(RawEvent {
                run_id: RunId(run_id),
                seq,
                ts,
                schema,
                idempotency_key,
                event: decode_event(&ty, &payload)?,
            });
        }
        Ok(out)
    }

    /// Every event of one type, for a run. The shape of §20.5's cross-run file
    /// history and cost analytics: a `SELECT` over data already stored.
    pub fn events_of_type(&self, run: &RunId, ty: &str) -> Result<Vec<RawEvent>> {
        let mut stmt = self.conn.prepare(
            "SELECT seq, ts, schema, idempotency_key, payload
             FROM raw_events WHERE run_id = ?1 AND type = ?2 ORDER BY seq",
        )?;
        let rows = stmt.query_map(params![run.0, ty], |r| {
            Ok((
                r.get::<_, i64>(0).map(from_i64)?,
                r.get::<_, String>(1)?,
                r.get::<_, u32>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, String>(4)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (seq, ts, schema, idempotency_key, payload) = row?;
            out.push(RawEvent {
                run_id: run.clone(),
                seq,
                ts,
                schema,
                idempotency_key,
                event: decode_event(ty, &payload)?,
            });
        }
        Ok(out)
    }

    // ----------------------------------------------------------- projections --

    /// Recompute a run's metrics from the log.
    ///
    /// Nothing here is stored. Per §14 the token totals come from the per-model
    /// breakdown rather than the provider's own `usage` field, which covers the
    /// main model only and under-reports silently.
    pub fn metrics(&self, run: &RunId) -> Result<RunMetrics> {
        let mut m = RunMetrics::default();
        for ev in self.events_of_type(run, "turn.finished")? {
            let Event::TurnFinished(t) = ev.event else {
                continue;
            };
            m.turns += 1;
            m.cost_usd += t.usage.cost_usd;
            m.permission_denials += t.permission_denials;
            if let Some(d) = t.duration_ms {
                m.duration_ms = Some(m.duration_ms.unwrap_or(0) + d);
            }
            for mu in &t.usage.models {
                m.add_model_usage(mu);
            }
        }
        m.tool_calls = self.conn.query_row(
            "SELECT COUNT(*) FROM raw_events WHERE run_id = ?1 AND type = 'tool.started'",
            params![run.0],
            |r| r.get(0),
        )?;
        Ok(m)
    }

    /// The three-word status and the message count, for the window the graph
    /// draws.
    ///
    /// A projection like [`metrics`](Self::metrics) — nothing here is stored —
    /// and it is a crate function for the reason the roadmap's §8 table gives:
    /// the shell owns no logic, and neither of these two numbers is a
    /// relabelling of something the `runs` row already holds.
    ///
    /// **needs you** means a `permission.requested` with no matching
    /// `permission.resolved`. *Matching* is by count rather than by
    /// `tool_use_id`, and that is a decision rather than a shortcut:
    /// `PermissionResolved::tool_use_id` is optional — the provider does not
    /// always echo it — so pairing on the id would report a run as blocked
    /// forever the first time a resolution arrived without one. Requests are
    /// answered one at a time and in order, so "more requested than resolved"
    /// asks the same question where the data can answer it.
    ///
    /// **Messages are not events, and the gap is not only tool traffic.** One
    /// API message fans out into one `assistant.message` per content block
    /// (§5), all sharing a `message_id`, so counting rows would report a
    /// three-paragraph answer as three messages. The count is therefore
    /// distinct `message_id`s, falling back to the `seq` for the blocks the
    /// provider left unidentified — one message each, which over-counts far
    /// less than merging them into one would under-count. User messages are
    /// counted per row, because a prompt is one line of stdin and arrives once:
    /// the driver passes `--replay-user-messages`, so our own prompts reach the
    /// log as replays and those replays are the *only* form they take. Not
    /// counting them would show every conversation as one-sided.
    ///
    /// Plural, and limited exactly as [`recent_runs`](Self::recent_runs) is,
    /// because it serves the same caller: a header per node, refreshed as
    /// events stream. One grouped query rather than one per node, which at a
    /// two-hundred-node window is the difference between a round trip and a
    /// stall.
    pub fn recent_activity(&self, limit: u32) -> Result<Vec<RunActivity>> {
        // The join is restricted to the four types this reads so that
        // `raw_events_type (run_id, type)` covers it. Without the restriction
        // the query walks every event of every run in the window, which is the
        // whole log, on the surface that refreshes most often.
        let mut stmt = self.conn.prepare(
            "SELECT r.id, r.status,
                    COUNT(DISTINCT CASE
                        WHEN e.type = 'user.message' THEN 'u' || e.seq
                        WHEN e.type = 'assistant.message'
                        THEN 'a' || COALESCE(json_extract(e.payload, '$.message_id'),
                                             CAST(e.seq AS TEXT))
                    END),
                    COALESCE(SUM(e.type = 'permission.requested'), 0),
                    COALESCE(SUM(e.type = 'permission.resolved'), 0)
             FROM (SELECT id, status FROM runs
                   ORDER BY started_at DESC, id DESC LIMIT ?1) r
             LEFT JOIN raw_events e
                    ON e.run_id = r.id
                   AND e.type IN ('user.message', 'assistant.message',
                                  'permission.requested', 'permission.resolved')
             GROUP BY r.id, r.status",
        )?;
        let rows = stmt.query_map(params![limit], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, i64>(4)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id, status, messages, requested, resolved) = row?;
            let status =
                RunStatus::parse(&status).ok_or_else(|| Error::UnknownRunStatus(status.clone()))?;
            out.push(RunActivity {
                run_id: RunId(id),
                status: Activity::of(status, requested > resolved),
                messages: messages as u32,
            });
        }
        Ok(out)
    }

    /// Which blocks of these runs can be forked at, and what each fork would
    /// inherit ([`ForkPoints`]).
    ///
    /// The rule is one function — [`normalizer::claude_code::fork_address`] —
    /// and this is the only thing between it and a surface: the hover toolbar
    /// appears on a block if and only if that block's `seq` is in this answer.
    /// Re-deriving it from the `idempotency_key` the webview already receives
    /// would be a second copy of the rule in a language no test here reaches,
    /// and the copy would drift towards offering forks the driver refuses.
    ///
    /// **Plural, and by name rather than by window.** [`recent_activity`] takes
    /// a limit because every node needs a header; this takes the runs the
    /// caller can actually fork in — the one being read, and the parents of the
    /// children on screen — because the answer is per *block* rather than per
    /// run, and a two-hundred-run window of it is most of the log.
    ///
    /// A run with no addressable block gets a row with an empty `blocks`, not
    /// no row: "nothing here can be forked" is an answer, and a missing row
    /// reads as "not loaded yet" on the surface that asks.
    ///
    /// [`recent_activity`]: Self::recent_activity
    pub fn fork_points(&self, runs: &[RunId]) -> Result<Vec<ForkPoints>> {
        let mut stmt = self.conn.prepare(
            "SELECT seq, type, idempotency_key, json_extract(payload, '$.message_id')
               FROM raw_events WHERE run_id = ?1 ORDER BY seq",
        )?;
        let mut out = Vec::with_capacity(runs.len());
        for run in runs {
            let rows = stmt.query_map(params![run.0], |r| {
                Ok((
                    from_i64(r.get::<_, i64>(0)?),
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, Option<String>>(3)?,
                ))
            })?;

            let mut blocks = Vec::new();
            let mut messages: u32 = 0;
            // Distinct `message_id`, exactly as `recent_activity` counts it:
            // one API message fans out into one event per content block, and a
            // strip that said "inherits 3 messages" over one three-paragraph
            // answer would be the lie the no-snapping rule exists to prevent.
            let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
            for row in rows {
                let (seq, ty, key, message_id) = row?;
                match ty.as_str() {
                    "user.message" => messages += 1,
                    "assistant.message" => {
                        let id = message_id.unwrap_or_else(|| format!("seq:{seq}"));
                        if seen.insert(id) {
                            messages += 1;
                        }
                    }
                    _ => {}
                }
                if normalizer::claude_code::fork_address(key.as_deref()).is_some() {
                    blocks.push(ForkBlock {
                        seq,
                        inherited_messages: messages,
                    });
                }
            }
            out.push(ForkPoints {
                run_id: run.clone(),
                blocks,
            });
        }
        Ok(out)
    }

    /// Normalizer telemetry for a run (§16).
    pub fn health(&self, run: &RunId) -> Result<Health> {
        let mut stmt = self
            .conn
            .prepare("SELECT type, COUNT(*) FROM raw_events WHERE run_id = ?1 GROUP BY type")?;
        let rows = stmt.query_map(params![run.0], |r| {
            Ok((r.get::<_, String>(0)?, from_i64(r.get::<_, i64>(1)?)))
        })?;
        let mut h = Health::default();
        for row in rows {
            let (ty, n) = row?;
            h.events += n;
            if ty == "raw.unclassified" {
                h.unclassified += n;
            }
            h.by_type.insert(ty, n);
        }
        Ok(h)
    }
}

// ------------------------------------------------------------------- helpers --

fn last_seq_tx(tx: &Transaction<'_>, run: &RunId) -> Result<u64> {
    tx.query_row(
        "SELECT COALESCE(MAX(seq), 0) FROM raw_events WHERE run_id = ?1",
        params![run.0],
        |r| r.get::<_, i64>(0).map(from_i64),
    )
    .map_err(Error::from)
}

fn insert(tx: &Transaction<'_>, ev: &RawEvent) -> Result<Appended> {
    let (ty, payload) = encode_event(&ev.event)?;
    let n = tx.execute(
        "INSERT INTO raw_events (run_id, seq, ts, schema, idempotency_key, type, payload)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT DO NOTHING",
        params![
            ev.run_id.0,
            to_i64(ev.seq),
            ev.ts,
            ev.schema,
            ev.idempotency_key,
            ty,
            payload
        ],
    )?;
    if n == 1 {
        return Ok(Appended::Stored(ev.seq));
    }

    // The insert was refused. With a key, that is the resume path. Without one
    // it can only be the primary key, i.e. this exact seq is taken -- which the
    // monotonicity check above already rules out, so it cannot happen.
    let Some(key) = ev.idempotency_key.as_deref() else {
        return Err(Error::SeqNotMonotonic {
            run: ev.run_id.clone(),
            got: ev.seq,
            last: last_seq_tx(tx, &ev.run_id)?,
        });
    };

    let (seq, existing_ty, existing_payload): (u64, String, String) = tx.query_row(
        "SELECT seq, type, payload FROM raw_events
         WHERE run_id = ?1 AND idempotency_key = ?2",
        params![ev.run_id.0, key],
        |r| Ok((from_i64(r.get(0)?), r.get(1)?, r.get(2)?)),
    )?;

    // Same key, different event. Not a replay -- a key collision, which means
    // either the provider reused a uuid or we derived the key wrongly. Either
    // way the log would silently lose an event if we called this a duplicate.
    if existing_ty != ty || existing_payload != payload {
        return Err(Error::IdempotencyConflict {
            run: ev.run_id.clone(),
            key: key.to_owned(),
            existing: existing_ty,
            seq,
        });
    }
    Ok(Appended::Duplicate(seq))
}

fn encode_event(event: &Event) -> Result<(String, String)> {
    let v = serde_json::to_value(event)?;
    let payload = v.get("payload").cloned().unwrap_or(Value::Null);
    Ok((
        event.type_name().to_owned(),
        serde_json::to_string(&payload)?,
    ))
}

fn decode_event(ty: &str, payload: &str) -> Result<Event> {
    let payload: Value = serde_json::from_str(payload)?;
    Ok(serde_json::from_value(
        serde_json::json!({ "type": ty, "payload": payload }),
    )?)
}

fn row_to_run(r: &rusqlite::Row<'_>) -> rusqlite::Result<Run> {
    Ok(Run {
        id: RunId(r.get(0)?),
        agent: r.get(1)?,
        agent_version: r.get(2)?,
        provider_session_id: r.get::<_, Option<String>>(3)?.map(SessionId),
        workspace_path: r.get(4)?,
        worktree_path: r.get(5)?,
        branch: r.get(6)?,
        shadow_ref: r.get(7)?,
        prompt: r.get(8)?,
        model: r.get(9)?,
        // A status we do not recognize is a database written by a newer build.
        // Guessing at it would quietly relabel someone's run; refusing to read
        // the row says so.
        status: {
            let raw: String = r.get(10)?;
            RunStatus::parse(&raw).ok_or_else(|| {
                rusqlite::Error::FromSqlConversionFailure(
                    10,
                    rusqlite::types::Type::Text,
                    Box::new(crate::Error::UnknownRunStatus(raw)),
                )
            })?
        },
        started_at: r.get(11)?,
        finished_at: r.get(12)?,
        parent_run_id: r.get::<_, Option<String>>(13)?.map(RunId),
        branch_point_seq: r.get::<_, Option<i64>>(14)?.map(from_i64),
    })
}
