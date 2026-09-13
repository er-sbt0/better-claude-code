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
    /// The run is inserted before the supervisor is spawned. The other order
    /// has a real race in it — a stub agent, or a launch that fails
    /// immediately, can end the stream before the insert lands, and the
    /// supervisor would then find nothing to reap and leave the session in the
    /// map forever.
    pub fn start<F>(&self, log: EventLog, spec: RunSpec, on_event: F) -> Result<Live>
    where
        F: FnMut(RawEvent) + Send + 'static,
    {
        // The accept thread of a workspace MCP server holds a clone of the
        // event sink for as long as the listener is up, and the listener only
        // comes down inside `Session::wait`. Supervising such a run would
        // deadlock: the supervisor waits for the channel to close, the channel
        // waits for the wait that the supervisor has not reached. Permissions
        // are a later piece (§12's V1 surface is a dialog and there is no
        // `Decider` that is a person yet), so this is refused loudly rather
        // than left as a hang for whoever builds it.
        if spec.workspace_mcp.is_some() {
            return Err(Error::NotSupervisable {
                reason: "the workspace MCP server's accept thread holds a clone of the event \
                         sink until the session is waited on, so the stream would never end \
                         and the run would never be reaped"
                    .to_owned(),
            });
        }

        let run_id = spec.run_id.clone();
        let branch = spec.branch.clone();
        let started_at = self.clock.now();

        let (tx, rx) = mpsc::channel();
        let started = crate::start(log, spec, &started_at, Some(tx))?;
        let live = Live {
            run_id: run_id.clone(),
            branch,
            worktree_path: started.worktree.path().to_path_buf(),
            shadow_ref: started.shadow_ref,
            started_at,
        };

        self.lock().insert(
            run_id.clone(),
            Entry {
                session: Some(started.session),
                supervisor: None,
            },
        );

        let supervisor = self.clone().supervise(run_id.clone(), rx, on_event);
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

        Ok(live)
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
    /// The channel closing is the signal, and it means exactly one thing: every
    /// [`Sender`](std::sync::mpsc::Sender) has been dropped, which happens when
    /// the driver's reader thread has emitted the tail events the process exit
    /// produced and returned. There is no other end condition, which is why
    /// [`Registry::start`] refuses a run that would keep a sender alive
    /// elsewhere.
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
    /// An interrupted run exits by signal, so it has no exit code and lands as
    /// `Failed`. That is the honest reading of the four statuses we have:
    /// inventing `Interrupted` is a schema change, and calling a killed run
    /// `Finished` would be worse than blunt.
    fn finish(&self, run: &RunId, session: Session) -> Result<Outcome> {
        let (outcome, mut log) = session.wait()?;
        let status = match outcome.exit_code {
            Some(0) => RunStatus::Finished,
            _ => RunStatus::Failed,
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
