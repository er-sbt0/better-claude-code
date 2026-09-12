//! Worktree-per-run (§13) and its shadow history (§13.1).
//!
//! §13 is a V1 requirement, not a V2 feature: the moment a second run starts in
//! the same repository, two agents edit the same files and both runs are
//! corrupted. Parallel runs are the wedge, so isolation comes first.
//!
//! §13.1 rides on that isolation. Because every run has its own checkout, the
//! runtime can snapshot it after each mutating tool call and attribute diff
//! hunks by blaming the result — which is what lets Review show the reasoning
//! that produced a hunk rather than guessing at it.

mod error;
mod git;
mod shadow;
mod worktree;

pub use error::{Error, Result};
pub use shadow::{shadow_ref, Attribution, Cause, ShadowCommit, ShadowHistory};
pub use worktree::Worktree;
