//! Claude Code `--output-format stream-json` → raw events.
//!
//! Written against recorded streams from 2.1.269, not against documentation.
//! Every surprising branch below has a fixture behind it; see
//! `fixtures/claude-code/2.1.269/MANIFEST.md` and design §5.
//!
//! Deliberately concrete. There is no trait here and no second adapter: §5's
//! rule is to write the second one before extracting the abstraction, and we
//! have recorded exactly one agent so far.

use std::collections::HashMap;

use agent_protocol::{
    AssistantMessage, AssistantThinking, Event, ModelUsage, PermissionResolved, RawEvent,
    RunFailed, RunFinished, RunId, RunRateLimited, RunStarted, SessionId, ToolCompleted,
    ToolResult, ToolStarted, ToolUseId, TurnFinished, TurnStarted, Unclassified,
    UnclassifiedReason, Usage, UserMessage, SCHEMA_VERSION,
};
use serde_json::Value;

use crate::clock::{Clock, SystemClock};
use crate::stats::Stats;

pub const AGENT_ID: &str = "claude-code";

/// Provider message types that describe the present moment of a turn and carry
/// nothing once it ends.
///
/// Dropped rather than stored, per design §8: one 52-line fixture contains 45
/// `thinking_tokens` messages, each restating the same fact with a larger
/// number. The terminal value of every counter survives in `result`.
///
/// This is the *only* place the normalizer discards anything, and the test for
/// membership is whether replaying the log without the message loses
/// information. When unsure, do not add it here — the invariant wins ties.
fn is_transient(msg_type: &str, subtype: Option<&str>) -> bool {
    match (msg_type, subtype) {
        ("system", Some("status")) => true,
        ("system", Some("thinking_tokens")) => true,
        // Raw SSE passthrough under --include-partial-messages. The aggregated
        // `assistant` message is emitted too, so every delta is a duplicate of
        // something we already keep (design §5).
        ("stream_event", _) => true,
        _ => false,
    }
}

pub struct ClaudeCodeNormalizer {
    run_id: RunId,
    clock: Box<dyn Clock>,
    seq: u64,
    started: bool,
    /// Zero-based index of the turn currently open.
    current_turn: u32,
    turns_finished: u32,
    /// Set by the first turn that ended badly; decides run.finished vs run.failed.
    failure: Option<String>,
    /// tool_use_id → tool name, so a completion can name the tool that produced
    /// it. The provider does not repeat the name on the result, and the driver
    /// needs it to label the shadow-history snapshot a tool call causes (§13.1).
    tool_names: HashMap<String, String>,
    stats: Stats,
}

impl ClaudeCodeNormalizer {
    pub fn new(run_id: RunId) -> Self {
        Self::with_clock(run_id, Box::new(SystemClock))
    }

    pub fn with_clock(run_id: RunId, clock: Box<dyn Clock>) -> Self {
        Self {
            run_id,
            clock,
            seq: 0,
            started: false,
            current_turn: 0,
            turns_finished: 0,
            failure: None,
            tool_names: HashMap::new(),
            stats: Stats::default(),
        }
    }

    /// Continue numbering a run that already has events in the log.
    ///
    /// A resumed run **must** use this. A fresh normalizer restarts `seq` at 1
    /// and interleaves with history; the log rejects that rather than storing
    /// it, so the symptom is an error rather than corruption — but the fix is
    /// here. Seed from `EventLog::last_seq` (design §8, §14).
    pub fn resume_at(run_id: RunId, clock: Box<dyn Clock>, last_seq: u64) -> Self {
        let mut n = Self::with_clock(run_id, clock);
        n.seq = last_seq;
        // A resumed stream's `init` is structurally identical to a fresh run's
        // (§14): no marker, no replayed history. The provider will not tell us,
        // so the run row remembers and we start already started -- otherwise
        // the resume emits a second `run.started` for the same run.
        n.started = true;
        n
    }

    pub fn stats(&self) -> &Stats {
        &self.stats
    }

    /// Feed one line of the provider's stdout.
    ///
    /// Never fails. A line we cannot parse becomes a `raw.unclassified` event
    /// holding the original bytes, because a normalizer bug must cost fidelity
    /// and never information (design §7).
    pub fn push_line(&mut self, line: &str) -> Vec<RawEvent> {
        if line.trim().is_empty() {
            return Vec::new();
        }

        let value: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => {
                self.stats.see("<not json>");
                self.stats.unclassified += 1;
                return vec![self.wrap(
                    None,
                    Event::Unclassified(Unclassified {
                        source_type: None,
                        source_subtype: None,
                        reason: UnclassifiedReason::NotJson,
                        line: line.to_owned(),
                    }),
                )];
            }
        };

        let msg_type = value
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let subtype = value.get("subtype").and_then(Value::as_str);

        let source_key = match subtype {
            Some(s) => format!("{msg_type}/{s}"),
            None => msg_type.to_owned(),
        };
        self.stats.see(&source_key);

        if is_transient(msg_type, subtype) {
            self.stats.transient_dropped += 1;
            return Vec::new();
        }

        let uuid = value.get("uuid").and_then(Value::as_str).map(str::to_owned);
        let events = match msg_type {
            "system" => self.on_system(&value, subtype, line),
            "assistant" => self.on_assistant(&value),
            "user" => self.on_user(&value),
            "result" => self.on_result(&value),
            "rate_limit_event" => vec![Event::RunRateLimited(RunRateLimited {
                detail: value.clone(),
            })],
            _ => vec![Event::Unclassified(Unclassified {
                source_type: Some(msg_type.to_owned()),
                source_subtype: subtype.map(str::to_owned),
                reason: UnclassifiedReason::UnknownType,
                line: line.to_owned(),
            })],
        };

        if events.iter().any(|e| matches!(e, Event::Unclassified(_))) {
            self.stats.unclassified += 1;
        } else {
            self.stats.classified += 1;
        }

        let multi = events.len() > 1;
        events
            .into_iter()
            .enumerate()
            .map(|(i, event)| {
                let key = uuid
                    .as_ref()
                    .map(|u| if multi { format!("{u}#{i}") } else { u.clone() });
                self.wrap(key, event)
            })
            .collect()
    }

    /// Close the run once the process has exited.
    ///
    /// The stream itself never says a run is over — `result` is per turn, not
    /// per run (design §5), so the only authority on the run ending is the
    /// process.
    pub fn finish(&mut self, exit_code: Option<i32>) -> Vec<RawEvent> {
        let turns = self.turns_finished;

        if !self.started {
            return vec![self.wrap(
                None,
                Event::RunFailed(RunFailed {
                    exit_code,
                    turns,
                    reason: "the agent never emitted an init message".to_owned(),
                }),
            )];
        }

        let reason = self.failure.clone().or_else(|| match exit_code {
            Some(0) | None => None,
            Some(code) => Some(format!("agent exited with status {code}")),
        });

        let event = match reason {
            Some(reason) => Event::RunFailed(RunFailed {
                exit_code,
                turns,
                reason,
            }),
            None => Event::RunFinished(RunFinished { exit_code, turns }),
        };
        vec![self.wrap(None, event)]
    }

    fn wrap(&mut self, idempotency_key: Option<String>, event: Event) -> RawEvent {
        self.seq += 1;
        RawEvent {
            run_id: self.run_id.clone(),
            seq: self.seq,
            ts: self.clock.now(),
            schema: SCHEMA_VERSION,
            idempotency_key,
            event,
        }
    }

    // ---------------------------------------------------------------- system --

    fn on_system(&mut self, v: &Value, subtype: Option<&str>, line: &str) -> Vec<Event> {
        match subtype {
            // `init` is NOT the start of a run. It is re-emitted at the start of
            // every turn when stdin is a stream — same session_id, new uuid — so
            // mapping it 1:1 to run.started invents a phantom run per turn. This
            // was the most consequential error in the original design (§5).
            Some("init") => {
                if self.started {
                    self.current_turn += 1;
                    vec![Event::TurnStarted(TurnStarted {
                        index: self.current_turn,
                    })]
                } else {
                    self.started = true;
                    self.current_turn = 0;
                    vec![
                        Event::RunStarted(RunStarted {
                            agent: AGENT_ID.to_owned(),
                            agent_version: str_field(v, "claude_code_version"),
                            session_id: SessionId::from(
                                v.get("session_id")
                                    .and_then(Value::as_str)
                                    .unwrap_or_default(),
                            ),
                            model: str_field(v, "model"),
                            cwd: str_field(v, "cwd").unwrap_or_default(),
                            permission_mode: str_field(v, "permissionMode"),
                            tools: string_list(v, "tools"),
                            mcp_servers: mcp_server_names(v),
                        }),
                        Event::TurnStarted(TurnStarted { index: 0 }),
                    ]
                }
            }

            // A resolution with no matching request: nobody was asked, because
            // this session had no approval surface. Still a permission decision
            // and still belongs in the log.
            Some("permission_denied") => vec![Event::PermissionResolved(PermissionResolved {
                tool_use_id: str_field(v, "tool_use_id").map(ToolUseId::from),
                tool_name: str_field(v, "tool_name").unwrap_or_default(),
                allowed: false,
                reason_type: str_field(v, "decision_reason_type"),
                reason: str_field(v, "decision_reason"),
            })],

            // Hook lifecycle and anything else: recognized, not modeled. Kept
            // verbatim so that turning it into a real event type later is a
            // normalizer change and not a data-loss problem.
            _ => vec![Event::Unclassified(Unclassified {
                source_type: Some("system".to_owned()),
                source_subtype: subtype.map(str::to_owned),
                reason: UnclassifiedReason::NotModeled,
                line: line.to_owned(),
            })],
        }
    }

    // ------------------------------------------------------------- assistant --

    /// One provider line can carry several content blocks, and several lines can
    /// share one `message.id`. Each block becomes its own event, tagged with the
    /// message id so consumers can regroup.
    ///
    /// Usage is deliberately ignored here. Every one of those lines repeats an
    /// identical `message.usage`, so summing across them double-counts; usage is
    /// read once, from `result` (design §5, §14).
    fn on_assistant(&mut self, v: &Value) -> Vec<Event> {
        let message_id = v
            .get("message")
            .and_then(|m| m.get("id"))
            .and_then(Value::as_str)
            .map(str::to_owned);
        let parent = str_field(v, "parent_tool_use_id").map(ToolUseId::from);

        let blocks = match v.get("message").and_then(|m| m.get("content")) {
            Some(Value::Array(blocks)) => blocks,
            _ => return vec![malformed(v, "assistant message has no content array")],
        };

        blocks
            .iter()
            .enumerate()
            .map(|(i, block)| {
                let block_index = i as u32;
                match block.get("type").and_then(Value::as_str) {
                    Some("text") => Event::AssistantMessage(AssistantMessage {
                        text: str_field(block, "text").unwrap_or_default(),
                        message_id: message_id.clone(),
                        block_index,
                        parent_tool_use_id: parent.clone(),
                    }),
                    Some("thinking") => {
                        let text = str_field(block, "thinking").unwrap_or_default();
                        Event::AssistantThinking(AssistantThinking {
                            disclosed: !text.is_empty(),
                            text,
                            message_id: message_id.clone(),
                            block_index,
                            parent_tool_use_id: parent.clone(),
                        })
                    }
                    Some("tool_use") => {
                        let id = str_field(block, "id").unwrap_or_default();
                        let name = str_field(block, "name").unwrap_or_default();
                        self.tool_names.insert(id.clone(), name.clone());
                        Event::ToolStarted(ToolStarted {
                            tool_use_id: ToolUseId::from(id),
                            name,
                            input: block.get("input").cloned().unwrap_or(Value::Null),
                            message_id: message_id.clone(),
                            parent_tool_use_id: parent.clone(),
                        })
                    }
                    _ => malformed(block, "unrecognized assistant content block"),
                }
            })
            .collect()
    }

    // ------------------------------------------------------------------ user --

    fn on_user(&mut self, v: &Value) -> Vec<Event> {
        let is_replay = v.get("isReplay").and_then(Value::as_bool).unwrap_or(false);
        let parent = str_field(v, "parent_tool_use_id").map(ToolUseId::from);

        // `tool_result_meta` is a sibling of the content, keyed by tool_use id,
        // and is absent more often than not. It carries `non_execution_kind`,
        // which is the only machine-readable way to tell "refused" from
        // "failed" — the prose in the result says both the same way.
        let mut non_exec: HashMap<String, String> = HashMap::new();
        if let Some(Value::Array(meta)) = v.get("tool_result_meta") {
            for m in meta {
                if let (Some(id), Some(kind)) = (
                    m.get("id").and_then(Value::as_str),
                    m.get("non_execution_kind").and_then(Value::as_str),
                ) {
                    non_exec.insert(id.to_owned(), kind.to_owned());
                }
            }
        }

        let result_detail = v.get("tool_use_result").and_then(|r| match r {
            Value::String(s) => Some(ToolResult::Text(s.clone())),
            Value::Null => None,
            other => Some(ToolResult::Structured(other.clone())),
        });

        let content = v.get("message").and_then(|m| m.get("content"));
        let blocks = match content {
            Some(Value::Array(blocks)) => blocks.clone(),
            // A plain-string user message: our own prompt, going in.
            Some(Value::String(text)) => {
                return vec![Event::UserMessage(UserMessage {
                    text: text.clone(),
                    is_replay,
                })]
            }
            _ => return vec![malformed(v, "user message has no content")],
        };

        blocks
            .iter()
            .map(|block| match block.get("type").and_then(Value::as_str) {
                Some("tool_result") => {
                    let id = str_field(block, "tool_use_id").unwrap_or_default();
                    Event::ToolCompleted(ToolCompleted {
                        name: self.tool_names.get(&id).cloned(),
                        is_error: block
                            .get("is_error")
                            .and_then(Value::as_bool)
                            .unwrap_or(false),
                        content: flatten_text(block.get("content")),
                        result: result_detail.clone(),
                        non_execution: non_exec.get(&id).cloned(),
                        parent_tool_use_id: parent.clone(),
                        tool_use_id: ToolUseId::from(id),
                    })
                }
                Some("text") => Event::UserMessage(UserMessage {
                    text: str_field(block, "text").unwrap_or_default(),
                    is_replay,
                }),
                _ => malformed(block, "unrecognized user content block"),
            })
            .collect()
    }

    // ---------------------------------------------------------------- result --

    /// `result` ends a **turn**, not the run. Several arrive in one process,
    /// distinguished by `result_index`, each reporting `num_turns: 1`.
    fn on_result(&mut self, v: &Value) -> Vec<Event> {
        let subtype = str_field(v, "subtype").unwrap_or_default();

        // Not `is_error`: a run whose every tool call was permission-denied
        // still reports `is_error: false` with subtype `success` (design §5).
        let ok = subtype == "success";
        if !ok && self.failure.is_none() {
            self.failure = Some(format!("turn ended with subtype `{subtype}`"));
        }

        let index = v
            .get("result_index")
            .and_then(Value::as_u64)
            .map(|i| i as u32)
            .unwrap_or(self.current_turn);

        let denials = v
            .get("permission_denials")
            .and_then(Value::as_array)
            .map(|a| a.len() as u32)
            .unwrap_or(0);

        self.turns_finished += 1;

        vec![Event::TurnFinished(TurnFinished {
            index,
            subtype,
            ok,
            usage: read_usage(v),
            duration_ms: v.get("duration_ms").and_then(Value::as_u64),
            text: str_field(v, "result"),
            permission_denials: denials,
        })]
    }
}

// ------------------------------------------------------------------ helpers --

/// Read usage from `modelUsage`, never from `usage`.
///
/// `usage` covers the main model only. Every recorded run also spends on a side
/// model that appears solely in `modelUsage`, whose costs reconcile exactly to
/// `total_cost_usd`. See [`agent_protocol::Usage`] and design §14.
fn read_usage(v: &Value) -> Usage {
    let cost = v
        .get("total_cost_usd")
        .and_then(Value::as_f64)
        .unwrap_or(0.0);

    let mut models = Vec::new();
    if let Some(Value::Object(map)) = v.get("modelUsage") {
        for (model, entry) in map {
            models.push(ModelUsage {
                model: model.clone(),
                input_tokens: u64_field(entry, "inputTokens"),
                output_tokens: u64_field(entry, "outputTokens"),
                cache_creation_tokens: u64_field(entry, "cacheCreationInputTokens"),
                cache_read_tokens: u64_field(entry, "cacheReadInputTokens"),
                thinking_tokens: u64_field(entry, "thinkingTokens"),
                cost_usd: entry.get("costUSD").and_then(Value::as_f64).unwrap_or(0.0),
            });
        }
        models.sort_by(|a, b| a.model.cmp(&b.model));
    }

    Usage::from_models(cost, models)
}

fn malformed(v: &Value, reason: &str) -> Event {
    Event::Unclassified(Unclassified {
        source_type: str_field(v, "type"),
        source_subtype: str_field(v, "subtype"),
        reason: UnclassifiedReason::MalformedShape,
        line: format!("{reason}: {v}"),
    })
}

fn str_field(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn u64_field(v: &Value, key: &str) -> u64 {
    v.get(key).and_then(Value::as_u64).unwrap_or(0)
}

fn string_list(v: &Value, key: &str) -> Vec<String> {
    v.get(key)
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn mcp_server_names(v: &Value) -> Vec<String> {
    v.get("mcp_servers")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|s| match s {
                    Value::String(name) => Some(name.clone()),
                    Value::Object(_) => str_field(s, "name"),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default()
}

/// A tool result's `content` is a string sometimes and a block array other
/// times. Both mean the same thing to a reader.
fn flatten_text(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}
