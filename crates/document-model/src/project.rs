//! Raw events in, blocks out.
//!
//! One pass in `seq` order. Most events become a block of their own; the ones
//! that belong to a tool call are folded into the block that call opened, which
//! is the "several events fan into one block" half of §7.

use std::collections::HashMap;

use agent_protocol::{Event, RawEvent, RunId, ToolCompleted, ToolResult, ToolUseId};
use serde_json::Value;

use crate::block::{
    Block, Body, Code, Diff, Feed, Hunk, Raw, Speaker, Terminal, Text, ToolCall, ToolStatus,
    DOCUMENT_VERSION,
};
use crate::fences::{self, Chunk};
use crate::hunks;
use crate::summary;

/// Tools whose arguments *are* the change they made (§6 source 1), so the call
/// reads as a [`Diff`] rather than as a generic [`ToolCall`].
///
/// A short list on purpose. A tool that is not on it is not mis-rendered — it
/// is a `ToolCall` with its arguments visible, which is the honest default —
/// whereas a tool guessed onto this list would show a diff of fields that do
/// not mean what `old_string` and `new_string` mean.
const EDIT_TOOLS: [&str; 3] = ["Edit", "Write", "MultiEdit"];

/// The tool whose argument is a command line, so the call reads as a
/// [`Terminal`].
const SHELL_TOOL: &str = "Bash";

/// A tool call this pass has opened and not yet closed.
struct Open {
    /// Index into the blocks built so far.
    index: usize,
    /// The `ts` of the event that opened it — one half of the duration.
    started_at: String,
}

/// Project a run's events into §7's feed.
///
/// The whole log every time, rather than appending to a previous answer. Two
/// reasons, and the second is the one that decides it:
///
/// 1. A block spans events — a tool call is three of them — so an incremental
///    projection has to carry the open calls between invocations, which is the
///    same state this function already holds, kept somewhere it can go stale.
/// 2. **It makes a frozen feed's catch-up free.** A column that stops rendering
///    while the camera is elsewhere has to rejoin a stream that moved on; if
///    the answer is a function of the log alone, rejoining is calling this
///    again — there is no delta to reconcile and nothing to get wrong.
///
/// It is cheap enough to mean it: one match per event and a string or two, over
/// a slice the caller already holds.
pub fn project(run_id: &RunId, events: &[RawEvent]) -> Feed {
    let mut blocks: Vec<Block> = Vec::new();
    // `tool_use_id` → the call it opened. A completion for a call we never saw
    // start is the resumed-stream case (§8), and it falls out as a block of its
    // own rather than as a lookup failure.
    let mut open: HashMap<String, Open> = HashMap::new();

    for ev in events {
        match &ev.event {
            Event::AssistantMessage(m) => {
                push_prose(&mut blocks, ev, Speaker::Assistant, &m.text, true);
            }
            Event::AssistantThinking(t) => {
                blocks.push(new_block(
                    ev,
                    Body::Text(Text {
                        speaker: Speaker::Thinking,
                        text: t.text.clone(),
                        disclosed: t.disclosed,
                    }),
                ));
            }
            Event::UserMessage(m) => {
                // A replayed prompt is the user's own message coming back under
                // `--replay-user-messages`, which the driver always passes. It
                // is the only form the user's turn takes in the log, so the
                // feed shows it as what it is rather than as an acknowledgement
                // (`docs/verification/claude-code-fork-anchor-types.md`).
                push_prose(&mut blocks, ev, Speaker::User, &m.text, false);
            }
            Event::ToolStarted(t) => {
                open.insert(
                    t.tool_use_id.0.clone(),
                    Open {
                        index: blocks.len(),
                        started_at: ev.ts.clone(),
                    },
                );
                blocks.push(started(
                    ev,
                    &t.name,
                    &t.input,
                    t.parent_tool_use_id.as_ref(),
                ));
            }
            Event::WorkspaceReport(r) => {
                // §8: a report *replaces* the `tool.started` for its block, so
                // this opens the call the matching `tool.completed` closes.
                open.insert(
                    r.tool_use_id.0.clone(),
                    Open {
                        index: blocks.len(),
                        started_at: ev.ts.clone(),
                    },
                );
                blocks.push(started(
                    ev,
                    &r.tool,
                    &r.payload,
                    r.parent_tool_use_id.as_ref(),
                ));
            }
            Event::ToolCompleted(c) => match open.remove(&c.tool_use_id.0) {
                Some(call) => {
                    let elapsed = elapsed_ms(&call.started_at, &ev.ts);
                    complete(&mut blocks[call.index], ev, c, elapsed);
                }
                // A result with no call: rendered as a finished `ToolCall`
                // rather than as `Raw`, because we know the tool's name (the
                // normalizer carries it over onto the completion) and what it
                // returned. That is a tool call — one whose beginning is not in
                // this window.
                None => {
                    let mut block = new_block(
                        ev,
                        Body::ToolCall(ToolCall {
                            name: c.name.clone().unwrap_or_else(|| "?".to_owned()),
                            input: Value::Null,
                            status: ToolStatus::Running,
                            output: None,
                            non_execution: None,
                            duration_ms: None,
                            parent_tool_use_id: c
                                .parent_tool_use_id
                                .as_ref()
                                .map(|id| id.0.clone()),
                        }),
                    );
                    block.seqs.clear();
                    complete(&mut block, ev, c, None);
                    blocks.push(block);
                }
            },
            // A permission decision belongs to the call it is about. It has no
            // block of its own — §7 ships six types and `PermissionRequest` is
            // named there as *likely next*, not now — so it widens the call's
            // `seq` range and, when it refuses, sets the call's status. That is
            // where a reader looks for it anyway.
            Event::PermissionRequested(p) => match open.get(&p.tool_use_id.0) {
                Some(call) => blocks[call.index].seqs.push(ev.seq),
                None => blocks.push(raw(ev)),
            },
            Event::PermissionResolved(p) => {
                match p.tool_use_id.as_ref().and_then(|id| open.get(&id.0)) {
                    Some(call) => {
                        let index = call.index;
                        blocks[index].seqs.push(ev.seq);
                        if !p.allowed {
                            deny(&mut blocks[index]);
                        }
                    }
                    // The provider resolves some decisions without naming the
                    // call, and a decision attached to nothing is still a fact
                    // about the run.
                    None => blocks.push(raw(ev)),
                }
            }
            // Everything else — run and turn lifecycle, rate limits, PTY
            // output, and anything the normalizer could not classify. §7's
            // invariant, and the reason there is no seventh block type.
            _ => blocks.push(raw(ev)),
        }
    }

    // Summaries last, so each is computed over the finished block rather than
    // over a tool call that had not returned yet.
    for block in &mut blocks {
        block.summary = summary::of(&block.body);
    }

    Feed {
        run_id: run_id.clone(),
        version: DOCUMENT_VERSION,
        blocks,
    }
}

/// Prose, split at fences into §7's `Text` and `Code`.
///
/// `fenced` is false for the user's own turn: what a person typed is what they
/// typed, and lifting three backticks out of it into a code panel is the shell
/// deciding they meant code.
fn push_prose(blocks: &mut Vec<Block>, ev: &RawEvent, speaker: Speaker, text: &str, fenced: bool) {
    let chunks = if fenced {
        fences::split(text)
    } else {
        vec![Chunk::Prose(text.to_owned())]
    };
    for chunk in chunks {
        let body = match chunk {
            Chunk::Prose(text) => Body::Text(Text {
                speaker,
                text,
                disclosed: true,
            }),
            Chunk::Code { language, code } => Body::Code(Code { language, code }),
        };
        blocks.push(new_block(ev, body));
    }
}

fn new_block(ev: &RawEvent, body: Body) -> Block {
    Block {
        run_id: ev.run_id.clone(),
        seqs: vec![ev.seq],
        fork_seq: address(ev),
        // Filled in at the end of `project`, over the finished block.
        summary: String::new(),
        body,
    }
}

fn raw(ev: &RawEvent) -> Block {
    let line = match &ev.event {
        Event::Unclassified(u) => Some(u.line.clone()),
        _ => None,
    };
    // The event serializes as `{"type": ..., "payload": ...}` (§8's envelope),
    // and the payload half is the part a raw block shows.
    let payload = serde_json::to_value(&ev.event)
        .ok()
        .and_then(|v| v.get("payload").cloned())
        .unwrap_or(Value::Null);
    new_block(
        ev,
        Body::Raw(Raw {
            event_type: ev.type_name().to_owned(),
            payload,
            line,
        }),
    )
}

/// The provider address of one event, through the one function that knows the
/// rule. Never re-derived here (`docs/branching-surface.md`).
fn address(ev: &RawEvent) -> Option<u64> {
    normalizer::claude_code::fork_address(ev.type_name(), ev.idempotency_key.as_deref())
        .map(|_| ev.seq)
}

/// A tool call, in whichever of the three readings its arguments earn.
fn started(ev: &RawEvent, name: &str, input: &Value, parent: Option<&ToolUseId>) -> Block {
    let body = if name == SHELL_TOOL {
        Body::Terminal(Terminal {
            command: string_arg(input, "command").unwrap_or_default(),
            description: string_arg(input, "description"),
            output: String::new(),
            exit_code: None,
            status: ToolStatus::Running,
            duration_ms: None,
        })
    } else if EDIT_TOOLS.contains(&name) {
        let (files, hunks) = edit_hunks(name, input);
        let stats = hunks::stats(hunks.iter().flat_map(|h| &h.lines));
        Body::Diff(Diff {
            tool: name.to_owned(),
            files,
            hunks,
            stats,
            status: ToolStatus::Running,
            duration_ms: None,
        })
    } else {
        Body::ToolCall(ToolCall {
            name: name.to_owned(),
            input: input.clone(),
            status: ToolStatus::Running,
            output: None,
            non_execution: None,
            duration_ms: None,
            parent_tool_use_id: parent.map(|id| id.0.clone()),
        })
    };
    new_block(ev, body)
}

/// A refused call, recorded from `permission.resolved` before the completion
/// arrives.
fn deny(block: &mut Block) {
    match &mut block.body {
        Body::ToolCall(c) => c.status = ToolStatus::Denied,
        Body::Diff(d) => d.status = ToolStatus::Denied,
        Body::Terminal(t) => t.status = ToolStatus::Denied,
        Body::Text(_) | Body::Code(_) | Body::Raw(_) => {}
    }
}

/// Close an open call with its result.
fn complete(block: &mut Block, ev: &RawEvent, c: &ToolCompleted, duration_ms: Option<u64>) {
    block.seqs.push(ev.seq);
    // The block's address moves to the completion: a fork "at this call" is one
    // that has the call *and its result*. Both ends resolve
    // (`claude-code-fork-anchor-types.md`), so this is a choice rather than the
    // only option — and it is the one the inherited-message count beside the
    // toolbar is computed at.
    if let Some(seq) = address(ev) {
        block.fork_seq = Some(seq);
    }
    // A denial recorded by `permission.resolved` outranks the completion's
    // `is_error`: the provider reports a refused call as an error, and "denied"
    // is the more specific truth (§5).
    let denied = matches!(
        &block.body,
        Body::ToolCall(ToolCall {
            status: ToolStatus::Denied,
            ..
        }) | Body::Diff(Diff {
            status: ToolStatus::Denied,
            ..
        }) | Body::Terminal(Terminal {
            status: ToolStatus::Denied,
            ..
        })
    );
    let status = if denied || c.non_execution.is_some() {
        ToolStatus::Denied
    } else if c.is_error {
        ToolStatus::Failed
    } else {
        ToolStatus::Ok
    };
    let output = match &c.result {
        Some(ToolResult::Text(text)) => text.clone(),
        Some(ToolResult::Structured(v)) => {
            structured_output(v).unwrap_or_else(|| c.content.clone())
        }
        None => c.content.clone(),
    };
    match &mut block.body {
        Body::Terminal(t) => {
            t.output = output;
            t.status = status;
            t.duration_ms = duration_ms;
        }
        Body::Diff(d) => {
            d.status = status;
            d.duration_ms = duration_ms;
        }
        Body::ToolCall(call) => {
            call.status = status;
            call.output = Some(output);
            call.non_execution.clone_from(&c.non_execution);
            call.duration_ms = duration_ms;
        }
        // A completion cannot reach a text or raw block: `open` only ever holds
        // blocks this module made from a `tool.started` or a report.
        Body::Text(_) | Body::Code(_) | Body::Raw(_) => {}
    }
}

/// Wall time between the two events, in milliseconds.
///
/// From our own timestamps (§8: `ts` is the runtime's, never the agent's), and
/// `None` rather than zero when either end will not parse — a duration we
/// cannot compute is not a call that took no time. A negative interval is also
/// `None`: the clock is ours, but it is still a clock.
fn elapsed_ms(from: &str, to: &str) -> Option<u64> {
    let start = chrono::DateTime::parse_from_rfc3339(from).ok()?;
    let end = chrono::DateTime::parse_from_rfc3339(to).ok()?;
    u64::try_from((end - start).num_milliseconds()).ok()
}

/// A `tool_use_result` object's human-readable half, when it has one.
///
/// Claude Code's structured results are shaped per tool (`stdout` for `Bash`,
/// `file` for `Read`, …) and the one thing they agree on is that the text the
/// model saw is also in `content` on the event. This lifts the two fields worth
/// preferring and otherwise says so by returning `None`.
fn structured_output(v: &Value) -> Option<String> {
    for key in ["stdout", "content"] {
        if let Some(s) = v.get(key).and_then(Value::as_str) {
            return Some(s.to_owned());
        }
    }
    None
}

fn string_arg(input: &Value, key: &str) -> Option<String> {
    input
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .filter(|s| !s.is_empty())
}

/// The files and hunks an edit tool's arguments describe.
fn edit_hunks(name: &str, input: &Value) -> (Vec<String>, Vec<Hunk>) {
    let file = string_arg(input, "file_path").unwrap_or_default();
    match name {
        "Write" => {
            let content = input
                .get("content")
                .and_then(Value::as_str)
                .unwrap_or_default();
            (
                vec![file.clone()],
                vec![Hunk {
                    file,
                    lines: hunks::whole_file(content),
                }],
            )
        }
        "MultiEdit" => {
            let edits: Vec<Hunk> = input
                .get("edits")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .map(|e| Hunk {
                    file: file.clone(),
                    lines: hunks::replacement(
                        e.get("old_string").and_then(Value::as_str).unwrap_or(""),
                        e.get("new_string").and_then(Value::as_str).unwrap_or(""),
                    ),
                })
                .collect();
            (vec![file], edits)
        }
        // `Edit`, and anything else that reaches EDIT_TOOLS.
        _ => {
            let before = input
                .get("old_string")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let after = input
                .get("new_string")
                .and_then(Value::as_str)
                .unwrap_or_default();
            (
                vec![file.clone()],
                vec![Hunk {
                    file,
                    lines: hunks::replacement(before, after),
                }],
            )
        }
    }
}
