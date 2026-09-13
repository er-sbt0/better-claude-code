import type { Run } from '@agent-workspace/shared-types/Run'
import type { RunStatus } from '@agent-workspace/shared-types/RunStatus'

// Presentation only. Nothing here asks the backend anything or decides what a
// run *is* — `RunStatus` comes off the row, and the job of this file is to say
// it in four characters and one colour.

/** The status vocabulary, and the only place a status maps to a colour. */
export const STATUS: Record<RunStatus, { label: string; dot: string; text: string }> = {
  running: { label: 'running', dot: 'bg-accent', text: 'text-accent' },
  finished: { label: 'finished', dot: 'bg-ok', text: 'text-ok' },
  failed: { label: 'failed', dot: 'bg-err', text: 'text-err' },
  // §8: a run whose process died with the last session. Not an error — nobody
  // did anything wrong — so it is stated in grey rather than red.
  abandoned: { label: 'abandoned', dot: 'bg-faint', text: 'text-muted' },
}

/** The last path segment, which is what a workspace is called out loud. */
export const basename = (path: string): string =>
  path.replace(/\/+$/u, '').split('/').pop() ?? path

/** `8m42s`, `2h04m`, `12s` — the status bar's unit, never milliseconds. */
export function elapsed(fromIso: string, toIso: string | null): string {
  const from = Date.parse(fromIso)
  const to = toIso === null ? Date.now() : Date.parse(toIso)
  if (Number.isNaN(from) || Number.isNaN(to)) return '—'
  const s = Math.max(0, Math.round((to - from) / 1000))
  if (s < 60) return `${s}s`
  const m = Math.floor(s / 60)
  if (m < 60) return `${m}m${String(s % 60).padStart(2, '0')}s`
  return `${Math.floor(m / 60)}h${String(m % 60).padStart(2, '0')}m`
}

/** Four decimals, because a run that cost $0.0004 did not cost $0.00. */
export const money = (usd: number): string => `$${usd.toFixed(usd < 0.01 ? 4 : 2)}`

/** Long text on one line — the same clip `describe.ts` applies to events. */
export const clip = (text: string, max: number): string => {
  const line = text.replace(/\s+/gu, ' ').trim()
  return line.length <= max ? line : `${line.slice(0, max - 1)}…`
}

/** What a run is *about*, for a node header. Runs may carry no prompt. */
export const goalOf = (run: Run): string =>
  run.prompt === null || run.prompt.trim() === '' ? 'no prompt recorded' : clip(run.prompt, 120)
