import { invoke } from '@tauri-apps/api/core'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import type { BootReport } from '@agent-workspace/shared-types/BootReport'
import type { CleanUpReport } from '@agent-workspace/shared-types/CleanUpReport'
import type { Feed } from '@agent-workspace/shared-types/Feed'
import type { RawEvent } from '@agent-workspace/shared-types/RawEvent'
import type { Run } from '@agent-workspace/shared-types/Run'
import type { RunId } from '@agent-workspace/shared-types/RunId'
import type { ForkPoints } from '@agent-workspace/shared-types/ForkPoints'
import type { ForkRunRequest } from '@agent-workspace/shared-types/ForkRunRequest'
import type { RunActivity } from '@agent-workspace/shared-types/RunActivity'
import type { RunMetrics } from '@agent-workspace/shared-types/RunMetrics'
import type { StartRunRequest } from '@agent-workspace/shared-types/StartRunRequest'
import type { StartedRun } from '@agent-workspace/shared-types/StartedRun'

// The only place a Rust command name is spelled. Every payload type here is
// generated from the Rust struct (§22) — if one of these imports fails to
// resolve, the fix is `cargo test`, never a hand-written type.

export const bootReport = () => invoke<BootReport>('boot_report')

export const runningRuns = () => invoke<Run[]>('running_runs')

// Every run the log has, newest first — finished and failed ones included,
// which is the difference that matters: the graph draws a branch *after* it
// finishes, and `runningRuns()` is the query that hides exactly those.
export const recentRuns = (limit: number) => invoke<Run[]>('recent_runs', { limit })

// One run's events out of the log — the unprocessed transcript (§7). The live
// channel below only carries runs this process is supervising, so a run from an
// earlier session has no stream at all until this is called.
//
// A node body does not call it: runFeed() below is what a column renders, and
// asking for both would send the same run across the boundary twice.
export const runEvents = (runId: RunId) => invoke<RawEvent[]>('run_events', { runId })

// §7's blocks for one run — the run feed itself.
//
// A projection, like runActivity() and forkPoints(), and the reason is the same
// and stronger: deciding that a `tool.started`, a `permission.resolved` and a
// `tool.completed` are *one block* is deciding. The webview holds the events
// already, so projecting here would cost no round trip — and would put the rule
// where no test in this workspace can reach it.
//
// The whole feed, every time, rather than a delta. That is what makes a column
// whose feed stopped while the camera was elsewhere correct when it comes back:
// it calls this again, and there is no missed delta to reconcile.
export const runFeed = (runId: RunId) => invoke<Feed>('run_feed', { runId })

// Cost, tokens and turns — a projection recomputed from `turn.finished` (§14),
// never a column on the run row.
export const runMetrics = (runId: RunId) => invoke<RunMetrics>('run_metrics', { runId })

// The three-word status and the message count, for the same window the graph
// draws. A projection like runMetrics(), and for the same reason it is a call
// rather than a field: "needs you" is a permission request with no resolution
// and a message count is not an event count, so both are derivations — and the
// shell owns no logic. Plural because the graph needs a header per node.
export const runActivity = (limit: number) =>
  invoke<RunActivity[]>('run_activity', { limit })

// Which blocks of these runs can be forked at, and what a fork there inherits.
//
// The hover toolbar appears on a block if and only if its `seq` is in this
// answer. `RawEvent.idempotency_key` is right here in the webview and deriving
// forkability from it would cost one line and no round trip — and it would be
// `normalizer::claude_code::fork_address`'s rule written a second time, in a
// language no test in this workspace reaches, drifting silently towards
// offering forks that `forkRun()` then refuses. So this is a call.
//
// By id rather than by window, unlike runActivity(): the answer is per block,
// so ask for the runs a fork can actually be anchored in — the one being read,
// the ones this session is supervising, and the parents of the children on
// screen.
export const forkPoints = (runIds: RunId[]) =>
  invoke<ForkPoints[]>('fork_points', { runIds })

// ⌘⇧F: a new run, branched from a block of an existing one (§19). Everything
// that makes it a fork happens below the window — the address is resolved from
// the parent's log, the child's worktree is checked out from the parent's
// shadow commit at that `seq`, and both halves of the graph edge are written to
// the child's row.
//
// An unaddressable block is refused here rather than rounded to a neighbour,
// and the refusal costs no run: it happens before a process exists.
export const forkRun = (request: ForkRunRequest) =>
  invoke<StartedRun>('fork_run', { request })

// Runs this process is supervising, which is not the same question as
// runningRuns(): that one asks the log about rows, this one asks about
// processes.
export const liveRuns = () => invoke<RunId[]>('live_runs')

export const startRun = (request: StartRunRequest) =>
  invoke<StartedRun>('start_run', { request })

export const sendTurn = (runId: RunId, text: string) =>
  invoke<void>('send_turn', { runId, text })

// The only graceful end: nothing closes a run's stdin on its behalf, so a run
// started here holds a live agent process until this is called (or the app
// exits).
export const endRun = (runId: RunId) => invoke<void>('end_run', { runId })

export const interruptRun = (runId: RunId) => invoke<void>('interrupt_run', { runId })

// Take a finished run's worktree and its `agent/<run_id>` branch away again
// (§13). The only call in this file that destroys anything, and nothing calls
// it but a person: no timer, no eviction, no sweep at exit — so until it is
// pressed every run keeps a tree and a branch inside the user's own repository.
//
// The shadow ref `refs/agent-workspace/runs/<id>` survives it, deliberately and
// load-bearingly: a cleaned-up run is still forkable and Review still has its
// attribution (§13.1).
//
// `force` is one specific confirmation and not a general override: a tree with
// uncommitted changes comes back as `{ outcome: 'dirty' }` rather than a
// rejection, because §13 says the answer there is to ask. A live run is refused
// outright and no `force` lifts it — end or interrupt it first.
export const cleanUpRun = (runId: RunId, force: boolean) =>
  invoke<CleanUpReport>('clean_up_run', { runId, force })

// Every live run's events, on one channel for all of them, in stream order.
//
// The topic is spelled `RUN_EVENT` in `src-tauri/src/lib.rs` and that doc
// comment carries the reasoning; a test there reads this file and fails if the
// two drift, because a renamed channel is otherwise a live view that simply
// never updates. The payload is a `RawEvent` verbatim, so the caller filters on
// `run_id` — there is no per-run channel to subscribe to.
//
// Subscribe once, on mount, before anything can start a run. `listen` is itself
// an IPC round trip: a subscription registered after `startRun()` resolves has
// already lost the beginning of that run's stream, and nothing anywhere
// reports the loss.
export const RUN_EVENT = 'run:event'

export const onRunEvent = (
  handler: (event: RawEvent) => void,
): Promise<UnlistenFn> => listen<RawEvent>(RUN_EVENT, (e) => handler(e.payload))
