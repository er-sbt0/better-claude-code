//! One provider line in; log writes and a worktree snapshot out.
//!
//! This is the driver without the process. It is where the two rules that only
//! the driver can enforce actually live — append per line in one transaction,
//! and snapshot after every tool call that changed something — and keeping it
//! free of `Command` is what lets both be tested by replaying a recorded
//! fixture instead of by running an agent.

use agent_protocol::{Event, RawEvent, RunId};
use event_log::{Appended, EventLog};
use normalizer::{ClaudeCodeNormalizer, Clock, SystemClock};
use worktree::{Cause, ShadowCommit, ShadowHistory};

use crate::error::Result;

/// What one line produced.
#[derive(Debug, Default)]
pub struct Ingested {
    pub events: Vec<RawEvent>,
    pub stored: usize,
    /// Events already in the log under the same idempotency key. Non-zero only
    /// while a resumed stream replays its history (§14).
    pub duplicates: usize,
    /// The snapshot this line caused, if it changed anything on disk.
    pub snapshot: Option<ShadowCommit>,
    /// How many tool completions shared that snapshot.
    ///
    /// One is the good case. More than one means the provider completed
    /// several tool calls in a single message — parallel calls, typically a
    /// subagent — and the filesystem change cannot be split between them, so
    /// attribution for it is "one of these calls". Surface it rather than
    /// implying a precision we do not have (§13.1).
    pub snapshot_covers: usize,
}

/// Normalizer, log and shadow history, wired together.
pub struct Pipeline {
    run_id: RunId,
    normalizer: ClaudeCodeNormalizer,
    log: EventLog,
    shadow: Option<ShadowHistory>,
}

impl Pipeline {
    /// A pipeline for a run that has no events yet.
    pub fn new(run_id: RunId, log: EventLog, shadow: Option<ShadowHistory>) -> Self {
        Self::with_clock(run_id, log, shadow, Box::new(SystemClock))
    }

    pub fn with_clock(
        run_id: RunId,
        log: EventLog,
        shadow: Option<ShadowHistory>,
        clock: Box<dyn Clock>,
    ) -> Self {
        Pipeline {
            normalizer: ClaudeCodeNormalizer::with_clock(run_id.clone(), clock),
            run_id,
            log,
            shadow,
        }
    }

    /// A pipeline for a run being resumed.
    ///
    /// Seeds the normalizer from the log, which is not optional: a fresh
    /// normalizer restarts `seq` at 1 and every append fails. §14 also explains
    /// why this cannot be detected from the stream — a resumed `init` is
    /// structurally identical to a fresh one.
    pub fn resuming(run_id: RunId, log: EventLog, shadow: Option<ShadowHistory>) -> Result<Self> {
        Self::resuming_with_clock(run_id, log, shadow, Box::new(SystemClock))
    }

    pub fn resuming_with_clock(
        run_id: RunId,
        log: EventLog,
        shadow: Option<ShadowHistory>,
        clock: Box<dyn Clock>,
    ) -> Result<Self> {
        let last = log.last_seq(&run_id)?;
        Ok(Pipeline {
            normalizer: ClaudeCodeNormalizer::resume_at(run_id.clone(), clock, last),
            run_id,
            log,
            shadow,
        })
    }

    pub fn run_id(&self) -> &RunId {
        &self.run_id
    }
    pub fn log(&self) -> &EventLog {
        &self.log
    }
    pub fn into_log(self) -> EventLog {
        self.log
    }
    pub fn stats(&self) -> &normalizer::Stats {
        self.normalizer.stats()
    }

    /// Feed one line of the agent's stdout.
    pub fn ingest(&mut self, line: &str) -> Result<Ingested> {
        let events = self.normalizer.push_line(line);
        self.absorb(events)
    }

    /// Record an event that did not come from the agent's stdout.
    ///
    /// The workspace MCP server is the other producer (§6 source 3, §12): the
    /// provider calls our `request_permission` tool directly, on a thread that
    /// is not the one reading the stream. That call is an observed fact about
    /// the run and belongs in the log beside everything else, in order.
    ///
    /// Two consequences the caller must respect. `seq` still comes from the
    /// normalizer, so this must not run concurrently with [`Pipeline::ingest`]
    /// — the driver holds one mutex over the whole pipeline for exactly that
    /// reason. And no snapshot is taken: nothing has run yet at the moment
    /// permission is *asked*, so there is nothing on disk to attribute (§13.1).
    pub fn ingest_external(&mut self, event: Event) -> Result<Ingested> {
        let event = self.normalizer.wrap_external(event);
        self.absorb(vec![event])
    }

    /// Close the run once the process has exited.
    ///
    /// The stream never says a run is over — `result` is per turn (§5) — so the
    /// process is the only authority, which is why this is the driver's call
    /// and not the normalizer's.
    pub fn finish(&mut self, exit_code: Option<i32>) -> Result<Ingested> {
        let events = self.normalizer.finish(exit_code);
        self.absorb(events)
    }

    fn absorb(&mut self, events: Vec<RawEvent>) -> Result<Ingested> {
        if events.is_empty() {
            return Ok(Ingested::default());
        }

        // One transaction per provider line. A line can fan out into several
        // events (§5), and a crash must never leave half a provider message in
        // the log.
        let results = self.log.append_all(&events)?;
        let duplicates = results.iter().filter(|r| r.is_duplicate()).count();
        let stored = results.len() - duplicates;

        // Snapshot only what was actually stored. A replayed event describes
        // work the worktree already reflects and the shadow history already
        // records.
        let completions: Vec<&RawEvent> = events
            .iter()
            .zip(&results)
            .filter(|(_, r)| matches!(r, Appended::Stored(_)))
            .map(|(e, _)| e)
            .filter(|e| matches!(e.event, Event::ToolCompleted(_)))
            .collect();

        let mut out = Ingested {
            stored,
            duplicates,
            snapshot_covers: completions.len(),
            snapshot: None,
            events: Vec::new(),
        };

        // The log is written before the worktree is snapshotted, deliberately.
        // Crash between the two and attribution for this call falls back to the
        // previous snapshot -- a loss of precision. The other order would put a
        // shadow commit in the history naming a `seq` that is not in the log,
        // which is a loss of sense.
        if let (Some(first), Some(shadow)) = (completions.first(), self.shadow.as_mut()) {
            let Event::ToolCompleted(tc) = &first.event else {
                unreachable!("filtered above")
            };
            out.snapshot = shadow.snapshot(&Cause {
                seq: first.seq,
                tool: tc.name.clone().unwrap_or_else(|| "?".to_owned()),
                tool_use_id: Some(tc.tool_use_id.0.clone()),
            })?;
        }

        out.events = events;
        Ok(out)
    }
}
