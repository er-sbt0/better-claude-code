<!--
Design record. Fixes the shape of §10's run graph, which the design described as
a diagram and never as an interaction.

Decided from a working prototype — `ai_coding_mgmt_prototype.zip`, "Branching
Agents" — which is a live React component rather than a picture: it forks on
click, moves a camera, and ships three switchable fork transitions behind a
toggle in its own title bar. That toggle is the question it was built to ask.

Decided: 2026-09-13 · supersedes parts of §10 and §19. Every contradiction is
listed under "What this overturns" with the new information that earned it,
because §8's table says decisions are not relitigated without any.
-->

# The branching surface — what the run graph actually is

§10 gave the run graph a picture and a set of rules. Both survive. What neither
supplied was the interaction: where the user's attention lives, what a node
looks like while you are using it rather than while you are looking at it, and
what happens in the second after ⌘⇧F. The prototype answers those, and the
answers move two things the design had written down.

## What was decided

| | Decision |
|---|---|
| **The node** | A conversation column, 560 wide: header, message list, composer. Not a card you expand. |
| **The view** | Two representations of one canvas. `focus` is the default; ⌘\ pulls back to `map`, where columns re-render as §10's shape cards. |
| **Fork motion** | **B — split in place.** The child starts translated onto the parent at `scale(.88)` and peels out of it while the camera fits both. ~900ms. |
| **Fork anchor** | A message, and only where a provider chain entry actually resolves. No snapping to a nearby address. |
| **Filesystem** | Lazy. The fork spawns a session with no git work; the worktree materializes on the child's first mutating tool call. |
| **The feed** | §7's six block types plus `Raw`, virtualized, `(runId, seqRange)` on every block. `EventStream.tsx` is replaced, as its own comment demands. |
| **Fork config** | Immediate. Inherit the parent's model and role; edit the header afterwards. |

Carried over from the prototype without argument: edges anchored to the forked
message rather than to the node, the inherited-context strip on a child, fork
breadcrumbs inside the parent at the branch point, edge slivers for off-screen
siblings, inline decision blocks, and a three-word status vocabulary —
idle / running / **needs you** — that propagates to border, pill, sliver and card.

## The conversation is the node

§10 says a node "collapses to its shape" and that "expanding is a deliberate act,
not the default state." That was written for a node you inspect. The prototype's
node is a node you *talk to*: it has a composer pinned to its bottom edge, and
every branch in the prototype begins with the user typing into the branch.

The rule inverts for a reason that is mechanical rather than aesthetic. **Forking
is anchored to a message, so forking requires reading messages.** A surface whose
default state hides messages makes its primary act cost a preliminary step, every
time. That is the whole argument.

§10's shape is not lost — it is relocated. Map view renders each column as a card
carrying exactly what §10 listed: name, status, `role · model · N msgs`, and a
one-line summary. The shape became a zoom level instead of a collapse state, and
the thing §10 was protecting (that you can see the work without reading it)
is protected by ⌘\ rather than by a chevron.

## Fork is anchored to a message, and only where that is real

`--resume-session-at` takes a **chain-entry uuid**, not our `seq`. The mapping we
need therefore already exists, by accident and at no cost:
`normalizer/src/claude_code.rs:177` lifts the provider's `uuid` off each stdout
line and stores it as `raw_events.idempotency_key`. It was put there to make a
replayed resume deduplicate (§14); it turns out to be the fork address.

Which makes "forkable" a computable property of a block rather than a policy:

- A block whose `idempotency_key` is a bare uuid has an address.
- One provider line can fan out into several events, and those are keyed
  `{uuid}#{i}` — the same chain entry, so the address is the part before the `#`.
- Out-of-band events have no key at all (`wrap_external`, deliberately: "there is
  no provider `uuid` to key on"). Permission decisions are the ones that matter
  here — they reach us over the MCP channel and never appear on stdout. Nor does
  `run.finished`, which is produced at process exit.

So the hover toolbar appears where an address resolves and nowhere else. The
alternative — snapping backwards to the nearest addressable block — was rejected
because the inherited-context strip states a message count out loud, and a fork
that silently rewinds past the block you clicked makes that count a lie on the
one surface whose job is to say where you came from.

### The finding that costs us something

**We have not verified that the stdout `uuid` is the identifier
`--resume-session-at` accepts.** The spike
(`docs/verification/session-fork-spike.sh:127`) read its uuid out of the
transcript JSONL at `~/.claude/projects/<slug>/<session>.jsonl`, not off the
stream. The two are near-certainly the same identifier — the transcript is
written from the same messages — but "near-certainly" is what §16's rule about
help text exists to refuse.

It is a cheap spike and it gates the entire feature: if the stream `uuid` and the
transcript `uuid` are different namespaces, then mid-conversation forking
requires reading the provider's transcript files at fork time, which is a
dependency on a storage layout we do not own and the spike already found varies.
The spike also only exercised a **user** entry; whether an assistant entry is
accepted as a branch point is untested, and the prototype forks from assistant
messages far more often than from user ones.

Run it before building the fork path, not after.

## The worktree arrives late

The first version of this argument was wrong and is worth recording as wrong,
because it is the intuitive version and it will be re-proposed.

**Wrong:** two `claude` processes in one directory corrupt each other's work.
They do not. Disjoint files are fine, Claude Code requires a Read before an Edit
and refuses a stale one, and running several sessions in a single checkout is
ordinary practice. What actually bites people is shared *build* state — a target
directory, a dev-server port, a test database, or either agent running
`git checkout`.

**Right, and specific to this repository:** `driver/src/pipeline.rs:171`
snapshots the **whole worktree** whenever a batch of provider events contained a
stored `tool.completed`, and stamps it `Cause { seq, tool, tool_use_id }` — the
first such completion in the batch, which is to say *this* run's tool call.
Nothing filters by who wrote the bytes, and nothing can: git sees a tree, not an
author. (`Ingested::snapshot_covers` already reports when one snapshot stood for
several completions, so a snapshot standing for more than its `Cause` says is an
existing condition — but within one run, where the extra causes are at least the
same run's.) Two runs
in one tree means run A's shadow commit absorbs whatever B changed during the
window, recorded as caused by A's edit. §13.1 blames against that ref, so Review
attaches A's reasoning to B's hunk. That is deterministic, not racy, and it
contaminates the **trunk** as much as the fork — a child writing while the parent
snapshots poisons the parent's history.

So the tree is what a run needs in order to *write*, not what a fork needs in
order to *exist*. Splitting those:

```
⌘⇧F  ──▶  --fork-session --session-id <ours>     no git, column appears at once
           │
           │  reads, greps, reasons               still no worktree
           │
           │  first Edit / Write / Bash
           ▼
          worktree from the parent's shadow commit at the fork seq
          └──▶ writes isolated, attribution clean, Review has an input
```

Forking to think is free, which is the behavior the thesis wants to encourage and
which an eager worktree taxes on every branch. Forking to work is isolated
exactly as §13 intends. `driver::start` — today one unit that creates the tree,
roots the shadow history, records the run and spawns — splits into a spawn and an
ensure-tree.

Two consequences, neither of them free:

- **A fork that never writes has no worktree and no shadow ref.** Review must
  report attribution as unavailable for it. §13.1 already requires that behavior
  for a missing ref, so this is a new path into an existing branch rather than a
  new state — but the path is new and nothing exercises it.
- **`runs.worktree_path` is nullable already** (`event-log/src/schema.rs:44`), so
  the record survives without a migration. The comment above it
  (`schema.rs:42`) — "a run is created *with* its worktree, so cwd is never the
  user's own checkout" — no longer describes the fork case and needs amending. The *invariant* it protects
  still holds: a run that writes never writes into the user's checkout.

## What this overturns

Three recorded positions move. §8's table forbids relitigating a decision without
new information, so each is paired with the information.

1. **§10: "expanding is a deliberate act, not the default state."**
   New information: the node acquired a composer, and fork is anchored to a
   message. The default state must therefore show messages. §10's shape survives
   as map view.
2. **§13 / `driver/src/lib.rs:6`: "a run is created *with* its worktree."**
   New information: the cost of an eager tree is paid by every fork, and the
   benefit it buys — clean shadow attribution — is only owed once a run writes.
   Now: a run that *writes* is created with its worktree.
3. **§19: "each fork pairs with a worktree forked at the same `seq`."**
   New information: none, and this one is not really overturned. Same `seq`,
   later moment. Recorded only so the wording does not read as violated.

## What this does not decide

- **Whether the stream `uuid` is the fork address.** Above. Highest priority and
  cheapest of anything here.
- **Concurrent forks**, still. The roadmap has carried this as an open unknown
  since the fork spike ran its branches sequentially, and the prototype's central
  image — `by-package` mid-edit while `big-bang` waits on a permission decision —
  is precisely two live descendants of one trunk. Lazy worktrees make the
  filesystem side of it safe; they say nothing about whether the provider
  tolerates it.
- **How many feeds are live at once.** The prototype keeps every column mounted
  and dims the unfocused ones to opacity .2. With §7's virtualized feed inside
  each, that is N live feeds in focus view, and the WebKitGTK budget item in
  §2 — already re-priced once for pan/zoom over a DAG — gets heavier again.
  Unmounting or freezing unfocused feeds is the obvious answer and is not free
  either: a frozen feed that resumes has to catch up on a stream. Measure before
  choosing.
- **Whether a branch can differ in model from its parent.** The header shows
  `role · model` per column and the prototype forks `planner · opus` into
  `coder · sonnet`, but nothing in the driver varies model per run today. The
  header is currently decoration; either make it real or drop it before it reads
  as a feature.

## Sequencing

**A vertical slice, fork first.** One fork working end to end — ⌘⇧F, forked
session, branch `seq` stored, child column, anchored edge, lazy tree on first
write — against a deliberately thin feed. Then §7's six blocks widen underneath
it.

The order is not arbitrary. All three overturned positions live on the fork path,
which makes it the part most likely to be wrong; and the feed is the largest
single piece of work in V1's UI, which makes it the worst thing to build against
a guess. Built second, it is built against `(runId, seqRange)` addresses that a
working fork is already resolving against for real.

The one thing that goes before either: the uuid spike.
