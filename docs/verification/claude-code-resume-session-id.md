<!--
Verification record. Establishes what argv a *plain* resume may take. Its own
record rather than a paragraph in claude-code-session-forking.md, because that
one answers "can a conversation become a graph" and this one answers "does the
thing we shipped run at all" -- and the answer is no. A defect in our own argv
buried under a capability record is a defect nobody finds twice.

Verified: 2026-09-14 · Claude Code 2.1.270 · claude-haiku-4-5-20251001 · Ubuntu, x86-64
Method: docs/verification/resume-session-id-spike.sh, which asserts rather than
        prints. Six live runs in a throwaway git sandbox, under a cent.
-->

# Resuming a session — `--session-id` is not allowed to come along

Every resumed run was dead on arrival.

`LaunchSpec::argv` pushed `--session-id` unconditionally, and `driver::resume`
set `resume` to the same id the caller had put in `session_id`
(`apps/desktop/src-tauri/src/lib.rs::send_turn`). So a plain resume — the path
behind "a conversation outlives the window that started it", commit `e7821d2` —
produced argv containing both, with one uuid:

```
--session-id <X> --resume <X>
```

The provider refuses it by name:

```
Error: --session-id can only be used with --continue or --resume if --fork-session is also specified.
```

Nothing had checked this. `claude-code-session-forking.md` verified
`--session-id` on a **fresh** session and `--session-id` composed with
`--fork-session`; the combination that was actually shipping — an existing
session id on a resume that is *not* a fork — appears in no record and in no
fixture. The only test covering the path runs against a stub agent binary, which
accepts whatever it is handed and therefore could never have caught this.

## What was verified

| Claim | Result |
|---|---|
| `--session-id X --resume X` — the argv the driver built | **refused**; exit 1, stderr as quoted above |
| that refusal is legible before any turn is spent | holds — but see below, it is not the shape we look for |
| `--resume X` alone, the fix | holds; `subtype: "success"` |
| the resumed session keeps `X` as its id | holds; `system/init` and `result` both report `X` |
| it is the same conversation, not a new one | holds; it returned the trunk's codeword |
| `--session-id X` on a **fresh** launch, `X` already existing | refused, `Error: Session ID <X> is already in use.` |

The last row is why the fix is "suppress on a plain resume" rather than
"suppress whenever the id already exists": those are two different refusals. One
is about composing the flag with `--resume`, the other about the flag's own
uniqueness. A fork trips neither — it resumes the parent and names a **new**
child id, which is the combination `claude-code-session-forking.md` verified and
which still works.

## The finding that costs us something

**This refusal does not look like the last one.** `claude-code-fork-address.md`
taught us to read acceptance off the `result` line's `subtype`, because a
refused fork address arrives as a normally-exiting process carrying
`error_during_execution` and even a fresh `session_id`. This one is the
opposite: the CLI rejects the flag combination during argument parsing, so it
exits 1 having written **nothing** to stdout — no `system/init`, no `result`, no
subtype to read. Code that checks only `subtype` sees nothing at all, and code
that checks only the exit code would have caught this one and missed the fork
one. Both checks are needed, and stderr is the only place either failure names
itself.

The knock-on for us is that a refused launch produces a run with no events. The
normalizer has nothing to classify and the run ends on the exit code alone —
which is the path `driver::resume`'s callers were silently taking on every
single resume.

## The rule, as implemented

`LaunchSpec::argv` emits `--session-id` unless this is a resume that is not a
fork. Stated as the provider states it: the flag is legal on a fresh start, and
legal alongside `--resume` only with `--fork-session`.

Dropping it is lossless — the resumed session comes back under `X` regardless,
so there was never anything to assign — but only while the dropped id *is* `X`.
`LaunchSpec::validate` therefore refuses a non-forking resume whose `session_id`
names some other session: that is a caller asking to rename a session on resume,
which the provider does not offer in any spelling, and letting the suppression
swallow it would trade a refused launch for a run wearing an id nobody chose.

Both shapes are pinned by
`crates/driver/tests/driver.rs::a_plain_resume_drops_the_session_id_a_fork_keeps_it`,
which asserts the fork half too so the fix cannot later be generalised into
"drop it whenever resuming".

## What this does not prove

- **That nothing else in the launch is wrong on resume.** The spike ran the
  driver's argv minus the workspace MCP flags, because there is no server to
  point them at outside the app. `--mcp-config`, `--permission-prompt-tool` and
  the appended system prompt are verified elsewhere but not verified *on a
  resume*, and §6's snapshot trap lives exactly there.
- **`--continue`.** Named in the same error message and never used by us.
- **Codex.** Unknown, as ever.

## Reproducing

```sh
bash docs/verification/resume-session-id-spike.sh    # ~1 min, under a cent
```

It never runs `claude` inside this repository.
