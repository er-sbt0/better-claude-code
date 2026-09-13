<!--
Verification record. Closes the debt the built surface opened on its first day:
a fork toolbar was offered on a `system` / `hook_started` block — keyed, because
the normalizer keys every uuid-bearing stdout line — and the provider refused it.
The sequel to `claude-code-fork-address.md`, which proved the stdout uuid is the
fork address and then over-generalised from a text-only trunk.

Verified: 2026-09-13 · Claude Code 2.1.270 · claude-sonnet-5 · Ubuntu, x86-64
Method: an executable spike, docs/verification/fork-anchor-types-spike.sh, which
        asserts rather than prints and exits non-zero on failure. Twenty-one
        live runs in a throwaway git sandbox, $0.1757 total.
-->

# Which keyed blocks are forkable? Not all of them

`docs/verification/claude-code-fork-address.md` proved that the `uuid` we already
store off each stdout line is the identifier `--resume-session-at` accepts.
`branching-surface.md` turned that into a rule the surface is built on — "a block
whose `idempotency_key` is a bare uuid has an address" — and §8's decision table
recorded it as *fork is offered exactly where a chain-entry uuid resolves*.

That rule is **false**, and it was false in a way the address spike could not
have seen. Its trunk was text-only and it read uuids off `assistant` and `user`
lines only. The provider puts a `uuid` on **all twelve** of its `type`/`subtype`
combinations, and `normalizer/src/claude_code.rs` keys every one of them. Driving
the real UI produced the counter-example: a fork anchored at `seq 1` — a `system`
/ `hook_started` line — ran the provider's `SessionStart:fork` hook and then came
back `error_during_execution`, no turns, $0.00.

One observation is not a rule. This spike anchored a live fork at one uuid of
**every** combination and read the answer off the result's `subtype`.

**The accepted set is exactly `assistant` and `user`, and it is narrower than the
record claimed.** Nine of the fifteen anchor kinds tested are keyed and refused.

## What was asked, and what came back

The trunk is built so truncation is *observable*, exactly as the address spike's
was: fact A ("codeword ZEPHYR") enters the chain in turn one, fact B ("animal
OTTER") in turn two, and a fork anchored in turn one must know A and must **not**
know B. A fork that silently resumed from the tip would know B and fail these
assertions rather than pass them. Both facts arrive in the *prompt* of their
turn rather than out of a tool result, so that every turn-one anchor is
equivalent and "knows A" is a statement about the fork rather than about where in
the turn the anchor happened to sit.

Two things the address spike left explicitly unproven are closed here by
construction. **The trunk is tool-bearing**: every turn runs a `Read`, so the
`assistant` line carrying a `tool_use` block and the `user` line carrying the
matching `tool_result` are both anchored at. And **the product's own `user`
line** is anchored at: `crates/driver/src/launch.rs` passes
`--input-format stream-json --replay-user-messages` unconditionally, which echoes
the user's prompt back onto stdout with a uuid of its own — a line no `-p` run
can produce, and one the surface offers a fork on every day.

```
claude: 2.1.270 (Claude Code)

trunk: fact A in turn one, fact B in turn two, every turn runs a tool
  [t1] session=36942c80 result=success reply=ok
  ok       turn one uses the session id we assigned
  ok       turn one succeeded
  [t2] session=36942c80 result=success reply=zephyr, otter
  ok       a plain resume stays on the trunk id
  ok       the trunk knows A
  ok       the trunk knows B
  [t3] session=36942c80 result=error_max_turns reply=
  [t4] session=36942c80 result=success reply=i couldn't create `notes.txt` — writing to the sandbox requi
  [t5] session=36942c80 result=success reply=ready

anchors: one uuid per type/subtype/content-block kind, in stream order
    system/hook_started/-              979427cf  early
    system/hook_response/-             ef2f6e49  early
    system/init/-                      911336b9  early
    rate_limit_event/-/-               28a6c9cc  early
    system/thinking_tokens/-           5924c05d  early
    assistant/-/thinking               05845211  earlyA
    assistant/-/tool_use               8c339d89  earlyA
    user/-/tool_result                 191209ed  earlyA
    assistant/-/text                   72f7a8a0  earlyA
    result/success/-                   b4a970b3  earlyA
    result/error_max_turns/-           5ef76373  late
    system/status/-                    3eb668ae  late
    stream_event/-/-                   5ab6e893  late
    system/permission_denied/-         cb119c35  late
    user/-/text                        2f4bfc02  late
  ok       the trunk is tool-bearing: an assistant line carries a tool_use block
  ok       the trunk carries a user tool_result line, the other half of a tool call
  ok       the trunk carries a replayed user prompt line, as every driver-launched run does

forks: one per anchor kind — accepted or refused, read off result.subtype

  anchor system/hook_started/- (early)
  [fork_system_hook_started_-] session=9ee40973 result=error_during_execution reply=
  ok         refused, and refused by name
  ok         a refused fork answers nothing, so it did not resume the tip

  anchor system/hook_response/- (early)
  [fork_system_hook_response_-] session=3fd89a47 result=error_during_execution reply=
  ok         refused, and refused by name
  ok         a refused fork answers nothing, so it did not resume the tip

  anchor system/init/- (early)
  [fork_system_init_-] session=d6910a58 result=error_during_execution reply=
  ok         refused, and refused by name
  ok         a refused fork answers nothing, so it did not resume the tip

  anchor rate_limit_event/-/- (early)
  [fork_rate_limit_event_-_-] session=7472d77d result=error_during_execution reply=
  ok         refused, and refused by name
  ok         a refused fork answers nothing, so it did not resume the tip

  anchor system/thinking_tokens/- (early)
  [fork_system_thinking_tokens_-] session=4349e9c5 result=error_during_execution reply=
  ok         refused, and refused by name
  ok         a refused fork answers nothing, so it did not resume the tip

  anchor assistant/-/thinking (earlyA)
  [fork_assistant_-_thinking] session=e6a8237c result=success reply=zephyr
  ok         accepted, and the branch got its own session id
  ok         it kept the prefix: knows A
  ok         it truncated: does NOT know B

  anchor assistant/-/tool_use (earlyA)
  [fork_assistant_-_tool_use] session=44c8a81b result=success reply=zephyr
  ok         accepted, and the branch got its own session id
  ok         it kept the prefix: knows A
  ok         it truncated: does NOT know B

  anchor user/-/tool_result (earlyA)
  [fork_user_-_tool_result] session=1633bffd result=success reply=zephyr
  ok         accepted, and the branch got its own session id
  ok         it kept the prefix: knows A
  ok         it truncated: does NOT know B

  anchor assistant/-/text (earlyA)
  [fork_assistant_-_text] session=d23eea10 result=success reply=zephyr
  ok         accepted, and the branch got its own session id
  ok         it kept the prefix: knows A
  ok         it truncated: does NOT know B

  anchor result/success/- (earlyA)
  [fork_result_success_-] session=9679db76 result=error_during_execution reply=
  ok         refused, and refused by name
  ok         a refused fork answers nothing, so it did not resume the tip

  anchor result/error_max_turns/- (late)
  [fork_result_error_max_turns_-] session=66e3d28a result=error_during_execution reply=
  ok         refused, and refused by name
  ok         a refused fork answers nothing, so it did not resume the tip

  anchor system/status/- (late)
  [fork_system_status_-] session=256c66a8 result=error_during_execution reply=
  ok         refused, and refused by name
  ok         a refused fork answers nothing, so it did not resume the tip

  anchor stream_event/-/- (late)
  [fork_stream_event_-_-] session=b2c56b97 result=error_during_execution reply=
  ok         refused, and refused by name
  ok         a refused fork answers nothing, so it did not resume the tip

  anchor system/permission_denied/- (late)
  [fork_system_permission_denied_-] session=287b773f result=error_during_execution reply=
  ok         refused, and refused by name
  ok         a refused fork answers nothing, so it did not resume the tip

  anchor user/-/text (late)
  [fork_user_-_text] session=5b0ba7b8 result=success reply=zephyr, otter
  ok         accepted, and the branch got its own session id
  ok         a late anchor keeps A
  ok         a late anchor keeps B — the address selects, it does not rewind

the matrix
    kind                               outcome   phase   knowsA knowsB
    system/hook_started/-              refused   early   no     no
    system/hook_response/-             refused   early   no     no
    system/init/-                      refused   early   no     no
    rate_limit_event/-/-               refused   early   no     no
    system/thinking_tokens/-           refused   early   no     no
    assistant/-/thinking               accepted  earlyA  yes    no
    assistant/-/tool_use               accepted  earlyA  yes    no
    user/-/tool_result                 accepted  earlyA  yes    no
    assistant/-/text                   accepted  earlyA  yes    no
    result/success/-                   refused   earlyA  no     no
    result/error_max_turns/-           refused   late    no     no
    system/status/-                    refused   late    no     no
    stream_event/-/-                   refused   late    no     no
    system/permission_denied/-         refused   late    no     no
    user/-/text                        accepted  late    yes    yes

Q1/Q2: is the accepted set 'assistant and user', and nothing else?
           accepted types: assistant user
           refused  types: rate_limit_event result stream_event system
  ok       every 'assistant' anchor was accepted
  ok       every 'user' anchor was accepted
  ok       a keyed block was refused — 'every keyed block is forkable' has a counter-example
  ok       acceptance is a function of the provider type alone, not of its subtype
  ok       the accepted set is exactly {assistant, user}

the trunk must be unchanged by every fork above
  [readTrunk] session=36942c80 result=success reply=zephyr, otter
  ok       resuming the trunk stays on the trunk id
  ok       the trunk still knows A
  ok       the trunk still knows B

total cost: $0.1757 over 21 runs
ALL PASS - only assistant and user lines are addressable; the rest are keyed and refused.
```

## The matrix, and what it maps to

Every one of the twelve `type`/`subtype` combinations §16's re-record confirmed
carries a `uuid`. Only six of the fifteen anchor kinds resolve.

| provider `type`/`subtype` | content block | carries a uuid | fork accepted | our event type |
|---|---|---|---|---|
| `assistant` | `text` | yes | **accepted** | `assistant.message` |
| `assistant` | `thinking` | yes | **accepted** | `assistant.thinking` |
| `assistant` | `tool_use` | yes | **accepted** | `tool.started` (or `workspace.report`) |
| `user` | `text` | yes | **accepted** | `user.message` |
| `user` | `tool_result` | yes | **accepted** | `tool.completed` |
| `system`/`init` | — | yes | refused | `run.started` + `turn.started` |
| `system`/`hook_started` | — | yes | refused | `raw.unclassified` |
| `system`/`hook_response` | — | yes | refused | `raw.unclassified` |
| `system`/`status` | — | yes | refused | dropped, transient |
| `system`/`thinking_tokens` | — | yes | refused | dropped, transient |
| `system`/`permission_denied` | — | yes | refused | `permission.resolved` |
| `result`/`success` | — | yes | refused | `turn.finished` |
| `result`/`error_max_turns` | — | yes | refused | `turn.finished` |
| `rate_limit_event` | — | yes | refused | `run.rate_limited` |
| `stream_event` | — | yes | refused | dropped, transient |

Read down the "carries a uuid" column: it is `yes` fifteen times out of fifteen.
That column is what the old rule was written against, and it is why the old rule
offered a toolbar everywhere.

**Acceptance is a function of the provider's `type` alone.** Both `result`
subtypes behaved identically and so did all four `system` ones, which is asserted
rather than eyeballed — the spike fails if any `type` splits across the two
outcomes. That is what lets the crate encode a list of *our* event types rather
than a `(type, subtype)` pair lookup: every arm of the match maps back to exactly
one provider `type`.

## What this buys, and what it cost

**Tool-bearing conversations fork.** The address spike's largest open item is
closed, and it closed cleanly: an `assistant` line whose content is a `tool_use`
block is accepted, and so is the `user` line carrying the matching
`tool_result` — a fork anchored *mid-tool-call* resumed and answered. Both
branches knew A and neither knew B, so the truncation is real rather than a tip
resume wearing a new session id.

**The replayed user prompt is addressable**, which matters more than it looks.
The driver always runs with `--replay-user-messages`, so a run's `user.message`
events are two different things — the user's own prompt echoed back, and the text
half of a tool result — and the surface offers a fork on both. Both resolve. The
address spike's finding that "the user's own prompt entries never reach stdout"
was true of a `-p` run and is **not** true of the configuration the product uses:
they reach stdout, they are keyed, and they fork.

**A refused fork is still loud, still free, and still not a tip resume.** Every
one of the nine refusals came back `error_during_execution` with
`No message found with message.uuid of: <uuid>` in `errors[]`, and none of them
produced an answer. That property is what made this a debt rather than a bug for
a day, and it held across nine different refusals rather than the one the address
spike bought it with.

The cost is a sentence in the design record. *Every keyed block is forkable* was
the careful phrasing the address spike settled on, and it is wrong. The true
statement is narrower: **a block is forkable when its event came off an
`assistant` or `user` line and it carries a key.** A key is necessary and not
sufficient.

## What was encoded

`normalizer::claude_code::is_addressable` is the rule, and
`fork_address(event_type, idempotency_key)` is the one function both callers go
through — `EventLog::fork_points` for the projection the toolbar is drawn from,
`driver::ForkPoint::resolve` for the address the fork is launched with. The
existing registry test that asserts the two agree still asserts it; the event-log
projection test now includes a keyed `raw.unclassified` (a `hook_started` line)
and a keyed `turn.finished`, and asserts neither is offered.

Two arms of the match are deliberate rather than measured, and both fail closed:

- **`raw.unclassified` is not addressable.** Every unclassified event this spike
  produced came from a `system` line and every one was refused. It can also come
  from an `assistant` or `user` line whose content block the normalizer could not
  read — a normalizer bug — and such a block would in principle resolve. Offering
  nothing there costs a fork point nobody can reach today; offering a toolbar
  that fails costs a user a gesture. The payload keeps `source_type` if this ever
  needs splitting.
- **An event type the table has not met is not addressable.** A vocabulary change
  shows up in `Health` (§16); it must not show up as a broken button.

## What this does not prove

- **Long transcripts and compaction.** Five turns. Unchanged from the address
  spike: nothing says a uuid observed in turn one is still resolvable after
  twenty more, in a file the provider owns and rewrites.
- **Subagent lines.** `parent_tool_use_id` was null on every line here, as it is
  in every fixture. Whether an `assistant` line emitted *inside* a subagent is a
  chain entry of the parent session is untested, and it is the one case where
  "came off an `assistant` line" might not be enough.
- **Concurrency.** Still. Every run was sequential, for the fifth record running.
- **The transient three.** `system/status`, `system/thinking_tokens` and
  `stream_event` were forked at and refused, so the rule is right about them —
  but they are dropped by `is_transient` before they are ever stored, so they can
  never reach a fork point anyway. The measurement is belt and braces rather than
  load-bearing.
- **Codex.** Unknown, as everything about Codex is. This table is Claude Code's
  and lives in Claude Code's module.

## Reproducing

```sh
bash docs/verification/fork-anchor-types-spike.sh     # ~5 min, ~$0.18
```

Raw streams are kept in the temp dir the script prints. It never runs `claude`
inside this repository, and it touches no worktree or log outside its own
`mktemp -d`.
