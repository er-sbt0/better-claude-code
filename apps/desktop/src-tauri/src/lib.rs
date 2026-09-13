//! The desktop shell: the Rust side of the webview boundary (§4, §22).
//!
//! This crate owns no logic of its own. The spine is five crates below it —
//! `normalizer` knows the wire format, `event-log` the storage, `worktree` git,
//! `driver` the process — and everything here is the translation of that spine
//! into what a webview can call and be told. Anything in this file that starts
//! deciding something belongs in a crate instead, where a test can reach it
//! without a window. The run registry is the worked example: it decides when a
//! run is over and what happens to live runs at exit, so it lives in `driver`
//! and every command below is three lines of translation over it.
//!
//! Two obligations here are real rather than scaffolding.
//!
//! **The app reconciles orphaned runs before it shows anything** (§8). Every run
//! the log still calls `Running` at startup is a run whose process died with the
//! last session, and the log cannot work that out on its own — it has no
//! processes. Doing it in `setup`, not lazily in a command, is what guarantees
//! the UI never renders a run that looks alive and is not.
//!
//! **The app ends every live run before it exits.** A run started here holds a
//! live `claude` process until it is ended on purpose (see
//! [`driver::Registry`]), so an exit that skipped this would leave one agent per
//! run running on the machine — every `tauri dev` reload, forever.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use agent_protocol::{RawEvent, RunId};
use driver::{LaunchSpec, Permission, Registry, RunSpec};
use event_log::{EventLog, Run, RunActivity, RunMetrics};
use normalizer::{Clock, SystemClock};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State};
use ts_rs::TS;

/// The one channel every live run's events arrive on, for every run.
///
/// **One global channel, not `run:<id>:event`.** Two reasons, and the second is
/// the one that decides it:
///
/// 1. A [`RawEvent`](agent_protocol::RawEvent) already carries `run_id`, so a
///    per-run channel name would encode in the topic what the payload states —
///    the same fact stored twice, which §8 rejects everywhere else.
/// 2. The webview's `listen()` is **asynchronous**: it is itself an IPC call,
///    and the subscription is not live until the host has processed it. A
///    per-run topic cannot be known before [`start_run`] returns, by which time
///    the supervisor thread is already emitting — so subscribing to it always
///    races the first events of the run it is about. A channel that exists
///    before any run does is subscribed once, on mount, and cannot lose them.
///
/// The cost is that the frontend filters by `run_id`, which is one comparison
/// per event. The trade is recorded here rather than rediscovered.
pub const RUN_EVENT: &str = "run:event";

/// What starting up found and fixed.
///
/// `abandoned` is not a diagnostic: §8 says a user who left three runs going is
/// entitled to be told they are gone, rather than to find them quietly
/// finished. It is on this struct so the UI has to receive it.
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
pub struct BootReport {
    /// Absolute path to the SQLite log this session opened.
    pub log_path: String,
    /// Runs the log still called `Running`, now marked `Abandoned`.
    pub abandoned: Vec<RunId>,
}

/// What the UI has to say to start a run.
///
/// Deliberately short of a permission policy. §12's V1 surface is a dialog and
/// the `Decider` behind it does not exist, so a run started from here hosts no
/// workspace MCP server and nothing may prompt: every tool is either in
/// `allowed_tools` up front or denied outright. That is an honest run rather
/// than the product, and it is why this struct has no field that would suggest
/// otherwise.
#[derive(Debug, Clone, Deserialize, TS)]
#[ts(export)]
pub struct StartRunRequest {
    /// The user's repository. The run gets a worktree of its own (§13); this is
    /// never the agent's cwd.
    pub repo: String,
    pub prompt: String,
    pub model: Option<String>,
    /// Pre-granted tools, e.g. `Read`, `Edit`. Anything absent is denied.
    pub allowed_tools: Vec<String>,
    pub max_turns: Option<u32>,
    pub max_budget_usd: Option<f64>,
}

/// Where a started run landed.
///
/// All of it is in the run's row as well. It comes back on the call because the
/// UI has just started the run and should not have to query for what it just
/// caused.
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
pub struct StartedRun {
    pub run_id: RunId,
    pub branch: String,
    /// The agent's cwd. Never the user's own checkout, and never absent: §13
    /// creates a run with its worktree, forks included.
    pub worktree_path: String,
    /// `refs/agent-workspace/runs/<id>`: what Review attributes against (§13.1).
    ///
    /// Always present on a run that has just started. [`Run::shadow_ref`] on the
    /// stored row is nullable for the other case — a ref that is gone — where
    /// §13.1 requires Review to report attribution as unavailable rather than
    /// omit it.
    pub shadow_ref: String,
    pub started_at: String,
}

pub struct AppState {
    /// One connection, serialized. The log is single-writer by design (§14);
    /// when a live run needs its own handle, the driver takes one — this is
    /// the UI's, not the runtime's.
    pub log: Mutex<EventLog>,
    pub boot: BootReport,
    /// The runs this process is holding open, and the threads that end them.
    pub runs: Registry,
}

#[tauri::command]
fn boot_report(state: State<'_, AppState>) -> BootReport {
    state.boot.clone()
}

/// Runs that are alive *now* — after reconciliation, so this is honest.
///
/// Narrow on purpose. The surfaces that draw runs want [`recent_runs`]; this
/// one answers "what is in flight", which is a status-bar question, and the
/// two must not be collapsed into one call that serves neither.
#[tauri::command]
fn running_runs(state: State<'_, AppState>) -> Result<Vec<Run>, String> {
    state
        .log
        .lock()
        .map_err(|_| "event log lock poisoned".to_owned())?
        .running_runs()
        .map_err(|e| e.to_string())
}

/// Every run the log has, newest first — what the graph (§10) is drawn from.
///
/// The limit is the caller's because the shell decides how much history it is
/// willing to lay out, and an unbounded query behind a surface that draws a
/// node per row is a cliff nobody sees until the log is a year old.
#[tauri::command]
fn recent_runs(state: State<'_, AppState>, limit: u32) -> Result<Vec<Run>, String> {
    state
        .log
        .lock()
        .map_err(|_| "event log lock poisoned".to_owned())?
        .recent_runs(limit)
        .map_err(|e| e.to_string())
}

/// One run's events, from the log.
///
/// The live channel ([`RUN_EVENT`]) carries a run's events only while this
/// process is supervising it, so it is the *only* source for a run started in
/// an earlier session — and selecting such a run without this command shows an
/// empty stream that looks like a bug in the feed rather than an absent
/// subscription.
#[tauri::command]
fn run_events(state: State<'_, AppState>, run_id: RunId) -> Result<Vec<RawEvent>, String> {
    state
        .log
        .lock()
        .map_err(|_| "event log lock poisoned".to_owned())?
        .events(&run_id)
        .map_err(|e| e.to_string())
}

/// Cost, tokens and turns for one run.
///
/// §14 and [`RunMetrics`]: these are a projection recomputed from
/// `turn.finished`, never columns on the run row, so this is a query and not a
/// field the UI can read off [`Run`].
#[tauri::command]
fn run_metrics(state: State<'_, AppState>, run_id: RunId) -> Result<RunMetrics, String> {
    state
        .log
        .lock()
        .map_err(|_| "event log lock poisoned".to_owned())?
        .metrics(&run_id)
        .map_err(|e| e.to_string())
}

/// The three-word status and the message count, per run, for the graph's
/// headers.
///
/// A projection recomputed from the log ([`EventLog::recent_activity`]) and not
/// a field on [`Run`], for the same reason [`run_metrics`] is a command: **needs
/// you** is a `permission.requested` with no matching `permission.resolved`,
/// and a message count is not an event count. Both are derivations, and the
/// rule at the top of this file is that a derivation belongs in a crate where a
/// test can reach it without a window — so what the shell receives is three
/// words to colour.
///
/// The limit is the caller's and means what it means on [`recent_runs`]: the
/// same window, so every node the graph drew has a header.
#[tauri::command]
fn run_activity(state: State<'_, AppState>, limit: u32) -> Result<Vec<RunActivity>, String> {
    state
        .log
        .lock()
        .map_err(|_| "event log lock poisoned".to_owned())?
        .recent_activity(limit)
        .map_err(|e| e.to_string())
}

/// The runs *this process* is supervising.
///
/// Not the same question as [`running_runs`], which asks the log. These two
/// agree only because the app reconciles at boot and reaps at exit; the log's
/// answer is about rows and this one is about processes.
#[tauri::command]
fn live_runs(state: State<'_, AppState>) -> Vec<RunId> {
    state.runs.live()
}

/// Start a run and hand it to the registry.
///
/// The one thing worth reading twice: `EventLog::open` here is a **second**
/// connection to the same file. `driver::start` takes an `EventLog` by value
/// and gives it back only from `Session::wait()`, so lending the UI's would
/// mean the UI could not read while a run was writing — WAL and the two
/// connections are what make a live feed possible at all.
#[tauri::command]
fn start_run(
    app: AppHandle,
    state: State<'_, AppState>,
    request: StartRunRequest,
) -> Result<StartedRun, String> {
    let repo = PathBuf::from(&request.repo)
        .canonicalize()
        .map_err(|e| format!("{}: {e}", request.repo))?;
    let log = EventLog::open(&state.boot.log_path).map_err(|e| e.to_string())?;

    let run_id = driver::new_run_id();
    let mut launch = LaunchSpec::new(&repo, driver::new_session_id(), Permission::DenyPrompts);
    launch.model = request.model;
    launch.allowed_tools = request.allowed_tools;
    launch.max_turns = request.max_turns;
    launch.max_budget_usd = request.max_budget_usd;

    let spec = RunSpec {
        run_id: run_id.clone(),
        repo,
        branch: format!("agent/{}", run_id.0),
        base: "HEAD".to_owned(),
        prompt: request.prompt,
        post_create_hook: None,
        launch,
        // §12 option A needs a `Decider` that is a person, and there is not one
        // yet. Without the server nothing can prompt, which is the only honest
        // posture until there is.
        workspace_mcp: None,
    };

    let live = state
        .runs
        .start(log, spec, move |event| {
            // The live feed, and the whole of it: one `emit` per event, inline
            // on the run's supervisor thread, in stream order.
            //
            // Inline is the load-bearing word. §8's standing trap is that
            // *events produced at process exit must reach the live view, not
            // just the log* — `run.finished` is not a line of stdout, it is
            // synthesized once the process has been waited for. The supervisor
            // delivers it here as the last call it makes before reaping, so any
            // queue, channel or thread of our own between this closure and the
            // webview would be a thing that can still hold the tail when the
            // run is already gone. There is none.
            //
            // An emit can fail — the window can be closing while a run is still
            // talking — and there is nothing useful to do about it: the run is
            // already running and the log already has the event. Say so on
            // stderr rather than swallow it, because a live view that silently
            // stops updating is the symptom this would produce.
            if let Err(e) = app.emit(RUN_EVENT, &event) {
                eprintln!(
                    "{}: {} seq {} never reached the webview: {e}",
                    event.run_id.0,
                    event.type_name(),
                    event.seq
                );
            }
        })
        .map_err(|e| e.to_string())?;

    Ok(StartedRun {
        run_id: live.run_id,
        branch: live.branch,
        worktree_path: live.worktree_path.display().to_string(),
        shadow_ref: live.shadow_ref,
        started_at: live.started_at,
    })
}

/// A follow-up turn, through the process that is already running (§5).
#[tauri::command]
fn send_turn(state: State<'_, AppState>, run_id: RunId, text: String) -> Result<(), String> {
    state.runs.send(&run_id, &text).map_err(|e| e.to_string())
}

/// End a run: close its stdin, and let the agent finish the turn it is in.
///
/// Nothing else ends a run. The agent waits for another turn until stdin
/// closes, and this shell — unlike `driver/examples/run.rs`, which closes on
/// every `turn.finished` and is single-turn as a result — never closes it on
/// the user's behalf.
#[tauri::command]
fn end_run(state: State<'_, AppState>, run_id: RunId) -> Result<(), String> {
    state.runs.end(&run_id).map_err(|e| e.to_string())
}

/// Stop a run now. Everything already logged stays logged (§8).
#[tauri::command]
fn interrupt_run(state: State<'_, AppState>, run_id: RunId) -> Result<(), String> {
    state.runs.interrupt(&run_id).map_err(|e| e.to_string())
}

pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            let dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&dir)?;
            let path = dir.join("log.sqlite");

            let mut log = EventLog::open(&path)?;
            let abandoned = driver::reconcile_after_restart(&mut log, &SystemClock.now())?;

            app.manage(AppState {
                boot: BootReport {
                    log_path: path.display().to_string(),
                    abandoned,
                },
                log: Mutex::new(log),
                // §8: timestamps come from our runtime, and this is the one
                // place the app names which clock that is.
                runs: Registry::with_clock(Arc::new(SystemClock)),
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            boot_report,
            running_runs,
            recent_runs,
            run_events,
            run_metrics,
            run_activity,
            live_runs,
            start_run,
            send_turn,
            end_run,
            interrupt_run
        ])
        .build(tauri::generate_context!())
        .expect("failed to start the Agent Workspace window")
        // `build` + this loop rather than `run`, for the sake of one event:
        // without it, every live run outlives the window as an orphaned
        // `claude` process, because nothing in this app closes an agent's stdin
        // on its own. Interrupt rather than end: the app is going away, and
        // waiting for each agent to finish its turn would be a hang.
        .run(|app, event| {
            if let tauri::RunEvent::Exit = event {
                app.state::<AppState>().runs.shutdown();
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_protocol::{Event, RawEvent, TurnStarted, SCHEMA_VERSION};

    /// The three fields the live view is built on, at the top level of the
    /// payload the webview receives.
    ///
    /// Nothing here wraps the event before emitting it, and this is the test
    /// that says so: the frontend filters on `run_id`, keys on `seq` and
    /// switches on `type`, and an envelope added between this crate and
    /// `app.emit` would leave every one of those reading `undefined` — a live
    /// view that renders nothing, with no error anywhere.
    #[test]
    fn the_emitted_payload_is_the_raw_event_itself() {
        let event = RawEvent {
            run_id: RunId::from("run_1842"),
            seq: 7,
            ts: "2026-09-13T14:22:08.412Z".to_owned(),
            schema: SCHEMA_VERSION,
            idempotency_key: None,
            event: Event::TurnStarted(TurnStarted { index: 0 }),
        };
        let v = serde_json::to_value(&event).unwrap();
        assert_eq!(v["run_id"], serde_json::json!("run_1842"));
        assert_eq!(v["seq"], serde_json::json!(7));
        assert_eq!(v["type"], serde_json::json!("turn.started"));
    }

    /// Both halves of the channel name, checked against each other.
    ///
    /// The topic is a string on each side of the webview boundary and nothing
    /// generates it, so it is the one part of this bridge that can drift
    /// silently: a renamed constant here and a live view that simply never
    /// updates, with the run itself working perfectly.
    #[test]
    fn the_frontend_listens_on_the_channel_this_crate_emits() {
        let ipc = include_str!("../../src/lib/ipc.ts");
        assert!(
            ipc.contains(&format!("'{RUN_EVENT}'")),
            "apps/desktop/src/lib/ipc.ts does not listen on `{RUN_EVENT}`"
        );
    }
}
