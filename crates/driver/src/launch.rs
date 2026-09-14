//! The launch shape for Claude Code (design §5).
//!
//! Every flag here is verified against 2.1.270 —
//! `docs/verification/claude-code-cli-2.1.270.md` — and the argv is built by a
//! pure function so a flag change is a unit-test diff rather than a broken run.
//! §16: these change without semver, so re-verify per version.

use std::path::PathBuf;

use agent_protocol::{RunId, SessionId};
use event_log::{LaunchFlags, Run};

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
    ///
    /// Reaches argv on a fresh start and on a fork, but **not** on a plain
    /// resume, where the provider refuses the flag outright — see
    /// [`argv`](LaunchSpec::argv). It is not dead there: a resume is still
    /// driven by an id, and [`validate`](LaunchSpec::validate) requires this to
    /// be the one being resumed.
    pub session_id: SessionId,
    pub permission: Permission,
    pub model: Option<String>,
    /// Pre-granted tools, or `None` when the caller named no allow-list at all.
    ///
    /// **`Some(vec![])` is not `None`, and that distinction is the reason this
    /// is an `Option`.** Under §12's `DenyPrompts` posture this field is the
    /// whole of what a run may do, so "pre-grant nothing" is a posture a caller
    /// is entitled to ask for — and on a fork or a resume it is the *opposite*
    /// of saying nothing, which inherits the parent's list
    /// ([`inherit_from`](LaunchSpec::inherit_from)). A bare `Vec` cannot tell
    /// those two apart, and the reading it collapsed them to hands the child
    /// *more* capability than the caller asked for — on the one field where
    /// "fails open" means what it sounds like.
    ///
    /// [`argv`](LaunchSpec::argv) emits the same argv for both spellings, and
    /// says why. The difference lives between building a spec and merging it
    /// with a row, which is the only place it is a question anyone can answer.
    pub allowed_tools: Option<Vec<String>>,
    /// Tools denied outright, or `None` when the caller named no deny-list.
    ///
    /// The same `Option` as [`allowed_tools`](LaunchSpec::allowed_tools) and for
    /// the mirrored reason: `Some(vec![])` is a fork that deliberately *drops*
    /// the parent's deny-list, which a caller may well want and which an empty
    /// `Vec` could only spell as "say nothing" — inheriting the very list it was
    /// trying to clear. The failure is quieter in this direction and it is the
    /// same failure.
    pub disallowed_tools: Option<Vec<String>>,
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
    /// Escape hatch for flags this struct has no opinion about, or `None` when
    /// the caller named none.
    ///
    /// `Option` for the same reason as the two tool lists, with one twist: we
    /// have no opinion about what is in here, so we equally have none about
    /// whether inheriting it is safe. `Some(vec![])` is how a caller says "start
    /// this branch without whatever argv the parent carried" — which, for a flag
    /// this struct cannot classify, is a thing only the caller can decide.
    pub extra: Option<Vec<String>>,
}

impl LaunchSpec {
    /// The half of this that is a fact about the *run*, for the run's row.
    ///
    /// Read at the launch seam, before [`crate::wire_workspace_mcp`] has
    /// rewritten anything: what the caller asked for, not what this process
    /// arranged. That matters for two of these. `append_system_prompt` gains
    /// the workspace MCP server's own paragraph when a server is hosted, and
    /// storing the appended value would append it a second time on every
    /// resume; `allowed_tools` gains `report_finding` the same way, and it is
    /// re-added by the same code on the next launch whether or not it is
    /// stored.
    ///
    /// What it leaves behind — the binary, the cwd, the session id, the resume
    /// and fork addresses, the MCP config and the permission posture — is
    /// argued field by field on [`LaunchFlags`]. The short version is that each
    /// of them is a property of this machine, this process or this moment
    /// rather than of the run, and several are already columns of their own.
    ///
    /// **The three list fields flatten here, and that is not the distinction
    /// [`allowed_tools`](LaunchSpec::allowed_tools) exists to keep.** `None` and
    /// `Some(vec![])` both store `[]`, because the row records a *launch* and
    /// the two spellings launch identically: [`argv`](LaunchSpec::argv) emits no
    /// `--allowed-tools` for either, so a parent that pre-granted nothing on
    /// purpose and one that never mentioned tools ran the same process under the
    /// same policy, and a child inheriting `[]` from either inherits that policy
    /// exactly. What the `Option` carries is a fact about a *caller* — did this
    /// call say anything — and it is answerable only while the spec is still the
    /// caller's, before [`inherit_from`](LaunchSpec::inherit_from) merges it with
    /// a row. Storing it would file a question about one call as a property of
    /// the run, and it would move a serialized shape that has rows on disk;
    /// [`LaunchFlags`] argues at length for not doing that, and nothing here
    /// needs it to.
    pub fn flags(&self) -> LaunchFlags {
        LaunchFlags {
            allowed_tools: self.allowed_tools.clone().unwrap_or_default(),
            disallowed_tools: self.disallowed_tools.clone().unwrap_or_default(),
            permission_mode: self.permission_mode.clone(),
            max_turns: self.max_turns,
            max_budget_usd: self.max_budget_usd,
            append_system_prompt: self.append_system_prompt.clone(),
            extra: self.extra.clone().unwrap_or_default(),
        }
    }

    /// Fill in what `run` was launched with, for everything the caller did not
    /// name.
    ///
    /// The inverse of [`flags`](LaunchSpec::flags), and the reason the `runs`
    /// row stores any of this: [`crate::fork`] and [`crate::resume`] both start
    /// from a conversation that already happened, and a continuation that
    /// quietly holds a different allow-list is not a continuation. Under §12's
    /// `DenyPrompts` posture the difference is not a prompt, it is a denial —
    /// so the child of a run that could `Read` could not, for no reason
    /// anywhere on the screen.
    ///
    /// **The caller wins wherever it said anything.** That rule is not new: it
    /// is what [`crate::fork`] already did for the model, on the argument that
    /// a caller naming a model is making exactly the change the branch exists
    /// to make. Applied to every field, it is also what keeps this honest about
    /// what it does not know — `LaunchSpec::new`'s default *is* "unset" for
    /// every field here, so "the caller said nothing" is decidable rather than
    /// guessed.
    ///
    /// **That sentence was false for three fields, and making it true is why
    /// they are `Option<Vec<String>>`.** `allowed_tools`, `disallowed_tools` and
    /// `extra` were bare `Vec`s tested with `is_empty()`, which spells "the
    /// caller said nothing" and "the caller asked for nothing" with the same
    /// value — so a fork or a resume that deliberately pre-granted *no* tools
    /// inherited its parent's entire allow-list instead, silently, and under
    /// §12's `DenyPrompts` posture that list is the whole of what a run may do.
    /// The same collapse on the other two lists errs the other way: a caller
    /// clearing the deny-list or the `extra` argv got the parent's back, and a
    /// child that can do less than its caller asked at least fails as a refusal
    /// someone can read. On the allow-list it erred toward a child that could do
    /// *more*, under a posture whose whole premise is that nothing may prompt —
    /// so the extra capability arrives unannounced and the record of the launch
    /// agrees with it.
    ///
    /// Two shapes were considered and rejected. A second field beside each list
    /// — a `bool` saying whether it was set — is two values that can disagree,
    /// and nothing stops a caller from setting the list and forgetting the flag.
    /// A named enum (`Unset` / `Set(Vec<String>)`) is `Option` with a vocabulary
    /// nobody else on this struct uses, next to eight fields that already spell
    /// "unset" as `None`. The cost of `Option` is one `unwrap_or_default` in
    /// [`flags`](LaunchSpec::flags) and one `is_none` here, in exchange for the
    /// two states being different values rather than the same one.
    ///
    /// A run whose row stores nothing — one recorded before the column existed
    /// — leaves the spec exactly as the caller built it, which is the behaviour
    /// every such run already had.
    pub fn inherit_from(&mut self, run: &Run) {
        if self.model.is_none() {
            self.model = run.model.clone();
        }
        let flags = &run.launch_flags;
        if self.allowed_tools.is_none() {
            self.allowed_tools = Some(flags.allowed_tools.clone());
        }
        if self.disallowed_tools.is_none() {
            self.disallowed_tools = Some(flags.disallowed_tools.clone());
        }
        if self.permission_mode.is_none() {
            self.permission_mode = flags.permission_mode.clone();
        }
        if self.max_turns.is_none() {
            self.max_turns = flags.max_turns;
        }
        if self.max_budget_usd.is_none() {
            self.max_budget_usd = flags.max_budget_usd;
        }
        if self.append_system_prompt.is_none() {
            self.append_system_prompt = flags.append_system_prompt.clone();
        }
        if self.extra.is_none() {
            self.extra = Some(flags.extra.clone());
        }
    }

    /// A spec with the defaults that make a run observable and bounded.
    pub fn new(cwd: impl Into<PathBuf>, session_id: SessionId, permission: Permission) -> Self {
        LaunchSpec {
            binary: PathBuf::from("claude"),
            cwd: cwd.into(),
            session_id,
            permission,
            model: None,
            // `None`, never an empty `Vec`: [`inherit_from`] reads these to
            // decide whether the caller said anything, and a spec fresh out of
            // here has said nothing about any of them.
            allowed_tools: None,
            disallowed_tools: None,
            permission_mode: None,
            max_turns: None,
            max_budget_usd: None,
            append_system_prompt: None,
            mcp_config: None,
            thinking: Thinking::Summarized,
            partial_messages: false,
            resume: None,
            fork_at: None,
            extra: None,
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
        // The provider's rule, quoted from its own refusal: "--session-id can
        // only be used with --continue or --resume if --fork-session is also
        // specified." [`argv`](LaunchSpec::argv) therefore drops the flag on a
        // plain resume, and that is lossless only while the id it drops is the
        // id the session already has — verified live: a resume keeps its id
        // (`docs/verification/claude-code-resume-session-id.md`). A caller
        // naming a *different* one is asking to rename a session on resume,
        // which the provider does not offer in any spelling. Refused here
        // rather than swallowed by the suppression, or the run would come back
        // under an id nobody chose and the row would point at the wrong
        // conversation for every resume after it.
        if let Some(r) = &self.resume {
            if self.fork_at.is_none() && self.session_id != *r {
                return Err(Error::LaunchSpec {
                    reason: format!(
                        "a resume that is not a fork cannot rename the session: \
                         --resume {r} with session_id {}; \
                         set session_id to the session being resumed, or fork",
                        self.session_id
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
    /// That third one is the only place this launch overrides something §5
    /// promises users they keep, so it is worth knowing exactly what it costs
    /// and what it does not. Measured both directions against 2.1.270
    /// (`docs/verification/claude-code-user-config-inheritance.md`): it drops
    /// MCP servers from *every* scope — user, project `.mcp.json`, local,
    /// plugin-contributed and the account's claude.ai connectors, with no
    /// spelling that keeps some — and it changes **nothing else**. Hooks at all
    /// three scopes still fire, both `CLAUDE.md`s still apply, and skills,
    /// subagents, commands, plugins, the output style and the user's own
    /// `permissions` block are byte-identical with the flag and without it.
    ///
    /// So a run is not hermetic and is not trying to be; the exception is
    /// argued per-reason in §5's "What users keep", and the short version is
    /// §12: under `DenyPrompts` a user MCP tool we cannot pre-grant is denied
    /// silently, so inheriting their servers would advertise capabilities the
    /// run would then refuse. 2.1.270's `--setting-sources`, `--restricted` and
    /// `--bare` are the knobs that *would* shut the rest out, deliberately not
    /// passed — `--bare` alone would cost the user's OAuth credentials.
    ///
    /// `--allowed-tools` and `--disallowed-tools` are emitted only for a
    /// **non-empty** list, so `None` and `Some(vec![])` produce the same argv —
    /// deliberately, and it is the right argv for both. The CLI has no spelling
    /// for an empty list — both flags take `<tools...>`, a required variadic
    /// value (`docs/verification/claude-code-cli-2.1.270.md`) — and it needs
    /// none, because a run launched with no `--allowed-tools` pre-grants
    /// exactly nothing, which is what a caller asking for an empty list is
    /// asking for. The two spellings
    /// differ in what the *caller said*, and the only reader of that is
    /// [`inherit_from`](LaunchSpec::inherit_from) — by the time argv is built,
    /// the question has already been answered.
    ///
    /// `-p` comes last and carries **no value**: with
    /// `--input-format stream-json` the prompt arrives over stdin as a `user`
    /// message, which is what turns a one-shot into a live session.
    ///
    /// `--session-id` is the one flag here that is not always emitted, and the
    /// rule is the provider's, quoted from its own refusal:
    ///
    /// > `--session-id can only be used with --continue or --resume if
    /// > --fork-session is also specified.`
    ///
    /// So it is emitted on a fresh start, and on a fork — where it is the whole
    /// point, naming the child before the process speaks (§14) — and suppressed
    /// on a plain resume, the one shape the provider will not take. Suppressing
    /// it costs nothing: a resumed session comes back under the id it already
    /// had, so there was never anything to assign
    /// (`docs/verification/claude-code-resume-session-id.md`).
    ///
    /// Worth knowing how that refusal arrives, because it is not the shape the
    /// fork records trained us to look for: exit 1 with **nothing on stdout**,
    /// no `system/init` and no `result` line at all. Reading acceptance off
    /// `subtype` — right for a refused fork address — would have read nothing
    /// here.
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
        // Every shape but a plain resume; the doc comment above quotes the
        // provider's own refusal.
        if self.resume.is_none() || self.fork_at.is_some() {
            push!("--session-id");
            push!(self.session_id.0);
        }

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
        // Empty and unset are one argv; the doc comment above says why.
        if let Some(tools) = self.allowed_tools.as_deref().filter(|t| !t.is_empty()) {
            push!("--allowed-tools");
            for t in tools {
                a.push(t.clone());
            }
        }
        if let Some(tools) = self.disallowed_tools.as_deref().filter(|t| !t.is_empty()) {
            push!("--disallowed-tools");
            for t in tools {
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
        // `--session-id` is already above, and a fork is the one resuming shape
        // that keeps it — which is the half of forking §14 had built before §19
        // asked for it: the child is named by us, so its run row is resumable
        // before the process has spoken. These two say which conversation it
        // starts from.
        if let Some(at) = &self.fork_at {
            push!("--fork-session");
            push!("--resume-session-at");
            push!(at);
        }
        a.extend(self.extra.iter().flatten().cloned());
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
