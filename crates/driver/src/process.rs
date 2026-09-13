//! Spawning the agent and pumping its stream.
//!
//! Threads rather than an async runtime: the driver needs exactly three tasks —
//! read stdout, drain stderr, write stdin — and adding a runtime to the
//! workspace to express that would be the largest dependency in it.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use event_log::EventLog;

use crate::error::{Error, Result};
use crate::launch::{user_message, LaunchSpec};
use crate::pipeline::{Ingested, Pipeline};
use crate::sink::{EventSink, StreamEnd};

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

    /// Stop the agent now.
    ///
    /// The log keeps everything received up to this point — that is the whole
    /// point of writing per line rather than at the end.
    pub fn interrupt(&self) -> Result<()> {
        if let Some(child) = self.killer.lock().expect("not poisoned").as_mut() {
            let _ = child.kill();
        }
        Ok(())
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
        let status = {
            let mut guard = reader_killer.lock().expect("not poisoned");
            match guard.as_mut() {
                Some(child) => child.wait().ok(),
                None => None,
            }
        };
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
