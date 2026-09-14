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
    AssistantMessage, Event, ModelUsage, PermissionRequested, PermissionResolved, RawEvent, RunId,
    ToolUseId, TurnFinished, TurnStarted, Unclassified, UnclassifiedReason, Usage, UserMessage,
    SCHEMA_VERSION,
};
use event_log::{Activity, Appended, Error, EventLog, LaunchFlags, Run, RunStatus};
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
        model: Some("claude-sonnet-5".to_owned()),
        launch_flags: LaunchFlags::default(),
        status: RunStatus::Running,
        started_at: "2026-09-12T14:22:00.000Z".to_owned(),
        finished_at: None,
        parent_run_id: None,
        branch_point_seq: None,
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

/// A run can end and start again, and the row has to survive the round trip.
///
/// Not a hypothetical: nothing closes an agent's stdin on its behalf, so the
/// only way to stop paying for an idle conversation is to end it, and typing
/// into it again resumes the same session under the same row
/// (`docs/verification/idle-run-cost.md`). `finished_at` is the field that
/// makes this more than a status flip — a row that says `running` next to the
/// moment it finished is describing two runs at once, and `running_runs`, which
/// is where crash recovery starts, selects on the status alone.
#[test]
fn a_run_that_is_resumed_is_running_again_with_no_finish_time() {
    let run = RunId::from("woken");
    let mut log = open_with_run("woken");
    log.finish_run(&run, RunStatus::Finished, "2026-09-14T09:00:00.000Z")
        .unwrap();
    assert!(log.running_runs().unwrap().is_empty());

    log.reopen_run(&run).unwrap();
    let row = log.run(&run).unwrap().unwrap();
    assert_eq!(row.status, RunStatus::Running);
    assert_eq!(row.finished_at, None);
    assert_eq!(
        log.running_runs().unwrap().len(),
        1,
        "a woken run is reconcilable like any other running run"
    );

    // Twice, because a run is woken as often as the user comes back to it.
    log.finish_run(&run, RunStatus::Finished, "2026-09-14T10:00:00.000Z")
        .unwrap();
    log.reopen_run(&run).unwrap();
    assert_eq!(log.run(&run).unwrap().unwrap().status, RunStatus::Running);

    let err = log
        .reopen_run(&RunId::from("never-existed"))
        .expect_err("there is no row to reopen");
    assert!(
        err.to_string().contains("never-existed"),
        "reopening a run that is not here says which one: {err}"
    );
}

/// A tree removed after the fact: the column stops naming it, and nothing else
/// on the row moves.
///
/// `worktree_path` is nullable for this one reason — §13 creates every run with
/// a tree, so it is never empty at the start — and `driver::Registry::clean_up`
/// is the only thing that empties it. The column is *dereferenced*
/// (`driver::resume` opens it to find the run's git dir and shadow index), so
/// leaving it pointing at a directory that is gone is a resume that fails
/// inside git some weeks later.
///
/// The three assertions about what did *not* change are the point. `shadow_ref`
/// in particular survives a cleanup on disk and must survive it here: it lives
/// in the shared ref namespace, outlives the checkout (§13.1), and is what a
/// fork of a cleaned-up run is placed from. Clearing it alongside would be the
/// tidy-looking change that silently makes every cleaned-up run unforkable.
#[test]
fn clearing_a_worktree_path_leaves_the_rest_of_the_row_alone() {
    let run = RunId::from("cleaned");
    let mut log = open_with_run("cleaned");
    let before = log.run(&run).unwrap().unwrap();
    assert!(before.worktree_path.is_some());

    log.clear_worktree_path(&run).unwrap();

    let after = log.run(&run).unwrap().unwrap();
    assert_eq!(after.worktree_path, None);
    assert_eq!(
        after.shadow_ref, before.shadow_ref,
        "the shadow ref outlives the checkout, so a cleanup must not clear it"
    );
    assert_eq!(after.branch, before.branch);
    assert_eq!(after.status, before.status);
    assert_eq!(
        Run {
            worktree_path: before.worktree_path.clone(),
            ..after.clone()
        },
        before,
        "exactly one column moved"
    );

    // Idempotent, because cleanup is: a second click lands here.
    log.clear_worktree_path(&run).unwrap();
    assert_eq!(log.run(&run).unwrap().unwrap().worktree_path, None);

    let err = log
        .clear_worktree_path(&RunId::from("never-existed"))
        .expect_err("there is no row to clear");
    assert!(
        err.to_string().contains("never-existed"),
        "clearing a run that is not here says which one: {err}"
    );
}

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

/// The graph surface (§10) draws finished branches as well as live ones, so
/// this query must answer with what `running_runs` deliberately hides.
#[test]
fn recent_runs_returns_finished_runs_newest_first_and_honours_the_limit() {
    let mut log = EventLog::open_in_memory().unwrap();
    log.create_run(&Run {
        started_at: "2026-09-12T14:00:00.000Z".to_owned(),
        ..run_row("oldest")
    })
    .unwrap();
    log.create_run(&Run {
        status: RunStatus::Finished,
        started_at: "2026-09-12T15:00:00.000Z".to_owned(),
        finished_at: Some("2026-09-12T15:05:00.000Z".to_owned()),
        ..run_row("middle")
    })
    .unwrap();
    log.create_run(&Run {
        status: RunStatus::Failed,
        started_at: "2026-09-12T16:00:00.000Z".to_owned(),
        finished_at: Some("2026-09-12T16:01:00.000Z".to_owned()),
        parent_run_id: Some(RunId::from("middle")),
        branch_point_seq: Some(12),
        ..run_row("newest")
    })
    .unwrap();

    let ids: Vec<_> = log
        .recent_runs(10)
        .unwrap()
        .into_iter()
        .map(|r| r.id)
        .collect();
    assert_eq!(
        ids,
        vec![
            RunId::from("newest"),
            RunId::from("middle"),
            RunId::from("oldest")
        ],
        "newest first, and a finished or failed run is still a run"
    );

    // The edge the graph is drawn from survives the round trip; nothing else
    // in this repository records it (§10).
    let newest = log.recent_runs(1).unwrap();
    assert_eq!(newest.len(), 1, "the limit is honoured");
    assert_eq!(newest[0].parent_run_id, Some(RunId::from("middle")));
    assert_eq!(newest[0].branch_point_seq, Some(12));
}

// --------------------------------------------------------------- the edge ---

/// The graph edge, both halves, through the database and back.
///
/// This is the only place the edge will ever live. The provider records no
/// parentage — a forked transcript is a full copy re-stamped with its own
/// session id (`docs/verification/claude-code-session-forking.md`) — so unlike
/// every other number the UI shows, this one cannot be recomputed if the write
/// is lost. Hence a column, against §8's rule that recomputable things are
/// projections, and hence a test that reads it back off disk rather than out of
/// the struct it was built from.
#[test]
fn a_forks_parent_and_branch_point_survive_the_round_trip() {
    let mut log = EventLog::open_in_memory().unwrap();
    log.create_run(&run_row("trunk")).unwrap();
    log.create_run(&Run {
        started_at: "2026-09-12T15:00:00.000Z".to_owned(),
        parent_run_id: Some(RunId::from("trunk")),
        branch_point_seq: Some(17),
        ..run_row("branch")
    })
    .unwrap();

    let branch = log.run(&RunId::from("branch")).unwrap().expect("stored");
    assert_eq!(branch.parent_run_id, Some(RunId::from("trunk")));
    assert_eq!(
        branch.branch_point_seq,
        Some(17),
        "the branch point is *where* in the parent, and nothing else records it"
    );

    let trunk = log.run(&RunId::from("trunk")).unwrap().expect("stored");
    assert_eq!(trunk.parent_run_id, None);
    assert_eq!(
        trunk.branch_point_seq, None,
        "a root run has no branch point"
    );

    // Every path that reads a row reads both halves, not just `run()` --
    // §10's graph is drawn from `recent_runs`, and §8's boot reconcile walks
    // `running_runs`.
    let recent = log.recent_runs(10).unwrap();
    let drawn = recent
        .iter()
        .find(|r| r.id == RunId::from("branch"))
        .expect("recent_runs sees the fork");
    assert_eq!(drawn.branch_point_seq, Some(17));
    let running = log.running_runs().unwrap();
    let orphan = running
        .iter()
        .find(|r| r.id == RunId::from("branch"))
        .expect("running_runs sees the fork");
    assert_eq!(orphan.branch_point_seq, Some(17));
}

/// The pairing invariant, reached without a database.
///
/// Deliberately not a SQL `CHECK`: SQLite cannot add one through
/// `ALTER TABLE` without rebuilding the table, and the repo's rule is that
/// anything which starts deciding belongs where a test can reach it. This test
/// is what "reach it" means — no file, no connection, no migration.
#[test]
fn half_a_fork_edge_is_refused_without_a_database() {
    run_row("root")
        .check_fork_pairing()
        .expect("neither half: a root run");
    Run {
        parent_run_id: Some(RunId::from("trunk")),
        branch_point_seq: Some(4),
        ..run_row("fork")
    }
    .check_fork_pairing()
    .expect("both halves: a fork");

    // A parent with no branch point draws an arrow the surface cannot anchor.
    let dangling = Run {
        parent_run_id: Some(RunId::from("trunk")),
        branch_point_seq: None,
        ..run_row("dangling")
    };
    assert!(
        matches!(
            dangling.check_fork_pairing(),
            Err(Error::IncompleteForkEdge { ref run, .. }) if *run == RunId::from("dangling")
        ),
        "a parent without a branch point must be refused"
    );

    // A branch point with no parent names a seq in a log nobody can name.
    let orphaned = Run {
        parent_run_id: None,
        branch_point_seq: Some(4),
        ..run_row("orphaned")
    };
    assert!(
        matches!(
            orphaned.check_fork_pairing(),
            Err(Error::IncompleteForkEdge { .. })
        ),
        "a branch point without a parent must be refused"
    );
}

/// And the log applies it, so nothing reaches the table by another route.
#[test]
fn create_run_refuses_half_a_fork_edge() {
    let mut log = EventLog::open_in_memory().unwrap();
    log.create_run(&run_row("trunk")).unwrap();
    let err = log
        .create_run(&Run {
            parent_run_id: Some(RunId::from("trunk")),
            branch_point_seq: None,
            ..run_row("half")
        })
        .unwrap_err();
    assert!(matches!(err, Error::IncompleteForkEdge { .. }), "{err}");
    assert!(
        log.run(&RunId::from("half")).unwrap().is_none(),
        "the row must not have been written"
    );
}

/// The `runs` table exactly as it shipped at `user_version` 1.
///
/// A frozen copy, and it must **not** be updated when the schema moves — that
/// is the whole point. `EventLog::open` on a fresh file runs every migration in
/// order, so the fresh path is exercised by every other test in this file and
/// the upgrade path by none of them. The upgrade is the one that runs on a
/// user's existing log, and it is the one that breaks in the field.
const V1_AS_SHIPPED: &str = r#"
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

/// A log written by the previous build gains the column and keeps its rows.
///
/// Both halves matter. A migration that loses the old runs is obvious; one that
/// lands an *existing* database in a different shape from a fresh one is not,
/// and shows up much later as a query that works on the developer's machine and
/// not on anyone else's. So this asserts the old row survives, reads back with
/// no branch point, and that a fork can be recorded afterwards.
#[test]
fn an_existing_v1_database_migrates_and_keeps_its_runs() {
    use rusqlite::Connection;

    let dir = tempdir();
    let path = dir.join("log.sqlite");

    // A database as the previous build left it: V1's schema, V1's
    // `user_version`, and a run in it.
    {
        let old = Connection::open(&path).unwrap();
        old.execute_batch(V1_AS_SHIPPED).unwrap();
        old.pragma_update(None, "user_version", 1u32).unwrap();
        old.execute(
            "INSERT INTO runs (id, agent, workspace_path, status, started_at)
             VALUES ('before', 'claude-code', '/home/u/my-app', 'finished',
                     '2026-09-12T14:22:00.000Z')",
            [],
        )
        .unwrap();
        assert!(
            old.query_row(
                "SELECT COUNT(*) FROM pragma_table_info('runs') WHERE name = 'branch_point_seq'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap()
                == 0,
            "the fixture is only meaningful if the old schema lacks the column"
        );
    }

    let mut log = EventLog::open(&path).unwrap();

    let before = log.run(&RunId::from("before")).unwrap().expect("kept");
    assert_eq!(before.status, RunStatus::Finished);
    assert_eq!(
        before.branch_point_seq, None,
        "a run recorded before the column existed is a root run"
    );

    // The migrated database is the same database a fresh one would have been:
    // a fork records both halves and reads them back.
    log.create_run(&Run {
        parent_run_id: Some(RunId::from("before")),
        branch_point_seq: Some(9),
        ..run_row("after")
    })
    .unwrap();
    assert_eq!(
        log.run(&RunId::from("after"))
            .unwrap()
            .unwrap()
            .branch_point_seq,
        Some(9)
    );

    // And the log itself still works on the upgraded file: the events table
    // came from V1 and the migration must not have disturbed it.
    log.append(&ev(
        "after",
        1,
        None,
        Event::TurnStarted(TurnStarted { index: 0 }),
    ))
    .unwrap();
    assert_eq!(log.events(&RunId::from("after")).unwrap().len(), 1);

    // Re-opening is a no-op rather than a second attempt at the ALTER.
    drop(log);
    let reopened = EventLog::open(&path).unwrap();
    assert!(reopened.run(&RunId::from("after")).unwrap().is_some());
}

/// The V3 column lands on a database written by the build before it, too.
///
/// The V1 fixture above is frozen and stays frozen; V2 is that schema plus the
/// one `ALTER` V2 shipped, which is the only honest way to build "what the
/// previous release left on disk" without freezing a second copy of the whole
/// thing. The upgrade path is the one that runs on a user's log and the one no
/// other test in this file exercises.
#[test]
fn an_existing_v2_database_migrates_and_keeps_its_runs() {
    use rusqlite::Connection;

    let dir = tempdir();
    let path = dir.join("log.sqlite");

    {
        let old = Connection::open(&path).unwrap();
        old.execute_batch(V1_AS_SHIPPED).unwrap();
        old.execute_batch("ALTER TABLE runs ADD COLUMN branch_point_seq INTEGER;")
            .unwrap();
        old.pragma_update(None, "user_version", 2u32).unwrap();
        old.execute(
            "INSERT INTO runs (id, agent, workspace_path, status, started_at)
             VALUES ('before', 'claude-code', '/home/u/my-app', 'finished',
                     '2026-09-12T14:22:00.000Z')",
            [],
        )
        .unwrap();
        assert_eq!(
            old.query_row(
                "SELECT COUNT(*) FROM pragma_table_info('runs') WHERE name = 'model'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0,
            "the fixture is only meaningful if the old schema lacks the column"
        );
    }

    let mut log = EventLog::open(&path).unwrap();

    let before = log.run(&RunId::from("before")).unwrap().expect("kept");
    assert_eq!(before.status, RunStatus::Finished);
    assert_eq!(
        before.model, None,
        "a run recorded before the column existed names no model, and inventing \
         the default it probably used would be a guess stored as a fact"
    );

    // The migrated database is the same database a fresh one would have been.
    log.create_run(&Run {
        model: Some("claude-opus-4-6".to_owned()),
        ..run_row("after")
    })
    .unwrap();
    assert_eq!(
        log.run(&RunId::from("after"))
            .unwrap()
            .unwrap()
            .model
            .as_deref(),
        Some("claude-opus-4-6")
    );

    // And the rest of the row still reads back: this ALTER lands in the middle
    // of the column list every query names, so a mis-ordered `SELECT` would
    // show up here as a status or a timestamp in the wrong field.
    let after = log.run(&RunId::from("after")).unwrap().unwrap();
    assert_eq!(after.status, RunStatus::Running);
    assert_eq!(after.started_at, "2026-09-12T14:22:00.000Z");
    assert_eq!(after.branch.as_deref(), Some("feat/oauth"));

    log.append(&ev(
        "after",
        1,
        None,
        Event::TurnStarted(TurnStarted { index: 0 }),
    ))
    .unwrap();
    assert_eq!(log.events(&RunId::from("after")).unwrap().len(), 1);

    drop(log);
    let reopened = EventLog::open(&path).unwrap();
    assert!(reopened.run(&RunId::from("after")).unwrap().is_some());
}

// ---------------------------------------------------------------- activity ---

/// The three-word vocabulary, reached without a database.
///
/// This is the decision `docs/branching-surface.md` fixes and the shell is not
/// allowed to make: five run states, three words. `failed`, `abandoned` and
/// `interrupted` land on `idle` because the vocabulary answers "does this node
/// want me now", and on that axis a run that is not running and not blocked is
/// the same whatever became of it. Outcome is a second axis and `RunStatus`
/// still carries it.
///
/// `interrupted` is the state that arrived last and it bought no fourth word.
/// It had better not: it is the *ordinary* ending — nothing closes a run's
/// stdin for it, so app exit interrupts every live run — and a vocabulary in
/// which the common case demands attention says nothing at all.
#[test]
fn five_run_states_collapse_into_three_words() {
    assert_eq!(Activity::of(RunStatus::Running, false), Activity::Running);
    assert_eq!(Activity::of(RunStatus::Running, true), Activity::NeedsYou);
    assert_eq!(Activity::of(RunStatus::Finished, false), Activity::Idle);
    assert_eq!(Activity::of(RunStatus::Failed, false), Activity::Idle);
    assert_eq!(Activity::of(RunStatus::Abandoned, false), Activity::Idle);
    assert_eq!(Activity::of(RunStatus::Interrupted, false), Activity::Idle);

    // A dead process is not waiting for an answer. An unresolved request on a
    // run that has ended is a record of how it ended, not a demand on anyone.
    assert_eq!(Activity::of(RunStatus::Finished, true), Activity::Idle);
    assert_eq!(Activity::of(RunStatus::Failed, true), Activity::Idle);
    assert_eq!(Activity::of(RunStatus::Abandoned, true), Activity::Idle);
    assert_eq!(Activity::of(RunStatus::Interrupted, true), Activity::Idle);
}

/// Every status survives the round trip through the `TEXT` column, and the
/// spellings are the wire.
///
/// `RunStatus` is exported to TypeScript by `ts-rs` and stored by
/// `RunStatus::as_str`, so these strings are two contracts at once: a row
/// written by one build has to be readable by the next, and the shell's
/// `STATUS` table is keyed by exactly these words. A variant added without a
/// spelling is an `UnknownRunStatus` on somebody's existing database.
#[test]
fn every_run_status_round_trips_through_its_stored_spelling() {
    for (status, spelled) in [
        (RunStatus::Running, "running"),
        (RunStatus::Finished, "finished"),
        (RunStatus::Failed, "failed"),
        (RunStatus::Abandoned, "abandoned"),
        (RunStatus::Interrupted, "interrupted"),
    ] {
        assert_eq!(status.as_str(), spelled);
        assert_eq!(RunStatus::parse(spelled), Some(status));
    }
    assert_eq!(RunStatus::parse("stopped"), None);
}

/// **needs you** is derived from the log, and answered permissions clear it.
#[test]
fn an_unanswered_permission_request_is_needs_you() {
    let mut log = open_with_run("blocked");
    log.append_all(&[ev(
        "blocked",
        1,
        None,
        Event::PermissionRequested(PermissionRequested {
            tool_use_id: ToolUseId::from("toolu_01"),
            tool_name: "Edit".to_owned(),
            input: serde_json::json!({}),
        }),
    )])
    .unwrap();
    assert_eq!(activity(&log, "blocked").status, Activity::NeedsYou);

    // Resolved without echoing the tool_use_id, which the provider is entitled
    // to do — and which is why the pairing is by count and not by id. Pairing
    // on the id would leave this run blocked forever.
    log.append(&ev(
        "blocked",
        2,
        None,
        Event::PermissionResolved(PermissionResolved {
            tool_use_id: None,
            tool_name: "Edit".to_owned(),
            allowed: true,
            reason_type: None,
            reason: None,
        }),
    ))
    .unwrap();
    assert_eq!(activity(&log, "blocked").status, Activity::Running);
}

/// Messages are not events, and one assistant message is not one per block.
#[test]
fn the_message_count_counts_messages() {
    let mut log = open_with_run("chatty");
    log.append_all(&[
        ev(
            "chatty",
            1,
            None,
            Event::UserMessage(UserMessage {
                text: "add refresh-token support".to_owned(),
                // The only form our own prompt takes in the log: the driver
                // passes `--replay-user-messages`, so this *is* the user's
                // message rather than a second copy of one.
                is_replay: true,
            }),
        ),
        // One API message, three content blocks, one `message_id` (§5).
        ev("chatty", 2, None, assistant("msg_01", 0)),
        ev("chatty", 3, None, assistant("msg_01", 1)),
        ev("chatty", 4, None, assistant("msg_01", 2)),
        // Tool traffic is not conversation.
        ev(
            "chatty",
            5,
            None,
            Event::PermissionRequested(PermissionRequested {
                tool_use_id: ToolUseId::from("toolu_01"),
                tool_name: "Edit".to_owned(),
                input: serde_json::json!({}),
            }),
        ),
        ev(
            "chatty",
            6,
            None,
            Event::PermissionResolved(PermissionResolved {
                tool_use_id: Some(ToolUseId::from("toolu_01")),
                tool_name: "Edit".to_owned(),
                allowed: true,
                reason_type: None,
                reason: None,
            }),
        ),
        ev("chatty", 7, None, assistant("msg_02", 0)),
    ])
    .unwrap();

    let a = activity(&log, "chatty");
    assert_eq!(
        a.messages, 3,
        "one prompt and two answers — not seven events, and not four blocks"
    );
    assert_eq!(a.status, Activity::Running);

    // A run nobody has said anything to yet reads as zero rather than as
    // missing: the query is a LEFT JOIN for exactly this row.
    log.create_run(&run_row("silent")).unwrap();
    assert_eq!(activity(&log, "silent").messages, 0);
}

/// Forkability is a fact about a block, computed where a test reaches it.
///
/// The rule is `normalizer::claude_code::fork_address` and this is the
/// projection over it: the hover toolbar appears on a block if and only if the
/// block is in this answer. Four things it has to get right, and all four are
/// live findings rather than opinions
/// (`docs/verification/claude-code-fork-address.md`,
/// `docs/verification/claude-code-fork-anchor-types.md`):
/// a fanned-out `{uuid}#{i}` block is still forkable at its bare uuid, an event
/// with no key is not forkable at all and must not round to a neighbour, a
/// **keyed** block whose event did not come off an `assistant` or `user` line
/// is not forkable either, and the inherited message count is the count *at*
/// the block.
#[test]
fn fork_points_are_the_blocks_that_carry_a_provider_address() {
    let mut log = open_with_run("branchy");
    log.append_all(&[
        // The user's own prompt: a chain entry we hold no key for, because it
        // never reaches stdout. Counted as a message, not offered as a fork.
        ev(
            "branchy",
            1,
            None,
            Event::UserMessage(UserMessage {
                text: "add refresh-token support".to_owned(),
                is_replay: true,
            }),
        ),
        // One API message, two content blocks, one uuid fanned out by us.
        ev("branchy", 2, Some("u1#0"), assistant("msg_01", 0)),
        ev("branchy", 3, Some("u1#1"), assistant("msg_01", 1)),
        // Out-of-band: reaches us over the MCP channel, never over stdout.
        ev(
            "branchy",
            4,
            None,
            Event::PermissionRequested(PermissionRequested {
                tool_use_id: ToolUseId::from("toolu_01"),
                tool_name: "Edit".to_owned(),
                input: serde_json::json!({}),
            }),
        ),
        // Keyed, and refused by the provider. The normalizer keys every
        // uuid-bearing stdout line, so a `system`/`hook_started` line arrives
        // here with an address-shaped key that resolves nowhere — which is the
        // counter-example the built surface found on its first day.
        ev(
            "branchy",
            5,
            Some("u2"),
            Event::Unclassified(Unclassified {
                source_type: Some("system".to_owned()),
                source_subtype: Some("hook_started".to_owned()),
                reason: UnclassifiedReason::NotModeled,
                line: "{}".to_owned(),
            }),
        ),
        // Keyed and refused for the same reason: `result` lines carry uuids
        // that are not chain entries, on both of their subtypes.
        ev(
            "branchy",
            6,
            Some("u3"),
            Event::TurnFinished(TurnFinished {
                index: 0,
                subtype: "success".to_owned(),
                ok: true,
                usage: Usage::from_models(0.0, Vec::new()),
                duration_ms: None,
                text: None,
                permission_denials: 0,
            }),
        ),
        ev("branchy", 7, Some("u4"), assistant("msg_02", 0)),
    ])
    .unwrap();

    let points = log.fork_points(&[RunId::from("branchy")]).unwrap();
    assert_eq!(points.len(), 1);
    let blocks = &points[0].blocks;
    assert_eq!(
        blocks.iter().map(|b| b.seq).collect::<Vec<_>>(),
        vec![2, 3, 7],
        "a keyed block off a message line is forkable and nothing else is — \
         neither the unkeyed prompt and permission request nor the keyed hook \
         and result lines, and none of them may round to a neighbour"
    );
    assert_eq!(
        blocks
            .iter()
            .map(|b| b.inherited_messages)
            .collect::<Vec<_>>(),
        vec![2, 2, 3],
        "a fanned-out message is one message, and the count is the count *at* \
         the block"
    );

    // The two numbers that appear beside each other on the canvas — a node's
    // `N msgs` and its child's "inherited M" — come out of one counting rule.
    assert_eq!(
        blocks.last().unwrap().inherited_messages,
        activity(&log, "branchy").messages,
        "a fork at the last block inherits the whole conversation"
    );
}

/// Nothing addressable is an answer, and it is not an empty list of runs.
///
/// The surface tells "this run cannot be forked yet" from "this run's fork
/// points have not arrived" by whether the row is there, so a run with no keyed
/// block must still get one.
#[test]
fn a_run_with_no_addressable_block_still_gets_a_row() {
    let mut log = open_with_run("quiet");
    log.append(&ev(
        "quiet",
        1,
        None,
        Event::TurnStarted(TurnStarted { index: 0 }),
    ))
    .unwrap();
    let points = log.fork_points(&[RunId::from("quiet")]).unwrap();
    assert_eq!(points.len(), 1);
    assert_eq!(points[0].run_id, RunId::from("quiet"));
    assert!(points[0].blocks.is_empty());
}

fn assistant(message_id: &str, block_index: u32) -> Event {
    Event::AssistantMessage(AssistantMessage {
        text: "…".to_owned(),
        message_id: Some(message_id.to_owned()),
        block_index,
        parent_tool_use_id: None,
    })
}

fn activity(log: &EventLog, id: &str) -> event_log::RunActivity {
    log.recent_activity(100)
        .unwrap()
        .into_iter()
        .find(|a| a.run_id == RunId::from(id))
        .expect("every run in the window gets a row")
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

/// The same promise, for the write that actually runs concurrently: two live
/// runs appending on their own connections.
///
/// This is not covered by the test above, and the difference is the whole
/// finding. `append_all` reads before it writes — `last_seq` for the batch's
/// run — so a DEFERRED transaction would be holding a read lock at the moment
/// it asks for the write lock, and SQLite refuses to promote a read lock while
/// another connection holds a reserved one *without calling the busy handler*:
/// two connections doing that to each other would deadlock, so it returns
/// `SQLITE_BUSY` instantly and hopes someone backs off. The timeout is real and
/// simply never consulted, which is why this failed for a run started while
/// another was writing and looked exactly like a five-second timeout that had
/// been ignored.
///
/// The sleep has the same role as above: it widens the overlap and nothing is
/// asserted about it. A scheduling accident can only make this pass without
/// contending.
#[test]
fn an_append_waits_out_another_connections_write_instead_of_failing() {
    use rusqlite::Connection;
    use std::sync::{Arc, Barrier};
    use std::time::Duration;

    let dir = tempdir();
    let path = dir.join("log.sqlite");

    let mut a = EventLog::open(&path).unwrap();
    a.create_run(&run_row("r")).unwrap();
    let mut b = EventLog::open(&path).unwrap();

    let holder = Connection::open(&path).unwrap();
    holder.execute_batch("BEGIN IMMEDIATE").unwrap();

    let gate = Arc::new(Barrier::new(2));
    let theirs = Arc::clone(&gate);
    let appending = std::thread::spawn(move || {
        theirs.wait();
        b.append_all(&[ev("r", 1, None, turn(0, 0.01, vec![]))])
    });

    gate.wait();
    std::thread::sleep(Duration::from_millis(200));
    holder.execute_batch("COMMIT").unwrap();

    appending
        .join()
        .unwrap()
        .expect("the append should have waited for the lock, not failed");
    assert_eq!(a.events(&RunId::from("r")).unwrap().len(), 1);
}

/// The V4 column lands on a database written by the build before it, too.
///
/// The V1 fixture above is frozen and stays frozen; V3 is that schema plus the
/// two `ALTER`s V2 and V3 shipped, which is the only honest way to build "what
/// the previous release left on disk" without freezing a copy of the whole
/// thing per version. Every other test in this file opens a fresh file, which
/// runs every migration in order and exercises the upgrade path not at all —
/// and the upgrade is the one that runs on a user's existing log.
#[test]
fn an_existing_v3_database_migrates_and_keeps_its_runs() {
    use rusqlite::Connection;

    let dir = tempdir();
    let path = dir.join("log.sqlite");

    {
        let old = Connection::open(&path).unwrap();
        old.execute_batch(V1_AS_SHIPPED).unwrap();
        old.execute_batch("ALTER TABLE runs ADD COLUMN branch_point_seq INTEGER;")
            .unwrap();
        old.execute_batch("ALTER TABLE runs ADD COLUMN model TEXT;")
            .unwrap();
        old.pragma_update(None, "user_version", 3u32).unwrap();
        old.execute(
            "INSERT INTO runs (id, agent, workspace_path, branch, model, status, started_at)
             VALUES ('before', 'claude-code', '/home/u/my-app', 'feat/oauth',
                     'claude-opus-4-6', 'finished', '2026-09-12T14:22:00.000Z')",
            [],
        )
        .unwrap();
        assert_eq!(
            old.query_row(
                "SELECT COUNT(*) FROM pragma_table_info('runs') WHERE name = 'launch_flags'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0,
            "the fixture is only meaningful if the old schema lacks the column"
        );
    }

    let mut log = EventLog::open(&path).unwrap();

    let before = log.run(&RunId::from("before")).unwrap().expect("kept");
    assert_eq!(before.status, RunStatus::Finished);
    assert_eq!(
        before.model.as_deref(),
        Some("claude-opus-4-6"),
        "V3's column survives V4's ALTER"
    );
    assert!(
        before.launch_flags.is_empty(),
        "a run recorded before the column existed stores no flags, and that reads \
         back as the same empty value a run launched with none does — there is \
         nothing to tell them apart with, and inventing an allow-list for it \
         would pre-grant tools nobody can show were granted"
    );

    // The migrated database is the same database a fresh one would have been:
    // flags written after the upgrade round-trip.
    let flags = LaunchFlags {
        allowed_tools: vec!["Read".to_owned(), "Edit".to_owned()],
        disallowed_tools: vec!["Bash".to_owned()],
        permission_mode: Some("acceptEdits".to_owned()),
        max_turns: Some(7),
        max_budget_usd: Some(2.5),
        append_system_prompt: Some("be terse".to_owned()),
        extra: vec!["--some-flag".to_owned()],
    };
    log.create_run(&Run {
        launch_flags: flags.clone(),
        ..run_row("after")
    })
    .unwrap();
    assert_eq!(
        log.run(&RunId::from("after"))
            .unwrap()
            .unwrap()
            .launch_flags,
        flags
    );

    // And the rest of the row still reads back. This ALTER lands in the middle
    // of the column list every query names and shifts every index after it in
    // `row_to_run`, so a mis-ordered `SELECT` shows up here as a status or a
    // timestamp in the wrong field — which is what the V3 test caught for the
    // column before this one.
    let after = log.run(&RunId::from("after")).unwrap().unwrap();
    assert_eq!(after.status, RunStatus::Running);
    assert_eq!(after.started_at, "2026-09-12T14:22:00.000Z");
    assert_eq!(after.branch.as_deref(), Some("feat/oauth"));
    assert_eq!(after.model.as_deref(), Some("claude-sonnet-5"));
    assert_eq!(
        log.recent_runs(10).unwrap().len(),
        2,
        "the list query names the same columns and must have moved with it"
    );

    log.append(&ev(
        "after",
        1,
        None,
        Event::TurnStarted(TurnStarted { index: 0 }),
    ))
    .unwrap();
    assert_eq!(log.events(&RunId::from("after")).unwrap().len(), 1);

    drop(log);
    let reopened = EventLog::open(&path).unwrap();
    assert!(reopened.run(&RunId::from("after")).unwrap().is_some());
}

/// A migration that dies part-way leaves the database exactly as it found it.
///
/// This is the one that decides whether a bad day costs a user their log. The
/// migrations are a dozen-odd statements and `user_version` is bumped after the
/// last of them, so applied bare they have a window in which some of the schema
/// is on disk and the version still says it is not: the next `open` re-runs the
/// same batch, collides with what the last attempt left, and fails — every
/// time, for good, with nothing in the crate able to undo it. §8's promise that
/// the log outlives the code is exactly the promise that state breaks.
///
/// Making a migration fail on purpose takes a collision, and the honest one to
/// plant is a column a later migration adds: a V1 database that somehow already
/// has V3's `model` gets through V2's `ALTER` and dies on V3's. That is the
/// interesting shape rather than a failure on the first statement, because V2's
/// column is then *already written* when the failure arrives — it is the
/// partial schema, and asserting it is absent afterwards is asserting the
/// rollback happened rather than that nothing was attempted.
///
/// Both halves are checked because both brick the log on their own: the version
/// must still be 1 (a bumped version would skip migrations this database has
/// not had) and `branch_point_seq` must be gone (a left-behind column makes
/// V2's `ALTER` fail forever on the next open). Then the collision is removed
/// and the log opens, which is the property that does not hold without the
/// transaction: before it, this last `open` died on
/// `duplicate column name: branch_point_seq` no matter what was cleaned up.
#[test]
fn a_failed_migration_leaves_the_database_at_its_old_version() {
    use rusqlite::Connection;

    let dir = tempdir();
    let path = dir.join("log.sqlite");

    {
        let old = Connection::open(&path).unwrap();
        old.execute_batch(V1_AS_SHIPPED).unwrap();
        old.pragma_update(None, "user_version", 1u32).unwrap();
        old.execute(
            "INSERT INTO runs (id, agent, workspace_path, status, started_at)
             VALUES ('before', 'claude-code', '/home/u/my-app', 'finished',
                     '2026-09-12T14:22:00.000Z')",
            [],
        )
        .unwrap();
        // The collision. Whatever put it there — a crashed migration, a
        // hand-edited log — the database now asks for V2, V3 and V4 and can
        // only be given the first of the three.
        old.execute_batch("ALTER TABLE runs ADD COLUMN model TEXT;")
            .unwrap();
    }

    let Err(err) = EventLog::open(&path) else {
        panic!("V3's ALTER must collide with the planted column");
    };
    assert!(
        err.to_string().contains("duplicate column name: model"),
        "the failure must be the planted collision, not something else: {err}"
    );

    {
        let after = Connection::open(&path).unwrap();
        assert_eq!(
            after
                .query_row("PRAGMA user_version", [], |r| r.get::<_, u32>(0))
                .unwrap(),
            1,
            "a migration that did not finish must not claim a version it did not reach"
        );
        assert_eq!(
            after
                .query_row(
                    "SELECT COUNT(*) FROM pragma_table_info('runs') \
                     WHERE name = 'branch_point_seq'",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            0,
            "V2's column was applied before V3 failed; it must have gone back with it"
        );
        assert_eq!(
            after
                .query_row("SELECT status FROM runs WHERE id = 'before'", [], |r| {
                    r.get::<_, String>(0)
                })
                .unwrap(),
            "finished",
            "the rollback is of the migration, not of the log"
        );

        // Clear the collision, which is the only thing that was ever actually
        // wrong with this file.
        after
            .execute_batch("ALTER TABLE runs DROP COLUMN model;")
            .unwrap();
    }

    // And now it opens, migrates the whole way, and keeps the run it had.
    let mut log = EventLog::open(&path).expect("the failed attempt left nothing in the way");
    assert_eq!(
        log.run(&RunId::from("before")).unwrap().unwrap().status,
        RunStatus::Finished
    );
    log.create_run(&run_row("after")).unwrap();
    log.append(&ev(
        "after",
        1,
        None,
        Event::TurnStarted(TurnStarted { index: 0 }),
    ))
    .unwrap();
    assert_eq!(log.events(&RunId::from("after")).unwrap().len(), 1);
}

/// A blob written by an older build decodes, and so does one written by a newer.
///
/// This is the price of choosing one JSON column over seven columns, and the
/// reason `LaunchFlags` carries `#[serde(default)]` on the container rather than
/// a version tag: a field this build does not know about is dropped, and a field
/// it knows and the blob lacks takes its default. Both directions are the
/// migration, written once, and neither needs a reader that branches on a
/// version.
#[test]
fn a_launch_flags_blob_survives_the_struct_changing_in_either_direction() {
    use rusqlite::Connection;

    let dir = tempdir();
    let path = dir.join("log.sqlite");
    {
        let mut log = EventLog::open(&path).unwrap();
        log.create_run(&run_row("r")).unwrap();
    }
    let write = |json: &str| {
        let c = Connection::open(&path).unwrap();
        c.execute("UPDATE runs SET launch_flags = ?1 WHERE id = 'r'", [json])
            .unwrap();
    };

    // An older build's blob: written before `extra` existed.
    write(r#"{"allowed_tools":["Read"],"max_turns":3}"#);
    let row = EventLog::open(&path)
        .unwrap()
        .run(&RunId::from("r"))
        .unwrap()
        .unwrap();
    assert_eq!(row.launch_flags.allowed_tools, vec!["Read".to_owned()]);
    assert_eq!(row.launch_flags.max_turns, Some(3));
    assert!(row.launch_flags.extra.is_empty());

    // A newer build's: a field this one has never heard of.
    write(r#"{"allowed_tools":["Edit"],"sandbox_profile":"strict"}"#);
    let row = EventLog::open(&path)
        .unwrap()
        .run(&RunId::from("r"))
        .unwrap()
        .unwrap();
    assert_eq!(
        row.launch_flags.allowed_tools,
        vec!["Edit".to_owned()],
        "the fields this build knows still decode around the one it does not"
    );

    // An empty object is the same state as NULL, which is the same state as a
    // run launched with nothing.
    write("{}");
    assert!(EventLog::open(&path)
        .unwrap()
        .run(&RunId::from("r"))
        .unwrap()
        .unwrap()
        .launch_flags
        .is_empty());
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
