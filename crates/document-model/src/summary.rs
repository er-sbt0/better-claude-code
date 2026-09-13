//! One line per block, for the collapsed state.
//!
//! This is where `apps/desktop/src/lib/describe.ts` went. That file summarized
//! each raw event in TypeScript and said of itself that "§7's run feed is a
//! different thing entirely […] none of this survives into it" — which was
//! right about the shape and incomplete about the reason. Summarizing is
//! *deciding what a block is about*, and the shell owns no logic (roadmap §8):
//! a rule that lives in the webview is a rule no test in this workspace can
//! reach.
//!
//! Collapse is first-class in §3's row, so this is not decoration. It is the
//! entire content of a collapsed block, and a feed of forty collapsed blocks is
//! forty of these lines.

use crate::block::{Body, Speaker, ToolStatus};

/// How much of a line survives. Long enough to identify a block, short enough
/// that a collapsed feed is a list rather than a paragraph.
const MAX: usize = 120;

pub fn of(body: &Body) -> String {
    match body {
        Body::Text(t) => {
            let who = match t.speaker {
                Speaker::User => "you",
                Speaker::Assistant => "agent",
                Speaker::Thinking => "thinking",
            };
            if t.speaker == Speaker::Thinking && !t.disclosed {
                // §5: the block arrived carrying a signature and no text. "Not
                // disclosed" is a fact about the launch flags, never about
                // whether the agent thought.
                return "thinking · not disclosed".to_owned();
            }
            format!("{who} · {}", clip(&t.text, MAX - who.len() - 3))
        }
        Body::Code(c) => {
            let lines = c.code.lines().count();
            let lang = c.language.as_deref().unwrap_or("code");
            format!("{lang} · {lines} line{}", if lines == 1 { "" } else { "s" })
        }
        Body::ToolCall(call) => {
            let arg = primary_arg(&call.input);
            format!(
                "{}{} {}",
                call.name,
                arg.map(|a| format!(" {a}")).unwrap_or_default(),
                mark(call.status)
            )
            .trim_end()
            .to_owned()
        }
        Body::Diff(d) => format!(
            "{} {} +{} −{} {}",
            d.tool,
            d.files.join(", "),
            d.stats.additions,
            d.stats.deletions,
            mark(d.status)
        )
        .trim_end()
        .to_owned(),
        Body::Terminal(t) => format!("$ {} {}", clip(&t.command, MAX - 4), mark(t.status))
            .trim_end()
            .to_owned(),
        Body::Raw(r) => match (&r.line, r.payload.get("reason")) {
            // A line the normalizer could not classify says which kind of
            // failure it was — §16's early warning that a provider changed
            // something, and the one number in `Health` worth alerting on.
            (Some(_), Some(reason)) => format!(
                "{} · {}",
                r.event_type,
                reason.as_str().unwrap_or("unclassified")
            ),
            _ => format!("{} · {}", r.event_type, clip(&r.payload.to_string(), 80)),
        },
    }
}

/// The status, in one character, or nothing at all when it is the ordinary one.
///
/// `Ok` is silent on purpose: a collapsed feed where every successful call
/// carries a tick is a column of ticks, and the states worth seeing at a glance
/// are the other three.
fn mark(status: ToolStatus) -> &'static str {
    match status {
        ToolStatus::Running => "…",
        ToolStatus::Ok => "",
        ToolStatus::Failed => "✗",
        ToolStatus::Denied => "⊘ denied",
    }
}

/// The argument a tool call is *about*, when one of them plainly is.
///
/// Not a general rendering of the input: a `Grep` is about its pattern and a
/// `Read` about its path, and the alternative — every key and value clipped to
/// a line — says nothing in more characters. A tool whose arguments are not on
/// this list gets its name alone, and the expanded block shows the arguments
/// verbatim either way.
fn primary_arg(input: &serde_json::Value) -> Option<String> {
    for key in [
        "file_path",
        "path",
        "command",
        "pattern",
        "query",
        "url",
        "title",
        "prompt",
    ] {
        if let Some(s) = input.get(key).and_then(serde_json::Value::as_str) {
            if !s.is_empty() {
                return Some(clip(s, 80));
            }
        }
    }
    None
}

/// Long text on one line, clipped on a character boundary.
fn clip(text: &str, max: usize) -> String {
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.chars().count() <= max {
        return line;
    }
    let mut out: String = line.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::{Code, Text};

    #[test]
    fn undisclosed_thinking_says_so_rather_than_showing_nothing() {
        let body = Body::Text(Text {
            speaker: Speaker::Thinking,
            text: String::new(),
            disclosed: false,
        });
        assert_eq!(of(&body), "thinking · not disclosed");
    }

    #[test]
    fn a_long_line_is_clipped_on_a_character_boundary() {
        let body = Body::Text(Text {
            speaker: Speaker::Assistant,
            text: "é".repeat(400),
            disclosed: true,
        });
        assert!(of(&body).ends_with('…'));
        assert!(of(&body).chars().count() <= MAX);
    }

    #[test]
    fn code_is_counted_rather_than_quoted() {
        let body = Body::Code(Code {
            language: Some("rust".to_owned()),
            code: "fn a() {}\nfn b() {}\n".to_owned(),
        });
        assert_eq!(of(&body), "rust · 2 lines");
    }
}
