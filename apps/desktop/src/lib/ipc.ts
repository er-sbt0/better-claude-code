import { invoke } from '@tauri-apps/api/core'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import type { BootReport } from '@agent-workspace/shared-types/BootReport'
import type { RawEvent } from '@agent-workspace/shared-types/RawEvent'
import type { Run } from '@agent-workspace/shared-types/Run'
import type { RunId } from '@agent-workspace/shared-types/RunId'
import type { StartRunRequest } from '@agent-workspace/shared-types/StartRunRequest'
import type { StartedRun } from '@agent-workspace/shared-types/StartedRun'

// The only place a Rust command name is spelled. Every payload type here is
// generated from the Rust struct (§22) — if one of these imports fails to
// resolve, the fix is `cargo test`, never a hand-written type.

export const bootReport = () => invoke<BootReport>('boot_report')

export const runningRuns = () => invoke<Run[]>('running_runs')

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
