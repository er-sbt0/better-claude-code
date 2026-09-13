//! The runs this process is holding open, and the threads that reap them.
//!
//! [`start`](crate::start) hands back a [`Session`] and nothing owns it. For a
//! CLI that is enough — `examples/run.rs` keeps it in a local and blocks on it
//! — but an app has to find that session again later, from another thread, to
//! send it a second turn or to end it. This module is that map plus the one
//! thread per run that drains the event stream and reaps the session when the
//! stream ends.
//!
//! It lives in this crate rather than in `apps/desktop` because it decides
//! things — when a run is over, what its final status is, and what happens to
//! every live run when the app exits — and the rule recorded against
//! `apps/desktop` is that anything there which starts deciding belongs in a
//! crate, where a test can reach it without a window. Every behaviour below is
//! covered by `tests/registry.rs` against a stub agent.
//!
//! ## Two rules that only exist here
//!
//! **Nothing closes stdin on the run's behalf.** With
//! `--input-format stream-json` the agent waits for another turn until stdin
//! closes, so closing it is what ends a run — and `examples/run.rs` does that
//! on every `turn.finished`, which makes every run there single-turn. An app
//! wants the opposite: a run is a conversation, and the user decides when it is
//! over. The consequence is the part to keep in mind: **a run started through
//! this registry holds a live `claude` process, its worktree and a second
//! connection to the event log until [`Registry::end`] or
//! [`Registry::interrupt`] is called** — or until [`Registry::shutdown`] runs
//! at app exit, which is why that is not optional.
//!
//! **The reaper is what closes the second connection.** [`Session::wait`]
//! consumes the session and hands back the [`EventLog`] the run was writing to
//! (§14's single-writer rule is per connection: the driver's log is never the
//! UI's). So reaping has to take the session *out* of the map before waiting on
//! it, and dropping the log it returns is how the run's connection goes away.
//! A session left in the map is a file handle and a process that outlive the
//! run.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};

use agent_protocol::{RawEvent, RunId};
use event_log::{EventLog, RunStatus};
use normalizer::{Clock, SystemClock};

use crate::error::{Error, Result};
use crate::process::{Outcome, Session};
use crate::RunSpec;

/// What a caller gets back for a run that is now under supervision.
///
/// Everything here is already in the run's row, and it is returned anyway: the
/// caller has just started the run and would otherwise have to query the log to
/// learn where it landed.
#[derive(Debug, Clone)]
pub struct Live {
    pub run_id: RunId,
    pub branch: String,
    /// The agent's cwd, which is never the user's own checkout (§13).
    ///
    /// Not an `Option`: a run has its tree before it has a process, because a
    /// cwd cannot be changed afterwards. `runs.worktree_path` is nullable for a
    /// different question — a tree removed after the fact — which this type,
    /// describing a run that has just started, cannot be in.
    pub worktree_path: PathBuf,
    /// `refs/agent-workspace/runs/<id>` — what Review attributes against
    /// (§13.1).
    pub shadow_ref: String,
    /// The timestamp the run row carries, taken from this registry's clock.
    pub started_at: String,
}

/// One live run: the session, and the thread watching its stream.
///
/// The supervisor handle is kept so that [`Registry::shutdown`] can join it
/// rather than racing it. Without that, app exit would return while a thread
/// was still writing the run's final status.
struct Entry {
    session: Option<Session>,
    supervisor: Option<JoinHandle<()>>,
}

/// The runs alive in this process, and the machinery that ends them.
///
/// Cloning shares the map: a supervisor thread holds a clone so it can reap
/// itself out of it.
#[derive(Clone)]
pub struct Registry {
    live: Arc<Mutex<HashMap<RunId, Entry>>>,
    /// §8: timestamps come from our runtime, never from the agent. The
    /// registry needs one of its own because it writes the moment a run ends,
    /// which no event carries.
    clock: Arc<dyn Clock>,
}

impl Default for Registry {
    fn default() -> Self {
        Registry::new()
    }
}

impl Registry {
    pub fn new() -> Self {
        Registry::with_clock(Arc::new(SystemClock))
    }

    pub fn with_clock(clock: Arc<dyn Clock>) -> Self {
        Registry {
            live: Arc::new(Mutex::new(HashMap::new())),
            clock,
        }
    }

    /// Start a run, keep its session, and supervise its event stream.
    ///
    /// `log` is the run's own connection to the event log and is consumed:
    /// [`start`](crate::start) takes an [`EventLog`] by value and only
    /// [`Session::wait`] gives it back, which is precisely why a caller must
    /// open a second connection rather than lend the one the UI reads through.
    ///
    /// `on_event` runs on the supervisor thread, once per event, in stream
    /// order, for the life of the run — including the events produced at
    /// process exit, which are not lines of stdout and are the ones a live view
    /// most needs. It is the only hook this type offers on purpose: a caller
    /// that wants to push events at a webview has exactly one place to do it,
    /// and everything else about the run's lifetime stays here.
    ///
    /// The run is inserted before the supervisor is spawned; that ordering and
    /// the race it avoids are described on [`adopt`](Self::adopt), which
    /// [`fork`](Self::fork) shares.
    pub fn start<F>(&self, log: EventLog, spec: RunSpec, on_event: F) -> Result<Live>
    where
        F: FnMut(RawEvent) + Send + 'static,
    {
        let run_id = spec.run_id.clone();
        let branch = spec.branch.clone();
        let started_at = self.clock.now();

        let (tx, rx) = mpsc::channel();
        let started = crate::start(log, spec, &started_at, Some(tx))?;
        let live = Live {
            run_id,
            branch,
            worktree_path: started.tree.worktree.path().to_path_buf(),
            shadow_ref: started.tree.shadow_ref,
            started_at,
        };
        Ok(self.adopt(live, started.session, rx, on_event))
    }

    /// Fork a live or finished run, and supervise the child exactly like a root
    /// run (§19, `docs/branching-surface.md`).
    ///
    /// The same three lines of translation as [`start`](Self::start) over
    /// [`fork`](crate::fork), and that is the point: **a fork is a run**. It is
    /// inserted into the same map, drained by the same supervisor, reaped by
    /// the same rule and ended by the same [`shutdown`](Self::shutdown) — so
    /// nothing downstream of here has to know which of the two started it.
    ///
    /// Everything that distinguishes a fork happens before this returns and is
    /// [`fork`](crate::fork)'s: the branch point is resolved against the
    /// parent's log, the child's tree is checked out from the parent's shadow
    /// commit at that `seq`, and both halves of the graph edge are written to
    /// the child's row. A fork that cannot be placed fails here rather than
    /// starting a process, which is why this returns before there is anything
    /// to supervise.
    ///
    /// What it does **not** promise is that the provider accepted the address.
    /// A refused fork is a process that starts and exits cleanly, with an
    /// `error_during_execution` result and a session id of its own
    /// (`docs/verification/claude-code-fork-address.md`), so it arrives here as
    /// a perfectly ordinary run that the supervisor reaps — and
    /// [`finish`](Self::finish) files as `Failed`, because it reads the log
    /// rather than the exit code.
    pub fn fork<F>(&self, log: EventLog, spec: crate::ForkSpec, on_event: F) -> Result<Live>
    where
        F: FnMut(RawEvent) + Send + 'static,
    {
        let run_id = spec.run_id.clone();
        let branch = spec.branch.clone();
        let started_at = self.clock.now();

        let (tx, rx) = mpsc::channel();
        let started = crate::fork(log, spec, &started_at, Some(tx))?;
        let live = Live {
            run_id,
            branch,
            worktree_path: started.tree.worktree.path().to_path_buf(),
            shadow_ref: started.tree.shadow_ref,
            started_at,
        };
        Ok(self.adopt(live, started.session, rx, on_event))
    }

    /// Take ownership of a run that has just been spawned: file it, watch it,
    /// and describe it back to the caller.
    ///
    /// Shared by [`start`](Self::start) and [`fork`](Self::fork) because every
    /// line of it is about supervision rather than about how the process came
    /// to exist. The ordering inside it is the part that is load-bearing: the
    /// run is inserted **before** the supervisor is spawned, since a stub agent
    /// or a launch that fails immediately can end the stream before the insert
    /// lands, and the supervisor would then find nothing to reap and leave the
    /// session in the map forever.
    fn adopt<F>(
        &self,
        live: Live,
        session: Session,
        events: Receiver<RawEvent>,
        on_event: F,
    ) -> Live
    where
        F: FnMut(RawEvent) + Send + 'static,
    {
        let run_id = live.run_id.clone();

        self.lock().insert(
            run_id.clone(),
            Entry {
                session: Some(session),
                supervisor: None,
            },
        );

        let supervisor = self.clone().supervise(run_id.clone(), events, on_event);
        // The supervisor may have reaped the run already — a run that fails in
        // its first millisecond is not hypothetical — in which case there is no
        // entry to file the handle against and joining it here costs nothing:
        // the thread is on its way out.
        let orphaned = match self.lock().get_mut(&run_id) {
            Some(entry) => {
                entry.supervisor = Some(supervisor);
                None
            }
            None => Some(supervisor),
        };
        if let Some(handle) = orphaned {
            let _ = handle.join();
        }

        live
    }

    /// Take a run this process is not holding and make it live again (§14).
    ///
    /// **This is what makes a conversation outlive a window.** Nothing closes
    /// stdin on a run's behalf, so a run ends when the app exits, and until
    /// this existed the composer on every run from a previous session was dead
    /// forever: the only runs that could take a turn were the ones started
    /// since launch. The provider still holds the session, we still hold its
    /// id (§14: we *assign* it), and [`resume`](crate::resume) already knew how
    /// to seed the normalizer from the log — what was missing was a way to put
    /// the result back under supervision.
    ///
    /// The run row is the input rather than a spec, because everything this
    /// needs was written down when the run was created: its worktree, its
    /// shadow ref, its branch, and the provider session to resume. A caller
    /// that had to reconstruct those would be guessing at three of them.
    ///
    /// Refuses a run with no provider session id — there is nothing to resume,
    /// and starting a fresh session under an old run's row would silently
    /// detach the row from the conversation it names.
    pub fn resume<F>(
        &self,
        log: EventLog,
        run: &event_log::Run,
        launch: crate::LaunchSpec,
        prompt: &str,
        workspace_mcp: Option<&crate::WorkspaceMcp>,
        on_event: F,
    ) -> Result<Live>
    where
        F: FnMut(RawEvent) + Send + 'static,
    {
        if run.provider_session_id.is_none() {
            return Err(Error::CannotFork {
                run: run.id.0.clone(),
                reason: "it has no provider session id, so there is no conversation to resume"
                    .to_owned(),
            });
        }

        let (tx, rx) = mpsc::channel();
        let session = crate::resume(log, run, launch, prompt, Some(tx), workspace_mcp)?;

        // Straight off the row: a resumed run is the same run, in the same tree,
        // writing the same shadow history. Anything invented here would be a
        // second opinion about a fact already recorded.
        let live = Live {
            run_id: run.id.clone(),
            branch: run.branch.clone().unwrap_or_default(),
            worktree_path: PathBuf::from(
                run.worktree_path.as_deref().unwrap_or(&run.workspace_path),
            ),
            shadow_ref: run.shadow_ref.clone().unwrap_or_default(),
            started_at: run.started_at.clone(),
        };
        Ok(self.adopt(live, session, rx, on_event))
    }

    /// Send a follow-up user turn to a live run (§5: one process, many turns).
    pub fn send(&self, run: &RunId, text: &str) -> Result<()> {
        self.with_session(run, |session| session.send(text))
    }

    /// End a run: close its stdin and let the agent finish the turn it is in.
    ///
    /// This is the only graceful way a run ends, because nothing here closes
    /// stdin on a `turn.finished`. The session is *not* reaped from this call:
    /// the agent has a turn to finish, and the supervisor reaps it when the
    /// stream actually stops.
    pub fn end(&self, run: &RunId) -> Result<()> {
        self.with_session(run, Session::close_input)
    }

    /// Stop a run now, mid-turn.
    ///
    /// Everything the log received up to this point stays: writing per line
    /// rather than at the end is what makes an interrupt cheap (§8).
    ///
    /// The row this leaves behind is `Interrupted`, not `Failed`: the user
    /// asked for the run to stop, and nothing the agent did went wrong. A
    /// failure already in the log still outranks it — see
    /// [`finish`](Self::finish).
    pub fn interrupt(&self, run: &RunId) -> Result<()> {
        self.with_session(run, Session::interrupt)
    }

    /// The runs this process is holding open, in no particular order.
    pub fn live(&self) -> Vec<RunId> {
        self.lock()
            .iter()
            .filter(|(_, e)| e.session.is_some())
            .map(|(id, _)| id.clone())
            .collect()
    }

    pub fn is_live(&self, run: &RunId) -> bool {
        self.lock().get(run).is_some_and(|e| e.session.is_some())
    }

    /// Take a run out of the map, wait for its process, and close its log.
    ///
    /// `None` means the run was not here to reap — it ended already, or this
    /// process never started it. Ordinarily the supervisor calls this, and a
    /// caller only needs it to reap a run whose stream it is not watching.
    pub fn reap(&self, run: &RunId) -> Option<Result<Outcome>> {
        let session = self.lock().get_mut(run)?.session.take()?;
        let outcome = self.finish(run, session);
        // Dropping the entry drops the supervisor's handle when the supervisor
        // is the caller, which detaches a thread that is one statement from
        // returning.
        self.lock().remove(run);
        Some(outcome)
    }

    /// End every live run, and do not return until they are gone.
    ///
    /// This exists for app exit and is not a nicety: with no auto-close, a
    /// `tauri dev` reload that skipped it would strand a `claude` process per
    /// run on the developer's machine, still holding a worktree and a log
    /// connection. Interrupting rather than closing stdin is deliberate — the
    /// app is going away, and waiting for every agent to finish its turn first
    /// is a hang, not a courtesy.
    ///
    /// So **every run open at app exit is interrupted**, healthy ones included,
    /// and that is the ordinary case rather than the exception — which is why
    /// [`finish`](Self::finish) must not read a missing exit code as a failure.
    ///
    /// Returns what it ended, so a caller can say so.
    pub fn shutdown(&self) -> Vec<RunId> {
        let ended = self.live();
        // Kill first, all of them, then wait: each `wait` below blocks until
        // that run's reader thread sees stdout close, so interrupting inside
        // the same loop would serialize the runs behind each other.
        for run in &ended {
            let _ = self.interrupt(run);
        }

        // Drained in one lock, so a supervisor that is racing us finds nothing
        // and returns instead of waiting on a session we already hold. Nothing
        // below touches the map, which is what keeps the joins deadlock-free.
        let entries: Vec<(RunId, Entry)> = self.lock().drain().collect();
        for (run, mut entry) in entries {
            if let Some(session) = entry.session.take() {
                let _ = self.finish(&run, session);
            }
            if let Some(supervisor) = entry.supervisor.take() {
                let _ = supervisor.join();
            }
        }
        ended
    }

    /// Drain the run's events into `on_event`, then reap it.
    ///
    /// The channel closing is the signal, and it means exactly one thing: the
    /// driver's reader thread has emitted the tail events the process exit
    /// produced and returned. There is no other end condition, and there does
    /// not need to be — `driver::sink` makes that the *only* thing it can
    /// mean. The sender lives in a [`StreamEnd`](crate::StreamEnd) that only
    /// the reader thread holds, and every other producer of the run's events —
    /// the workspace MCP server's connection threads, which outlive the agent's
    /// process by however long teardown takes — holds an
    /// [`EventSink`](crate::EventSink), which cannot keep the channel open.
    ///
    /// That used to be false, and the difference is what unblocked §12: while
    /// the listener's accept thread held a sender, a supervised run that hosted
    /// the permission server would have waited here for a close that only
    /// [`Session::wait`](crate::Session::wait) could cause, and this thread is
    /// what has to reach that wait. Such a run was refused at the door rather
    /// than left to hang; now it is an ordinary run.
    fn supervise<F>(self, run: RunId, events: Receiver<RawEvent>, mut on_event: F) -> JoinHandle<()>
    where
        F: FnMut(RawEvent) + Send + 'static,
    {
        thread::spawn(move || {
            for event in events {
                on_event(event);
            }
            let _ = self.reap(&run);
        })
    }

    /// Wait for the process and close the run's row and its connection.
    ///
    /// The row is the part the driver deliberately does not do — `start` leaves
    /// it `Running` and a test pins that — because only something that owns the
    /// session knows the run is over rather than merely quiet. Left alone, the
    /// Runs list would show every finished run as live until the next boot
    /// reconciled it (§8), which is the same lie crash recovery exists to
    /// prevent.
    ///
    /// **The exit code is not the only authority, and on its own it lies about
    /// every run this app ended.** Two readings are needed and neither is the
    /// exit code by itself.
    ///
    /// A run can fail in the stream and still exit cleanly — the provider
    /// reports a bad turn as a `result` with a non-`success` subtype, and a
    /// refused fork is exactly that: `--resume-session-at` on an address that
    /// resolves nowhere produces `error_during_execution` from a process that
    /// started and stopped normally, and even hands the refused run a fresh
    /// `session_id` (`docs/verification/claude-code-fork-address.md`). The
    /// normalizer already reads acceptance off the subtype and ends such a run
    /// as `run.failed`; this reads that back, so the row cannot say `Finished`
    /// over a log that says the run failed.
    ///
    /// And a run can succeed and have no exit code at all. Nothing closes
    /// stdin on a run's behalf, so a healthy run sits waiting for its next turn
    /// until something kills it — [`interrupt`](Self::interrupt), or
    /// [`shutdown`](Self::shutdown) at app exit, which is *every* live run. A
    /// process killed by a signal exits with no code, so the old rule filed all
    /// of them as `Failed` over logs full of `turn.finished`, ending in
    /// `run.finished`, with no `run.failed` anywhere. `RunStatus::Interrupted`
    /// is the word for that, and no exit code is how it is detected: whether
    /// the signal came from us or from the machine, nothing the *agent* did
    /// ended this run.
    ///
    /// **A failure in the log outranks the interrupt**, which is the one
    /// ordering here worth arguing. A run that failed a turn and then sat
    /// waiting for another is killed at shutdown like any other, and filing it
    /// `Interrupted` would erase the failure exactly the way the old rule
    /// invented one. The cost is the reverse corner — a run killed before it
    /// emitted an init message is `Failed`, because the log says it never
    /// started — and that is the cheaper mistake: it is rare, it is one
    /// millisecond wide, and it errs towards keeping a recorded failure rather
    /// than towards hiding one.
    fn finish(&self, run: &RunId, session: Session) -> Result<Outcome> {
        let (outcome, mut log) = session.wait()?;
        let failed_in_stream = !log.events_of_type(run, "run.failed")?.is_empty();
        let status = match outcome.exit_code {
            _ if failed_in_stream => RunStatus::Failed,
            Some(0) => RunStatus::Finished,
            // Killed by a signal: there is no exit code to read, and that is
            // the signature of a run something outside it stopped.
            None => RunStatus::Interrupted,
            Some(_) => RunStatus::Failed,
        };
        log.finish_run(run, status, &self.clock.now())?;
        // `log` is dropped here: the run's second connection to the event log
        // closes with it, which is the whole reason `wait` returns it.
        Ok(outcome)
    }

    fn with_session<T>(&self, run: &RunId, f: impl FnOnce(&Session) -> Result<T>) -> Result<T> {
        let live = self.lock();
        let session = live
            .get(run)
            .and_then(|e| e.session.as_ref())
            .ok_or_else(|| Error::NotLive { run: run.0.clone() })?;
        f(session)
    }

    /// The map is only ever held across operations that cannot block on a run.
    /// A poisoned lock means a supervisor panicked mid-reap; recovering is
    /// better than taking the app down with it, since the worst case is a
    /// session that outlives its run and is killed at shutdown anyway.
    fn lock(&self) -> MutexGuard<'_, HashMap<RunId, Entry>> {
        self.live.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl std::fmt::Debug for Registry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Registry")
            .field("live", &self.live())
            .finish_non_exhaustive()
    }
}
