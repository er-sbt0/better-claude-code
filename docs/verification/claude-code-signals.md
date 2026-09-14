<!--
Verification record. Establishes how Claude Code leaves when it is signalled,
which the driver had never measured: `Session::interrupt` sent SIGKILL because
`std::process::Child::kill` does, and that choice was load-bearing for two
things at once -- the session transcript `--resume` reads, and the exit status
`Registry::finish` turns into a row.

Verified: 2026-09-14 · Claude Code 2.1.270 · claude-haiku-4-5-20251001 · Ubuntu 24.04, x86-64
Method: docs/verification/signals-spike.sh, which asserts rather than prints.
        Six live runs in a throwaway git sandbox, under a cent.
-->

# Stopping a run — what each signal actually does

| Signal | Time to exit | Exit status | `ExitStatus::code()` | `Registry::finish` would file it |
|---|---|---|---|---|
| `SIGTERM` | ~0.55s | killed by signal 15 | `None` | `Interrupted` |
| `SIGKILL` | ~0.01s | killed by signal 9 | `None` | `Interrupted` |
| `SIGINT` | ~0.56s | **exited 0** | `Some(0)` | **`Finished`** |

Three facts, and the third is the one that would have been shipped by reasoning
instead of measuring.

## `SIGTERM` is handled, and then re-raised

It takes ~0.55s rather than dying instantly, so there is a handler. It still
reports as *killed by signal 15* rather than exiting with a code, so the handler
ends by restoring the default disposition and re-raising — the POSIX-correct
shape, and the one that matters to us: **an interrupted run still has no exit
code**, which is exactly what `Registry::finish` reads to file it as
`Interrupted` rather than `Finished` or `Failed`. Switching the driver from
`SIGKILL` to `SIGTERM` therefore changes nothing about the row, which is why it
could be done without touching that derivation.

0.55s is also the number behind `driver::process::GRACE`. Two seconds is that,
tripled, and it is the only thing app exit waits for.

## `SIGINT` is the trap

It is the signal a person sends with Ctrl-C, it is handled the same way — and
the process **exits 0**. A driver that reached for it as the polite option would
have filed every run the user stopped, and every run open at app exit, as
`Finished`: a run that was killed mid-turn claiming in the runs list to have
completed its work. Nothing in the stream says otherwise either; the `result`
line for the interrupted turn never arrives.

This is why `examples/run.rs` forwards Ctrl-C to the agent as `SIGTERM` and not
as the `SIGINT` it received.

## The transcript

The provider owns `~/.claude/projects/<cwd>/<session-id>.jsonl`, and everything
this workspace does with `--resume` and `--fork-session` depends on it. Both
signalled sessions resumed cleanly afterwards and returned the codeword they had
been given, so **the file is written as the conversation goes, not at exit**, the
same property our own event log has. `SIGKILL` does not corrupt it.

**What `SIGKILL` can lose is the tail.** In one run the assistant half of a
turn that had already produced its `result` line on stdout was still missing
from the transcript when `SIGKILL` landed, and never arrived; a `SIGTERM` at the
same point in another run wrote it during the ~0.55s it spends leaving. So the
transcript write lags the stream by a window of tens of milliseconds, and
`SIGKILL` inside that window drops whatever had not been written yet.

Stated plainly because the spike does not assert it: it is timing-dependent, it
did not reproduce on every attempt, and a cheap spike cannot make it
deterministic. It is a real window, not a certainty, and it is enough of a
reason to ask first and kill second — the ask costs half a second and the loss
is silent.

## What was not measured here

**Whether `SIGTERM` reaches the CLI's own children.** It does by construction —
the driver signals the process *group*, and a child inherits its parent's group
— and `crates/driver/tests/registry.rs` pins it against a stub that spawns a
long-lived grandchild. Nothing here established what `claude` itself does to
`workspace-mcp-relay` or to a `Bash` tool call when it is asked to stop, and the
driver deliberately does not depend on that: the group signal reaches them
whether the CLI forwards anything or not.

§16's rule applies. None of this is documented by the provider, all of it can
change without a semver bump, and the version above is the one it was true for.
