//! The launch shape for Claude Code (design §5).
//!
//! Every flag here is verified against 2.1.270 —
//! `docs/verification/claude-code-cli-2.1.270.md` — and the argv is built by a
//! pure function so a flag change is a unit-test diff rather than a broken run.
//! §16: these change without semver, so re-verify per version.

use std::path::PathBuf;

use agent_protocol::{RunId, SessionId};

use crate::error::{Error, Result};

/// How permission prompts are answered.
///
/// Not defaulted, because there is no safe default: every value is a posture
/// the user is entitled to see named. §12's rule is to never claim enforcement
/// we do not have.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Permission {
    /// Nothing may prompt; anything that would is denied outright
    /// (`--permission-prompts none`).
    ///
    /// The only honest posture until the workspace MCP server exists, and the
    /// right one for derived runs (§9.1), which must not prompt at all.
    DenyPrompts,
    /// Route prompts to a tool on our own MCP server
    /// (`--permission-prompt-tool`), which is §12 option A.
    ///
    /// Requires [`LaunchSpec::mcp_config`]; the tool lives on the same server
    /// as §6's, and [`LaunchSpec::validate`] refuses the combination without
    /// it. The name must be fully qualified as `mcp__<server>__<tool>` — the
    /// CLI parses the server out of it and waits for that server to load, so a
    /// bare tool name does not resolve.
    AskVia { tool: String },
    /// Hand the decision to the agent's own mode, with no prompt reaching us.
    Mode(String),
}

/// How thinking is disclosed (`--thinking-display`).
///
/// Without `summarized`, thinking blocks still arrive — carrying a signature
/// and `thinking: ""`. An empty string means "not disclosed", never "did not
/// think" (§5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Thinking {
    Off,
    Summarized,
}

/// Everything needed to start one run of Claude Code.
#[derive(Debug, Clone)]
pub struct LaunchSpec {
    pub binary: PathBuf,
    /// A worktree, never the user's own checkout (§13).
    ///
    /// Usually the run's own. A fork that has not written has none yet, and
    /// works in the nearest ancestor's until it does — see [`crate::fork`],
    /// which is also where the argument for that lives. The half of this that
    /// never bends is the second half: no run of ours has the user's checkout
    /// as its working directory.
    pub cwd: PathBuf,
    /// We assign it rather than reading it back out of the stream, so the run
    /// row can be resumed before the process has said anything (§14).
    pub session_id: SessionId,
    pub permission: Permission,
    pub model: Option<String>,
    pub allowed_tools: Vec<String>,
    pub disallowed_tools: Vec<String>,
    pub permission_mode: Option<String>,
    pub max_turns: Option<u32>,
    pub max_budget_usd: Option<f64>,
    pub append_system_prompt: Option<String>,
    /// Our generated MCP config — a file path or inline JSON, both accepted.
    /// Never the user's own file (§14).
    pub mcp_config: Option<String>,
    pub thinking: Thinking,
    /// Token-level streaming. Off by default: it delivers the same text twice
    /// (§5), and nothing renders it yet.
    pub partial_messages: bool,
    /// Resume an earlier session by its id (§14).
    pub resume: Option<SessionId>,
    /// Branch [`resume`](LaunchSpec::resume) at a chain entry instead of
    /// continuing it: `--fork-session --resume-session-at <uuid>` (§19).
    ///
    /// The value is a **bare** chain-entry uuid — what
    /// [`normalizer::claude_code::fork_address`] returns, never a
    /// `raw_events.idempotency_key` straight out of the log, because the
    /// `{uuid}#{i}` fan-out suffix is ours and the CLI refuses it by name. Nor
    /// is every key a chain entry: the same function refuses to produce one for
    /// an event that did not come off an `assistant` or `user` line.
    /// [`validate`](LaunchSpec::validate) enforces that here rather than
    /// letting a live run discover it.
    ///
    /// There is no "fork at the tip" spelling on purpose. The branching surface
    /// anchors every fork to a message, and the tip is a message with an
    /// address like any other.
    pub fork_at: Option<String>,
    /// Escape hatch for flags this struct has no opinion about.
    pub extra: Vec<String>,
}

impl LaunchSpec {
    /// A spec with the defaults that make a run observable and bounded.
    pub fn new(cwd: impl Into<PathBuf>, session_id: SessionId, permission: Permission) -> Self {
        LaunchSpec {
            binary: PathBuf::from("claude"),
            cwd: cwd.into(),
            session_id,
            permission,
            model: None,
            allowed_tools: Vec::new(),
            disallowed_tools: Vec::new(),
            permission_mode: None,
            max_turns: None,
            max_budget_usd: None,
            append_system_prompt: None,
            mcp_config: None,
            thinking: Thinking::Summarized,
            partial_messages: false,
            resume: None,
            fork_at: None,
            extra: Vec::new(),
        }
    }

    /// Refuse a launch that would fail as something else.
    ///
    /// `--permission-prompt-tool` names a tool on an MCP server, and
    /// `--strict-mcp-config` — which [`LaunchSpec::argv`] always passes —
    /// guarantees the only servers loaded are the ones we injected. So an
    /// `AskVia` without an [`mcp_config`](LaunchSpec::mcp_config) names a tool
    /// that cannot exist, and the provider reports that as every permission
    /// request failing rather than as a bad flag. The symptom is an agent that
    /// mysteriously cannot do anything, which is expensive to diagnose and
    /// cheap to prevent here.
    pub fn validate(&self) -> Result<()> {
        if let Permission::AskVia { tool } = &self.permission {
            if self.mcp_config.is_none() {
                return Err(Error::LaunchSpec {
                    reason: format!(
                        "--permission-prompt-tool {tool} needs the MCP server that defines it, \
                         but mcp_config is unset and --strict-mcp-config is always passed"
                    ),
                });
            }
            // `mcp__<server>__<tool>`: the CLI splits on `__` and requires the
            // first segment to be exactly `mcp`, then waits for `<server>` to
            // load. Anything else parses to nothing and never resolves.
            if !tool.starts_with("mcp__") {
                return Err(Error::LaunchSpec {
                    reason: format!(
                        "--permission-prompt-tool needs a fully qualified \
                         mcp__<server>__<tool> name, got {tool}"
                    ),
                });
            }
        }
        if let Some(at) = &self.fork_at {
            // `--fork-session` is documented "use with --resume or --continue",
            // and the fork spike only ever ran it that way: without a session
            // to branch, there is nothing for the address to resolve against.
            if self.resume.is_none() {
                return Err(Error::LaunchSpec {
                    reason: format!(
                        "--resume-session-at {at} needs --resume: a fork branches a session, \
                         and there is none to branch"
                    ),
                });
            }
            // Our fan-out suffix, not the provider's. Verified refused by name:
            // `No message found with message.uuid of: <uuid>#0`. That failure
            // arrives as a *completed* process with an error subtype rather
            // than as a bad flag, so catching it here is the difference between
            // a refused launch and a live run spent discovering it
            // (`docs/verification/claude-code-fork-address.md`).
            if at.contains('#') {
                return Err(Error::LaunchSpec {
                    reason: format!(
                        "--resume-session-at takes a bare chain-entry uuid, got {at}: the \
                         `#i` fan-out suffix is ours and the CLI refuses it by name"
                    ),
                });
            }
        }
        Ok(())
    }

    /// Build the argv.
    ///
    /// Three of these exist only to defuse a silent failure, each found by
    /// recording rather than by reading the help text:
    ///
    /// - `--verbose` is hard-required with `--output-format stream-json`; the
    ///   CLI refuses to start without it.
    /// - `--system-prompt-snapshot off`, or the first request's system prompt
    ///   is replayed verbatim forever and a changed `--append-system-prompt` is
    ///   silently ignored on resume. §6's whole premise is that appended text.
    /// - `--strict-mcp-config`, so the user's own MCP servers do not leak into
    ///   a run we are claiming to describe.
    ///
    /// `-p` comes last and carries **no value**: with
    /// `--input-format stream-json` the prompt arrives over stdin as a `user`
    /// message, which is what turns a one-shot into a live session.
    pub fn argv(&self) -> Vec<String> {
        let mut a: Vec<String> = Vec::new();
        macro_rules! push {
            ($($x:expr),+) => { $( a.push(($x).to_string()); )+ };
        }

        push!("--output-format");
        push!("stream-json");
        push!("--verbose");
        push!("--input-format");
        push!("stream-json");
        // stdout acks our stdin writes, which is how the driver knows a turn
        // was ingested rather than guessing.
        push!("--replay-user-messages");
        push!("--strict-mcp-config");
        push!("--session-id");
        push!(self.session_id.0);

        if self.partial_messages {
            push!("--include-partial-messages");
        }
        if let Thinking::Summarized = self.thinking {
            push!("--thinking-display");
            push!("summarized");
        }
        if let Some(m) = &self.model {
            push!("--model");
            push!(m);
        }
        if let Some(cfg) = &self.mcp_config {
            push!("--mcp-config");
            push!(cfg);
        }
        match &self.permission {
            Permission::DenyPrompts => {
                push!("--permission-prompts");
                push!("none");
            }
            Permission::AskVia { tool } => {
                push!("--permission-prompt-tool");
                push!(tool);
            }
            Permission::Mode(_) => {}
        }
        let mode = match &self.permission {
            Permission::Mode(m) => Some(m.clone()),
            _ => self.permission_mode.clone(),
        };
        if let Some(m) = mode {
            push!("--permission-mode");
            push!(m);
        }
        if !self.allowed_tools.is_empty() {
            push!("--allowed-tools");
            for t in &self.allowed_tools {
                a.push(t.clone());
            }
        }
        if !self.disallowed_tools.is_empty() {
            push!("--disallowed-tools");
            for t in &self.disallowed_tools {
                a.push(t.clone());
            }
        }
        if let Some(p) = &self.append_system_prompt {
            push!("--append-system-prompt");
            push!(p);
            push!("--system-prompt-snapshot");
            push!("off");
        }
        if let Some(n) = self.max_turns {
            push!("--max-turns");
            push!(n.to_string());
        }
        if let Some(b) = self.max_budget_usd {
            push!("--max-budget-usd");
            push!(format!("{b}"));
        }
        if let Some(r) = &self.resume {
            push!("--resume");
            push!(r.0);
        }
        // `--session-id` is already above and unconditional, which is the half
        // of forking §14 had built before §19 asked for it: the child is named
        // by us, so its run row is resumable before the process has spoken.
        // These two say which conversation it starts from.
        if let Some(at) = &self.fork_at {
            push!("--fork-session");
            push!("--resume-session-at");
            push!(at);
        }
        a.extend(self.extra.iter().cloned());
        push!("-p");
        a
    }
}

/// A `user` message as Claude Code's `--input-format stream-json` expects it.
///
/// This exact shape is what the `multi-turn` fixture was recorded with; two
/// user turns through one process (§5).
pub fn user_message(text: &str) -> String {
    serde_json::json!({
        "type": "user",
        "message": { "role": "user", "content": [{ "type": "text", "text": text }] },
        "parent_tool_use_id": null,
    })
    .to_string()
}

/// A fresh session id for a run. We assign it; §14 explains why.
pub fn new_session_id() -> SessionId {
    SessionId(uuid::Uuid::new_v4().to_string())
}

/// A fresh run id.
///
/// Ours rather than the provider's (§14), and it names things a user reads: the
/// run's branch is `agent/<run_id>` and its shadow ref ends in it, so it is
/// prefixed and shortened rather than left as a bare UUID. Every caller that
/// starts a run needs one, and a caller inventing its own is a caller deciding
/// what our identifiers look like.
pub fn new_run_id() -> RunId {
    let uuid = uuid::Uuid::new_v4().simple().to_string();
    RunId(format!("run_{}", &uuid[..12]))
}
