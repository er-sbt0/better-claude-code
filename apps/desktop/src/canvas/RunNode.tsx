import type { RawEvent } from '@agent-workspace/shared-types/RawEvent'
import type { Run } from '@agent-workspace/shared-types/Run'
import type { RunMetrics } from '@agent-workspace/shared-types/RunMetrics'
import { STATUS, elapsed, goalOf, money } from '../lib/format'
import { EventStream } from './EventStream'

// A node on the graph. §10: "a node collapses to its shape. Goal, tool-call
// count, files touched, findings, cost. Expanding is a deliberate act, not the
// default state."
//
// Collapsed is therefore the honest default here, and the header is the shape.
// What expands is today's flat event list, because §7's blocks do not exist
// yet — the frame is right even though its contents are still scaffolding.

// The node's size. It lives here rather than in `layout.ts` because it is a
// fact about what this component draws, and the layout pass only needs to be
// told. Expanding widens as well as lengthens: the body is a stream of
// `seq · type · summary` lines and the summary is the half worth reading, which
// a 272px node crushes to four characters.
const COLLAPSED = { w: 272, h: 74 }
const HEAD_H = 68
const ROW_H = 20
const BODY_MIN = 44
const BODY_MAX = 260

/**
 * How much room this node needs, given what is in it.
 *
 * The expanded height follows the event count rather than being a constant,
 * because a fixed body leaves a run with nine events sitting in three hundred
 * pixels of empty card — and on a surface whose whole claim is that a run
 * "collapses to its shape", a node that lies about its size is the wrong first
 * impression.
 */
export const sizeOf = (expanded: boolean, events: number) =>
  expanded
    ? { w: 468, h: HEAD_H + clamp(events * ROW_H + 4, BODY_MIN, BODY_MAX) }
    : COLLAPSED

const clamp = (n: number, lo: number, hi: number) => Math.max(lo, Math.min(hi, n))

export function RunNode({
  run,
  metrics,
  events,
  selected,
  expanded,
  onSelect,
  onToggle,
}: {
  run: Run
  metrics: RunMetrics | null
  events: RawEvent[]
  selected: boolean
  expanded: boolean
  onSelect: () => void
  onToggle: () => void
}) {
  const status = STATUS[run.status]

  return (
    <div
      onPointerDown={(e) => {
        // The canvas pans on a background drag; a drag that starts on a node
        // is a click on the node, and must not become a pan.
        e.stopPropagation()
        onSelect()
      }}
      onDoubleClick={onToggle}
      className={[
        'flex h-full w-full cursor-default flex-col overflow-hidden rounded-md border bg-panel',
        selected ? 'border-accent' : 'border-border hover:border-elevated',
      ].join(' ')}
    >
      <header className="flex shrink-0 items-center gap-2 px-2.5 pt-2">
        <i
          className={`size-1.5 shrink-0 rounded-full ${status.dot}`}
          aria-hidden="true"
          title={status.label}
        />
        <span className="truncate font-mono text-mono font-semibold">{run.id}</span>
        <span className="ml-auto shrink-0 font-mono text-micro text-faint">
          {elapsed(run.started_at, run.finished_at)}
        </span>
        <button
          type="button"
          onPointerDown={(e) => e.stopPropagation()}
          onClick={onToggle}
          title={expanded ? 'Collapse' : 'Expand'}
          className="-mr-1 flex size-4 shrink-0 items-center justify-center rounded-sm text-muted hover:bg-hover hover:text-text"
        >
          <svg viewBox="0 0 12 12" className="size-3" aria-hidden="true">
            <path
              d={expanded ? 'M2.5 7.5 6 4l3.5 3.5' : 'M2.5 4.5 6 8l3.5-3.5'}
              fill="none"
              stroke="currentColor"
              strokeWidth="1.2"
              strokeLinecap="round"
              strokeLinejoin="round"
            />
          </svg>
        </button>
      </header>

      <p className="shrink-0 truncate px-2.5 pt-1 text-micro text-text" title={run.prompt ?? ''}>
        {goalOf(run)}
      </p>

      <div className="flex shrink-0 items-center gap-2.5 px-2.5 py-1.5 font-mono text-micro text-muted">
        <span className={status.text}>{status.label}</span>
        {run.branch !== null && <span className="truncate">{run.branch}</span>}
        <span className="ml-auto shrink-0">{events.length}ev</span>
        {metrics !== null && (
          <>
            <span className="shrink-0">{metrics.tool_calls}t</span>
            <span className="shrink-0">{money(metrics.cost_usd)}</span>
          </>
        )}
      </div>

      {expanded && (
        <div className="min-h-0 flex-1 overflow-y-auto border-t border-border-soft">
          <EventStream events={events} />
        </div>
      )}
    </div>
  )
}
