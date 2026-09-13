//! The line change inside an `Edit`, as §7's `Diff` block wants it.
//!
//! §6 source 1 is the whole reason this is cheap: `Edit` carries
//! `{file_path, old_string, new_string}`, so the before and after are on the
//! wire and nothing has to be inferred from prose or read off disk. What is
//! left is turning two strings into lines a reader can scan.
//!
//! **This is not §13.1's attribution and must not be mistaken for it.** This is
//! what the agent *said it was doing*, taken from the call it made. §13.1 blames
//! a shadow history and therefore records what actually happened to the tree,
//! including the formatter that ran afterwards and the `Bash` that rewrote the
//! file. They disagree sometimes, and when they do, both are worth seeing.

use crate::block::{DiffLine, DiffLineKind, DiffStats};

/// How much context to keep around a change.
const CONTEXT: usize = 3;

/// One replacement, as context/removed/added lines.
///
/// Deliberately **not** an LCS diff. An `Edit`'s `old_string` and `new_string`
/// are a contiguous replacement by construction — that is what the tool does —
/// so the honest rendering is the shared head, the change, and the shared tail.
/// A real diff algorithm would find coincidental matching lines inside the
/// change and interleave them, which reads as a cleverer edit than the one that
/// happened.
pub fn replacement(before: &str, after: &str) -> Vec<DiffLine> {
    let old: Vec<&str> = lines(before);
    let new: Vec<&str> = lines(after);

    let head = old
        .iter()
        .zip(new.iter())
        .take_while(|(a, b)| a == b)
        .count();
    let tail = old
        .iter()
        .rev()
        .zip(new.iter().rev())
        .take_while(|(a, b)| a == b)
        .take(old.len().min(new.len()) - head)
        .count();

    let mut out = Vec::new();
    for line in old[head.saturating_sub(CONTEXT)..head].iter() {
        out.push(line_of(DiffLineKind::Context, line));
    }
    for line in &old[head..old.len() - tail] {
        out.push(line_of(DiffLineKind::Removed, line));
    }
    for line in &new[head..new.len() - tail] {
        out.push(line_of(DiffLineKind::Added, line));
    }
    let tail_start = old.len() - tail;
    for line in &old[tail_start..(tail_start + CONTEXT).min(old.len())] {
        out.push(line_of(DiffLineKind::Context, line));
    }
    out
}

/// A whole file arriving at once, which is what `Write` is.
pub fn whole_file(content: &str) -> Vec<DiffLine> {
    lines(content)
        .into_iter()
        .map(|l| line_of(DiffLineKind::Added, l))
        .collect()
}

/// `+n / -n` over any set of lines.
///
/// Takes an iterator rather than a slice so a whole `Diff`'s stats are one call
/// over its hunks — `stats(hunks.iter().flat_map(|h| &h.lines))` — instead of a
/// copy of every line into one vector to be counted and thrown away.
pub fn stats<'a>(lines: impl IntoIterator<Item = &'a DiffLine>) -> DiffStats {
    let mut stats = DiffStats::default();
    for line in lines {
        match line.kind {
            DiffLineKind::Added => stats.additions += 1,
            DiffLineKind::Removed => stats.deletions += 1,
            DiffLineKind::Context => {}
        }
    }
    stats
}

/// Lines without their terminators, and **without** the phantom empty line a
/// trailing newline would otherwise produce: a file ending in `\n` has not
/// gained a blank last line, and counting one would inflate every `+n`.
fn lines(text: &str) -> Vec<&str> {
    if text.is_empty() {
        return Vec::new();
    }
    let text = text.strip_suffix('\n').unwrap_or(text);
    text.split('\n').collect()
}

fn line_of(kind: DiffLineKind, text: &str) -> DiffLine {
    DiffLine {
        kind,
        text: text.trim_end_matches('\r').to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rendered(lines: &[DiffLine]) -> Vec<String> {
        lines
            .iter()
            .map(|l| {
                let sign = match l.kind {
                    DiffLineKind::Context => ' ',
                    DiffLineKind::Added => '+',
                    DiffLineKind::Removed => '-',
                };
                format!("{sign}{}", l.text)
            })
            .collect()
    }

    #[test]
    fn a_one_line_change_keeps_its_context() {
        let lines = replacement("a\nb\nc\n", "a\nB\nc\n");
        assert_eq!(rendered(&lines), [" a", "-b", "+B", " c"]);
        assert_eq!(
            stats(&lines),
            DiffStats {
                additions: 1,
                deletions: 1
            }
        );
    }

    #[test]
    fn an_insertion_removes_nothing() {
        let lines = replacement("a\nc\n", "a\nb\nc\n");
        assert_eq!(rendered(&lines), [" a", "+b", " c"]);
        assert_eq!(
            stats(&lines),
            DiffStats {
                additions: 1,
                deletions: 0
            }
        );
    }

    /// A trailing newline is a terminator, not a blank line. Counting it would
    /// make every `Write` report one addition more than the file has.
    #[test]
    fn a_trailing_newline_is_not_a_line() {
        assert_eq!(
            stats(&whole_file("one\ntwo\n")),
            DiffStats {
                additions: 2,
                deletions: 0
            }
        );
    }

    #[test]
    fn an_empty_write_is_an_empty_hunk() {
        assert!(whole_file("").is_empty());
    }

    /// The head and tail scans must not meet in the middle and double-count a
    /// line as both context and change.
    #[test]
    fn identical_strings_change_nothing() {
        let lines = replacement("a\nb\n", "a\nb\n");
        assert_eq!(
            stats(&lines),
            DiffStats {
                additions: 0,
                deletions: 0
            }
        );
    }
}
