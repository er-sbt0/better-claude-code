//! What the feed costs *below* the window, measured rather than assumed.
//!
//!     cargo run --release -p document-model --example project_cost
//!
//! `docs/verification/webkitgtk-feed-budget.md` measures the half of §4's budget
//! that WebKitGTK owns: frames, under pan and zoom, with N columns mounted. This
//! measures the other half, which that page cannot see because it is handed its
//! blocks already made.
//!
//! Three numbers, and the third is the one that decides a design:
//!
//! - **Projection.** Raw events in, blocks out. Run on every call because the
//!   feed is a function of the log rather than a delta (`project`'s own
//!   documentation says why).
//! - **Serialization.** The document has to cross the webview boundary as JSON,
//!   and a `Feed` is the largest payload this app sends.
//! - **Bytes.** What a live run re-sends every poll. The frame budget is not
//!   what bounds a streaming feed; this is.

use std::time::Instant;

use agent_protocol::{
    AssistantMessage, Event, RawEvent, RunId, ToolCompleted, ToolStarted, ToolUseId, TurnFinished,
    TurnStarted, Usage, SCHEMA_VERSION,
};

fn main() {
    println!(
        "{:<12} {:>8} {:>12} {:>14} {:>12} {:>10}",
        "events", "blocks", "project", "to JSON", "total", "payload"
    );
    for events in [100usize, 1_000, 5_000, 20_000] {
        let run = RunId::from("run_cost");
        let stream = synthesize(&run, events);

        // Warm: the first pass over a fresh allocation measures the allocator.
        let _ = document_model::project(&run, &stream);

        let t0 = Instant::now();
        let feed = document_model::project(&run, &stream);
        let projected = t0.elapsed();

        let t1 = Instant::now();
        let json = serde_json::to_string(&feed).unwrap();
        let serialized = t1.elapsed();

        println!(
            "{:<12} {:>8} {:>10.2}ms {:>12.2}ms {:>10.2}ms {:>8} KB",
            events,
            feed.blocks.len(),
            projected.as_secs_f64() * 1000.0,
            serialized.as_secs_f64() * 1000.0,
            (projected + serialized).as_secs_f64() * 1000.0,
            json.len() / 1024,
        );
    }

    println!(
        "\nA live run re-projects on a poll (`FEED_POLL_MS` in App.tsx); a finished\n\
         one is projected once. The payload column is what a poll re-sends."
    );
}

/// A run shaped like a real one: prose, tool calls, turns.
///
/// The mix matters more than the words. A run that was all prose would
/// under-count the fan-in this projection does, and one that was all tool calls
/// would over-count it.
fn synthesize(run: &RunId, count: usize) -> Vec<RawEvent> {
    let mut out = Vec::with_capacity(count);
    let mut seq = 0u64;
    let mut push = |event: Event, out: &mut Vec<RawEvent>| {
        seq += 1;
        out.push(RawEvent {
            run_id: run.clone(),
            seq,
            ts: "2026-09-13T14:22:08.412Z".to_owned(),
            schema: SCHEMA_VERSION,
            idempotency_key: Some(format!("00000000-0000-4000-8000-{seq:012}")),
            event,
        });
    };
    while out.len() < count {
        push(Event::TurnStarted(TurnStarted { index: 0 }), &mut out);
        for i in 0..4 {
            push(
                Event::AssistantMessage(AssistantMessage {
                    text: format!(
                        "Looked at the refresh path; the token is read before the clock \
                         skew is applied, which is why the retry loop never ends ({i})."
                    ),
                    message_id: Some(format!("msg_{i}")),
                    block_index: 0,
                    parent_tool_use_id: None,
                }),
                &mut out,
            );
            let id = ToolUseId::from(format!("call_{}", out.len()));
            push(
                Event::ToolStarted(ToolStarted {
                    tool_use_id: id.clone(),
                    name: "Read".to_owned(),
                    input: serde_json::json!({ "file_path": "/repo/src/auth/refresh.ts" }),
                    message_id: None,
                    parent_tool_use_id: None,
                }),
                &mut out,
            );
            push(
                Event::ToolCompleted(ToolCompleted {
                    tool_use_id: id,
                    name: Some("Read".to_owned()),
                    is_error: false,
                    content: "42 lines".to_owned(),
                    result: None,
                    non_execution: None,
                    parent_tool_use_id: None,
                }),
                &mut out,
            );
        }
        push(
            Event::TurnFinished(TurnFinished {
                index: 0,
                subtype: "success".to_owned(),
                ok: true,
                usage: Usage::default(),
                duration_ms: Some(7023),
                text: None,
                permission_denials: 0,
            }),
            &mut out,
        );
    }
    out.truncate(count);
    out
}
