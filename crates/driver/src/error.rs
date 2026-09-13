#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("could not spawn {binary}: {source}")]
    Spawn {
        binary: String,
        #[source]
        source: std::io::Error,
    },

    #[error("the post-create hook failed ({status:?}): {stderr}")]
    PostCreateHook { status: Option<i32>, stderr: String },

    #[error("the run is no longer accepting input")]
    Closed,

    /// A launch that would have failed in a way the provider reports as
    /// something else. Caught before spawning, because the alternative is an
    /// agent that denies every tool call for no visible reason.
    #[error("the launch spec is not coherent: {reason}")]
    LaunchSpec { reason: String },

    #[error(transparent)]
    WorkspaceMcp(#[from] workspace_mcp::Error),

    #[error(transparent)]
    Log(#[from] event_log::Error),

    #[error(transparent)]
    Worktree(#[from] worktree::Error),

    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, Error>;
