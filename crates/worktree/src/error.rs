#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("could not run `git {args}`: {source}")]
    Spawn {
        args: String,
        #[source]
        source: std::io::Error,
    },

    #[error("`git {args}` failed ({status:?}): {stderr}")]
    Git {
        args: String,
        status: Option<i32>,
        stderr: String,
    },

    #[error("{path} is not inside a git repository")]
    NotARepo { path: String },

    #[error("{path} already exists")]
    WorktreeExists { path: String },

    #[error("git reported a shadow commit trailer that is not a number: {0:?}")]
    BadTrailer(String),

    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, Error>;
