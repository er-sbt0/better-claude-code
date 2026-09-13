//! Replay every recorded stream through the normalizer.
//!
//! Design §16: golden fixtures replayed in CI are the only defense against a
//! provider changing its format without notice. These tests are also the real
//! definition of the adapter contract — more than the types are.
//!
//! Two layers:
//!
//! - **Golden output** for every fixture, so any change in interpretation shows
//!   up as a reviewable diff. Regenerate with `UPDATE_GOLDEN=1 cargo test`.
//! - **Named regression tests** for each place the recorded streams contradicted
//!   the design doc. Those are the bugs this crate exists to not have.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use agent_protocol::{Event, RawEvent};
use normalizer::{ClaudeCodeNormalizer, FixedClock};

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/claude-code/2.1.269")
}

fn golden_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden")
}

fn fixtures() -> Vec<(String, PathBuf)> {
    let mut out: Vec<(String, PathBuf)> = fs::read_dir(fixture_dir())
        .expect("fixtures/claude-code/2.1.269 is missing — see MANIFEST.md")
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "ndjson"))
        .map(|p| {
            let name = p.file_stem().unwrap().to_string_lossy().into_owned();
            (name, p)
        })
        .collect();
    out.sort();
    assert!(!out.is_empty(), "no fixtures found");
    out
}

/// Normalize a whole fixture, finishing the run as a clean exit.
fn replay(path: &Path) -> (Vec<RawEvent>, normalizer::Stats) {
    let text = fs::read_to_string(path).unwrap();
    let mut n =
        ClaudeCodeNormalizer::with_clock("run_test".into(), Box::new(FixedClock::default()));
    let mut events: Vec<RawEvent> = text.lines().flat_map(|l| n.push_line(l)).collect();
    events.extend(n.finish(Some(0)));
    let stats = n.stats().clone();
    (events, stats)
}

fn events_of(name: &str) -> Vec<RawEvent> {
    replay(&fixture_dir().join(format!("{name}.ndjson"))).0
}

fn type_counts(events: &[RawEvent]) -> BTreeMap<&'static str, usize> {
    let mut counts = BTreeMap::new();
    for e in events {
        *counts.entry(e.type_name()).or_default() += 1;
    }
    counts
}

// ------------------------------------------------------------------ goldens --

/// A stable, readable rendering of what the normalizer made of a stream.
fn render(events: &[RawEvent], stats: &normalizer::Stats) -> String {
    let mut out = String::new();
    for e in events {
        let detail = match &e.event {
            Event::RunStarted(p) => format!("agent={} model={}", p.agent, opt(&p.model)),
            Event::RunFinished(p) => format!("turns={}", p.turns),
            Event::RunFailed(p) => format!("turns={} reason={}", p.turns, p.reason),
            Event::RunRateLimited(_) => String::new(),
            Event::TurnStarted(p) => format!("index={}", p.index),
            Event::TurnFinished(p) => format!(
                "index={} subtype={} ok={} denials={} models={} cost_reconciles={}",
                p.index,
                p.subtype,
                p.ok,
                p.permission_denials,
                p.usage.models.len(),
                p.usage.cost_reconciles(),
            ),
            Event::UserMessage(p) => format!("replay={} len={}", p.is_replay, p.text.len()),
            Event::AssistantMessage(p) => format!("block={} len={}", p.block_index, p.text.len()),
            Event::AssistantThinking(p) => {
                format!(
                    "block={} disclosed={} len={}",
                    p.block_index,
                    p.disclosed,
                    p.text.len()
                )
            }
            Event::ToolStarted(p) => format!("name={} args={}", p.name, arg_keys(&p.input)),
            Event::ToolCompleted(p) => format!(
                "name={} is_error={} result={} non_execution={}",
                opt(&p.name),
                p.is_error,
                match &p.result {
                    Some(agent_protocol::ToolResult::Text(_)) => "text",
                    Some(agent_protocol::ToolResult::Structured(_)) => "structured",
                    None => "none",
                },
                opt(&p.non_execution),
            ),
            Event::PermissionResolved(p) => {
                format!(
                    "tool={} allowed={} reason_type={}",
                    p.tool_name,
                    p.allowed,
                    opt(&p.reason_type)
                )
            }
            Event::PermissionRequested(p) => format!("tool={}", p.tool_name),
            Event::WorkspaceReport(p) => {
                format!("tool={} args={}", p.tool, arg_keys(&p.payload))
            }
            Event::ProcessOutput(p) => format!("stream={}", p.stream),
            Event::Unclassified(p) => {
                let source = match (&p.source_type, &p.source_subtype) {
                    (Some(t), Some(s)) => format!("{t}/{s}"),
                    (Some(t), None) => t.clone(),
                    _ => "-".to_owned(),
                };
                format!("reason={:?} source={source}", p.reason)
            }
        };
        out.push_str(format!("{:>3}  {:<22} {}\n", e.seq, e.type_name(), detail).trim_end());
        out.push('\n');
    }
    out.push_str(&format!(
        "\nlines={} classified={} unclassified={} transient_dropped={}\n",
        stats.lines, stats.classified, stats.unclassified, stats.transient_dropped
    ));
    for (source, n) in &stats.by_source {
        out.push_str(&format!("  {source} {n}\n"));
    }
    out
}

fn opt(v: &Option<String>) -> &str {
    v.as_deref().unwrap_or("-")
}

/// Tool argument *keys* only — §6 source 1 is about the shape being typed, and
/// the values are long and full of absolute paths.
fn arg_keys(v: &serde_json::Value) -> String {
    match v.as_object() {
        Some(map) => {
            let mut keys: Vec<&str> = map.keys().map(String::as_str).collect();
            keys.sort();
            format!("{{{}}}", keys.join(","))
        }
        None => "-".to_owned(),
    }
}

#[test]
fn golden_output_for_every_fixture() {
    let update = std::env::var("UPDATE_GOLDEN").is_ok();
    fs::create_dir_all(golden_dir()).unwrap();

    let mut stale = Vec::new();
    for (name, path) in fixtures() {
        let (events, stats) = replay(&path);
        let actual = render(&events, &stats);
        let golden = golden_dir().join(format!("{name}.txt"));

        if update {
            fs::write(&golden, &actual).unwrap();
            continue;
        }

        let expected = fs::read_to_string(&golden).unwrap_or_else(|_| {
            panic!("no golden for {name}; regenerate with UPDATE_GOLDEN=1 cargo test")
        });
        if expected != actual {
            stale.push(name);
        }
    }

    assert!(
        stale.is_empty(),
        "normalizer output changed for: {stale:?}\n\
         If the change is intended, review it with UPDATE_GOLDEN=1 cargo test and commit the diff."
    );
}

// --------------------------------------------------------------- invariants --

/// §7's degradation invariant, checked mechanically: every line is classified,
/// preserved, or accounted for as transient — never silently lost.
#[test]
fn every_line_is_accounted_for() {
    for (name, path) in fixtures() {
        let (_, stats) = replay(&path);
        assert!(
            stats.balances(),
            "{name}: {} lines but {} classified + {} unclassified + {} transient",
            stats.lines,
            stats.classified,
            stats.unclassified,
            stats.transient_dropped
        );
    }
}

#[test]
fn seq_is_monotonic_and_starts_at_one() {
    for (name, path) in fixtures() {
        let (events, _) = replay(&path);
        assert!(!events.is_empty(), "{name}: produced no events");
        for (i, e) in events.iter().enumerate() {
            assert_eq!(e.seq, i as u64 + 1, "{name}: seq gap at index {i}");
            assert_eq!(e.schema, agent_protocol::SCHEMA_VERSION);
        }
    }
}

/// The log is forever, so everything in it has to survive a round-trip (§7).
#[test]
fn every_event_round_trips_through_json() {
    for (name, path) in fixtures() {
        let (events, _) = replay(&path);
        for e in &events {
            let json = serde_json::to_string(e).unwrap();
            let back: RawEvent = serde_json::from_str(&json)
                .unwrap_or_else(|err| panic!("{name}: seq {} failed to round-trip: {err}", e.seq));
            assert_eq!(&back, e, "{name}: seq {} changed on round-trip", e.seq);
        }
    }
}

/// Every run has exactly one start and exactly one end.
#[test]
fn exactly_one_run_lifecycle_per_fixture() {
    for (name, path) in fixtures() {
        let (events, _) = replay(&path);
        let counts = type_counts(&events);
        assert_eq!(
            counts.get("run.started").copied().unwrap_or(0),
            1,
            "{name}: expected exactly one run.started"
        );
        let ended = counts.get("run.finished").copied().unwrap_or(0)
            + counts.get("run.failed").copied().unwrap_or(0);
        assert_eq!(ended, 1, "{name}: expected exactly one run end");
    }
}

// ------------------------------------------- regressions: the six corrections --

/// §5 correction 1 and 2. `init` repeats per turn and `result` is per turn, so
/// the obvious 1:1 mapping invents a second run and ends the first one early.
#[test]
fn repeated_init_does_not_start_a_second_run() {
    let events = events_of("multi-turn");
    let counts = type_counts(&events);

    assert_eq!(
        counts["run.started"], 1,
        "init repeats but a run starts once"
    );
    assert_eq!(counts["turn.started"], 2, "two turns in one process");
    assert_eq!(counts["turn.finished"], 2);
    assert_eq!(counts.get("run.finished").copied().unwrap_or(0), 1);

    let turn_indexes: Vec<u32> = events
        .iter()
        .filter_map(|e| match &e.event {
            Event::TurnFinished(p) => Some(p.index),
            _ => None,
        })
        .collect();
    assert_eq!(
        turn_indexes,
        vec![0, 1],
        "turns keep the provider's ordering"
    );
}

/// §5. `--replay-user-messages` echoes our own stdin back at us. Rendering it
/// as content shows the user their own prompt twice.
#[test]
fn replayed_user_messages_are_marked_as_echoes() {
    let events = events_of("multi-turn");
    let replays = events
        .iter()
        .filter(|e| matches!(&e.event, Event::UserMessage(p) if p.is_replay))
        .count();
    assert_eq!(replays, 2, "both turns were acknowledged");
}

/// §5 correction 3. Thinking blocks arrive either way; the text is only
/// disclosed when the agent was launched with `--thinking-display summarized`.
#[test]
fn thinking_text_is_disclosed_only_when_requested() {
    let disclosed = events_of("thinking")
        .into_iter()
        .filter_map(|e| match e.event {
            Event::AssistantThinking(p) => Some(p),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(!disclosed.is_empty(), "the fixture has thinking blocks");
    assert!(
        disclosed.iter().any(|p| p.disclosed && !p.text.is_empty()),
        "recorded with --thinking-display summarized, so text should be present"
    );

    let undisclosed = events_of("tool-edit")
        .into_iter()
        .filter_map(|e| match e.event {
            Event::AssistantThinking(p) => Some(p),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(
        !undisclosed.is_empty() && undisclosed.iter().all(|p| !p.disclosed),
        "without the flag the block still arrives, carrying no text"
    );
}

/// §6. `tool_use_result` is an object on success and a bare string on failure.
/// A parser that assumes one shape panics the first time a command exits
/// non-zero.
#[test]
fn tool_results_change_type_with_the_outcome() {
    use agent_protocol::ToolResult;

    let ok = events_of("tool-bash")
        .into_iter()
        .filter_map(|e| match e.event {
            Event::ToolCompleted(p) => p.result,
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(
        ok.iter().any(|r| matches!(r, ToolResult::Structured(_))),
        "a successful Bash returns an object"
    );

    let failed = events_of("tool-bash-fail")
        .into_iter()
        .filter_map(|e| match e.event {
            Event::ToolCompleted(p) => Some(p),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(
        failed
            .iter()
            .any(|p| matches!(p.result, Some(ToolResult::Text(_)))),
        "a failing Bash returns a bare string"
    );
    assert!(failed.iter().any(|p| p.is_error), "and flags is_error");
}

/// §5. `non_execution_kind` is the only machine-readable way to tell a refusal
/// from a failure — the prose says both the same way.
#[test]
fn refusals_are_distinguishable_from_failures() {
    let refused = events_of("tool-bash-fail")
        .into_iter()
        .filter_map(|e| match e.event {
            Event::ToolCompleted(p) => p.non_execution,
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(
        refused.iter().any(|k| k == "user-rejected"),
        "expected a refusal marker, got {refused:?}"
    );
}

/// §5 correction 5. The trap: a run whose tool calls were all denied still
/// reports success. Keying failure on `is_error` silently calls it a good run.
#[test]
fn a_permission_blocked_turn_still_reports_success() {
    let events = events_of("permission-denied");

    let denied = events
        .iter()
        .filter(|e| matches!(&e.event, Event::PermissionResolved(p) if !p.allowed))
        .count();
    assert!(denied > 0, "the fixture contains a denial");

    let turn = events
        .iter()
        .find_map(|e| match &e.event {
            Event::TurnFinished(p) => Some(p),
            _ => None,
        })
        .expect("a turn finished");
    assert_eq!(turn.subtype, "success");
    assert!(turn.ok, "the provider calls it success, and we record that");

    // What matters is that the denial is in the log as its own fact, so a
    // review surface can show it without trusting the turn's own verdict.
    assert!(events
        .iter()
        .any(|e| matches!(&e.event, Event::PermissionResolved(_))));
}

/// §5 correction 6 and §14. Usage appears on every assistant line and is
/// identical each time; only `result` carries it once. And it must be read from
/// `modelUsage`, which is the only field that sees side-model spend.
#[test]
fn usage_is_counted_once_per_turn_and_includes_side_models() {
    let events = events_of("text-only");

    let usages: Vec<_> = events
        .iter()
        .filter_map(|e| match &e.event {
            Event::TurnFinished(p) => Some(&p.usage),
            _ => None,
        })
        .collect();
    assert_eq!(usages.len(), 1, "one turn, one usage record");

    let usage = usages[0];
    assert!(
        usage.models.len() > 1,
        "every recorded run spends on a side model that result.usage omits; \
         got {:?}",
        usage.models.iter().map(|m| &m.model).collect::<Vec<_>>()
    );
    assert!(
        usage.cost_reconciles(),
        "per-model costs must add up to total_cost_usd: {:?} vs {}",
        usage.models.iter().map(|m| m.cost_usd).collect::<Vec<_>>(),
        usage.cost_usd
    );
    assert!(usage.input_tokens > 0 && usage.output_tokens > 0);
}

/// A turn that ends badly fails the run, and the reason is the provider's
/// `subtype` rather than our guess.
#[test]
fn a_failing_turn_fails_the_run() {
    let events = events_of("error");
    let failed = events
        .iter()
        .find_map(|e| match &e.event {
            Event::RunFailed(p) => Some(p),
            _ => None,
        })
        .expect("error fixture should fail the run");
    assert!(
        failed.reason.contains("error_max_turns"),
        "reason should name the provider's subtype, got {:?}",
        failed.reason
    );
}

/// §6 source 1: the claim the whole review surface rests on. `Edit` gives the
/// changed file exactly, as an absolute path, with no inference.
#[test]
fn edit_tool_calls_carry_typed_arguments() {
    let edit = events_of("tool-edit")
        .into_iter()
        .find_map(|e| match e.event {
            Event::ToolStarted(p) if p.name == "Edit" => Some(p),
            _ => None,
        })
        .expect("the fixture edits a file");

    let args = edit.input.as_object().expect("typed arguments");
    for key in ["file_path", "old_string", "new_string"] {
        assert!(args.contains_key(key), "Edit should carry {key}");
    }
    let path = args["file_path"].as_str().unwrap();
    assert!(path.starts_with('/'), "file_path is absolute: {path}");
}

// ------------------------------------------------------- degradation, proved --

/// The invariant is only worth stating if it survives hostile input.
#[test]
fn garbage_is_preserved_rather_than_dropped() {
    let mut n = ClaudeCodeNormalizer::with_clock("run_x".into(), Box::new(FixedClock::default()));

    let cases = [
        ("not json at all", "NotJson"),
        (r#"{"no_type": true}"#, "UnknownType"),
        (r#"{"type": "invented_by_a_future_release"}"#, "UnknownType"),
        (r#"{"type": "assistant", "message": {}}"#, "MalformedShape"),
    ];

    for (line, expected) in cases {
        let events = n.push_line(line);
        assert_eq!(events.len(), 1, "one event per unparseable line: {line}");
        match &events[0].event {
            Event::Unclassified(p) => {
                assert_eq!(format!("{:?}", p.reason), expected, "for input {line}");
                assert!(!p.line.is_empty(), "the original is kept");
            }
            other => panic!("expected unclassified for {line}, got {other:?}"),
        }
    }

    assert_eq!(n.stats().unclassified, 4);
    assert!(n.stats().balances());
}

/// Blank lines are not data and must not consume a seq number.
#[test]
fn blank_lines_produce_nothing() {
    let mut n = ClaudeCodeNormalizer::with_clock("run_x".into(), Box::new(FixedClock::default()));
    assert!(n.push_line("").is_empty());
    assert!(n.push_line("   \t ").is_empty());
    assert_eq!(n.stats().lines, 0);
}

/// A stream that dies before saying anything is a failed run, not a silent one.
#[test]
fn a_stream_that_never_starts_is_a_failure() {
    let mut n = ClaudeCodeNormalizer::with_clock("run_x".into(), Box::new(FixedClock::default()));
    let events = n.finish(Some(1));
    assert!(matches!(events[0].event, Event::RunFailed(_)));
}

/// A `tool_result` carries only `tool_use_id`; the provider never repeats the
/// tool's name. The normalizer already correlates the two, so the completion
/// carries it — otherwise every consumer rebuilds the same map, and the shadow
/// snapshot a tool call causes (§13.1) cannot be labelled with what caused it.
#[test]
fn a_completion_names_the_tool_that_produced_it() {
    let mut checked = 0;
    for (name, _) in fixtures() {
        let events = events_of(&name);
        let started: BTreeMap<String, String> = events
            .iter()
            .filter_map(|e| match &e.event {
                Event::ToolStarted(p) => Some((p.tool_use_id.0.clone(), p.name.clone())),
                _ => None,
            })
            .collect();
        for e in &events {
            let Event::ToolCompleted(p) = &e.event else {
                continue;
            };
            let expected = started.get(&p.tool_use_id.0);
            assert_eq!(
                p.name.as_ref(),
                expected,
                "{name}: completion of {} names the wrong tool",
                p.tool_use_id
            );
            if expected.is_some() {
                checked += 1;
            }
        }
    }
    assert!(checked > 0, "no fixture exercised a tool completion");
}

// ------------------------------------------- regressions: workspace reports --

/// One `assistant` line, ready to hand to `push_line`.
fn assistant_line(uuid: &str, blocks: &str) -> String {
    format!(
        r#"{{"type":"assistant","uuid":"{uuid}","message":{{"id":"msg_1","content":[{blocks}]}}}}"#
    )
}

fn tool_use_block(id: &str, name: &str, input: &str) -> String {
    format!(r#"{{"type":"tool_use","id":"{id}","name":"{name}","input":{input}}}"#)
}

fn tool_result_line(uuid: &str, id: &str, is_error: bool) -> String {
    format!(
        r#"{{"type":"user","uuid":"{uuid}","message":{{"content":[{{"type":"tool_result","tool_use_id":"{id}","is_error":{is_error},"content":"ok"}}]}}}}"#
    )
}

fn normalizer() -> ClaudeCodeNormalizer {
    ClaudeCodeNormalizer::with_clock("run_x".into(), Box::new(FixedClock::default()))
}

/// §6 source 3 and §8. A call to our own workspace MCP server is a *report*,
/// and it stands in for `tool.started` rather than joining it: emitting both
/// would make one derivable from the other by prefix, and a projection does not
/// belong in the log.
#[test]
fn a_workspace_tool_call_becomes_a_report_rather_than_a_tool_call() {
    let mut n = normalizer();
    let events = n.push_line(&assistant_line(
        "u1",
        &tool_use_block(
            "toolu_1",
            "mcp__agent-workspace__report_finding",
            r#"{"severity":"high","title":"unbounded loop"}"#,
        ),
    ));

    assert_eq!(events.len(), 1, "one content block, one event");
    let Event::WorkspaceReport(report) = &events[0].event else {
        panic!("expected workspace.report, got {:?}", events[0].event);
    };
    assert_eq!(
        report.tool, "report_finding",
        "the bare tool name, not the wire spelling"
    );
    assert_eq!(
        report.payload["severity"], "high",
        "the tool_use input verbatim"
    );
    assert_eq!(report.payload["title"], "unbounded loop");

    assert!(
        !events
            .iter()
            .any(|e| matches!(e.event, Event::ToolStarted(_))),
        "the report replaces tool.started, it does not accompany it"
    );
    assert_eq!(
        events[0].idempotency_key.as_deref(),
        Some("u1"),
        "a single event keeps the provider uuid unsuffixed"
    );
    assert!(n.stats().balances());
}

/// The deliberate asymmetry: the report replaces `tool.started`, but the
/// matching `tool_result` still becomes `tool.completed`. It carries `is_error`
/// — the only record that our own server rejected a payload — so suppressing it
/// would lose an observed fact.
#[test]
fn a_workspace_tool_result_still_completes_and_names_the_tool() {
    let mut n = normalizer();
    n.push_line(&assistant_line(
        "u1",
        &tool_use_block(
            "toolu_1",
            "mcp__agent-workspace__open_plan",
            r#"{"steps":[]}"#,
        ),
    ));
    let events = n.push_line(&tool_result_line("u2", "toolu_1", true));

    assert_eq!(events.len(), 1);
    let Event::ToolCompleted(done) = &events[0].event else {
        panic!("expected tool.completed, got {:?}", events[0].event);
    };
    assert_eq!(
        done.name.as_deref(),
        Some("mcp__agent-workspace__open_plan"),
        "correlation still names the tool, by its wire spelling"
    );
    assert!(done.is_error, "a rejection by our own server is recorded");
    assert!(n.stats().balances());
}

/// Only our own server's calls are reports. Another MCP server's tools are
/// ordinary tool calls and must stay that way.
#[test]
fn an_mcp_call_to_another_server_stays_an_ordinary_tool_call() {
    let mut n = normalizer();
    let events = n.push_line(&assistant_line(
        "u1",
        &tool_use_block(
            "toolu_1",
            "mcp__github__create_issue",
            r#"{"title":"flaky test"}"#,
        ),
    ));

    assert_eq!(events.len(), 1);
    let Event::ToolStarted(started) = &events[0].event else {
        panic!("expected tool.started, got {:?}", events[0].event);
    };
    assert_eq!(
        started.name, "mcp__github__create_issue",
        "the name is untouched"
    );
}

/// The change must be invisible to every tool that is not ours.
#[test]
fn an_ordinary_tool_call_is_unaffected() {
    let mut n = normalizer();
    let events = n.push_line(&assistant_line(
        "u1",
        &tool_use_block("toolu_1", "Edit", r#"{"file_path":"/tmp/a.rs"}"#),
    ));

    assert_eq!(events.len(), 1);
    let Event::ToolStarted(started) = &events[0].event else {
        panic!("expected tool.started, got {:?}", events[0].event);
    };
    assert_eq!(started.name, "Edit");
    assert_eq!(started.tool_use_id.0, "toolu_1");
    assert_eq!(started.input["file_path"], "/tmp/a.rs");
}

/// An `assistant` line fans out per content block, and only some blocks are
/// workspace calls. The other blocks keep their own event kinds, and the
/// `#{i}` idempotency suffix still makes every event on the line distinct.
#[test]
fn a_mixed_assistant_line_keeps_one_key_per_block() {
    let mut n = normalizer();
    let blocks = format!(
        r#"{{"type":"text","text":"here goes"}},{},{}"#,
        tool_use_block("toolu_1", "Read", r#"{"file_path":"/tmp/a.rs"}"#),
        tool_use_block(
            "toolu_2",
            "mcp__agent-workspace__note_decision",
            r#"{"title":"use a queue"}"#
        ),
    );
    let events = n.push_line(&assistant_line("u1", &blocks));

    let kinds: Vec<&str> = events.iter().map(|e| e.type_name()).collect();
    assert_eq!(
        kinds,
        vec!["assistant.message", "tool.started", "workspace.report"],
        "each block keeps its own kind, in order"
    );

    let keys: Vec<Option<&str>> = events
        .iter()
        .map(|e| e.idempotency_key.as_deref())
        .collect();
    assert_eq!(
        keys,
        vec![Some("u1#0"), Some("u1#1"), Some("u1#2")],
        "a fanned-out line suffixes each event's key"
    );

    let seqs: Vec<u64> = events.iter().map(|e| e.seq).collect();
    assert_eq!(seqs, vec![1, 2, 3], "seq assignment is unchanged");

    // Both calls still correlate, whichever event kind they became.
    let done = n.push_line(&tool_result_line("u2", "toolu_2", false));
    let Event::ToolCompleted(p) = &done[0].event else {
        panic!("expected tool.completed");
    };
    assert_eq!(
        p.name.as_deref(),
        Some("mcp__agent-workspace__note_decision")
    );

    assert_eq!(n.stats().classified, 2, "two lines, both classified");
    assert!(n.stats().balances());
}
