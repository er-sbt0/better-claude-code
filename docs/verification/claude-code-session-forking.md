<!--
Verification record. Establishes whether the fork-graph UI direction is
buildable on Claude Code's session model, which nothing in fixtures/ could
answer: the recorded `resume` scenario is linear, and the 2.1.270 flag record
documents --fork-session, --session-id and --resume-session-at from --help text
only. §16's rule is that help text is not evidence.

Verified: 2026-09-13 · Claude Code 2.1.270 · claude-sonnet-5 · Ubuntu, x86-64
Method: an executable spike, docs/verification/session-fork-spike.sh, which
        asserts rather than prints and exits non-zero on regression. Eight live
        runs in a throwaway git sandbox, $0.09 total.
-->

# Session forking — can one conversation become a graph?

The UI direction is a forkable conversation graph rather than a chat chain. That
rests on one provider capability nobody had checked: whether a single session
can be resumed into **two branches that diverge**, rather than one line that
gets longer. If resuming twice from the same point collides, or silently
linearizes, the graph is not a UI decision — it is a reimplementation of session
state we said in §14 the provider owns.

It does not collide. Every assertion below passed on the first run.

## What was verified

| Claim | Result |
|---|---|
| `--session-id <uuid>` makes the trunk's id ours to assign | holds; `system/init` and `result` agree on it |
| `--resume T --fork-session` yields a **new** session id, not `T` | holds |
| Two forks of the same trunk get **different** ids | holds |
| Each branch inherits the trunk's context | holds — both recalled the trunk's codeword |
| Neither branch can see the other's turns | holds — A never learned B's fact, B never learned A's |
| The trunk survives being forked twice | holds — resuming `T` stays on `T` and absorbs neither branch |
| `--fork-session` **plus** `--session-id` — we name the fork | holds; this is the important one |
| `--resume-session-at <chain-entry uuid>` forks from a **mid-conversation point** | holds; new id, trunk prefix retained, later turns dropped |

The last two are what turn this from "branching exists" into "branching is
drivable". We assign both the trunk's id and each fork's id, so a node in the
graph is addressable by an id we chose *before* the process starts — the same
property §14 already relies on for the linear case. And `--resume-session-at`
means a fork can start from block N rather than only from the tip, which is the
difference between "start over from here" and a genuine graph.

## The finding that costs us something

**The provider records no parentage.** A fork's transcript is a *full copy* of
the parent's, re-stamped with the fork's own `sessionId`; the trunk's id appears
nowhere in it. `parentUuid` exists but chains entries *within* one transcript,
not sessions to each other.

Two consequences:

1. **The graph edge is ours to store, and only ours.** `Run.parentRunId` (§9.1,
   designed for derived runs) is now load-bearing for the primary UI, not just
   for tl;dr artifacts. It should also carry the `seq` the fork was taken at,
   since `--resume-session-at` makes the branch point arbitrary. Nothing can
   reconstruct this from the provider's files after the fact.
2. **Forks are not free on disk, provider-side.** Each fork copied ~200 KB for a
   three-message conversation. A fork-heavy session multiplies the prefix rather
   than sharing it. Not a blocker — it is the provider's directory, not ours —
   but "forking is free" is false, and a UI that encourages speculative
   branching should know the number.

Our own storage is unaffected: the event log is append-only per run (§8), and a
fork is a new run with a parent pointer.

## What this does not prove

- **Concurrency.** The branches were forked and read sequentially. Two forks
  running *at the same time* against one trunk is the actual product case, and
  it was not tested. Worktree-per-run (§13) already isolates the filesystem; what
  is unverified is whether the provider's session store tolerates concurrent
  readers of the same parent transcript.
- **Forking a run with tool calls.** Every run here was text-only. A fork taken
  mid-run, after edits, must pair with a worktree forked at the same `seq` — the
  §13.1 shadow history makes that possible, and nothing has exercised the two
  together.
- **Codex.** Unknown, as everything about Codex is. If its session model has no
  equivalent, the graph is a Claude-Code-tier capability and must degrade (§5).
- **`--resume-session-at`'s safety interlock.** `--resume-drops-turn` exists to
  make truncating resume verifiable rather than blind; the spike passed a chain
  entry uuid without it and was not refused. Anything that offers rewind in the
  UI should re-read that flag first.

## Reproducing

```sh
bash docs/verification/session-fork-spike.sh     # ~2 min, ~$0.09
```

Raw streams are kept in the temp dir the script prints. It never runs `claude`
inside this repository.
