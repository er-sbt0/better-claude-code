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

use agent_protocol::RawEvent;
use event_log::EventLog;

use crate::error::{Error, Result};
use crate::launch::{user_message, LaunchSpec};
use crate::pipeline::{Ingested, Pipeline};

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
    reader: JoinHandle<Result<(Outcome, Pipeline)>>,
    killer: Arc<Mutex<Option<Child>>>,
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
    pub fn wait(self) -> Result<(Outcome, EventLog)> {
        let (outcome, pipeline) = self.reader.join().unwrap_or_else(|_| Err(Error::Closed))?;
        Ok((outcome, pipeline.into_log()))
    }
}

/// Spawn the agent and start pumping.
///
/// `first_prompt` is written to stdin rather than passed as `-p`'s value: with
/// `--input-format stream-json` the prompt is a `user` message on the wire.
pub fn spawn(
    spec: &LaunchSpec,
    mut pipeline: Pipeline,
    first_prompt: &str,
    sink: Option<Sender<RawEvent>>,
) -> Result<Session> {
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
    let reader = thread::spawn(move || {
        let outcome = read_loop(stdout, &mut pipeline, sink.as_ref());
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
        let tail = pipeline.finish(outcome.exit_code)?;
        outcome.stored += tail.stored as u64;
        emit(sink.as_ref(), tail.events);
        drop(writer);
        Ok((outcome, pipeline))
    });

    Ok(Session {
        control: control_tx,
        reader,
        killer,
    })
}

fn read_loop(
    stdout: ChildStdout,
    pipeline: &mut Pipeline,
    sink: Option<&Sender<RawEvent>>,
) -> Result<Outcome> {
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
        let Ingested {
            events,
            stored,
            duplicates,
            snapshot,
            ..
        } = pipeline.ingest(&line)?;
        outcome.stored += stored as u64;
        outcome.duplicates += duplicates as u64;
        if snapshot.is_some() {
            outcome.snapshots += 1;
        }
        emit(sink, events);
    }
    Ok(outcome)
}

/// Offer events to the live view.
///
/// A closed receiver means nobody is watching any more, which is not an error:
/// the log is the durable copy and this channel is only the view.
fn emit(sink: Option<&Sender<RawEvent>>, events: Vec<RawEvent>) {
    let Some(sink) = sink else { return };
    for e in events {
        if sink.send(e).is_err() {
            return;
        }
    }
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
