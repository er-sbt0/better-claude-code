import { invoke } from '@tauri-apps/api/core'
import type { BootReport } from '@agent-workspace/shared-types/BootReport'
import type { Run } from '@agent-workspace/shared-types/Run'

// The only place a Rust command name is spelled. Every payload type here is
// generated from the Rust struct (§22) — if one of these imports fails to
// resolve, the fix is `cargo test`, never a hand-written type.

export const bootReport = () => invoke<BootReport>('boot_report')

export const runningRuns = () => invoke<Run[]>('running_runs')
