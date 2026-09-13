//! The desktop shell: the Rust side of the webview boundary (§4, §22).
//!
//! This crate owns no logic of its own. The spine is five crates below it —
//! `normalizer` knows the wire format, `event-log` the storage, `worktree` git,
//! `driver` the process — and everything here is the translation of that spine
//! into what a webview can call and be told. Anything in this file that starts
//! deciding something belongs in a crate instead, where a test can reach it
//! without a window.
//!
//! One obligation is already real rather than scaffolding: **the app reconciles
//! orphaned runs before it shows anything** (§8). Every run the log still calls
//! `Running` at startup is a run whose process died with the last session, and
//! the log cannot work that out on its own — it has no processes. Doing it in
//! `setup`, not lazily in a command, is what guarantees the UI never renders a
//! run that looks alive and is not.

use std::sync::Mutex;

use agent_protocol::RunId;
use event_log::{EventLog, Run};
use normalizer::{Clock, SystemClock};
use serde::Serialize;
use tauri::{Manager, State};
use ts_rs::TS;

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

pub struct AppState {
    /// One connection, serialized. The log is single-writer by design (§14);
    /// when a live run needs its own handle, the driver takes one — this is
    /// the UI's, not the runtime's.
    pub log: Mutex<EventLog>,
    pub boot: BootReport,
}

#[tauri::command]
fn boot_report(state: State<'_, AppState>) -> BootReport {
    state.boot.clone()
}

/// Runs that are alive *now* — after reconciliation, so this is honest.
///
/// The Runs home screen (§10) needs every run, not just the live ones, and
/// `EventLog` has no query for that yet. That is the next thing to add, and
/// this command is deliberately the narrow version rather than a stub of the
/// wide one.
#[tauri::command]
fn running_runs(state: State<'_, AppState>) -> Result<Vec<Run>, String> {
    state
        .log
        .lock()
        .map_err(|_| "event log lock poisoned".to_owned())?
        .running_runs()
        .map_err(|e| e.to_string())
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
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![boot_report, running_runs])
        .run(tauri::generate_context!())
        .expect("failed to start the Agent Workspace window");
}
