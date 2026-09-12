# Handoff — Agent Workspace design

**Last session:** 2026-09-12 · **Branch:** `main` · **Head:** `8ea5bcf`
**Tree:** clean · **Code written:** none — this repo is still design only.

The single artifact is `agent-workspace-design.md`. Everything below is state
that lives in a person's head rather than in the doc.

---

## What the last session changed

Commit `8ea5bcf` folded four threads into the design doc. In dependency order,
because they build on each other:

1. **Block addressability (§7)** — every block carries
   `{runId, seqStart, seqEnd}`. Promoted from implied to a stated V1 foundation.
   Everything else here depends on it.
2. **Block-range actions and derived runs (new §9.1)** — tl;dr, explain-hunk,
   illustrate, commit-message and review are one mechanism:
   `(runId, seqRange, actionId) → artifact`, executed as a derived run on the
   user's own agent with zero tools.
3. **Visual reporting (new §6.1)** — `attach_diagram` emits a declarative spec,
   not code. `read_canvas` recorded as an explicit extension of the
   reporting-only MCP contract.
4. **Agent harnesses (§5)** — concrete launch shapes for Claude Code and Codex,
   the event mapping, and the divergence table.

Supporting edits: three §15 threat rows, fixtures-before-normalizer (§16),
canvas-on-WebKitGTK-first (§4), study Codex's sandbox before §12 option B,
new §20.5 pricing what the foundations unlock, principles 16–17.

---

## Decisions now recorded (do not relitigate without new information)

| Decision | Where | Rationale |
|---|---|---|
| CLI subprocess, not a Node SDK sidecar | §5 | fixtures absorb drift; a sidecar would outlive the first adapter |
| Provider owns session state, we own run state | §14 | pre-existing; unchanged |
| Lexical only where something is editable | §9, §9.1 | the feed does not virtualize under it |
| Diagrams are declarative specs, never executed code | §6.1 | §15 collision otherwise |
| Derived runs get zero tools | §9.1, §15 | untrusted input; confused-deputy risk |
| V1 ships permission option A only | §12 | never claim enforcement we lack |

---

## Open questions, in rough priority order

1. **Business model (§2).** Still unresolved, and §14 hardens around single-user
   local storage. The doc says decide before V2 architecture sets. It has not
   been decided.
2. **ACP as the adapter substrate (§5).** Flagged as time-sensitive, never
   evaluated. If adopted it changes the shape of the V2 abstraction entirely.
   If declined, the doc asks for the reason to be recorded — it has not been.
3. **Bidirectional canvas vs. read-only (§6.1).** Read-only fits the current
   contract; bidirectional is several times the work and needs `read_canvas`.
   Not chosen.
4. **Codex's own sandbox (§5, §12).** Whether it partly moots §12 option B or
   serves as its reference changes the size of the single biggest bet in the
   document. Unexamined.

---

## Verification debts

The doc's own status header applies, and these are the specific items:

- **Every CLI flag in §5.** `--input-format stream-json`,
  `--permission-prompt-tool`, `--mcp-config`, `codex exec --json`,
  `codex exec resume`, the `--sandbox` modes. Asserted from knowledge that was
  already months stale when written. None have been run.
- **Codex event key names (§5).** The event *shapes* are trustworthy; the keys
  are not. The mapping table needs a real stream to be checked against.
- **Competitive table (§2).** Explicitly marked as moving quarterly. Last
  reviewed at authoring time.
- **ACP maturity and adoption (§5).** Never checked.

---

## Suggested next action

Record real `stream-json` and `codex exec --json` streams from a handful of
representative runs into `fixtures/`. It costs an afternoon, discharges the two
largest verification debts above, and §16 argues it must happen before the
normalizer exists rather than after. Having both streams side by side is also
what lets the V2 abstraction fall out of evidence instead of a guess — which is
§5's rule about not generalizing from one example.

Everything else in the doc is cheaper to decide once those fixtures exist.
