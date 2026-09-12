# Agent Workspace — Product & Technical Design

> **Status:** design draft. Vendor CLI flags, SDK surfaces, and protocol names in
> this document were accurate as of early 2026 and change without semver
> guarantees. Verify every one against the current tool before building on it.

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

- turn boundaries, message text, thinking (where exposed)
- tool calls with **typed arguments** — `Edit{file_path, old_string, new_string}` gives you changed files exactly
- tool results, exit codes, durations
- token usage and cost
- session/run lifecycle and errors

This alone powers the review surface and the parallel-run triage view — i.e.
both of the wedge features in §1. **Ship this first; it carries the product.**

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
```

Paired with a short system-prompt append telling the agent these tools exist and
when to use them, this converts the hardest problem in the product (extracting
meaning from prose) into a prompt-engineering problem with a typed schema at the
end of it. It works identically across every agent that speaks MCP.

Caveats to design around, not ignore:

- the agent may simply not call them — every semantic block must be optional
- an agent that calls them is a **tier 2** integration; tier 1 still works, with less
- these tools are *reporting*, not control — they never let the agent mutate workspace policy

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
```

> **Rule: if you cannot recompute it from the raw log, it is a raw event. If you
> can, it is a projection and must not be the source of truth.**

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
something no terminal and no `git diff` can offer. Accept, reject, comment,
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

Ubuntu-first makes B unusually tractable, and it is the one capability in this
document that a competitor cannot trivially copy. Treat it as a deliberate V2/V3
bet, not an accident.

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

**Agent** — Claude Code only, via `stream-json`. Detect, launch, prompt,
interrupt, resume via provider session id. **No adapter abstraction yet** (§5).

**Worktrees** — run-per-worktree from day one (§13). Create, list, clean up.

**Run feed** — six block types plus `Raw` (§7), plain React, virtualized.

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
- tier-3 PTY fallback agents
- persistent tasks spanning runs
- run forking and run comparison (cheap, given §13)
- background runs
- richer artifact handling
- GitHub integration
- richer MCP integration, inbound and outbound

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
│   ├── agent-protocol/            ← Rust is the source of truth for types
│   └── document-model/
│
├── packages/
│   └── shared-types/              ← GENERATED from crates/ (ts-rs / typeshare)
│
├── fixtures/                      ← recorded event streams per agent version (§16)
│
└── docs/
    ├── architecture.md
    ├── agent-adapters.md
    ├── document-model.md
    ├── event-log.md
    └── security.md
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
