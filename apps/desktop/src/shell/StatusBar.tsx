import type { Run } from '@agent-workspace/shared-types/Run'
import type { RunMetrics } from '@agent-workspace/shared-types/RunMetrics'
import { STATUS, elapsed, money } from '../lib/format'

// The bottom row. Everything here is about the run in the middle of the screen
// except the two right-hand items, which are about the app itself.

export function StatusBar({
  run,
  metrics,
  blocks,
  live,
  logPath,
}: {
  run: Run | null
  metrics: RunMetrics | null
  /** §7's block count for the selected run, or `null` while its feed is unfetched. */
  blocks: number | null
  live: boolean
  logPath: string | null
}) {
  const status = run === null ? null : STATUS[run.status]

  return (
    <footer className="flex h-6 shrink-0 items-center gap-3 border-t border-border bg-panel px-3 text-micro text-muted">
      {run === null || status === null ? (
        <span className="text-faint">No run selected</span>
      ) : (
        <>
          <span className="flex items-center gap-1.5">
            <i className={`size-1.5 rounded-full ${status.dot}`} aria-hidden="true" />
            <span className={status.text}>{status.label}</span>
          </span>
          <span className="font-mono">{elapsed(run.started_at, run.finished_at)}</span>
          {metrics !== null && (
            <>
              <span className="font-mono">{money(metrics.cost_usd)}</span>
              <span className="font-mono">
                {metrics.turns} {metrics.turns === 1 ? 'turn' : 'turns'}
              </span>
              <span className="font-mono">{metrics.tool_calls} tools</span>
            </>
          )}
          {/* Blocks, not events: a run with 200 events does not have 200
              blocks — a tool call is three events and one block — and this row
              is about what is on the screen beside it. */}
          <span className="font-mono">{blocks === null ? '— blocks' : `${blocks} blocks`}</span>
        </>
      )}

      <span className="ml-auto flex items-center gap-3">
        {/* Whether the event subscription is actually live. A run started
            before it is would lose its first events silently, so this is not a
            decoration — it is the one thing the user cannot otherwise see. */}
        <span className="flex items-center gap-1.5" title="Live event subscription">
          <i
            className={`size-1.5 rounded-full ${live ? 'bg-ok' : 'bg-warn'}`}
            aria-hidden="true"
          />
          {live ? 'listening' : 'connecting'}
        </span>
        {logPath !== null && (
          <span className="max-w-[22rem] truncate font-mono text-faint" title={logPath}>
            {logPath}
          </span>
        )}
        {/* §4 develops against WebKitGTK, the weakest of the three engines.
            Which one is actually rendering should never be a guess. */}
        <span className="font-mono text-faint" title={navigator.userAgent}>
          {engine()}
        </span>
      </span>
    </footer>
  )
}

/** The rendering engine, short enough for a status bar. */
function engine(): string {
  const ua = navigator.userAgent
  const webkit = /AppleWebKit\/([\d.]+)/u.exec(ua)
  if (/Chrome\/([\d.]+)/u.test(ua)) return `Chromium ${/Chrome\/([\d.]+)/u.exec(ua)?.[1] ?? ''}`
  if (webkit !== null) return `WebKit ${webkit[1] ?? ''}`
  return 'unknown engine'
}
