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

    #[error(transparent)]
    Log(#[from] event_log::Error),

    #[error(transparent)]
    Worktree(#[from] worktree::Error),

    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, Error>;
