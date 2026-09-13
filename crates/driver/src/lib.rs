//! The process driver: the rest of the headless spine.
//!
//! It owns the four things only the component that launches the process can
//! own, and each of them is a rule from the design rather than a convenience:
//!
//! - **The worktree comes first** (§13). A run is created *with* its worktree;
//!   the user's own checkout is never an agent's working directory.
//! - **The shadow history is rooted after the post-create hook and before the
//!   first tool call** (§13.1). Any earlier and every file the hook wrote is
//!   attributed to the run's first `Edit`.
//! - **Events are appended per provider line, in one transaction** (§8). A
//!   crash must never leave half a provider message in the log.
//! - **Every tool call that changed something is snapshotted** (§13.1). This is
//!   the requirement the attribution decision imposes on the driver, and the
//!   reason it had to be decided before this crate was written.
//!
//! What it deliberately does not do: interpret. Everything about the wire
//! format lives in `normalizer`, everything about storage in `event-log`, and
//! everything about git in `worktree`. This crate is the wiring, and
//! [`Pipeline`] is that wiring without the process — which is what lets a
//! recorded fixture test it.
//!
//! [`start`] hands the caller a live [`Session`] and forgets about it, which is
//! all a one-shot CLI needs. An app needs to find that session again, steer it
//! and reap it, so [`Registry`] owns those runs and the threads that end them.
//! It is in this crate rather than in the shell because it decides when a run
//! is over, and a decision has to be somewhere a test reaches without a window.

mod error;
mod launch;
mod permission;
mod pipeline;
mod process;
mod registry;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

use agent_protocol::{RawEvent, RunId};
use event_log::{EventLog, Run, RunStatus};
use worktree::{ShadowHistory, Worktree};

pub use error::{Error, Result};
pub use launch::{new_run_id, new_session_id, user_message, LaunchSpec, Permission, Thinking};
pub use permission::{Decider, DenyEverything, RunWorkspace};
pub use pipeline::{Ingested, Pipeline};
pub use process::{Outcome, Session};
pub use registry::{Live, Registry};

/// `seq` 0 is the run's state before anything happened.
///
/// It belongs to no event — the normalizer's first event is 1, and
/// `EventLog::last_seq` reports 0 for a run with none — so it is the honest
/// label for the root snapshot, which records the worktree as the agent found
/// it.
pub const SEQ_BEFORE_RUN: u64 = 0;

/// Everything needed to start a run, above the launch flags.
#[derive(Debug, Clone)]
pub struct RunSpec {
    pub run_id: RunId,
    /// The user's repository. Never the agent's cwd.
    pub repo: PathBuf,
    pub branch: String,
    /// Commit-ish the run branches from.
    pub base: String,
    pub prompt: String,
    /// §13: dependency install, codegen. Runs in the new worktree, before the
    /// agent starts, and its output is attributed to the run rather than to a
    /// tool call.
    pub post_create_hook: Option<Vec<String>>,
    /// Launch flags. `cwd` is overwritten with the worktree — §13 does not
    /// leave that to the caller. So are the four flags the workspace MCP
    /// server owns, when one is requested.
    pub launch: LaunchSpec,
    /// Inject our own MCP server into the run (§6 source 3, §12 option A).
    ///
    /// `None` leaves the launch exactly as the caller built it, which in
    /// practice means `Permission::DenyPrompts`: a run that can only be
    /// pre-authorized. That is honest, and it is not the product.
    pub workspace_mcp: Option<WorkspaceMcp>,
}

/// How to host the workspace MCP server for one run.
#[derive(Clone)]
pub struct WorkspaceMcp {
    /// Absolute path to the relay binary the provider will spawn.
    ///
    /// The provider starts stdio MCP servers as its own children, so the
    /// server cannot simply live in this process. The relay is a byte pipe from
    /// those pipes to our socket; all protocol logic stays in
    /// `workspace-mcp`, where a test reaches it without spawning anything.
    pub relay: PathBuf,
    /// Directory to put the run's socket in.
    ///
    /// **Not the worktree.** §15 treats everything the agent can write as
    /// untrusted, and the socket is the one channel whose integrity the
    /// permission model depends on. `$XDG_RUNTIME_DIR` is the right answer on
    /// Linux; see [`default_runtime_dir`].
    pub runtime_dir: PathBuf,
    /// Who answers a permission request.
    pub decider: Arc<dyn Decider>,
}

impl std::fmt::Debug for WorkspaceMcp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkspaceMcp")
            .field("relay", &self.relay)
            .field("runtime_dir", &self.runtime_dir)
            .finish_non_exhaustive()
    }
}

/// Where run sockets go when the caller has no opinion.
///
/// `$XDG_RUNTIME_DIR` is per-user, mode 0700 and on tmpfs, which is what a
/// socket carrying permission decisions wants. Falling back to the temp dir is
/// weaker — it is world-readable — so the socket's own directory is created
/// 0700 either way rather than relying on where it landed.
pub fn default_runtime_dir() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("agent-workspace")
}

/// A started run: its worktree, its shadow history, and the live process.
pub struct Started {
    pub session: Session,
    pub worktree: Worktree,
    pub shadow_ref: String,
}

/// Create the worktree, root the shadow history, record the run, and spawn.
///
/// The order is the contract. Each step depends on the one before it, and two
/// of the dependencies are not obvious: the hook has to run before the shadow
/// history is rooted, and the run row has to exist before the first event is
/// appended, because the log refuses events for a run it does not know.
pub fn start(
    mut log: EventLog,
    mut spec: RunSpec,
    started_at: &str,
    sink: Option<Sender<RawEvent>>,
) -> Result<Started> {
    let wt = Worktree::create(&spec.repo, &spec.run_id, &spec.branch, &spec.base)?;

    if let Some(hook) = &spec.post_create_hook {
        run_hook(wt.path(), hook)?;
    }

    let (shadow, _root) = ShadowHistory::begin(&wt, &spec.run_id, SEQ_BEFORE_RUN)?;
    let shadow_ref = shadow.ref_name().to_owned();

    log.create_run(&Run {
        id: spec.run_id.clone(),
        agent: normalizer::claude_code::AGENT_ID.to_owned(),
        agent_version: None,
        // §14: assigned, not captured. The run is resumable before the process
        // has said anything.
        provider_session_id: Some(spec.launch.session_id.clone()),
        workspace_path: spec.repo.display().to_string(),
        worktree_path: Some(wt.path().display().to_string()),
        branch: Some(spec.branch.clone()),
        shadow_ref: Some(shadow_ref.clone()),
        prompt: Some(spec.prompt.clone()),
        status: RunStatus::Running,
        started_at: started_at.to_owned(),
        finished_at: None,
        // A root run: no parent, therefore no branch point. The fork path that
        // fills both in is not built yet (§19).
        parent_run_id: None,
        branch_point_seq: None,
    })?;

    spec.launch.cwd = wt.path().to_path_buf();
    let pipeline = Arc::new(Mutex::new(Pipeline::new(
        spec.run_id.clone(),
        log,
        Some(shadow),
    )));
    let mcp = wire_workspace_mcp(
        &mut spec.launch,
        &spec.run_id,
        spec.workspace_mcp.as_ref(),
        &pipeline,
        sink.as_ref(),
    )?;
    let session = process::spawn(&spec.launch, pipeline, &spec.prompt, sink, mcp)?;

    Ok(Started {
        session,
        worktree: wt,
        shadow_ref,
    })
}

/// Resume a run the provider still holds.
///
/// The normalizer is seeded from the log; §14 explains why nothing in the
/// stream can tell us this is a resume.
pub fn resume(
    log: EventLog,
    run: &Run,
    mut launch: LaunchSpec,
    prompt: &str,
    sink: Option<Sender<RawEvent>>,
    workspace_mcp: Option<&WorkspaceMcp>,
) -> Result<Session> {
    let wt = Worktree::open(Path::new(
        run.worktree_path.as_deref().unwrap_or(&run.workspace_path),
    ))?;
    let shadow = ShadowHistory::open(&wt, &run.id)?;
    launch.cwd = wt.path().to_path_buf();
    launch.resume = run.provider_session_id.clone();
    let pipeline = Arc::new(Mutex::new(Pipeline::resuming(
        run.id.clone(),
        log,
        Some(shadow),
    )?));
    // A resumed run gets its own socket and its own listener. The server is
    // per-process, not per-run-row: the old one died with the old process, and
    // the provider will spawn a fresh relay for this one.
    let mcp = wire_workspace_mcp(
        &mut launch,
        &run.id,
        workspace_mcp,
        &pipeline,
        sink.as_ref(),
    )?;
    process::spawn(&launch, pipeline, prompt, sink, mcp)
}

/// Bind the run's MCP server and rewrite the flags that depend on it.
///
/// Four flags, and each is load-bearing:
///
/// - `--mcp-config` as inline JSON. The provider treats a value starting with
///   `{` as JSON and anything else as a path, so nothing here writes a file.
/// - `--permission-prompt-tool mcp__agent-workspace__request_permission`, which
///   is the whole of §12 option A.
/// - `mcp__agent-workspace__report_finding` on the allow-list. Verified the
///   hard way: without it, reporting a finding raises a permission request of
///   its own — the agent asking to be allowed to tell us something — so a
///   deny-by-default policy silently denies its own reporting channel.
///   `request_permission` is deliberately *not* allow-listed: the provider
///   invokes it directly, and the model has no business calling it.
/// - `--append-system-prompt`, because a tool the agent is never told about is
///   a tool it never calls (§6). Appended to whatever the caller set rather
///   than replacing it.
fn wire_workspace_mcp(
    launch: &mut LaunchSpec,
    run_id: &RunId,
    cfg: Option<&WorkspaceMcp>,
    pipeline: &Arc<Mutex<Pipeline>>,
    sink: Option<&Sender<RawEvent>>,
) -> Result<Option<workspace_mcp::Listener>> {
    let Some(cfg) = cfg else { return Ok(None) };

    let workspace = Arc::new(RunWorkspace::new(
        Arc::clone(pipeline),
        sink.cloned(),
        Arc::clone(&cfg.decider),
    ));
    let listener = workspace_mcp::Listener::bind(&cfg.runtime_dir.join(&run_id.0), workspace)?;

    launch.mcp_config = Some(listener.mcp_config(&cfg.relay));
    launch.permission = Permission::AskVia {
        tool: workspace_mcp::qualified(workspace_mcp::REQUEST_PERMISSION),
    };
    let report = workspace_mcp::qualified(workspace_mcp::REPORT_FINDING);
    if !launch.allowed_tools.contains(&report) {
        launch.allowed_tools.push(report);
    }
    launch.append_system_prompt = Some(match launch.append_system_prompt.take() {
        Some(existing) => format!("{existing}\n\n{}", workspace_mcp::SYSTEM_PROMPT),
        None => workspace_mcp::SYSTEM_PROMPT.to_owned(),
    });

    Ok(Some(listener))
}

/// Reconcile runs the log still calls running (§8's crash recovery).
///
/// Every one of them is a lie after a restart: no process of ours survived it.
/// Returns what was reconciled, because a user who left three runs going is
/// entitled to be told they are gone rather than to find them quietly finished.
pub fn reconcile_after_restart(log: &mut EventLog, now: &str) -> Result<Vec<RunId>> {
    let orphans = log.running_runs()?;
    let mut out = Vec::new();
    for run in orphans {
        log.finish_run(&run.id, RunStatus::Abandoned, now)?;
        out.push(run.id);
    }
    Ok(out)
}

fn run_hook(cwd: &Path, hook: &[String]) -> Result<()> {
    let Some((program, args)) = hook.split_first() else {
        return Ok(());
    };
    let out = Command::new(program)
        .args(args)
        .current_dir(cwd)
        .output()
        .map_err(|source| Error::Spawn {
            binary: program.clone(),
            source,
        })?;
    if !out.status.success() {
        return Err(Error::PostCreateHook {
            status: out.status.code(),
            stderr: String::from_utf8_lossy(&out.stderr).trim().to_owned(),
        });
    }
    Ok(())
}
