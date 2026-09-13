<!--
Verification record. Discharges part of the "every CLI flag in §5" debt in
docs/roadmap.md. Re-run against each new Claude Code version; these flags change
without semver (agent-workspace-design.md §16).

Verified: 2026-09-12 against 2.1.269 · re-verified 2026-09-13 against 2.1.270
          · Ubuntu, x86-64
Method: `claude --help`, subcommand help, commander option definitions
        extracted from the binary, cross-checked against live validation errors.
        The re-verification additionally checked that all 105 flags named below
        still resolve, and re-ran every hard-constraint probe.
-->

# CLI flag verification — Claude Code 2.1.270

Binary: `/home/eransa/.local/bin/claude` -> `/home/eransa/.local/share/claude/versions/2.1.270`
`claude --version` => `2.1.270 (Claude Code)`

**Everything below was written against 2.1.269 and re-checked against 2.1.270.**
All 105 flags it names still resolve — visible in `--help` or still registered
with `.hideHelp()` — and every hard constraint it records still fires with the
same message, with **one exception**, in §0 immediately below. The 2.1.269
binary was ELF x86-64, not stripped, BUILD_TIME 2026-09-11T17:33:46Z, GIT_SHA
d0733697ad641a564c7cfcb19a6eb1eb9d61357e.

---

## 0. What changed in 2.1.270

One thing, and it loosens a constraint rather than tightening one.

### `--input-format=stream-json` no longer requires `--print`

2.1.269 refused the combination:

```
Error: --input-format=stream-json requires --print.
```

2.1.270 accepts it. Verified by running a complete turn — `system/init`,
`assistant`, `result/success` — with `--input-format stream-json
--output-format stream-json --verbose` and no `-p` anywhere on the command
line. The error string is still present in the binary and still fires for
`--include-partial-messages` and `--prompt-suggestions`, so this is that one
check being relaxed rather than the whole family going away.

**No consequence for this repository.** `LaunchSpec::argv` passes `-p` last and
unconditionally, and should keep doing so: `--permission-prompt-tool` is
documented "only works with --print", and nothing has been verified about
whether *that* constraint moved too. This is recorded because §16 asks for the
diff between versions, not because anything needs changing.

The companion constraint is unchanged and still fires:

```
Error: --input-format=stream-json requires output-format=stream-json.
Error: When using --print, --output-format=stream-json requires --verbose
```

as do the choice lists for `--permission-mode` (`acceptEdits, auto,
bypassPermissions, manual, dontAsk, plan`) and `--permission-prompts`
(`host, none`).

**Not re-checked at the same depth:** the exact help *text* of every flag in the
tables below. Flag existence and the hard constraints were re-run; the prose
was spot-checked on the flags the driver actually passes. A wording change in a
flag nobody uses would not have been caught.

See also `claude-code-permission-prompt-tool.md`, which verifies MCP injection
and the permission-prompt response contract against 2.1.270 — both of which this
record predates and does not cover.

Help surfaces checked: `claude --help` (full, captured), `claude -p --help` (identical option list —
no per-flag subhelp), `claude mcp --help`, `claude mcp serve --help`, `claude agents --help`.
Hidden options (registered with `.hideHelp()`) were recovered by extracting the commander option
definitions out of the binary and cross-checked with live "argument missing" / validation errors.

---

## Verified as documented

### `-p` / `--print`  — EXISTS, unchanged
```
  -p, --print                           Print response and exit (useful for
                                        pipes). Note: The workspace trust dialog
                                        is skipped when Claude is run in
                                        non-interactive mode (via -p, or when
                                        stdout is not a TTY, e.g. piped or
                                        redirected output). Only use this in
                                        directories you trust. Settings files
                                        that fail validation are silently
                                        ignored in this mode (no error dialog is
                                        shown).
```
Note: print mode is also entered implicitly when stdout is not a TTY, even without `-p`.

### `--output-format` — EXISTS, `stream-json` still a valid value
```
  --output-format <format>              Output format (only works with --print):
                                        "text" (default), "json" (single
                                        result), or "stream-json" (realtime
                                        streaming) (choices: "text", "json",
                                        "stream-json")
```

### `--verbose` — EXISTS and IS STILL REQUIRED alongside `--print --output-format stream-json`
Help text is unhelpfully generic:
```
  --verbose                             Override verbose mode setting from
                                        config
```
But the requirement is still hard-enforced. Live check:
```
$ echo "" | claude -p "hi" --output-format stream-json
Error: When using --print, --output-format=stream-json requires --verbose
```
Matching code in the binary:
```js
if(w.outputFormat==="stream-json"&&!w.verbose){
  process.stderr.write(`Error: When using --print, --output-format=stream-json requires --verbose\n`),Ur(1);return}
```
=> The doc's `--output-format stream-json --verbose` pairing is correct and still mandatory.

### `--input-format` — EXISTS, `stream-json` is a valid value
```
  --input-format <format>               Input format (only works with --print):
                                        "text" (default), or "stream-json"
                                        (realtime streaming input) (choices:
                                        "text", "stream-json")
```
Two additional constraints, enforced at startup (exact strings from the binary, both reproduced live):
```
Error: --input-format=stream-json requires output-format=stream-json.
Error: --input-format=stream-json requires --print.    # 2.1.269 only — see §0
```
=> The doc's combination (`-p` + `--output-format stream-json` + `--input-format stream-json`) satisfies
all three constraints. Correct as written.

**In 2.1.270 the second of those no longer fires** and the combination runs
without `-p`. §0 has the evidence. Passing `-p` anyway remains correct and is
what the driver does.

### `--mcp-config` — EXISTS, takes BOTH file paths and inline JSON strings, and is variadic
```
  --mcp-config <configs...>             Load MCP servers from JSON files or
                                        strings (space-separated)
```
`<configs...>` is variadic: multiple values after one flag, and the flag may also be repeated.
So `--mcp-config <generated.json>` in the doc is valid, but the doc understates it — inline JSON
(`--mcp-config '{"mcpServers":{...}}'`) works too, so the generated temp file is optional.
Companion flag (already existed, still there):
```
  --strict-mcp-config                   Only use MCP servers from --mcp-config,
                                        ignoring all other MCP configurations
```
(`claude agents` has its own narrower `--mcp-config <config>` — singular, "(repeatable)".)

### `--permission-prompt-tool` — EXISTS, exact spelling unchanged, but is now HIDDEN from `--help`
It does **not** appear in `claude --help` output. It is registered with `.hideHelp()`. Its real
registered spec and description, extracted from the binary:
```
--permission-prompt-tool <tool>
MCP tool to use for permission prompts (only works with --print)
```
Live confirmation that commander knows it:
```
$ claude --permission-prompt-tool
error: option '--permission-prompt-tool <tool>' argument missing
```
It is still referenced in visible help, inside `--permission-prompts`:
```
  --permission-prompts <target>         Who answers permission prompts with
                                        --print: "host" (the SDK host or
                                        --permission-prompt-tool) or "none" ...
```
Stated constraint: "only works with --print". Also, from the binary, it is rejected for cloud sessions:
```
--permission-prompt-tool (permission prompts reach the host over stdio; an MCP tool cannot answer them here)
```
=> The doc's `--permission-prompt-tool mcp__workspace__request_permission` is still valid. Caveat: the
flag is undocumented in help now, so it should be treated as semi-public API.

### `--append-system-prompt` — EXISTS, unchanged, still visible in help
```
  --append-system-prompt <prompt>       Append a system prompt to the default
                                        system prompt
```

### `--resume` — EXISTS, takes an OPTIONAL value; `--continue` also exists
```
  -r, --resume [value]                  Resume a conversation by session ID, or
                                        open interactive picker with optional
                                        search term
  -c, --continue                        Continue the most recent conversation in
                                        the current directory
```
Note `[value]` (optional), not `<value>`. With no value in a non-interactive context there is no picker,
so a driver must always pass the id explicitly. The doc's `--resume <providerSessionId>` is correct.

---

## Changed / wrong / missing

### 1. **"Claude Code ships no sandbox of its own" is NOW FALSE.** This is the biggest correction.
Claude Code 2.1.269 ships a full, first-class Bash sandbox (macOS Seatbelt, Linux, and Windows WFP/ACL
via an `srt-win` helper), with network egress control and filesystem read/write confinement.

Evidence — settings keys present in the binary's settings schema:
```
sandbox.enabled
sandbox.enabledPlatforms
sandbox.failIfUnavailable
sandbox.autoAllowBashIfSandboxed
sandbox.allowUnsandboxedCommands
sandbox.excludedCommands
sandbox.enableWeakerNestedSandbox
sandbox.enableWeakerNetworkIsolation
sandbox.allowAppleEvents
sandbox.network.allowManagedDomainsOnly
sandbox.network.tlsTerminate
sandbox.filesystem.allowManagedReadPathsOnly
sandbox.filesystem.denyRead / denyWrite / allowWrite
```
Description strings from the binary:
```
"All bash commands invoked by the model must run in the sandbox unless they are explicitly listed in excludedCommands."
"Allow commands to run outside the sandbox via the dangerouslyDisableSandbox parameter. When false, the dangerouslyDisableSandbox parameter is completely ignored and all commands must run sandboxed. Default: true."
"Additional paths to allow writing within the sandbox. Merged with paths from Edit(...) allow permission rules."
"Additional paths to deny reading within the sandbox. Merged with paths from Read(...) deny permission rules."
"Access mode for this path. `deny` blocks reads inside the sandbox; `mask` shows sandboxed commands a sentinel-substituted copy ..."
"Access mode for this environment variable. `deny` unsets the variable for sandboxed commands; `mask` shows sandboxed commands a sentinel value ..."
```
UI mode labels present: `Sandbox BashTool, with auto-allow` / `Sandbox BashTool, with regular permissions` /
`Sandbox BashTool` / `No Sandbox`, plus `Strict sandbox mode:` and a docs link
`https://code.claude.com/docs/en/sandboxing#configure-sandboxing`.
The Bash tool itself exposes a `dangerouslyDisableSandbox` parameter.

**However, there is NO top-level `--sandbox` CLI flag.** Confirmed live:
```
$ claude --sandbox -p
error: unknown option '--sandbox'
$ claude --no-sandbox -p
error: unknown option '--no-sandbox'    (same)
```
`--sandbox` / `--no-sandbox` strings do exist in a separate hand-rolled arg parser inside the binary
(alongside `--debug-file`, `--permission-mode`, `--name`, `--remote-control-session-name-prefix`), i.e.
a non-main entry point, not the primary `claude` command.

**Practical upshot for a subprocess driver:** the sandbox is configured through settings, not argv — so you
reach it with `--settings '<json>'` (or `--settings <file>`), e.g.
`--settings '{"sandbox":{"enabled":true,"autoAllowBashIfSandboxed":true}}'`.
Any design that assumes it must supply its own sandbox layer should be revisited.

### 2. `--permission-prompt-tool` is hidden from `--help`
Correct spelling, correct behavior, but no longer discoverable via help output (see above). The doc is
factually right; it just can't be verified by a reader running `claude --help`.

### 3. `--mcp-config` is understated
Doc implies a generated file is required. It accepts inline JSON strings and is space-separated variadic.

### 4. `--verbose` is present in the doc, which is right — but its help text no longer says why
The doc's justification ("still required alongside stream-json") holds; the help text does not state it.
Only the runtime error reveals the coupling. Worth keeping the note in the doc.

### 5. Nothing in the documented shape is *wrong* or *removed*
Every flag in the asserted launch shape exists with the exact spelling given. The launch shape as written
still runs. The problems are omissions (below), not errors.

---

## Newly relevant flags not in the doc

These exist in 2.1.269 and materially change the design space for driving Claude Code as a subprocess.

### Streaming / wire protocol
| Flag | Help text | Why it matters |
|---|---|---|
| `--include-partial-messages` | "Include partial message chunks as they arrive (only works with --print and --output-format=stream-json)" | Token-level streaming. Without it, stream-json emits only complete blocks. Essential for live UI. |
| `--include-hook-events` | "Include all hook lifecycle events in the output stream (only works with --output-format=stream-json)" | Hook lifecycle observability on the wire — a supervisor can watch PreToolUse/PostToolUse etc. without writing hook scripts. |
| `--replay-user-messages` | "Re-emit user messages from stdin back on stdout for acknowledgment (only works with --input-format=stream-json and --output-format=stream-json)" | Ack/flow-control for a stream-json writer: lets the driver confirm the child ingested each message. Removes a whole class of race conditions. |
| `--forward-subagent-text` | "Forward subagent text and thinking blocks as assistant/user messages with parent_tool_use_id set (only works with --print and --output-format=stream-json)" | Makes subagent (Task tool) activity visible to the parent process, correlated by `parent_tool_use_id`. Previously subagent internals were opaque. |
| `--await-initialize` *(hidden)* | "Read the initialize control request from stdin during startup so its launch-scoped fields (plugins) apply exactly like their command-line flags. Pass it only from the process that writes that request as the first stdin line at spawn (only works with --input-format=stream-json)" | A real stdin control-request handshake exists. Launch-scoped config can be negotiated over the wire instead of argv. |
| `--session-mirror` *(hidden)* | "Emit transcript_mirror frames on stdout (SDK-internal; set by ProcessTransport when sessionStore is configured)" | The SDK's own mechanism for mirroring transcript state to the host. |
| `--sdk-url <url>` *(hidden)* | "Use remote WebSocket endpoint for SDK I/O streaming (only with -p and stream-json format)" | Reserved: "This flag is reserved for Remote Control worker processes connecting to Anthropic's backend." Rejected for general hosts. |

### Session identity / resume control
| Flag | Help text | Why it matters |
|---|---|---|
| `--session-id <uuid>` | "Use a specific session ID for the conversation (must be a valid UUID)" | **Big one.** The driver can *assign* the session id at launch rather than scraping it out of the init event. Removes the "capture the providerSessionId then resume it" dance entirely. |
| `--fork-session` | "When resuming, create a new session ID instead of reusing the original (use with --resume or --continue)" | Branch a conversation. Enables parallel/speculative runs from a shared prefix — directly relevant to a parallel-run review design. |
| `--no-session-persistence` | "Disable session persistence - sessions will not be saved to disk and cannot be resumed (only works with --print)" | Ephemeral runs with no disk footprint. |
| `--resume-session-at <message id>` *(hidden)* | "When resuming, only messages up to and including the chain entry with <message.id> — any chain-entry UUID, typically the kept turn's last entry (use with --resume in print mode)" | Truncating resume / rewind to an arbitrary point. Enables retry-from-a-point without replaying the whole conversation. |
| `--resume-drops-turn <message id>` *(hidden)* | "With --resume-session-at in print mode: declare the prompt uuid of the turn the truncating resume intends to discard; the resume is refused if the discarded range contains anything not attributable to that turn ..." | Safety interlock for the above — makes truncating resume verifiable rather than blind. |
| `--rewind-files <user-message-id>` *(hidden)* | "Restore files to state at the specified user message and exit (requires --resume)" | Filesystem rollback to a conversation point. A supervisor can undo an agent's edits without git. |
| `--from-pr [value]` | "Resume a session linked to a PR by PR number/URL, or open interactive picker with optional search term" | Session<->PR linkage. |
| `--reply-on-resume` *(hidden)* | "When resuming, immediately query if the loaded transcript ends in a user-role message (set by /background mid-turn so the fork continues the in-flight turn)." | Continue an interrupted in-flight turn. |

### Permissions
| Flag | Help text | Why it matters |
|---|---|---|
| `--permission-prompts <target>` | "Who answers permission prompts with --print: \"host\" (the SDK host or --permission-prompt-tool) or \"none\" (nobody: anything that would prompt is denied automatically; the permission mode still decides everything else) (choices: \"host\", \"none\", default: \"host\")" | **New alternative to `--permission-prompt-tool`.** `none` gives deterministic non-interactive behavior with zero MCP plumbing — no permission MCP server needed at all for a deny-by-default run. |
| `--permission-mode <mode>` | "Permission mode to use for the session (choices: \"acceptEdits\", \"auto\", \"bypassPermissions\", \"manual\", \"dontAsk\", \"plan\")" | Six modes now, incl. `auto`, `manual`, `dontAsk`. Far more granular than the old accept/bypass split. |
| `--allowedTools, --allowed-tools <tools...>` | "Comma or space-separated list of tool names to allow (e.g. \"Bash(git *) Edit\")" | Scoped tool rules with argument patterns. |
| `--disallowedTools, --disallowed-tools <tools...>` | "Comma or space-separated list of tool names to deny (e.g. \"Bash(git *) Edit\")" | |
| `--tools <tools...>` | "Specify the list of available tools from the built-in set. Use \"\" to disable all tools, \"default\" to use all tools, or specify tool names (e.g. \"Bash,Edit,Read\")." | Distinct from allow/deny: this sets which built-ins *exist*. `--tools ""` yields a pure-LLM run. |
| `--restricted` | "Restricted mode: removes the built-in tools that run commands or code (Bash, PowerShell, REPL and the other code-running tools) and WebFetch unless --tools names them, and ignores user, project and local settings files (managed settings and --settings still apply; add --strict-mcp-config to skip MCP servers too). Also confines the file tools to the working directories (--add-dir included), refuses bypassPermissions, and lets only a person or the configured permission handler approve writes to settings, git and tool-configuration files." | A ready-made hardened profile — relevant to the same threat model the doc's sandbox discussion addresses. |
| `--allow-dangerously-skip-permissions` | "Enable bypassing all permission checks as an option, without it being enabled by default. Recommended only for sandboxes with no internet access." | Makes bypass *available* without defaulting to it. |
| `--inherit-permission-mode <mode>` *(hidden)* | "Permission mode carried from a parent session, used only when nothing else configures one" | Parent->child permission inheritance for nested harnesses. |

### Configuration injection (how a driver controls the child without touching the user's disk)
| Flag | Help text | Why it matters |
|---|---|---|
| `--settings <file-or-json>` | "Path to a settings JSON file or a JSON string to load additional settings from" | Inline JSON accepted. **This is the channel for sandbox config** (see the sandbox finding above), plus hooks, permissions, env. |
| `--setting-sources <sources>` | "Comma-separated list of setting sources to load (user, project, local)." | Suppress ambient user/project settings for reproducible runs. |
| `--agents <json>` | "JSON object defining custom agents (e.g. '{\"reviewer\": {\"description\": \"Reviews code\", \"prompt\": \"You are a code reviewer\"}}')" | Define subagents inline at launch — no `.claude/agents/*.md` files needed. Directly relevant to "concrete agent harnesses". |
| `--agent <agent>` | "Agent for the current session. Overrides the 'agent' setting." | Select the top-level agent persona. |
| `--managed-settings <json>` *(hidden)* | "Policy-tier settings JSON from a spawning parent process (SDK use only)" | Parent can inject *policy-tier* (highest precedence) settings the child cannot override. Strong containment primitive. |
| `--bare` | "Minimal mode: skip hooks, LSP, plugin sync, attribution, auto-memory, background prefetches, keychain reads, and CLAUDE.md auto-discovery. Sets CLAUDE_CODE_SIMPLE=1. Anthropic auth is strictly ANTHROPIC_API_KEY or apiKeyHelper via --settings (OAuth and keychain are never read). ... Explicitly provide context via: --system-prompt[-file], --append-system-prompt[-file], --add-dir (CLAUDE.md dirs), --mcp-config, --settings, --agents, --plugin-dir." | **Purpose-built for programmatic embedding.** Deterministic, no ambient state, no keychain. This is the flag a subprocess harness probably wants as its base. |
| `--safe-mode` | "Start with all customizations (CLAUDE.md, skills, plugins, hooks, MCP servers, custom commands and agents, output styles, workflows, custom themes, keybindings, and more) disabled — useful for troubleshooting a broken configuration. Admin-managed (policy) settings still apply. ... Sets CLAUDE_CODE_SAFE_MODE=1." | |
| `--add-dir <directories...>` | "Additional directories to allow tool access to" | |
| `--plugin-dir <path>` / `--plugin-url <url>` | "Load a plugin from a directory or .zip for this session only ... (repeatable)" / "Fetch a plugin .zip from a URL for this session only (repeatable)" | Session-scoped plugin injection. |
| `--plugin-dir-no-mcp <path>` | "Like --plugin-dir but the engine will not read this plugin's .mcp.json (caller owns its MCP connections)" | Caller keeps ownership of MCP wiring. |
| `--disable-slash-commands` | "Disable all skills" | |

### System prompt (beyond `--append-system-prompt`)
| Flag | Help text | Why it matters |
|---|---|---|
| `--system-prompt <prompt>` | "System prompt to use for the session" | Full *replacement* of the default system prompt, not just append. |
| `--system-prompt-file <file>` *(hidden)* | "Read system prompt from a file" | Avoids argv length limits. |
| `--append-system-prompt-file <file>` *(hidden)* | "Read system prompt from a file and append to the default system prompt" | Same, for the append case — relevant if the doc's `--append-system-prompt "<text>"` carries a large generated block. |
| `--append-subagent-system-prompt <prompt>` *(hidden)* | "Append a system prompt to every Task-tool subagent's system prompt, propagated to nested subagents (only works with --print). Implies CLAUDE_CODE_ENABLE_APPEND_SUBAGENT_PROMPT=1." | Reach *into* subagents — previously impossible. |
| `--append-subagent-system-prompt-file <file>` *(hidden)* | "Read a system prompt from a file and append it to every Task-tool subagent's system prompt (only works with --print)" | |
| `--system-prompt-snapshot <on\|off>` | "Record the system prompt once per conversation and reuse it verbatim on every request and resume. on (the default): ... every later request and resume sends the record as-is, even when a later launch passes different text, until the conversation is compacted. off: never record; the prompt is rendered fresh every request (for iterating on prompt text)." | **Sharp edge for the doc's design.** With the default `on`, a `--resume` that passes a *different* `--append-system-prompt` will be silently ignored — the recorded prompt wins. Any design that varies the appended system prompt across resumes of one session must pass `--system-prompt-snapshot off`. |
| `--exclude-dynamic-system-prompt-sections` | "Move per-machine sections (cwd, env info, memory paths, git status) from the system prompt into the first user message. Improves cross-user prompt-cache reuse. Only applies with the default system prompt (ignored with --system-prompt). (default: false)" | Prompt-cache economics for many parallel runs. |
| `--plan-mode-instructions <instructions>` *(hidden)* | "Custom workflow body for plan mode. Replaces the default code-implementation phases in the plan-mode system reminder; the read-only enforcement preamble and ExitPlanMode protocol footer are always kept." | Retarget plan mode to a non-coding workflow. |

### Budgets, limits, structured output
| Flag | Help text | Why it matters |
|---|---|---|
| `--max-turns <turns>` *(hidden)* | "Maximum number of agentic turns in non-interactive mode. This will early exit the conversation after the specified number of turns. (only works with --print)" | Runaway protection. Hidden but live (`error: option '--max-turns <turns>' argument '--version' is invalid. must be a number`). |
| `--max-budget-usd <amount>` | "Maximum dollar amount to spend on API calls (only works with --print)" | Hard cost ceiling per run. New and very relevant to parallel fan-out. |
| `--task-budget <tokens>` | "API-side task budget in tokens (output_config.task_budget)" | Server-side token budget. |
| `--json-schema <schema>` | "JSON Schema for structured output validation. Example: {\"type\":\"object\",\"properties\":{\"name\":{\"type\":\"string\"}},\"required\":[\"name\"]}" | **Structured output.** A driver can get schema-validated JSON back instead of parsing prose — directly relevant to a "structured integration" design. |
| `--autocompact <auto\|tokens>` | "Auto-compact window size (auto, or 100k–1M tokens)" | Context-window policy control. |
| `--max-thinking-tokens <tokens>` *(hidden)* | "[DEPRECATED. Use --thinking instead for newer models] Maximum number of thinking tokens (only works with --print)" | Deprecated. |
| `--thinking <mode>` *(hidden)* | "Thinking mode: enabled (equivalent to adaptive), disabled" | Replacement for the above. |
| `--thinking-display <display>` *(hidden)* | "How thinking content appears in the response" | |

### Model selection
| Flag | Help text | Why it matters |
|---|---|---|
| `--model <model>` | "Model for the current session. Provide an alias for the latest model (e.g. 'fable', 'opus', or 'sonnet') or a model's full name (e.g. 'claude-fable-5')." | |
| `--fallback-model <model>` | "Enable automatic fallback to specified model(s) when the default model is overloaded or not available. Accepts a comma-separated list to try each in order. Re-tries the primary at the start of each user turn." | Now accepts a *list*. Resilience for long unattended runs. |
| `--effort <level>` | "Effort level for the current session (low, medium, high, xhigh, max)" | New reasoning-effort axis, orthogonal to model choice. |
| `--betas <betas...>` | "Beta headers to include in API requests (API key users only)" | |
| `--advisor <model>` *(hidden)* | "Enable the server-side advisor tool with the specified model (alias or full ID)." | |

### Background / multi-session orchestration (entirely absent from the doc)
| Flag / command | Help text | Why it matters |
|---|---|---|
| `--bg, --background` | "Start the session in the background and return immediately. Prints the id that `claude attach`, `logs`, `stop` and `rm` take; `claude agents` lists them. With --resume <session-id>, continues that session in the background under the same ID, or starts a copy and says so when the session is already running" | **Claude Code now has its own process supervisor.** A driver may not need to manage child processes at all. |
| `claude agents --json` | "Print active sessions (interactive and background) as a JSON array and exit (for scripting; does not require a TTY)" | Machine-readable session inventory. |
| `claude attach <id>` / `logs <id>` / `stop\|kill <id>` / `rm <id>` / `respawn [id]` | see `claude --help` Commands section | Full lifecycle control over background sessions from the shell. |
| `--messaging-socket-path <path>` | "Cross-session messaging server path: a Unix domain socket on Mac/Linux, a \\\\.\\pipe\\ name on Windows (defaults to an auto-generated path)" | **Cross-session messaging exists.** Sessions can talk to each other over a UDS — a parallel-run design could use this instead of inventing a channel. |
| `--brief` | "Enable SendUserMessage tool for agent-to-user communication" | Gives the agent a tool to push messages to the user/host mid-run. |
| `--channels <servers...>` *(hidden)* | "MCP servers whose channel notifications (inbound push) should register this session. Space-separated server names." | Inbound push from MCP servers into a running session. |
| `-w, --worktree [name]` | "Create a new git worktree for this session (optionally specify a name)" | Built-in isolation for parallel runs — no external worktree management needed. |
| `--tmux` | "Create a tmux session for the worktree (requires --worktree). Uses iTerm2 native panes when available; use --tmux=classic for traditional tmux." | |
| `-n, --name <name>` | "Set a display name for this session (shown in the prompt box, /resume picker, and terminal title)" | |
| `--parent-session-id <id>` *(hidden)* | "Parent session ID for analytics correlation" | Trace nested runs. |
| `--agent-id` / `--agent-name` / `--agent-type` / `--agent-color` / `--team-name` *(all hidden)* | "Teammate agent ID" / "Teammate display name" / "Custom agent type for this teammate" / "Teammate UI color" / "Team name for teammate coordination" | A whole "teammate" coordination layer exists. |
| `--cloud` / `--environment <ccpool_...>` / `--ref` / `--on-branch` / `--correlation-id` / `--forward-home-settings` | see help | Remote/cloud execution on self-hosted runners, with branch targeting and an orchestrator correlation id. Another execution substrate the doc doesn't consider. |
| `--workload <tag>` *(hidden)* | "Workload tag for billing-header attribution (cc_workload). Process-scoped; set by SDK daemon callers that spawn subprocesses for cron work. (only works with --print)" | Billing attribution for subprocess fleets — explicitly designed for exactly this use case. |

### Misc relevant
| Flag | Help text | Why it matters |
|---|---|---|
| `--prompt-suggestions [value]` | "Enable prompt suggestions. In print/SDK mode, emits a prompt_suggestion message after each turn with a predicted next user prompt (choices: \"true\", \"false\", \"1\", \"0\", \"yes\", \"no\", \"on\", \"off\", preset: \"true\")" | Extra stream message type a parser must tolerate. |
| `--debug-file <path>` | "Write debug logs to a specific file path (implicitly enables debug mode)" | Debug logs out-of-band instead of polluting stderr. |
| `-d, --debug [filter]` | "Enable debug mode with optional category filtering (e.g., \"api,hooks\" or \"!1p,!file\")" | Category filters. |
| `--ax-screen-reader` | "Render screen-reader friendly output (flat text, no decorative borders or animations)." | |
| `--file <specs...>` | "File resources to download at startup. Format: file_id:relative_path" | |
| `--init` / `--init-only` / `--maintenance` *(hidden)* | "Run Setup hooks with init trigger, then continue" / "Run Setup and SessionStart:startup hooks, then exit" / "Run Setup hooks with maintenance trigger, then continue" | Warm/prepare a workspace without running a turn. |
| `--enable-auth-status` *(hidden)* | "Enable auth status messages in SDK mode" | |
| `--watch-artifact <artifact>` *(hidden)* | "Watch a Claude artifact (id or URL) in this session and hear about new versions and comments" | |

---

## Corrected launch shape

The doc's shape still works verbatim. A version that reflects 2.1.269:

```
claude -p "<prompt>" \
  --output-format stream-json --verbose \
  --input-format stream-json \
  --include-partial-messages \
  --replay-user-messages \
  --session-id <uuid you generate> \
  --mcp-config '<inline json>' --strict-mcp-config \
  --permission-prompt-tool mcp__workspace__request_permission \
  --permission-mode <acceptEdits|auto|manual|dontAsk|plan|bypassPermissions> \
  --append-system-prompt "<text>" --system-prompt-snapshot off \
  --settings '{"sandbox":{"enabled":true}}' \
  --max-turns <n> --max-budget-usd <n> \
  --resume <sessionId>          # only on continuation
```
Changes worth flagging to the design:
1. `--session-id <uuid>` removes the need to scrape a provider session id before you can resume.
2. `--system-prompt-snapshot off` is required if the appended system prompt varies across resumes.
3. The sandbox is real and reached via `--settings`, not a CLI flag — revisit "ships no sandbox".
4. `--permission-prompts none` is a zero-MCP alternative to `--permission-prompt-tool`.
5. `--bare` is the intended base for programmatic embedding.
