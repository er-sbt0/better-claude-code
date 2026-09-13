import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type FormEvent,
} from 'react'
import type { ForkBlock } from '@agent-workspace/shared-types/ForkBlock'
import type { RawEvent } from '@agent-workspace/shared-types/RawEvent'
import type { Run } from '@agent-workspace/shared-types/Run'
import type { RunActivity } from '@agent-workspace/shared-types/RunActivity'
import type { Size } from './layout'
import { ACTIVITY, STATUS, clip, goalOf, nameOf } from '../lib/format'
import { EventStream } from './EventStream'

// A node on the graph, and `docs/branching-surface.md` decided what that is: a
// conversation column. Header, message list, composer — not a card you expand.
//
// The rule it overturns is §10's "expanding is a deliberate act, not the
// default state", and the argument is mechanical rather than aesthetic:
// **forking is anchored to a message, so forking requires reading messages.** A
// default state that hides them makes the surface's primary act cost a
// preliminary step every single time. §10's shape is not lost — it is the
// `map` half of this file, which is a zoom level (⌘\) and not a chevron.
//
// Both halves are rendered from the same run, the same projection and the same
// three-word vocabulary. They differ in what they draw and in nothing else: one
// camera, one layout pass, two renderings.

/** 560 wide, per the record's table. */
const COLUMN: Size = { w: 560, h: 620 }

/**
 * §10's shape card, at the width of the column it is the shape *of*.
 *
 * The width is the same 560 on purpose. Map view is a zoom level rather than a
 * different diagram, so the columns must not slide sideways when it is entered:
 * what changes is that each column collapses to its shape, and the camera pulls
 * back far enough to see the branches. A card of a different width would
 * re-flow the whole graph horizontally and read as a relayout.
 */
const CARD: Size = { w: 560, h: 132 }

export type ViewMode = 'focus' | 'map'

/**
 * How much room a node needs, in this view.
 *
 * `layout.ts` takes this as a parameter so the layout pass never has to know
 * what a node draws — which is why the second representation of the canvas
 * costs that file nothing.
 */
export const sizeOf =
  (mode: ViewMode) =>
  (): Size =>
    mode === 'map' ? CARD : COLUMN

/** Two measurements are equal when every block is in the same place. */
const same = (a: Record<number, number>, b: Record<number, number>): boolean => {
  const ka = Object.keys(a)
  return ka.length === Object.keys(b).length && ka.every((k) => a[Number(k)] === b[Number(k)])
}

/** Where a child branched, and which child it was — for the parent's own body. */
export type Branch = { seq: number; child: Run }

export function RunNode({
  run,
  mode,
  activity,
  events,
  forkable,
  branches,
  parent,
  inherited,
  selected,
  live,
  onSelect,
  onSend,
  onHoverBlock,
  onFork,
  onMeasure,
}: {
  run: Run
  mode: ViewMode
  /** The three-word status and the message count, both projections (§8). */
  activity: RunActivity | null
  events: RawEvent[]
  /** The blocks of *this* run a fork can be anchored at (`EventLog::fork_points`). */
  forkable: ForkBlock[]
  /** Where this run's children branched off it. */
  branches: Branch[]
  /** The run this one was forked from, when it is on screen. */
  parent: Run | null
  /** Messages inherited at the branch point, from the parent's projection. */
  inherited: number | null
  selected: boolean
  /** Whether *this process* is supervising the run, i.e. can take a turn. */
  live: boolean
  onSelect: () => void
  onSend: (text: string) => Promise<void>
  /** The block under the pointer, or null — never a nearby one. */
  onHoverBlock: (seq: number | null) => void
  onFork: (seq: number) => void
  /** Where each branch point rendered, in node-local pixels, for the edges. */
  onMeasure: (anchors: Record<number, number>) => void
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
        // `relative` is load-bearing, not decoration: it makes this element the
        // offset parent every block measurement below is expressed against.
        'relative flex h-full w-full cursor-default flex-col overflow-hidden rounded-md border bg-panel',
        selected ? 'border-accent' : tone.border,
      ].join(' ')}
    >
      {mode === 'map' ? (
        <ShapeCard run={run} activity={activity} parent={parent} inherited={inherited} />
      ) : (
        <>
          <Header run={run} activity={activity} />
          {parent !== null && (
            <InheritedContext run={run} parent={parent} inherited={inherited} />
          )}
          <Body
            events={events}
            forkable={forkable}
            branches={branches}
            onHoverBlock={onHoverBlock}
            onFork={onFork}
            onMeasure={onMeasure}
            onSelect={onSelect}
          />
          <Composer live={live} onSend={onSend} />
        </>
      )}
    </div>
  )
}

function Header({ run, activity }: { run: Run; activity: RunActivity | null }) {
  const tone = ACTIVITY[activity?.status ?? 'idle']
  return (
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
  )
}

/**
 * §10's shape card: name, status, `model · N msgs`, one-line summary.
 *
 * **It carries the outcome as well as the attention pill, and that is not a
 * fourth word.** The three-word vocabulary answers *does this want me now*, so
 * `finished`, `failed` and `abandoned` all collapse into **idle** — correctly,
 * because on that axis they are the same. The consequence lands here and
 * nowhere else: map view exists to compare branches, and a branch that failed
 * and a branch that succeeded must not be one grey pill apart on the one
 * surface whose whole job is that comparison. So the card states both axes —
 * the pill from `Activity`, the outcome from `RunStatus`, which is still on the
 * row and which `format.ts` still spells in four words. The record says the
 * three words propagate to border, pill, sliver and card; it does not say the
 * card shows only them.
 */
function ShapeCard({
  run,
  activity,
  parent,
  inherited,
}: {
  run: Run
  activity: RunActivity | null
  parent: Run | null
  inherited: number | null
}) {
  const tone = ACTIVITY[activity?.status ?? 'idle']
  const outcome = STATUS[run.status]
  return (
    <div className="flex h-full flex-col gap-1 px-3 py-2">
      <div className="flex items-center gap-2">
        <i className={`size-1.5 shrink-0 rounded-full ${tone.dot}`} aria-hidden="true" />
        <span className="truncate font-mono text-mono font-semibold">{nameOf(run)}</span>
        <span className={`shrink-0 rounded-sm px-1 font-mono text-micro ${tone.text}`}>
          {tone.label}
        </span>
        {/* The second axis. Silent only while the two would say the same thing:
            a running run's outcome is not yet a fact. */}
        {run.status !== 'running' && (
          <span className={`shrink-0 font-mono text-micro ${outcome.text}`}>
            {outcome.label}
          </span>
        )}
      </div>

      <p className="truncate font-mono text-micro text-muted">
        {run.model ?? 'provider default model'}
        {' · '}
        {activity === null ? '—' : `${activity.messages} msgs`}
      </p>

      <p className="line-clamp-2 text-micro text-muted">{clip(goalOf(run), 160)}</p>

      {parent !== null && (
        <p className="mt-auto truncate font-mono text-micro text-faint">
          ⑂ {nameOf(parent)}
          {inherited === null ? '' : ` · inherited ${inherited} msgs`}
        </p>
      )}
    </div>
  )
}

/**
 * The strip a child wears: where it came from, and how much it started with.
 *
 * Carried over from the prototype without argument (`branching-surface.md`),
 * and it is the reason forking never snaps to a nearby address. The count is
 * stated out loud here, so a fork that silently rewound past the block the user
 * clicked would make this line a lie — which is why an unaddressable block
 * offers no toolbar at all rather than the nearest one that resolves.
 *
 * When the parent's projection has not arrived the count is simply absent. A
 * number this strip cannot substantiate is worse than no number.
 */
function InheritedContext({
  run,
  parent,
  inherited,
}: {
  run: Run
  parent: Run | null
  inherited: number | null
}) {
  return (
    <p className="shrink-0 truncate border-b border-border-soft bg-elevated/60 px-3 py-1 font-mono text-micro text-muted">
      <span className="text-alt">⑂ forked from</span> {parent === null ? '—' : nameOf(parent)}
      {run.branch_point_seq === null ? '' : ` at seq ${run.branch_point_seq}`}
      {inherited === null ? '' : ` · inherits ${inherited} messages`}
    </p>
  )
}

/**
 * The scrolling body — a frame around scaffolding, and the one place the fork
 * gesture can live.
 *
 * `EventStream.tsx`'s own comment says it "must be replaced, not extended", and
 * it is untouched: §7's feed is the next piece of work and the largest in V1's
 * UI. What this adds sits *over* it rather than inside it — a hover toolbar, a
 * breadcrumb per branch point, and the measurement the anchored edges need.
 *
 * **How it addresses a block without editing the feed.** `EventStream` renders
 * exactly one `<li>` per event, in order, so the *n*th child of its list is
 * `events[n]`. That index is the whole coupling, it is read and never written,
 * and it disappears at step 6: §7's blocks carry `(runId, seqRange)` addresses
 * of their own, which is precisely what this is a stand-in for.
 */
function Body({
  events,
  forkable,
  branches,
  onHoverBlock,
  onFork,
  onMeasure,
  onSelect,
}: {
  events: RawEvent[]
  forkable: ForkBlock[]
  branches: Branch[]
  onHoverBlock: (seq: number | null) => void
  onFork: (seq: number) => void
  onMeasure: (anchors: Record<number, number>) => void
  onSelect: () => void
}) {
  const scroller = useRef<HTMLDivElement | null>(null)
  const pinned = useRef(true)
  const [hover, setHover] = useState<{ seq: number; top: number } | null>(null)
  const [marks, setMarks] = useState<Record<number, number>>({})

  const forkableAt = new Map(forkable.map((b) => [b.seq, b]))

  // Follow a live run, but only while the reader is already at the bottom —
  // yanking someone back down mid-scroll is how a feed becomes unreadable.
  useEffect(() => {
    const el = scroller.current
    if (el === null || !pinned.current) return
    el.scrollTop = el.scrollHeight
  }, [events.length])

  /**
   * Measure every branch point, twice over: once in the node's own coordinates
   * for the edge that leaves the column, and once in the feed's for the
   * breadcrumb that stays with the block.
   *
   * Offsets rather than `getBoundingClientRect`, because the canvas draws this
   * node inside a CSS transform: a rect would come back multiplied by the
   * camera's scale and the layout pass works in unscaled coordinates.
   */
  const measure = useCallback(() => {
    const el = scroller.current
    if (el === null) return
    const list = el.querySelector('ol')
    const nodeLocal: Record<number, number> = {}
    const feedLocal: Record<number, number> = {}
    for (const { seq } of branches) {
      const index = events.findIndex((e) => e.seq === seq)
      const li = index < 0 ? null : list?.children[index]
      if (!(li instanceof HTMLElement)) continue
      feedLocal[seq] = li.offsetTop + li.offsetHeight / 2
      nodeLocal[seq] = el.offsetTop + feedLocal[seq] - el.scrollTop
    }
    // Both guarded by value: this runs on every layout, and handing back a
    // fresh object each time would re-render, re-measure and never settle.
    setMarks((prev) => (same(prev, feedLocal) ? prev : feedLocal))
    onMeasure(nodeLocal)
  }, [branches, events, onMeasure])

  useLayoutEffect(measure, [measure])

  const blockAt = (target: EventTarget | null): RawEvent | null => {
    if (!(target instanceof Element)) return null
    const li = target.closest('li')
    const list = li?.parentElement
    if (li === null || list === undefined || list === null) return null
    const index = Array.prototype.indexOf.call(list.children, li)
    return events[index] ?? null
  }

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <p className="shrink-0 border-b border-border-soft px-3 py-1 text-micro text-faint">
        raw event log — scaffolding, not §7’s feed · ⌘⇧F forks at a message
      </p>
      <div
        ref={scroller}
        // Read by the canvas's wheel handler, which otherwise swallows the
        // gesture to pan and leaves this region unable to scroll at all.
        data-scrollable="true"
        onScroll={(e) => {
          const el = e.currentTarget
          pinned.current = el.scrollHeight - el.scrollTop - el.clientHeight < 24
          measure()
        }}
        onMouseMove={(e) => {
          // The toolbar sits over the block it belongs to, so a pointer that
          // has moved onto the toolbar is still on that block. Without this,
          // reaching for the button clears the hover that produced it.
          if (e.target instanceof Element && e.target.closest('[data-fork-ui]') !== null) return
          const event = blockAt(e.target)
          const li = e.target instanceof Element ? e.target.closest('li') : null
          // **No snapping.** A block whose `idempotency_key` resolves to no
          // address is not forkable, and the surface says so by offering
          // nothing — never by rounding back to the last block that resolves.
          if (event === null || li === null || !forkableAt.has(event.seq)) {
            setHover(null)
            onHoverBlock(null)
            return
          }
          setHover({ seq: event.seq, top: li.offsetTop })
          onHoverBlock(event.seq)
        }}
        onMouseLeave={() => {
          setHover(null)
          onHoverBlock(null)
        }}
        // `relative` makes this the offset parent for the overlays below, so a
        // breadcrumb pinned at a block's `offsetTop` scrolls with the block.
        className="relative min-h-0 flex-1 overflow-y-auto"
      >
        <EventStream events={events} />

        {/* In-parent fork breadcrumbs, at the branch point (the record carries
            these over from the prototype without argument). This is the parent
            half of the edge the canvas draws: the graph says a branch left
            here, and the feed says it left *from this message*. */}
        {branches.map(({ seq, child }) =>
          marks[seq] === undefined ? null : (
            <button
              key={child.id}
              type="button"
              data-fork-ui="true"
              onPointerDown={(e) => e.stopPropagation()}
              onClick={(e) => {
                e.stopPropagation()
                onSelect()
              }}
              style={{ top: marks[seq] }}
              title={`A branch left this message: ${nameOf(child)}`}
              className="absolute left-0 -translate-y-1/2 rounded-r-sm border-y border-r border-alt/50 bg-panel px-1.5 font-mono text-micro text-alt"
            >
              ⑂ {nameOf(child)}
            </button>
          ),
        )}

        {/* The hover toolbar. It exists on forkable blocks and nowhere else. */}
        {hover !== null && (
          <div
            data-fork-ui="true"
            style={{ top: hover.top }}
            className="absolute right-2 flex items-center gap-1"
          >
            <span className="rounded-sm bg-bg/80 px-1 font-mono text-micro text-faint">
              inherits {forkableAt.get(hover.seq)?.inherited_messages ?? '—'} msgs
            </span>
            <button
              type="button"
              onPointerDown={(e) => e.stopPropagation()}
              onClick={(e) => {
                e.stopPropagation()
                onFork(hover.seq)
              }}
              title="Fork the conversation at this message (⌘⇧F)"
              className="rounded-sm border border-alt/60 bg-panel px-1.5 font-mono text-micro text-alt hover:bg-hover"
            >
              ⑂ fork here ⌘⇧F
            </button>
          </div>
        )}
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
