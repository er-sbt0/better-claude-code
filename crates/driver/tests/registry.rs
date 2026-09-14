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
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use agent_protocol::{Event, RawEvent, RunId};
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

/// Drain to the end of the run's stream, or fail loudly.
///
/// `for e in rx` is the natural spelling and it is unbounded, which is exactly
/// the wrong shape for the failure this file has to be able to catch: a stream
/// that never closes is a supervisor that never reaps, and an unbounded drain
/// turns that into a CI job that times out rather than a red test.
fn drain_to_end(rx: &Receiver<RawEvent>) -> Vec<String> {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut seen = Vec::new();
    while Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(e) => seen.push(e.type_name().to_owned()),
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => return seen,
        }
    }
    panic!("the run's stream never closed; it had delivered {seen:?}");
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

/// A turn that fails, and then a process that keeps waiting for another one.
///
/// The combination is what makes it interesting: the failure is in the log and
/// the process is still alive, so the only way this run can end is a kill — and
/// a kill must not be allowed to overwrite the failure with "we stopped it".
const FAILS_THEN_WAITS: &str = r#"#!/usr/bin/env bash
read -r _prompt
printf '%s\n' '{"type":"system","subtype":"init","uuid":"u0","session_id":"s","cwd":"'"$PWD"'","tools":[],"mcp_servers":[],"model":"claude-sonnet-5","permissionMode":"default"}'
printf '%s\n' '{"type":"result","uuid":"u1","subtype":"error_during_execution","result_index":0,"is_error":true,"num_turns":0,"duration_ms":10,"total_cost_usd":0,"modelUsage":{},"permission_denials":[],"errors":["it went wrong"]}'
while read -r _line; do :; done
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

/// A run that started something of its own, which is what every real run does:
/// `claude` spawns `workspace-mcp-relay` for our injected MCP server and a
/// child per `Bash` tool call — `npm test`, a dev server, whatever the agent
/// was asked to run.
///
/// The grandchild is the whole point. It inherits the agent's process group and
/// nothing else, so a signal aimed at the agent's pid never reaches it: it is
/// reparented to init and keeps running, still holding the run's worktree.
/// Writing its pid where a test can read it is the only way to ask the question
/// from outside, since the test is not its parent and cannot wait for it.
///
/// Written to a temporary and renamed, because a half-written pid file read at
/// the wrong moment is a flake rather than a failure.
fn spawns_a_grandchild(pidfile: &Path) -> String {
    format!(
        r#"#!/usr/bin/env bash
read -r _prompt
( while true; do sleep 0.05; done ) &
echo $! > {p}.tmp && mv {p}.tmp {p}
printf '%s\n' '{{"type":"system","subtype":"init","uuid":"u0","session_id":"s","cwd":"'"$PWD"'","tools":[],"mcp_servers":[],"model":"claude-sonnet-5","permissionMode":"default"}}'
printf '%s\n' '{{"type":"result","uuid":"u1","subtype":"success","result_index":0,"is_error":false,"num_turns":1,"duration_ms":10,"total_cost_usd":0.01,"modelUsage":{{}},"permission_denials":[]}}'
while read -r _line; do :; done
"#,
        p = pidfile.display()
    )
}

/// Is that pid still a process we could signal?
///
/// `kill(pid, 0)` asks the kernel and answers for a process the caller did not
/// start, which `Child::try_wait` cannot do. An orphan is reparented to init
/// and reaped there, so a dead grandchild is gone rather than a zombie.
fn alive(pid: i32) -> bool {
    unsafe { libc::kill(pid, 0) == 0 }
}

/// Read the pid the stub wrote, or fail loudly rather than hang.
fn grandchild_pid(pidfile: &Path) -> i32 {
    eventually("the agent to start a child of its own", || pidfile.exists());
    fs::read_to_string(pidfile)
        .unwrap()
        .trim()
        .parse()
        .expect("the stub wrote a pid")
}

/// A `Decider` that says yes and remembers what it was asked — which is the
/// shape §12's dialog will have, minus the person.
struct Answers {
    asked: Arc<Mutex<Vec<String>>>,
}

impl driver::Decider for Answers {
    fn decide(&self, request: &workspace_mcp::PermissionRequest) -> workspace_mcp::Decision {
        self.asked.lock().unwrap().push(request.tool_name.clone());
        workspace_mcp::Decision::Allow {
            updated_input: None,
        }
    }
}

/// A launch the provider refuses outright: one line on stderr, nothing at all
/// on stdout, exit 1.
///
/// Not invented. It is what `claude` 2.1.270 does to a malformed resume —
/// `--session-id can only be used with --continue or --resume if
/// --fork-session is also specified`, found while recording `f5ba282` — and
/// the shape is the point: a `runs` row, no events, and the only sentence that
/// said why on a channel nothing read.
///
/// Distinct from `REFUSED` above, which is the *other* silent failure: a
/// process that exits 0 with a full stream and says `error_during_execution`.
/// Neither check covers both, which is why stderr is not redundant with the
/// subtype.
const REFUSES_TO_START: &str = r#"#!/usr/bin/env bash
cat > /dev/null &
echo 'Error: --session-id can only be used with --continue or --resume if --fork-session is also specified.' >&2
exit 1
"#;

/// A healthy single turn that also writes a deprecation warning to stderr.
///
/// The noise case: this run's log explains itself perfectly well, and the
/// warning must not be in it.
const ONE_TURN_NOISY: &str = r#"#!/usr/bin/env bash
cat > /dev/null &
echo 'warning: --thinking is deprecated' >&2
printf '%s\n' '{"type":"system","subtype":"init","uuid":"u0","session_id":"s","cwd":"'"$PWD"'","tools":[],"mcp_servers":[],"model":"claude-sonnet-5","permissionMode":"default"}'
printf '%s\n' '{"type":"result","uuid":"u1","subtype":"success","result_index":0,"is_error":false,"num_turns":1,"duration_ms":10,"total_cost_usd":0.01,"modelUsage":{},"permission_denials":[]}'
"#;

/// A run that fails while shouting: 500 lines, then one 200 KB line with no
/// newline in it.
///
/// Both halves are the shapes `-d/--debug` produces, and each defeats a
/// different bound. Neither is hypothetical — debug output is a user-controlled
/// variable, so "how much stderr may this run cost us" is a question the agent
/// would otherwise answer.
const SHOUTS_THEN_FAILS: &str = r#"#!/usr/bin/env bash
cat > /dev/null &
for i in $(seq 1 500); do echo "line $i" >&2; done
head -c 200000 /dev/zero | tr '\0' 'x' >&2
exit 1
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
        live.worktree_path.exists(),
        "a run is created with its worktree (§13), forks included"
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
///
/// **What it records is `Interrupted`, and this test used to say `Failed`.**
/// The old assertion carried its own defence — "we have no better word for it
/// than Failed" — which is an admission that it was pinning the current thing
/// rather than the right thing. It is the right thing now: the user asked for
/// the run to stop, the agent did nothing wrong, and `failed` is a claim about
/// the agent's work that the canvas, the runs list and the inspector all
/// repeat.
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
        RunStatus::Interrupted,
        "no exit code: a killed run is neither a finished one nor a failed one"
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

/// **An interrupt has to reach the run's whole tree, not just its root.**
///
/// The agent is never one process. Killing the `claude` pid alone — which is
/// what `Session::interrupt` did, with `SIGKILL`, on the *ordinary* path
/// through `shutdown` — reparented the relay and every `Bash` tool call to
/// init, where they kept running and kept holding the worktree. One leaked tree
/// per live run, on every app exit and every `tauri dev` reload.
///
/// The stub stands in for that faithfully: a long-lived background child, and a
/// parent that then waits on stdin like the real one. The assertion is about
/// the grandchild and nothing else, because the parent died either way and that
/// is exactly what made the leak invisible.
///
/// Run against the old code this fails one line earlier, at "the run to be
/// reaped", and that is worth knowing: the orphan inherited the agent's stdout
/// pipe, so the read loop never saw EOF and the run was never reaped either.
/// The leak was not only a stray process — it was a session that could not end.
#[test]
fn interrupting_a_run_takes_the_processes_it_started_with_it() {
    let home = TempDir::new("tree");
    let repo = repo(home.path());
    let pidfile = home.path().join("grandchild.pid");
    let bin = stub_agent(home.path(), "spawns", &spawns_a_grandchild(&pidfile));
    let run = RunId::from("run_tree");
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

    let grandchild = grandchild_pid(&pidfile);
    assert!(
        alive(grandchild),
        "the control case: there is a descendant to orphan"
    );

    registry.interrupt(&run).unwrap();
    eventually("the run to be reaped", || !registry.is_live(&run));
    // Bounded, like everything else here: before the fix this was never true,
    // and an unbounded wait would have made that a CI timeout instead of a red
    // test.
    eventually("the agent's child to go with it", || !alive(grandchild));
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

/// **The bug this status vocabulary was widened for**: a run that did its work
/// and was still open when the app exited was recorded as `failed`.
///
/// It is not an edge case, it is the common path. Nothing closes a run's stdin
/// on its behalf, so a healthy run waits for its next turn until `shutdown`
/// kills it — and a process killed by a signal has no exit code, which the old
/// rule read as "not `Some(0)`, therefore `Failed`". Everything the log knew
/// said otherwise, and this test says it in the log's own words: a
/// `turn.finished`, a `run.finished`, and no `run.failed` anywhere.
#[test]
fn a_run_that_did_its_work_and_was_still_open_at_app_exit_is_not_filed_as_failed() {
    let home = TempDir::new("exit-ok");
    let repo = repo(home.path());
    let bin = stub_agent(home.path(), "waits", WAITS_FOR_STDIN);
    let run = RunId::from("run_exit_ok");
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

    // The window closes on a run that is perfectly well.
    assert_eq!(registry.shutdown(), vec![run.clone()]);

    let log = EventLog::open(&db).unwrap();
    let row = log.run(&run).unwrap().expect("the run row");
    assert_ne!(
        row.status,
        RunStatus::Failed,
        "the app ended this run; nothing about it failed"
    );
    assert_eq!(row.status, RunStatus::Interrupted);
    assert_eq!(row.finished_at.as_deref(), Some(FINISHED_AT));

    let types: Vec<String> = log
        .events(&run)
        .unwrap()
        .iter()
        .map(|e| e.type_name().to_owned())
        .collect();
    assert!(types.contains(&"turn.finished".to_owned()), "{types:?}");
    assert!(
        !types.contains(&"run.failed".to_owned()),
        "the log never said this run failed, so neither may the row: {types:?}"
    );
    assert_eq!(log.metrics(&run).unwrap().turns, 1, "the turn still counts");
}

/// A failure already in the log outranks the interrupt that stopped the
/// process.
///
/// The ordering only shows up in this shape: a turn that failed, and then a
/// process still sitting on stdin, so that the only available ending is a kill.
/// Reading "no exit code" as `Interrupted` unconditionally would erase the
/// failure — the same class of lie as the old rule inventing one, pointed the
/// other way, and the more expensive of the two because a hidden failure is a
/// branch the runs list says nothing is wrong with.
#[test]
fn a_failure_in_the_log_survives_the_interrupt_that_ended_the_process() {
    let home = TempDir::new("kill-failed");
    let repo = repo(home.path());
    let bin = stub_agent(home.path(), "fails-then-waits", FAILS_THEN_WAITS);
    let run = RunId::from("run_kill_failed");
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
        "the turn failed before anyone killed it, and killing it is not an \
         excuse to forget"
    );
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

/// **The run the whole permission feature was waiting on**: supervised, hosting
/// the workspace MCP server, answering a request through a `Decider` — and then
/// reaped, which is the part that was impossible.
///
/// It used to be refused at the door. The listener's accept thread held a clone
/// of the event sink and the listener only came down inside `Session::wait`, so
/// a supervisor that drains the channel before waiting would have waited for a
/// close that waited for the wait it had not reached. `driver::sink` moved the
/// only sender into the reader thread and gave every other producer a handle
/// that cannot keep the channel open, which is why this now reads like any
/// other run.
///
/// The relay is played by this test rather than spawned: the socket is the
/// contract, `workspace-mcp` already covers the relay end of it against a real
/// child process, and what is under test here is the driver's ownership, not
/// the byte pipe.
#[test]
fn a_supervised_run_hosts_the_permission_server_and_is_still_reaped() {
    let home = TempDir::new("mcp");
    let repo = repo(home.path());
    let bin = stub_agent(home.path(), "waits", WAITS_FOR_STDIN);
    let run = RunId::from("run_mcp");
    let db = home.path().join("log.sqlite");
    let runtime_dir = home.path().join("rt");

    let asked = Arc::new(Mutex::new(Vec::new()));
    let mut s = spec(&repo, &bin, &run);
    s.workspace_mcp = Some(driver::WorkspaceMcp {
        // Never read: nothing here spawns the provider, so nothing spawns the
        // relay. The path is in the injected `--mcp-config` and that is all.
        relay: home.path().join("workspace-mcp-relay"),
        // §15: outside the worktree, which is the one property of this path
        // that is not a detail.
        runtime_dir: runtime_dir.clone(),
        decider: Arc::new(Answers {
            asked: Arc::clone(&asked),
        }),
    });

    let registry = registry();
    let (on_event, rx) = events();
    registry
        .start(EventLog::open(&db).unwrap(), s, on_event)
        .unwrap();

    // The provider's permission layer, arriving on the runtime's own channel
    // (§15) while the run is live.
    let socket = runtime_dir.join(&run.0).join("mcp.sock");
    let mut relay = UnixStream::connect(&socket)
        .unwrap_or_else(|e| panic!("the run bound no socket at {}: {e}", socket.display()));
    // Bounded, because the failure being tested for is a thread that never
    // answers. A blocked read here would hang the test binary.
    relay
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut replies = BufReader::new(relay.try_clone().unwrap());
    writeln!(
        relay,
        r#"{{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{{"name":"request_permission","arguments":{{"tool_name":"Edit","input":{{"file_path":"README.md"}},"tool_use_id":"toolu_1"}}}}}}"#
    )
    .unwrap();
    relay.flush().unwrap();
    let mut line = String::new();
    replies
        .read_line(&mut line)
        .expect("the workspace answered the permission request within ten seconds");
    let response: serde_json::Value = serde_json::from_str(&line).unwrap();
    let decision: serde_json::Value =
        serde_json::from_str(response["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(decision["behavior"], "allow", "{line}");
    assert_eq!(
        asked.lock().unwrap().as_slice(),
        ["Edit"],
        "the decider is what answered, not a default"
    );

    // Both halves of the round trip reach the live view, on the same channel as
    // everything else the run says (§8's trap).
    assert_eq!(
        wait_for_event(&rx, "permission.requested").type_name(),
        "permission.requested"
    );
    assert_eq!(
        wait_for_event(&rx, "permission.resolved").type_name(),
        "permission.resolved"
    );

    // Now end it, with the MCP connection still open — which is the real
    // condition, since a relay holds its connection for the life of its agent
    // and dies asynchronously with it. The run must not be hostage to it.
    registry.end(&run).unwrap();

    let seen = drain_to_end(&rx);
    assert_eq!(
        seen.last().map(String::as_str),
        Some("run.finished"),
        "the tail event did not come last: {seen:?}"
    );

    eventually("the run to leave the registry", || !registry.is_live(&run));

    let log = EventLog::open(&db).unwrap();
    eventually("the run row to be closed", || {
        status(&log, &run) != RunStatus::Running
    });
    assert_eq!(
        log.run(&run).unwrap().unwrap().status,
        RunStatus::Finished,
        "reaping handed the log back, so `finish` could write the row"
    );

    // The audit record, which is the half §14 says is the account of what the
    // agent was allowed to do.
    let kinds: Vec<&str> = log
        .events(&run)
        .unwrap()
        .iter()
        .map(|e| e.type_name())
        .filter(|t| t.starts_with("permission."))
        .collect();
    assert_eq!(kinds, ["permission.requested", "permission.resolved"]);

    // Held until here on purpose: dropping it earlier would have let the
    // connection close on its own and proved nothing.
    drop(relay);
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

/// A fork is a run: the registry starts it, supervises it and reaps it through
/// the same machinery, and the child's row carries both halves of the graph
/// edge.
///
/// This is the command behind ⌘⇧F. `driver::fork` has existed since the fork
/// path was built and nothing could call it from a window, because the registry
/// — the one thing the shell talks to — had no `fork`. A button wired to
/// nothing is the decoration this project keeps legislating against, so the
/// wire is tested where a test reaches it without a window.
#[test]
fn forking_through_the_registry_supervises_the_child_like_any_other_run() {
    let home = TempDir::new("fork");
    let repo = repo(home.path());
    let bin = stub_agent(home.path(), "one-turn", ONE_TURN);
    let parent = RunId::from("run_parent");
    let db = home.path().join("log.sqlite");

    let registry = registry();
    let (on_event, _rx) = events();
    registry
        .start(
            EventLog::open(&db).unwrap(),
            spec(&repo, &bin, &parent),
            on_event,
        )
        .unwrap();
    eventually("the parent to be reaped", || !registry.is_live(&parent));

    // The branch point, resolved the way the shell resolves it: a `seq` in the
    // parent's log, through the parent's own `idempotency_key`.
    let log = EventLog::open(&db).unwrap();
    eventually("the parent's assistant message", || {
        log.events(&parent)
            .unwrap()
            .iter()
            .any(|e| e.type_name() == "assistant.message")
    });
    let at_seq = log
        .events(&parent)
        .unwrap()
        .iter()
        .find(|e| e.type_name() == "assistant.message")
        .expect("the parent said something")
        .seq;
    // The projection the toolbar is drawn from and the resolution the driver
    // performs are the same rule, and this is where they are checked against
    // each other: the surface must not offer a fork the driver then refuses.
    let offered = log.fork_points(std::slice::from_ref(&parent)).unwrap();
    assert!(
        offered[0].blocks.iter().any(|b| b.seq == at_seq),
        "the block the fork is anchored at was not offered as forkable"
    );
    assert_eq!(
        offered[0]
            .blocks
            .iter()
            .find(|b| b.seq == at_seq)
            .unwrap()
            .inherited_messages,
        1,
        "one answer, and the strip on the child says so"
    );
    let at = driver::ForkPoint::resolve(&log, &parent, at_seq).unwrap();
    assert_eq!(
        at.address, "u1",
        "the address is the bare uuid, never our `#i` fan-out suffix"
    );

    let child = RunId::from("run_child");
    let mut launch = LaunchSpec::new(&repo, driver::new_session_id(), Permission::DenyPrompts);
    launch.binary = bin.clone();
    launch.thinking = Thinking::Off;
    let live = registry
        .fork(
            EventLog::open(&db).unwrap(),
            driver::ForkSpec {
                run_id: child.clone(),
                at: at.clone(),
                branch: format!("agent/{}", child.0),
                post_create_hook: None,
                prompt: "try it the other way".to_owned(),
                launch,
                workspace_mcp: None,
            },
            |_| {},
        )
        .unwrap();

    assert_eq!(live.run_id, child);
    assert_eq!(live.branch, "agent/run_child");
    assert!(
        live.worktree_path.exists()
            && live.worktree_path != repo.join(".worktrees").join(&parent.0),
        "a fork is created with its own worktree (§13), from the parent's \
         shadow commit at the branch point"
    );

    // Supervised by the same thread with the same end condition, and reaped by
    // the same rule — which is the whole claim of `Registry::fork`.
    eventually("the child to be reaped", || !registry.is_live(&child));
    eventually("the child's row to be closed", || {
        status(&log, &child) != RunStatus::Running
    });

    let row = log.run(&child).unwrap().expect("the child's row");
    assert_eq!(row.parent_run_id.as_ref(), Some(&parent));
    assert_eq!(
        row.branch_point_seq,
        Some(at_seq),
        "the graph edge is stored whole, or it is lost: the provider records \
         no parentage"
    );
    assert_eq!(
        row.model,
        log.run(&parent).unwrap().unwrap().model,
        "a fork inherits its parent's model, by copy onto its own row"
    );
}

/// **A conversation outlives the window that started it** (§14).
///
/// Nothing closes a run's stdin on its behalf, so every run ends when the app
/// does. Until `Registry::resume` existed the composer could only reach runs
/// this process happened to be holding, which meant it was dead on every run
/// from every previous session — nearly all of them, for anyone who has closed
/// the app once. The provider owns the session and we assign its id, so a run
/// we are not holding is asleep rather than gone.
#[test]
fn a_run_this_process_is_not_holding_can_be_resumed_and_take_another_turn() {
    let home = TempDir::new("resume");
    let repo = repo(home.path());
    let bin = stub_agent(home.path(), "waits", WAITS_FOR_STDIN);
    let run = RunId::from("run_resume");
    let db = home.path().join("log.sqlite");

    let registry = registry();
    let (on_event, rx) = events();
    let spec = spec(&repo, &bin, &run);
    let session_id = spec.launch.session_id.clone();
    let binary = spec.launch.binary.clone();
    registry
        .start(EventLog::open(&db).unwrap(), spec, on_event)
        .unwrap();
    wait_for_event(&rx, "turn.finished");

    // The window closes: the run is ended and reaped, exactly as `shutdown`
    // would leave it.
    registry.end(&run).unwrap();
    eventually("the run to be reaped", || !registry.is_live(&run));

    let log = EventLog::open(&db).unwrap();
    let row = log.run(&run).unwrap().expect("the run row");
    assert_eq!(
        row.provider_session_id.as_ref(),
        Some(&session_id),
        "§14: we assign the session id, so the row is what makes a resume possible"
    );
    let last = log.last_seq(&run).unwrap();
    assert!(last > 0, "the first life of the run wrote events");

    // A second life, from the row alone.
    let mut launch = LaunchSpec::new(
        row.worktree_path.as_deref().unwrap_or(&row.workspace_path),
        session_id,
        Permission::DenyPrompts,
    );
    launch.binary = binary;
    launch.thinking = Thinking::Off;

    let (on_event2, rx2) = events();
    let live = registry
        .resume(
            EventLog::open(&db).unwrap(),
            &row,
            launch,
            "and now the second turn",
            None,
            on_event2,
        )
        .unwrap();

    assert_eq!(
        live.run_id, run,
        "a resumed run is the same run, not a new one"
    );
    assert_eq!(
        live.worktree_path,
        std::path::PathBuf::from(row.worktree_path.as_deref().unwrap()),
        "and it works in the tree the row already named"
    );
    assert!(registry.is_live(&run), "it is supervised again");

    wait_for_event(&rx2, "turn.finished");
    assert!(
        log.last_seq(&run).unwrap() > last,
        "the second life appended to the same run's log rather than starting one"
    );

    registry.end(&run).unwrap();
    eventually("the resumed run to be reaped", || !registry.is_live(&run));
}

/// A run that never reached a provider session has nothing to resume, and
/// saying so is better than starting a fresh session under its row — which
/// would detach the row from the conversation it names.
#[test]
fn resuming_a_run_with_no_provider_session_is_refused() {
    let home = TempDir::new("resume-none");
    let repo = repo(home.path());
    let bin = stub_agent(home.path(), "waits", WAITS_FOR_STDIN);
    let run = RunId::from("run_sessionless");
    let db = home.path().join("log.sqlite");

    let registry = registry();
    let (on_event, _rx) = events();
    let spec = spec(&repo, &bin, &run);
    registry
        .start(EventLog::open(&db).unwrap(), spec, on_event)
        .unwrap();
    registry.end(&run).unwrap();
    eventually("the run to be reaped", || !registry.is_live(&run));

    let log = EventLog::open(&db).unwrap();
    let mut row = log.run(&run).unwrap().expect("the run row");
    row.provider_session_id = None;

    let launch = LaunchSpec::new(&repo, driver::new_session_id(), Permission::DenyPrompts);
    let (on_event2, _rx2) = events();
    let err = registry
        .resume(
            EventLog::open(&db).unwrap(),
            &row,
            launch,
            "wake up",
            None,
            on_event2,
        )
        .expect_err("a run with no session cannot be resumed");
    assert!(
        err.to_string().contains("no provider session id"),
        "the refusal says why: {err}"
    );
    assert!(!registry.is_live(&run), "and it started no process");
}
/// The motivating failure, end to end: a run whose only account of itself was
/// on a channel nothing read.
///
/// Three assertions, and the first is the one that was missing for the whole
/// life of the `process.output` vocabulary entry — that anything produces one
/// at all.
///
/// The ordering is deliberate and is asserted rather than observed: the
/// explanation is written *before* the failure it explains, so
/// `run.failed` stays the last thing a run says.
#[test]
fn a_run_that_only_wrote_to_stderr_says_why() {
    let home = TempDir::new("stderr-says-why");
    let repo = repo(home.path());
    let bin = stub_agent(home.path(), "refuses-to-start", REFUSES_TO_START);
    let run = RunId::from("run_refused_launch");
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

    assert_eq!(
        drain_to_end(&rx),
        vec!["process.output", "run.failed"],
        "the explanation has to reach the live view too, not only the log"
    );
    eventually("the run to be reaped", || !registry.is_live(&run));

    let log = EventLog::open(&db).unwrap();
    assert_eq!(status(&log, &run), RunStatus::Failed);

    let stderr = log.events_of_type(&run, "process.output").unwrap();
    let [event] = &stderr[..] else {
        panic!("one stderr event for the run, got {}", stderr.len());
    };
    let Event::ProcessOutput(p) = &event.event else {
        panic!("process.output carries a ProcessOutput");
    };
    assert_eq!(p.stream, "stderr");
    assert!(
        p.text.contains("--fork-session"),
        "the line that said why is in the log verbatim: {:?}",
        p.text
    );

    let failed = log.events_of_type(&run, "run.failed").unwrap();
    assert!(
        event.seq < failed[0].seq,
        "the explanation is ordered before the failure it explains"
    );
}

/// The other half of the policy, which is the half that keeps the log small: a
/// run the log can already account for keeps its stderr out of it.
#[test]
fn a_run_the_log_explains_keeps_its_stderr_out_of_it() {
    let home = TempDir::new("stderr-noise");
    let repo = repo(home.path());
    let bin = stub_agent(home.path(), "noisy", ONE_TURN_NOISY);
    let run = RunId::from("run_noisy");
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
    let seen = drain_to_end(&rx);
    assert!(
        !seen.contains(&"process.output".to_owned()),
        "a deprecation warning on a run that streamed a full turn is noise: {seen:?}"
    );
    eventually("the run to be reaped", || !registry.is_live(&run));

    let log = EventLog::open(&db).unwrap();
    assert!(log
        .events_of_type(&run, "process.output")
        .unwrap()
        .is_empty());
}

/// A chatty or debug-enabled run must not decide how much of the log, or of
/// our memory, it takes.
///
/// Both bounds at once, because they are different bounds: a tail of N lines
/// is only bounded if a line is, and `-d/--debug` produces both shapes.
#[test]
fn stderr_is_bounded_in_both_directions_the_agent_controls() {
    let home = TempDir::new("stderr-bounded");
    let repo = repo(home.path());
    let bin = stub_agent(home.path(), "shouts", SHOUTS_THEN_FAILS);
    let run = RunId::from("run_shouts");
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
    drain_to_end(&rx);
    eventually("the run to be reaped", || !registry.is_live(&run));

    let log = EventLog::open(&db).unwrap();
    let stderr = log.events_of_type(&run, "process.output").unwrap();
    let [event] = &stderr[..] else {
        panic!("one stderr event, however much was written");
    };
    let Event::ProcessOutput(p) = &event.event else {
        panic!("process.output carries a ProcessOutput");
    };
    assert!(
        p.text.len() < 32 * 1024,
        "200 KB of stderr became {} bytes in the log",
        p.text.len()
    );
    assert!(
        p.text.contains("earlier stderr lines dropped"),
        "a tail presented as the whole channel is a lie: {:?}",
        &p.text[..p.text.len().min(120)]
    );
    assert!(
        !p.text.contains("line 1\n"),
        "the head is what a tail drops"
    );
    assert!(
        p.text.contains("line 500"),
        "and the last thing said before the exit is what it keeps"
    );
    assert!(
        p.text.trim_end().ends_with('…'),
        "the 200 KB line is clipped, and says so"
    );
}
