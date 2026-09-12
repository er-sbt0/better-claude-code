use std::collections::BTreeMap;
use std::path::Path;

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
use crate::run::{Run, RunMetrics, RunStatus};
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
    pub fn create_run(&mut self, run: &Run) -> Result<()> {
        self.conn.execute(
            "INSERT INTO runs (id, agent, agent_version, provider_session_id, workspace_path,
                               worktree_path, branch, shadow_ref, prompt, status, started_at,
                               finished_at, parent_run_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
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
                run.status.as_str(),
                run.started_at,
                run.finished_at,
                run.parent_run_id.as_ref().map(|r| &r.0),
            ],
        )?;
        Ok(())
    }

    pub fn run(&self, id: &RunId) -> Result<Option<Run>> {
        self.conn
            .query_row(
                "SELECT id, agent, agent_version, provider_session_id, workspace_path,
                        worktree_path, branch, shadow_ref, prompt, status, started_at,
                        finished_at, parent_run_id
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
                    worktree_path, branch, shadow_ref, prompt, status, started_at,
                    finished_at, parent_run_id
             FROM runs WHERE status = 'running' ORDER BY started_at",
        )?;
        let rows = stmt.query_map([], row_to_run)?;
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
        let tx = self.conn.transaction()?;
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
        // A status we do not recognize is a database written by a newer build.
        // Guessing at it would quietly relabel someone's run; refusing to read
        // the row says so.
        status: {
            let raw: String = r.get(9)?;
            RunStatus::parse(&raw).ok_or_else(|| {
                rusqlite::Error::FromSqlConversionFailure(
                    9,
                    rusqlite::types::Type::Text,
                    Box::new(crate::Error::UnknownRunStatus(raw)),
                )
            })?
        },
        started_at: r.get(10)?,
        finished_at: r.get(11)?,
        parent_run_id: r.get::<_, Option<String>>(12)?.map(RunId),
    })
}
