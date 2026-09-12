//! Provider event streams in, raw events out.
//!
//! The normalizer is a pure function over a stream: it spawns nothing, touches
//! no filesystem, and holds only the state needed to interpret the next line.
//! That is what makes it testable against recorded fixtures with no process,
//! no UI and no clock (design §16).
//!
//! Two rules it exists to enforce:
//!
//! - **Nothing is ever dropped.** Anything unclassified is preserved verbatim
//!   as `raw.unclassified` (design §7). The single exception is transient
//!   progress, which restates rather than adds — see
//!   [`claude_code::is_transient`]'s documentation and design §8.
//! - **Raw events only.** Anything we could recompute is a projection and does
//!   not belong here (design §8).

pub mod claude_code;
pub mod clock;
pub mod stats;

pub use claude_code::ClaudeCodeNormalizer;
pub use clock::{Clock, FixedClock, SystemClock};
pub use stats::Stats;
