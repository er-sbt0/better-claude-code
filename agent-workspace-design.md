# Agent Workspace — Product & Technical Design

## 1. Product thesis

### Working concept

**Agent Workspace** is a desktop application for managing and interacting with users' existing coding agents.

The product is **not a coding agent** and does not attempt to replace Claude Code, Codex, Gemini CLI, OpenCode, or custom agents.

Instead:

> **Bring your agents. We provide the workspace.**

The application owns the developer workspace, agent sessions, rich presentation, history, orchestration, permissions, and project context. The underlying agent remains responsible for reasoning and executing coding work.

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

Example:

```text
Agent says:
"I changed 7 files and 42 tests passed."

Instead render:

7 files changed   [Review changes]
42 tests passed   [View test report]
3 findings        [Inspect findings]
```

---

## 2. Product positioning

Do not position the product as:

- “a better Claude Code”
- “Claude Desktop for developers”
- “another AI coding agent”

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

---

## 3. Core architecture

```text
                         AGENT WORKSPACE
┌─────────────────────────────────────────────────────────────┐
│                         React UI                            │
│                                                             │
│  Workspace   Agent Runs   Tasks   Files   Documents         │
│                                                             │
│  Lexical        Monaco          xterm.js                    │
└──────────────────────────┬──────────────────────────────────┘
                           │
                    Tauri IPC / Events
                           │
┌──────────────────────────▼──────────────────────────────────┐
│                       Rust Runtime                          │
│                                                             │
│  Workspace Manager                                          │
│  Agent Manager                                              │
│  Process / PTY Manager                                      │
│  Filesystem                                                 │
│  Git                                                        │
│  Permissions                                                │
│  Event Bus                                                  │
│  Persistence                                                │
└──────────────────────────┬──────────────────────────────────┘
                           │
            ┌──────────────┼──────────────┐
            │              │              │
            ▼              ▼              ▼
      Claude Code       Codex CLI      Gemini CLI
      / SDK             / SDK         / CLI
            │              │              │
            └──────────────┼──────────────┘
                           │
                     User's environment
                files / shell / git / MCP
```

### Architectural principle

> **Agents produce events and artifacts. The desktop application renders, persists, observes, and orchestrates them.**

The app should not become tightly coupled to any single agent vendor.

---

## 4. Desktop technology stack

### Recommended stack

| Layer | Recommendation |
|---|---|
| Desktop shell | **Tauri 2** |
| Native runtime | **Rust** |
| UI | **React + TypeScript** |
| Rich agent document | **Lexical** |
| Code editor | **Monaco Editor** |
| Terminal | **xterm.js + native PTY** |
| State | **Zustand** or equivalent lightweight state store |
| Persistence | **SQLite** |
| Agents | User's existing Claude Code, Codex, Gemini CLI, OpenCode, etc. |
| Integrations | **MCP** where appropriate |
| Git | Native `git` initially |

### Why Tauri

The application is a developer tool with heavy OS integration requirements:

- launching agent processes
- PTY management
- filesystem access
- Git
- workspace management
- permissions
- environment variables
- process lifecycle
- OS integration

Tauri + Rust provides a good native boundary while allowing the UI to remain a fast React/TypeScript application.

### Why not make Electron the default

Electron is viable, but the product's most important native capabilities are process, filesystem, PTY, Git, and OS-level integration. A Rust-native layer keeps those responsibilities explicit and avoids making Node the core process-management layer.

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

Examples:

```text
Claude Code
Codex CLI
Gemini CLI
OpenCode
Custom internal CLI agents
Future agent SDKs
```

### Adapter architecture

```text
                 Agent Manager
                       │
        ┌──────────────┼──────────────┐
        │              │              │
        ▼              ▼              ▼
 ClaudeCodeAdapter  CodexAdapter  GeminiAdapter
        │              │              │
        ▼              ▼              ▼
   claude process  codex process  gemini process
```

Every provider is exposed through a common internal interface.

### Conceptual interface

```ts
interface CodingAgent {
  id: string
  name: string
  capabilities: AgentCapabilities

  detect(): Promise<boolean>

  start(options: {
    cwd: string
    prompt?: string
    environment?: Record<string, string>
  }): Promise<AgentSession>

  send(session: AgentSession, input: string): Promise<void>

  interrupt(session: AgentSession): Promise<void>

  dispose(session: AgentSession): Promise<void>
}
```

The actual interface should evolve around observable capabilities rather than forcing all agents into identical semantics.

### V1 integration strategy

Start with **process/CLI integration** for the strongest portability and least vendor coupling.

Conceptually:

```text
Your App
   │
   │ spawn + PTY
   ▼
claude / codex / gemini / etc.
   │
   ├── stdout
   ├── stderr
   ├── terminal activity
   └── process state
```

This lets users keep using their existing:

- credentials
- CLI installation
- configuration
- MCP servers
- skills
- permissions
- project setup

### Deeper integrations

Where a provider exposes a stable SDK or structured event interface, add a deeper adapter later.

Examples:

- Claude Agent SDK
- provider APIs / SDKs
- structured agent protocols

The desktop app should present the same high-level workspace regardless of whether the provider uses a CLI process, SDK, or another transport underneath.

---

## 6. Agent event model

The system should be event-driven.

```text
agent.started
assistant.message
assistant.thinking (optional / provider-dependent)
tool.started
tool.completed
file.read
file.changed
terminal.started
terminal.output
permission.requested
permission.resolved
test.started
test.completed
git.changed
artifact.created
agent.finished
agent.failed
```

### Example

```ts
{
  type: "file_changed",
  runId: "run_1842",
  file: "src/auth/refresh.ts",
  additions: 42,
  deletions: 12
}
```

The event stream is the source from which the UI, persistence, and run history are derived.

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

### Initial block types

```text
Text
Heading
Code
FileReference
SymbolReference
Diff
Command
Terminal
TestResult
Finding
Warning
Error
Plan
Task
Checklist
Decision
Question
PermissionRequest
Artifact
Table
Image
GitBranch
Commit
PullRequest
Agent
Run
```

The list should emerge from real workflows rather than being designed exhaustively up front.

### Semantic example

```json
{
  "type": "document",
  "version": 1,
  "children": [
    {
      "type": "heading",
      "level": 2,
      "text": "Investigation"
    },
    {
      "type": "finding",
      "severity": "high",
      "title": "Refresh token race condition",
      "references": [
        {
          "file": "src/auth/refresh.ts",
          "line": 122
        }
      ]
    },
    {
      "type": "diff",
      "files": ["src/auth/refresh.ts"],
      "stats": {
        "additions": 42,
        "deletions": 12
      }
    },
    {
      "type": "test_result",
      "passed": 42,
      "failed": 0
    }
  ]
}
```

### Important separation

Do **not** make Lexical's internal JSON format the application's canonical protocol.

Use:

```text
Agent Document Model
        ↓
Lexical adapter / renderer
        ↓
React components
```

This keeps the semantic model independent of the UI/editor technology.

---

## 8. Lexical's role

Lexical should power the rich document surface, especially for agent messages and editable structured content.

Use custom Lexical nodes for semantic blocks such as:

- findings
- file references
- diffs
- test results
- tasks
- permissions
- artifacts

But keep the application's canonical domain model above Lexical.

This enables future rendering targets:

- desktop UI
- web UI
- exported HTML
- Markdown fallback
- PDF
- collaboration
- API consumers

---

## 9. UI model

The app should not be “one big chat window.”

### Primary surfaces

#### Agent

The conversational interaction surface.

#### Workspace

The project context:

- files
- branch
- Git state
- running processes
- environment
- MCP configuration

#### Runs

Every agent execution is a first-class object.

Example:

```text
Run #1842

Goal
Implement OAuth refresh tokens

Agent
Claude Code

Workspace
my-project

Branch
feat/oauth-refresh

Status
✓ Planning
✓ Investigating
✓ Editing
● Testing

Changes
12 files
+482 / -91

Tests
124 passed
```

#### Tasks

Persistent work items independent of a specific chat message.

Example:

```text
AUTH-142
Implement OAuth refresh

Status: In progress
Agent: Claude Code
Branch: feat/oauth-refresh

Plan
✓ Understand existing auth
✓ Implement refresh tokens
→ Add tests
→ Run integration suite
```

#### Artifacts

Anything produced by an agent:

- diffs
- test reports
- diagrams
- documentation
- screenshots
- generated files
- migration plans

---

## 10. Code-aware interaction

References to the development environment should become interactive objects.

For example:

```text
src/auth/token.ts:84
```

should render as an actionable reference:

- hover preview
- click to open file
- jump to line
- show diff
- related agent actions

Likewise:

```text
npm test
```

can become a terminal/action object.

```text
42 tests passed
```

can become a test-report object.

```text
feat/auth-refresh
```

can become a Git branch object.

The goal is to connect agent output directly to the user's actual workspace.

---

## 11. Permission model

Permissions should be a first-class UI surface rather than raw terminal prompts.

Example:

```text
Agent permission

Delete generated build artifacts

Command
rm -rf ./dist

Workspace
my-app

Impact
312 files

Risk
Medium

[Allow once] [Allow for workspace] [Deny]
```

The app should eventually support policies such as:

```text
Filesystem
✓ Read workspace
✓ Modify workspace
× Modify outside workspace

Terminal
✓ git
✓ npm
? docker

Network
× Default
? api.github.com
```

The exact policy capabilities should depend on what the underlying agent actually exposes.

---

## 12. Local-first persistence

Start with SQLite.

### Core entities

```text
workspaces
projects
agents
agent_sessions
runs
run_events
documents
artifacts
tasks
permissions
mcp_servers
settings
```

### Example Run entity

```ts
Run {
  id
  workspaceId
  agentId
  branch
  prompt
  status
  startedAt
  finishedAt
  parentRunId?
}
```

Store raw/normalized events separately from the derived document representation.

---

## 13. Multi-agent direction

Do not require multi-agent orchestration in V1, but design for it from the beginning.

Long-term:

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

## 14. MCP strategy

Use MCP as an important integration path rather than inventing a completely separate ecosystem.

Potential integrations:

- GitHub
- databases
- browsers
- documentation systems
- issue trackers
- internal tools
- custom enterprise tools

The application should distinguish between:

1. MCP configuration owned by the agent
2. MCP servers surfaced/managed by the workspace
3. Rich UI returned by compatible integrations

The exact boundary should be kept configurable because different agents may manage MCP differently.

---

## 15. V1 scope

The first version should stay narrow.

### V1 goal

Provide a substantially better desktop workspace around one or two existing coding agents without implementing an agent ourselves.

### V1 features

#### Workspace

- open/create project
- filesystem browser
- Git status
- branch information

#### Agents

- detect installed agents
- configure agent adapters
- launch agent in workspace
- send prompts
- interrupt runs
- resume sessions where supported

#### Rich output

- structured agent messages
- file references
- diffs
- terminal blocks
- test results
- findings
- plans
- task/checklist blocks

#### Run history

- current run
- previous runs
- run status
- duration
- agent
- changes
- artifacts

#### Permissions

- surface provider permission requests where observable
- clear approval/rejection UI

### Explicitly out of V1

- building a proprietary coding model/agent
- full cloud backend
- organization-wide collaboration
- complex autonomous orchestration
- replacing GitHub/GitLab
- fully reimplementing each vendor's native agent UI

---

## 16. V2 direction

Add:

- Codex and additional agent adapters
- Gemini / OpenCode / custom agents
- richer MCP integration
- persistent tasks
- branch/run forking
- background agents
- agent handoffs
- richer artifact handling
- GitHub integration
- run comparison

---

## 17. V3 / long-term opportunity

Build the agent orchestration workspace.

Capabilities could include:

- multiple concurrent agents
- delegated subtasks
- agent-to-agent handoff
- reviewer agents
- background execution
- task graphs
- automated validation
- run replay
- run evaluation
- shared team workspaces

The differentiating UI remains the structured representation of agent activity.

---

## 18. Repository structure

Suggested initial structure:

```text
agent-workspace/
├── apps/
│   └── desktop/
│       ├── src/
│       │   ├── app/
│       │   ├── components/
│       │   ├── features/
│       │   │   ├── agents/
│       │   │   ├── workspaces/
│       │   │   ├── runs/
│       │   │   ├── tasks/
│       │   │   ├── documents/
│       │   │   ├── terminal/
│       │   │   └── permissions/
│       │   ├── lexical/
│       │   ├── stores/
│       │   └── lib/
│       └── src-tauri/
│           ├── src/
│           │   ├── agents/
│           │   ├── process/
│           │   ├── pty/
│           │   ├── filesystem/
│           │   ├── git/
│           │   ├── permissions/
│           │   ├── events/
│           │   └── persistence/
│           └── tauri.conf.json
│
├── packages/
│   ├── agent-protocol/
│   ├── document-model/
│   └── shared-types/
│
└── docs/
    ├── architecture.md
    ├── agent-adapters.md
    └── document-model.md
```

The key reusable packages should be independent of the desktop shell where possible.

---

## 19. Key design principles

### 1. Do not build another agent

Use users' existing agents.

### 2. Agent-neutral by design

Claude Code is an integration, not the product.

### 3. Structured over Markdown

Markdown is a fallback/interchange format; semantic objects are the core representation.

### 4. Event-driven internally

The event stream should be durable and replayable.

### 5. Workspace-aware

The UI must understand files, Git, terminals, processes, and projects.

### 6. Local-first

The user's code and agent credentials should remain local unless they explicitly opt into a remote service.

### 7. Progressive integration

Start with process/CLI adapters and add deeper SDK integrations when valuable.

### 8. Don't overfit Lexical

Lexical is a rendering/editor implementation, not the domain protocol.

### 9. Runs are first-class

A run is a durable unit of agent work, not merely a conversation.

### 10. Design for orchestration, ship a focused V1

The architecture should support multiple agents and agent graphs without making V1 complicated.

---

## 20. The north-star experience

The user should be able to open a project and see:

```text
PROJECT
my-app

AGENTS
● Claude Code
● Codex
○ Gemini

CURRENT TASK
Implement OAuth refresh

RUN
Claude Code · 8m 42s

────────────────────────────────────

Investigation

3 findings

⚠ Refresh race condition
  src/auth/refresh.ts:122

⚠ Missing transaction boundary
  src/db/user.ts:81

✓ Existing token expiry behavior confirmed

────────────────────────────────────

Changes

12 files changed       +482 / -91
[Review changes]

────────────────────────────────────

Tests

124 passed   0 failed
[View report]

────────────────────────────────────

Next

→ Add integration tests

[Continue]
```

The experience should feel less like reading an LLM transcript and more like watching a capable software worker operate inside a structured development environment.

---

## 21. Bottom line

The strongest version of the product is:

> **A local-first desktop workspace that lets developers bring their own coding agents and gives those agents a rich, structured, interactive operating environment.**

The technical foundation should be:

**Tauri 2 + Rust + React + TypeScript + Lexical + Monaco + xterm.js + SQLite**, with a provider-neutral agent adapter layer and an event-driven, semantic Agent Document Model.

The first milestone is not a perfect desktop app.

It is proving that this interaction:

```text
existing coding agent
        ↓
agent events / output
        ↓
semantic normalization
        ↓
rich interactive document
        ↓
workspace actions
```

is dramatically better than a Markdown chat transcript.
