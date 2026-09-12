# Agent Workspace — Product & Technical Design

> **Status:** design draft. Vendor CLI flags, SDK surfaces, and protocol names in
> this document were accurate as of early 2026 and change without semver
> guarantees. Verify every one against the current tool before building on it.
>
> **Claude Code's surface was verified on 2026-09-12 against 2.1.269** — flags in
> `docs/verification/claude-code-cli-2.1.269.md`, real event streams in
> `fixtures/claude-code/2.1.269/`. Everything said here about Claude Code is
> evidence now. Everything said about Codex, Gemini and ACP is still assertion.

## 1. Product thesis

### Working concept

**Agent Workspace** is a desktop application for managing and interacting with users' existing coding agents.

The product is **not a coding agent** and does not attempt to replace Claude Code, Codex, Gemini CLI, OpenCode, or custom agents.

Instead:

> **Bring your agents. We provide the workspace.**

The application owns the developer workspace, agent sessions, rich presentation, history, orchestration, permissions, and project context. The underlying agent remains responsible for reasoning and executing coding work.

### The wedge

The differentiating *technology* is structured rendering of agent activity. The
differentiating *pain* is something narrower and more acute:

> **A developer running several agents at once cannot keep track of them, and
> cannot review what they did without babysitting each terminal.**

That is the wedge. Rich rendering is how the review queue becomes reviewable —
it is the mechanism, not the pitch. Concretely, the user we are building for:

- runs 2–6 agent sessions concurrently, today, in terminal tabs or tmux panes
- loses track of which finished, which is blocked, and which is asking for permission
- reviews the result as a `git diff` in a pager, disconnected from the reasoning that produced it
- cannot answer "what did that run actually change, and what did it cost?" after the fact

If the product does not measurably improve *that*, prettier transcripts will not
save it.

### Core differentiation

Most coding-agent interfaces flatten agent activity into Markdown/chat.

Agent Workspace should model agent output as **structured, semantic information**:

- files
- symbols
- diffs
- commands
- terminal output
- tests
- findings
- plans
- tasks
- permissions
- artifacts
- Git objects
- agent runs

This allows the UI to make agent output interactive rather than treating it as a text blob.

The honest version of the payoff, in priority order:

```text
1. Review surface
   Run #1842 · 12 files · +482 / -91        [Review changes]
   Reasoning that produced each hunk is attached to the hunk.

2. Parallel run triage
   ● run 1841  Claude Code   feat/oauth      needs permission
   ● run 1842  Claude Code   feat/billing    testing
   ○ run 1843  Codex         fix/flaky-e2e   done · 3 findings

3. Derived detail (best effort)
   42 tests passed   [View report]
```

Note the ordering. (1) and (2) are achievable from provider metadata alone and
are the reason a user opens the app. (3) requires per-ecosystem parsing or agent
cooperation (see §6) and is enrichment, never a load-bearing promise.

---

## 2. Product positioning

Do not position the product as:

- "a better Claude Code"
- "Claude Desktop for developers"
- "another AI coding agent"

Position it as:

> **The workspace for coding agents.**

Long-term, the product can become the combination of:

- IDE workspace
- agent manager
- run history
- task system
- permission system
- artifact viewer
- agent orchestration layer

The long-term opportunity is closer to **Linear/Jira + IDE + agent orchestration** than to another chatbot.

### Competitive landscape

This space filled up during 2025–26. Any pitch must survive contact with:

| Category | Examples | What they do well | Gap we target |
|---|---|---|---|
| Parallel-agent managers | Conductor, Crystal, Vibe Kanban, Sculptor | worktree-per-run, parallel sessions | output is still a transcript; thin review surface |
| Cloud agent runners | Terragon and similar | no local setup, background runs | not local-first; your code leaves the machine |
| IDE agent panels | Cursor, Zed, VS Code Claude Code extension | editing context, deep IDE integration | single-agent, single-session framing |
| Vendor-native UIs | Claude Code web/desktop surfaces | first-party fidelity | vendor-locked by construction |

Our defensible position is the intersection: **local-first, agent-neutral,
parallel, with a real review surface.** No single competitor above holds all
four. Verify this table before quoting it — the list moves quarterly.

### Business model

BYO-agent means there is no inference margin and no credential to own. That is
a genuine adoption advantage (no signup, no keys, no data egress) and it means
the product has no revenue model by default. Plausible paths, none assumed by
this design:

- paid team tier: shared run history, review handoff, policy enforcement
- paid orchestration: background/scheduled runs, run evaluation
- enterprise: sandbox policy, audit log, SSO

Decide this before V2 architecture hardens around single-user local storage.

---

## 3. Core architecture

```text
                         AGENT WORKSPACE
┌─────────────────────────────────────────────────────────────┐
│                         React UI                            │
│                                                             │
│  Workspace   Runs   Review   Tasks   Files   Documents      │
│                                                             │
│  Virtualized feed   CodeMirror   xterm.js   (Lexical)       │
└──────────────────────────┬──────────────────────────────────┘
                           │
                    Tauri IPC / Events
                           │
┌──────────────────────────▼──────────────────────────────────┐
│                       Rust Runtime                          │
│                                                             │
│  Workspace Manager      Worktree Manager                    │
│  Agent Manager          Workspace MCP Server  ◀── outbound  │
│  Process / PTY Manager  Normalizer                          │
│  Filesystem             Git                                 │
│  Permissions            Event Log (append-only)             │
│  Projections            Persistence                         │
└──────────────────────────┬──────────────────────────────────┘
              structured   │   MCP (workspace tools)
              event stream │   ▲
            ┌──────────────┼───┴──────────┐
            │              │              │
            ▼              ▼              ▼
      Claude Code       Codex CLI      Gemini CLI
      / Agent SDK       / exec --json  / ACP
            │              │              │
            └──────────────┼──────────────┘
                           │
                     User's environment
              worktree / shell / git / user MCP
```

Two channels connect the runtime to an agent, and both matter:

- **Downstream (observe):** the agent's structured event stream — what it said, which tools it called, what they returned.
- **Upstream (collect):** an MCP server *we* host and inject, which the agent calls to hand us structured facts directly (§6).

### Architectural principle

> **Agents produce events and artifacts. The desktop application renders, persists, observes, and orchestrates them.**

The app should not become tightly coupled to any single agent vendor.

---

## 4. Desktop technology stack

### Recommended stack

| Layer | Recommendation | Note |
|---|---|---|
| Desktop shell | **Tauri 2** | see tradeoffs below |
| Native runtime | **Rust** | |
| UI | **React + TypeScript** | |
| Run feed | **Plain React + virtualized list** | *not* Lexical — see §9 |
| Code / diff viewer | **CodeMirror 6** | Monaco only if we become an IDE |
| Rich editable documents | **Lexical** | composer, plans, notes only |
| Diagrams | **Mermaid / Graphviz**, rendered read-only | agent-reported, see §6.1 |
| Canvas editor | *(post-V1)* tldraw or excalidraw | only if the canvas is bidirectional |
| Terminal | **xterm.js + native PTY** | fallback agents and user shells |
| State | **Zustand** or equivalent lightweight state store | |
| Persistence | **SQLite** | |
| Agents | User's existing Claude Code, Codex, Gemini CLI, OpenCode, etc. | |
| Integrations | **MCP**, inbound and outbound | |
| Git | Native `git` initially, incl. worktrees | |

### Why Tauri — and what it costs

Tauri is the right call, but for narrower reasons than "Rust is better at
processes." Node manages processes and PTYs at scale perfectly well; VS Code is
the existence proof. The real arguments:

**For:** memory footprint and binary size for a tool that stays open all day;
Rust for the parts that genuinely benefit — PTY supervision, filesystem
watching, git, and any future sandboxing.

**Against, and this is not small:** Tauri ships no webview. You get WebKitGTK on
Linux, WKWebView on macOS, WebView2 on Windows — three engines, three sets of
bugs. Ubuntu-first development means we build against **WebKitGTK, the weakest
of the three**: worst performance, weakest DevTools, most canvas/media quirks.
A long virtualized feed plus a code viewer plus a terminal is precisely the
workload that exposes it.

Mitigations, adopted deliberately:

- performance budget enforced on WebKitGTK, not on the developer's fastest platform
- virtualize the run feed from day one, not as a later optimization
- prefer CodeMirror over Monaco (dramatically lighter; we need a viewer, not an IDE)
- keep the DOM per feed block small; no editor instance per block
- prototype any canvas or diagram surface on WebKitGTK **before** committing to
  it (§6.1) — canvas and media are precisely where that engine is weakest

If these mitigations start failing, Electron remains a viable fallback and the
Rust runtime boundary survives the switch — which is itself a reason to keep
that boundary explicit.

### Ubuntu-first development

Develop against **Ubuntu 24.04 LTS** as the primary Linux environment.

Initial distribution targets:

- Linux: `.deb` and AppImage
- macOS: `.dmg`
- Windows: native installer

Flatpak can be considered later once the application's filesystem/process permission model is stable.

---

## 5. Agent integration model

### Fundamental rule

The product should **not implement its own coding agent**.

Users bring and authenticate their own agents.

### Structured output is the integration surface — not the PTY

This is the most consequential decision in this document.

An agent's TUI is a redrawing, ANSI-escape-laden screen. Scraping it into
semantic objects is unreliable on day one and broken by every upstream release.
**We never derive meaning from a PTY.** We integrate against structured output:

```text
Claude Code   claude -p --output-format stream-json --verbose   (NDJSON)
              or the Agent SDK (TS/Python) in-process
Codex         codex exec --json                                  (event stream)
Gemini CLI    ACP, where supported
Others        ACP if available, else structured flag, else fallback
```

Verify each of these against the current release; they are not stable APIs.

### Three integration tiers

Every adapter declares which tier it achieves, and the UI degrades honestly.

| Tier | Transport | UI we can offer |
|---|---|---|
| **1. Structured** | NDJSON / SDK / ACP event stream | full semantic feed, review surface, run metrics |
| **2. Cooperative** | tier 1 + workspace MCP server (§6) | above, plus findings/plans/tests the agent reports directly |
| **3. Terminal** | PTY only | an embedded terminal, run lifecycle, and nothing more |

Tier 3 is a real, supported, honest product state. We render a terminal and say
so. We do not fake a semantic feed out of scraped text.

### The two harnesses, concretely

Every flag below must be verified against the installed version; both tools
change them without semver. Record what you verified as fixtures (§16).

**Claude Code**

Verified against 2.1.269 and recorded; see the status header for where.

```bash
claude -p "<prompt>" \
  --output-format stream-json --verbose \   # --verbose is hard-required with stream-json
  --input-format stream-json \              # keeps stdin open for follow-up turns
  --replay-user-messages \                  # stdout acks our stdin writes
  --include-partial-messages \              # token-level streaming; see the caveat below
  --session-id <uuid we generate> \         # we assign it; see §14
  --mcp-config <generated.json> \           # our workspace server (§6)
  --permission-prompt-tool mcp__workspace__request_permission \
  --append-system-prompt "<the workspace MCP tools exist; when to use them>" \
  --system-prompt-snapshot off \            # or the appended prompt is frozen; see below
  --thinking-display summarized \           # or thinking blocks arrive empty
  --max-budget-usd <n> --max-turns <n> \    # bound an unattended run
  --resume <sessionId>                      # on continuation, see §14
```

`--input-format stream-json` is the piece that turns a one-shot `-p` into a live
session, and the `multi-turn` fixture proves it: two user turns, one process.
It requires `--print` and `--output-format stream-json` together.
`--permission-prompt-tool` is a real callback: the agent calls a tool on *our*
MCP server, our runtime renders the §12 dialog, we return the decision. Same
server as §6 — one more tool on it. It is no longer listed in `--help`, so treat
it as semi-public and pin the version.

Three flags above exist to defuse silent failures, each found by recording:

- **`--system-prompt-snapshot off`.** The default (`on`) records the system
  prompt on a conversation's first request and replays it verbatim forever. A
  resume that passes different `--append-system-prompt` text is silently
  ignored. §6's whole premise is an appended prompt telling the agent our tools
  exist; if that text ever changes, the change does not land.
- **`--thinking-display summarized`.** Without it thinking blocks still arrive,
  with `thinking: ""` and a signature. Raw chain-of-thought never reaches the
  wire under any setting; `summarized` and `omitted` are the only values.
- **`--permission-prompts none`** is the zero-MCP alternative to
  `--permission-prompt-tool`: anything that would prompt is denied outright.
  Useful for derived runs (§9.1), which must not prompt at all.

Event mapping into §8's raw vocabulary. Rows marked ⚠ are where the obvious
mapping is wrong — each was caught by a fixture, not by reading:

| stream-json | raw event |
|---|---|
| `system` / `subtype: init` | ⚠ **not** `run.started` — re-emitted at the start of *every* turn on a streamed stdin, same `session_id`, new `uuid`. Start the run on the first one only |
| `assistant`, text block | `assistant.message` |
| `assistant`, thinking block | `assistant.thinking` — empty without `--thinking-display` |
| `assistant`, `tool_use` block | `tool.started` — typed args, §6 source 1 |
| `user`, `tool_result` | `tool.completed` |
| `result` | ⚠ per **turn**, not per run: `result_index` 0,1,… each with `num_turns: 1`. `run.finished` only when the process exits |
| `result`, `subtype != "success"` | `run.failed` — ⚠ key on `subtype`; a wholly permission-blocked run still reports `is_error: false` |
| `result.modelUsage` | `run.metrics` — ⚠ not `result.usage`; see §14 |
| `system` / `permission_denied` | `permission.resolved` (denied) — carries `tool_use_id`, `decision_reason_type` |
| `system` / `hook_started`, `hook_response` | ignore, or `process.output` |
| `system` / `status`, `thinking_tokens` | progress only; never persist as raw events |
| `rate_limit_event` | run state, not conversation content; position in the stream is not stable |
| `stream_event` | raw Anthropic SSE passthrough under `--include-partial-messages` |

Two normalizer hazards the table cannot express:

- **One API message fans out into N `assistant` lines**, one per content block,
  sharing `message.id` and each repeating an *identical* `message.usage`.
  Summing usage across lines double-counts. Dedupe on `message.id`.
- **`--include-partial-messages` delivers the same text twice.** The aggregated
  `assistant` message is still emitted, interleaved *between* the final
  `content_block_delta` and `content_block_stop`. The aggregate arrives before
  the block it aggregates closes. A naive appender renders everything twice.

**Codex**

```bash
codex exec --json "<prompt>" \
  --sandbox workspace-write \
  -c <config overrides, including our MCP server>
# resume: codex exec resume <session_id>
```

A comparable stream: thread/turn lifecycle, agent messages, command execution
begin/end, patch application, token usage on turn completion. Treat the event
*shapes* as reliable and the *key names* as not.

### Where the two agents diverge

Streaming is the easy half and both vendors have nearly solved it. Control is
where they separate, and it is what should shape the V2 interface:

The Claude Code column is verified; the Codex column is not.

| Axis | Claude Code | Codex |
|---|---|---|
| Permissions | interactive callback via `--permission-prompt-tool`, or `--permission-prompts none`; six `--permission-mode` values | launch-time policy + sandbox; no mid-run ask |
| MCP injection | `--mcp-config`, per launch, file *or* inline JSON, variadic; `--strict-mcp-config` to exclude the user's own | `~/.codex/config.toml` or `-c`; must be generated, never mutating the user's file (§14) |
| Sandbox | **ships its own** — filesystem confinement and network egress control, configured via `--settings`, not argv | ships its own sandbox modes |
| Session id | we can *assign* it with `--session-id` | captured from the stream |
| Supervision | ships its own: `--bg`, `claude agents --json`, `attach`/`logs`/`stop` | — |

Consequences:

- §12 option A is not one feature. For Claude Code it is an interactive dialog;
  for Codex it is "choose the posture at launch, show what it did afterwards."
  The UI must label which one the user is getting.
- **Both agents now ship a sandbox**, which is the single biggest change to this
  document since it was written. §12 option B was sized as the thing that makes
  the policy panel truthful where vendors offer nothing. That premise is gone on
  both of the agents we intend to support first. Re-read §12 with that in mind.
- Per-run MCP injection is clean on one and awkward on the other, and MCP
  injection is the spine of §6. Budget for it.
- Claude Code has grown a background-session supervisor and a `--worktree` flag
  of its own. §13 and §17 assume we build both. Decide deliberately whether to
  use the vendor's or own the mechanism — owning it is defensible on
  agent-neutrality grounds (Codex has neither), but it should be a decision, not
  an oversight.

### Decision: CLI subprocess, not an SDK sidecar

§3 puts the Agent Manager in the Rust runtime, but Claude Code's richest surface
is the Agent SDK (TypeScript/Python), which Rust cannot use in-process. Two
coherent options:

- **Rust spawns the CLI and parses NDJSON.** One process, no sidecar. Because
  `--permission-prompt-tool` is an MCP tool, Rust hosts the permission hook
  itself. The bet is on the CLI flag contract.
- **A Node sidecar running the SDK**, speaking an internal protocol back to
  Rust. Typed messages and a `canUseTool` callback instead of an MCP round-trip
  — at the cost of shipping Node inside a Tauri app, which is much of the weight
  Tauri was chosen to avoid.

> **Decision: CLI subprocess.** Golden fixtures (§16) absorb the drift risk, and
> a sidecar adopted for the first adapter would exist for the lifetime of every
> adapter after it.

Revisit only if the CLI contract proves measurably less stable than the SDK's.

### Consider ACP before inventing an interface

Zed's **Agent Client Protocol** (JSON-RPC over stdio) exists to standardize
exactly this editor↔agent boundary, with adapters for multiple agents. Before
finalizing a bespoke `CodingAgent` interface, evaluate adopting ACP as the
adapter substrate — every ACP-speaking agent would then work without bespoke
work on our side. If we decline it, record why. Check its current maturity and
adoption; this recommendation is time-sensitive.

### Do not generalize the interface from one example

The interface below is a sketch, deliberately not a commitment:

```ts
interface CodingAgent {
  id: string
  name: string
  tier: 1 | 2 | 3
  capabilities: AgentCapabilities

  detect(): Promise<{ installed: boolean; version?: string }>

  start(options: {
    cwd: string              // a worktree, see §13
    prompt?: string
    environment?: Record<string, string>
    mcpServers?: McpServerSpec[]   // includes our workspace server
  }): Promise<AgentSession>

  send(session: AgentSession, input: string): Promise<void>
  interrupt(session: AgentSession): Promise<void>
  dispose(session: AgentSession): Promise<void>
}
```

**Rule: write the second adapter before extracting the abstraction.** An
interface designed against one agent encodes that agent's assumptions as
universal truth. Build Claude Code concretely, build Codex concretely, then let
the shared shape fall out of the diff between them.

The divergence table above is that exercise done on paper, and it already shows
the sketch is wrong: it has no permission surface, no MCP-injection strategy,
and no capability declaration beyond a tier number. The genuinely shared shape
is narrower, and the differences want to be *data* rather than methods:

```ts
interface AgentCapabilities {
  permissions: 'interactive' | 'launch-policy' | 'none'
  mcpInjection: 'flag' | 'config-file' | 'none'
  sandbox: 'own' | 'none'
  thinking: boolean
  resume: 'session-id' | 'none'
}
```

Shared: detect, launch, stream to normalized events, session id, usage,
interrupt, resume. Per-vendor: everything in the divergence table. A uniform
method set that pretends otherwise lies about half the agents.

### What users keep

Process integration lets users keep their existing:

- credentials
- CLI installation
- configuration
- MCP servers
- skills
- permissions
- project setup

### Deeper integrations

Where a provider exposes a stable SDK or structured event interface, prefer it
over the CLI — it is typically the same data with fewer parsing surprises and
better backpressure handling. The desktop app should present the same
high-level workspace regardless of the transport underneath.

---

## 6. Where semantics actually come from

§7's document model promises blocks like `finding` and `test_result`. No agent
emits those. This section is the bridge, and it is the hardest engineering in
the product — it deserves to be explicit rather than an arrow on a diagram.

There are exactly three sources, in ascending order of value:

### Source 1 — Provider event metadata (cheap, thin, reliable)

From a structured stream you get, for free and without heuristics:

- turn boundaries, message text, thinking (where exposed, and see §5 — Claude
  Code sends empty thinking blocks unless asked otherwise)
- tool calls with **typed arguments** — verified: Claude Code's `Edit` gives
  `{replace_all, file_path, old_string, new_string}` with an *absolute*
  `file_path`, so changed files are exact and need no inference
- tool results and durations
- token usage and cost
- session/run lifecycle and errors

This alone powers the review surface and the parallel-run triage view — i.e.
both of the wedge features in §1. **Ship this first; it carries the product.**

One thing this list used to claim and should not: **exit codes are not on the
wire.** Claude Code returns `tool_use_result` as an object
`{stdout, stderr, interrupted, isImage, noOutputExpected}` on success and as a
bare *string* `"Error: Exit code 1"` on failure — the type itself varies, and
the code only exists inside prose. Treat `is_error` as the signal and the exit
code as source 2 work (below), not as free metadata. The `tool-bash-fail`
fixture is the evidence.

### Source 2 — Tool-call introspection (brittle, per-ecosystem, optional)

Recognizing that `Bash("npm test")` produced jest output, and parsing it into
`{passed: 42, failed: 0}`, means owning a parser per framework per version.
This is real, unglamorous, endless work.

Constrain it rather than attempting it generally:

- the workspace has **configured commands** (`test`, `lint`, `build`) in project settings
- we only parse output for commands we recognize as those
- we prefer machine formats we can request: JUnit XML, `--reporter=json`, `cargo test --message-format=json`
- everything unrecognized stays a terminal block — which is a fine outcome

Never let a failed parse degrade the run. Unparsed is the default, not an error.

### Source 3 — Ask the agent (the spine)

**The workspace hosts its own MCP server and injects it into every launched
agent.** The agent then reports structure to us directly, agent-neutrally, by
construction — no scraping, no parsers, no vendor-specific event schemas.

```text
workspace MCP server — tools exposed to the agent

  report_finding(severity, title, body, references[])
  open_plan(steps[])           update_plan(step_id, status)
  record_test_run(command, passed, failed, report_path?)
  attach_artifact(kind, path, title)
  request_permission(action, detail, risk)     # see §12
  note_decision(title, rationale)
  attach_diagram(format, source, title, references[])   # see §6.1
```

Paired with a short system-prompt append telling the agent these tools exist and
when to use them, this converts the hardest problem in the product (extracting
meaning from prose) into a prompt-engineering problem with a typed schema at the
end of it. It works identically across every agent that speaks MCP.

Caveats to design around, not ignore:

- the agent may simply not call them — every semantic block must be optional
- an agent that calls them is a **tier 2** integration; tier 1 still works, with less
- these tools are *reporting*, not control — they never let the agent mutate workspace policy

### 6.1 Visual reporting — diagrams and canvas

An agent can illustrate, and the channel already exists: it is one more
reporting tool on the same MCP server. No vendor cooperation is needed beyond
MCP support, which makes this tier 2 and agent-neutral by construction.

**Emit a declarative spec, not code.**

```text
attach_diagram(format: "mermaid" | "dot" | "state", source, title, references[])
```

Text in, we render. No code execution, round-trips through JSON losslessly (§7),
diffs well in the event log, and agents emit mermaid competently. That covers
most of what "illustrate this" means in practice: architecture, state machines,
sequence, call graphs, data flow.

Two harder transports, and the rules for them:

- **SVG artifact written to the worktree.** Nearly free — it is just an artifact
  (§10). But agent-produced SVG carries `<script>`, `<foreignObject>`, event
  handlers and external references. **Never inline it into the app DOM.** A
  sanitizer allowlist is an arms race; an iframe is a boundary.
- **Executable illustration** — the agent emits HTML/JS and we run it. The only
  route to genuinely *interactive* illustrations, and a direct collision with
  §15. Defensible only inside the hard boundary described in §15's table. A
  deliberate later bet, never a convenience.

**The direction question, which is the real fork.** Read-only illustration fits
the current contract: the agent draws, the user looks, nothing is mutated. A
*bidirectional* canvas — the user edits the diagram and the agent responds to
the edit — requires the workspace MCP server to expose a read tool:

```text
read_canvas(id)     # an extension of the reporting-only contract
```

A read cannot mutate policy, spawn runs, or escape the worktree, so it is
compatible with the spirit of that rule — but it *is* an extension, and it is
recorded here rather than discovered later. Bidirectional is where the feature
becomes genuinely interesting (sketch a box-and-arrow architecture, hand it to
the agent, have it implement it) and it is several times the work of read-only.

**The agent may simply never call the tool** — §6's standing caveat applies. So
"illustrate this" must also exist as a user-initiated block-range action (§9.1),
which makes the capability reliable rather than opportunistic.

### The resulting pipeline

```text
provider event stream ──┐
workspace MCP calls ────┼──▶ normalizer ──▶ raw event log (immutable)
PTY output (tier 3) ────┘                          │
                                                   ▼
                                          projections (derived)
                                                   │
                                                   ▼
                                       agent document model (§7)
```

---

## 7. Structured Agent Document Model

Markdown should be treated primarily as an interchange or fallback format, not the application's internal representation.

### Principle

> **The agent output model should describe meaning, not formatting.**

Instead of:

```md
### Tests

42 passed
```

represent:

```json
{
  "type": "test_result",
  "passed": 42,
  "failed": 0
}
```

### The degradation invariant

State this as law, because it is what makes an aggressive normalization layer
safe to ship:

> **No event is ever dropped. Anything we cannot classify renders as a raw
> block, and the unprocessed transcript is always one click away.**

A normalizer bug must cost fidelity, never information.

### Start with six block types

It is tempting to enumerate every block type the product might eventually
want. Resist it: a list designed up front encodes guesses as commitments, and
every unused type is schema we are stuck supporting. Ship these six:

```text
Text          assistant prose
Code          fenced code, language-tagged
ToolCall      name + typed args + result + status + duration
Diff          file set + hunks + stats
Terminal      command + output + exit code
Raw           anything unclassified (the invariant above)
```

Add a block type only when a real workflow demands it and a real source (§6)
can populate it. Likely next: `Finding`, `Plan`, `TestResult`, `PermissionRequest`
— all of which arrive via source 3, which is why source 3 is the spine.

**Entities are not blocks.** `Agent`, `Run`, `Task`, `Workspace` are first-class
rows in the database. A block *references* them by id; it is never one of them.
Collapsing the two is an easy mistake and an expensive one — it puts mutable
entity state inside an immutable document.

### Blocks are addressable

Every block carries the provenance of the raw events it was derived from:

```ts
{ runId: string, seqStart: number, seqEnd: number }
```

This costs almost nothing — §8's envelope already carries `seq` — but it has to
be plumbed through the normalizer into the block, and it is painful to retrofit.
It is what makes two separate things possible:

- **Review** can attach reasoning and tool calls to the hunk that they produced,
  which is the surface that justifies the product (§10).
- **Block-range actions** (§9.1) — selecting "that response" is selecting a
  contiguous `seq` range, and every derived capability is a function of it.

Treat addressability as a V1 foundation, not a feature.

### Extensibility rules

- every document carries a `version`; every block carries a `type` and may carry a `v`
- an unknown `type` renders as `Raw` and is preserved verbatim on round-trip
- blocks are additive-only; never repurpose a `type` string
- block payloads must round-trip through JSON without loss — the event log is forever

### Semantic example

```json
{
  "type": "document",
  "version": 1,
  "children": [
    {
      "type": "text",
      "text": "Investigating the refresh path."
    },
    {
      "type": "tool_call",
      "name": "Read",
      "args": { "file_path": "src/auth/refresh.ts" },
      "status": "ok",
      "durationMs": 42
    },
    {
      "type": "diff",
      "files": ["src/auth/refresh.ts"],
      "stats": { "additions": 42, "deletions": 12 },
      "runId": "run_1842"
    }
  ]
}
```

### Important separation

Do **not** make any editor's internal JSON format the application's canonical protocol.

```text
Agent Document Model
        ↓
renderer (React components; Lexical only where editable)
        ↓
UI
```

This keeps the semantic model independent of the UI/editor technology.

---

## 8. Event model and the event log

The system should be event-driven. §6 describes where events come from; this
section describes what we store and what we derive from it.

### Two kinds of event — do not conflate them

`tool.completed` and `test.completed` look like peers and are not. One is
something we observed; the other is something we concluded. Storing them in the
same place corrupts the storage model — it makes an interpretation as immutable
as a fact.

**Raw events — observed, immutable, append-only.** These are facts about what
the provider did. They are the log. They are never edited, never deleted, never
recomputed.

```text
run.started
run.finished
run.failed
assistant.message
assistant.thinking          (provider-dependent)
tool.started
tool.completed
permission.requested
permission.resolved
workspace.report            (a workspace MCP tool call, §6 source 3)
process.output              (tier 3 / PTY)
```

**Derived facts — projections, recomputable, disposable.** These are our
interpretation. If the normalizer improves, we drop them and recompute from the
log.

```text
file.changed        ← derived from Edit/Write tool args + git
test.result         ← derived from a configured test command, or reported
finding             ← reported via workspace MCP, or absent
git.state           ← derived from the worktree
run.metrics         ← derived from usage in the raw stream
hunk.attribution    ← blame against the run's shadow history (§13.1)
```

> **Rule: if you cannot recompute it from the raw log, it is a raw event. If you
> can, it is a projection and must not be the source of truth.**

**The log is not the only durable artifact.** `hunk.attribution` is recomputable
— but only against the run's shadow history (§13.1), which is itself *not*
recomputable from the log. File content after a `Bash` mutation cannot be
reconstructed from tool arguments; that is the whole reason the shadow history
exists. So a run has two durable records, the event log and its shadow ref, and
neither derives from the other. Deleting the ref does not corrupt the log, it
just makes Review's attribution unavailable for that run forever — which must be
shown rather than hidden, exactly as §14 requires for a deleted provider session.

### A third kind: transient progress

The recorded streams (§16) force a category the original two do not cover. One
52-line fixture contains 45 `system/thinking_tokens` messages, each an updated
running estimate. `system/status` and the `stream_event` SSE passthrough are the
same: they describe the *present moment* of a turn and carry nothing once it
ends.

These are observed facts, so §7's invariant argues for keeping them, and the
storage cost argues loudly against — a long run would bury its own log in
progress ticks.

> **Rule: transient progress drives the UI and is never written to the log.**
> It is not dropped information, because it is not information — it is the same
> fact restated. The terminal value of every counter survives in `result`.

The test is whether replaying the log without the message loses anything. For
`thinking_tokens` and `status`, nothing. For a partial `content_block_delta`
whose aggregate `assistant` message is also emitted (§5), nothing. For anything
where the answer is unclear, persist it — the invariant wins ties.

### Envelope

Every raw event carries:

```ts
{
  runId: string
  seq: number            // monotonic per run; ordering and gap detection
  ts: string             // ISO 8601, from the runtime, not the agent
  schema: number         // event schema version; the log outlives the code
  idempotencyKey?: string // dedupe on resume/replay
  type: string           // dotted, lowercase; see list above
  payload: unknown       // versioned by `schema`
}
```

Naming is `dotted.lowercase` everywhere, in both the type list and the
payloads. Pick it once and enforce it in the schema — mixed `file.changed` /
`file_changed` conventions are the kind of drift that survives into the log
forever.

**Correction, from building the log: `seq` gives ordering, not gap detection.**
The two do not survive together. A resumed stream replays events we already
hold; each one consumes a `seq` from the writer and then deduplicates away on
its idempotency key, so the stored sequence has holes wherever a resume
happened. Holes are the normal result of resuming, not evidence of loss, and
anything that alerts on them will cry wolf every time. Ordering is what `seq` is
actually for.

Related, and load-bearing: **the writer assigns `seq`, not the store.** An event
has to be addressable (§7) the moment the normalizer produces it, before it is
durable, or the UI cannot render a run as it streams. The consequence is that a
resumed run must seed its normalizer from the log's last `seq` — a fresh
normalizer restarts at 1 and silently interleaves with history. The log rejects
a non-advancing `seq` rather than storing it, so that mistake is a loud failure
instead of a corrupted order.

### Example

```ts
{
  runId: "run_1842",
  seq: 91,
  ts: "2026-09-12T14:22:08.412Z",
  schema: 1,
  type: "tool.completed",
  payload: {
    name: "Edit",
    args: { file_path: "src/auth/refresh.ts" },
    status: "ok",
    durationMs: 37
  }
}
```

The `file.changed` projection with `+42 / -12` is computed from this plus git —
it is not stored as truth.

### Why events matter

A durable event stream enables:

- crash recovery
- run replay
- audit history
- agent debugging
- run comparison
- analytics
- future collaboration
- multi-agent orchestration
- background agents

Do not make the rendered UI document the only stored representation.

---

## 9. Rendering: what Lexical is and is not for

### Lexical must not power the run feed

The run feed is append-only, streaming, read-only, and unbounded — a 40-minute
run produces thousands of blocks. Lexical is a *text editor* framework. Using it
for the feed means paying reconciler cost per streamed token, fighting streaming
updates, and — decisively — **Lexical does not virtualize**. Long runs will make
the app unusable, and that failure arrives exactly when the user is most
invested.

```text
Run feed            plain React components in a virtualized list
Diff / file view    CodeMirror 6, read-only, one instance at a time
Terminal            xterm.js
Composer            Lexical (mentions, file refs, slash commands)
Editable documents  Lexical (plans, tasks, notes, exported docs)
```

This also deletes an entire custom-node layer from V1.

### Where Lexical earns its place

For genuinely editable structured content — a plan the user edits, a task
document, a note attached to a run — Lexical with custom nodes is the right
tool. Build those nodes as *renderers over the domain model*, so the same block
types can also target:

- desktop UI
- web UI
- exported HTML
- Markdown fallback
- PDF
- API consumers

Keep the application's canonical domain model above Lexical, always.

## 9.1 Block-range actions and derived runs

A recurring class of product request — "summarize this response", "explain this
hunk", "illustrate that", "write the commit message" — looks like a set of
buttons and is really one mechanism:

```text
(runId, seqRange, actionId) → artifact
```

Nothing in that signature mentions an editor. These features do **not** need
Lexical, and they must not become the excuse that puts Lexical in the feed (§9).

### The mechanism

1. **Select a range.** Blocks are addressable (§7), so "a response" is a
   contiguous `seq` range — one `assistant.message` plus the `tool.*` events
   beneath it.
2. **Extract the slice as block JSON**, not as rendered text and not as scraped
   Markdown. This is the concrete payoff of §7: a model handed
   `{type: "diff", files: [...], stats: {+42,-12}}` writes a better summary than
   one handed a prose transcript, for fewer tokens.
3. **Spawn a derived run.** BYO-agent (§2) means we hold no credential and earn
   no inference margin, so the work runs on *the user's own agent* — a
   non-interactive invocation with the slice as input.
4. **Attach the result** to the source range.

### Derived run rules

- a derived run is a `Run` with `parentRunId` set and a `kind` discriminator, so
  its cost lands in the existing metrics columns (§14) instead of vanishing
- **zero tools, no MCP servers, no filesystem write.** Its input is untrusted
  agent output (§15) that may contain injected instructions; a summarizer with
  tools is a confused deputy waiting to happen
- output renders as escaped text, never HTML
- the result is a **projection, not truth** (§8). It carries the `seqRange` it
  covers and is invalidated when the run continues past it — otherwise the
  product ships confidently stale summaries
- it degrades honestly: a tier 3 agent cannot do this, and neither can an agent
  that is not installed. The action is capability-gated like everything else

### The family

```text
tl;dr this range          → summary document
explain this hunk         → annotation attached to the diff
illustrate this           → attach_diagram (§6.1)
write the commit message  → text, pre-filled into the merge action
review this run           → findings, from a second agent
make this a task          → task entity (post-V1)
```

One selection model, one derived-run mechanism, one side panel. Shipping tl;dr
as a one-off button and then rebuilding it four times is the failure mode.

### Where Lexical actually enters

At exactly one moment: when the user can **edit** the result.

- read-only summary in a side panel → plain React over the block model
- the user edits it, pins it to the run, turns it into a task description,
  pastes a hunk reference into it → now it is an editable document, and Lexical
  with custom nodes is correct

The §7 separation holds, and it is the rule most likely to be violated here:
Lexical's `EditorState` is **not** the storage format. A `DiffRefNode` holds
`{runId, seqRange}` and serializes back to a block; the document persists as
block JSON and Lexical is a view over it.

An editable *canvas* is a second editor technology (tldraw, excalidraw) with the
same rule and none of the same code — unbudgeted work, and subject to the
WebKitGTK caveat in §4.

---

## 10. UI model

The app should not be "one big chat window."

### Primary surfaces

#### Runs (the home screen)

Not "Agent." The first thing the user sees is every run, across every workspace,
with its state — because §1's wedge is losing track of parallel agents.

```text
● run 1841  Claude Code  my-app/feat-oauth     ⏸ needs permission   4m
● run 1842  Claude Code  my-app/feat-billing   ● testing           8m42s
○ run 1843  Codex        my-app/fix-flaky-e2e  ✓ done · 3 findings  12m
○ run 1839  Claude Code  other/refactor        ✗ failed             2m
```

Every agent execution is a first-class object:

```text
Run #1842

Goal         Implement OAuth refresh tokens
Agent        Claude Code
Workspace    my-project
Worktree     .worktrees/feat-oauth-refresh
Branch       feat/oauth-refresh
Cost         $0.84 · 214k in / 12k out
Status       ✓ Planning ✓ Investigating ✓ Editing ● Testing
Changes      12 files  +482 / -91
Tests        124 passed
```

#### Review

The surface that justifies the product. A run's diff, hunk by hunk, with the
agent's reasoning and tool calls attached to the hunk that they produced —
something no terminal and no `git diff` can offer. The attachment is real, not
heuristic: it is blame against the run's shadow history (§13.1). Accept, reject, comment,
request changes, hand back to the agent.

#### Agent

The conversational interaction surface for a single run.

#### Workspace

The project context: files, branch, Git state, worktrees, running processes,
environment, MCP configuration.

#### Tasks *(post-V1)*

Persistent work items independent of a specific chat message, spanning runs.

#### Artifacts *(post-V1)*

Anything produced by an agent: diffs, test reports, diagrams, documentation,
screenshots, generated files, migration plans.

---

## 11. Code-aware interaction

References to the development environment should become interactive objects.

```text
src/auth/token.ts:84     → hover preview, open, jump to line, show diff
npm test                 → terminal/action object
42 tests passed          → test-report object
feat/auth-refresh        → Git branch object
```

The goal is to connect agent output directly to the user's actual workspace.

### Safety constraint

These references are constructed from **untrusted text** (see §15). Therefore:

- a file reference resolves only against the run's worktree, after path
  canonicalization; anything escaping it renders as inert text
- a command object is *never* executed on click without an explicit,
  unambiguous user action — no single-click run, no auto-run
- no agent-produced string is ever rendered as HTML

---

## 12. Permission model

Permissions should be a first-class UI surface rather than raw terminal prompts.

### Decide: proxy or sandbox

A policy panel of the form `Network × Default` / `? docker` cannot be enforced
by a process that merely *spawns* a child agent. The child's permission engine
belongs to the child. There are two coherent designs and they are not
interchangeable:

**Option A — Proxy (V1).** Display and answer the provider's own permission
prompts. Claude Code exposes `--permission-prompt-tool`, an MCP tool invoked for
approval — that is precisely the hook, and it is per-vendor. Codex and Gemini
may expose nothing; for those agents the surface degrades to "we show you what
it asked in the terminal."

- Cheap, honest, ships in V1.
- Capability is exactly what the vendor exposes, and no more.
- The UI must say so: `Permission surface: provider-mediated` / `not available`.

**Option B — Sandbox (later, and possibly the real moat).** Enforce policy
ourselves by confining the child process: namespaces, seccomp, and Landlock on
Linux; `sandbox-exec` on macOS. This is substantial work, but it is
**agent-neutral by construction** — it works for agents that expose no
permission API at all, including tier 3 — and it makes the policy panel truthful.

Ubuntu-first makes B unusually tractable. It was written down as the one
capability a competitor cannot trivially copy.

**That claim has weakened and the section should not be read without this.**
Verification (§5) found Claude Code 2.1.269 shipping its own Bash sandbox —
filesystem read/write confinement, network egress control, per-path and
per-env-var deny/mask modes — reachable from a subprocess driver through
`--settings`, with no work from us. Codex already shipped one. Both of the
agents we intend to support first now enforce policy themselves.

What survives of option B, honestly:

- a *uniform* policy across agents that each express policy differently, and a
  single panel that means the same thing in both columns
- coverage for tier 3 and for agents that ship nothing
- enforcement the user's own agent config cannot silently relax

What does not survive is "nobody else offers this." Option B is now an
integration-and-uniformity argument, not a moat argument. That is a much smaller
prize for the same large build, and it should be re-priced before anyone commits
to it. The cheap move — surface and drive the vendor sandboxes through
`--settings` and Codex's `--sandbox`, and label which engine is enforcing — gets
most of the user-visible value for a fraction of the work, at the cost of
claiming only what each vendor actually enforces. Which §12's own rule requires
anyway.

**V1 ships Option A and shows only what it can enforce.**

### V1 surface

```text
Agent permission

Delete generated build artifacts

Command      rm -rf ./dist
Workspace    my-app
Worktree     .worktrees/feat-oauth
Impact       312 files
Risk         Medium

[Allow once] [Allow for workspace] [Deny]
```

### Post-sandbox surface

Only once Option B exists may the UI claim enforcement:

```text
Filesystem   ✓ Read worktree   ✓ Modify worktree   × Modify outside
Terminal     ✓ git   ✓ npm   ? docker
Network      × Default   ? api.github.com
```

Until then, do not draw this panel. A permission UI that does not enforce is
worse than none — it manufactures false confidence about an agent running
arbitrary commands on the user's machine.

---

## 13. Concurrency: one worktree per run

This is a V1 problem, not a V2 feature. The moment a second run starts in the
same repository, two agents edit the same files, `git status` becomes
meaningless, and both runs are corrupted. Parallel runs are the wedge (§1), so
this must be solved before the first release.

```text
my-app/                          main checkout, user's own work
my-app/.worktrees/
  ├── run-1841-feat-oauth/       run 1841 · branch feat/oauth-refresh
  ├── run-1842-feat-billing/     run 1842 · branch feat/billing
  └── run-1843-fix-flaky/        run 1843 · branch fix/flaky-e2e
```

Rules:

- a run is created **with** its worktree and branch; `cwd` is always the worktree
- the user's own checkout is never an agent's working directory by default
- worktree lifecycle is owned by the runtime: create on run start, retain after
  completion until the run is reviewed and merged or discarded
- cleanup is explicit and reversible — never delete a worktree with uncommitted
  changes without confirmation
- setup cost (dependency install per worktree) is real; support a configurable
  post-create hook and consider linking or copying heavy directories

This also makes run forking, run comparison, and background runs (§17) nearly
free later, because isolation already exists.

### 13.1 Shadow history: how Review attributes a hunk

§1, §7 and §10 all promise that Review shows each hunk beside the reasoning and
tool calls that produced it. That promise needs a mechanism, and the obvious one
does not work: matching hunks to `Edit` calls by comparing `old_string` /
`new_string` fails on a file edited three times, a `Write` later edited, a
formatter, and any `Bash` mutation — which is to say, it fails precisely where
Review earns its keep. A tool call records an *intention*; Review needs the
*outcome*.

> **Decision: attribution is blame against a per-run shadow history.**
>
> After every mutating tool call, the runtime snapshots the run's worktree into
> a commit on a private ref, `refs/agent-workspace/runs/<run_id>`, using a
> separate `GIT_INDEX_FILE` and `git commit-tree` so that HEAD, the user's index
> and the working tree are never touched. Each commit carries `run`, `seq`,
> `tool` and `tool-use-id` trailers. A hunk is then attributed by blaming the
> shadow tip: line → shadow commit → `seq` → the events at that `seq` → blocks.

It records outcomes rather than intentions, so it catches formatter runs and
`Bash` mutations for free, and it gives §20.5's run replay — the worktree at any
`seq` — as a checkout rather than as a feature.

Verified against git 2.48.1 before being adopted, per §16:
`docs/verification/git-shadow-attribution.md`, with an executable spike beside
it. Incremental snapshots cost 25–40 ms and are near-flat in repo size; only the
first snapshot of a run scales with the worktree. What the verification changed:

- **Root the ref at a `seq 0` snapshot taken after the post-create hook and
  before the first tool call.** Rooted at the base commit instead, every file the
  hook wrote is attributed to the first `Edit`.
- **Never commit an unchanged tree.** Most tool calls mutate nothing; empty
  commits make blame attribute to the wrong `seq`.
- **Deleted lines need the pickaxe, not blame** — blame only sees survivors.
  Review must run a second query for removed hunks or silently show nothing for
  them.
- **Snapshots serialize per worktree.** The shadow index is not
  concurrency-safe, and parallel subagent tool calls degrade attribution to "one
  of these calls" — untested, since no recorded fixture has subagent traffic.

> **Consequence for the process driver: "snapshot after every mutating tool
> call" is a requirement of the driver, not a later addition.** The driver is the
> only component that knows a tool completed and can act before the next one
> starts. This is why the decision had to be made before the driver was written.

---

## 14. Local-first persistence and state ownership

### Who owns session state?

The agent CLI already persists its own session (Claude Code, for example, keeps
transcripts on disk and supports resumption). If we also store events, there are
two sources of truth — and a session resumed *outside* our app silently
desynchronizes us. This decision shapes the entire persistence layer, so make it
explicitly:

> **Decision: the provider owns conversational/session state; we own run state.**
>
> We mirror the provider's event stream into our log for history, review, and
> analytics, but we never claim to be able to reconstruct a resumable session
> ourselves. Resumption is delegated to the provider via its own session id,
> which we store on the run.

Consequences to accept:

- a run row stores `providerSessionId`; resume means asking the provider to resume
- if the provider's state is deleted, the run becomes read-only history — this is
  correct and must be shown, not hidden
- on resume we reconcile by `seq`/idempotency key and backfill any events we missed
- we never mutate provider state files

Two refinements from the recorded streams:

- **We can assign the id rather than capture it.** Claude Code's `--session-id`
  takes a UUID we generate, so a run row has its `providerSessionId` before the
  process starts. That closes the window where a run exists, has crashed, and
  cannot be resumed because the init event never arrived. Prefer assigning; fall
  back to capturing for agents that only report.
- **A resumed stream does not announce itself.** The `resume-2` fixture's `init`
  is structurally identical to a fresh run's — correct `session_id`, no
  resumption marker, no replayed history, `num_turns` back to 1. The provider
  will not tell us; the run row must remember. Anything keyed on "is this the
  first init" has to come from our state, not the stream.

### Core entities

```text
workspaces      worktrees       runs           raw_events
projects        agents          run_events*    documents
artifacts       tasks           permissions    mcp_servers
settings

* projections — see §8; recomputable, safe to drop and rebuild
```

### Run entity

```ts
Run {
  id
  workspaceId
  worktreeId
  agentId
  providerSessionId?
  branch
  shadowRef            // refs/agent-workspace/runs/<id> — §13.1
  prompt
  status
  startedAt
  finishedAt?
  parentRunId?

  // metrics — the columns users check first
  inputTokens
  outputTokens
  cachedTokens
  costUsd
  durationMs
  filesChanged
  additions
  deletions
}
```

Cost and token accounting come free from the provider's usage data, and they
are the columns users check first. A run entity without them is incomplete.

**Read them from the right field.** Claude Code's `result` message carries usage
in three places that do not agree:

| Field | What it is |
|---|---|
| `total_cost_usd` | authoritative cost **for that turn** — sum across turns for the run |
| `usage` | snake_case, detailed, **main model only** |
| `modelUsage` | camelCase, keyed by full model id, **every model the turn used** |

Every recorded run spends on a side model that appears only in `modelUsage`, and
`sum(modelUsage[*].costUSD)` reconciles exactly to `total_cost_usd` while
`usage` does not. Fed from `usage`, the token columns above under-report
systematically and silently — the kind of error nobody notices until a user adds
the numbers up. Sum `modelUsage`, and store cost per turn rather than per run so
`result_index` (§5) does not overwrite it.

Store raw events separately from derived projections, and treat the raw log as
the only durable truth.

---

## 15. Security: agent output is untrusted input

The product renders agent output as **interactive objects with actions attached**.
Agent output contains tool results fetched from the internet — issue text, web
pages, dependency READMEs, other people's code. That content is adversarial by
default, and prompt injection through it is a known, active technique.

Threat model, minimum viable:

| Threat | Control |
|---|---|
| Injected HTML/script in rendered output | never render agent text as HTML; escape everything; no `dangerouslySetInnerHTML` |
| Confused deputy — a rendered action does something else | actions carry structured, runtime-validated payloads, never parsed strings |
| Path traversal via file references | canonicalize and confine to the run's worktree; inert outside it |
| Auto-executed commands | no click-to-run without explicit user intent; never automatic |
| Injected permission approval | permission requests come from the runtime's own channel, never from parsed message text |
| Secret leakage into the event log | redact known env/credential patterns before persisting; the log is long-lived |
| Malicious MCP server config | workspace-surfaced MCP servers require explicit user approval before injection |
| Agent-produced SVG inlined into the app DOM | never inline; render inside the sandboxed frame below (§6.1) |
| Executable illustration escaping the page | `iframe sandbox="allow-scripts"` with **no** `allow-same-origin`; CSP `default-src 'none'`; no network; no parent DOM access; `postMessage` only, schema-validated at the boundary (§6.1) |
| Derived run turned into a confused deputy | derived runs (§9.1) get zero tools, no MCP servers, and no filesystem write |

The workspace MCP server (§6) is a *reporting* interface. It must expose no tool
that changes policy, approves permissions, spawns runs, or writes outside the
worktree. An agent that gets injected must not be able to escalate through us.

---

## 16. Adapter testing and version drift

Agent CLIs change output formats without semver, without notice, and without
a deprecation window. Unmanaged, every upstream release is an outage for us.

- **Golden fixtures.** Record real event streams per agent per version; replay
  them in CI against the normalizer. Every adapter change ships with a fixture.
- **Version detection at launch.** `detect()` returns a version. Compare against
  a tested-versions manifest.
- **Untested-version banner.** Unknown version → the run still works, the feed
  still renders (the §7 invariant guarantees it), and the UI says
  `Claude Code 2.4.1 — untested with this build. Output may render as raw.`
- **Normalizer failures are telemetry, not crashes.** Count unclassified events
  per adapter per version; a spike is the early warning that a format changed.
- **Contract tests per tier.** A tier-1 adapter must satisfy a shared test suite;
  that suite is the real definition of the interface, more than the TypeScript is.

**Record fixtures before writing the normalizer, not after.** Capturing real
`stream-json` and `codex exec --json` streams from a handful of representative
runs costs an afternoon, is the only defense against drift, and having both
streams side by side is what lets the V2 abstraction (§5) fall out of evidence
rather than out of a guess.

**Half of that is now done.** `fixtures/claude-code/2.1.269/` holds 13 recorded
scenarios with a `record.sh` that regenerates the set against whatever `claude`
is on `PATH`, and a `MANIFEST.md` describing each. It cost an afternoon exactly
as predicted, and it corrected six rows of §5's event-mapping table, one claim
in §6, and the token columns in §14 — all before a line of normalizer existed.
This is the argument for the rule, not an anecdote about it.

Two things the recording exposed about the method itself:

- **Fixtures capture the recording machine.** Hook events, plugin lists and
  skill inventories from the recorder's own config are in every file.
  `--strict-mcp-config` does not suppress them; `--setting-sources` or `--bare`
  would. Left in deliberately — a real driver sees this noise too — but a
  fixture is a recording of an environment, not just of a version.
- **Negative results are fixtures too.** `Bash("echo hello")` is auto-allowed by
  Claude Code's own safe-command classification even under
  `--permission-prompts none`, so the first attempt at a denial fixture recorded
  no denial. Write to a file if you want a permission decision.

Still owed: Codex, and the two event classes that need a workspace MCP server
that does not exist yet — `permission.requested` via `--permission-prompt-tool`,
and `workspace.report` (§6 source 3). Subagent traffic is a third gap worth
closing early: `parent_tool_use_id` is present but null in every current
fixture, which is exactly the shape that hides a normalizer bug.

---

## 17. Multi-agent direction

Do not require multi-agent orchestration in V1, but design for it from the beginning.
Worktree-per-run (§13) and a durable event log (§8) are the two pieces that make
it possible later; both are in V1 for independent reasons.

```text
                       Task
                         │
                  Orchestrator
             ┌───────────┼───────────┐
             ▼           ▼           ▼
         Researcher     Coder      Reviewer
             │           │           │
             └───────────┼───────────┘
                         ▼
                    Final result
```

The UI can represent this as a task graph rather than a flat conversation.

---

## 18. MCP strategy

MCP runs in **two directions**. The outbound one is easy to overlook and is
the more important of the two.

### Outbound — the workspace as an MCP server *(the important one)*

We host a server and inject it into agents so they report structured facts to
us. This is §6 source 3 and the spine of the semantic model. It is
agent-neutral, requires no vendor cooperation beyond MCP support, and replaces
most screen-scraping.

### Inbound — MCP servers as integrations

GitHub, databases, browsers, documentation systems, issue trackers, internal
tools. Here the app must distinguish:

1. MCP configuration owned by the agent (we read, we do not fight it)
2. MCP servers surfaced/managed by the workspace (we inject, user approves — §15)
3. Rich UI returned by compatible integrations

The exact boundary should be kept configurable because different agents manage
MCP differently.

---

## 19. V1 scope

Narrow means narrow. Twenty-five features across five surfaces is close to a
year for a small team, and most of it does not test the thesis. This is the
version that does.

### V1 goal

> Prove that structured run review plus parallel-run triage is dramatically
> better than terminal tabs — for one agent, in one workspace.

### V1 features

**Agent** — Claude Code only, via `stream-json`, using the verified launch shape
in §5. Detect, launch, prompt, interrupt, resume via a session id we assign.
**No adapter abstraction yet** (§5). Pin the tested version and replay the
fixtures in CI from the first commit (§16) — they exist already, so this is free.

**Worktrees** — run-per-worktree from day one (§13). Create, list, clean up.

**Run feed** — six block types plus `Raw` (§7), plain React, virtualized. Every
block carries `(runId, seqRange)` provenance (§7) — cheap now, painful later.

**Review** — the diff surface with reasoning attached to hunks. The one screen
that must be excellent.

**Runs list** — parallel run triage: status, agent, worktree, duration, cost.

**Persistence** — SQLite with `runs` + `raw_events` only. Projections are
recomputed on load until that measurably hurts.

**Permissions** — proxied via `--permission-prompt-tool` (§12 option A).

**Workspace MCP server** — `report_finding`, `open_plan`, `record_test_run`.
Small, but it proves source 3 works and it is cheap to build.

### Explicitly out of V1

- Lexical (nothing is editable yet)
- Monaco (CodeMirror read-only viewer instead)
- block-range actions and derived runs (§9.1)
- diagram and canvas rendering (§6.1)
- Tasks, Artifacts as first-class surfaces
- the adapter abstraction, and any second agent
- MCP server management UI
- the policy/sandbox engine
- multi-agent orchestration of any kind
- building a proprietary coding model/agent
- cloud backend, teams, collaboration
- replacing GitHub/GitLab
- reimplementing any vendor's native agent UI

---

## 20. V2 direction

- **Second adapter (Codex)** — and only now, extract the interface from the diff
  between two real implementations (§5)
- ACP support, if evaluation favors it
- block-range actions (§9.1), starting with tl;dr and commit-message generation
- read-only diagram rendering via `attach_diagram` (§6.1)
- tier-3 PTY fallback agents
- persistent tasks spanning runs
- run forking and run comparison (cheap, given §13)
- background runs
- richer artifact handling
- GitHub integration
- richer MCP integration, inbound and outbound

---

## 20.5 Capability surface enabled by the foundations

This is not a roadmap. It is the argument for why V1's unglamorous foundations —
the event log, worktree-per-run, block addressability — are worth more than they
look. Each capability is priced by the foundation it rides on, and almost none
of it should be built early.

**From the append-only event log (§8)**

| Capability | Cost |
|---|---|
| Run replay — scrub to any `seq` | free |
| Cross-run file history — every time any agent touched a file, and why | free |
| Run comparison — same prompt, two runs, diff the diffs | cheap |
| Cost and token analytics per workspace, agent, prompt shape | free |
| Audit export — what ran, what it touched, what it cost | cheap |
| "What happened while I was away" | cheap |

Cross-run file history is the underrated one: a `SELECT` over data already
stored, and nothing in §2's competitive table can answer it.

**From worktree-per-run (§13)**

- **run forking** — branch a run at `seq` N with a different instruction *(cheap)*
- **racing** — the same prompt to three agents or three phrasings, review all
  three, keep one *(cheap, and the concrete payoff of agent-neutrality)*
- **background and scheduled runs** *(real work — needs a supervisor)*
- **safe discard** *(free, and the reason a user tolerates an unattended agent)*

**From derived runs (§9.1)** — the block-range action family: roughly a day each
once the first one exists.

**From owning process launch**

- version-drift detection (§16) *(real work)*
- environment and secret control *(cheap)*
- the sandbox policy engine (§12 option B) *(the bet)*

**What we deliberately do not offer.** No inference of our own, no model, no
cloud storage, no credential, no code egress. Every capability above is a
consequence of *observing and isolating* the user's own agents. That constraint
is what makes the product defensible, and it is the same constraint that leaves
the business model in §2 unresolved.

---

## 21. V3 / long-term opportunity

Build the agent orchestration workspace:

- multiple concurrent agents, delegated subtasks, agent-to-agent handoff
- reviewer agents, automated validation
- task graphs, run replay, run evaluation
- shared team workspaces
- **the sandbox policy engine (§12 option B)** — the capability competitors
  cannot trivially copy, and the one that makes an enterprise tier real

The differentiating UI remains the structured representation of agent activity.

---

## 22. Repository structure

```text
agent-workspace/
├── apps/
│   └── desktop/
│       ├── src/
│       │   ├── app/
│       │   ├── components/
│       │   ├── features/
│       │   │   ├── runs/          ← home surface
│       │   │   ├── review/        ← the surface that justifies the product
│       │   │   ├── agents/
│       │   │   ├── workspaces/
│       │   │   ├── terminal/
│       │   │   └── permissions/
│       │   ├── stores/
│       │   └── lib/
│       └── src-tauri/
│           ├── src/
│           │   ├── agents/        ← concrete adapters; abstraction comes in V2
│           │   ├── mcp/           ← our outbound workspace MCP server
│           │   ├── normalizer/
│           │   ├── process/
│           │   ├── pty/
│           │   ├── worktrees/
│           │   ├── filesystem/
│           │   ├── git/
│           │   ├── permissions/
│           │   ├── events/
│           │   └── persistence/
│           └── tauri.conf.json
│
├── crates/
│   ├── agent-protocol/            ← exists: Rust is the source of truth for types
│   ├── normalizer/                ← exists: provider streams in, raw events out
│   ├── event-log/                 ← exists: SQLite; the append-only log (§8)
│   ├── worktree/                  ← exists: worktree-per-run and its shadow history
│   └── document-model/
│
├── packages/
│   └── shared-types/              ← GENERATED from crates/ (ts-rs / typeshare)
│
├── fixtures/                      ← recorded event streams per agent version (§16)
│   └── claude-code/2.1.269/       ← exists: 13 scenarios + record.sh + MANIFEST
│
└── docs/
    ├── architecture.md
    ├── agent-adapters.md
    ├── document-model.md
    ├── event-log.md
    ├── security.md
    └── verification/              ← exists: what was checked, against which version
        ├── claude-code-cli-2.1.269.md
        ├── git-shadow-attribution.md     ← §13.1, with an executable spike beside it
        └── shadow-attribution-spike.sh
```

**One source of truth for types.** Hand-written TypeScript types alongside Rust
structs for the same wire format drift within a week. Define them once in Rust
and generate the TypeScript (`ts-rs` or `typeshare`), or define both from JSON
Schema. Do not maintain two copies.

---

## 23. Key design principles

1. **Do not build another agent.** Use users' existing agents.
2. **Agent-neutral by design.** Claude Code is an integration, not the product.
3. **Never scrape a TUI for meaning.** Structured output, or honest degradation to a terminal (§5).
4. **Ask the agent for structure.** The workspace MCP server is the spine of the semantic model (§6).
5. **Structured over Markdown.** Markdown is fallback/interchange; semantic objects are core.
6. **Raw events are truth; everything else is a projection** (§8).
7. **Nothing is ever dropped.** Unclassified renders raw; the transcript is one click away (§7).
8. **One worktree per run.** Isolation is a V1 requirement, not a V2 feature (§13).
9. **Never claim enforcement you do not have.** Proxy honestly, or sandbox for real (§12).
10. **Agent output is untrusted input** (§15).
11. **Local-first.** Code and credentials stay local unless the user opts in.
12. **Do not generalize from one example.** Write the second adapter before extracting the interface (§5).
13. **Lexical and Monaco are implementation details** — and neither belongs in the run feed (§9).
14. **Runs are first-class.** A run is a durable unit of work, not a conversation.
15. **Design for orchestration, ship a focused V1.**
16. **Blocks are addressable.** `(runId, seqRange)` provenance on every block is
    a V1 foundation; Review and every derived action depend on it (§7, §9.1).
17. **Derived work runs on the user's agent, with no tools.** We add no
    inference of our own, and a summarizer holding tools is a confused
    deputy (§9.1, §15).
18. **Record before you model.** Every claim about a vendor's wire format is an
    assertion until a fixture proves it. The first recording session corrected
    six rows of §5, a claim in §6, and the metrics in §14 — none of which looked
    wrong on paper (§16).

---

## 24. The north-star experience

Open the app and see every agent you are running:

```text
RUNS

● 1841  Claude Code  feat/oauth     ⏸ needs permission    4m     $0.31
● 1842  Claude Code  feat/billing   ● testing             8m42s  $0.84
○ 1843  Codex        fix/flaky-e2e  ✓ ready for review   12m     $0.52

────────────────────────────────────────────────────────────

RUN 1843 · Codex · fix/flaky-e2e · ready for review

Findings                                          3

⚠ Refresh race condition          src/auth/refresh.ts:122
⚠ Missing transaction boundary    src/db/user.ts:81
✓ Existing token expiry behavior confirmed

Changes                          12 files  +482 / -91
                                              [Review]

Tests                            124 passed   0 failed
                                          [View report]

Next                             → Add integration tests

                                  [Continue]  [Merge]  [Discard]
```

Then open Review and see each hunk next to the reasoning and tool calls that
produced it — the thing no terminal can show you.

The experience should feel less like reading an LLM transcript and more like
reviewing the work of a capable colleague who kept good notes.

---

## 25. Bottom line

> **A local-first desktop workspace that lets developers run several of their own
> coding agents in parallel, isolated per worktree, and review what each one did
> through a structured, interactive surface instead of a terminal transcript.**

Technical foundation:

**Tauri 2 + Rust + React + TypeScript + CodeMirror + xterm.js + SQLite**, with
structured provider event streams, an outbound workspace MCP server for
semantics, an append-only event log, and worktree-per-run isolation. Lexical
enters when something becomes editable; the adapter abstraction enters with the
second agent.

The first milestone is not a perfect desktop app. It is falsifying one claim:

```text
several agents, running in parallel
        ↓
structured event streams + reported semantics
        ↓
append-only event log
        ↓
run triage + hunk-level review with reasoning attached
        ↓
merge, continue, or discard
```

Test that against terminal tabs and `git diff`. Do not test it against "42 tests
passed" — that was never the painful part.
