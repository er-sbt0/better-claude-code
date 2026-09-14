//! Spawning the agent and pumping its stream.
//!
//! Threads rather than an async runtime: the driver needs exactly three tasks —
//! read stdout, drain stderr, write stdin — and adding a runtime to the
//! workspace to express that would be the largest dependency in it.
//!
//! # The agent is a tree, not a process
//!
//! `claude` is never the only process a run owns. It spawns
//! `workspace-mcp-relay` for the MCP server we inject (§12), and a child per
//! `Bash` tool call — `npm test`, a dev server, anything the agent was asked to
//! run. Killing the `claude` pid alone reparents every one of them to init,
//! where they keep running, keep holding the run's worktree, and are no longer
//! reachable from anything that knows about the run.
//!
//! That is not the failure path, it is the *ordinary* one: nothing closes a
//! run's stdin on its behalf (`registry`), so [`Registry::shutdown`] interrupts
//! every live run at app exit, healthy ones included. Every app exit and every
//! `tauri dev` reload leaked a tree per run.
//!
//! So [`spawn`] gives the child a process group of its own and
//! [`Session::interrupt`] signals the *group*. The group is exactly the run's
//! descendants: a child inherits its parent's group, and nothing outside the
//! tree can join one it does not know the id of.
//!
//! [`Registry::shutdown`]: crate::Registry::shutdown
//!
//! # Why this file is Unix-only, and says so with `libc`
//!
//! The workspace already is — `workspace_mcp::Listener` is a `UnixListener`
//! with no `cfg` around it — so there is no portability being given up here,
//! only a dependency to justify. `libc` is the platform's own header rather
//! than a reimplementation of it, so it is not the kind of dependency
//! `workspace_mcp`'s "why it is hand-rolled" argues against: the alternative is
//! declaring `kill` ourselves and writing down signal numbers, which is a
//! reimplementation of a header that is already in this workspace's build graph
//! (`rusqlite` pulls it) and costs nothing to name.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::process::CommandExt;
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use event_log::EventLog;

use crate::error::{Error, Result};
use crate::launch::{user_message, LaunchSpec};
use crate::pipeline::{Ingested, Pipeline};
use crate::sink::{EventSink, StreamEnd};

/// How long a run gets to stop politely before it is killed outright.
///
/// It is bounded and short because it is the one thing app exit waits for, and
/// the rule recorded on [`Registry::shutdown`](crate::Registry::shutdown) is
/// that waiting for an agent to finish is a hang rather than a courtesy. The
/// number is a measurement with room in it: Claude Code 2.1.270 takes ~0.6s to
/// leave on `SIGTERM` (`docs/verification/claude-code-signals.md`), and a
/// machine under load, or a run with a long transcript to flush, is allowed to
/// be three times slower before we stop being polite.
const GRACE: Duration = Duration::from_secs(2);

/// Signal a process group, and do not care whether anything was there.
///
/// `kill(2)` with a negated pid addresses the group, which is the whole point:
/// the agent's descendants are the group, and they are what the plain
/// [`Child::kill`] this replaced could never reach. `ESRCH` — an already-empty
/// group — is the normal ending, not an error.
fn signal_group(group: libc::pid_t, signal: libc::c_int) {
    // Safe: `kill` touches no memory of ours, and `group` is the pid of a child
    // that has not been reaped, so the kernel has not reused it. Both callers
    // hold the `killer` lock to establish that second half.
    unsafe { libc::kill(-group, signal) };
}

/// What the caller can do to a running agent.
enum Control {
    /// A follow-up user turn.
    Send(String),
    /// Close stdin. The agent finishes the turn in flight and exits.
    Close,
}

/// How a run ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub exit_code: Option<i32>,
    /// Events written to the log. Excludes replayed duplicates.
    pub stored: u64,
    pub duplicates: u64,
    /// Snapshots the run's tool calls produced (§13.1).
    pub snapshots: u64,
    /// Lines that arrived after the process had already been reaped.
    pub stderr: String,
}

/// A running agent.
pub struct Session {
    control: Sender<Control>,
    reader: JoinHandle<Result<Outcome>>,
    killer: Arc<Mutex<Option<Child>>>,
    /// Shared, because the stream is no longer the only writer: the workspace
    /// MCP server appends `permission.requested` from the thread serving the
    /// provider's call (§6, §12). The log is still single-writer in the sense
    /// §14 means — one process, one connection — and this mutex is what makes
    /// that true across the two producers inside it.
    pipeline: Arc<Mutex<Pipeline>>,
    /// Held so it outlives the run and is shut down before the log is
    /// reclaimed. `None` when no workspace server was injected.
    mcp: Option<workspace_mcp::Listener>,
}

impl Session {
    /// Send a follow-up user turn.
    ///
    /// This is what `--input-format stream-json` buys: one process, many turns
    /// (§5, the `multi-turn` fixture).
    pub fn send(&self, text: &str) -> Result<()> {
        self.control
            .send(Control::Send(user_message(text)))
            .map_err(|_| Error::Closed)
    }

    /// Close stdin and let the agent finish the turn it is in.
    pub fn close_input(&self) -> Result<()> {
        self.control.send(Control::Close).map_err(|_| Error::Closed)
    }

    /// Stop the agent now — it, and everything it started.
    ///
    /// The log keeps everything received up to this point — that is the whole
    /// point of writing per line rather than at the end.
    ///
    /// **`SIGTERM` to the group, then `SIGKILL` to the group**, and each half
    /// of that is a measured fact rather than a convention
    /// (`docs/verification/claude-code-signals.md`).
    ///
    /// *The group*, because the run is a tree and `SIGKILL` to the `claude` pid
    /// alone orphans the rest of it — see the module docs.
    ///
    /// *`SIGTERM` first*, because the provider owns the session transcript that
    /// `--resume` later reads, and it writes the tail of a turn to that file
    /// *after* the `result` line has already reached our stdout. `SIGKILL`
    /// inside that window drops whatever had not been written — observed once,
    /// the assistant half of a completed turn missing and never arriving, and
    /// not reproducible on demand, which is the honest version: a real window
    /// of tens of milliseconds rather than a certainty. Claude Code 2.1.270
    /// handles `SIGTERM` — it spends ~0.6s leaving, then re-raises it — and the
    /// window closes. Half a second against a silent loss is a cheap trade.
    ///
    /// Because it re-raises rather than exiting, the run still has **no exit
    /// code**, which is what [`Registry::finish`] reads to file it as
    /// `Interrupted`. `SIGINT` would have broken that quietly: the same CLI
    /// answers it by exiting `0`, and every interrupted run would have been
    /// filed as `Finished`.
    ///
    /// [`Registry::finish`]: crate::Registry
    pub fn interrupt(&self) -> Result<()> {
        let guard = self.killer.lock().expect("not poisoned");
        // `None` once the reader thread has reaped: there is nothing left to
        // stop, and signalling a negated pid that the kernel has since handed
        // to somebody else is how a bug like this gets worse rather than fixed.
        let Some(child) = guard.as_ref() else {
            return Ok(());
        };
        let group = child.id() as libc::pid_t;
        signal_group(group, libc::SIGTERM);
        drop(guard);

        // The escalation is detached on purpose. `Registry::shutdown` signals
        // every live run before waiting on any of them, precisely so the grace
        // periods overlap; a blocking wait here would serialize app exit behind
        // one grace period per run.
        let killer = Arc::clone(&self.killer);
        thread::spawn(move || {
            thread::sleep(GRACE);
            let guard = killer.lock().expect("not poisoned");
            if let Some(child) = guard.as_ref() {
                signal_group(child.id() as libc::pid_t, libc::SIGKILL);
            }
        });
        Ok(())
    }

    /// The agent's process group id, or `None` once it has been reaped.
    ///
    /// It is the child's pid — [`spawn`] makes the child a group leader — and
    /// it exists for the one caller that cannot use [`interrupt`](Self::interrupt):
    /// a signal handler, which may not take a lock. `examples/run.rs` latches
    /// this into a static while the run is live and signals the group from its
    /// `SIGINT` handler, where `kill(2)` is one of the few calls it is allowed
    /// to make. Anything that is not a signal handler should call `interrupt`.
    pub fn process_group(&self) -> Option<i32> {
        self.killer
            .lock()
            .expect("not poisoned")
            .as_ref()
            .map(|c| c.id() as i32)
    }

    /// Block until the agent exits, and take back the log.
    pub fn wait(mut self) -> Result<(Outcome, EventLog)> {
        let outcome = self.reader.join().unwrap_or_else(|_| Err(Error::Closed))?;

        // Order matters. Every connection thread the server is still serving
        // holds a reference to the pipeline, so the log cannot be reclaimed
        // until the listener is down and joined. Doing this after the reader
        // has finished also means a permission request that arrives during
        // teardown is answered rather than dropped on a closed socket.
        if let Some(mcp) = self.mcp.take() {
            mcp.shutdown();
        }

        let pipeline = Arc::try_unwrap(self.pipeline)
            .map_err(|_| Error::Closed)?
            .into_inner()
            .expect("pipeline mutex poisoned");
        Ok((outcome, pipeline.into_log()))
    }
}

/// Spawn the agent and start pumping.
///
/// `first_prompt` is written to stdin rather than passed as `-p`'s value: with
/// `--input-format stream-json` the prompt is a `user` message on the wire.
pub fn spawn(
    spec: &LaunchSpec,
    pipeline: Arc<Mutex<Pipeline>>,
    first_prompt: &str,
    stream: StreamEnd,
    mcp: Option<workspace_mcp::Listener>,
) -> Result<Session> {
    spec.validate()?;
    let argv = spec.argv();
    let mut child = Command::new(&spec.binary)
        .args(&argv)
        .current_dir(&spec.cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // A group of its own, so the run's whole tree is addressable by one
        // signal (module docs). `0` means "a new group led by the child", so
        // the group id is the child's pid and nothing else can ever be in it.
        //
        // The cost is that the child no longer shares this process's foreground
        // group, so a Ctrl-C at a terminal reaches us and not the agent. Only
        // `examples/run.rs` is a terminal program, and it handles `SIGINT`
        // itself for exactly this reason.
        .process_group(0)
        .spawn()
        .map_err(|source| Error::Spawn {
            binary: spec.binary.display().to_string(),
            source,
        })?;

    let stdin = child.stdin.take().expect("piped");
    let stdout = child.stdout.take().expect("piped");
    let stderr = child.stderr.take().expect("piped");

    let (control_tx, control_rx) = mpsc::channel();
    control_tx
        .send(Control::Send(user_message(first_prompt)))
        .map_err(|_| Error::Closed)?;

    let writer = thread::spawn(move || write_loop(stdin, control_rx));
    let errors = thread::spawn(move || drain(stderr));

    let killer = Arc::new(Mutex::new(Some(child)));
    let reader_killer = Arc::clone(&killer);
    let reader_pipeline = Arc::clone(&pipeline);
    let reader = thread::spawn(move || {
        // `stream` is moved in and never leaves: the run's live channel closes
        // where this thread returns, by whatever path it returns — the `?`
        // below, or a panic. Nothing else holds a sender, so no other producer
        // can keep a finished run looking live (`sink.rs`).
        let sink = stream.sink();
        let outcome = read_loop(stdout, &reader_pipeline, &sink);
        // Reap before joining the helpers: the writer is blocked on a channel
        // that stays open until the caller drops the Session, and stderr does
        // not close until the process does.
        //
        // Taken out rather than borrowed, and the lock released before the
        // wait. Both halves matter now that `interrupt` signals a *group*: a
        // reaped pid can be handed to an unrelated process, and negating it
        // would then aim a `SIGKILL` at a stranger's group. `None` is how
        // `interrupt` and its escalation thread know to do nothing, and holding
        // the lock across the wait would instead make `interrupt` block behind
        // the exit it is trying to cause.
        let child = reader_killer.lock().expect("not poisoned").take();
        let status = child.and_then(|mut c| c.wait().ok());
        let mut outcome = outcome?;
        outcome.exit_code = status.and_then(|s| s.code());
        outcome.stderr = errors.join().unwrap_or_default();

        // Only the process knows the run is over (§5): `result` is per turn.
        // These events go to the sink like any other: `run.finished` is the one
        // a live view most needs, and it is produced here rather than by a line
        // of stdout, so forgetting it leaves every run looking unfinished.
        let tail = reader_pipeline
            .lock()
            .expect("pipeline mutex poisoned")
            .finish(outcome.exit_code)?;
        outcome.stored += tail.stored as u64;
        sink.emit(tail.events);
        drop(writer);
        Ok(outcome)
    });

    Ok(Session {
        control: control_tx,
        reader,
        killer,
        pipeline,
        mcp,
    })
}

fn read_loop(stdout: ChildStdout, pipeline: &Mutex<Pipeline>, sink: &EventSink) -> Result<Outcome> {
    let mut outcome = Outcome {
        exit_code: None,
        stored: 0,
        duplicates: 0,
        snapshots: 0,
        stderr: String::new(),
    };
    for line in BufReader::new(stdout).lines() {
        let line = match line {
            Ok(l) => l,
            // A read error mid-stream is the process dying under us. Everything
            // up to here is already durable.
            Err(_) => break,
        };
        // Held for one line only. A permission request arriving mid-stream
        // waits for this to be released, which is the serialization `seq`
        // requires -- and the agent is blocked on that answer anyway, so it
        // costs nothing it was not already paying.
        let Ingested {
            events,
            stored,
            duplicates,
            snapshot,
            ..
        } = pipeline
            .lock()
            .expect("pipeline mutex poisoned")
            .ingest(&line)?;
        outcome.stored += stored as u64;
        outcome.duplicates += duplicates as u64;
        if snapshot.is_some() {
            outcome.snapshots += 1;
        }
        sink.emit(events);
    }
    Ok(outcome)
}

fn write_loop(mut stdin: ChildStdin, rx: Receiver<Control>) {
    while let Ok(msg) = rx.recv() {
        match msg {
            Control::Send(line) => {
                if writeln!(stdin, "{line}").is_err() || stdin.flush().is_err() {
                    return;
                }
            }
            Control::Close => return,
        }
    }
}

fn drain(stderr: ChildStderr) -> String {
    let mut out = String::new();
    for line in BufReader::new(stderr)
        .lines()
        .map_while(std::result::Result::ok)
    {
        out.push_str(&line);
        out.push('\n');
    }
    out
}
