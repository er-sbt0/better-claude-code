import type { Activity } from '@agent-workspace/shared-types/Activity'
import type { Run } from '@agent-workspace/shared-types/Run'
import type { RunStatus } from '@agent-workspace/shared-types/RunStatus'

// Presentation only. Nothing here asks the backend anything or decides what a
// run *is* — `RunStatus` comes off the row and `Activity` off a projection the
// `event-log` crate computes, and the job of this file is to say either one in
// a few characters and one colour.

/** What the *row* says a run is, in five words. Not the graph's vocabulary. */
export const STATUS: Record<RunStatus, { label: string; dot: string; text: string }> = {
  running: { label: 'running', dot: 'bg-accent', text: 'text-accent' },
  finished: { label: 'finished', dot: 'bg-ok', text: 'text-ok' },
  failed: { label: 'failed', dot: 'bg-err', text: 'text-err' },
  // §8: a run whose process died with the last session. Not an error — nobody
  // did anything wrong — so it is stated in grey rather than red.
  abandoned: { label: 'abandoned', dot: 'bg-faint', text: 'text-muted' },
  // §8: the user stopped it, or the app exited under it — and since nothing
  // closes a run's stdin for it, that is how most runs end. Grey for the same
  // reason as `abandoned`, and emphatically not red: this used to be spelled
  // `failed`, which accused the agent of something the user had done.
  interrupted: { label: 'interrupted', dot: 'bg-faint', text: 'text-muted' },
}

/**
 * The graph's vocabulary: three words, and the only place one maps to a colour.
 *
 * The keys come from `Activity`, which is **generated** — the collapse from
 * five run states to three words happens in `event-log`, not here, because
 * deriving **needs you** (a `permission.requested` with no matching
 * `permission.resolved`) is deciding, and the shell owns no logic. What is left
 * on this side is the part that genuinely is presentation: which colour.
 *
 * `STATUS` above survives alongside it and is not a duplicate. These answer
 * different questions — *does this run want me now* against *what became of
 * it* — and the second is why `failed` can collapse into `idle` here without
 * the information going anywhere: the inspector, the runs list and the status
 * bar still spell every one of them.
 */
export const ACTIVITY: Record<
  Activity,
  { label: string; dot: string; text: string; border: string }
> = {
  idle: { label: 'idle', dot: 'bg-faint', text: 'text-muted', border: 'border-border' },
  running: {
    label: 'running',
    dot: 'bg-accent',
    text: 'text-accent',
    border: 'border-accent/40',
  },
  // The record propagates this one to border, pill, sliver and card: a blocked
  // run is the only thing on the canvas that is waiting for a person.
  needs_you: {
    label: 'needs you',
    dot: 'bg-warn',
    text: 'text-warn',
    border: 'border-warn/60',
  },
}

/**
 * What to call a run in a node header.
 *
 * There is no `name` column and this does not invent one. A stored name is a
 * product decision nobody has made, so the header uses the identity the run
 * already has: its branch, which is what the work is called everywhere else in
 * git, falling back to the run id for a row that has none.
 */
export const nameOf = (run: Run): string => run.branch ?? run.id

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

/** Long text on one line, for a header or a card. */
export const clip = (text: string, max: number): string => {
  const line = text.replace(/\s+/gu, ' ').trim()
  return line.length <= max ? line : `${line.slice(0, max - 1)}…`
}

/** What a run is *about*, for a node header. Runs may carry no prompt. */
export const goalOf = (run: Run): string =>
  run.prompt === null || run.prompt.trim() === '' ? 'no prompt recorded' : clip(run.prompt, 120)
