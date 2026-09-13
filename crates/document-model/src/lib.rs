//! §7's Structured Agent Document Model: raw events in, blocks out.
//!
//! The run feed is not a list of events. It is a *projection* over them — six
//! block types plus `Raw`, each carrying the `(runId, seq…)` provenance of the
//! events it was derived from — and this crate is where that projection lives.
//!
//! **Why a crate and not the webview.** Deriving is deciding. Several events
//! fan into one `ToolCall` block; one `assistant` line fans into a `Text` and a
//! `Code` block; a permission decision belongs to the call it refused and to no
//! block of its own. Every one of those is a decision, and the repository's
//! standing rule is that anything which starts deciding belongs where a test
//! can reach it without a window (roadmap §8). `apps/desktop` renders blocks.
//! It does not work out what one is.
//!
//! **The degradation invariant is the law here** (§7): *no event is ever
//! dropped*. Anything this projection has no type for — run and turn lifecycle,
//! PTY output, and anything the normalizer itself could not classify — becomes
//! a [`Raw`] block carrying the payload verbatim. `tests/fixtures.rs` replays
//! every recorded stream and asserts it: every event of every fixture is
//! accounted for by a block, and an unclassified line survives byte for byte.
//!
//! **Entities are not blocks** (§7). `Run` and its status live in `event-log`
//! and are referenced by id; nothing mutable is inside a block, because a block
//! is a projection of an immutable log and putting entity state in one puts a
//! changing thing inside an unchanging one.

mod block;
mod fences;
mod hunks;
mod project;
mod summary;

pub use block::{
    Block, Body, Code, Diff, DiffLine, DiffLineKind, DiffStats, Feed, Hunk, Raw, Speaker, Terminal,
    Text, ToolCall, ToolStatus, DOCUMENT_VERSION,
};
pub use project::project;
