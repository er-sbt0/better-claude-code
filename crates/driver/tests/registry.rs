//! The registry, against a stub agent that behaves like the real one in the
//! only way that matters here: it waits for stdin to close.
//!
//! That is what makes these tests worth writing rather than asserting. Every
//! interesting property of the registry — that a run stays alive until
//! something ends it, that ending it reaps the session, that app exit takes
//! every run with it — is a property of a process that will otherwise sit there
//! forever, and a stub that exits on its own proves none of them.
//!
//! What no test here can cover: a real `claude`. The stub speaks the same
//! stream-json and obeys the same stdin rule, but nothing here exercises the
//! provider's own turn boundaries, its permission layer, or the timing of a run
//! that thinks for a minute. Those need a live run and an API key; the fixture
//! suite and `examples/run.rs` cover the parts of that gap they can.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::time::{Duration, Instant};

use agent_protocol::{RawEvent, RunId};
use driver::{Error, LaunchSpec, Permission, Registry, RunSpec, Thinking};
use event_log::{EventLog, RunStatus};

// ------------------------------------------------------------------ helpers --

fn isolate_git_config() {
    static ONCE: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    ONCE.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("registry-test-cfg-{}", std::process::id()));
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
        let p = std::env::temp_dir().join(format!("registry-test-{}-{name}", std::process::id()));
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

fn git(cwd: &Path, args: &[&str]) {
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
}

fn repo(home: &Path) -> PathBuf {
    isolate_git_config();
    let repo = home.join("my-app");
    fs::create_dir_all(&repo).unwrap();
    fs::write(repo.join("README.md"), "# app\n").unwrap();
    git(&repo, &["init", "-q", "."]);
    git(&repo, &["config", "user.name", "u"]);
    git(&repo, &["config", "user.email", "u@e"]);
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "base"]);
    repo
}

fn stub_agent(home: &Path, name: &str, script: &str) -> PathBuf {
    let p = home.join(name);
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
        branch: format!("agent/{}", run.0),
        base: "HEAD".to_owned(),
        prompt: "explain this repository".to_owned(),
        post_create_hook: None,
        launch,
        workspace_mcp: None,
    }
}

/// A registry whose clock does not move, so a finished run's timestamp is an
/// assertion rather than a guess.
fn registry() -> Registry {
    Registry::with_clock(Arc::new(normalizer::FixedClock(FINISHED_AT.to_owned())))
}

const FINISHED_AT: &str = "2026-09-13T09:00:00.000Z";

/// Nobody is watching the events in most of these tests; the callback still has
/// to exist, and dropping them is what the shell does until step 3.
fn events() -> (impl FnMut(RawEvent) + Send + 'static, Receiver<RawEvent>) {
    let (tx, rx) = mpsc::channel();
    (
        move |e: RawEvent| {
            let _ = tx.send(e);
        },
        rx,
    )
}

/// Block until `f` holds, or fail the test.
///
/// Reaping is asynchronous by construction — the supervisor notices the stream
/// ended, not the caller — so an assertion about a reaped run is an assertion
/// about something that becomes true. Only `shutdown` is synchronous, and its
/// test says so by not using this.
fn eventually(what: &str, mut f: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if f() {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("timed out waiting for {what}");
}

fn wait_for_event(rx: &Receiver<RawEvent>, type_name: &str) -> RawEvent {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(e) if e.type_name() == type_name => return e,
            Ok(_) => continue,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    panic!("the stream ended without a {type_name}");
}

fn status(log: &EventLog, run: &RunId) -> RunStatus {
    log.run(run).unwrap().expect("the run row").status
}

// ------------------------------------------------------------------- agents --

/// A refused fork, as the provider actually reports one: a `result` with
/// `subtype: "error_during_execution"` and a fresh `session_id` of its own,
/// from a process that starts and stops cleanly.
///
/// The exit status is 0 deliberately. That is the case the row status has to
/// survive — `docs/verification/claude-code-fork-address.md` is explicit that
/// an unresolvable `--resume-session-at` is loud in the stream and nowhere
/// else, and that a refused fork still gets a session id, so neither "we got a
/// result" nor "we got a session id" is evidence the fork worked.
const REFUSED: &str = r#"#!/usr/bin/env bash
cat > /dev/null &
printf '%s\n' '{"type":"system","subtype":"init","uuid":"u0","session_id":"f9bdee13","cwd":"'"$PWD"'","tools":[],"mcp_servers":[],"model":"claude-sonnet-5","permissionMode":"default"}'
printf '%s\n' '{"type":"result","uuid":"u1","subtype":"error_during_execution","result_index":0,"is_error":true,"num_turns":0,"duration_ms":10,"total_cost_usd":0,"modelUsage":{},"permission_denials":[],"errors":["No message found with message.uuid of: nope"]}'
exit 0
"#;

/// One turn, then exit without being told to. A real agent does not do this —
/// it is the control case for "the stream ended on its own".
const ONE_TURN: &str = r#"#!/usr/bin/env bash
cat > /dev/null &
printf '%s\n' '{"type":"system","subtype":"init","uuid":"u0","session_id":"s","cwd":"'"$PWD"'","tools":[],"mcp_servers":[],"model":"claude-sonnet-5","permissionMode":"default"}'
printf '%s\n' '{"type":"assistant","uuid":"u1","message":{"id":"m1","content":[{"type":"text","text":"it is an app"}]}}'
printf '%s\n' '{"type":"result","uuid":"u2","subtype":"success","result_index":0,"is_error":false,"num_turns":1,"duration_ms":10,"total_cost_usd":0.01,"modelUsage":{"claude-sonnet-5":{"inputTokens":10,"outputTokens":5,"costUSD":0.01}},"permission_denials":[]}'
"#;

/// The real stdin contract: finish the turn, then wait for another one until
/// stdin closes. Every test about a run's lifetime uses this.
const WAITS_FOR_STDIN: &str = r#"#!/usr/bin/env bash
read -r _prompt
printf '%s\n' '{"type":"system","subtype":"init","uuid":"u0","session_id":"s","cwd":"'"$PWD"'","tools":[],"mcp_servers":[],"model":"claude-sonnet-5","permissionMode":"default"}'
printf '%s\n' '{"type":"assistant","uuid":"u1","message":{"id":"m1","content":[{"type":"text","text":"it is an app"}]}}'
printf '%s\n' '{"type":"result","uuid":"u2","subtype":"success","result_index":0,"is_error":false,"num_turns":1,"duration_ms":10,"total_cost_usd":0.01,"modelUsage":{"claude-sonnet-5":{"inputTokens":10,"outputTokens":5,"costUSD":0.01}},"permission_denials":[]}'
while read -r line; do
  if [[ $line == *heard-you* ]]; then
    printf '%s\n' '{"type":"assistant","uuid":"u3","message":{"id":"m2","content":[{"type":"text","text":"the follow-up said heard-you"}]}}'
    printf '%s\n' '{"type":"result","uuid":"u4","subtype":"success","result_index":1,"is_error":false,"num_turns":2,"duration_ms":10,"total_cost_usd":0.01,"modelUsage":{"claude-sonnet-5":{"inputTokens":10,"outputTokens":5,"costUSD":0.01}},"permission_denials":[]}'
  fi
done
"#;

// -------------------------------------------------------------------- tests --

/// The supervisor's end condition: the stream closes, the session comes out of
/// the map, the process is waited for, and the run row stops saying `Running` —
/// which `driver::start` deliberately never does, because only something
/// holding the session can tell a finished run from a quiet one.
#[test]
fn a_run_is_reaped_when_its_stream_ends() {
    let home = TempDir::new("reap");
    let repo = repo(home.path());
    let bin = stub_agent(home.path(), "one-turn", ONE_TURN);
    let run = RunId::from("run_reap");
    let db = home.path().join("log.sqlite");

    let registry = registry();
    let (on_event, rx) = events();
    let live = registry
        .start(
            EventLog::open(&db).unwrap(),
            spec(&repo, &bin, &run),
            on_event,
        )
        .unwrap();
    assert_eq!(live.run_id, run);
    assert_eq!(live.branch, "agent/run_reap");
    assert!(
        live.worktree_path.is_some_and(|p| p.exists()),
        "a run started from nothing has a worktree (§13); only a fork lacks one"
    );

    // The tail events a process exit produces reach the callback, not only the
    // log. A live view that misses these shows every run as unfinished.
    //
    // `rx.iter()` runs to the end of the channel, which is the strong version
    // of the claim: not merely that `run.finished` was delivered, but that it
    // was the **last** thing delivered before the senders were dropped. That
    // ordering is what the shell's emit path depends on — it forwards each
    // event inline, so whatever arrives here last is what the webview sees
    // last, and a queue anywhere in between would be a place for exactly this
    // event to be stranded.
    let seen: Vec<String> = rx.iter().map(|e| e.type_name().to_owned()).collect();
    assert!(seen.contains(&"run.finished".to_owned()), "{seen:?}");
    assert_eq!(
        seen.last().map(String::as_str),
        Some("run.finished"),
        "the tail event did not come last: {seen:?}"
    );
    assert!(
        seen.iter().any(|t| t == "turn.finished"),
        "the stdout events came through too: {seen:?}"
    );

    eventually("the run to leave the registry", || !registry.is_live(&run));
    assert!(registry.live().is_empty());

    // Read it back on a third connection, which is the UI's view of it.
    let log = EventLog::open(&db).unwrap();
    eventually("the run row to be closed", || {
        status(&log, &run) != RunStatus::Running
    });
    let row = log.run(&run).unwrap().unwrap();
    assert_eq!(row.status, RunStatus::Finished, "a clean exit");
    assert_eq!(row.finished_at.as_deref(), Some(FINISHED_AT));
}

/// The decision this registry exists to make: a finished *turn* is not a
/// finished *run*. `examples/run.rs` closes stdin on `turn.finished` and is
/// therefore single-turn by construction; an app must not, or a follow-up has
/// nowhere to go.
#[test]
fn a_run_stays_live_after_a_turn_until_something_ends_it() {
    let home = TempDir::new("stdin");
    let repo = repo(home.path());
    let bin = stub_agent(home.path(), "waits", WAITS_FOR_STDIN);
    let run = RunId::from("run_stdin");
    let db = home.path().join("log.sqlite");

    let registry = registry();
    let (on_event, rx) = events();
    registry
        .start(
            EventLog::open(&db).unwrap(),
            spec(&repo, &bin, &run),
            on_event,
        )
        .unwrap();

    wait_for_event(&rx, "turn.finished");
    assert!(
        registry.is_live(&run),
        "the turn ended and nothing closed stdin, so the run is still a run"
    );
    let log = EventLog::open(&db).unwrap();
    assert_eq!(status(&log, &run), RunStatus::Running);

    registry.end(&run).unwrap();
    eventually("the ended run to be reaped", || !registry.is_live(&run));
    eventually("the run row to be closed", || {
        status(&log, &run) == RunStatus::Finished
    });
}

/// What the open stdin is for (§5: one process, many turns).
#[test]
fn a_follow_up_turn_reaches_the_agent() {
    let home = TempDir::new("turn2");
    let repo = repo(home.path());
    let bin = stub_agent(home.path(), "waits", WAITS_FOR_STDIN);
    let run = RunId::from("run_turn2");
    let db = home.path().join("log.sqlite");

    let registry = registry();
    let (on_event, rx) = events();
    registry
        .start(
            EventLog::open(&db).unwrap(),
            spec(&repo, &bin, &run),
            on_event,
        )
        .unwrap();
    wait_for_event(&rx, "turn.finished");

    registry.send(&run, "and again, heard-you?").unwrap();
    // The agent answers on a second turn, through the same process.
    wait_for_event(&rx, "turn.finished");

    registry.end(&run).unwrap();
    eventually("the run to be reaped", || !registry.is_live(&run));

    let log = EventLog::open(&db).unwrap();
    let said: Vec<String> = log
        .events(&run)
        .unwrap()
        .into_iter()
        .filter_map(|e| match e.event {
            agent_protocol::Event::AssistantMessage(m) => Some(m.text),
            _ => None,
        })
        .collect();
    assert!(
        said.iter().any(|t| t.contains("heard-you")),
        "the follow-up never reached the agent: {said:?}"
    );
    assert_eq!(log.metrics(&run).unwrap().turns, 2);
}

/// An interrupt is the other way a run ends, and the log keeps everything it
/// received (§8: per line, not at the end).
#[test]
fn interrupting_a_run_reaps_it_and_records_that_it_did_not_finish() {
    let home = TempDir::new("kill");
    let repo = repo(home.path());
    let bin = stub_agent(home.path(), "waits", WAITS_FOR_STDIN);
    let run = RunId::from("run_kill");
    let db = home.path().join("log.sqlite");

    let registry = registry();
    let (on_event, rx) = events();
    registry
        .start(
            EventLog::open(&db).unwrap(),
            spec(&repo, &bin, &run),
            on_event,
        )
        .unwrap();
    wait_for_event(&rx, "turn.finished");

    registry.interrupt(&run).unwrap();
    eventually("the killed run to be reaped", || !registry.is_live(&run));

    let log = EventLog::open(&db).unwrap();
    eventually("the run row to be closed", || {
        status(&log, &run) != RunStatus::Running
    });
    assert_eq!(
        status(&log, &run),
        RunStatus::Failed,
        "no exit code: a killed run is not a finished one, and we have no \
         better word for it than Failed"
    );
    let types: Vec<String> = log
        .events(&run)
        .unwrap()
        .iter()
        .map(|e| e.type_name().to_owned())
        .collect();
    assert!(
        types.contains(&"turn.finished".to_owned()),
        "the interrupt took the log with it: {types:?}"
    );
}

/// App exit. Without this, a `tauri dev` reload leaves one `claude` per run
/// running on the developer's machine — and unlike the other tests here,
/// `shutdown` is synchronous, so nothing below needs to wait for anything.
#[test]
fn shutdown_ends_every_live_run() {
    let home = TempDir::new("exit");
    let repo = repo(home.path());
    let bin = stub_agent(home.path(), "waits", WAITS_FOR_STDIN);
    let db = home.path().join("log.sqlite");
    let registry = registry();

    let mut streams = Vec::new();
    for n in 0..2 {
        let run = RunId::from(format!("run_exit_{n}"));
        let (on_event, rx) = events();
        registry
            .start(
                EventLog::open(&db).unwrap(),
                spec(&repo, &bin, &run),
                on_event,
            )
            .unwrap();
        streams.push((run, rx));
    }
    for (_, rx) in &streams {
        wait_for_event(rx, "turn.finished");
    }
    assert_eq!(registry.live().len(), 2);

    let ended = registry.shutdown();
    assert_eq!(ended.len(), 2, "both runs were ours to end");
    assert!(
        registry.live().is_empty(),
        "shutdown returned with a session still in the map"
    );

    let log = EventLog::open(&db).unwrap();
    for (run, _) in &streams {
        assert_ne!(
            status(&log, run),
            RunStatus::Running,
            "{} was left looking alive",
            run.0
        );
    }
}

/// A run that ended, and a run that was never ours, are the same answer: this
/// process is not holding it. The alternative is a command that silently
/// succeeds at nothing.
#[test]
fn steering_a_run_that_is_not_live_is_an_error() {
    let registry = Registry::new();
    let gone = RunId::from("run_never");
    for result in [
        registry.send(&gone, "hello"),
        registry.end(&gone),
        registry.interrupt(&gone),
    ] {
        assert!(matches!(result, Err(Error::NotLive { .. })), "{result:?}");
    }
    assert!(registry.reap(&gone).is_none());
}

/// The deadlock this would otherwise become: the MCP listener's accept thread
/// holds a clone of the event sink until the session is waited on, and the
/// supervisor waits on the session only once the sink is gone. Neither moves.
/// Refused at the door until permissions are built, because a hang is the worst
/// way to discover it.
#[test]
fn a_run_hosting_the_workspace_mcp_server_is_refused() {
    let home = TempDir::new("mcp");
    let repo = repo(home.path());
    let bin = stub_agent(home.path(), "one-turn", ONE_TURN);
    let run = RunId::from("run_mcp");

    let mut s = spec(&repo, &bin, &run);
    s.workspace_mcp = Some(driver::WorkspaceMcp {
        relay: home.path().join("workspace-mcp-relay"),
        runtime_dir: home.path().join("run"),
        decider: Arc::new(driver::DenyEverything),
    });

    let registry = Registry::new();
    let err = registry
        .start(EventLog::open_in_memory().unwrap(), s, |_| {})
        .unwrap_err();

    assert!(matches!(err, Error::NotSupervisable { .. }), "{err:?}");
    // Refused before anything happened, which is why the check is in `start`
    // and not in the supervisor: no registry entry, no worktree, no process.
    // A run that cannot be supervised must not half-exist.
    assert!(registry.live().is_empty());
    assert!(
        !repo.join(".git/worktrees").exists(),
        "the refused run got a worktree anyway"
    );
}

/// A run that failed in the stream is never filed as finished, however politely
/// its process exited.
///
/// This is the shape a refused fork arrives in: `--resume-session-at` on an
/// address that resolves nowhere produces `error_during_execution` from a
/// process that started, answered nothing and exited. Deriving the row's status
/// from the exit code alone would call that `Finished`, and the run list would
/// show a branch that does not exist as a branch that worked.
#[test]
fn a_run_that_failed_in_the_stream_is_not_filed_as_finished() {
    let home = TempDir::new("refused");
    let repo = repo(home.path());
    let bin = stub_agent(home.path(), "refused", REFUSED);
    let run = RunId::from("run_refused");
    let db = home.path().join("log.sqlite");

    let registry = registry();
    let (on_event, _rx) = events();
    registry
        .start(
            EventLog::open(&db).unwrap(),
            spec(&repo, &bin, &run),
            on_event,
        )
        .unwrap();
    eventually("the refused run to be reaped", || !registry.is_live(&run));

    let log = EventLog::open(&db).unwrap();
    eventually("the run row to be closed", || {
        status(&log, &run) != RunStatus::Running
    });
    assert_eq!(
        status(&log, &run),
        RunStatus::Failed,
        "the process exited 0; acceptance is read off the result subtype"
    );

    let types: Vec<String> = log
        .events(&run)
        .unwrap()
        .iter()
        .map(|e| e.type_name().to_owned())
        .collect();
    assert!(types.contains(&"run.failed".to_owned()), "{types:?}");
    assert!(
        !types.contains(&"assistant.message".to_owned()),
        "a refused fork answers nothing: {types:?}"
    );
}
