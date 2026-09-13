//! Every recorded stream, projected into §7's feed.
//!
//! The normalizer's fixture suite asks *what did we make of this line*; this
//! one asks *what does the feed make of the run*. Same discipline, one layer
//! up:
//!
//! - **Golden output** per fixture, so a change in what a block *is* shows up
//!   as a reviewable diff rather than as a surface that quietly reads
//!   differently. Regenerate with `UPDATE_GOLDEN=1 cargo test`.
//! - **The degradation invariant**, asserted rather than believed: no event of
//!   any fixture is dropped, and an unclassified provider line survives into a
//!   `Raw` block byte for byte.
//! - **Agreement with the fork projection**, because two surfaces reading one
//!   rule is only safe while they agree.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use agent_protocol::{Event, ProcessOutput, RawEvent, RunId, SCHEMA_VERSION};
use document_model::{Block, Body, Feed, Speaker, ToolStatus};
use normalizer::{ClaudeCodeNormalizer, FixedClock};

/// The version the named assertions below are about, pinned for the same reason
/// the normalizer's are: a regression test that follows the newest recording
/// stops being one.
const BASELINE: &str = "2.1.269";

fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/claude-code")
}

fn golden_dir(version: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(version)
}

fn versions() -> Vec<String> {
    let mut out: Vec<String> = fs::read_dir(fixture_root())
        .expect("fixtures/claude-code is missing — see MANIFEST.md")
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    out.sort();
    assert!(
        out.contains(&BASELINE.to_owned()),
        "the baseline set is gone"
    );
    out
}

/// `(version, name, path)` for every recorded stream, across every version.
fn fixtures() -> Vec<(String, String, PathBuf)> {
    let mut out = Vec::new();
    for version in versions() {
        for entry in fs::read_dir(fixture_root().join(&version)).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_none_or(|x| x != "ndjson") {
                continue;
            }
            let name = path.file_stem().unwrap().to_string_lossy().into_owned();
            out.push((version.clone(), name, path));
        }
    }
    out.sort();
    assert!(!out.is_empty(), "no fixtures found");
    out
}

fn run_id() -> RunId {
    RunId::from("run_test")
}

/// Normalize a whole fixture, finishing the run as a clean exit.
fn replay(path: &Path) -> Vec<RawEvent> {
    let text = fs::read_to_string(path).unwrap();
    let mut n = ClaudeCodeNormalizer::with_clock(run_id(), Box::new(FixedClock::default()));
    let mut events: Vec<RawEvent> = text.lines().flat_map(|l| n.push_line(l)).collect();
    events.extend(n.finish(Some(0)));
    events
}

fn feed_of(version: &str, name: &str) -> Feed {
    let events = replay(&fixture_root().join(version).join(format!("{name}.ndjson")));
    document_model::project(&run_id(), &events)
}

// ------------------------------------------------------------------ goldens --

/// A stable, readable rendering of what the feed made of a stream.
///
/// Deliberately the block's *address* and its collapsed line rather than its
/// whole payload: the golden is for reviewing a change in interpretation, and a
/// dump of every hunk and every tool result would bury that in kilobytes of
/// content the normalizer's own goldens already cover.
fn render(feed: &Feed) -> String {
    let mut out = String::new();
    for block in &feed.blocks {
        let seqs = block
            .seqs
            .iter()
            .map(u64::to_string)
            .collect::<Vec<_>>()
            .join(",");
        let fork = match block.fork_seq {
            Some(seq) => format!("fork@{seq}"),
            None => "-".to_owned(),
        };
        out.push_str(
            format!(
                "{:>9}  {:<10} {:<8} {}\n",
                seqs,
                block.type_name(),
                fork,
                block.summary
            )
            .trim_end(),
        );
        out.push('\n');
    }
    out.push_str(&format!(
        "\nblocks={} version={} addressable={}\n",
        feed.blocks.len(),
        feed.version,
        feed.blocks.iter().filter(|b| b.fork_seq.is_some()).count()
    ));
    out
}

#[test]
fn goldens_match() {
    let update = std::env::var_os("UPDATE_GOLDEN").is_some();
    let mut stale = Vec::new();
    for (version, name, path) in fixtures() {
        let feed = document_model::project(&run_id(), &replay(&path));
        let rendered = render(&feed);
        let golden = golden_dir(&version).join(format!("{name}.txt"));
        if update {
            fs::create_dir_all(golden.parent().unwrap()).unwrap();
            fs::write(&golden, &rendered).unwrap();
            continue;
        }
        let expected = fs::read_to_string(&golden).unwrap_or_else(|_| {
            panic!("no golden for {version}/{name} — run UPDATE_GOLDEN=1 cargo test")
        });
        if expected != rendered {
            stale.push(format!(
                "--- {version}/{name}\n{}\n+++ actual\n{rendered}",
                expected
            ));
        }
    }
    assert!(
        stale.is_empty(),
        "block projection changed:\n{}",
        stale.join("\n")
    );
}

// ------------------------------------------------- the degradation invariant --

/// §7, stated as law: "No event is ever dropped."
///
/// The feed is the surface that could break it, because a projection is exactly
/// the place where an event gets quietly interpreted into nothing. So: every
/// `seq` the normalizer produced is claimed by some block, and every `seq` a
/// block claims is a real event of the run. Both directions, every fixture.
#[test]
fn no_event_is_dropped_by_any_fixture() {
    for (version, name, path) in fixtures() {
        let events = replay(&path);
        let feed = document_model::project(&run_id(), &events);
        let produced: BTreeSet<u64> = events.iter().map(|e| e.seq).collect();
        let claimed: BTreeSet<u64> = feed
            .blocks
            .iter()
            .flat_map(|b| b.seqs.iter().copied())
            .collect();
        assert_eq!(
            produced, claimed,
            "{version}/{name}: the feed does not account for exactly the run's events"
        );
        assert!(
            feed.blocks.iter().all(|b| !b.seqs.is_empty()),
            "{version}/{name}: a block claims no event, so it has no address"
        );
    }
}

/// The other half of the invariant: "the unprocessed transcript is always one
/// click away".
///
/// A line the *normalizer* could not classify reaches the feed with the
/// provider's bytes still on it. Every fixture has two — the hook lifecycle §5
/// says to ignore — so this is checked against real recorded input rather than
/// a constructed one.
#[test]
fn an_unclassified_line_survives_into_the_block_verbatim() {
    let path = fixture_root().join(BASELINE).join("tool-edit.ndjson");
    let source = fs::read_to_string(&path).unwrap();
    let feed = document_model::project(&run_id(), &replay(&path));

    let raws: Vec<&String> = feed
        .blocks
        .iter()
        .filter_map(|b| match &b.body {
            Body::Raw(r) => r.line.as_ref(),
            _ => None,
        })
        .collect();
    assert_eq!(
        raws.len(),
        2,
        "the hook lifecycle is two unclassified lines"
    );
    for line in raws {
        assert!(
            source.lines().any(|l| l == line),
            "a raw block's line is not one of the fixture's, byte for byte"
        );
    }
}

/// An event type this projection has no block for degrades rather than
/// disappearing.
///
/// `process.output` is the live example — a tier 3 PTY agent's stdout, which
/// nothing produces yet — and it stands in for the general case: the day
/// `agent-protocol` grows a variant, the feed shows it as `Raw` with its
/// payload intact instead of rendering a gap.
#[test]
fn an_event_with_no_block_type_becomes_raw() {
    let event = RawEvent {
        run_id: run_id(),
        seq: 1,
        ts: "2026-09-13T14:22:08.412Z".to_owned(),
        schema: SCHEMA_VERSION,
        idempotency_key: None,
        event: Event::ProcessOutput(ProcessOutput {
            stream: "stdout".to_owned(),
            text: "hello".to_owned(),
        }),
    };
    let feed = document_model::project(&run_id(), &[event]);
    let [block] = &feed.blocks[..] else {
        panic!("one event, one block");
    };
    let Body::Raw(raw) = &block.body else {
        panic!("an unmodeled event must land in a raw block, not vanish");
    };
    assert_eq!(raw.event_type, "process.output");
    assert_eq!(raw.payload["text"], "hello");
    assert_eq!(block.seqs, vec![1]);
}

/// §7: "block payloads must round-trip through JSON without loss — the event
/// log is forever."
///
/// Every fixture, both directions. This is also what keeps §9.1 honest: a range
/// action extracts *block JSON* and hands it to another agent, so a payload
/// that does not survive `serde` is a summary written from a lossy copy.
#[test]
fn every_document_round_trips_through_json() {
    for (version, name, path) in fixtures() {
        let feed = document_model::project(&run_id(), &replay(&path));
        let json = serde_json::to_string(&feed).unwrap();
        let back: Feed = serde_json::from_str(&json).unwrap();
        assert_eq!(
            feed, back,
            "{version}/{name} did not survive a JSON round trip"
        );
    }
}

// ---------------------------------------------------------- what a block is --

/// Several events fan into one block, which is the whole shape of §7.
#[test]
fn a_tool_call_is_one_block_over_its_start_and_its_result() {
    let feed = feed_of(BASELINE, "tool-read");
    let call = feed
        .blocks
        .iter()
        .find(|b| matches!(&b.body, Body::ToolCall(c) if c.name == "Read"))
        .expect("the Read call");
    assert_eq!(call.seqs.len(), 2, "start and completion, one block");
    assert!(matches!(&call.body, Body::ToolCall(c) if c.status == ToolStatus::Ok));
    assert!(call.seq_end() > call.seq_start());
}

/// An `Edit` reads as the change it made, not as a tool call with two long
/// string arguments (§6 source 1).
#[test]
fn an_edit_is_a_diff_block_with_stats() {
    let feed = feed_of(BASELINE, "tool-edit");
    let diff = feed
        .blocks
        .iter()
        .find_map(|b| match &b.body {
            Body::Diff(d) => Some(d),
            _ => None,
        })
        .expect("the Edit call");
    assert_eq!(diff.tool, "Edit");
    assert_eq!(diff.files.len(), 1);
    assert!(diff.stats.additions > 0 || diff.stats.deletions > 0);
    assert_eq!(diff.status, ToolStatus::Ok);
}

/// A `Bash` call reads as the command and its output.
#[test]
fn a_bash_call_is_a_terminal_block() {
    let feed = feed_of(BASELINE, "tool-bash");
    let term = feed
        .blocks
        .iter()
        .find_map(|b| match &b.body {
            Body::Terminal(t) => Some(t),
            _ => None,
        })
        .expect("the Bash call");
    assert!(!term.command.is_empty());
    assert_eq!(term.status, ToolStatus::Ok);
    // §6: the exit code is not on the wire in either shape of
    // `tool_use_result`. The field exists to say so rather than to be invented
    // from `is_error`.
    assert_eq!(term.exit_code, None);
}

/// A refused call is `Denied` rather than `Failed`. The provider reports it as
/// an error and `non_execution` is the only machine-readable difference (§5).
#[test]
fn a_permission_denied_call_is_denied_and_not_merely_failed() {
    let feed = feed_of(BASELINE, "permission-denied");
    let denied = feed.blocks.iter().any(|b| match &b.body {
        Body::ToolCall(c) => c.status == ToolStatus::Denied,
        Body::Diff(d) => d.status == ToolStatus::Denied,
        Body::Terminal(t) => t.status == ToolStatus::Denied,
        _ => false,
    });
    assert!(denied, "the refused call should read as denied");
}

/// Thinking that was not disclosed is a block that says so, not an empty one:
/// §5 is explicit that an empty text means "not disclosed", never "did not
/// think". The two fixtures are the two halves — `thinking` was recorded with
/// `--thinking-display summarized` and `tool-edit` was not.
#[test]
fn thinking_says_whether_it_was_disclosed() {
    let disclosed = feed_of(BASELINE, "thinking");
    let block = disclosed
        .blocks
        .iter()
        .find(|b| matches!(&b.body, Body::Text(t) if t.speaker == Speaker::Thinking))
        .expect("a thinking block");
    assert!(matches!(&block.body, Body::Text(t) if t.disclosed && !t.text.is_empty()));
    assert!(block.summary.starts_with("thinking · "));

    let silent = feed_of(BASELINE, "tool-edit");
    let block = silent
        .blocks
        .iter()
        .find(|b| matches!(&b.body, Body::Text(t) if t.speaker == Speaker::Thinking))
        .expect("a thinking block");
    assert_eq!(block.summary, "thinking · not disclosed");
}

// ------------------------------------------------------------ addressability --

/// The feed must never offer a fork the driver would refuse.
///
/// `EventLog::fork_points` and this projection read the same function —
/// `normalizer::claude_code::fork_address` — and this is the test that says the
/// two readings agree. Not equality: a tool call covers two addressable events
/// and offers the later one, so the feed's addresses are a *subset* of the
/// log's. The direction that matters is this one, because the other failure is
/// a missing affordance and this one is a broken promise.
#[test]
fn every_address_the_feed_offers_is_one_the_log_would_fork_at() {
    for (version, name, path) in fixtures() {
        let events = replay(&path);
        let feed = document_model::project(&run_id(), &events);

        let mut log = event_log::EventLog::open_in_memory().unwrap();
        log.create_run(&run(&run_id())).unwrap();
        log.append_all(&events).unwrap();
        let points = log.fork_points(std::slice::from_ref(&run_id())).unwrap();
        let addressable: BTreeSet<u64> = points[0].blocks.iter().map(|b| b.seq).collect();

        for block in &feed.blocks {
            let Some(seq) = block.fork_seq else { continue };
            assert!(
                addressable.contains(&seq),
                "{version}/{name}: the feed offers a fork at seq {seq}, which \
                 EventLog::fork_points does not"
            );
            assert!(
                block.seqs.contains(&seq),
                "{version}/{name}: a block's address is not one of its own events"
            );
        }
    }
}

/// A tool call offers its *completion* as the address, so a fork taken at it
/// starts with the result of the call the reader pointed at.
#[test]
fn a_tool_call_is_addressed_at_its_result() {
    let feed = feed_of(BASELINE, "tool-read");
    let call = feed
        .blocks
        .iter()
        .find(|b| matches!(&b.body, Body::ToolCall(c) if c.name == "Read"))
        .expect("the Read call");
    assert_eq!(call.fork_seq, Some(call.seq_end()));
}

/// Lifecycle is keyed and is not a chain entry, so it offers nothing
/// (`claude-code-fork-anchor-types.md`: nine of fifteen keyed kinds are
/// refused).
#[test]
fn a_lifecycle_block_offers_no_fork() {
    let feed = feed_of(BASELINE, "text-only");
    for block in &feed.blocks {
        let Body::Raw(raw) = &block.body else {
            continue;
        };
        if raw.event_type == "run.started" || raw.event_type.starts_with("turn.") {
            assert_eq!(
                block.fork_seq, None,
                "{} is keyed but is not a chain entry",
                raw.event_type
            );
        }
    }
}

fn run(id: &RunId) -> event_log::Run {
    event_log::Run {
        id: id.clone(),
        agent: "claude-code".to_owned(),
        agent_version: None,
        provider_session_id: None,
        workspace_path: "/tmp/ws".to_owned(),
        worktree_path: None,
        branch: None,
        shadow_ref: None,
        prompt: None,
        model: None,
        status: event_log::RunStatus::Running,
        started_at: "2026-09-13T14:00:00Z".to_owned(),
        finished_at: None,
        parent_run_id: None,
        branch_point_seq: None,
    }
}

/// Blocks in a live run are not a different shape from blocks in a finished
/// one: a call with no result yet is `Running`, which is what lets the same
/// projection serve a streaming feed and a replayed one.
#[test]
fn an_unfinished_call_is_running_rather_than_absent() {
    let events = replay(&fixture_root().join(BASELINE).join("tool-read.ndjson"));
    let cut = events
        .iter()
        .position(|e| e.type_name() == "tool.completed")
        .expect("a completion to cut before");
    let feed = document_model::project(&run_id(), &events[..cut]);
    let running = feed.blocks.iter().any(|b| match &b.body {
        Body::ToolCall(c) => c.status == ToolStatus::Running,
        _ => false,
    });
    assert!(
        running,
        "a call whose result has not arrived is still a block"
    );
}

/// The fan-out direction: one `assistant` line, two blocks, one address.
#[test]
fn a_fenced_example_becomes_its_own_code_block() {
    let mut events = replay(&fixture_root().join(BASELINE).join("text-only.ndjson"));
    let message = events
        .iter_mut()
        .find(|e| e.type_name() == "assistant.message")
        .expect("an assistant message");
    let seq = message.seq;
    if let Event::AssistantMessage(m) = &mut message.event {
        m.text = "before\n```rust\nfn main() {}\n```\nafter".to_owned();
    }
    let feed = document_model::project(&run_id(), &events);
    let from_message: Vec<&Block> = feed.blocks.iter().filter(|b| b.seqs == [seq]).collect();
    assert_eq!(from_message.len(), 3, "prose, code, prose");
    assert!(matches!(from_message[1].body, Body::Code(_)));
    // One chain entry, so all three address the same message.
    assert!(from_message.iter().all(|b| b.fork_seq == Some(seq)));
}
