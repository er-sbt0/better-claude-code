import { useEffect, useRef, useState, type FormEvent } from 'react'
import type { RawEvent } from '@agent-workspace/shared-types/RawEvent'
import type { Run } from '@agent-workspace/shared-types/Run'
import type { RunActivity } from '@agent-workspace/shared-types/RunActivity'
import type { Size } from './layout'
import { ACTIVITY, nameOf } from '../lib/format'
import { EventStream } from './EventStream'

// A node on the graph, and `docs/branching-surface.md` decided what that is: a
// conversation column. Header, message list, composer — not a card you expand.
//
// The rule it overturns is §10's "expanding is a deliberate act, not the
// default state", and the argument is mechanical rather than aesthetic:
// **forking is anchored to a message, so forking requires reading messages.** A
// default state that hides them makes the surface's primary act cost a
// preliminary step every single time. §10's shape is not lost — it becomes map
// view's card, which is a zoom level (⌘\) and not a chevron. Neither map view
// nor forking is built here.
//
// The chevron is therefore gone, and with it the collapsed size. What replaced
// it is a column that can be *used*: the composer below sends a real turn
// through `send_turn`, because a composer that cannot send is the same
// decoration problem as a header that names a model nothing stored.

/** 560 wide, per the record's table. */
const COLUMN: Size = { w: 560, h: 620 }

/**
 * How much room this node needs.
 *
 * A constant now, and the seam survives anyway: `layout.ts` takes `sizeOf` as a
 * parameter so the layout pass never has to know what a node draws, and map
 * view will hand it a different function for the same runs.
 */
export const sizeOf = (): Size => COLUMN

export function RunNode({
  run,
  activity,
  events,
  selected,
  live,
  onSelect,
  onSend,
}: {
  run: Run
  /** The three-word status and the message count, both projections (§8). */
  activity: RunActivity | null
  events: RawEvent[]
  selected: boolean
  /** Whether *this process* is supervising the run, i.e. can take a turn. */
  live: boolean
  onSelect: () => void
  onSend: (text: string) => Promise<void>
}) {
  const tone = ACTIVITY[activity?.status ?? 'idle']

  return (
    <div
      onPointerDown={(e) => {
        // The canvas pans on a background drag; a drag that starts on a node
        // is a click on the node, and must not become a pan.
        e.stopPropagation()
        onSelect()
      }}
      // The canvas fits the graph on a double-click. Selecting a word in the
      // composer is a double-click, and it must not move the camera.
      onDoubleClick={(e) => e.stopPropagation()}
      className={[
        'flex h-full w-full cursor-default flex-col overflow-hidden rounded-md border bg-panel',
        selected ? 'border-accent' : tone.border,
      ].join(' ')}
    >
      <header className="shrink-0 border-b border-border-soft px-3 py-2">
        <div className="flex items-center gap-2">
          <i className={`size-1.5 shrink-0 rounded-full ${tone.dot}`} aria-hidden="true" />
          <span className="truncate font-mono text-mono font-semibold" title={run.prompt ?? ''}>
            {nameOf(run)}
          </span>
          <span className={`shrink-0 font-mono text-micro ${tone.text}`}>{tone.label}</span>
          <span className="ml-auto shrink-0 font-mono text-micro text-faint">
            {activity === null ? '—' : `${activity.messages} msg`}
          </span>
        </div>
        {/* The model, on its own line and read off the run's own row. It is a
            column as of schema V3 precisely so this is a fact rather than
            decoration; a run launched without `--model` names none, and saying
            which default the provider picked would be a guess. */}
        <p className="mt-0.5 truncate font-mono text-micro text-muted">
          {run.model ?? 'provider default model'}
        </p>
      </header>

      <Body events={events} />

      <Composer live={live} onSend={onSend} />
    </div>
  )
}

/**
 * The scrolling body — and a frame around scaffolding, deliberately.
 *
 * `EventStream.tsx`'s own comment says it "must be replaced, not extended", so
 * it is untouched here and simply given a frame and a label. §7's feed — six
 * block types plus `Raw`, virtualized, `(runId, seqRange)` on every block — is
 * the next piece of work and the largest in V1's UI; building any of it here
 * would be building it against a guess rather than against the addresses a
 * working fork already resolves.
 */
function Body({ events }: { events: RawEvent[] }) {
  const scroller = useRef<HTMLDivElement | null>(null)
  const pinned = useRef(true)

  // Follow a live run, but only while the reader is already at the bottom —
  // yanking someone back down mid-scroll is how a feed becomes unreadable.
  useEffect(() => {
    const el = scroller.current
    if (el === null || !pinned.current) return
    el.scrollTop = el.scrollHeight
  }, [events.length])

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <p className="shrink-0 border-b border-border-soft px-3 py-1 text-micro text-faint">
        raw event log — scaffolding, not §7’s feed
      </p>
      <div
        ref={scroller}
        // Read by the canvas's wheel handler, which otherwise swallows the
        // gesture to pan and leaves this region unable to scroll at all.
        data-scrollable="true"
        onScroll={(e) => {
          const el = e.currentTarget
          pinned.current = el.scrollHeight - el.scrollTop - el.clientHeight < 24
        }}
        className="min-h-0 flex-1 overflow-y-auto"
      >
        <EventStream events={events} />
      </div>
    </div>
  )
}

/**
 * The composer, pinned to the bottom edge — and it sends.
 *
 * `send_turn` has existed since the shell was built and nothing called it, so
 * the run in the window could be started, watched and killed but never talked
 * to. The record's node is one you *talk to*: every branch in the prototype
 * begins with the user typing into the branch, which is the whole reason the
 * default state shows messages.
 *
 * A run this process is not supervising cannot take a turn — the agent is a
 * process, and one from an earlier session is gone even though its log is
 * right there. That is said out loud rather than shown as a dead button.
 */
function Composer({
  live,
  onSend,
}: {
  live: boolean
  onSend: (text: string) => Promise<void>
}) {
  const [text, setText] = useState('')
  const [sending, setSending] = useState(false)
  const ready = live && !sending && text.trim() !== ''

  const submit = (e?: FormEvent) => {
    e?.preventDefault()
    if (!ready) return
    setSending(true)
    onSend(text)
      .then(() => setText(''))
      // The error is already a toast: `App.tsx` owns every command's failure
      // string, and this keeps what was typed so it can be sent again.
      .catch(() => undefined)
      .finally(() => setSending(false))
  }

  return (
    <form onSubmit={submit} className="shrink-0 border-t border-border p-2">
      <textarea
        value={text}
        onChange={(e) => setText(e.target.value)}
        onKeyDown={(e) => {
          // Enter sends, Shift+Enter is a newline — a composer, not a form.
          if (e.key === 'Enter' && !e.shiftKey) submit()
        }}
        disabled={!live}
        rows={2}
        placeholder={
          live
            ? 'Reply to this run…  (Enter to send, Shift+Enter for a newline)'
            : 'This run is not supervised by this session — its agent process is gone.'
        }
        className="w-full resize-none rounded-sm border border-border bg-bg px-2 py-1 font-mono text-mono text-text outline-none select-text placeholder:text-faint focus:border-accent/70 disabled:cursor-not-allowed disabled:text-faint"
      />
      <div className="mt-1 flex items-center gap-2">
        <span className="min-w-0 flex-1 truncate text-micro text-faint">
          {live
            ? 'Nothing can prompt: a tool outside the run’s allow-list is denied (§12).'
            : 'Only a run this session started can take another turn.'}
        </span>
        <button
          type="submit"
          disabled={!ready}
          className="shrink-0 rounded-sm border border-border bg-panel px-3 py-1 text-micro text-text enabled:hover:border-accent/60 disabled:text-faint"
        >
          {sending ? 'Sending…' : 'Send'}
        </button>
      </div>
    </form>
  )
}
