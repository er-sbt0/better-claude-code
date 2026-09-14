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
use document_model::Feed;
use driver::{ForkPoint, ForkSpec, LaunchSpec, Permission, Registry, RunSpec};
use event_log::{EventLog, ForkPoints, Run, RunActivity, RunMetrics};
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

/// What the UI has to say to fork one (§19, `docs/branching-surface.md`).
///
/// Two fields are the fork and the rest are the launch. `parent` and `seq` name
/// the block the branch is anchored at — a `seq` in the **parent's** log, which
/// [`ForkPoint::resolve`] turns into the chain-entry uuid
/// `--resume-session-at` takes, refusing rather than snapping to a neighbour if
/// that block carries no address.
///
/// **Why the launch flags are not here.** They used to be, and the reason was a
/// hole in the schema rather than a decision: the `runs` row stored only the
/// model, so a fork could pre-grant nothing the caller did not pass, and this
/// struct carried an allow-list the window had to remember for itself — which
/// it could only do for runs *this session* started. Schema V4 stores what a
/// run was launched with, `driver::fork` inherits it, and so the fields are
/// gone rather than left as a second way to say the same thing. A fork inherits
/// the parent's policy and bounds; the model stays, because
/// `docs/branching-surface.md` gives the header a model to edit and that is the
/// one thing a branch is meant to differ in.
#[derive(Debug, Clone, Deserialize, TS)]
#[ts(export)]
pub struct ForkRunRequest {
    /// The run being branched.
    pub parent: RunId,
    /// The branch point: a `seq` in the parent's log.
    #[ts(type = "number")]
    pub seq: u64,
    /// The first thing said to the child. A fork begins with the user typing
    /// into the branch — that is the whole reason a node shows its messages.
    pub prompt: String,
    /// `None` inherits the parent's, which is what a fork means; naming one is
    /// how a branch comes to differ from its parent (`driver::fork`).
    pub model: Option<String>,
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

/// What [`clean_up_run`] did, or why it did nothing.
///
/// A three-way answer rather than `Result<(), String>`, because one of the
/// three is a question for the user and not a failure. `driver::Cleanup` is
/// what decides which; this is that enum translated for the webview, and it is
/// translated rather than re-exported because `driver` has no business
/// carrying `serde` and `ts-rs` for the sake of one window (§4).
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum CleanUpReport {
    /// The tree and the `agent/<run_id>` branch are gone, and the run row no
    /// longer names a tree.
    ///
    /// `refs/agent-workspace/runs/<run_id>` is untouched and is not reported
    /// here for the same reason it is not removed: nothing happened to it. The
    /// run is still forkable and Review still has its attribution (§13.1).
    Removed { worktree_path: String },
    /// There was nothing to remove. A second click, or a tree someone deleted
    /// by hand.
    AlreadyGone,
    /// Refused: the tree has uncommitted changes.
    ///
    /// §13 — "never delete a worktree with uncommitted changes without
    /// confirmation" — and this is where the confirmation is asked for. The
    /// caller shows the path, asks, and calls again with `force` if the user
    /// says yes. Deciding *that* a confirmation is owed is the driver's; asking
    /// is the window's, because a window is the only thing here with a user in
    /// front of it.
    Dirty { worktree_path: String },
}

impl From<driver::Cleanup> for CleanUpReport {
    fn from(c: driver::Cleanup) -> Self {
        match c {
            driver::Cleanup::Removed {
                worktree_path,
                branch: _,
            } => CleanUpReport::Removed {
                worktree_path: worktree_path.display().to_string(),
            },
            driver::Cleanup::AlreadyGone => CleanUpReport::AlreadyGone,
            driver::Cleanup::Dirty { worktree_path } => CleanUpReport::Dirty {
                worktree_path: worktree_path.display().to_string(),
            },
        }
    }
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

/// One run's events, from the log — the unprocessed transcript itself.
///
/// The live channel ([`RUN_EVENT`]) carries a run's events only while this
/// process is supervising it, so this is the *only* source for a run started in
/// an earlier session.
///
/// The node body no longer calls it: [`run_feed`] is what a column renders, and
/// asking for both would send the same run over the boundary twice. It stays
/// because §7's invariant asks for more than "nothing is dropped" — "the
/// unprocessed transcript is always one click away" — and this is the click.
#[tauri::command]
fn run_events(state: State<'_, AppState>, run_id: RunId) -> Result<Vec<RawEvent>, String> {
    state
        .log
        .lock()
        .map_err(|_| "event log lock poisoned".to_owned())?
        .events(&run_id)
        .map_err(|e| e.to_string())
}

/// §7's feed for one run: six block types plus `Raw`, in stream order.
///
/// A projection recomputed from the log ([`document_model::project`]), like
/// [`run_metrics`] and [`run_activity`] and for the same reason, which here is
/// the load-bearing one: **deriving is deciding**. Several events fan into one
/// `ToolCall` block, one `assistant` line fans into a `Text` and a `Code` block,
/// and a permission decision belongs to the call it refused. Every one of those
/// is a decision, so it lives in a crate a test can reach without a window and
/// the webview receives blocks it only has to draw.
///
/// **Why the whole feed rather than a delta.** The webview already holds the
/// run's events — they arrive on [`RUN_EVENT`] — so projecting there would cost
/// no round trip, and that is exactly the trade the rule above refuses. What it
/// costs instead is re-reading the log and re-projecting it on each call, and
/// that buys something a delta would not: a feed that stopped rendering while
/// the camera was elsewhere rejoins the stream by calling this again. There is
/// no catch-up to get wrong, because there is no delta to miss.
#[tauri::command]
fn run_feed(state: State<'_, AppState>, run_id: RunId) -> Result<Feed, String> {
    let events = state
        .log
        .lock()
        .map_err(|_| "event log lock poisoned".to_owned())?
        .events(&run_id)
        .map_err(|e| e.to_string())?;
    Ok(document_model::project(&run_id, &events))
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
        .start(log, spec, emit_events(app))
        .map_err(|e| e.to_string())?;

    Ok(StartedRun {
        run_id: live.run_id,
        branch: live.branch,
        worktree_path: live.worktree_path.display().to_string(),
        shadow_ref: live.shadow_ref,
        started_at: live.started_at,
    })
}

/// Which blocks of these runs can be forked at, and what a fork there inherits.
///
/// A projection ([`EventLog::fork_points`]), and a command for exactly the
/// reason [`run_activity`] is one: **forkability is a decision and the shell
/// owns no logic**. The rule — strip the key at the first `#`, an absent or
/// empty key is not an address — is
/// `normalizer::claude_code::fork_address`, one function with live runs behind
/// it (`docs/verification/claude-code-fork-address.md`). `RawEvent`'s
/// `idempotency_key` reaches the webview, so re-deriving it there would take
/// ten lines and no round trip; it would also be that rule in a second
/// language, out of reach of every test in this workspace, drifting towards
/// offering forks [`fork_run`] then refuses.
///
/// By name rather than by window, unlike [`run_activity`]: the answer is per
/// *block*, so the caller asks for the runs it can actually fork in — the one
/// being read, the ones it is supervising, and the parents of the children on
/// screen.
#[tauri::command]
fn fork_points(state: State<'_, AppState>, run_ids: Vec<RunId>) -> Result<Vec<ForkPoints>, String> {
    state
        .log
        .lock()
        .map_err(|_| "event log lock poisoned".to_owned())?
        .fork_points(&run_ids)
        .map_err(|e| e.to_string())
}

/// Fork a run at a block, and supervise the child like any other run.
///
/// Three lines of translation over [`Registry::fork`], which is where every
/// decision in a fork lives: the branch point is resolved against the parent's
/// log, the child's worktree is checked out from the parent's shadow commit at
/// that `seq`, and both halves of the graph edge are written to the child's row
/// (§19 — the provider records no parentage, so a fork we fail to store is a
/// fork that never happened).
///
/// Two things are decided here because there is nowhere else for them, and both
/// are the shell choosing the *absence* of a behaviour rather than inventing
/// one:
///
/// - **The child's branch is `agent/<child id>`,** exactly as [`start_run`]
///   names a root run's. Two worktrees cannot share a branch and §13.1's
///   history is per run, so a fork cannot reuse the parent's.
/// - **No post-create hook.** [`ForkSpec::post_create_hook`] is a live question
///   the branching record does not answer, and it is a real one: a shadow
///   commit respects `.gitignore`, so the child's checkout has none of the
///   parent's `node_modules` or `target` even though the parent has them on
///   disk. `None` is chosen because the hook is arbitrary user code and this
///   app has no configuration surface for one — [`StartRunRequest`] has no such
///   field either, so every run this window starts already runs none. A fork
///   that ran a command its parent never ran would be the shell deciding what
///   builds the user's repository, which is the one thing the rule at the top
///   of this file forbids. The consequence is stated rather than hidden: a
///   child's tree holds the parent's *tracked* files at the branch point and
///   nothing that `.gitignore` covers, so a fork of a project with a build step
///   starts from a clean checkout. When [`StartRunRequest`] grows a hook, a
///   fork inherits it here and this comment goes away.
#[tauri::command]
fn fork_run(
    app: AppHandle,
    state: State<'_, AppState>,
    request: ForkRunRequest,
) -> Result<StartedRun, String> {
    // Resolved through the UI's connection, before a second one is opened:
    // an unaddressable block is a refusal, and a refusal should cost no run.
    let at = {
        let log = state
            .log
            .lock()
            .map_err(|_| "event log lock poisoned".to_owned())?;
        ForkPoint::resolve(&log, &request.parent, request.seq).map_err(|e| e.to_string())?
    };

    let log = EventLog::open(&state.boot.log_path).map_err(|e| e.to_string())?;
    let run_id = driver::new_run_id();
    // `cwd`, `resume` and `fork_at` are all overwritten by `driver::fork` —
    // the first because §13 does not leave a working directory to a caller, the
    // other two because they *are* the fork. The repo path here is a
    // placeholder for that reason; the fork reads the real one off the parent.
    //
    // Everything else is left unset, and that is the whole of the change: the
    // parent's row now carries what it was launched with, so `driver::fork`
    // fills in the allow-list and the bounds from there rather than from
    // whatever this window happened to remember. A model is passed only when
    // the caller named one.
    let mut launch = LaunchSpec::new(
        PathBuf::from(&request.parent.0),
        driver::new_session_id(),
        Permission::DenyPrompts,
    );
    launch.model = request.model;

    let spec = ForkSpec {
        run_id: run_id.clone(),
        at,
        branch: format!("agent/{}", run_id.0),
        post_create_hook: None,
        prompt: request.prompt,
        launch,
        // As on `start_run`, and for the same reason: §12 option A needs a
        // `Decider` that is a person and there is not one yet.
        workspace_mcp: None,
    };

    let live = state
        .runs
        .fork(log, spec, emit_events(app))
        .map_err(|e| e.to_string())?;

    Ok(StartedRun {
        run_id: live.run_id,
        branch: live.branch,
        worktree_path: live.worktree_path.display().to_string(),
        shadow_ref: live.shadow_ref,
        started_at: live.started_at,
    })
}

/// A follow-up turn — through the running process, or through a new one.
///
/// **A conversation outlives the window it was started in.** Nothing closes a
/// run's stdin on its behalf, so every run dies when the app exits, and a
/// composer that could only reach runs this process happens to be holding was
/// dead on every run from any previous session — which is to say, on almost
/// every run a returning user sees. §14 is what makes the other path possible:
/// the provider owns the session, we *assign* its id and keep it on the row, so
/// a run we are not holding is asleep rather than gone.
///
/// So the fall-back is not an error path, it is the ordinary one: send to the
/// live process if there is one, and otherwise resume the session and send the
/// turn as the prompt that wakes it. The run keeps its id, its row, its
/// worktree and its shadow history, because [`driver::Registry::resume`] takes
/// all four off the row rather than inventing them.
#[tauri::command]
fn send_turn(
    app: AppHandle,
    state: State<'_, AppState>,
    run_id: RunId,
    text: String,
) -> Result<(), String> {
    if state.runs.is_live(&run_id) {
        return state.runs.send(&run_id, &text).map_err(|e| e.to_string());
    }

    let log = EventLog::open(&state.boot.log_path).map_err(|e| e.to_string())?;
    let run = log
        .run(&run_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("{} is not in the log", run_id.0))?;

    // Its own connection, for the same reason `start_run` opens one: the
    // driver takes an `EventLog` by value and only gives it back from
    // `Session::wait()`, so the UI's connection cannot be lent to a run.
    let write = EventLog::open(&state.boot.log_path).map_err(|e| e.to_string())?;
    let session_id = run
        .provider_session_id
        .clone()
        .ok_or_else(|| format!("{} has no provider session to resume", run_id.0))?;
    // Nothing but the session id: `driver::resume` takes the cwd, the model and
    // the rest of the launch off the row, which is where schema V4 put them. A
    // woken run is the run it was, under the policy it was having the
    // conversation under.
    let launch = LaunchSpec::new(
        PathBuf::from(run.worktree_path.as_deref().unwrap_or(&run.workspace_path)),
        session_id,
        Permission::DenyPrompts,
    );

    state
        .runs
        .resume(write, &run, launch, &text, None, emit_events(app))
        .map(|_| ())
        .map_err(|e| e.to_string())
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

/// Take a finished run's worktree and its `agent/<run_id>` branch away again
/// (§13).
///
/// Three lines of translation, as the rule at the top of this file requires,
/// and every one of the decisions is behind them in [`driver::Registry::clean_up`]:
/// a run this process is still supervising is refused outright, a tree with
/// uncommitted changes comes back as [`CleanUpReport::Dirty`] until `force`
/// says the user confirmed, and `refs/agent-workspace/runs/<run_id>` is kept so
/// the run stays forkable and reviewable with its tree gone. None of that is
/// checked here, and the liveness check in particular is not re-implemented
/// here: `state.runs.is_live` is right there, and a second copy of the rule in
/// this file is a second rule to keep in step with the first.
///
/// This is the **only** thing in the app that removes anything, and nothing
/// calls it but a person. Until it is pressed, every run's tree and branch stay
/// in the user's own repository — which is a cost stated out loud rather than
/// traded away by a sweep that would have to delete uncommitted work without
/// asking.
///
/// The UI's connection, not a second one: unlike [`start_run`] there is no
/// process to lend a log to, and holding the lock across a `git worktree
/// remove` is the honest thing to do anyway — it is one command on a directory
/// nothing live is reading.
#[tauri::command]
fn clean_up_run(
    state: State<'_, AppState>,
    run_id: RunId,
    force: bool,
) -> Result<CleanUpReport, String> {
    let mut log = state
        .log
        .lock()
        .map_err(|_| "event log lock poisoned".to_owned())?;
    state
        .runs
        .clean_up(&mut log, &run_id, force)
        .map(CleanUpReport::from)
        .map_err(|e| e.to_string())
}

/// The live feed, and the whole of it: one `emit` per event, inline on the
/// run's supervisor thread, in stream order.
///
/// Inline is the load-bearing word. §8's standing trap is that *events produced
/// at process exit must reach the live view, not just the log* —
/// `run.finished` is not a line of stdout, it is synthesized once the process
/// has been waited for. The supervisor delivers it here as the last call it
/// makes before reaping, so any queue, channel or thread of our own between
/// this closure and the webview would be a thing that can still hold the tail
/// when the run is already gone. There is none.
///
/// An emit can fail — the window can be closing while a run is still talking —
/// and there is nothing useful to do about it: the run is already running and
/// the log already has the event. Say so on stderr rather than swallow it,
/// because a live view that silently stops updating is the symptom this would
/// produce.
///
/// One function rather than a closure per command, because a fork's events are
/// a run's events: they arrive on the same channel, filtered by `run_id` like
/// every other run's, and a second copy of this would be a second place for the
/// tail to go missing.
fn emit_events(app: AppHandle) -> impl FnMut(RawEvent) + Send + 'static {
    move |event: RawEvent| {
        if let Err(e) = app.emit(RUN_EVENT, &event) {
            eprintln!(
                "{}: {} seq {} never reached the webview: {e}",
                event.run_id.0,
                event.type_name(),
                event.seq
            );
        }
    }
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
            run_feed,
            run_metrics,
            run_activity,
            fork_points,
            live_runs,
            start_run,
            fork_run,
            send_turn,
            end_run,
            interrupt_run,
            clean_up_run
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

    /// §7's degradation invariant, checked across the webview boundary.
    ///
    /// "An unknown `type` renders as `Raw` and is preserved verbatim." The Rust
    /// half is tested where it lives — `document-model`'s fixture suite asserts
    /// that an event with no block type becomes a `Raw` block with its payload
    /// intact. The half this checks is the one no test in this workspace can
    /// otherwise reach: the *renderer* must have a fallback for a block type it
    /// has never heard of, because a document projected by a newer backend than
    /// the bundle is an ordinary thing once §9.1 starts storing them.
    ///
    /// A string match, like the channel test below and for the same reason: the
    /// alternative is a TypeScript test runner this workspace does not have, and
    /// the failure this catches — a renderer refactored into an exhaustive
    /// switch with no default, which type-checks perfectly — is otherwise
    /// invisible until it renders nothing.
    #[test]
    fn the_renderer_keeps_a_fallback_for_a_block_type_it_does_not_know() {
        let blocks = include_str!("../../src/canvas/Blocks.tsx");
        assert!(
            blocks.contains("unknown block type"),
            "apps/desktop/src/canvas/Blocks.tsx has no fallback for an unknown \
             block type — §7's degradation invariant is not rendered"
        );
        assert!(
            blocks.contains("const unreachable: never = block"),
            "the exhaustiveness check is gone from Blocks.tsx, so a seventh \
             block type would silently fall through to the fallback instead of \
             failing the build"
        );
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
