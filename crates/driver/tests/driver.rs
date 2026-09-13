//! The driver, end to end, without Claude Code installed.
//!
//! A stub agent stands in for the real binary: it reads a script of stdout
//! lines, and — crucially — actually mutates the worktree between them. That is
//! what makes the §13.1 wiring testable. A stub that only printed JSON would
//! prove the parser works and nothing about whether the snapshot lands on the
//! right tool call.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use agent_protocol::{Event, RunId};
use driver::{LaunchSpec, Permission, Pipeline, RunSpec, Thinking};
use event_log::{EventLog, RunStatus};
use worktree::{ShadowHistory, Worktree};

// ------------------------------------------------------------------ helpers --

fn isolate_git_config() {
    static ONCE: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    ONCE.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("driver-test-cfg-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let cfg = dir.join("gitconfig");
        fs::write(&cfg, "[init]\n\tdefaultBranch = main\n").unwrap();
        std::env::set_var("GIT_CONFIG_GLOBAL", &cfg);
        std::env::set_var("GIT_CONFIG_SYSTEM", "/dev/null");
        cfg
    });
}

struct TempDir(PathBuf);
impl TempDir {
    fn new(name: &str) -> Self {
        let p = std::env::temp_dir().join(format!("driver-test-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        TempDir(p)
    }
    fn path(&self) -> &Path {
        &self.0
    }
}
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn git(cwd: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .current_dir(cwd)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim_end().to_owned()
}

/// A repo with one commit and a file the stub agent will edit.
fn repo(home: &Path) -> PathBuf {
    isolate_git_config();
    let repo = home.join("my-app");
    fs::create_dir_all(repo.join("src")).unwrap();
    fs::write(repo.join("src/auth.ts"), "export const a = 1;\n").unwrap();
    git(&repo, &["init", "-q", "."]);
    git(&repo, &["config", "user.name", "u"]);
    git(&repo, &["config", "user.email", "u@e"]);
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "base"]);
    repo
}

/// A stub `claude`: emits lines, and performs the side effects a real agent
/// would, so the driver sees a worktree that actually changed.
fn stub_agent(home: &Path, script: &str) -> PathBuf {
    let p = home.join("fake-claude");
    fs::write(&p, script).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
    }
    p
}

fn spec(repo: &Path, binary: &Path, run: &RunId) -> RunSpec {
    let mut launch = LaunchSpec::new(repo, driver::new_session_id(), Permission::DenyPrompts);
    launch.binary = binary.to_path_buf();
    launch.thinking = Thinking::Off;
    RunSpec {
        run_id: run.clone(),
        repo: repo.to_path_buf(),
        branch: format!("run/{}", run.0),
        base: "HEAD".to_owned(),
        prompt: "add retry to refresh()".to_owned(),
        post_create_hook: None,
        launch,
        workspace_mcp: None,
    }
}

/// A stub that edits a file, then reports the edit — the ordering a real agent
/// produces, and the one the snapshot rule depends on.
const EDIT_THEN_REPORT: &str = r#"#!/usr/bin/env bash
cat > /dev/null &          # drain stdin; we do not need the prompt
emit() { printf '%s\n' "$1"; }
emit '{"type":"system","subtype":"init","uuid":"u0","session_id":"s","cwd":"'"$PWD"'","tools":["Edit"],"mcp_servers":[],"model":"claude-sonnet-5","permissionMode":"default"}'
emit '{"type":"assistant","uuid":"u1","message":{"id":"m1","content":[{"type":"tool_use","id":"toolu_01","name":"Edit","input":{"file_path":"src/auth.ts"}}]}}'
printf 'export const a = 2; // edited by the agent\n' > src/auth.ts
emit '{"type":"user","uuid":"u2","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_01","content":"ok"}]}}'
emit '{"type":"assistant","uuid":"u3","message":{"id":"m2","content":[{"type":"text","text":"done"}]}}'
emit '{"type":"result","uuid":"u4","subtype":"success","result_index":0,"is_error":false,"num_turns":1,"duration_ms":10,"total_cost_usd":0.01,"modelUsage":{"claude-sonnet-5":{"inputTokens":10,"outputTokens":5,"costUSD":0.01}},"permission_denials":[]}'
"#;

// -------------------------------------------------------------------- tests --

/// §5's launch shape, exactly. Three of these flags exist only to defuse a
/// silent failure, so a diff here is worth reading carefully.
#[test]
fn argv_is_the_verified_launch_shape() {
    let mut s = LaunchSpec::new(
        "/wt",
        agent_protocol::SessionId::from("uuid-1"),
        Permission::DenyPrompts,
    );
    s.thinking = Thinking::Summarized;
    let a = s.argv();

    assert_eq!(a.last().unwrap(), "-p", "-p comes last and takes no value");
    let joined = a.join(" ");
    for required in [
        "--output-format stream-json",
        // Hard-enforced: the CLI refuses stream-json without it.
        "--verbose",
        // Turns a one-shot into a live session.
        "--input-format stream-json",
        "--replay-user-messages",
        // Keeps the user's own MCP servers out of a run we describe.
        "--strict-mcp-config",
        // Assigned, not captured (§14).
        "--session-id uuid-1",
        // Or thinking blocks arrive empty (§5).
        "--thinking-display summarized",
        // Nothing may prompt, and we do not pretend otherwise.
        "--permission-prompts none",
    ] {
        assert!(
            joined.contains(required),
            "argv is missing `{required}`: {joined}"
        );
    }
    assert!(
        !joined.contains("--include-partial-messages"),
        "partial messages are off by default: they deliver the same text twice"
    );

    // An appended system prompt must disable the snapshot, or a later change to
    // it is silently ignored on resume -- and §6's premise is that text.
    let mut s2 = s.clone();
    s2.append_system_prompt = Some("the workspace tools exist".to_owned());
    assert!(s2.argv().join(" ").contains("--system-prompt-snapshot off"));
}

/// The whole spine: worktree, shadow root, run row, spawn, stream, log,
/// snapshot, attribution.
#[test]
fn a_run_produces_a_log_and_an_attributable_diff() {
    let home = TempDir::new("spine");
    let repo = repo(home.path());
    let bin = stub_agent(home.path(), EDIT_THEN_REPORT);
    let run = RunId::from("run_1842");

    let log = EventLog::open(home.path().join("log.sqlite")).unwrap();
    let started = driver::start(
        log,
        spec(&repo, &bin, &run),
        "2026-09-13T09:00:00.000Z",
        None,
    )
    .unwrap();
    let wt_path = started.worktree.path().to_path_buf();
    let (outcome, log) = started.session.wait().unwrap();

    assert_eq!(outcome.exit_code, Some(0));
    assert!(outcome.stored > 0, "nothing reached the log");
    assert_eq!(outcome.duplicates, 0, "a fresh run replays nothing");
    assert_eq!(outcome.snapshots, 1, "one tool call changed the worktree");

    // The log holds the run.
    let events = log.events(&run).unwrap();
    let types: Vec<&str> = events.iter().map(|e| e.type_name()).collect();
    assert!(types.contains(&"run.started"));
    assert!(types.contains(&"tool.started"));
    assert!(types.contains(&"tool.completed"));
    assert!(types.contains(&"turn.finished"));
    assert!(
        types.contains(&"run.finished"),
        "only the process ends a run"
    );

    // §14's accounting, off the wire and through storage.
    let m = log.metrics(&run).unwrap();
    assert_eq!(m.turns, 1);
    assert_eq!(m.tool_calls, 1);
    assert!((m.cost_usd - 0.01).abs() < 1e-9);
    assert_eq!(m.input_tokens, 10);

    // §13.1: the line the agent wrote is attributable to the tool call that
    // wrote it -- by seq, back into the log.
    let completed_seq = events
        .iter()
        .find(|e| matches!(e.event, Event::ToolCompleted(_)))
        .map(|e| e.seq)
        .expect("a completion");
    let wt = Worktree::open(&wt_path).unwrap();
    let shadow = ShadowHistory::open(&wt, &run).unwrap();
    let attr = shadow.attribute("src/auth.ts").unwrap();
    let line = attr
        .iter()
        .find(|a| a.text.contains("edited by the agent"))
        .expect("the edited line");
    assert_eq!(line.seq, completed_seq);
    assert_eq!(line.tool, "Edit");
    assert_eq!(line.tool_use_id.as_deref(), Some("toolu_01"));

    // And the run row knows where to find all of it.
    let row = log.run(&run).unwrap().unwrap();
    assert_eq!(row.shadow_ref.as_deref(), Some(shadow.ref_name()));
    assert!(
        row.provider_session_id.is_some(),
        "assigned before launch (§14)"
    );
    assert_eq!(
        row.status,
        RunStatus::Running,
        "the driver does not close the row"
    );
}

/// `run.finished` is produced when the process exits, not by a line of stdout.
/// A live view that subscribes to the event stream must still receive it — the
/// first real run against Claude Code showed every run looking unfinished
/// because these tail events went to the log and nowhere else.
#[test]
fn the_live_view_sees_the_events_the_process_exit_produces() {
    let home = TempDir::new("sink");
    let repo = repo(home.path());
    let bin = stub_agent(home.path(), EDIT_THEN_REPORT);
    let run = RunId::from("run_sink");

    let (tx, rx) = std::sync::mpsc::channel();
    let log = EventLog::open(home.path().join("log.sqlite")).unwrap();
    let started = driver::start(
        log,
        spec(&repo, &bin, &run),
        "2026-09-13T09:00:00.000Z",
        Some(tx),
    )
    .unwrap();

    let seen: Vec<String> = rx.iter().map(|e| e.type_name().to_owned()).collect();
    let (_, log) = started.session.wait().unwrap();

    assert!(
        seen.iter().any(|t| t == "run.finished"),
        "the subscriber never saw the run end: {seen:?}"
    );
    let stored: Vec<String> = log
        .events(&run)
        .unwrap()
        .iter()
        .map(|e| e.type_name().to_owned())
        .collect();
    assert_eq!(seen, stored, "the live view and the log disagree");
}

/// §13's post-create hook runs before the agent and writes files. They must not
/// land on the agent's first tool call.
#[test]
fn post_create_hook_output_is_attributed_to_the_run_not_to_a_tool_call() {
    let home = TempDir::new("hook");
    let repo = repo(home.path());
    let bin = stub_agent(home.path(), EDIT_THEN_REPORT);
    let run = RunId::from("run_hook");

    let log = EventLog::open(home.path().join("log.sqlite")).unwrap();
    let mut s = spec(&repo, &bin, &run);
    s.post_create_hook = Some(vec![
        "sh".to_owned(),
        "-c".to_owned(),
        "echo 'installed by the hook' > vendor.lock".to_owned(),
    ]);
    let started = driver::start(log, s, "2026-09-13T09:00:00.000Z", None).unwrap();
    let wt_path = started.worktree.path().to_path_buf();
    let (_, _log) = started.session.wait().unwrap();

    let wt = Worktree::open(&wt_path).unwrap();
    let shadow = ShadowHistory::open(&wt, &run).unwrap();
    let attr = shadow.attribute("vendor.lock").unwrap();
    assert_eq!(
        attr[0].seq,
        driver::SEQ_BEFORE_RUN,
        "hook output was attributed to a tool call"
    );
    assert_eq!(attr[0].tool, "-");
}

/// A failing hook must not leave a half-started run behind.
#[test]
fn a_failing_post_create_hook_stops_the_run() {
    let home = TempDir::new("badhook");
    let repo = repo(home.path());
    let bin = stub_agent(home.path(), EDIT_THEN_REPORT);
    let run = RunId::from("run_bad");

    let log = EventLog::open(home.path().join("log.sqlite")).unwrap();
    let mut s = spec(&repo, &bin, &run);
    s.post_create_hook = Some(vec!["sh".to_owned(), "-c".to_owned(), "exit 3".to_owned()]);
    let err = match driver::start(
        EventLog::open(home.path().join("log.sqlite")).unwrap(),
        s,
        "2026-09-13T09:00:00.000Z",
        None,
    ) {
        Ok(_) => panic!("a failing hook started a run"),
        Err(e) => e,
    };
    assert!(matches!(
        err,
        driver::Error::PostCreateHook {
            status: Some(3),
            ..
        }
    ));
    assert!(
        log.run(&run).unwrap().is_none(),
        "a run row was created for a run that never started"
    );
    assert!(log.running_runs().unwrap().is_empty());
}

/// Every recorded stream, through the real pipeline. No process, no worktree —
/// this is the §16 fixture guarantee extended over storage and the driver.
#[test]
fn every_recorded_fixture_survives_the_pipeline() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/claude-code/2.1.269");
    let mut seen = 0;
    for entry in fs::read_dir(&dir).expect("fixtures") {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|e| e != "ndjson") {
            continue;
        }
        let name = path.file_stem().unwrap().to_string_lossy().into_owned();
        let run = RunId::from(name.as_str());
        let mut log = EventLog::open_in_memory().unwrap();
        log.create_run(&event_log::Run {
            id: run.clone(),
            agent: "claude-code".to_owned(),
            agent_version: None,
            provider_session_id: None,
            workspace_path: "/tmp".to_owned(),
            worktree_path: None,
            branch: None,
            shadow_ref: None,
            prompt: None,
            status: RunStatus::Running,
            started_at: "2026-09-13T09:00:00.000Z".to_owned(),
            finished_at: None,
            parent_run_id: None,
            branch_point_seq: None,
        })
        .unwrap();

        // No shadow history: this asserts the pipeline is usable without a
        // worktree, which is what a tier-3 or read-only run needs.
        let mut p = Pipeline::new(run.clone(), log, None);
        for line in fs::read_to_string(&path).unwrap().lines() {
            p.ingest(line).unwrap_or_else(|e| panic!("{name}: {e}"));
        }
        p.finish(Some(0)).unwrap();

        assert!(p.stats().balances(), "{name}: a line went unaccounted for");
        let log = p.into_log();
        assert!(!log.events(&run).unwrap().is_empty(), "{name}: empty log");
        seen += 1;
    }
    assert!(seen >= 13, "expected every fixture, saw {seen}");
}

/// §8's crash recovery. Every run the log still calls running after a restart
/// is one whose process died with the app.
#[test]
fn a_restart_reconciles_runs_whose_process_is_gone() {
    let home = TempDir::new("reconcile");
    let repo = repo(home.path());
    let bin = stub_agent(home.path(), EDIT_THEN_REPORT);
    let run = RunId::from("run_crash");
    let db = home.path().join("log.sqlite");

    let started = driver::start(
        EventLog::open(&db).unwrap(),
        spec(&repo, &bin, &run),
        "2026-09-13T09:00:00.000Z",
        None,
    )
    .unwrap();
    started.session.wait().unwrap(); // the app exits without closing the row

    let mut log = EventLog::open(&db).unwrap();
    assert_eq!(log.running_runs().unwrap().len(), 1);
    let fixed = driver::reconcile_after_restart(&mut log, "2026-09-13T10:00:00.000Z").unwrap();
    assert_eq!(fixed, vec![run.clone()]);
    assert_eq!(log.run(&run).unwrap().unwrap().status, RunStatus::Abandoned);
    assert!(
        !log.events(&run).unwrap().is_empty(),
        "an abandoned run keeps its history; nothing is ever deleted"
    );
    assert!(driver::reconcile_after_restart(&mut log, "x")
        .unwrap()
        .is_empty());
}

/// A resumed run continues the log rather than colliding with it.
#[test]
fn resuming_continues_the_sequence_and_deduplicates_the_replay() {
    let home = TempDir::new("resume");
    let repo = repo(home.path());
    let bin = stub_agent(home.path(), EDIT_THEN_REPORT);
    let run = RunId::from("run_resume");
    let db = home.path().join("log.sqlite");

    let started = driver::start(
        EventLog::open(&db).unwrap(),
        spec(&repo, &bin, &run),
        "2026-09-13T09:00:00.000Z",
        None,
    )
    .unwrap();
    let (_, log) = started.session.wait().unwrap();
    let first_pass = log.events(&run).unwrap();
    let last_seq = log.last_seq(&run).unwrap();
    assert!(last_seq > 0);

    // The same stream again, as a resume would replay it.
    assert!(
        first_pass.iter().any(|e| e.idempotency_key.is_some()),
        "dedupe needs keys; without them a replay would double the run"
    );
    let mut p = Pipeline::resuming(run.clone(), log, None).unwrap();
    for line in EDIT_THEN_REPORT_LINES {
        let ing = p.ingest(line).unwrap();
        assert_eq!(ing.stored, 0, "a replayed line must store nothing again");
        assert!(ing.duplicates > 0);
        assert!(ing.snapshot.is_none(), "a replay must not re-snapshot");
    }
    let log = p.into_log();
    assert_eq!(
        log.events(&run).unwrap().len(),
        first_pass.len(),
        "the replay duplicated the run"
    );
}

/// The exact stdout of the stub, for the resume test to replay.
const EDIT_THEN_REPORT_LINES: &[&str] = &[
    r#"{"type":"assistant","uuid":"u1","message":{"id":"m1","content":[{"type":"tool_use","id":"toolu_01","name":"Edit","input":{"file_path":"src/auth.ts"}}]}}"#,
    r#"{"type":"user","uuid":"u2","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_01","content":"ok"}]}}"#,
    r#"{"type":"assistant","uuid":"u3","message":{"id":"m2","content":[{"type":"text","text":"done"}]}}"#,
];

// ------------------------------------------------ the permission channel --

fn in_memory_run(run: &RunId) -> EventLog {
    let mut log = EventLog::open_in_memory().unwrap();
    log.create_run(&event_log::Run {
        id: run.clone(),
        agent: "claude-code".to_owned(),
        agent_version: None,
        provider_session_id: None,
        workspace_path: "/tmp".to_owned(),
        worktree_path: None,
        branch: None,
        shadow_ref: None,
        prompt: None,
        status: RunStatus::Running,
        started_at: "2026-09-13T09:00:00.000Z".to_owned(),
        finished_at: None,
        parent_run_id: None,
        branch_point_seq: None,
    })
    .unwrap();
    log
}

struct Answer(workspace_mcp::Decision);

impl driver::Decider for Answer {
    fn decide(&self, _r: &workspace_mcp::PermissionRequest) -> workspace_mcp::Decision {
        match &self.0 {
            workspace_mcp::Decision::Allow { updated_input } => workspace_mcp::Decision::Allow {
                updated_input: updated_input.clone(),
            },
            workspace_mcp::Decision::Deny { message, interrupt } => workspace_mcp::Decision::Deny {
                message: message.clone(),
                interrupt: *interrupt,
            },
        }
    }
}

/// The permission round trip, without a process.
///
/// This is the class of event no recorded stream contains and no parser can
/// produce: the provider calls our tool, so the request exists only on the
/// runtime's own channel (§12 option A, §15). What it has to prove is that the
/// channel both *answers* and *records* — a decision that reaches the agent but
/// not the log would be a grant nobody can audit.
#[test]
fn a_permission_request_is_answered_and_recorded() {
    use std::sync::{Arc, Mutex};
    use workspace_mcp::Workspace;

    let run = RunId::from("run_perm");
    let pipeline = Arc::new(Mutex::new(Pipeline::new(
        run.clone(),
        in_memory_run(&run),
        None,
    )));
    let (tx, rx) = std::sync::mpsc::channel();

    let ws = driver::RunWorkspace::new(
        Arc::clone(&pipeline),
        Some(tx),
        Arc::new(Answer(workspace_mcp::Decision::Deny {
            message: "not in this worktree".to_owned(),
            interrupt: false,
        })),
    );

    let decision = ws.request_permission(workspace_mcp::PermissionRequest {
        tool_name: "Write".to_owned(),
        input: serde_json::json!({"file_path": "/etc/passwd", "content": "x"}),
        tool_use_id: Some("toolu_1".to_owned()),
    });

    match decision {
        workspace_mcp::Decision::Deny { message, .. } => {
            assert_eq!(message, "not in this worktree")
        }
        other => panic!("expected the decider's answer, got {other:?}"),
    }

    let stored = pipeline.lock().unwrap().log().events(&run).unwrap();
    let kinds: Vec<&str> = stored.iter().map(|e| e.type_name()).collect();
    assert_eq!(
        kinds,
        ["permission.requested", "permission.resolved"],
        "the question is recorded before the answer, so a crash mid-decision \
         still says what was asked"
    );
    assert_eq!(stored[0].seq, 1);
    assert_eq!(stored[1].seq, 2);

    let Event::PermissionResolved(r) = &stored[1].event else {
        panic!("expected permission.resolved")
    };
    assert!(!r.allowed);
    assert_eq!(r.reason.as_deref(), Some("not in this worktree"));
    assert_eq!(
        r.reason_type.as_deref(),
        Some("workspace"),
        "our answer must be distinguishable from the provider's own refusals"
    );

    // §8's trap: an event that reaches the log but not the live view leaves the
    // UI showing a run that is waiting on nothing.
    let seen: Vec<String> = rx.try_iter().map(|e| e.type_name().to_owned()).collect();
    assert_eq!(seen, ["permission.requested", "permission.resolved"]);
}

/// A run that cannot be asked must not claim it can.
///
/// `--strict-mcp-config` means the only servers loaded are ours, so an
/// `AskVia` naming a tool on a server we never injected makes every permission
/// request fail — and the provider reports that as the agent being unable to do
/// anything, not as a bad flag.
#[test]
fn a_permission_tool_without_its_server_is_refused_before_spawning() {
    let mut s = LaunchSpec::new(
        "/wt",
        agent_protocol::SessionId::from("uuid-1"),
        Permission::AskVia {
            tool: workspace_mcp::qualified(workspace_mcp::REQUEST_PERMISSION),
        },
    );
    assert!(s.validate().is_err(), "no mcp_config: cannot resolve");

    s.mcp_config = Some("{\"mcpServers\":{}}".to_owned());
    assert!(s.validate().is_ok());

    s.permission = Permission::AskVia {
        tool: "request_permission".to_owned(),
    };
    assert!(
        s.validate().is_err(),
        "a bare tool name parses to nothing: the CLI needs mcp__<server>__<tool>"
    );
}
