//! The log's contract, and the fixtures replayed through it.
//!
//! The fixture test is the one that matters: every recorded stream is
//! normalized, stored, and read back, and the events that come out must equal
//! the ones that went in. A storage layer that quietly reshapes what it is
//! given breaks §7's degradation invariant just as thoroughly as a normalizer
//! that drops a line.

use std::fs;
use std::path::{Path, PathBuf};

use agent_protocol::{
    Event, ModelUsage, RawEvent, RunId, TurnFinished, TurnStarted, Unclassified,
    UnclassifiedReason, Usage, SCHEMA_VERSION,
};
use event_log::{Appended, Error, EventLog, Run, RunStatus};
use normalizer::{ClaudeCodeNormalizer, FixedClock};

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/claude-code/2.1.269")
}

fn fixtures() -> Vec<(String, PathBuf)> {
    let mut out: Vec<(String, PathBuf)> = fs::read_dir(fixture_dir())
        .expect("fixtures/claude-code/2.1.269 is missing — see MANIFEST.md")
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "ndjson"))
        .map(|p| (p.file_stem().unwrap().to_string_lossy().into_owned(), p))
        .collect();
    out.sort();
    assert!(!out.is_empty(), "no fixtures found");
    out
}

fn run_row(id: &str) -> Run {
    Run {
        id: RunId::from(id),
        agent: "claude-code".to_owned(),
        agent_version: Some("2.1.269".to_owned()),
        provider_session_id: None,
        workspace_path: "/home/u/my-app".to_owned(),
        worktree_path: Some("/home/u/my-app/.worktrees/run-1842".to_owned()),
        branch: Some("feat/oauth".to_owned()),
        shadow_ref: Some(format!("refs/agent-workspace/runs/{id}")),
        prompt: Some("add refresh-token support".to_owned()),
        status: RunStatus::Running,
        started_at: "2026-09-12T14:22:00.000Z".to_owned(),
        finished_at: None,
        parent_run_id: None,
    }
}

fn open_with_run(id: &str) -> EventLog {
    let mut log = EventLog::open_in_memory().unwrap();
    log.create_run(&run_row(id)).unwrap();
    log
}

fn ev(run: &str, seq: u64, key: Option<&str>, event: Event) -> RawEvent {
    RawEvent {
        run_id: RunId::from(run),
        seq,
        ts: "2026-09-12T14:22:08.412Z".to_owned(),
        schema: SCHEMA_VERSION,
        idempotency_key: key.map(str::to_owned),
        event,
    }
}

fn turn(index: u32, cost: f64, models: Vec<ModelUsage>) -> Event {
    Event::TurnFinished(TurnFinished {
        index,
        subtype: "success".to_owned(),
        ok: true,
        usage: Usage::from_models(cost, models),
        duration_ms: Some(1000),
        text: None,
        permission_denials: 0,
    })
}

// --------------------------------------------------------------- the fixtures --

/// Normalize, store, read back. What comes out must be what went in.
#[test]
fn every_fixture_round_trips_through_the_log() {
    for (name, path) in fixtures() {
        let text = fs::read_to_string(&path).unwrap();
        let run = RunId::from(name.as_str());
        let mut n = ClaudeCodeNormalizer::with_clock(run.clone(), Box::new(FixedClock::default()));
        let mut produced: Vec<RawEvent> = text.lines().flat_map(|l| n.push_line(l)).collect();
        produced.extend(n.finish(Some(0)));

        let mut log = open_with_run(&name);
        // One transaction per provider line is the real batching, but a single
        // batch exercises the same path and keeps the test about storage.
        let results = log.append_all(&produced).unwrap();
        assert!(
            results.iter().all(|r| matches!(r, Appended::Stored(_))),
            "{name}: a first pass must store everything"
        );

        let read_back = log.events(&run).unwrap();
        assert_eq!(
            read_back, produced,
            "{name}: the log reshaped the events it was given"
        );
    }
}

/// The fixtures are also the only honest test of §14's usage accounting, and
/// they are how we find out the provider changed it.
#[test]
fn metrics_recompute_from_the_log_and_reconcile() {
    let mut checked = 0;
    for (name, path) in fixtures() {
        let text = fs::read_to_string(&path).unwrap();
        let run = RunId::from(name.as_str());
        let mut n = ClaudeCodeNormalizer::with_clock(run.clone(), Box::new(FixedClock::default()));
        let mut produced: Vec<RawEvent> = text.lines().flat_map(|l| n.push_line(l)).collect();
        produced.extend(n.finish(Some(0)));

        let mut log = open_with_run(&name);
        log.append_all(&produced).unwrap();
        let m = log.metrics(&run).unwrap();

        let turns = produced
            .iter()
            .filter(|e| e.type_name() == "turn.finished")
            .count() as u32;
        assert_eq!(m.turns, turns, "{name}: turn count");
        if turns == 0 {
            continue;
        }
        checked += 1;

        // §5: cost is per turn. A multi-turn run's cost is the sum, and reading
        // the last `result` would under-report it.
        let summed: f64 = produced
            .iter()
            .filter_map(|e| match &e.event {
                Event::TurnFinished(t) => Some(t.usage.cost_usd),
                _ => None,
            })
            .sum();
        assert!((m.cost_usd - summed).abs() < 1e-9, "{name}: cost is a sum");

        // §14: totals come from the per-model breakdown, never from the
        // provider's `usage` field.
        let by_model: f64 = m.by_model.iter().map(|x| x.cost_usd).sum();
        assert!(
            (by_model - m.cost_usd).abs() < 0.0001,
            "{name}: per-model costs no longer reconcile to the total — \
             the §14 assumption needs re-checking against this provider version"
        );
        assert!(m.input_tokens > 0 && m.output_tokens > 0, "{name}: tokens");
    }
    assert!(checked >= 10, "expected most fixtures to have turns");
}

// --------------------------------------------------------------- append-only --

#[test]
fn raw_events_reject_update_and_delete() {
    use rusqlite::Connection;
    let dir = tempdir();
    let path = dir.join("log.sqlite");
    {
        let mut log = EventLog::open(&path).unwrap();
        log.create_run(&run_row("r")).unwrap();
        log.append(&ev(
            "r",
            1,
            None,
            Event::TurnStarted(TurnStarted { index: 0 }),
        ))
        .unwrap();
    }
    // Go around the API entirely: the guarantee has to hold against SQL, not
    // against our own good manners.
    let conn = Connection::open(&path).unwrap();
    let update = conn.execute("UPDATE raw_events SET type = 'nonsense'", []);
    let delete = conn.execute("DELETE FROM raw_events", []);
    for (what, r) in [("UPDATE", update), ("DELETE", delete)] {
        let msg = r.expect_err("{what} must be refused").to_string();
        assert!(
            msg.contains("append-only"),
            "{what} was refused for the wrong reason: {msg}"
        );
    }
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM raw_events", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
}

// ------------------------------------------------------------------- resume --

/// §14: a resumed stream replays events we already hold. They must deduplicate
/// rather than double.
#[test]
fn replayed_events_deduplicate_by_idempotency_key() {
    let mut log = open_with_run("r");
    let first: Vec<RawEvent> = (1..=3)
        .map(|i| {
            ev(
                "r",
                i,
                Some(&format!("uuid-{i}")),
                Event::TurnStarted(TurnStarted { index: i as u32 }),
            )
        })
        .collect();
    log.append_all(&first).unwrap();

    // The run is resumed. A correctly seeded normalizer numbers from last_seq,
    // and the provider replays the same three messages before producing a new
    // one.
    let last = log.last_seq(&RunId::from("r")).unwrap();
    assert_eq!(last, 3);
    let replayed: Vec<RawEvent> = (1..=3)
        .map(|i| {
            ev(
                "r",
                last + i,
                Some(&format!("uuid-{i}")),
                Event::TurnStarted(TurnStarted { index: i as u32 }),
            )
        })
        .collect();
    let results = log.append_all(&replayed).unwrap();
    assert!(
        results.iter().all(|r| r.is_duplicate()),
        "a replayed event must not be stored twice"
    );
    assert_eq!(
        results.iter().map(|r| r.seq()).collect::<Vec<_>>(),
        vec![1, 2, 3],
        "a duplicate reports the seq the event already has, not the one it was offered"
    );
    assert_eq!(log.events(&RunId::from("r")).unwrap().len(), 3);

    // And the genuinely new event lands after the hole the replay left.
    let new = ev(
        "r",
        last + 4,
        Some("uuid-4"),
        Event::TurnStarted(TurnStarted { index: 4 }),
    );
    assert_eq!(log.append(&new).unwrap(), Appended::Stored(7));

    // §8 calls seq "ordering and gap detection". Resume is why the second half
    // is not true: 4, 5 and 6 were consumed by the replay and stored nothing,
    // so a hole here is normal and must never be read as lost events.
    let seqs: Vec<u64> = log
        .events(&RunId::from("r"))
        .unwrap()
        .iter()
        .map(|e| e.seq)
        .collect();
    assert_eq!(seqs, vec![1, 2, 3, 7]);
    assert!(seqs.windows(2).all(|w| w[0] < w[1]), "ordering still holds");
}

/// The specific bug this error exists to catch: resuming a run with a fresh
/// normalizer, which starts numbering at 1 again.
#[test]
fn an_unseeded_normalizer_on_resume_fails_loudly() {
    let mut log = open_with_run("r");
    log.append_all(&[
        ev(
            "r",
            1,
            Some("a"),
            Event::TurnStarted(TurnStarted { index: 0 }),
        ),
        ev(
            "r",
            2,
            Some("b"),
            Event::TurnStarted(TurnStarted { index: 1 }),
        ),
    ])
    .unwrap();

    // A new normalizer, not seeded from last_seq, produces a genuinely new
    // event numbered 1. Nothing about its content says it is out of order.
    let err = log
        .append(&ev(
            "r",
            1,
            Some("c"),
            Event::TurnStarted(TurnStarted { index: 2 }),
        ))
        .unwrap_err();
    assert!(
        matches!(
            err,
            Error::SeqNotMonotonic {
                got: 1,
                last: 2,
                ..
            }
        ),
        "got {err:?}"
    );
    assert!(err
        .to_string()
        .contains("seed its normalizer from last_seq"));
    assert_eq!(log.events(&RunId::from("r")).unwrap().len(), 2);
}

/// Same key, different event. Calling that a duplicate would silently drop the
/// second one, which is exactly the failure the log exists to prevent.
#[test]
fn a_reused_idempotency_key_with_different_content_is_an_error() {
    let mut log = open_with_run("r");
    log.append(&ev(
        "r",
        1,
        Some("same"),
        Event::TurnStarted(TurnStarted { index: 0 }),
    ))
    .unwrap();
    let err = log
        .append(&ev(
            "r",
            2,
            Some("same"),
            Event::Unclassified(Unclassified {
                source_type: Some("x".to_owned()),
                source_subtype: None,
                reason: UnclassifiedReason::UnknownType,
                line: "{}".to_owned(),
            }),
        ))
        .unwrap_err();
    assert!(
        matches!(err, Error::IdempotencyConflict { .. }),
        "got {err:?}"
    );
}

/// A batch is one provider line. A crash must not leave half of it stored.
#[test]
fn a_failing_batch_stores_nothing() {
    let mut log = open_with_run("r");
    let batch = vec![
        ev(
            "r",
            1,
            Some("a"),
            Event::TurnStarted(TurnStarted { index: 0 }),
        ),
        ev(
            "r",
            2,
            Some("b"),
            Event::TurnStarted(TurnStarted { index: 1 }),
        ),
        ev(
            "r",
            2,
            Some("c"),
            Event::TurnStarted(TurnStarted { index: 2 }),
        ), // bad seq
    ];
    log.append_all(&batch).unwrap_err();
    assert!(
        log.events(&RunId::from("r")).unwrap().is_empty(),
        "the batch was not atomic"
    );
}

// --------------------------------------------------------------- lifecycle ---

/// §8's crash recovery: after a restart, every run the log still calls running
/// is one whose process is gone.
#[test]
fn running_runs_survive_a_reopen_and_can_be_reconciled() {
    let dir = tempdir();
    let path = dir.join("log.sqlite");
    {
        let mut log = EventLog::open(&path).unwrap();
        log.create_run(&run_row("crashed")).unwrap();
        log.create_run(&Run {
            status: RunStatus::Finished,
            finished_at: Some("2026-09-12T14:30:00.000Z".to_owned()),
            ..run_row("clean")
        })
        .unwrap();
        log.append(&ev(
            "crashed",
            1,
            None,
            Event::TurnStarted(TurnStarted { index: 0 }),
        ))
        .unwrap();
    }

    let mut log = EventLog::open(&path).unwrap();
    let orphans = log.running_runs().unwrap();
    assert_eq!(orphans.len(), 1);
    assert_eq!(orphans[0].id, RunId::from("crashed"));
    // §13.1: the shadow ref is on the run, so Review still has attribution for
    // a run whose process died.
    assert_eq!(
        orphans[0].shadow_ref.as_deref(),
        Some("refs/agent-workspace/runs/crashed")
    );

    log.finish_run(
        &RunId::from("crashed"),
        RunStatus::Abandoned,
        "2026-09-12T15:00:00.000Z",
    )
    .unwrap();
    assert!(log.running_runs().unwrap().is_empty());
    // The events of an abandoned run are still there. Nothing is ever deleted.
    assert_eq!(log.events(&RunId::from("crashed")).unwrap().len(), 1);
}

#[test]
fn metrics_sum_cost_per_turn_and_merge_models_across_turns() {
    let mut log = open_with_run("r");
    let sonnet = |i, o, c| ModelUsage {
        model: "claude-sonnet-5".to_owned(),
        input_tokens: i,
        output_tokens: o,
        cost_usd: c,
        ..Default::default()
    };
    let haiku = |i, o, c| ModelUsage {
        model: "claude-haiku-4-5-20251001".to_owned(),
        input_tokens: i,
        output_tokens: o,
        cost_usd: c,
        ..Default::default()
    };
    log.append_all(&[
        ev(
            "r",
            1,
            None,
            turn(0, 0.03, vec![sonnet(100, 10, 0.028), haiku(20, 5, 0.002)]),
        ),
        ev("r", 2, None, turn(1, 0.05, vec![sonnet(200, 20, 0.050)])),
    ])
    .unwrap();

    let m = log.metrics(&RunId::from("r")).unwrap();
    assert_eq!(m.turns, 2);
    assert!((m.cost_usd - 0.08).abs() < 1e-9);
    assert_eq!(m.input_tokens, 320);
    assert_eq!(m.output_tokens, 35);
    assert_eq!(m.duration_ms, Some(2000));
    assert_eq!(
        m.by_model.len(),
        2,
        "models merge across turns, not per turn"
    );
    let s = m
        .by_model
        .iter()
        .find(|x| x.model == "claude-sonnet-5")
        .unwrap();
    assert_eq!(s.input_tokens, 300);
    assert!((s.cost_usd - 0.078).abs() < 1e-9);
}

#[test]
fn health_counts_unclassified_for_drift_detection() {
    let (name, path) = fixtures()
        .into_iter()
        .find(|(n, _)| n == "tool-bash")
        .expect("tool-bash fixture");
    let text = fs::read_to_string(&path).unwrap();
    let run = RunId::from(name.as_str());
    let mut n = ClaudeCodeNormalizer::with_clock(run.clone(), Box::new(FixedClock::default()));
    let mut produced: Vec<RawEvent> = text.lines().flat_map(|l| n.push_line(l)).collect();
    produced.extend(n.finish(Some(0)));

    let mut log = open_with_run(&name);
    log.append_all(&produced).unwrap();

    let h = log.health(&run).unwrap();
    assert_eq!(h.events, produced.len() as u64);
    assert_eq!(h.by_type.values().sum::<u64>(), h.events);
    assert!(
        h.unclassified_ratio() < 0.5,
        "ratio: {}",
        h.unclassified_ratio()
    );
    assert!(h.by_type.contains_key("tool.started"));
}

/// Events must be readable by a foreign-key-less path too: a run row is
/// required, so an event for a run we never recorded is refused rather than
/// orphaned.
#[test]
fn an_event_for_an_unknown_run_is_refused() {
    let mut log = EventLog::open_in_memory().unwrap();
    let err = log
        .append(&ev(
            "ghost",
            1,
            None,
            Event::TurnStarted(TurnStarted { index: 0 }),
        ))
        .unwrap_err();
    assert!(matches!(err, Error::Sqlite(_)), "got {err:?}");
}

// ------------------------------------------------------- two connections --

/// The WAL claim in `EventLog::from_conn`, exercised instead of asserted.
///
/// Two connections to one file is not an edge case but the shape of the app:
/// `driver::start` consumes an `EventLog` by value and gives it back only from
/// `Session::wait()`, so the shell cannot lend the UI's log to a run and keeps
/// its own. Everything the UI does to a live run — the run row, the first
/// render, the tail — happens on a connection that never sees the writer's
/// transactions except as committed data.
#[test]
fn a_second_connection_reads_a_run_the_first_is_still_writing() {
    let dir = tempdir();
    let path = dir.join("log.sqlite");

    // A is the run's writer, as `driver::start` would hold it; B is the UI's,
    // opened separately and never closed or reopened while A works.
    let mut writer = EventLog::open(&path).unwrap();
    let reader = EventLog::open(&path).unwrap();

    let run = RunId::from("live");
    writer.create_run(&run_row("live")).unwrap();
    writer
        .append(&ev(
            "live",
            1,
            None,
            Event::TurnStarted(TurnStarted { index: 0 }),
        ))
        .unwrap();

    // The run is visible on B as running, which is the state the UI renders it
    // in for as long as A is alive.
    let seen = reader.run(&run).unwrap().expect("run not visible on B");
    assert_eq!(seen.status, RunStatus::Running);
    assert_eq!(seen.prompt.as_deref(), Some("add refresh-token support"));

    let events = reader.events(&run).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].seq, 1);
    assert_eq!(reader.last_seq(&run).unwrap(), 1);

    // And the tail advances: B does not have to reopen to see A's later
    // appends, which is the whole of §7's live feed.
    writer
        .append(&ev(
            "live",
            2,
            None,
            Event::TurnStarted(TurnStarted { index: 1 }),
        ))
        .unwrap();
    let tail = reader.events_after(&run, 1, u32::MAX).unwrap();
    assert_eq!(tail.len(), 1);
    assert_eq!(tail[0].seq, 2);
    assert_eq!(reader.last_seq(&run).unwrap(), 2);

    writer
        .finish_run(&run, RunStatus::Finished, "2026-09-12T14:30:00.000Z")
        .unwrap();
    assert_eq!(
        reader.run(&run).unwrap().unwrap().status,
        RunStatus::Finished
    );
}

/// The other direction: a write that loses the race must wait for the winner,
/// not fail. With `BUSY_TIMEOUT` at zero this is an instant `SQLITE_BUSY` and
/// the user sees "database is locked" on a database that is merely busy —
/// checked by hand against this test, which is how the value earns a name.
///
/// What it does *not* catch is deleting the `busy_timeout` call outright:
/// rusqlite's own `Connection::open` sets 5s, so the behaviour survives. That
/// is the point of asserting the behaviour rather than the setting — this
/// passes whoever supplies the timeout, and fails the moment nobody does.
///
/// The lock is taken with a raw `BEGIN IMMEDIATE` because `EventLog` exposes
/// no way to hold a transaction open across calls. It stands in for the
/// in-flight `append_all` of a running run — the real collision being §8's
/// boot reconcile, which `finish_run`s orphans while a started run appends.
///
/// **On the sleep:** it only widens the window in which the two overlap, and
/// nothing is asserted about any duration. The failing direction is
/// deterministic — a connection that will not wait fails the instant it
/// touches the held lock, so the join below returns `Err` and the test fails.
/// A scheduling accident can only make the test pass without having contended;
/// it cannot make it fail spuriously, which is the tradeoff worth having when
/// the alternative is a test that asserts on wall-clock time.
#[test]
fn a_write_waits_out_the_other_connections_transaction() {
    use rusqlite::Connection;
    use std::sync::{Arc, Barrier};
    use std::time::Duration;

    let dir = tempdir();
    let path = dir.join("log.sqlite");

    let mut a = EventLog::open(&path).unwrap();
    a.create_run(&run_row("r")).unwrap();
    let mut b = EventLog::open(&path).unwrap();

    // Hold the write lock. An empty IMMEDIATE transaction is enough: it is the
    // lock, not the work under it, that the other connection collides with.
    let holder = Connection::open(&path).unwrap();
    holder.execute_batch("BEGIN IMMEDIATE").unwrap();

    let gate = Arc::new(Barrier::new(2));
    let theirs = Arc::clone(&gate);
    let writing = std::thread::spawn(move || {
        theirs.wait();
        b.finish_run(
            &RunId::from("r"),
            RunStatus::Finished,
            "2026-09-12T14:30:00.000Z",
        )
    });

    gate.wait();
    std::thread::sleep(Duration::from_millis(200));
    holder.execute_batch("COMMIT").unwrap();

    writing
        .join()
        .unwrap()
        .expect("the write should have waited for the lock, not failed");
    assert_eq!(
        a.run(&RunId::from("r")).unwrap().unwrap().status,
        RunStatus::Finished
    );
}

fn tempdir() -> PathBuf {
    let p = std::env::temp_dir().join(format!(
        "event-log-test-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = fs::remove_dir_all(&p);
    fs::create_dir_all(&p).unwrap();
    p
}
