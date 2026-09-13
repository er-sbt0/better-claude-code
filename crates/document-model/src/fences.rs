//! Splitting prose into §7's `Text` and `Code` blocks.
//!
//! `Code` is "fenced code, language-tagged", and the only place a fence comes
//! from is the assistant's own prose: there is no separate code event on the
//! wire (§6). So the fence is where one `assistant.message` becomes several
//! blocks — which is the second half of "several events fan into one block, one
//! message fans into several".
//!
//! Deliberately not a Markdown parser. It recognizes a fence and nothing else:
//! no lists, no headings, no inline spans. §7 puts Markdown at the edge of the
//! system as interchange, and a renderer that started interpreting prose would
//! be deciding what the agent meant.

/// A run of prose, or a fenced block of code.
#[derive(Debug, Clone, PartialEq)]
pub enum Chunk {
    Prose(String),
    Code {
        language: Option<String>,
        code: String,
    },
}

/// Split text at fenced code blocks.
///
/// The rules are CommonMark's, narrowed to what the provider actually emits:
///
/// - a fence opens on a line of three or more backticks or tildes, optionally
///   indented up to three spaces, with an info string after it;
/// - it closes on a line of the same character, at least as long, with nothing
///   else on it;
/// - **an unclosed fence runs to the end of the text.** Prose that opened a
///   fence and never closed it is code the provider got cut off in the middle
///   of, and showing it as prose would render a half-written program as
///   sentences. Nothing is lost either way — the raw event is one click away.
///
/// Empty prose runs are dropped; an input that yields nothing at all yields one
/// empty prose chunk, because the caller must be able to address the event.
pub fn split(text: &str) -> Vec<Chunk> {
    let mut out: Vec<Chunk> = Vec::new();
    let mut prose = String::new();
    let mut fence: Option<(char, usize, Option<String>)> = None;
    let mut code = String::new();

    for line in text.split_inclusive('\n') {
        match &fence {
            None => match open(line) {
                Some((ch, len, info)) => {
                    flush_prose(&mut out, &mut prose);
                    fence = Some((ch, len, info));
                }
                None => prose.push_str(line),
            },
            Some((ch, len, info)) => {
                if closes(line, *ch, *len) {
                    out.push(Chunk::Code {
                        language: info.clone(),
                        code: std::mem::take(&mut code),
                    });
                    fence = None;
                } else {
                    code.push_str(line);
                }
            }
        }
    }

    if let Some((_, _, info)) = fence {
        out.push(Chunk::Code {
            language: info,
            code: std::mem::take(&mut code),
        });
    }
    flush_prose(&mut out, &mut prose);

    if out.is_empty() {
        out.push(Chunk::Prose(text.to_owned()));
    }
    out
}

fn flush_prose(out: &mut Vec<Chunk>, prose: &mut String) {
    let taken = std::mem::take(prose);
    if !taken.trim().is_empty() {
        out.push(Chunk::Prose(taken.trim_end_matches('\n').to_owned()));
    }
}

/// `(fence character, fence length, info string)` if this line opens a fence.
fn open(line: &str) -> Option<(char, usize, Option<String>)> {
    let body = strip_indent(line)?;
    let ch = body.chars().next()?;
    if ch != '`' && ch != '~' {
        return None;
    }
    let len = body.chars().take_while(|c| *c == ch).count();
    if len < 3 {
        return None;
    }
    let info = body[len..].trim();
    // A backtick fence may not carry a backtick in its info string (CommonMark),
    // which is what keeps an inline ``code`` span from opening a block.
    if ch == '`' && info.contains('`') {
        return None;
    }
    let language = info
        .split_whitespace()
        .next()
        .filter(|s| !s.is_empty())
        .map(str::to_lowercase);
    Some((ch, len, language))
}

fn closes(line: &str, ch: char, len: usize) -> bool {
    let Some(body) = strip_indent(line) else {
        return false;
    };
    let run = body.chars().take_while(|c| *c == ch).count();
    run >= len && body[run..].trim().is_empty()
}

/// The line without its trailing newline and up to three leading spaces, or
/// `None` when it is indented far enough to be code in its own right.
fn strip_indent(line: &str) -> Option<&str> {
    let line = line.trim_end_matches('\n').trim_end_matches('\r');
    let indent = line.len() - line.trim_start_matches(' ').len();
    (indent <= 3).then(|| &line[indent..])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prose_with_no_fence_is_one_chunk() {
        assert_eq!(
            split("just words"),
            vec![Chunk::Prose("just words".to_owned())]
        );
    }

    #[test]
    fn a_fence_splits_prose_into_three() {
        let chunks = split("before\n```rust\nfn main() {}\n```\nafter\n");
        assert_eq!(
            chunks,
            vec![
                Chunk::Prose("before".to_owned()),
                Chunk::Code {
                    language: Some("rust".to_owned()),
                    code: "fn main() {}\n".to_owned(),
                },
                Chunk::Prose("after".to_owned()),
            ]
        );
    }

    #[test]
    fn a_bare_fence_has_no_language() {
        let chunks = split("```\nx\n```");
        assert_eq!(
            chunks,
            vec![Chunk::Code {
                language: None,
                code: "x\n".to_owned(),
            }]
        );
    }

    /// The streaming case: a message that ended mid-fence is still code.
    #[test]
    fn an_unclosed_fence_runs_to_the_end() {
        let chunks = split("here:\n```sh\ncargo test\n");
        assert_eq!(
            chunks,
            vec![
                Chunk::Prose("here:".to_owned()),
                Chunk::Code {
                    language: Some("sh".to_owned()),
                    code: "cargo test\n".to_owned(),
                },
            ]
        );
    }

    /// An inner fence of three backticks does not close a four-backtick one,
    /// which is how an agent quotes a fenced example inside a fenced example.
    #[test]
    fn a_longer_fence_survives_a_shorter_one() {
        let chunks = split("````md\n```\nnested\n```\n````");
        assert_eq!(
            chunks,
            vec![Chunk::Code {
                language: Some("md".to_owned()),
                code: "```\nnested\n```\n".to_owned(),
            }]
        );
    }

    #[test]
    fn an_inline_span_does_not_open_a_block() {
        let chunks = split("call ```x``` here");
        assert_eq!(chunks, vec![Chunk::Prose("call ```x``` here".to_owned())]);
    }

    /// The caller addresses a block by the event it came from, so an event that
    /// produced no text must still produce a block.
    #[test]
    fn empty_text_still_yields_a_chunk() {
        assert_eq!(split(""), vec![Chunk::Prose(String::new())]);
    }
}
