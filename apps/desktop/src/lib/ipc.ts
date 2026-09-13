import { invoke } from '@tauri-apps/api/core'
import type { BootReport } from '@agent-workspace/shared-types/BootReport'
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
