//! The live view's end of a run, and the one thing that decides when it ends.
//!
//! A run has two producers of events — the thread reading the agent's stdout,
//! and whatever socket thread the workspace MCP server is serving a permission
//! request on (§6, §12) — and exactly one consumer, which is whoever is
//! watching the run happen. That consumer's end condition matters more than it
//! looks: `Registry::supervise` drains the channel to completion and *then*
//! reaps the session, so "the channel closed" is the registry's definition of
//! "this run is over".
//!
//! A bare [`Sender`] cannot express that. It closes when the last clone is
//! dropped, so the moment a second producer holds one, the run's end becomes a
//! fact about reference counting rather than about the run. That is not
//! hypothetical: the MCP listener's accept thread held a clone for the life of
//! the listener, and the listener came down inside `Session::wait` — which the
//! supervisor only reaches after the drain it was waiting on. The supervisor
//! waited for a close that waited for the wait it had not reached, and
//! `Registry::start` refused to supervise such a run at all rather than hang.
//!
//! So the two capabilities are split into two types, and only one of them is
//! clonable. [`EventSink`] emits and can be handed to any number of producers;
//! it holds no [`Sender`] of its own and can therefore never keep a stream
//! alive. [`StreamEnd`] holds the only [`Sender`] there is and is not `Clone`,
//! so the stream ends where that value is dropped — the reader thread, on
//! every path out of it including a panic. Who else is still emitting cannot
//! change that, which is what makes the invariant structural rather than a
//! comment that has to stay true.

use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

use agent_protocol::RawEvent;

/// The only [`Sender`] a run has, and the run's stream lives exactly as long as
/// it does.
///
/// Deliberately not `Clone`: a second one would put the end of the stream back
/// in the hands of whoever happens to drop last. `driver::process::spawn` moves
/// it into the reader thread, which is the thread that produces the run's last
/// event (`run.finished`, which is not a line of stdout — §8), so dropping it
/// there is the same statement as "the run has nothing left to say".
pub struct StreamEnd {
    shared: Arc<Shared>,
}

/// Somewhere to put an event. Cloning gives another producer and never extends
/// the stream's life.
///
/// A sink whose [`StreamEnd`] is gone is not an error — it is a no-op. The log
/// is the durable copy and this channel is only the view, so an event that
/// arrives after the run's stream closed (a permission request answered during
/// teardown, say) is still recorded; it simply has nowhere live to be shown.
#[derive(Clone)]
pub struct EventSink {
    shared: Arc<Shared>,
}

struct Shared {
    /// `None` once the stream has ended, either because [`StreamEnd`] was
    /// dropped or because the receiver went away.
    tx: Mutex<Option<Sender<RawEvent>>>,
}

impl StreamEnd {
    /// Open a run's stream. `None` is a run nobody is watching, which is still
    /// a stream — it just has no receiver behind it and every emit is a no-op.
    pub fn open(tx: Option<Sender<RawEvent>>) -> StreamEnd {
        StreamEnd {
            shared: Arc::new(Shared { tx: Mutex::new(tx) }),
        }
    }

    /// A producer's handle on this stream.
    pub fn sink(&self) -> EventSink {
        EventSink {
            shared: Arc::clone(&self.shared),
        }
    }
}

impl Drop for StreamEnd {
    fn drop(&mut self) {
        // Taking the sender out is what disconnects the receiver. Every
        // `EventSink` still in existence goes quiet at the same instant, which
        // is the point: no producer can outvote this.
        let _ = self.shared.take();
    }
}

impl EventSink {
    /// Offer events to whoever is watching, in order.
    ///
    /// A closed receiver means nobody is watching any more, which is not an
    /// error — but it is permanent, so the sender is dropped rather than
    /// retried on every subsequent event.
    pub fn emit(&self, events: impl IntoIterator<Item = RawEvent>) {
        let mut guard = self.shared.tx.lock().unwrap_or_else(|e| e.into_inner());
        let Some(tx) = guard.as_ref() else { return };
        for event in events {
            if tx.send(event).is_err() {
                *guard = None;
                return;
            }
        }
    }
}

impl Shared {
    fn take(&self) -> Option<Sender<RawEvent>> {
        self.tx.lock().unwrap_or_else(|e| e.into_inner()).take()
    }
}

impl std::fmt::Debug for StreamEnd {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamEnd").finish_non_exhaustive()
    }
}

impl std::fmt::Debug for EventSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventSink").finish_non_exhaustive()
    }
}
