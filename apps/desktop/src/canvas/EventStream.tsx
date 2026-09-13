import type { RawEvent } from '@agent-workspace/shared-types/RawEvent'
import { describe } from '../lib/describe'

// ---------------------------------------------------------------------------
// THIS IS NOT §7'S RUN FEED. It is scaffolding that proves the bridge.
//
// It moved here from `App.tsx` when the shell was built, and it moved
// unchanged: a flat list of one line per raw event, the same thing
// `crates/driver/examples/run.rs` prints to a terminal. The surrounding
// furniture is new; this is not a step towards the feed and must not be read
// as one.
//
// The feed (§7, and "not started" in the roadmap's V1 table) is six block
// types plus `Raw`, virtualized, every block carrying `(runId, seqRange)`, with
// collapse/expand first-class — because §10's run graph is built out of those
// blocks and the roadmap's thesis is the graph.
//
// So: no block types, no virtualization, no collapse, and no `seqRange`
// addressing here. Do not extend this into the feed — replace it. The node it
// now sits inside is the frame it will be replaced within.
// ---------------------------------------------------------------------------

export function EventStream({ events }: { events: RawEvent[] }) {
  if (events.length === 0) {
    return (
      <p className="px-3 py-2 text-micro text-faint">
        Nothing yet. The first event is `run.started`, once the agent process
        has said hello.
      </p>
    )
  }

  return (
    <ol className="select-text font-mono text-mono">
      {events.map((event) => {
        const { type, summary } = describe(event)
        return (
          <li
            key={event.seq}
            className="flex gap-2 px-2 py-px whitespace-nowrap odd:bg-black/15"
          >
            <span className="w-8 shrink-0 text-right text-faint">{event.seq}</span>
            <span className={`w-36 shrink-0 ${tone(type)}`}>{type}</span>
            <span className="min-w-0 flex-1 truncate text-muted">{summary}</span>
          </li>
        )
      })}
    </ol>
  )
}

/**
 * One colour per event family, from the theme's status vocabulary.
 *
 * This is the whole of the semantics available at this layer: the line is a
 * string, so the only thing that can be typed is which *kind* of string it is.
 */
function tone(type: string): string {
  if (type.startsWith('run.')) return type === 'run.failed' ? 'text-err' : 'text-accent'
  if (type.startsWith('permission.')) return 'text-warn'
  if (type.startsWith('tool.')) return 'text-info'
  if (type === 'workspace.report') return 'text-alt'
  if (type.startsWith('assistant.')) return 'text-text'
  if (type === 'raw.unclassified') return 'text-faint'
  return 'text-muted'
}
