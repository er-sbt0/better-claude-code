//! Answering the provider's permission prompts (design §12 option A, §15).
//!
//! §12 offers two designs and warns they are not interchangeable. This is
//! option A: we do not enforce anything, we *answer* the enforcement the
//! provider already performs. Claude Code calls a tool of our choosing whenever
//! it would otherwise have prompted a terminal, and whatever that tool returns
//! is what happens. The capability is exactly what the vendor exposes and no
//! more, which is why §12 requires the UI to say `provider-mediated` rather
//! than draw a policy panel it cannot back.
//!
//! The reason this lives in the runtime rather than in a stream parser is
//! §15's rule: *permission requests come from the runtime's own channel, never
//! from parsed message text*. It is not a stylistic preference here — the
//! provider's permission layer calls the tool itself, so a permission request
//! is not a `tool_use` block and never appears on stdout at all. An agent that
//! gets prompt-injected can write something that *looks* like a permission
//! request into its own output; it cannot reach this channel.

use std::sync::{Arc, Mutex};

use agent_protocol::{Event, PermissionRequested, PermissionResolved, ToolUseId};
use workspace_mcp::{Decision, PermissionRequest, Workspace};

use crate::pipeline::Pipeline;
use crate::sink::EventSink;

/// Who answers a permission request.
///
/// The driver deliberately ships no policy of its own. In the app this is the
/// UI (§12's V1 surface is a dialog); headless, it is whatever the caller
/// decides. Keeping it a trait is what stops a default from quietly becoming
/// the product's security posture.
pub trait Decider: Send + Sync {
    fn decide(&self, request: &PermissionRequest) -> Decision;
}

/// Allow nothing. The honest default, and what a caller that has not thought
/// about §12 should get.
pub struct DenyEverything;

impl Decider for DenyEverything {
    fn decide(&self, _request: &PermissionRequest) -> Decision {
        Decision::Deny {
            message: "the workspace has no permission policy configured".to_owned(),
            interrupt: false,
        }
    }
}

/// The run's end of the workspace MCP server.
///
/// It is held by threads this type knows nothing about — the listener's accept
/// thread, and one per connection the relay opens — which is why the sink here
/// is an [`EventSink`] and not a channel sender. A sender would make this
/// object's lifetime decide when the run's live stream ends, and the threads
/// holding it are torn down inside `Session::wait`, which a supervisor only
/// reaches after that stream has already ended (`sink.rs`).
pub struct RunWorkspace {
    pipeline: Arc<Mutex<Pipeline>>,
    sink: EventSink,
    decider: Arc<dyn Decider>,
}

impl RunWorkspace {
    pub fn new(pipeline: Arc<Mutex<Pipeline>>, sink: EventSink, decider: Arc<dyn Decider>) -> Self {
        RunWorkspace {
            pipeline,
            sink,
            decider,
        }
    }

    /// Append one out-of-band event, and mirror it to the live view.
    ///
    /// Both halves matter. `process::read_loop` has the same pairing and the
    /// same trap behind it: an event that reaches the log but not the sink is
    /// invisible until the run is reloaded, which is how `run.finished` once
    /// left every run looking unfinished.
    ///
    /// The log half is the one that must not be skipped. The sink half is
    /// best-effort by construction: a request answered after the run's stream
    /// has closed — the provider's permission layer can still be mid-call while
    /// the process is exiting — is recorded and has nowhere live to be shown,
    /// which is the right way round. The alternative would be a `run.finished`
    /// that is not the last thing the live view sees.
    fn record(&self, event: Event) -> Result<(), crate::Error> {
        let ingested = self
            .pipeline
            .lock()
            .expect("pipeline mutex poisoned")
            .ingest_external(event)?;
        self.sink.emit(ingested.events);
        Ok(())
    }
}

impl Workspace for RunWorkspace {
    fn request_permission(&self, request: PermissionRequest) -> Decision {
        let tool_use_id = ToolUseId::from(request.tool_use_id.clone().unwrap_or_default());

        // Record the question before asking it. If the process dies while a
        // human is staring at the dialog, the log still says what was asked.
        if let Err(e) = self.record(Event::PermissionRequested(PermissionRequested {
            tool_use_id: tool_use_id.clone(),
            tool_name: request.tool_name.clone(),
            input: request.input.clone(),
        })) {
            // Deny rather than proceed. Granting a permission we cannot record
            // is an audit hole, and §14 makes the log — not this process — the
            // account of what an agent was allowed to do. A run stopped by a
            // broken log is recoverable; a grant nobody can find is not.
            return Decision::Deny {
                message: format!("the workspace could not record the request: {e}"),
                interrupt: false,
            };
        }

        // Outside the pipeline lock, and that is the property §12's dialog
        // rests on: this call blocks until somebody answers, and a person is
        // slow. The run's stream keeps being read and written while they think
        // — only the one tool call the provider is holding open waits.
        let decision = self.decider.decide(&request);

        let (allowed, reason) = match &decision {
            Decision::Allow { .. } => (true, None),
            Decision::Deny { message, .. } => (false, Some(message.clone())),
        };

        // A failure here cannot un-answer the question, so it must not change
        // the answer — that would make the log's existence alter the run's
        // behaviour after the fact. The gap is real and is why the request is
        // written first.
        let _ = self.record(Event::PermissionResolved(PermissionResolved {
            tool_use_id: Some(tool_use_id),
            tool_name: request.tool_name,
            allowed,
            // Distinguishes our answer from the provider's own refusals, which
            // the normalizer records with the provider's codes (`asyncAgent`
            // and friends) from `system/permission_denied`.
            reason_type: Some("workspace".to_owned()),
            reason,
        }));

        decision
    }

    fn report(&self, _tool: &str, _payload: serde_json::Value) {
        // Deliberately not logged here.
        //
        // A report reaches us twice: as this call, and as the ordinary
        // `assistant(tool_use: mcp__agent-workspace__…)` block the provider puts
        // on the wire for it. The normalizer already turns that block into
        // `workspace.report`, in stream order, from the single writer that owns
        // `seq`. Writing it again from this thread would put two events in the
        // log for one fact.
        //
        // Permissions are the opposite case and that asymmetry is the point:
        // they have no representation on the wire at all, so there this is the
        // only writer.
    }
}
