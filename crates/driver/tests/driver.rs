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
use event_log::{EventLog, LaunchFlags, Run, RunStatus};
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
        // Keeps the user's own MCP servers out of a run we describe -- every
        // scope of them, and nothing else of theirs. See the assertion below.
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

    // A run that starts fresh continues nothing and branches nothing. Worth an
    // assertion rather than an eyeball: `--fork-session` on a plain start would
    // silently hand the run a session id we did not assign (§14).
    for absent in ["--resume", "--fork-session", "--resume-session-at"] {
        assert!(
            !joined.contains(absent),
            "a normal start must not carry `{absent}`: {joined}"
        );
    }

    // The other half of `--strict-mcp-config`, pinned so that the asymmetry is
    // a decision rather than an oversight. MCP servers are the *only* thing
    // §5's "What users keep" list gives up; hooks, both `CLAUDE.md`s, skills,
    // subagents, commands, plugins, the output style and the user's own
    // `permissions` block all reach the run, verified against 2.1.270 in
    // `docs/verification/claude-code-user-config-inheritance.md`. These three
    // flags are what would shut them out, and `--bare` would take the user's
    // OAuth credentials with them. If one of them ever belongs here it belongs
    // here on an argument, in a diff someone reads.
    for absent in ["--setting-sources", "--restricted", "--bare"] {
        assert!(
            !joined.contains(absent),
            "a run inherits the user's configuration except MCP: `{absent}`: {joined}"
        );
    }
}

/// §19's fork, as argv. Three flags plus the one that was always there.
#[test]
fn forking_argv_names_the_child_and_the_branch_point() {
    let mut s = LaunchSpec::new(
        "/wt",
        agent_protocol::SessionId::from("child-session"),
        Permission::DenyPrompts,
    );
    s.resume = Some(agent_protocol::SessionId::from("parent-session"));
    s.fork_at = Some("93e6a054-0e51-4425-a90b-b46f7e28dc1c".to_owned());
    s.validate().unwrap();
    let joined = s.argv().join(" ");

    for required in [
        // The parent conversation. `--fork-session` is documented "use with
        // --resume", and without it there is nothing to branch.
        "--resume parent-session",
        // A new session id instead of reusing the parent's...
        "--fork-session",
        // ...except that we assign it, so the child is addressable before the
        // process speaks (§14). This is the flag that was already here.
        "--session-id child-session",
        // The branch point: a bare chain-entry uuid, off our own
        // `raw_events.idempotency_key`.
        "--resume-session-at 93e6a054-0e51-4425-a90b-b46f7e28dc1c",
    ] {
        assert!(
            joined.contains(required),
            "a forked argv is missing `{required}`: {joined}"
        );
    }

    // Verified refused by name, so refuse it here rather than spend a live run
    // on it: `No message found with message.uuid of: <uuid>#0`.
    let mut suffixed = s.clone();
    suffixed.fork_at = Some("93e6a054-0e51-4425-a90b-b46f7e28dc1c#0".to_owned());
    assert!(
        suffixed.validate().is_err(),
        "the `#i` fan-out suffix is ours; the CLI refuses it"
    );

    // A branch point with nothing to branch.
    let mut orphan = s.clone();
    orphan.resume = None;
    assert!(
        orphan.validate().is_err(),
        "--resume-session-at needs --resume"
    );
}

/// The two resuming shapes, pinned against each other: a fork keeps
/// `--session-id`, a plain resume must not carry it.
///
/// Bought live rather than reasoned about, because the code shipped the wrong
/// one — `--session-id X --resume X`, which the provider refuses outright with
/// "--session-id can only be used with --continue or --resume if
/// --fork-session is also specified" (exit 1, empty stdout, no `result` line to
/// read a subtype off). Every resumed run was dead on arrival
/// (`claude-code-resume-session-id.md`). The fork half is here so the fix
/// cannot be generalised into "drop it whenever resuming": that combination is
/// verified to work and is how §19 names a child.
#[test]
fn a_plain_resume_drops_the_session_id_a_fork_keeps_it() {
    let sid = agent_protocol::SessionId::from("uuid-1");
    let mut resume = LaunchSpec::new("/wt", sid.clone(), Permission::DenyPrompts);
    resume.resume = Some(sid.clone());
    resume.validate().unwrap();
    let joined = resume.argv().join(" ");

    assert!(
        joined.contains("--resume uuid-1"),
        "a resume names the session it continues: {joined}"
    );
    assert!(
        !joined.contains("--session-id"),
        "the provider refuses --session-id alongside --resume without \
         --fork-session, and the id is the resumed session's own anyway: {joined}"
    );
    assert!(
        !joined.contains("--fork-session"),
        "continuing a conversation is not branching it: {joined}"
    );

    // The suppression is only lossless while the dropped id is the one being
    // resumed. Renaming a session on resume is not something the provider
    // offers, so it is a refused launch rather than a silently different run.
    let mut renaming = resume.clone();
    renaming.session_id = agent_protocol::SessionId::from("uuid-2");
    assert!(
        renaming.validate().is_err(),
        "a non-forking resume cannot give the session a new id"
    );

    // A fork resumes too, and keeps the flag: we name the child (§19).
    let mut forked = renaming.clone();
    forked.fork_at = Some("93e6a054-0e51-4425-a90b-b46f7e28dc1c".to_owned());
    forked.validate().unwrap();
    let joined = forked.argv().join(" ");
    for required in [
        "--session-id uuid-2",
        "--resume uuid-1",
        "--fork-session",
        "--resume-session-at 93e6a054-0e51-4425-a90b-b46f7e28dc1c",
    ] {
        assert!(
            joined.contains(required),
            "a fork is the resuming shape that keeps `{required}`: {joined}"
        );
    }
}

/// The `{uuid}#{i}` fan-out is ours, and an unkeyed block has no address at all.
///
/// Both halves were bought live (`claude-code-fork-address.md`): the suffixed
/// form is refused by name, and the user's own chain entries never reach stdout
/// so nothing keys them. The second is why this is a fallible lookup and not an
/// accessor.
#[test]
fn an_address_is_the_part_before_the_hash_and_may_not_exist() {
    use normalizer::claude_code::fork_address;

    let bare = "93e6a054-0e51-4425-a90b-b46f7e28dc1c";
    assert_eq!(fork_address("assistant.message", Some(bare)), Some(bare));
    assert_eq!(
        fork_address("assistant.message", Some(&format!("{bare}#0"))),
        Some(bare),
        "one provider line fanned into several events is still one chain entry"
    );
    assert_eq!(
        fork_address("assistant.message", Some(&format!("{bare}#11"))),
        Some(bare)
    );

    // No key means no address, and nothing else. Not the run's first block, not
    // the nearest keyed neighbour: `branching-surface.md` rejected snapping.
    assert_eq!(fork_address("assistant.message", None), None);
    assert_eq!(fork_address("assistant.message", Some("")), None);
    assert_eq!(fork_address("assistant.message", Some("#0")), None);
}

/// A key is necessary and **not sufficient**: the event type decides.
///
/// Bought live in `claude-code-fork-anchor-types.md`, which anchored a fork at
/// one uuid of every `type`/`subtype` combination the provider emits. Only the
/// events that come off an `assistant` or `user` line resolve; a
/// `system`/`hook_started` line carries a uuid, is therefore keyed, and is
/// refused by the provider — which is exactly what the built surface tripped
/// over before this rule existed.
#[test]
fn a_key_is_not_an_address_unless_the_event_came_off_a_message_line() {
    use normalizer::claude_code::{fork_address, is_addressable};

    let bare = "93e6a054-0e51-4425-a90b-b46f7e28dc1c";

    // Accepted, live, one anchor each. `tool.started` and `tool.completed` are
    // the two halves of a tool call and are the case the address spike left
    // explicitly unproven.
    for ty in [
        "assistant.message",
        "assistant.thinking",
        "tool.started",
        "workspace.report",
        "user.message",
        "tool.completed",
    ] {
        assert!(is_addressable(ty), "{ty} is a chain entry and resolves");
        assert_eq!(fork_address(ty, Some(bare)), Some(bare), "{ty}");
    }

    // Refused, live, by name — and every one of them is keyed, which is why
    // stripping the `#` was never enough on its own.
    for ty in [
        "run.started",         // system/init
        "turn.started",        // system/init, on a later turn
        "turn.finished",       // result/success and result/error_max_turns alike
        "run.rate_limited",    // rate_limit_event
        "permission.resolved", // system/permission_denied
        "raw.unclassified",    // system/hook_started, hook_response, status
    ] {
        assert!(
            !is_addressable(ty),
            "{ty} is keyed but the provider refuses it"
        );
        assert_eq!(
            fork_address(ty, Some(bare)),
            None,
            "{ty} must not offer an address it cannot honour"
        );
    }

    // Never on stdout, so never keyed — stated rather than left to fall out.
    for ty in [
        "permission.requested",
        "run.finished",
        "run.failed",
        "process.output",
    ] {
        assert!(!is_addressable(ty));
    }

    // A vocabulary we have not met fails closed.
    assert!(!is_addressable("something.new"));
    assert_eq!(fork_address("something.new", Some(bare)), None);
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
    let wt_path = started.tree.worktree.path().to_path_buf();
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
    let wt_path = started.tree.worktree.path().to_path_buf();
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
            model: None,
            launch_flags: LaunchFlags::default(),
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

// ------------------------------------------------------------------- forks --

/// A parent worth forking: it edits one file **twice**, so its shadow history
/// has two snapshots and the tree at a branch point between them differs from
/// the tree at its tip. That difference is what a fork-at-`seq` has to get
/// right, and it is invisible in a parent that edits once.
///
/// `u1` is also one provider line carrying text *and* a tool call, so it fans
/// out into two events keyed `u1#0` and `u1#1` — the suffix is ours, and the CLI
/// refuses it by name.
const PARENT_TWO_EDITS: &[&str] = &[
    r#"{"type":"system","subtype":"init","uuid":"u0","session_id":"s","cwd":"/wt","tools":["Edit"],"mcp_servers":[],"model":"claude-sonnet-5","permissionMode":"default"}"#,
    r#"{"type":"assistant","uuid":"u1","message":{"id":"m1","content":[{"type":"text","text":"first pass"},{"type":"tool_use","id":"toolu_01","name":"Edit","input":{"file_path":"src/auth.ts"}}]}}"#,
    // -- the first edit lands here --
    r#"{"type":"user","uuid":"u2","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_01","content":"ok"}]}}"#,
    r#"{"type":"assistant","uuid":"u3","message":{"id":"m2","content":[{"type":"text","text":"now the second pass"}]}}"#,
    r#"{"type":"assistant","uuid":"u4","message":{"id":"m3","content":[{"type":"tool_use","id":"toolu_02","name":"Edit","input":{"file_path":"src/auth.ts"}}]}}"#,
    // -- the second edit lands here --
    r#"{"type":"user","uuid":"u5","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_02","content":"ok"}]}}"#,
    r#"{"type":"assistant","uuid":"u6","message":{"id":"m4","content":[{"type":"text","text":"done"}]}}"#,
];

/// A forked child. Writes its own argv and cwd out so the flags and the working
/// directory the driver chose are assertions rather than inspections, then
/// **appends** to the file its parent was editing — a fork's writes are
/// snapshotted and attributable like any other run's, and nothing but a real
/// edit proves that. Appending rather than overwriting leaves the inherited
/// lines in place, so blame has both to tell apart.
const FORK_CHILD_EDITS: &str = r#"#!/usr/bin/env bash
{ printf '%s\n' "$PWD"; printf '%s\n' "$@"; } > __DUMP__
cat > /dev/null &
emit() { printf '%s\n' "$1"; }
emit '{"type":"system","subtype":"init","uuid":"c0","session_id":"s","cwd":"'"$PWD"'","tools":["Edit"],"mcp_servers":[],"model":"claude-sonnet-5","permissionMode":"default"}'
emit '{"type":"assistant","uuid":"c1","message":{"id":"m1","content":[{"type":"tool_use","id":"toolu_09","name":"Edit","input":{"file_path":"src/auth.ts"}}]}}'
printf 'export const b = 2; // the child\n' >> src/auth.ts
emit '{"type":"user","uuid":"c2","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_09","content":"ok"}]}}'
emit '{"type":"assistant","uuid":"c3","message":{"id":"m2","content":[{"type":"text","text":"forked and edited"}]}}'
emit '{"type":"result","uuid":"c4","subtype":"success","result_index":0,"is_error":false,"num_turns":1,"duration_ms":10,"total_cost_usd":0.01,"modelUsage":{"claude-sonnet-5":{"inputTokens":10,"outputTokens":5,"costUSD":0.01}},"permission_denials":[]}'
"#;

/// Build a parent run that edited a file twice: its tree, its shadow history,
/// its row and its log — with the worktree mutated between lines exactly where
/// the agent would have mutated it.
///
/// Driven through [`Pipeline`] rather than by spawning a stub, and the reason is
/// the thing under test. A snapshot is taken when the *driver* reaches a line,
/// not when the agent emits it, so a stub's writes race its own stdout: both
/// edits can land in one snapshot, and there is no point in the stream a test
/// could wait on to stop it. `Pipeline` is the same code path per line with the
/// process — and therefore the timing — taken out. The fork below spawns a real
/// one.
fn parent_that_edited_twice(db: &Path, repo: &Path, run: &RunId) -> (PathBuf, EventLog) {
    let tree = driver::ensure_tree(&driver::TreeSpec {
        run_id: run.clone(),
        repo: repo.to_path_buf(),
        branch: format!("agent/{}", run.0),
        base: "HEAD".to_owned(),
        post_create_hook: None,
    })
    .unwrap();
    let wt = tree.worktree.path().to_path_buf();

    let mut log = EventLog::open(db).unwrap();
    log.create_run(&Run {
        id: run.clone(),
        agent: "claude-code".to_owned(),
        agent_version: None,
        provider_session_id: Some(agent_protocol::SessionId::from("parent-session")),
        workspace_path: repo.display().to_string(),
        worktree_path: Some(wt.display().to_string()),
        branch: Some(format!("agent/{}", run.0)),
        shadow_ref: Some(tree.shadow.ref_name().to_owned()),
        prompt: Some("make it retry".to_owned()),
        // The fork below inherits both of these, and asserts it.
        model: Some("claude-opus-4-6".to_owned()),
        launch_flags: LaunchFlags {
            allowed_tools: vec!["Read".to_owned(), "Edit".to_owned()],
            max_turns: Some(12),
            ..LaunchFlags::default()
        },
        status: RunStatus::Finished,
        started_at: "2026-09-13T09:00:00.000Z".to_owned(),
        finished_at: Some("2026-09-13T09:01:00.000Z".to_owned()),
        parent_run_id: None,
        branch_point_seq: None,
    })
    .unwrap();

    let mut pipeline = Pipeline::new(run.clone(), log, Some(tree.shadow));
    let mut snapshots = 0;
    for (i, line) in PARENT_TWO_EDITS.iter().enumerate() {
        // The agent writes, *then* reports. The snapshot rule depends on that
        // order and so does this test.
        if i == 2 {
            fs::write(wt.join("src/auth.ts"), "export const a = 1; // FIRST\n").unwrap();
        }
        if i == 5 {
            fs::write(wt.join("src/auth.ts"), "export const a = 2; // SECOND\n").unwrap();
            fs::write(wt.join("src/late.ts"), "export const late = true;\n").unwrap();
        }
        if pipeline.ingest(line).unwrap().snapshot.is_some() {
            snapshots += 1;
        }
    }
    assert_eq!(snapshots, 2, "two edits, two snapshots");

    (wt, pipeline.into_log())
}

/// The `seq` of the first `tool.completed` in a run's log — the event a snapshot
/// is stamped with.
fn first_completion_seq(log: &EventLog, run: &RunId) -> u64 {
    log.events(run)
        .unwrap()
        .iter()
        .find(|e| matches!(e.event, Event::ToolCompleted(_)))
        .expect("a completed tool call")
        .seq
}

/// The `seq` of the event carrying a given provider uuid.
fn seq_of(log: &EventLog, run: &RunId, key: &str) -> u64 {
    log.events(run)
        .unwrap()
        .iter()
        .find(|e| e.idempotency_key.as_deref() == Some(key))
        .unwrap_or_else(|| panic!("no event keyed {key}"))
        .seq
}

/// §19's pairing, which nothing had exercised: **the conversation fork and the
/// worktree fork are taken at the same `seq`**.
///
/// The parent edits `src/auth.ts` twice and creates a second file on the way.
/// The fork is anchored between the two edits, so three trees are in play and
/// only one of them is right: the parent's tree *now* (`SECOND`, plus
/// `src/late.ts`), the repository's `HEAD` (neither edit), and the parent's tree
/// **at the branch point** (`FIRST`, no `src/late.ts`). The lazy design this
/// replaced would have handed the child the first of those.
#[test]
fn a_fork_gets_its_own_tree_holding_the_parent_as_of_the_branch_point() {
    let home = TempDir::new("fork");
    let repo = repo(home.path()).canonicalize().unwrap();
    let parent = RunId::from("run_parent");
    let db = home.path().join("log.sqlite");
    let (parent_wt, log) = parent_that_edited_twice(&db, &repo, &parent);

    // The branch point: `u3`, the assistant message *between* the two edits.
    // Addressable, because it reached stdout under a bare uuid.
    let at = driver::ForkPoint::resolve(&log, &parent, seq_of(&log, &parent, "u3")).unwrap();
    assert_eq!(at.address, "u3");
    assert!(
        at.seq > first_completion_seq(&log, &parent),
        "the branch point must be after the first edit or this proves nothing"
    );
    assert_eq!(
        driver::ForkPoint::resolve(&log, &parent, seq_of(&log, &parent, "u1#0"))
            .unwrap()
            .address,
        "u1",
        "the address is the part before the '#'"
    );

    // The parent's tree, as it stands now. The child must not get this.
    assert!(fs::read_to_string(parent_wt.join("src/auth.ts"))
        .unwrap()
        .contains("SECOND"));
    assert!(parent_wt.join("src/late.ts").exists());

    let child = RunId::from("run_child");
    let dump = home.path().join("child-argv");
    let child_session = driver::new_session_id();
    let mut launch = LaunchSpec::new(
        // Deliberately nonsense: `fork` owns the cwd and must overwrite this.
        "/definitely/not/a/checkout",
        child_session.clone(),
        Permission::DenyPrompts,
    );
    launch.binary = stub_agent(
        home.path(),
        &FORK_CHILD_EDITS.replace("__DUMP__", &dump.display().to_string()),
    );
    launch.thinking = Thinking::Off;

    let forked = driver::fork(
        EventLog::open(&db).unwrap(),
        driver::ForkSpec {
            run_id: child.clone(),
            at: at.clone(),
            branch: "agent/run_child".to_owned(),
            post_create_hook: None,
            prompt: "try it without the cache".to_owned(),
            launch,
            workspace_mcp: None,
        },
        "2026-09-13T09:05:00.000Z",
        None,
    )
    .unwrap();
    let child_wt = forked.tree.worktree.path().to_path_buf();
    let child_ref = forked.tree.shadow_ref.clone();

    // **The pairing.** The child's checkout is the parent's tree at `seq`, and
    // is neither of the two trees it could have been confused with.
    let auth_at_fork = fs::read_to_string(child_wt.join("src/auth.ts")).unwrap();
    assert!(
        auth_at_fork.contains("FIRST"),
        "the child's tree must hold the parent's first edit: {auth_at_fork:?}"
    );
    assert!(
        !auth_at_fork.contains("SECOND"),
        "the child got the parent's tree at its tip, not at the branch point: \
         {auth_at_fork:?}"
    );
    assert!(
        !child_wt.join("src/late.ts").exists(),
        "a file the parent created after the branch point is in the child's tree"
    );
    assert_ne!(child_wt, parent_wt, "two runs must not share one tree");
    assert_eq!(child_wt, repo.join(".worktrees").join(&child.0));
    assert_eq!(child_ref, format!("refs/agent-workspace/runs/{}", child.0));
    assert_eq!(
        git(&child_wt, &["rev-parse", "--abbrev-ref", "HEAD"]),
        "agent/run_child",
        "a fork gets its own branch: git refuses two worktrees on one"
    );

    let (outcome, log) = forked.session.wait().unwrap();
    assert_eq!(outcome.exit_code, Some(0));
    assert!(outcome.stored > 0, "nothing reached the log");
    assert_eq!(
        outcome.snapshots, 1,
        "a fork has a shadow history of its own, so its edits are recorded"
    );

    // The flags and the working directory, as the child actually received them.
    let dumped = fs::read_to_string(&dump).unwrap();
    let mut dumped = dumped.lines();
    let cwd = Path::new(dumped.next().unwrap()).canonicalize().unwrap();
    let argv: Vec<&str> = dumped.collect();
    let pair = |flag: &str, value: &str| argv.windows(2).any(|w| w[0] == flag && w[1] == value);

    assert_eq!(
        cwd,
        child_wt.canonicalize().unwrap(),
        "a fork works in its own tree (§13), never an ancestor's and never the \
         user's own checkout"
    );
    assert!(argv.contains(&"--fork-session"), "{argv:?}");
    assert!(pair("--resume", "parent-session"), "{argv:?}");
    assert!(pair("--resume-session-at", "u3"), "{argv:?}");
    assert!(
        pair("--session-id", &child_session.0),
        "the child is named by us, not read back off the stream (§14): {argv:?}"
    );
    // **The child runs under the parent's policy.** Not because the caller
    // passed one — the `LaunchSpec` above names no tools at all — but because
    // the parent's row stored what it was launched with (schema V4) and
    // `driver::fork` reads it back. Before that column, a fork's allow-list was
    // whatever the *window* happened to remember launching the parent with, so
    // a branch off a run from a previous session was refused the `Read` its
    // parent had, silently: §12's posture is `DenyPrompts`, and a tool that is
    // not pre-granted is not prompted for, it is denied.
    assert!(pair("--allowed-tools", "Read"), "{argv:?}");
    assert!(
        argv.contains(&"Edit"),
        "the whole allow-list is inherited, not its first entry: {argv:?}"
    );
    assert!(pair("--max-turns", "12"), "{argv:?}");

    // The row: the whole graph edge, and git state of its own.
    let row = log.run(&child).unwrap().unwrap();
    assert_eq!(row.parent_run_id.as_ref(), Some(&parent));
    assert_eq!(
        row.branch_point_seq,
        Some(at.seq),
        "the branch point is stored at the fork or it is lost: the provider \
         records no parentage"
    );
    assert_eq!(
        row.worktree_path.as_deref(),
        Some(child_wt.to_str().unwrap())
    );
    assert_eq!(row.branch.as_deref(), Some("agent/run_child"));
    assert_eq!(row.shadow_ref.as_deref(), Some(child_ref.as_str()));
    assert_eq!(row.provider_session_id.as_ref(), Some(&child_session));
    assert_eq!(row.workspace_path, repo.display().to_string());
    assert_eq!(
        row.model.as_deref(),
        Some("claude-opus-4-6"),
        "`branching-surface.md`: a fork inherits its parent's model, and it is \
         copied onto the child's own row rather than read back through the edge"
    );
    assert_eq!(
        row.launch_flags,
        LaunchFlags {
            allowed_tools: vec!["Read".to_owned(), "Edit".to_owned()],
            max_turns: Some(12),
            ..LaunchFlags::default()
        },
        "and the rest of the launch with it, copied onto the child's own row for \
         the same reason: a fork of this fork inherits what *this* run ran with"
    );

    // §13.1 on the fork: its edit is attributable to the tool call that made it,
    // by `seq`, back into the *child's* log.
    let wt = Worktree::open(&child_wt).unwrap();
    let shadow = ShadowHistory::open(&wt, &child).unwrap();
    let attr = shadow.attribute("src/auth.ts").unwrap();
    let line = attr
        .iter()
        .find(|a| a.text.contains("the child"))
        .expect("the child's edited line");
    assert_eq!(line.seq, first_completion_seq(&log, &child));
    assert_eq!(line.tool_use_id.as_deref(), Some("toolu_09"));
    assert_eq!(
        attr.iter()
            .find(|a| a.text.contains("FIRST"))
            .map(|a| a.seq),
        Some(0),
        "the line the child inherited is attributed to the child's own root — \
         `seq` 0, the tree as this agent found it. The parent's `seq` for it is \
         in the parent's history, which is where Review looks the parent up"
    );

    // The parent is untouched by any of it: its tree and its history.
    assert!(fs::read_to_string(parent_wt.join("src/auth.ts"))
        .unwrap()
        .contains("SECOND"));
    assert_eq!(
        git(
            &parent_wt,
            &[
                "rev-list",
                "--count",
                &format!("refs/agent-workspace/runs/{}", parent.0)
            ]
        ),
        "3",
        "the child's snapshot landed in the parent's history"
    );
}

/// A fork of a fork branches from the **fork's** own history, not from the
/// nearest ancestor that happens to have a tree.
///
/// There is no ancestor walk any more, and this is what removed the need for
/// one: every run has a worktree and a shadow ref, so every run is a parent a
/// fork can be placed against.
#[test]
fn a_fork_of_a_fork_branches_from_the_fork() {
    let home = TempDir::new("fork2");
    let repo = repo(home.path()).canonicalize().unwrap();
    let root = RunId::from("run_root");
    let db = home.path().join("log.sqlite");
    let (root_wt, log) = parent_that_edited_twice(&db, &repo, &root);

    let child = RunId::from("run_child");
    let child_wt = fork_once(
        &db,
        home.path(),
        driver::ForkPoint::resolve(&log, &root, seq_of(&log, &root, "u3")).unwrap(),
        &child,
        "argv-child",
    );

    // The grandchild anchors in the child, after the child's own edit.
    let log = log_of(&db);
    let grandchild = RunId::from("run_grandchild");
    let grandchild_wt = fork_once(
        &db,
        home.path(),
        driver::ForkPoint::resolve(&log, &child, seq_of(&log, &child, "c3")).unwrap(),
        &grandchild,
        "argv-grandchild",
    );

    for (a, b) in [
        (&root_wt, &child_wt),
        (&child_wt, &grandchild_wt),
        (&root_wt, &grandchild_wt),
    ] {
        assert_ne!(a, b, "every run owns its own tree");
    }
    assert!(
        fs::read_to_string(grandchild_wt.join("src/auth.ts"))
            .unwrap()
            .contains("the child"),
        "the grandchild's tree must come from its parent's history, not its \
         grandparent's"
    );

    let row = log_of(&db).run(&grandchild).unwrap().unwrap();
    assert_eq!(row.parent_run_id, Some(child.clone()));
    assert_eq!(
        row.worktree_path.as_deref(),
        Some(grandchild_wt.to_str().unwrap())
    );
    assert!(row.shadow_ref.is_some());
}

/// Fork one run, run it to completion, and return the tree it worked in.
fn fork_once(db: &Path, home: &Path, at: driver::ForkPoint, id: &RunId, dump: &str) -> PathBuf {
    let dump = home.join(dump);
    let mut launch = LaunchSpec::new(
        "/definitely/not/a/checkout",
        driver::new_session_id(),
        Permission::DenyPrompts,
    );
    launch.binary = stub_agent(
        home,
        &FORK_CHILD_EDITS.replace("__DUMP__", &dump.display().to_string()),
    );
    launch.thinking = Thinking::Off;

    let forked = driver::fork(
        EventLog::open(db).unwrap(),
        driver::ForkSpec {
            run_id: id.clone(),
            at,
            branch: format!("agent/{}", id.0),
            post_create_hook: None,
            prompt: "again".to_owned(),
            launch,
            workspace_mcp: None,
        },
        "2026-09-13T09:05:00.000Z",
        None,
    )
    .unwrap();
    let wt = forked.tree.worktree.path().to_path_buf();
    let (outcome, _) = forked.session.wait().unwrap();
    assert_eq!(outcome.snapshots, 1, "{} edited a file", id.0);

    let cwd = Path::new(fs::read_to_string(&dump).unwrap().lines().next().unwrap())
        .canonicalize()
        .unwrap();
    assert_eq!(
        cwd,
        wt.canonicalize().unwrap(),
        "{} must work in its own tree",
        id.0
    );
    wt
}

/// A parent with no shadow ref cannot be forked, and the refusal is loud.
///
/// The tempting fallbacks are both wrong: the repository's `HEAD` is a tree the
/// conversation never saw, and the parent's worktree *now* is the tree the lazy
/// design was rejected for handing over. Refusing costs a fork; substituting
/// costs a fork that looks like it worked.
///
/// `runs.shadow_ref` stays nullable for §13.1's sake — a ref can be removed
/// after the fact, and Review must then report attribution as unavailable rather
/// than omit it — so this state is representable and something has to answer for
/// it.
#[test]
fn a_fork_of_a_run_with_no_shadow_ref_is_refused_and_writes_nothing() {
    let home = TempDir::new("fork-no-ref");
    let repo = repo(home.path()).canonicalize().unwrap();
    let db = home.path().join("log.sqlite");
    let parent = RunId::from("run_refless");

    let mut log = EventLog::open(&db).unwrap();
    log.create_run(&Run {
        id: parent.clone(),
        agent: "claude-code".to_owned(),
        agent_version: None,
        provider_session_id: Some(agent_protocol::SessionId::from("parent-session")),
        workspace_path: repo.display().to_string(),
        // Still on disk. It is not an answer, and the point of this test is that
        // it is not reached for.
        worktree_path: Some(repo.join(".worktrees/run_refless").display().to_string()),
        branch: Some("agent/run_refless".to_owned()),
        shadow_ref: None,
        prompt: None,
        model: None,
        launch_flags: LaunchFlags::default(),
        status: RunStatus::Finished,
        started_at: "2026-09-13T09:00:00.000Z".to_owned(),
        finished_at: Some("2026-09-13T09:01:00.000Z".to_owned()),
        parent_run_id: None,
        branch_point_seq: None,
    })
    .unwrap();

    let worktrees_before = git(&repo, &["worktree", "list", "--porcelain"]);
    let refs_before = git(&repo, &["for-each-ref", "refs/agent-workspace/"]);

    let child = RunId::from("run_orphan");
    let mut launch = LaunchSpec::new(
        "/definitely/not/a/checkout",
        driver::new_session_id(),
        Permission::DenyPrompts,
    );
    launch.binary = stub_agent(
        home.path(),
        &FORK_CHILD_EDITS.replace("__DUMP__", "/dev/null"),
    );
    launch.thinking = Thinking::Off;

    let forked = driver::fork(
        EventLog::open(&db).unwrap(),
        driver::ForkSpec {
            run_id: child.clone(),
            // Constructed rather than resolved: this parent has no events, and
            // the refusal under test is about its git state, not its log.
            at: driver::ForkPoint {
                parent: parent.clone(),
                seq: 4,
                address: "whatever".to_owned(),
            },
            branch: "agent/run_orphan".to_owned(),
            post_create_hook: None,
            prompt: "fork me".to_owned(),
            launch,
            workspace_mcp: None,
        },
        "2026-09-13T09:05:00.000Z",
        None,
    );
    let Err(err) = forked else {
        panic!("a fork with no recorded tree to branch from was allowed to start")
    };

    assert!(
        matches!(err, driver::Error::CannotFork { .. }),
        "a fork that cannot be placed at its branch point must refuse: {err}"
    );
    assert!(
        err.to_string().contains("shadow ref"),
        "the refusal must say what is missing: {err}"
    );

    // Nothing was written down on the way out: no row, no worktree, no ref.
    assert!(log_of(&db).run(&child).unwrap().is_none());
    assert_eq!(
        git(&repo, &["worktree", "list", "--porcelain"]),
        worktrees_before
    );
    assert_eq!(
        git(&repo, &["for-each-ref", "refs/agent-workspace/"]),
        refs_before
    );
    assert!(!repo.join(".worktrees").join(&child.0).exists());
}

fn log_of(db: &Path) -> EventLog {
    EventLog::open(db).unwrap()
}

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
        model: None,
        launch_flags: LaunchFlags::default(),
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
    let stream = driver::StreamEnd::open(Some(tx));

    let ws = driver::RunWorkspace::new(
        Arc::clone(&pipeline),
        stream.sink(),
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

    // And the half of the channel this workspace deliberately does *not* hold.
    // The run's stream ends where the reader thread ends, and a workspace that
    // is still alive — as one is, on every socket thread the listener has not
    // torn down yet — cannot keep it open. This is the whole of what unblocked
    // a supervised run hosting the permission server: the supervisor's drain
    // used to wait on a sender in here, and the only thing that would have
    // dropped it was the wait the supervisor had not reached.
    drop(stream);
    assert!(
        matches!(rx.recv(), Err(std::sync::mpsc::RecvError)),
        "the workspace kept the run's live channel open after the stream ended"
    );
    // Still usable, and now a no-op rather than an error: a request answered
    // during teardown is recorded, and has nowhere live to be shown.
    let late = ws.request_permission(workspace_mcp::PermissionRequest {
        tool_name: "Read".to_owned(),
        input: serde_json::json!({"file_path": "/etc/hosts"}),
        tool_use_id: Some("toolu_2".to_owned()),
    });
    assert!(matches!(late, workspace_mcp::Decision::Deny { .. }));
    let stored = pipeline.lock().unwrap().log().events(&run).unwrap();
    assert_eq!(stored.len(), 4, "the log still took it: {stored:?}");
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

// ------------------------------------------------------- what a run launched --

/// A stub that writes its argv out and ends its turn. Enough to assert what the
/// driver launched with, and deliberately nothing else: these tests are about
/// the flags, not about what an agent does with them.
const DUMPS_ARGV: &str = r#"#!/usr/bin/env bash
printf '%s\n' "$@" > __DUMP__
cat > /dev/null &
emit() { printf '%s\n' "$1"; }
emit '{"type":"system","subtype":"init","uuid":"__TAG__0","session_id":"s","cwd":"'"$PWD"'","tools":[],"mcp_servers":[],"model":"claude-sonnet-5","permissionMode":"default"}'
emit '{"type":"assistant","uuid":"__TAG__1","message":{"id":"__TAG__m","content":[{"type":"text","text":"ok"}]}}'
emit '{"type":"result","uuid":"__TAG__2","subtype":"success","result_index":0,"is_error":false,"num_turns":1,"duration_ms":1,"total_cost_usd":0.0,"modelUsage":{},"permission_denials":[]}'
"#;

fn dumping_agent(home: &Path, name: &str, dump: &Path, tag: &str) -> PathBuf {
    let script = DUMPS_ARGV
        .replace("__DUMP__", &dump.display().to_string())
        .replace("__TAG__", tag);
    let p = home.join(name);
    fs::write(&p, script).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
    }
    p
}

/// The flags a launch is made with are written to the run's row, and they are
/// the caller's rather than the ones the driver goes on to rewrite.
///
/// Nothing reads this back for the run itself — the process already has them.
/// It is written for the fork or the resume that comes later, which is the only
/// place they can come from once the process is gone: §8's rule against storing
/// what is recomputable does not reach this, because the log records what the
/// agent *did* and never what it was *permitted* to do. A denial arrives as a
/// refused tool call that does not name the allow-list it failed.
#[test]
fn a_run_records_the_flags_it_was_launched_with() {
    let home = TempDir::new("flags-start");
    let repo = repo(home.path());
    let bin = stub_agent(home.path(), EDIT_THEN_REPORT);
    let run = RunId::from("run_flags");
    let db = home.path().join("log.sqlite");

    let mut spec = spec(&repo, &bin, &run);
    spec.launch.model = Some("claude-opus-4-6".to_owned());
    spec.launch.allowed_tools = vec!["Read".to_owned(), "Edit".to_owned()];
    spec.launch.disallowed_tools = vec!["Bash".to_owned()];
    spec.launch.permission_mode = Some("acceptEdits".to_owned());
    spec.launch.max_turns = Some(7);
    spec.launch.max_budget_usd = Some(2.5);
    spec.launch.append_system_prompt = Some("be terse".to_owned());

    let started = driver::start(
        EventLog::open(&db).unwrap(),
        spec,
        "2026-09-14T09:00:00.000Z",
        None,
    )
    .unwrap();
    let (_outcome, log) = started.session.wait().unwrap();

    let row = log.run(&run).unwrap().unwrap();
    assert_eq!(row.model.as_deref(), Some("claude-opus-4-6"));
    assert_eq!(
        row.launch_flags,
        LaunchFlags {
            allowed_tools: vec!["Read".to_owned(), "Edit".to_owned()],
            disallowed_tools: vec!["Bash".to_owned()],
            permission_mode: Some("acceptEdits".to_owned()),
            max_turns: Some(7),
            max_budget_usd: Some(2.5),
            append_system_prompt: Some("be terse".to_owned()),
            extra: Vec::new(),
        },
        "the row carries the policy and the bounds, and nothing that is a \
         property of this machine or this process"
    );
}

/// A resumed run is launched with what the run was launched with.
///
/// The composer on a run from an earlier session is the ordinary path, not an
/// error path (`Registry::resume`), and until the row stored this it woke every
/// such run with an empty allow-list — the same conversation, under a policy it
/// never agreed to, denied outright whatever it was not handed again.
#[test]
fn a_resumed_run_is_launched_with_what_the_run_was_launched_with() {
    let home = TempDir::new("flags-resume");
    let repo = repo(home.path());
    let bin = stub_agent(home.path(), EDIT_THEN_REPORT);
    let run = RunId::from("run_resumed");
    let db = home.path().join("log.sqlite");

    let mut spec = spec(&repo, &bin, &run);
    spec.launch.model = Some("claude-opus-4-6".to_owned());
    spec.launch.allowed_tools = vec!["Read".to_owned(), "Edit".to_owned()];
    spec.launch.max_turns = Some(7);
    let started = driver::start(
        EventLog::open(&db).unwrap(),
        spec,
        "2026-09-14T09:00:00.000Z",
        None,
    )
    .unwrap();
    let (_outcome, log) = started.session.wait().unwrap();
    let row = log.run(&run).unwrap().unwrap();
    drop(log);

    // Everything the shell now passes: a binary, and the session to wake. No
    // flags — that is the point.
    let dump = home.path().join("resumed-argv");
    let mut launch = LaunchSpec::new(
        "/definitely/not/a/checkout",
        row.provider_session_id.clone().unwrap(),
        Permission::DenyPrompts,
    );
    launch.binary = dumping_agent(home.path(), "resume-claude", &dump, "r");
    launch.thinking = Thinking::Off;

    let session = driver::resume(
        EventLog::open(&db).unwrap(),
        &row,
        launch,
        "and now the second turn",
        None,
        None,
    )
    .unwrap();
    let (outcome, _log) = session.wait().unwrap();
    assert_eq!(outcome.exit_code, Some(0));

    let dumped = fs::read_to_string(&dump).unwrap();
    let argv: Vec<&str> = dumped.lines().collect();
    let pair = |flag: &str, value: &str| argv.windows(2).any(|w| w[0] == flag && w[1] == value);
    assert!(pair("--model", "claude-opus-4-6"), "{argv:?}");
    assert!(pair("--allowed-tools", "Read"), "{argv:?}");
    assert!(argv.contains(&"Edit"), "{argv:?}");
    assert!(pair("--max-turns", "7"), "{argv:?}");
    assert!(
        pair("--resume", &row.provider_session_id.as_ref().unwrap().0),
        "it is still a resume of the same session: {argv:?}"
    );
}

/// A run whose row stores no flags still starts.
///
/// That is every run recorded before the column existed, and it is also a run
/// launched with nothing — the two are the same state on disk on purpose, since
/// there is nothing to tell them apart with and pre-granting tools we cannot
/// show were granted would be a guess stored as a fact. The empty reading fails
/// closed, which is exactly the behaviour those rows already had; what must not
/// happen is an error where there used to be a run.
#[test]
fn a_run_whose_row_stores_no_flags_still_starts() {
    let home = TempDir::new("flags-absent");
    let repo = repo(home.path());
    let bin = stub_agent(home.path(), EDIT_THEN_REPORT);
    let run = RunId::from("run_old");
    let db = home.path().join("log.sqlite");

    // A run launched with nothing, which is how a V3-era row reads back.
    let started = driver::start(
        EventLog::open(&db).unwrap(),
        spec(&repo, &bin, &run),
        "2026-09-14T09:00:00.000Z",
        None,
    )
    .unwrap();
    let (_outcome, log) = started.session.wait().unwrap();
    let row = log.run(&run).unwrap().unwrap();
    assert!(
        row.launch_flags.is_empty(),
        "the fixture is only meaningful if the row stores nothing"
    );
    drop(log);

    let dump = home.path().join("old-argv");
    let mut launch = LaunchSpec::new(
        "/definitely/not/a/checkout",
        row.provider_session_id.clone().unwrap(),
        Permission::DenyPrompts,
    );
    launch.binary = dumping_agent(home.path(), "old-claude", &dump, "o");
    launch.thinking = Thinking::Off;

    let session = driver::resume(
        EventLog::open(&db).unwrap(),
        &row,
        launch,
        "wake up",
        None,
        None,
    )
    .unwrap();
    let (outcome, _log) = session.wait().unwrap();
    assert_eq!(
        outcome.exit_code,
        Some(0),
        "it started rather than erroring"
    );

    let dumped = fs::read_to_string(&dump).unwrap();
    let argv: Vec<&str> = dumped.lines().collect();
    assert!(
        !argv.contains(&"--allowed-tools"),
        "nothing is invented to fill the gap: {argv:?}"
    );
    assert!(!argv.contains(&"--max-turns"), "{argv:?}");
}

/// A caller that names something wins over what the row stored.
///
/// The rule is not new — `driver::fork` already applied it to the model, on the
/// argument that a caller naming a model is making exactly the change the
/// branch exists to make. Applied to every field, it is what makes a branch
/// able to differ from its parent at all, and it is decidable rather than
/// guessed because `LaunchSpec::new`'s default *is* "unset" for each of them.
#[test]
fn a_caller_that_names_a_flag_keeps_it_over_the_row() {
    let mut row = Run {
        id: RunId::from("run_row"),
        agent: "claude-code".to_owned(),
        agent_version: None,
        provider_session_id: None,
        workspace_path: "/tmp".to_owned(),
        worktree_path: None,
        branch: None,
        shadow_ref: None,
        prompt: None,
        model: Some("claude-opus-4-6".to_owned()),
        launch_flags: LaunchFlags {
            allowed_tools: vec!["Read".to_owned()],
            max_turns: Some(12),
            ..LaunchFlags::default()
        },
        status: RunStatus::Finished,
        started_at: "2026-09-14T09:00:00.000Z".to_owned(),
        finished_at: None,
        parent_run_id: None,
        branch_point_seq: None,
    };

    let mut named = LaunchSpec::new("/wt", driver::new_session_id(), Permission::DenyPrompts);
    named.model = Some("claude-sonnet-5".to_owned());
    named.allowed_tools = vec!["Bash".to_owned()];
    named.inherit_from(&row);
    assert_eq!(named.model.as_deref(), Some("claude-sonnet-5"));
    assert_eq!(named.allowed_tools, vec!["Bash".to_owned()]);
    assert_eq!(
        named.max_turns,
        Some(12),
        "the fields it said nothing about are still inherited"
    );

    let mut silent = LaunchSpec::new("/wt", driver::new_session_id(), Permission::DenyPrompts);
    silent.inherit_from(&row);
    assert_eq!(silent.model.as_deref(), Some("claude-opus-4-6"));
    assert_eq!(silent.allowed_tools, vec!["Read".to_owned()]);
    assert_eq!(silent.max_turns, Some(12));

    // And a row that stored nothing leaves the spec exactly as it was built.
    row.model = None;
    row.launch_flags = LaunchFlags::default();
    let mut over_nothing =
        LaunchSpec::new("/wt", driver::new_session_id(), Permission::DenyPrompts);
    over_nothing.allowed_tools = vec!["Edit".to_owned()];
    over_nothing.inherit_from(&row);
    assert_eq!(over_nothing.allowed_tools, vec!["Edit".to_owned()]);
    assert_eq!(over_nothing.model, None);
}
