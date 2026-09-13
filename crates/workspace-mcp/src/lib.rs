//! The workspace's own MCP server (design §6 source 3, §18).
//!
//! §6 names three sources of meaning and calls this one the spine. Sources 1
//! and 2 read the agent: provider metadata is thin but free, tool-call
//! introspection is a parser per framework per version and never ends. This
//! source inverts the direction — **the workspace hosts an MCP server and
//! injects it into the agent, and the agent reports structure to us directly.**
//! No scraping, no vendor event schemas, and it works identically for every
//! agent that speaks MCP, which is what makes it the only agent-neutral route
//! to the semantic block types §7 promises.
//!
//! Two tools, because the roadmap's smallest version that discharges all three
//! of its reasons needs exactly two: [`REPORT_FINDING`] is §6's reporting
//! channel, and [`REQUEST_PERMISSION`] is §12 option A — the driver's
//! `Permission::AskVia` arm has existed and been unreachable since the driver
//! was written, and this is the server it was waiting for.
//!
//! # Reporting, not control
//!
//! §15 requires that this server expose no tool that changes policy, approves
//! permissions, spawns runs, or writes outside the worktree: an agent that gets
//! prompt-injected must not be able to escalate through us. [`REQUEST_PERMISSION`]
//! looks like an exception and is not. It approves nothing — it *asks*, and
//! [`Workspace::request_permission`] decides. It is also not called by the
//! model at all: Claude Code's own permission layer invokes it, so the agent
//! calling it deliberately can achieve nothing but a decision the workspace was
//! going to make anyway. Everything an implementor of [`Workspace`] does with
//! these calls must keep that property.
//!
//! # Why it is hand-rolled
//!
//! The whole protocol surface here is a handshake, a tool list, a tool call and
//! a ping, over newline-delimited JSON-RPC. An MCP SDK would bring an async
//! runtime with it, and the workspace has deliberately chosen threads
//! (`driver::process`) and `git` as a subprocess (`worktree::shadow`) for the
//! same reason each time: one thing to verify instead of two. A dependency that
//! is larger than the thing it implements does not pay for itself.
//!
//! # Why there is a socket and a relay
//!
//! Claude Code spawns stdio MCP servers as **its own child processes**, so the
//! server cannot simply live in the driver's address space, and
//! `driver::process::spawn` gives the `claude` child no environment of its own,
//! so there is no way to hand a socket path down that way either. The
//! mcp-config's own `env` map is the only channel that reaches the server
//! process — see `docs/verification/claude-code-permission-prompt-tool.md` §1.
//!
//! So: the driver holds a [`Listener`] on a unix socket, and what Claude Code
//! actually spawns is `workspace-mcp-relay`, a dumb byte pipe that reads
//! [`SOCKET_ENV`], connects, and copies in both directions. The relay knows
//! nothing about MCP. Every line of protocol lives in [`Server`], where
//! [`Server::handle_line`] is reachable from a test without spawning anything —
//! the same property that makes `driver::Pipeline` testable against a recorded
//! fixture rather than against a process.
//!
//! # The contract this implements
//!
//! Verified live against Claude Code 2.1.270 and written down in
//! `docs/verification/claude-code-permission-prompt-tool.md`. §16's rule
//! applies: it changes without semver, so the tests here assert wire shapes
//! rather than behaviour, and a version bump means re-reading that document
//! before trusting this crate.
//!
//! Two things this crate found by running the real binary rather than by
//! reading it, both of which fail *silently* and are therefore written into
//! [`SERVER_NAME`] and [`REPORT_FINDING`] where someone will meet them:
//! `workspace` is a reserved server name and a config claiming it is dropped
//! with a warning, and the reporting tool is itself permission-gated, so a
//! deny-by-default workspace denies its own reporting channel.

mod error;
mod jsonrpc;
mod listener;
mod server;

pub use error::{Error, Result};
pub use listener::Listener;
pub use server::{Decision, PermissionRequest, Server, Workspace};

/// The MCP server name, as it appears in the injected config and in every
/// qualified tool name.
///
/// **Not `workspace`, however much §6 and the verification record spell it that
/// way.** Claude Code 2.1.270 reserves that name for its own hosted tools
/// (`mcp__workspace__bash`, `mcp__workspace__web_fetch`) and refuses a config
/// that claims it:
///
/// ```text
/// --mcp-config: 1 entry warning(s): mcpServers.workspace:
///   "workspace" is a reserved MCP server name and was not loaded
/// ```
///
/// That is a **warning on the debug channel, not an error**. The run starts
/// normally with no server, `--permission-prompt-tool` resolves to nothing, and
/// the only visible symptom is an agent that mysteriously has no workspace
/// tools — or, if the permission tool is being used, `MCP tool
/// mcp__workspace__request_permission ... not found. Available MCP tools: none`
/// at startup, which points at the tool rather than at the name. It was found
/// by running the binary, not by reading the flags, and nothing in the CLI's
/// help or its config errors mentions it. `stdio` is reserved too, along with
/// `claude_ai`, `computer-use`, `claude-in-chrome`, `hearthbot` and
/// `__proto__`.
///
/// The CLI also recovers this name out of `--permission-prompt-tool` and waits
/// for *that* server during startup, so a name here that disagrees with the one
/// in the injected config makes every permission request fail rather than hang.
pub const SERVER_NAME: &str = agent_protocol::WORKSPACE_MCP_SERVER;

/// The environment variable carrying the socket path to the relay.
///
/// Set through the mcp-config's `env` map, which is the only channel that
/// reaches a server process Claude Code spawned (see the module header).
pub const SOCKET_ENV: &str = "AGENT_WORKSPACE_MCP_SOCKET";

/// §12 option A: the tool `--permission-prompt-tool` names.
pub const REQUEST_PERMISSION: &str = "request_permission";

/// §6's reporting channel.
///
/// **This tool is itself permission-gated.** Verified live: an agent calling
/// `mcp__agent-workspace__report_finding` produces a [`REQUEST_PERMISSION`]
/// call for it first, and a workspace that denies by default therefore denies
/// its own reporting channel and silently gets no findings at all. The launch
/// has to pre-authorize it — `--allowed-tools mcp__agent-workspace__report_finding`,
/// which is [`qualified`]`(REPORT_FINDING)` and is confirmed to work — or the
/// decider has to allow this tool by name.
pub const REPORT_FINDING: &str = "report_finding";

/// `mcp__agent-workspace__<tool>` — the only spelling that works anywhere it
/// matters.
///
/// `--permission-prompt-tool` splits the value on `__` and requires the first
/// segment to be literally `mcp`; a bare `request_permission` parses to null
/// and cannot resolve. The same spelling is what `--allowed-tools` and
/// `--disallowed-tools` take, and there the parenthesis patterns that work for
/// built-in tools are rejected outright: the only valid MCP rules are
/// `mcp__<server>`, `mcp__<server>__*` and `mcp__<server>__<tool>`. So
/// `Bash(git*)` has an MCP analogue and `mcp__agent-workspace__report*` does not.
pub fn qualified(tool: &str) -> String {
    format!("mcp__{SERVER_NAME}__{tool}")
}

/// The §6 system-prompt append that tells the agent these tools exist.
///
/// §6's premise is that this converts the hardest problem in the product —
/// extracting meaning from prose — into prompt engineering with a typed schema
/// at the end of it. This string is that prompt engineering, and it is the half
/// of the mechanism that has no compiler checking it.
///
/// Two things in here are load-bearing and should survive editing:
///
/// - **Report as you go.** A finding the agent saves for its closing summary is
///   a finding the workspace never receives, because the run can end first.
/// - **Also say it in the reply.** §6's standing caveat is that the agent may
///   simply not call these tools, so nothing may depend on them; the inverse
///   failure — the agent reports a finding *instead* of mentioning it — would
///   make the transcript worse than if the server did not exist.
///
/// The driver must pair this with `--system-prompt-snapshot off`, or a resumed
/// session silently replays the system prompt from its first request and any
/// change here is ignored (`driver::LaunchSpec::argv`).
pub const SYSTEM_PROMPT: &str = "\
You are running inside Agent Workspace. It observes this run through an MCP \
server named `agent-workspace`, and one tool on it is yours to call:

- `mcp__agent-workspace__report_finding(severity, title, body, references)` — record \
something a reviewer of this run would want on the record: a bug you found \
(whether or not you fixed it), a risk you took, a broken assumption, a \
security or data-loss concern, a decision that could reasonably have gone the \
other way. `severity` is one of info, low, medium, high, critical. \
`references` are repository-relative paths, optionally `path:line`.

Call it as soon as you know the thing, not in a summary at the end — the run \
can finish before a summary is written. Report the same thing in your own \
reply as well; the tool is a record, not a substitute for telling the user. \
Do not report routine progress, and do not report the same finding twice.

`mcp__agent-workspace__request_permission` also lives on that server. It is called \
by the harness when a tool needs approval, never by you.";
