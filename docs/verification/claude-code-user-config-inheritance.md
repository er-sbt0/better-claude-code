<!--
Verification record. Establishes what a run launched by `LaunchSpec::argv`
actually inherits from the user's own machine, because §5's "What users keep"
list and `launch.rs`'s unconditional `--strict-mcp-config` could not both be
true and nobody had measured which of the seven items the flag costs us.

Verified: 2026-09-14 · Claude Code 2.1.270 · claude-haiku-4-5-20251001 · Ubuntu 24.04, x86-64
Method: docs/verification/user-config-inheritance-spike.sh, which asserts rather
        than prints. A fixture environment with its own HOME -- the real
        ~/.claude is never read except for a symlinked credential -- and three
        live haiku turns, well under a cent. Most checks need no model call at
        all: hooks fire at SessionStart before a prompt arrives, and the
        `system/init` message enumerates servers, tools, skills, agents,
        commands, plugins and the output style.
-->

# What a driver-launched run inherits from the user

The driver passes `--strict-mcp-config` on every launch. §5 promises that
process integration lets users keep their "credentials, CLI installation,
configuration, **MCP servers**, skills, permissions, project setup". The
question this record answers is not *whether* that list is wrong — it is —
but **how wrong**: whether the flag costs one item or six.

It costs exactly one. The line is sharp and it falls in a single place.

| What the user owns | Where it lives | Reaches the run? |
|---|---|---|
| Credentials | `~/.claude/.credentials.json` (OAuth) | **yes** — `apiKeySource: "none"`, i.e. the user's own login |
| CLI installation | whatever `claude` is on `PATH` | **yes** — `LaunchSpec::binary` |
| Hooks, user scope | `~/.claude/settings.json` | **yes** |
| Hooks, project scope | `<repo>/.claude/settings.json` | **yes** |
| Hooks, local scope | `<repo>/.claude/settings.local.json` | **yes** |
| `env` blocks | user *and* project `settings.json` | **yes**, both, merged |
| Permissions | `permissions` in any settings file | **yes** — a `deny` removes the tool from `init.tools` outright |
| `CLAUDE.md`, user scope | `~/.claude/CLAUDE.md` | **yes** |
| `CLAUDE.md`, project scope | `<repo>/CLAUDE.md` | **yes** |
| Skills | `~/.claude/skills`, `.claude/skills`, plugin-contributed | **yes**, all three |
| Subagents | `~/.claude/agents`, `.claude/agents` | **yes**, both |
| Slash commands | `.claude/commands`, plugin-contributed | **yes**, both |
| Plugins | `enabledPlugins` in user settings | **yes** — listed in `init.plugins` with name, path and version |
| Output style | `outputStyle` in user settings | **yes** |
| Project setup | the worktree the run is given | **yes** (§13 gives it a worktree of the user's repo, not the checkout) |
| **MCP servers, user scope** | `~/.claude.json` → `mcpServers` | **no** |
| **MCP servers, project scope** | `<repo>/.mcp.json` | **no** |
| **MCP servers, local scope** | `~/.claude.json` → `projects.<cwd>.mcpServers` | **no** |
| **MCP servers, plugin-contributed** | a plugin's `mcpServers` block | **no** |
| **MCP servers, account connectors** | the claude.ai connectors on the user's account | **no** |

## The decisive evidence is one diff

The same fixture, the same argv, run twice — once with `--strict-mcp-config`
and once without. The `system/init` message differs in exactly two fields, and
the second is a consequence of the first:

```
DIFF mcp_servers
  loose : ["plugin:spikeplug:plugscope", "userscope", "projectscope", "localscope",
           "claude.ai Google Drive", "claude.ai Gmail", "claude.ai Google Calendar"]
  strict: []
DIFF tools
  loose : [… 30 built-ins …, "mcp__localscope__probe_localscope",
           "mcp__plugin_spikeplug_plugscope__probe_plugscope",
           "mcp__projectscope__probe_projectscope", "mcp__userscope__probe_userscope"]
  strict: [… the same 30 built-ins, and nothing else …]
```

`init.skills`, `init.slash_commands`, `init.agents`, `init.plugins`,
`init.output_style` and `init.memory_paths` are byte-identical between the two.
So are the hooks: all three scopes fire under both, which the spike checks by
having each one append to a file rather than by reading it off the stream.

The servers do not merely fail to appear — they are never **started**. Each
fixture server writes its own name to a witness file on startup; under
`--strict-mcp-config` that file is empty. "Ignoring all other MCP
configurations" is the whole help text and it is accurate.

`--strict-mcp-config` excludes **every** scope. There is no partial spelling:
no flag selects "the user's servers but not the project's", and the CLI offers
no allow-list of server names.

## A plugin is cut in half

This is the one shape that surprises. A plugin can contribute commands, skills
*and* MCP servers. Under `--strict-mcp-config` the plugin still loads — it is
in `init.plugins` with its path and version, its command shows up as
`spikeplug:plugcmd` and its skill as `spikeplug:plugskill` — but its server is
silently dropped. A user who installed a plugin *for* its MCP tools gets the
plugin's prose and none of its capability, and nothing anywhere says so.

## The run is only half hermetic, and the CLI knows the other half

If the argument for the flag is that §6's workspace must describe the run, then
the flag does a third of the job. Hooks are arbitrary user code that runs on
our runs; `CLAUDE.md` is unbounded instruction text we never read; a user's
`permissions.deny` silently narrows the tool surface the run row claims to
record. None of that is excluded, and the driver excludes none of it on purpose
— §5's whole premise is that these are the point.

2.1.270 does offer the knobs, and the driver passes none of them. Measured, all
three with `--strict-mcp-config` still on:

| Flag | Help text | Measured |
|---|---|---|
| `--setting-sources <user,project,local>` | "Comma-separated list of setting sources to load" | `--setting-sources project` fires the project hook and neither of the others. The per-scope knob, and the only surgical one |
| `--restricted` | "…ignores user, project and local settings files (managed settings and `--settings` still apply; add `--strict-mcp-config` to skip MCP servers too)" | no hook fires at all. Also strips `Bash` and every other code-running tool, which makes it unusable here |
| `--bare` | "Minimal mode: skip hooks, LSP, plugin sync, attribution, auto-memory, background prefetches, keychain reads, and CLAUDE.md auto-discovery" | no hook fires. Also: "Anthropic auth is strictly `ANTHROPIC_API_KEY` or `apiKeyHelper`" — **OAuth and keychain are never read**, so it trades the user's credentials for hermeticity and §5's first bullet dies with it |

`--settings <file-or-json>` **merges** with the user's files rather than
replacing them: a hook passed that way fires *alongside* the user, project and
local hooks, all four. It is an addition, never a substitution, so it is not a
way to shut anything out.

§16 had already seen half of this — "Fixtures capture the recording machine.
Hook events, plugin lists and skill inventories from the recorder's own config
are in every file. `--strict-mcp-config` does not suppress them" — and filed it
under *testing hygiene*. It is the same fact, and it was also a product fact the
whole time.

## What it means for §12 and for our own server

Under `--mcp-config <ours> --strict-mcp-config` the run loads exactly one
server and `init.mcp_servers` is `[{"name": "agent-workspace", "status":
"connected"}]`. That is what makes `--permission-prompt-tool
mcp__agent-workspace__request_permission` resolvable, and it is why
`LaunchSpec::validate` refuses an `AskVia` without an `mcp_config`.

Enabling the user's servers would change that namespace in two ways worth
naming before anyone tries it:

- **Under `DenyPrompts` their tools are dead on arrival.** A user MCP tool that
  is not on `--allowed-tools` is denied without a prompt, and we cannot
  pre-grant a name we have never seen. The run would *advertise* the user's
  tools in its system prompt and be refused every one of them — an agent that
  plans around capabilities it does not have, which is worse than an agent that
  was never told about them.
- **A name collision resolves in our favour, silently.** With a user-scope
  server also called `agent-workspace`, the `--mcp-config` definition wins: only
  ours started, and `mcp__agent-workspace__*` was ours. The user's server never
  ran and nothing said so. Same family as the recorded trap that `workspace` is
  a name Claude Code reserves.

## Traps found on the way

- **An invalid `extraKnownMarketplaces` in `settings.json` discards the whole
  file.** A guessed shape (`{"source": {"source": "<path>"}}` instead of the
  real `{"source": "directory", "path": "<path>"}`) made the CLI silently ignore
  *every* setting in that file — hooks included — with nothing on stdout,
  stderr or `--debug`. The spike lets `claude plugin marketplace add` write it.
- **A run with no prompt still fires SessionStart hooks and then exits 0 with
  no `system/init` and no `result`.** Useful — it is what makes most of this
  spike free — but it is also another instance of the shape
  `crates/driver/src/process.rs` now guards: a process that exits 0 having said
  nothing on stdout is not necessarily a failure, and is never self-explaining.
- **`CLAUDECODE` and the `CLAUDE_CODE_*` variables leak into a child `claude`**
  when the spike is run from inside a Claude Code session. The spike uses
  `env -i`; a driver spawning from the desktop app has none of them.
