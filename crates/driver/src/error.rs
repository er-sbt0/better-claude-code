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

    /// A steering command for a run this process is not holding open. The two
    /// causes are indistinguishable from the registry and both are the same
    /// answer to the caller: the run ended, or it was never started here.
    #[error("run {run} is not live in this process")]
    NotLive { run: String },

    /// A run whose event stream would never end, so nothing would ever reap it.
    /// See [`crate::Registry::start`].
    #[error("this run cannot be supervised: {reason}")]
    NotSupervisable { reason: String },

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
