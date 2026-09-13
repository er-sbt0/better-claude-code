//! The process driver: the rest of the headless spine.
//!
//! It owns the four things only the component that launches the process can
//! own, and each of them is a rule from the design rather than a convenience:
//!
//! - **A run is created *with* its worktree** (§13). Unconditionally, forks
//!   included. The user's own checkout is never an agent's working directory,
//!   and nothing an agent writes lands outside the tree its own run owns.
//!   `docs/branching-surface.md` briefly made this conditional — a tree only
//!   once a run writes — and reversed itself: a process's cwd is fixed at
//!   spawn, so a tree created later cannot redirect writes that are already
//!   addressed at the old root. What distinguishes a fork is not *whether* it
//!   has a tree but *what that tree is based on*: [`start`] branches from
//!   whatever the caller chose, [`fork`] from the parent's shadow commit at the
//!   branch point. [`ensure_tree`] is the one seam both go through.
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
mod sink;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

use agent_protocol::{RawEvent, RunId};
use event_log::{EventLog, Run, RunStatus};
use worktree::{shadow_ref, ShadowHistory, Worktree};

pub use error::{Error, Result};
pub use launch::{new_run_id, new_session_id, user_message, LaunchSpec, Permission, Thinking};
pub use permission::{Decider, DenyEverything, RunWorkspace};
pub use pipeline::{Ingested, Pipeline};
pub use process::{Outcome, Session};
pub use registry::{Live, Registry};
pub use sink::{EventSink, StreamEnd};

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

impl RunSpec {
    /// The git half of this spec, for [`ensure_tree`].
    pub fn tree_spec(&self) -> TreeSpec {
        TreeSpec {
            run_id: self.run_id.clone(),
            repo: self.repo.clone(),
            branch: self.branch.clone(),
            base: self.base.clone(),
            post_create_hook: self.post_create_hook.clone(),
        }
    }
}

/// Everything needed to fork a run, above the launch flags.
///
/// Note what is *not* here: a `repo` and a `base`. Both are read off the parent
/// — the repository because a fork works on the same one by definition, and the
/// base because it is the parent's shadow commit at [`ForkPoint::seq`], which
/// only the parent's history knows.
#[derive(Debug, Clone)]
pub struct ForkSpec {
    /// The child's run id. Ours, like every run id (§14).
    pub run_id: RunId,
    /// Where the branch is anchored, resolved by [`ForkPoint::resolve`].
    pub at: ForkPoint,
    /// The child's own git branch. A fork does not share the parent's: two
    /// worktrees cannot check out one branch, and §13.1's history is per run.
    pub branch: String,
    /// §13's post-create hook, for the child's tree.
    ///
    /// Left to the caller rather than inherited, and worth a thought either way:
    /// a shadow commit respects `.gitignore`, so the parent's `node_modules` and
    /// build output are *not* in the tree the child is checked out from even
    /// though the parent has them on disk. A fork of a real project usually
    /// wants the hook to run again.
    pub post_create_hook: Option<Vec<String>>,
    pub prompt: String,
    /// Launch flags. `cwd`, `resume` and `fork_at` are overwritten by [`fork`]
    /// — the first because §13 does not leave a working directory to the
    /// caller, the other two because they *are* the fork. `session_id` is the
    /// caller's: it names the child.
    pub launch: LaunchSpec,
    pub workspace_mcp: Option<WorkspaceMcp>,
}

/// A resolved branch point: a `seq` in the parent's log, and the provider
/// address that block carries.
///
/// Two identifiers for one moment, and both are needed. The `seq` is ours and
/// is what the graph edge is stored as; the address is the provider's and is
/// the only thing `--resume-session-at` understands. Resolving one to the other
/// is [`ForkPoint::resolve`], and it can fail — which is the point.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForkPoint {
    pub parent: RunId,
    /// A `seq` in the **parent's** log.
    pub seq: u64,
    /// The bare chain-entry uuid `--resume-session-at` accepts.
    pub address: String,
}

impl ForkPoint {
    /// Resolve the block at `seq` in `parent`'s log to a provider address.
    ///
    /// Fallible on purpose, and in two distinguishable ways: there may be no
    /// event at that `seq`, or the event may have no address. The second is not
    /// a corner case, and it has two causes of its own. Out-of-band events carry
    /// no `idempotency_key` at all — permission decisions reach us over the MCP
    /// channel and `run.finished` is made at process exit. And an event can be
    /// keyed and still unaddressable: the provider puts a `uuid` on every line
    /// it emits, but only the `assistant` and `user` ones are chain entries
    /// `--resume-session-at` resolves, so `turn.finished`, `run.rate_limited`
    /// and a hook line's `raw.unclassified` are all keyed and all refused
    /// (`docs/verification/claude-code-fork-anchor-types.md`). *Every keyed
    /// block is forkable* was the first phrasing of this and it is **false**;
    /// the rule is `normalizer::claude_code::fork_address`, which is why this
    /// hands it the event type as well as the key.
    ///
    /// It does **not** fall back to a nearby block that does have an address.
    /// `branching-surface.md` rejected snapping, and the reason is on the
    /// screen rather than in the schema: the child's inherited-context strip
    /// states a message count out loud, and a fork that silently rewound past
    /// the block the user clicked would make the one surface whose job is to
    /// say where you came from lie about it.
    pub fn resolve(log: &EventLog, parent: &RunId, seq: u64) -> Result<ForkPoint> {
        // `seq` 0 belongs to no event ([`SEQ_BEFORE_RUN`]), and `events_after`
        // is exclusive, so asking for the one event after `seq - 1` is how a
        // single `seq` is read. A saturating subtraction keeps 0 out of range
        // rather than wrapping into it.
        let found = log
            .events_after(parent, seq.saturating_sub(1), 1)?
            .into_iter()
            .find(|e| e.seq == seq)
            .ok_or_else(|| Error::NoSuchEvent {
                run: parent.0.clone(),
                seq,
            })?;

        let address = normalizer::claude_code::fork_address(
            found.type_name(),
            found.idempotency_key.as_deref(),
        )
        .ok_or_else(|| Error::NotAddressable {
            run: parent.0.clone(),
            seq,
            event: found.type_name().to_owned(),
        })?;

        Ok(ForkPoint {
            parent: parent.clone(),
            seq,
            address: address.to_owned(),
        })
    }
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

/// A started run: the live process, and the git half of it.
///
/// Not an `Option`, and that is the invariant made structural: every run this
/// crate starts has a tree before its process exists, because a cwd cannot be
/// changed afterwards. `runs.worktree_path` and `runs.shadow_ref` are still
/// nullable, for the different question of a run whose tree was cleaned up
/// later — see [`event_log::Run::shadow_ref`].
pub struct Started {
    pub session: Session,
    pub tree: StartedTree,
}

/// Where a started run's git state landed.
pub struct StartedTree {
    pub worktree: Worktree,
    /// `refs/agent-workspace/runs/<id>` — what Review attributes against
    /// (§13.1).
    pub shadow_ref: String,
}

/// Everything [`ensure_tree`] needs: what a run is about git, and nothing about
/// a process.
///
/// Separate from [`RunSpec`] because two different specs reach it. A root run
/// arrives with a `base` the caller chose; a fork arrives with one only the
/// parent's shadow history could compute, and carries no [`RunSpec`] at all.
#[derive(Debug, Clone)]
pub struct TreeSpec {
    pub run_id: RunId,
    /// The user's repository. Never the agent's cwd.
    pub repo: PathBuf,
    pub branch: String,
    /// Commit-ish the worktree branches from. For a root run, whatever the user
    /// chose; for a fork, the parent's shadow commit at the branch point (§19).
    pub base: String,
    pub post_create_hook: Option<Vec<String>>,
}

/// The git half of a run: the tree it works in, and its rooted shadow history.
pub struct Tree {
    pub worktree: Worktree,
    pub shadow: ShadowHistory,
}

/// Create the worktree, root the shadow history, record the run, and spawn.
///
/// The order is still the contract, and the split below preserves every
/// dependency in it rather than reordering anything. [`ensure_tree`] does the
/// git — worktree, then the post-create hook, then the shadow root, because a
/// root taken before the hook attributes every file the hook wrote to the run's
/// first `Edit` (§13.1). [`spawn_run`] then records the run *before* a single
/// event is appended, because the log refuses events for a run it does not
/// know.
///
/// What the split buys is a second caller for each half. [`fork`] runs the same
/// two steps in the same order; what it supplies in between is a `base` that
/// only the parent's shadow history can compute. The split is therefore about
/// *where the base comes from*, not about deferring the tree — deferring it was
/// tried and does not work, for the reason in [`fork`].
pub fn start(
    log: EventLog,
    mut spec: RunSpec,
    started_at: &str,
    sink: Option<Sender<RawEvent>>,
) -> Result<Started> {
    let tree = ensure_tree(&spec.tree_spec())?;
    let shadow_ref = tree.shadow.ref_name().to_owned();

    // §13 does not leave this to the caller: the agent's cwd is the tree this
    // run owns, and nothing else.
    spec.launch.cwd = tree.worktree.path().to_path_buf();

    let session = spawn_run(
        log,
        SpawnSpec {
            row: Run {
                id: spec.run_id.clone(),
                agent: normalizer::claude_code::AGENT_ID.to_owned(),
                agent_version: None,
                // §14: assigned, not captured. The run is resumable before the
                // process has said anything.
                provider_session_id: Some(spec.launch.session_id.clone()),
                workspace_path: spec.repo.display().to_string(),
                worktree_path: Some(tree.worktree.path().display().to_string()),
                branch: Some(spec.branch.clone()),
                shadow_ref: Some(shadow_ref.clone()),
                prompt: Some(spec.prompt.clone()),
                // What we asked for, not what the provider announces. A run
                // launched without `--model` records `None` rather than the
                // default we would have to guess at (`schema.rs`, V3).
                model: spec.launch.model.clone(),
                status: RunStatus::Running,
                started_at: started_at.to_owned(),
                finished_at: None,
                // A root run: no parent, therefore no branch point. [`fork`]
                // is what fills both in (§19).
                parent_run_id: None,
                branch_point_seq: None,
            },
            launch: spec.launch,
            shadow: Some(tree.shadow),
            prompt: &spec.prompt,
            workspace_mcp: spec.workspace_mcp.as_ref(),
            stream: StreamEnd::open(sink),
        },
    )?;

    Ok(Started {
        session,
        tree: StartedTree {
            worktree: tree.worktree,
            shadow_ref,
        },
    })
}

/// The git half of starting a run: worktree, post-create hook, shadow root.
///
/// This is a seam, not a step: [`start`] and [`fork`] both call it first and
/// always, and differ only in the `base` they hand it. Keeping the three
/// operations in one function rather than three calls at each site is
/// deliberate, because their order is load-bearing and only one of the two
/// orderings is obvious:
///
/// - The **hook runs before the shadow root**. [`SEQ_BEFORE_RUN`] labels the
///   root "the worktree as the agent found it", and the agent finds it after
///   dependency install and codegen have run. Rooting first hands every file
///   the hook wrote to whichever tool call snapshots next (§13.1).
/// - The **worktree exists before the hook**, which is worth saying only
///   because the hook is the one step here that runs arbitrary user code — and
///   it runs in the new tree, never in the user's checkout.
///
/// Idempotent it is not: `Worktree::create` refuses a path that already exists,
/// so a second call for the same run is an error rather than a no-op. That is
/// the right failure for a caller that has lost track of whether a run has a
/// tree, since the alternative is silently re-rooting a shadow history that
/// already has commits in it.
pub fn ensure_tree(spec: &TreeSpec) -> Result<Tree> {
    let worktree = Worktree::create(&spec.repo, &spec.run_id, &spec.branch, &spec.base)?;

    if let Some(hook) = &spec.post_create_hook {
        run_hook(worktree.path(), hook)?;
    }

    let (shadow, _root) = ShadowHistory::begin(&worktree, &spec.run_id, SEQ_BEFORE_RUN)?;
    Ok(Tree { worktree, shadow })
}

/// Fork a run: a new session branched at a message, in a worktree branched at
/// the same moment.
///
/// `--fork-session`, a `--session-id` we assign like every other run's (§14),
/// and a `--resume-session-at` resolved from the parent's own log
/// ([`ForkPoint::resolve`]). The child's row carries both halves of the graph
/// edge — `parent_run_id` and `branch_point_seq`, which the provider records
/// nowhere and which cannot be reconstructed later — and, like every other
/// run's row, a `worktree_path`, a `branch` and a `shadow_ref` of its own.
///
/// # The tree, and why it is made here rather than on the first write
///
/// The child's `base` is the parent's shadow commit at [`ForkPoint::seq`]
/// ([`ShadowHistory::commit_at`]): the newest snapshot at or before the branch
/// point, which for a branch point before the parent's first snapshot is the
/// root commit — the tree as the parent's agent found it. §19's "each fork pairs
/// with a worktree forked at the same `seq`" is that sentence, and this is where
/// the pairing is made.
///
/// `branching-surface.md` decided the opposite for one commit — spawn now, make
/// the tree on the child's first mutating tool call — and this reverses it. Two
/// things kill the lazy version, and both are about the moment of `spawn`:
///
/// - **A process's cwd is fixed at spawn.** A treeless child would start in
///   some ancestor's worktree, and by the time it asks to write it has already
///   computed absolute paths under that root. Making a worktree at that point
///   redirects nothing: the write lands in the ancestor's tree and the new tree
///   is correct and empty. There is no gate that fixes this, because the gate
///   fires after the decision it would have to change.
/// - **A treeless child reads the wrong tree.** An ancestor's worktree is that
///   run's tree *now*, not at the branch point — and "now" is a moving target
///   precisely in the case the surface is built for, a parent left running while
///   a child explores. The child would grep files the branch point never saw.
///
/// The cost the lazy version was buying — a fork that only thinks pays for no
/// git — was real. It was priced against corruption, and this is correctness.
///
/// # What a fork does not inherit
///
/// Its branch and its shadow ref are its own, from [`ForkSpec::branch`] and
/// [`shadow_ref`](ShadowHistory::ref_name). Sharing either would mean two
/// worktrees on one branch, which git refuses, and two runs writing one
/// attribution history, which nothing refuses and which
/// `branching-surface.md`'s reading of `pipeline.rs` shows would mis-attribute
/// the *parent's* hunks as well as the child's.
///
/// What it *does* inherit is the model, unless the caller named one — a fork is
/// a continuation of a conversation, and the conversation was had with a model.
/// It is copied onto the child's own row rather than read back through the edge,
/// so changing one branch's model later changes only that branch.
///
/// # A parent with no shadow ref cannot be forked
///
/// Refused, loudly, rather than quietly substituted for. The two available
/// substitutes are both wrong in the same direction: the repository's `HEAD` is
/// a tree the conversation never saw, and the parent's worktree *now* is the
/// tree the lazy design was rejected for handing over. A fork whose content
/// cannot be placed at the branch point is not a fork.
///
/// # A refused fork is a finished process, not a failed spawn
///
/// An address the provider cannot resolve is refused loudly, but the refusal
/// arrives as a `result` with `subtype: "error_during_execution"` from a
/// process that started and exited normally — never as an `Err` from here. The
/// refused run is even handed a fresh `session_id` of its own, so neither "we
/// got a result" nor "we got a session id" means the fork worked
/// (`docs/verification/claude-code-fork-address.md`).
///
/// Acceptance is therefore read off `subtype`, and it already is: the
/// normalizer treats any non-`success` subtype as a failed turn and ends the
/// run as `run.failed`. So a refused fork reaches the log as a failed run with
/// no assistant output, and the one place that could have contradicted it —
/// the run row's final status, which [`Registry`] used to derive from the exit
/// code alone — reads the log instead.
pub fn fork(
    log: EventLog,
    mut spec: ForkSpec,
    started_at: &str,
    sink: Option<Sender<RawEvent>>,
) -> Result<Started> {
    let parent = log
        .run(&spec.at.parent)?
        .ok_or_else(|| Error::NoSuchRun(spec.at.parent.clone()))?;
    let Some(parent_session) = parent.provider_session_id.clone() else {
        return Err(Error::CannotFork {
            run: parent.id.0.clone(),
            reason: "the parent has no provider session id, so there is nothing to \
                     --fork-session from"
                .to_owned(),
        });
    };

    // Everything that can refuse the fork refuses it here, before a worktree
    // exists or a row is written: a fork we cannot place is one we must refuse
    // rather than place badly.
    let base = branch_point_commit(&parent, spec.at.seq)?;

    let tree = ensure_tree(&TreeSpec {
        run_id: spec.run_id.clone(),
        // The same repository, necessarily: a fork of a conversation about this
        // checkout is a fork about this checkout.
        repo: PathBuf::from(&parent.workspace_path),
        branch: spec.branch.clone(),
        base,
        post_create_hook: spec.post_create_hook.clone(),
    })?;
    let shadow_ref = tree.shadow.ref_name().to_owned();

    // §13, and not left to the caller for a fork any more than for a start.
    spec.launch.cwd = tree.worktree.path().to_path_buf();
    spec.launch.resume = Some(parent_session);
    spec.launch.fork_at = Some(spec.at.address.clone());

    // `branching-surface.md`: a fork inherits its parent's model. Inherited
    // here rather than read through the edge at query time, so the child gets
    // its own value in its own column and a surface that changes one branch's
    // model changes nothing about the parent. A caller that names a model is
    // making exactly that change, so its choice wins.
    if spec.launch.model.is_none() {
        spec.launch.model = parent.model.clone();
    }

    let session = spawn_run(
        log,
        SpawnSpec {
            row: Run {
                id: spec.run_id.clone(),
                agent: normalizer::claude_code::AGENT_ID.to_owned(),
                agent_version: None,
                provider_session_id: Some(spec.launch.session_id.clone()),
                workspace_path: parent.workspace_path.clone(),
                // A fork's git state is its own, and differs from a root run's
                // only in what it was branched from.
                worktree_path: Some(tree.worktree.path().display().to_string()),
                branch: Some(spec.branch.clone()),
                shadow_ref: Some(shadow_ref.clone()),
                prompt: Some(spec.prompt.clone()),
                // Inherited above unless the caller named one.
                model: spec.launch.model.clone(),
                status: RunStatus::Running,
                started_at: started_at.to_owned(),
                finished_at: None,
                // The graph edge, whole. `create_run` refuses half of one, and
                // half is all anyone would ever be able to recover: the
                // provider re-stamps a forked transcript with its own id and
                // records no parentage (§19).
                parent_run_id: Some(spec.at.parent.clone()),
                branch_point_seq: Some(spec.at.seq),
            },
            launch: spec.launch,
            // Its own history, rooted at its own tree. A fork's writes are
            // snapshotted and attributable exactly like any other run's.
            shadow: Some(tree.shadow),
            prompt: &spec.prompt,
            workspace_mcp: spec.workspace_mcp.as_ref(),
            stream: StreamEnd::open(sink),
        },
    )?;

    Ok(Started {
        session,
        tree: StartedTree {
            worktree: tree.worktree,
            shadow_ref,
        },
    })
}

/// The commit a fork's worktree is checked out from: the parent's tree as it
/// stood at the branch point.
///
/// Read through the parent's **repository** rather than its worktree, because
/// the shadow ref lives in the shared ref namespace and outlives the checkout it
/// was written from (§13.1, verified against `gc --prune=now`). A parent whose
/// worktree has been cleaned up is still forkable; a parent whose ref is gone is
/// not, and says so.
fn branch_point_commit(parent: &Run, seq: u64) -> Result<String> {
    if parent.shadow_ref.is_none() {
        return Err(Error::CannotFork {
            run: parent.id.0.clone(),
            reason: "it has no shadow ref, so nothing records what its tree held at the \
                     branch point — and neither the repository's HEAD nor the parent's \
                     tree as it stands now is that tree"
                .to_owned(),
        });
    }

    let repo = Worktree::open(Path::new(&parent.workspace_path))?;
    ShadowHistory::open(&repo, &parent.id)?
        .commit_at(seq)?
        .ok_or_else(|| Error::CannotFork {
            run: parent.id.0.clone(),
            reason: format!(
                "its shadow ref {} holds no snapshot at or before seq {seq}, so there is \
                 no recorded tree to branch from",
                shadow_ref(&parent.id),
            ),
        })
}

/// The non-git half of starting a run.
struct SpawnSpec<'a> {
    row: Run,
    launch: LaunchSpec,
    shadow: Option<ShadowHistory>,
    prompt: &'a str,
    workspace_mcp: Option<&'a WorkspaceMcp>,
    /// The run's live channel, and the only thing that ends it: it is moved
    /// into the reader thread and closes there (`sink.rs`).
    stream: StreamEnd,
}

/// Record the run, wire the MCP server, and spawn. Touches no git.
///
/// The run row is written first and that is not stylistic: the log refuses an
/// event for a run it does not know, and the first event arrives as soon as the
/// process speaks.
fn spawn_run(mut log: EventLog, spec: SpawnSpec<'_>) -> Result<Session> {
    let SpawnSpec {
        row,
        mut launch,
        shadow,
        prompt,
        workspace_mcp,
        stream,
    } = spec;

    let run_id = row.id.clone();
    log.create_run(&row)?;

    let pipeline = Arc::new(Mutex::new(Pipeline::new(run_id.clone(), log, shadow)));
    let mcp = wire_workspace_mcp(&mut launch, &run_id, workspace_mcp, &pipeline, &stream)?;
    process::spawn(&launch, pipeline, prompt, stream, mcp)
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
    let stream = StreamEnd::open(sink);
    let mcp = wire_workspace_mcp(&mut launch, &run.id, workspace_mcp, &pipeline, &stream)?;
    process::spawn(&launch, pipeline, prompt, stream, mcp)
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
    stream: &StreamEnd,
) -> Result<Option<workspace_mcp::Listener>> {
    let Some(cfg) = cfg else { return Ok(None) };

    // A sink, never the stream itself. The listener's threads outlive the
    // agent's process by however long teardown takes, and a sender in their
    // hands would be the run's live channel refusing to close (`sink.rs`).
    let workspace = Arc::new(RunWorkspace::new(
        Arc::clone(pipeline),
        stream.sink(),
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
