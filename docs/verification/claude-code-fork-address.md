<!--
Verification record. Closes the debt the branching surface was gated on: whether
the `uuid` we already store off each stdout line is the identifier
`--resume-session-at` accepts. The session-fork spike read its uuid out of the
provider's transcript JSONL instead, and near-certainly is what §16's rule about
help text exists to refuse.

Verified: 2026-09-13 · Claude Code 2.1.270 · claude-sonnet-5 · Ubuntu, x86-64
Method: an executable spike, docs/verification/fork-address-spike.sh, which
        asserts rather than prints and exits non-zero on failure. Eight live
        runs in a throwaway git sandbox, $0.074 total.

Superseded in part, 2026-09-13, by claude-code-fork-anchor-types.md. Every
observation here stands; one generalisation drawn from them does not. "Every
keyed block is forkable" was inferred from a text-only trunk whose only keyed
message lines were `assistant` ones, and it is false: the provider keys all
twelve of its type/subtype combinations and refuses a fork at nine of the fifteen
kinds. The corrections are marked below where they occur, and nothing is deleted
— the wrong generalisation is the one that will be re-proposed.
-->

# The fork address — is the stdout `uuid` the thing the flag takes?

`docs/branching-surface.md` anchors a fork to a message, and the reason that
costs no schema change is that `normalizer/src/claude_code.rs:177` already lifts
the provider's `uuid` off each stdout `stream-json` line and stores it as
`raw_events.idempotency_key`. It was put there for resume deduplication (§14);
the design record then claimed it as the fork address.

Nothing had checked that claim. `--resume-session-at` takes a **chain-entry
uuid**, and the session-fork spike read the uuid it passed out of
`~/.claude/projects/<slug>/<session>.jsonl` — a different file, written by a
different code path, in a storage layout that spike already found can vary. If
the two were different namespaces, forking at a message would mean opening
provider files at fork time and the fork-anchor decision would have to reopen.

**They are the same identifier, and an assistant entry is accepted.** Every
assertion below passed.

## What was asked, and what came back

The trunk was built so that truncation is *observable* rather than asserted:
fact A ("codeword ZEPHYR") in turn one, fact B ("animal OTTER") in turn two of
one session, then a fork anchored between them. A fork that silently resumed
from the tip would know B and fail.

```
claude: 2.1.270 (Claude Code)

trunk: fact A in turn one, fact B in turn two, one session id we assign
  [t1] session=8c74adb3-… result=success reply=ok
  ok       turn one uses the session id we assigned
  [t2] session=8c74adb3-… result=success reply=ok
  ok       a plain resume stays on the trunk id
  [tTip] session=8c74adb3-… result=success reply=zephyr, otter
  ok       the trunk tip knows A
  ok       the trunk tip knows B

stdout: which lines carry a uuid
          3 assistant/-
          3 rate_limit_event/-
          3 result/success
          3 system/hook_response
          3 system/hook_started
          3 system/init
  ok       stdout carries an assistant uuid (93e6a054-0e51-4425-a90b-b46f7e28dc1c)
  ok       turn one and the last turn have different assistant uuids

transcript: locate it, with a bounded wait — the provider writes it async
  transcript: …/-tmp-aw-addr-MPI7vr-sandbox/8c74adb3-….jsonl (50 entries)
          3 assistant
         25 attachment
          3 system
          3 user

Q1: are the stream uuids and the transcript chain-entry uuids one set?
  ok       every message uuid on stdout is a transcript chain entry
  ok       every transcript assistant entry appeared on stdout
    stream message uuids: 3 · transcript uuids: 34 · transcript assistant uuids: 3
    transcript entries with no stdout line: 31
  ok       user chain entries of a text-only trunk never reach stdout

Q2: fork at the turn-one ASSISTANT uuid, taken off the stream
    it must know A and must NOT know B — a tip resume fails here
  [forkAsst] session=d8f4a20d-… result=success reply=zephyr
  ok       --resume-session-at accepted a stream-sourced assistant uuid
  ok       the assistant-anchored fork got its own session id
  ok       the branch kept the prefix: it knows A
  ok       the branch truncated: it does NOT know B

control: fork at the LAST assistant uuid — the address must select a point,
         not always rewind, so this branch must know both A and B
  [forkTip] session=28c22069-… result=success reply=zephyr, otter
  ok       the late fork got its own session id
  ok       the late branch knows A
  ok       the late branch knows B

fan-out: {uuid}#{i} is our key, not the provider's address
  [forkSuffix] session=b052e32d-… result=error_during_execution reply=
  ok       the suffixed form is refused, so the address is the part before the '#'
  ok       and it is refused by name, not silently
  ok       a refused fork produced no answer, so it did not resume the tip

no silent fallback: an address that resolves nowhere must be an error
  [forkBogus] session=f9bdee13-… result=error_during_execution reply=
  ok       an unknown uuid is refused rather than rounded to the nearest entry
  ok       the refused run answered nothing

the trunk must be unchanged by three forks
  [readTrunk] session=8c74adb3-… result=success reply=zephyr, otter
  ok       resuming the trunk stays on the trunk id
  ok       the trunk still knows A
  ok       the trunk still knows B

ALL PASS — the stdout uuid IS the fork address, assistant entries included.
```

## What was verified

| Claim | Result |
|---|---|
| **Q1** Every message uuid on stdout is a chain entry in the transcript | holds, as a set — not a lucky sample |
| **Q1** Every transcript `assistant` entry appeared on stdout under the same uuid | holds; the assistant namespace matches both ways |
| **Q1** A uuid taken **off the stream** is accepted by `--resume-session-at` | holds; the flag never saw the transcript |
| **Q2** An **assistant** entry is accepted as a branch point | holds; the old spike's user entry was not a special case |
| **Q2** The branch has the prefix and not the suffix | holds; it recalled A, did not recall B |
| The address *selects* a point rather than always rewinding | holds; anchoring at the later assistant uuid kept both facts |
| `{uuid}#{i}` is not passed verbatim | holds; refused by name, so the address is the part before the `#` |
| An unresolvable address is refused, not rounded | holds; `error_during_execution`, no answer produced |
| The trunk survives three forks | holds |

The control is the assertion that matters most for believing the rest. Two forks
of the same trunk differing only in which stream uuid they were anchored at
produced different context — one knew A alone, the other knew A and B. That
rules out both "the flag ignores the uuid" and "the flag always resumes the tip",
which are the two ways this spike could have passed for the wrong reason.

## What this buys

The fork anchor stands as decided. `raw_events.idempotency_key` **is** the fork
address, so "forkable" stays a computable property of a block we already store,
and nothing in the fork path has to open a file under `~/.claude/projects/`.
The dependency on a storage layout we do not own is gone from the product path —
the spike still reads the transcript, but only to prove the two namespaces are
one.

Two details the design record can now state as fact rather than inference:

1. **The `#i` suffix is ours alone.** `--resume-session-at "<uuid>#0"` fails with
   `No message found with message.uuid of: <uuid>#0`. Strip at the first `#`.
2. **A bad address is loud.** An unresolvable uuid produces a `result` event with
   `subtype: "error_during_execution"`, `is_error: true`, and the message in
   `errors[]` — not a silent resume from the tip. So the UI can offer a fork
   optimistically and surface a failure, rather than needing to pre-validate.
   Note the shape: a refused fork **still emits a `result` event with a fresh
   `session_id`**. Acceptance must be read off `subtype`, never off the presence
   of a session id. Anything in the driver that treats "we got a result" as "the
   fork worked" will be wrong.

## The finding that costs us something

**Not every chain entry is addressable from what we store, and the missing ones
are the user's.** On this text-only trunk the transcript held 34 uuids and
stdout carried 3 of them. The 31 that never reached stdout were the user's own
prompt entries, 25 `attachment` entries, and `system` entries. The reverse
direction — every stdout message uuid is a transcript entry — is exact; the
forward direction is not, and was never going to be.

This costs nothing today, because the surface forks from assistant messages and
those match one-for-one. It does mean the design's phrasing has to stay precise:
**every keyed block is forkable** is true; *every chain entry is keyed* is false.
A future surface that wanted to fork at "just before I asked that" would be
addressing an entry we have no key for, and would be back to reading provider
files.

> **Corrected.** Both halves of that are wrong, and
> `claude-code-fork-anchor-types.md` measured how. *Every keyed block is
> forkable* is false — a `system`/`hook_started` line carries a uuid, is keyed
> here, and the provider refuses a fork at it; only `assistant` and `user` lines
> resolve. And "just before I asked that" is reachable after all: the driver
> passes `--replay-user-messages`, under which the user's own prompt **does**
> reach stdout with a uuid of its own, and forking at it works. The paragraph
> above is true of a `-p` run, which is what this spike used and not what the
> product does.

Unchecked, and deliberately: whether a `user` line that *does* reach stdout —
a tool result — carries a uuid the flag accepts. The trunk here was text-only on
purpose, to keep the truncation signal clean. Tool-bearing runs are already a
separate debt (a fork of a run that edited files), and this belongs with it.

## What this does not prove

- ~~**Tool-bearing conversations.**~~ Closed by
  `claude-code-fork-anchor-types.md`: both halves of a tool call are addressable,
  and a fork anchored mid-tool-call resumes with the prefix and not the suffix.
- **Long transcripts.** Three assistant entries. Nothing here says the provider
  resolves an address in a conversation with hundreds.
- **Concurrency.** Still. Every run in this spike was sequential, same as the
  last one. Two branches of one trunk running at the same time remains the
  product case nobody has tested.
- **Codex.** Unknown, as everything about Codex is.
- **Stability of the stdout uuid across a resume.** Each turn's assistant uuid
  was distinct and each appeared in the transcript, but nothing checked that a
  uuid observed in turn one is still resolvable after twenty more turns, or that
  a compaction leaves it resolvable. The provider owns that file and rewrites it.

## Reproducing

```sh
bash docs/verification/fork-address-spike.sh     # ~2 min, ~$0.07
```

Raw streams are kept in the temp dir the script prints. It never runs `claude`
inside this repository.
