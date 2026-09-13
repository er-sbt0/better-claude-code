#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("could not bind the workspace MCP socket at {path}: {source}")]
    Bind {
        path: String,
        #[source]
        source: std::io::Error,
    },

    /// A live server is already listening there.
    ///
    /// Distinguished from a stale socket file, which [`crate::Listener::bind`]
    /// removes rather than reporting: after a crash the file always survives
    /// and refusing to start would make a crash permanent.
    #[error("another workspace MCP server is already listening at {path}")]
    SocketInUse { path: String },

    /// The socket path is longer than `sockaddr_un` allows.
    ///
    /// Raised as itself because the kernel's own error for it is
    /// `EINVAL`/"invalid argument", which sends the reader looking at the
    /// wrong thing entirely.
    #[error("socket path is {len} bytes; the kernel limit is {limit}: {path}")]
    SocketPathTooLong {
        path: String,
        len: usize,
        limit: usize,
    },

    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, Error>;
