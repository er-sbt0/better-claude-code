import { useState, type FormEvent } from 'react'
import type { Block } from '@agent-workspace/shared-types/Block'
import type { Feed as FeedDocument } from '@agent-workspace/shared-types/Feed'
import type { Run } from '@agent-workspace/shared-types/Run'
import type { RunActivity } from '@agent-workspace/shared-types/RunActivity'
import type { Size } from './layout'
import { ACTIVITY, STATUS, clip, goalOf, nameOf } from '../lib/format'
import { Feed, type Branch } from './Feed'

export type { Branch } from './Feed'

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

export function RunNode({
  run,
  mode,
  activity,
  feed,
  inheritedAt,
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
  /** §7's blocks for this run, or `null` while the projection has not arrived. */
  feed: FeedDocument | null
  /** What a fork at a `seq` of this run would inherit (`EventLog::fork_points`). */
  inheritedAt: (seq: number) => number | null
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
          <Feed
            blocks={feed?.blocks ?? EMPTY_BLOCKS}
            loaded={feed !== null}
            branches={branches}
            inheritedAt={inheritedAt}
            onHoverBlock={onHoverBlock}
            onFork={onFork}
            onMeasure={onMeasure}
            onSelect={onSelect}
          />
          <Composer
            live={live}
            resumable={run.provider_session_id !== null}
            onSend={onSend}
          />
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

/** One shared empty list, so a run whose feed has not arrived is handed the same one twice. */
const EMPTY_BLOCKS: Block[] = []

/**
 * The composer, pinned to the bottom edge — and it sends.
 *
 * `send_turn` has existed since the shell was built and nothing called it, so
 * the run in the window could be started, watched and killed but never talked
 * to. The record's node is one you *talk to*: every branch in the prototype
 * begins with the user typing into the branch, which is the whole reason the
 * default state shows messages.
 *
 * **Three states, not two.** A run this process is not supervising is asleep,
 * not gone: the agent process died with the window that started it, but §14
 * says the provider owns the session and we assign its id, so the row carries
 * everything needed to wake it. Sending to a sleeping run resumes it and sends
 * the turn as the prompt that does the waking — `send_turn` decides which of
 * the two it is, because deciding is not the shell's.
 *
 * Only a run with no provider session id is genuinely unable to take a turn,
 * and that is a run whose process never got far enough to be resumable. The
 * earlier version of this component treated *every* unsupervised run that way,
 * which meant the composer was dead on every run from every previous session —
 * which is nearly all of them, for anyone who has closed the app once.
 */
function Composer({
  live,
  resumable,
  onSend,
}: {
  live: boolean
  /** Asleep rather than gone: there is a provider session to resume. */
  resumable: boolean
  onSend: (text: string) => Promise<void>
}) {
  const [text, setText] = useState('')
  const [sending, setSending] = useState(false)
  const canSend = live || resumable
  const ready = canSend && !sending && text.trim() !== ''

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
        disabled={!canSend}
        rows={2}
        placeholder={
          live
            ? 'Reply to this run…  (Enter to send, Shift+Enter for a newline)'
            : resumable
              ? 'Reply to wake this run…  (Enter to send, Shift+Enter for a newline)'
              : 'This run never reached a provider session, so there is nothing to resume.'
        }
        className="w-full resize-none rounded-sm border border-border bg-bg px-2 py-1 font-mono text-mono text-text outline-none select-text placeholder:text-faint focus:border-accent/70 disabled:cursor-not-allowed disabled:text-faint"
      />
      <div className="mt-1 flex items-center gap-2">
        <span className="min-w-0 flex-1 truncate text-micro text-faint">
          {live
            ? 'Nothing can prompt: a tool outside the run’s allow-list is denied (§12).'
            : resumable
              ? 'Asleep. Sending resumes the session in its own worktree, and pre-grants no tools.'
              : 'No provider session was ever recorded for this run.'}
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
