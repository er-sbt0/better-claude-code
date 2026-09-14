<!--
Measurement record. Answers "should an idle live run be closed and resumed on
demand?" with numbers rather than argument, and the answer it produced is no --
recorded in docs/roadmap.md §8. Written as a record rather than a roadmap
paragraph because the numbers are what would change the answer, and they have to
be re-measurable when they do.

Measured: 2026-09-14 · Claude Code 2.1.270 · node v22.23.1 · claude-haiku-4-5-20251001
Machine:  Ubuntu, Linux 6.14, x86-64, 12 cores, 14 GB RAM (~5 GB available at
          the time of measurement), ext4 on NVMe
Harness:  docs/verification/idle-run-cost-spike.py (live runs, ~15 haiku turns,
          well under a cent) and crates/event-log/examples/connection_cost.rs.
          The sandbox is on ext4 under ~/.cache, not in /tmp, which is tmpfs
          here -- the trap append_cost.rs records having fallen into once.
-->

# What an idle live run costs, and what waking one costs instead

`driver::Registry` holds a `claude` process for the life of a conversation.
Nothing closes stdin on a run's behalf, deliberately: a run is a conversation
and the user decides when it is over. But the home screen is a canvas of
conversations (`docs/branching-surface.md`), so **the number of held processes
is the number of columns on screen**, and since `f5ba282` the cheaper shape
actually works — `send_turn` resumes a run that is not live and sends the turn
as the prompt that wakes it, keeping the id, the row, the worktree and the
shadow history.

So the question is whether an idle run should be closed on a timer and woken on
demand. Three things had to be priced first.

## A. What an idle live run costs

One `claude` per run, one trivial turn in, then sitting on stdin. RSS is the
dishonest number for a Node process — the runtime's mapped pages are shared
between every one of them — so PSS and USS are here too, and the fifth process
is measured rather than extrapolated from the first.

| | RSS | PSS | USS (private) | shared | fds | threads | children |
|---|---:|---:|---:|---:|---:|---:|---:|
| one idle run | 235.3 M | 147.8 M | 133.4 M | 101.9 M | 23 | 18 | 0 |
| five idle runs, each | ~233.3 M | ~139.5 M | ~130.2 M | ~103.5 M | 23 | 17–18 | 0 |
| five, total | 1168.6 M | 697.3 M | 651.1 M | | | | |

**The marginal run costs what the first one did: 233 M RSS, 137 M PSS, ~130 M
private.** Sharing is real but bounded — the five processes between them share
~100 M of Node's mapped pages, so summing RSS double-counts 471 M and summing
PSS does not. The number to plan against is the private one: **~130 MB per idle
conversation, flat.**

For scale, `webkitgtk-feed-budget.md` measured the UI process at 352 MB RSS with
one column mounted, and drove the canvas to twelve. Twelve live conversations
would be ~1.6 GB private on top of that, on a 14 GB machine with 5 GB actually
available. This is not nothing.

It is also not the whole story, and the part that is missing is stated in "What
was not measured" below: every process here had held exactly one trivial turn.

### The other two things a live run holds are not costs

The registry's module doc listed three: the process, the worktree and a second
connection to the event log. Measured, only the first is a cost.

`crates/event-log/examples/connection_cost.rs`, sixteen connections to one
database, each having written and read:

| connections | RSS | per connection | fds | per connection |
|---:|---:|---:|---:|---:|
| baseline (1) | 4796 kB | | 7 | |
| 1 | 4848 kB | 52 kB | 9 | 2 |
| 4 | 5148 kB | 88 kB | 15 | 2 |
| 8 | 5548 kB | 94 kB | 23 | 2 |
| 16 | 6352 kB | **97 kB** | 39 | **2** |

**97 kB and two descriptors per run.** Against 130 MB of Node that is three
orders of magnitude down; a thousand live runs would spend 97 MB on connections
and 130 GB on processes. §14's single-writer-per-connection rule costs nothing
worth counting.

One thing the fd column looks like and is not: after dropping all sixteen
connections the process still held sixteen descriptors on the database file.
They are not leaked — reopening sixteen connections took the count straight back
to 39 rather than to 55. SQLite's unix VFS parks descriptors on the inode
instead of calling `close(2)` while the process holds locks on the file, because
POSIX advisory locks are dropped by *any* close on the same inode, and it reuses
them on the next open. A run that ends and a run that starts trade the same
descriptor.

And the worktree is on disk, is shared with the shadow history and Review, and
**survives the run either way** — closing stdin does not reclaim it. Ending an
idle run buys exactly one thing: the process.

## B. What waking one costs

Five turns into an already-live process, then five resumes of a closed session,
same sandbox, same model, same trivial prompt.

| | n | min | median | mean | max |
|---|---:|---:|---:|---:|---:|
| live: prompt → first event | 5 | 14 ms | **24 ms** | 22 ms | 30 ms |
| live: prompt → first text | 5 | 861 ms | **891 ms** | 889 ms | 912 ms |
| live: prompt → result | 5 | 892 ms | 939 ms | 940 ms | 992 ms |
| resume: spawn → first event | 5 | 621 ms | **658 ms** | 694 ms | 854 ms |
| resume: spawn → first text | 5 | 2076 ms | **2176 ms** | 2493 ms | 3582 ms |
| resume: spawn → result | 5 | 2122 ms | 2283 ms | 2551 ms | 3599 ms |
| close: stdin close → exit | 5 | 565 ms | 615 ms | 595 ms | 615 ms |

**A woken conversation answers ~1.3 s later than a live one, and shows nothing
at all for the first 0.63 s.** On a trivial haiku turn that is 891 ms becoming
2176 ms — the wait before the first token roughly *triples*. The extra is not
model time: it is Node starting, the CLI loading its config, and the session
transcript being read back.

Two things inflate the 658 ms in a way worth naming. The user's own
configuration is inherited by every run (`118ca34`,
`claude-code-user-config-inheritance.md`), so **every wake re-runs the user's
`SessionStart` hooks** — they are visible as `hook_started`/`hook_response` at
the head of every resumed stream. A user whose hook shells out to something slow
pays for it on every turn of an evicted conversation, and we would have made
that happen without asking. The other is that this machine's transcripts were
small; a long conversation is more to read back, and that was not measured.

The 615 ms close is off the user's path — the supervisor reaps in the
background — so it is not part of the trade, but it does mean eviction is not
instantaneous either.

## C. Repeated close/resume cycles are lossless

Three close/resume cycles on one session, each asking the agent to recall a
codeword it was given before the first close.

| | cycle 0 | cycle 1 | cycle 2 |
|---|---|---|---|
| `system/init` session id | unchanged | unchanged | unchanged |
| `result` subtype | success | success | success |
| codeword recalled | yes | yes | yes |
| exit code | 0 | 0 | 0 |
| provider transcript | 197,943 B | +17,448 B | +17,336 B |

The worktree was untouched and `git status` clean throughout, and the id the
provider reports is the one we assigned (§14) in every init and every result.

**A plain resume replays nothing.** This is the finding that matters to the log,
and it is the opposite of what the code was braced for. A resumed stream is 16
lines and every one of them belongs to the new turn:

```
system/hook_started, system/hook_response, system/init, system/thinking_tokens ×8,
user (the replayed prompt), assistant (thinking), assistant (text),
rate_limit_event, result/success
```

Not one uuid from the earlier life of the session appears. So the log continues
at `last_seq + 1` and `Pipeline::resuming`'s duplicate-detection path never fires
on this route — `seq` continuity comes entirely from seeding the normalizer out
of the log, which is exactly what `Pipeline::resuming` does and what
`resuming_continues_the_sequence_and_deduplicates_the_replay` pins. Keep the
duplicate path: it is the cheap insurance, it costs one index lookup, and the
provider has changed the shape of a stream before (§16). But do not believe a
resume is expensive because it replays; in 2.1.270 it does not.

**The one thing that was not lossless was ours.** `driver::resume` left the run
row exactly as `Registry::finish` had filed it — `finished`, with the
`finished_at` of the moment it was put down — so a woken run was live, talking,
and reading as over everywhere the row is shown, and `running_runs` could not see
it to reconcile if the app died under it. Fixed in the same commit as this
record: `EventLog::reopen_run` puts the row back to `running` and clears
`finished_at`, called from `driver::resume` at the same point in the sequence
where the fresh path calls `create_run`. Tests:
`a_woken_run_is_running_again_rather_than_still_finished` (driver) and
`a_run_that_is_resumed_is_running_again_with_no_finish_time` (event-log).

## What was not measured, and why

- **An idle run holding a *real* conversation.** Every process in table A had
  taken one trivial turn. A conversation with a hundred turns of context holds
  more, and the 130 MB is therefore a **floor**, not a range. The experiment is
  the same script with a longer warm-up; it was not run because it costs real
  turns and does not change the direction of the answer.
- **Resume latency against a large transcript.** Same reason, opposite
  direction: it can only make the resume look worse.
- **Drift over hours.** Whether an idle `claude` grows while doing nothing is
  the one measurement that could overturn the decision below, and it takes hours
  of wall clock. Recorded as a verification debt instead (§7).
- **Anything but haiku, and anything but this machine.** Model choice moves the
  live number (the 891 ms) far more than the resume number, so a slower model
  makes the +1.3 s a smaller *fraction* of the wait while leaving it the same
  number of milliseconds.
- **Contention.** The resumes in table B ran with no other live runs. Waking a
  run on a machine already holding five is plausibly slower.

## The decision

Recorded in `docs/roadmap.md` §8: **no idle-eviction policy.** The memory is
real but the price of reclaiming it automatically is paid in the one currency
the canvas cannot spend — latency on the next thing the user types — and it
would silently re-run their hooks and file a conversation that is not over as
`Finished`. The lever already exists and is already the user's: ending an idle
run and typing into it again is a lossless round trip, now including the row's
own status.
