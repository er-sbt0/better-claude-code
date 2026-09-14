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

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::process::CommandExt;
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use agent_protocol::{Event, ProcessOutput};
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
    /// What the agent wrote to stderr, as the bounded tail [`drain`] keeps.
    ///
    /// It is here for `examples/run.rs`, which prints it whatever the run did.
    /// It is *not* how the product sees stderr: the desktop shell drops the
    /// whole `Outcome` and still gets the explanation, because the same text
    /// reaches the log and the live stream whenever the log could not have
    /// explained the run without it — see [`unexplained`].
    pub stderr: String,
}

/// What can be asked of a running agent without owning its [`Session`].
///
/// Both fields are already shared handles, so a clone is a channel sender and
/// an `Arc` bump. That is what this type is for: [`Registry`] keeps the
/// `Session` in its map — taking it out is what reaping a run means — and
/// cannot lend a `&Session` without holding the map lock for as long as the
/// borrow lives. Handing out a `Controls` instead lets it release the lock
/// first, so no call into a run can freeze every other run's registry
/// operation behind it (`Registry::controls`).
///
/// **Crate-private, and the `Sender` is why.** Every handle here is meant to be
/// taken under the map lock, used, and dropped inside one statement. The
/// `killer` half would survive longer harmlessly — the child is `None` once the
/// reader thread has reaped it, and a handle that finds `None` does nothing —
/// but the `control` half would not: a clone that outlives its session holds
/// `write_loop` parked on `recv()` forever, and with it the dead child's stdin
/// and the thread waiting on it. That is `sink.rs`'s lesson in the other
/// direction, where a stray sender made the *end of a run* a fact about
/// reference counting; here it makes a leak per run. Keeping this out of the
/// public API is what bounds the lifetime to the one shape that is sound,
/// rather than a comment asking callers to be careful.
///
/// [`Registry`]: crate::Registry
#[derive(Clone)]
pub(crate) struct Controls {
    control: Sender<Control>,
    killer: Arc<Mutex<Option<Child>>>,
}

/// A running agent.
pub struct Session {
    controls: Controls,
    reader: JoinHandle<Result<Outcome>>,
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

impl Controls {
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
}

impl Session {
    /// A handle on this run that outlives a borrow of the session.
    ///
    /// [`Registry`](crate::Registry) steers its runs through these: it takes
    /// one under its map lock and releases the lock before using it.
    pub(crate) fn controls(&self) -> Controls {
        self.controls.clone()
    }

    /// See [`Controls::send`].
    pub fn send(&self, text: &str) -> Result<()> {
        self.controls.send(text)
    }

    /// See [`Controls::close_input`].
    pub fn close_input(&self) -> Result<()> {
        self.controls.close_input()
    }

    /// See [`Controls::interrupt`].
    pub fn interrupt(&self) -> Result<()> {
        self.controls.interrupt()
    }

    /// See [`Controls::process_group`].
    pub fn process_group(&self) -> Option<i32> {
        self.controls.process_group()
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

    let controls = Controls {
        control: control_tx,
        killer: Arc::new(Mutex::new(Some(child))),
    };
    let reader_killer = Arc::clone(&controls.killer);
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

        // Before `finish`, deliberately: the explanation is ordered ahead of
        // the conclusion it explains, and `run.finished`/`run.failed` stays the
        // last thing the run says — which `permission.rs` and the live view
        // both rely on.
        //
        // Emitted to the sink as well as written, for the reason recorded on
        // `permission::RunWorkspace::record`: an event produced at process exit
        // that reaches only the log is invisible until the run is reloaded,
        // which is how `run.finished` once left every run looking unfinished.
        if !outcome.stderr.is_empty() && unexplained(&outcome) {
            let noted = reader_pipeline
                .lock()
                .expect("pipeline mutex poisoned")
                .ingest_external(Event::ProcessOutput(ProcessOutput {
                    stream: "stderr".to_owned(),
                    text: outcome.stderr.clone(),
                }))?;
            outcome.stored += noted.stored as u64;
            sink.emit(noted.events);
        }

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
        controls,
        reader,
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

// ------------------------------------------------------------------ stderr --

/// The longest single line of stderr this will hold.
///
/// A separate bound from the tail below, and the one that actually stops the
/// agent from choosing how much of our memory to use: "keep the last N lines"
/// is bounded only if a *line* is bounded, and nothing bounds one. `-d/--debug`
/// and `--debug-file` turn this channel into a log
/// (`docs/verification/claude-code-cli-2.1.270.md`), and a log's natural
/// failure mode is one enormous line with no newline in it. `BufRead::lines`,
/// which this replaced, grows a `Vec` for such a line until the machine gives
/// out — before any policy downstream ever gets a chance to apply.
const STDERR_LINE_LIMIT: usize = 4 * 1024;

/// How many lines of stderr survive a run, and how many bytes of them.
///
/// **A tail rather than a head**, because a process that is about to exit says
/// why last: with debug on, the head of this channel is startup chatter and the
/// decisive line is the one before the exit. A head would win only for a CLI
/// that refuses its arguments immediately — and that run says nothing else at
/// all, so the whole of it fits under either rule. The tail is the one that is
/// also right for the chatty case.
const STDERR_TAIL_LINES: usize = 200;
const STDERR_TAIL_BYTES: usize = 16 * 1024;

/// The last of what a process said, and how much went past.
#[derive(Default)]
struct Tail {
    lines: VecDeque<String>,
    bytes: usize,
    dropped: u64,
}

impl Tail {
    fn push(&mut self, line: String) {
        self.bytes += line.len() + 1;
        self.lines.push_back(line);
        // `len() > 1` keeps the newest line whatever its size. A single
        // over-long line can be the *only* thing a run that died at launch
        // said, and evicting it to satisfy a byte budget would leave exactly
        // the empty, unexplained log this whole mechanism exists to fill.
        while self.lines.len() > 1
            && (self.lines.len() > STDERR_TAIL_LINES || self.bytes > STDERR_TAIL_BYTES)
        {
            let gone = self.lines.pop_front().expect("len > 1");
            self.bytes -= gone.len() + 1;
            self.dropped += 1;
        }
    }

    /// Render it, saying out loud what was dropped.
    ///
    /// A tail presented as if it were the whole channel is the same class of
    /// lie as a run row that says `failed` over a log that says otherwise: the
    /// reader cannot tell a short stderr from a truncated one, and the missing
    /// lines are invisible rather than absent.
    fn render(&self) -> String {
        let mut out = String::new();
        if self.dropped > 0 {
            out.push_str(&format!(
                "[{} earlier stderr lines dropped]\n",
                self.dropped
            ));
        }
        for line in &self.lines {
            out.push_str(line);
            out.push('\n');
        }
        out
    }
}

/// Read stderr to EOF, keeping a bounded tail of it.
///
/// Reading it all is not optional — the pipe has a buffer, and a process whose
/// stderr nobody drains blocks on the write — so the only question is what is
/// *kept*, and the answer has to be bounded in both directions the agent
/// controls: how long a line is, and how many there are.
fn drain(stderr: ChildStderr) -> String {
    let mut reader = BufReader::new(stderr);
    let mut tail = Tail::default();
    let mut buf = Vec::new();
    loop {
        buf.clear();
        let read = (&mut reader)
            .take(STDERR_LINE_LIMIT as u64)
            .read_until(b'\n', &mut buf)
            .unwrap_or(0);
        if read == 0 {
            break;
        }
        // Hitting the limit, not merely ending without a newline: the last
        // line before EOF has no newline either, and marking it clipped would
        // claim a truncation that did not happen.
        let clipped = read == STDERR_LINE_LIMIT && !buf.ends_with(b"\n");
        while matches!(buf.last(), Some(b'\n' | b'\r')) {
            buf.pop();
        }
        // Lossy rather than a parse failure: this is a byte channel, and a
        // mis-encoded line is still evidence about the run.
        let mut line = String::from_utf8_lossy(&buf).into_owned();
        if clipped {
            line.push('…');
            skip_rest_of_line(&mut reader);
        }
        tail.push(line);
    }
    tail.render()
}

/// Throw away everything up to the next newline, in bounded steps.
///
/// The bound is the point. The obvious spelling — one `read_until` — is the
/// same unbounded allocation [`STDERR_LINE_LIMIT`] exists to prevent, moved one
/// function along.
fn skip_rest_of_line<R: BufRead>(reader: &mut R) {
    let mut scratch = Vec::new();
    loop {
        scratch.clear();
        let read = reader
            .by_ref()
            .take(STDERR_LINE_LIMIT as u64)
            .read_until(b'\n', &mut scratch)
            .unwrap_or(0);
        if read == 0 || scratch.ends_with(b"\n") {
            return;
        }
    }
}

/// Would the log be able to account for this run without stderr?
///
/// This is the policy, and it is narrower than "record stderr" on purpose.
///
/// **Is stderr transient progress (§8) or a durable fact?** Both, depending on
/// the run, which is why the question cannot be answered per *channel* and is
/// answered per *run* here. §8's test is whether replaying the log without the
/// message loses anything. For a run that exited `1` having written nothing to
/// stdout, it loses everything: the log gets a `runs` row, a `run.failed` whose
/// reason is `"the agent never emitted an init message"` — a restatement of the
/// absence, not an account of it — and no explanation anywhere a user or a
/// developer can reach. That was observed, and it is the ordinary shape of a
/// bad launch: `--session-id can only be used with --continue or --resume if
/// --fork-session is also specified` went to stderr, into a `String`, and onto
/// the floor. Note that it is a different shape from the refused fork the
/// repo already handles, where acceptance is read off the `result` subtype of a
/// process that exits `0` with a full stream: neither check covers both, and
/// stderr is the only thing that separates them.
///
/// For a run that streamed a hundred turns and wrote one deprecation warning,
/// replaying without it loses nothing anyone will ask for — and with `-d/--debug`
/// on (`docs/verification/claude-code-cli-2.1.270.md`) the channel is a
/// progress log whose every line is the present moment of a turn. §8's rule for
/// that is unambiguous, so the rule here is: **stderr enters the log only where
/// the log has no other account of the run.**
///
/// Two cases qualify, and the second is not redundant. A non-zero exit is the
/// loud one. A run that stored no events at all is the quiet one — it may have
/// exited `0`, and the log would then say a run started and failed for lack of
/// an init message with nothing to add.
///
/// A resumed run replaying its history stores nothing new and is still fully
/// accounted for, which is why `duplicates` is checked too.
///
/// # What this refuses
///
/// **One `process.output` per stderr line, in real time through
/// [`Pipeline::ingest_external`].** That is the honest ordering — each stderr
/// line interleaved with the stdout line it explains — and it gives a live view
/// an error the moment it appears. It is refused on both of §8's grounds at
/// once: it writes an unbounded number of rows into an append-only log for a
/// channel whose volume the *user* chooses with a flag, and it takes the one
/// mutex that serializes `seq` once per line from a thread that is not the
/// reader, so a debug-enabled run's stderr competes with the agent's stream for
/// it. The price paid is stated rather than hidden: the log gets one blob at
/// the end, so nothing says which stdout line each stderr line was concurrent
/// with. In the case this exists for there are no stdout lines to interleave
/// with.
///
/// **A column on `runs`.** It would need a migration and a migration test for a
/// value nothing will ever query, and §8's own rule says an observed fact about
/// what the agent did is an event. It is also strictly worse here: a row holds
/// one value and a run can be launched more than once, so a resume would
/// overwrite the launch failure that is the whole reason to keep it. (The two
/// standing exceptions — `launch_flags` and the session id — are facts about
/// what a run was *permitted* to do, which the log deliberately never records.)
///
/// **The first stderr line in `run.failed.reason`.** Tempting, and it would put
/// the explanation where the failure already is. Refused because every rule in
/// `normalizer` is a pure function of the stream, which is what lets all of
/// them be tested by replaying a recorded fixture; threading a process-level
/// channel into it would end that. A `reason` is also a summary, and this keeps
/// the bytes.
///
/// [`Pipeline::ingest_external`]: crate::pipeline::Pipeline::ingest_external
fn unexplained(outcome: &Outcome) -> bool {
    matches!(outcome.exit_code, Some(code) if code != 0)
        || (outcome.stored == 0 && outcome.duplicates == 0)
}
