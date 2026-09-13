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

    /// A launch that would have failed in a way the provider reports as
    /// something else. Caught before spawning, because the alternative is an
    /// agent that denies every tool call for no visible reason.
    #[error("the launch spec is not coherent: {reason}")]
    LaunchSpec { reason: String },

    /// A fork whose parent the log does not have.
    #[error("run {0} is not in the log")]
    NoSuchRun(agent_protocol::RunId),

    /// A fork anchored at a `seq` the parent's log does not contain. Holes are
    /// normal — a replayed event consumes a `seq` and then deduplicates away
    /// (§8) — so this is a lookup failure, not a corruption.
    #[error("run {run} has no event at seq {seq}")]
    NoSuchEvent { run: String, seq: u64 },

    /// A fork anchored at a block that has no provider address.
    ///
    /// Not every block has one, and there are two ways to lack one. Out-of-band
    /// events carry no key at all. And a block can be **keyed and still
    /// unaddressable**: the normalizer keys every uuid-bearing stdout line, but
    /// only the events off an `assistant` or `user` line are chain entries the
    /// provider will resolve (`normalizer::claude_code::is_addressable`,
    /// `docs/verification/claude-code-fork-anchor-types.md`). Either way this is
    /// refused rather than rounded to a neighbouring block that does have one —
    /// see [`crate::ForkPoint::resolve`].
    #[error(
        "the {event} at run {run} seq {seq} has no chain-entry uuid, so it cannot be \
         a branch point"
    )]
    NotAddressable {
        run: String,
        seq: u64,
        event: String,
    },

    /// A fork that cannot be placed at all.
    #[error("run {run} cannot be forked: {reason}")]
    CannotFork { run: String, reason: String },

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
