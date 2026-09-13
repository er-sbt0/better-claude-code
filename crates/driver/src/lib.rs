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

mod error;
mod launch;
mod pipeline;
mod process;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::Sender;

use agent_protocol::{RawEvent, RunId};
use event_log::{EventLog, Run, RunStatus};
use worktree::{ShadowHistory, Worktree};

pub use error::{Error, Result};
pub use launch::{new_session_id, user_message, LaunchSpec, Permission, Thinking};
pub use pipeline::{Ingested, Pipeline};
pub use process::{Outcome, Session};

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
    /// leave that to the caller.
    pub launch: LaunchSpec,
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
        parent_run_id: None,
    })?;

    spec.launch.cwd = wt.path().to_path_buf();
    let pipeline = Pipeline::new(spec.run_id.clone(), log, Some(shadow));
    let session = process::spawn(&spec.launch, pipeline, &spec.prompt, sink)?;

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
) -> Result<Session> {
    let wt = Worktree::open(Path::new(
        run.worktree_path.as_deref().unwrap_or(&run.workspace_path),
    ))?;
    let shadow = ShadowHistory::open(&wt, &run.id)?;
    launch.cwd = wt.path().to_path_buf();
    launch.resume = run.provider_session_id.clone();
    let pipeline = Pipeline::resuming(run.id.clone(), log, Some(shadow))?;
    process::spawn(&launch, pipeline, prompt, sink)
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
