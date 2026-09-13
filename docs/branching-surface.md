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

Amended 2026-09-13, on the commit that built the fork path: the Filesystem row
reversed. Building it produced the new information, which is the only thing that
lets a record contradict itself. The reversal is argued under "What this
overturns"; the prose it replaced is described there rather than deleted
silently, because the wrong version is the one that will be re-proposed.

Amended again 2026-09-13, on the commit that built the column: the open question
about the model is closed, and `role` is dropped with it. Argued under "The
model is a column; `role` is not a thing", which is where the bullet that used
to sit under "What this does not decide" went.
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
| **Filesystem** | **Eager.** The fork is created with its worktree, checked out from the parent's shadow commit at the branch-point `seq`. Reversed from "lazy" once it was built — see "What this overturns". |
| **The feed** | §7's six block types plus `Raw`, virtualized, `(runId, seqRange)` on every block. `EventStream.tsx` is replaced, as its own comment demands. |
| **Fork config** | Immediate. Inherit the parent's model; edit the header afterwards. (`role` is gone — see below.) |

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
carrying exactly what §10 listed: name, status, `model · N msgs`, and a
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

### The finding that cost us something, and what replaced it

**The spike was run before the fork path was built**, as this section demanded:
`docs/verification/claude-code-fork-address.md`, eight live runs. The stream
`uuid` **is** the identifier `--resume-session-at` accepts, and the claim above
stops being inference.

As a set, every message uuid on stdout is a chain entry in the provider's
transcript, and every transcript `assistant` entry came back on stdout under the
same uuid. A uuid taken off the **stream** — the flag never saw the transcript —
was accepted, branched, and produced a context holding the prefix and not the
suffix. The control is the part that earns belief in the rest: two forks of one
trunk differing only in which stream uuid they were anchored at came back
knowing different things, which rules out both "the flag ignores the uuid" and
"the flag always resumes the tip". And an **assistant** entry is accepted, which
the older session-fork spike never showed and which this surface needs, since it
forks from assistant messages far more often than from user ones.

What that buys is a dependency removed rather than a capability added: the fork
path opens no file under `~/.claude/projects/`, so the storage layout we do not
own is off the product path entirely. Two details it settled in passing, both
now encoded in the driver:

- **`{uuid}#{i}` is refused by name** — `No message found with message.uuid of:
  <uuid>#0`. The fan-out suffix is ours alone, so an address is stripped at the
  first `#` before it is passed.
- **A refused fork still emits a `result` with a fresh `session_id`.** An
  unresolvable address is loud rather than a silent resume from the tip, which
  is the good news; the shape of the loudness is the trap. It arrives as a
  *completed process with an error subtype*, not as a failed spawn, so
  acceptance is read off `subtype` — `success` against `error_during_execution`
  — and never off having received a result or a session id. The spike's own
  first draft made that mistake.

What it still costs us is smaller, and is about phrasing. The match is exact in
one direction only: every stdout message uuid is a chain entry, but 31 of that
trunk's 34 chain entries never reached stdout, and the ones that matter are the
user's own prompt entries. So **every keyed block is forkable** stays true while
*every chain entry is keyed* is false — the distinction the hover toolbar above
is already drawn against. A surface that wanted to fork at "just before I asked
that" would be addressing an entry we hold no key for, and would be back to
reading provider files.

Three things the spike did not prove, and none of them blocks the fork path:

- **Tool-bearing conversations as anchors.** The trunk was text-only on purpose,
  to keep the truncation signal clean. Nothing has forked at an assistant
  message that requested a tool, with the `user` tool-result entry after it —
  which is what a branch point looks like in real work.
- **Stability of an address across a long conversation or a compaction.** Three
  assistant entries, one sitting. Nothing checked that a uuid observed in turn
  one is still resolvable twenty turns later, in a file the provider owns and
  rewrites.
- **Concurrency**, still, and for the fourth time. Every run in the spike was
  sequential.

## The worktree is forked with the conversation

Two intuitions about this are wrong, and both are worth recording as wrong,
because both will be re-proposed.

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
same run's.) Two runs in one tree means run A's shadow commit absorbs whatever B
changed during the window, recorded as caused by A's edit. §13.1 blames against
that ref, so Review attaches A's reasoning to B's hunk. That is deterministic,
not racy, and it contaminates the **trunk** as much as the fork — a child writing
while the parent snapshots poisons the parent's history.

That analysis is unchanged and it is the reason a fork cannot share its parent's
tree. What changed is what follows from it. It was read as *the tree is what a
run needs in order to write, not what a fork needs in order to exist*, and
therefore as a licence to defer the tree to the child's first mutating tool
call. Building that produced the second wrong intuition:

**Wrong:** a worktree created later can be the tree a running child writes into.
It cannot. **A process's cwd is fixed at spawn.** A treeless child starts in some
ancestor's worktree, and by the time it asks to write it has already read files
and computed absolute paths under that root. Holding its first `Edit` at the
permission boundary, making the worktree and letting the call through writes into
the ancestor's tree anyway: the new tree materializes correctly and stays empty
of the run's work. There is no gate that fixes this, because the gate fires after
the decision it would have to change.

And the wart the lazy design recorded as tolerable is not. A treeless child reads
its **ancestor's tree now**, not the tree at the branch point — and "now" is a
moving target in exactly the case this surface exists for. `by-package` mid-edit
while `big-bang` waits is two live descendants of one trunk; a child forked at an
early `seq` greps files the branch point never saw. Reading ahead of the branch
point was priced as a cosmetic loss. For a fork whose whole claim is *this
conversation, from here*, it is the product.

So the tree is forked with the conversation, at the same `seq`:

```
⌘⇧F  ──▶  ShadowHistory::commit_at(branch-point seq)
           │      the newest snapshot at or before it -- and `seq` 0, the
           │      tree as the agent found it, when there is none before
           ▼
          worktree add -b agent/<child> <that commit>
           │      own branch, own tree, own refs/agent-workspace/runs/<child>
           ▼
          --fork-session --session-id <ours> --resume-session-at <uuid>
                  cwd is the child's own tree from its first syscall
```

Forking to think is no longer free; it costs a `worktree add`. That price was
argued against and the argument was right about the price and wrong about what it
was buying. `driver::start`'s git half is still split out as `ensure_tree` — not
so a run can skip it, but because a fork's `base` is the one thing `start` could
never compute.

Two consequences, neither of them free:

- **A parent with no shadow ref cannot be forked.** There is no tree to place the
  child in, and both available substitutes are wrong in the same direction: the
  repository's `HEAD` is a tree the conversation never saw, and the parent's
  worktree *now* is the tree this section just rejected. `driver::fork` refuses,
  loudly.
- **`runs.worktree_path` and `runs.shadow_ref` stay nullable**
  (`event-log/src/schema.rs:51`), and not because a run might start without
  them. A worktree can be removed after the fact — §13 makes cleanup ordinary —
  while the run and its history survive, and §13.1 requires that state to be
  representable so Review can report attribution as *unavailable* rather than
  omit it. The comment above the column (`schema.rs:45`) now says both: what the
  invariant is, and why the column contradicts it in shape.

## What this overturns

§8's table forbids relitigating a decision without new information, so each
position that moves is paired with the information that moved it — **including
one of this record's own**, which is what the third entry is.

1. **§10: "expanding is a deliberate act, not the default state."**
   New information: the node acquired a composer, and fork is anchored to a
   message. The default state must therefore show messages. §10's shape survives
   as map view.
2. **§19: "each fork pairs with a worktree forked at the same `seq`."**
   New information: none, and this was never really overturned. It is now
   implemented exactly as written — `ShadowHistory::commit_at` resolves the
   `seq`, `driver::fork` checks the child out from it — and the entry stays only
   because the version below briefly made it "same `seq`, later moment".
3. **This record's own: "a run that *writes* is created with its worktree."**
   ~~§13 / `driver/src/lib.rs:6`: "a run is created *with* its worktree"~~ was
   overturned above on the grounds that an eager tree is a cost every fork pays
   for a benefit only a writing run collects. **Reinstated, unconditionally.**
   The new information is what building the fork path turned up, and it is
   correctness where the original argument was cost:

   - **A process's cwd is fixed at spawn.** The lazy design spawned the child in
     its nearest ancestor's worktree and planned to make the real tree at the
     permission boundary, on the first `Edit`/`Write`/`Bash`. A tree made then
     redirects nothing — the child has already computed absolute paths under the
     old root — so the write lands in the ancestor's tree and the new tree is
     correct and empty. The gate fires after the decision it would have to
     change, which means no gate works.
   - **A treeless child reads the wrong tree.** "The parent's tree *now*, not at
     the branch point" was recorded above as one tolerable wart. It is not
     tolerable for the central case: fork mid-conversation, or leave the parent
     running — this record's own image, `by-package` mid-edit while `big-bang`
     waits — and the child greps files the branch point never saw.
   - **It was live.** As shipped, a `Permission::Mode` fork had no gate at all
     and would write into its ancestor's worktree, unattributed and silent,
     because a treeless run's `Pipeline` carries no `ShadowHistory`.

   The cost argument survives intact and is simply outranked: forking to think
   now costs a `worktree add`, and that is the price of a fork that is about the
   conversation it claims to be about.

## The model is a column; `role` is not a thing

The header showed `role · model` and neither half was real. One of them is now.

**The model is stored.** `LaunchSpec.model` has been per-run since the driver
existed — it could always vary, and nothing ever wrote it down, so a run's model
was knowable only while its process was alive. It is a column as of schema V3,
written at launch from what we *asked for* rather than from what the provider
announces: `run.started` names a model only sometimes and `turn.finished` names
the models a turn actually billed, so a fallback would rewrite the request and a
run that failed before its first turn would have no answer at all. A fork
inherits its parent's, which is what the Fork config row above always meant, and
it inherits it by copy — onto the child's own row — so that changing one
branch's model later changes one branch. That copy is the whole content of
"whether a branch can differ in model from its parent": it can, because the
answer lives per run rather than being read back through the edge.

**`role` is dropped.** There is no such concept anywhere under the window —
not in `LaunchSpec`, not in the run row, not in the provider's stream. The
prototype's `planner` and `coder` are labels typed into a mock, and the only
place a role could have come from is the shell inventing one. That is the
failure §8's table names: a shell that starts deciding is deciding where no test
can reach it, and a made-up field is worse than a wrong one because it renders
as a feature and features acquire users. If roles turn out to be right they
arrive the way the model did — a launch-time concept in the driver, a column of
its own — and the header reads them instead of authoring them.

What this costs is the prototype's most legible image, `planner · opus` forking
into `coder · sonnet`. Half of it survives, and it is the half that was ever
ours to make true.

## What this does not decide

- ~~**Whether the stream `uuid` is the fork address.**~~ Settled: it is, for
  assistant entries included
  (`docs/verification/claude-code-fork-address.md`). What survives it is
  narrower. **Tool-bearing conversations as anchors** are untested, and they are
  the common case; so is **whether an address outlives a long conversation or a
  compaction**, since the provider owns the file the chain lives in. Neither
  gates the fork path — both are things that would show up as a fork failing
  loudly, which the same spike proved is how forks fail.
- **Concurrent forks**, still — and the surface has now made the question
  reachable by hand. ⌘⇧F is wired end to end, so two live descendants of one
  trunk are two keystrokes rather than a thought experiment, and the registry
  supervises a fork with the same map, supervisor and reaper as any other run:
  nothing in this process serializes them. A worktree per fork makes the
  filesystem side safe — that is what §13 buys, and it is now bought at the fork
  rather than deferred — and still says nothing about whether the provider
  tolerates two sessions branched from one transcript at once. What changed is
  only who can find out: this was an unknown nobody could trip over, and it is
  now an unknown a user can.
- **How many feeds are live at once.** Unchanged as a question, and it has
  acquired one lever and one new fact. Focus view keeps every column mounted, as
  the prototype does — the unfocused ones are not dimmed here, because with a
  real camera the thing that says *this one* is where the camera is pointed.
  **Map view mounts no feed at all**: ⌘\ renders cards, so every body unmounts
  and remounts across the toggle. That is not the answer to the budget question,
  it is the same trade taken at a coarser grain — a remount has to catch up on a
  stream exactly as a thawed feed would, and today's bodies are a flat list
  cheap enough that nobody would notice either way. With §7's virtualized feed
  inside each, N live feeds in focus view is still the number to measure, and
  the WebKitGTK item in §2 — already re-priced once for pan/zoom over a DAG —
  is still where it gets paid. Measure before choosing.
- **Whether every *keyed* block is really forkable.** Recorded as observed
  rather than decided, because it is the first thing the built surface turned up
  and it points at a claim above. Two forks of one live run, from the window: at
  the run's **first** block — a `system` / `hook_started` line, which carries a
  `uuid`, is therefore keyed, and was therefore offered a toolbar — the provider
  ran its `SessionStart:fork` hook and then refused with
  `error_during_execution`, no turns, no cost. At the run's **assistant
  message** it was accepted, and the branch ran a real turn in its own worktree.
  So the address resolved in one case and not the other, and *every keyed block
  is forkable* has a counter-example: the normalizer keys every uuid-bearing
  stdout line, and the provider's own `system` lines carry uuids that are not
  transcript chain entries. The spike could not have seen this — its trunk was
  text-only and it read uuids off `assistant` and `user` messages. Nothing is
  changed here on one observation: the failure is loud, free and immediate,
  which is the property the spike bought and it held. What is not yet known is
  the *rule* — whether "a chain entry" is exactly "a `user` or `assistant`
  message", or something narrower — and that is a question for a spike rather
  than for a filter written from one refused fork.

- **What a fork's worktree is prepared with.** `ForkSpec::post_create_hook` was
  left to the caller, and the first caller is the window, which passes `None`.
  The reasoning is the shell's rather than this record's: a hook is arbitrary
  user code, no run the window starts runs one, and a fork that ran a command
  its parent never ran would be the shell deciding what builds someone's
  repository. The consequence is the one flagged when the eager tree was built
  — a shadow commit respects `.gitignore`, so the child's checkout holds the
  parent's tracked files at the branch point and none of its `node_modules` or
  `target` — and it is now a choice rather than an oversight. What this does not
  decide is whether a fork of a real project *wants* the hook re-run: nothing
  has forked a repository with a build step and tried to work in the result. The
  answer arrives with a hook configuration surface, and the fork inherits it
  then.

## Sequencing

**A vertical slice, fork first.** One fork working end to end — ⌘⇧F, forked
session, branch `seq` stored, child column, anchored edge, worktree forked at the
same `seq` — against a deliberately thin feed. Then §7's six blocks widen
underneath it.

The order is not arbitrary. Every overturned position lives on the fork path,
which makes it the part most likely to be wrong — and one of them turned out to
be, which is entry 3 above; and the feed is the largest
single piece of work in V1's UI, which makes it the worst thing to build against
a guess. Built second, it is built against `(runId, seqRange)` addresses that a
working fork is already resolving against for real.

The one thing that went before either was the uuid spike, and it has
(`docs/verification/claude-code-fork-address.md`).
