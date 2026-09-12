# Handoff — Agent Workspace design

**Last session:** 2026-09-12 · **Branch:** `main` · **Head:** `a4438a8`
**Tree:** clean · **Code written:** none — still design only, but no longer
design *alone*: there are now recorded fixtures and a verification record.

Artifacts:

```
agent-workspace-design.md                  the design
docs/verification/claude-code-cli-2.1.269.md   what was checked, how, against what
fixtures/claude-code/2.1.269/              13 recorded streams + record.sh + MANIFEST.md
```

Everything below is state that lives in a person's head rather than in those files.

---

## What the last session changed

It discharged the largest verification debt: **Claude Code's surface is now
evidence.** Three commits, in dependency order.

1. **`8e6dc6d` — flags verified against 2.1.269.** Every flag in §5's launch
   shape exists with the exact spelling asserted. The problems were omissions,
   not errors.
2. **`5f7bf8b` — 13 real `stream-json` streams recorded.** Live nested runs
   against a throwaway sandbox repo; nothing hand-written. `record.sh`
   regenerates the set for any future version.
3. **`a4438a8` — the doc reconciled against both.** Six rows of §5's event
   mapping were wrong, one claim in §6 was false, and §14's token columns would
   have under-reported silently.

The rule in §16 — record before you model — paid for itself in one afternoon,
which is now written into the doc as principle 18.

### The three findings that change decisions, not just details

- **Both agents ship sandboxes now.** Claude Code 2.1.269 has filesystem
  confinement and network egress control, reached from a subprocess driver via
  `--settings`. Codex already had one. §12 option B was sized as "the one
  capability a competitor cannot trivially copy"; that premise is gone for both
  agents we support first. The doc now re-prices it as an
  integration-and-uniformity argument. **This is the biggest open re-think.**
- **Claude Code grew a supervisor and worktrees.** `--bg`,
  `claude agents --json`, `attach`/`logs`/`stop`, and `-w/--worktree`. §13 and
  §17 assume we build both. Owning them is still defensible on agent-neutrality
  grounds — Codex has neither — but it needs to be a decision.
- **Usage lives in three non-agreeing fields.** `result.usage` covers the main
  model only and misses side-model spend; `modelUsage` reconciles exactly to
  `total_cost_usd`. Wrong choice here produces numbers that are quietly wrong
  forever.

---

## Decisions now recorded (do not relitigate without new information)

| Decision | Where | Rationale |
|---|---|---|
| CLI subprocess, not a Node SDK sidecar | §5 | fixtures absorb drift; a sidecar would outlive the first adapter |
| Provider owns session state, we own run state | §14 | pre-existing; unchanged — but we now *assign* the session id |
| Lexical only where something is editable | §9, §9.1 | the feed does not virtualize under it |
| Diagrams are declarative specs, never executed code | §6.1 | §15 collision otherwise |
| Derived runs get zero tools | §9.1, §15 | untrusted input; confused-deputy risk |
| V1 ships permission option A only | §12 | never claim enforcement we lack |
| Transient progress never enters the event log | §8 | new; 45 progress messages in one 52-line fixture |

---

## Open questions, in rough priority order

1. **Re-price §12 option B (§12, §5).** Promoted to the top by the sandbox
   finding. The cheap alternative — drive the vendor sandboxes through
   `--settings` and `--sandbox`, label which engine is enforcing — now gets most
   of the user-visible value. Decide before anything in the runtime assumes we
   own enforcement.
2. **Vendor supervisor and worktrees, or our own (§13, §17).** Same shape of
   question, smaller stakes, and it touches V1 scope directly.
3. **Business model (§2).** Unresolved, and §14 continues to harden around
   single-user local storage. Unchanged from the last handoff.
4. **ACP as the adapter substrate (§5).** Still flagged time-sensitive, still
   never evaluated. If declined, the doc asks for the reason to be recorded —
   it still has not been.
5. **Bidirectional canvas vs. read-only (§6.1).** Unchanged; not chosen.

---

## Verification debts

**Discharged:** every §5 Claude Code flag, and the Claude Code event mapping.
Both now have a dated record and a regeneration script.

**Remaining:**

- **Codex, entirely.** Not installed on this machine. `codex exec --json` event
  shapes, `codex exec resume`, the `--sandbox` modes, and the key names in §5's
  mapping table are all still asserted from stale knowledge. This is now the
  largest debt by a wide margin, and §5's "write the second adapter before
  extracting the abstraction" rule is blocked behind it.
- **Two event classes need a workspace MCP server that does not exist:**
  `permission.requested` via `--permission-prompt-tool`, and `workspace.report`
  (§6 source 3 — the spine of the semantic model, still entirely unproven).
- **Subagent traffic.** `parent_tool_use_id` is present but null in every
  fixture. Exactly the shape that hides a normalizer bug.
- **ACP maturity and adoption (§5).** Never checked.
- **Competitive table (§2).** Moves quarterly; last reviewed at authoring time.

---

## Suggested next action

**Install Codex and record its fixtures**, mirroring
`fixtures/claude-code/2.1.269/`. It is the largest remaining debt, it unblocks
the V2 abstraction, and the method is now a script rather than an experiment.

Two alternatives, if Codex is not available:

- **Build the minimal workspace MCP server** (three tools: `report_finding`,
  `open_plan`, `record_test_run`) and record a tier-2 run. §6 source 3 is
  called the spine of the semantic model and has never been demonstrated once.
  It also unlocks the `--permission-prompt-tool` fixture, since both need the
  same server.
- **Settle question 1.** It is answerable from reading — Claude Code's
  sandboxing docs and Codex's sandbox modes — and it is upstream of how much
  runtime gets built.

Anything that involves writing a normalizer should wait for Codex. Generalizing
from one recorded agent is the mistake §5 exists to prevent, and it would be an
easy one to make now that one agent is so well documented.
