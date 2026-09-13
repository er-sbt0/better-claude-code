import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react'
import type { Block } from '@agent-workspace/shared-types/Block'
import type { Run } from '@agent-workspace/shared-types/Run'
import { BlockView } from './Blocks'
import { anchors, estimate, offsetsOf, slice } from './windowing'
import { nameOf } from '../lib/format'

// ---------------------------------------------------------------------------
// §7's run feed. This file replaced `EventStream.tsx`, which said of itself:
// "Do not extend this into the feed — replace it."
//
// What replaced it, and why each part is here:
//
// - **Six block types plus `Raw`**, drawn by `Blocks.tsx` and decided by
//   `crates/document-model`. Nothing in this file works out what a block is.
// - **Virtualized** (§9). The feed is "append-only, streaming, read-only, and
//   unbounded — a 40-minute run produces thousands of blocks", and the failure
//   mode of not virtualizing "arrives exactly when the user is most invested".
//   Only the blocks on screen are in the DOM; `windowing.ts` is the arithmetic.
// - **Collapse/expand is first-class** (§3's row), per block, with the
//   collapsed line coming from the projection rather than from here.
// - **Every block carries its own address**, so the fork toolbar asks the block
//   where it is instead of counting DOM nodes. Step 5's stand-in — "the *n*th
//   child of its list is `events[n]`" — is gone, and it had to be: under
//   virtualization the *n*th child is not block *n*, and the block an edge is
//   anchored at is usually not rendered at all.
// ---------------------------------------------------------------------------

/** Where a child branched, and which child it was — for the parent's own feed. */
export type Branch = { seq: number; child: Run }

/** How many blocks beyond the viewport to keep mounted, each way. */
const OVERSCAN = 4

/** Follow a live run only while the reader is already at the bottom. */
const PINNED_WITHIN = 24

/**
 * A block's identity across re-projections.
 *
 * The feed is re-derived from the whole log whenever the run moves, so the
 * `Block` objects are new every time and identity cannot be the object. The
 * address is the identity: the first event it covers, plus its index among the
 * blocks that share it — one `assistant` line can be prose, code and prose
 * again (§7), and those three must not collapse as one.
 */
function keyOf(block: Block, index: number, blocks: readonly Block[]): string {
  const first = block.seqs[0] ?? index
  let nth = 0
  for (let i = index - 1; i >= 0 && blocks[i]!.seqs[0] === first; i--) nth++
  return `${first}#${nth}`
}

/**
 * A tool call is one line until asked for.
 *
 * `Read src/auth.ts` is what a reader wants from a tool call nine times out of
 * ten, and it is what the projection's own `summary` already says. Expanded, it
 * is the arguments and the output. The default is collapsed because a coding
 * conversation is mostly prose *about* tool calls, and a feed that opens every
 * one of them in full is a log with prose in it rather than a conversation with
 * tool calls in it.
 *
 * `Diff`, `Terminal` and `Code` stay open: those are the work itself, not a
 * record that work happened.
 */
const collapsedByDefault = (block: Block): boolean =>
  block.type === 'raw' || block.type === 'tool_call'

/**
 * What "conversation" leaves out, and why it is a view rather than a filter in
 * the projection.
 *
 * `run.started`, both `turn.*`, the hook lifecycle and the rate-limit notices
 * are `Raw` blocks — §7 ships six types and lifecycle earns none of them — and
 * a run's feed opens with five of them before a word is said. They are plumbing
 * that happens to be on the same stream as the conversation, and interleaving
 * them is the single thing that stops a column reading as a chat.
 *
 * They are hidden here and nowhere else: `document-model` keeps every one of
 * them, §7's invariant is about the *projection* preserving what it cannot
 * classify, and the count is always on screen with one click to show them. That
 * count is not decoration — a `Raw` block can also be a message the normalizer
 * failed to classify, and a view that dropped those silently would be hiding a
 * bug in the one place nobody would look for it.
 */
const isPlumbing = (block: Block): boolean => block.type === 'raw'

export function Feed({
  blocks: allBlocks,
  branches,
  inheritedAt,
  loaded,
  onHoverBlock,
  onFork,
  onMeasure,
  onSelect,
}: {
  blocks: Block[]
  branches: Branch[]
  /** What a fork at this `seq` would inherit, from the parent's projection. */
  inheritedAt: (seq: number) => number | null
  /** False while this run's feed has not been fetched — not the same as empty. */
  loaded: boolean
  /** The block under the pointer, by its own address — never a nearby one. */
  onHoverBlock: (seq: number | null) => void
  onFork: (seq: number) => void
  /** Where each branch point rendered, in node-local pixels, for the edges. */
  onMeasure: (anchors: Record<number, number>) => void
  onSelect: () => void
}) {
  const scroller = useRef<HTMLDivElement | null>(null)
  const pinned = useRef(true)
  const [scrollTop, setScrollTop] = useState(0)
  const [viewport, setViewport] = useState(0)
  const [hovered, setHovered] = useState<number | null>(null)
  const [open, setOpen] = useState<Record<string, boolean>>({})
  const [showPlumbing, setShowPlumbing] = useState(false)

  // Everything below this line works on `blocks`, which is the *view*. The
  // filter is here rather than in the caller so that windowing, measurement and
  // the offset table all see one list: a hidden block that still occupied a row
  // in the prefix-sum table would leave a gap the reader could scroll into.
  const blocks = useMemo(
    () => (showPlumbing ? allBlocks : allBlocks.filter((b) => !isPlumbing(b))),
    [allBlocks, showPlumbing],
  )
  const hidden = allBlocks.length - blocks.length

  /** Which branches left from each `seq`, so a row can render its own. */
  const branchesAt = useMemo(() => {
    const by = new Map<number, Branch[]>()
    for (const b of branches) {
      const at = by.get(b.seq)
      if (at === undefined) by.set(b.seq, [b])
      else at.push(b)
    }
    return by
  }, [branches])

  const keys = useMemo(() => blocks.map((b, i) => keyOf(b, i, blocks)), [blocks])
  const isOpen = useCallback(
    (index: number): boolean => open[keys[index]!] ?? !collapsedByDefault(blocks[index]!),
    [open, keys, blocks],
  )

  // Measured heights, by index. A ref rather than state because it is written
  // from a ResizeObserver during layout: putting it in state would re-render
  // the feed once per measured block, which at a screenful of blocks is a
  // render per block per scroll.
  const sizes = useRef<number[]>([])
  const [tick, setTick] = useState(0)
  const pending = useRef<number | null>(null)

  // Heights already learned, by `key|expanded`. Keyed rather than indexed
  // because a re-projection can insert a block above one already measured, and
  // carrying a height by index would hand the new block its neighbour's height
  // and shift everything below it by the difference. The expanded flag is part
  // of the key because a collapsed block and an expanded one are two heights of
  // the same block, and remembering one as the other makes every collapse jump.
  const measured = useRef(new Map<string, number>())
  // The branch count is part of the key because a breadcrumb is real height in
  // the row: fork from a block already measured and the cached value is a
  // height that block no longer has, which would seat the next row on top of
  // this one until something happened to re-measure it. Forking re-projects the
  // parent, so this is the ordinary path rather than a corner.
  const sizeKey = (index: number) =>
    `${keys[index]}|${isOpen(index) ? 1 : 0}|${
      (blocks[index]?.seqs ?? []).reduce((n, seq) => n + (branchesAt.get(seq)?.length ?? 0), 0)
    }`

  // Re-seat the table whenever the run moves.
  if (sizes.current.length !== blocks.length) {
    sizes.current = blocks.map(
      (block, i) => measured.current.get(sizeKey(i)) ?? estimate(block, !isOpen(i)),
    )
  }

  // `tick` is the dependency that matters: it is bumped once per *batch* of
  // measurements, which is what makes this recompute at most once a frame
  // rather than once a block.
  const offsets = useMemo(() => offsetsOf(sizes.current), [tick, blocks, open])
  const total = offsets[offsets.length - 1] ?? 0

  const flush = useCallback(() => {
    if (pending.current !== null) return
    pending.current = requestAnimationFrame(() => {
      pending.current = null
      setTick((t) => t + 1)
    })
  }, [])

  useEffect(
    () => () => {
      if (pending.current !== null) cancelAnimationFrame(pending.current)
    },
    [],
  )

  // One observer for the whole feed rather than one per block: a feed that
  // mounts and unmounts a dozen blocks a frame would otherwise churn a dozen
  // observers a frame, and on WebKitGTK observer churn is not free (§4).
  const observer = useRef<ResizeObserver | null>(null)
  const observe = useCallback(() => {
    if (observer.current !== null) return observer.current
    observer.current = new ResizeObserver((entries) => {
      let changed = false
      for (const entry of entries) {
        const el = entry.target
        if (!(el instanceof HTMLElement)) continue
        const index = Number(el.dataset.index)
        const key = el.dataset.size
        const height = el.offsetHeight
        if (Number.isNaN(index) || height === 0) continue
        if (key !== undefined) measured.current.set(key, height)
        if (sizes.current[index] !== height) {
          sizes.current[index] = height
          changed = true
        }
      }
      if (changed) flush()
    })
    return observer.current
  }, [flush])

  useEffect(() => () => observer.current?.disconnect(), [])

  // React 19 takes a cleanup back from a ref callback, which is what keeps the
  // observer from holding every block this feed has ever scrolled past.
  const attach = useCallback(
    (el: HTMLDivElement) => {
      const ro = observe()
      ro.observe(el)
      return () => ro.unobserve(el)
    },
    [observe],
  )

  useLayoutEffect(() => {
    const el = scroller.current
    if (el === null) return
    const measure = () => setViewport(el.clientHeight)
    measure()
    const ro = new ResizeObserver(measure)
    ro.observe(el)
    return () => ro.disconnect()
  }, [])

  // Follow a live run, but only from the bottom — yanking someone back down
  // mid-scroll is how a feed becomes unreadable.
  useEffect(() => {
    const el = scroller.current
    if (el === null || !pinned.current) return
    el.scrollTop = el.scrollHeight
    setScrollTop(el.scrollTop)
  }, [blocks.length, total])

  const visible = slice(offsets, scrollTop, viewport, OVERSCAN)

  // Where the branch points are, for the wires and the breadcrumbs. Read off
  // the offset table, so it answers for a block that is scrolled out of the
  // window and for one that was never rendered.
  const branchSeqs = useMemo(() => branches.map((b) => b.seq), [branches])
  const marks = useMemo(
    () => anchors(blocks, offsets, sizes.current, branchSeqs),
    [blocks, offsets, branchSeqs],
  )
  useEffect(() => {
    const el = scroller.current
    if (el === null) return
    const nodeLocal: Record<number, number> = {}
    for (const [seq, y] of Object.entries(marks)) {
      nodeLocal[Number(seq)] = el.offsetTop + y - el.scrollTop
    }
    onMeasure(nodeLocal)
  }, [marks, scrollTop, onMeasure])

  const toggle = (index: number) =>
    setOpen((prev) => ({ ...prev, [keys[index]!]: !isOpen(index) }))

  const setAll = (value: boolean) =>
    setOpen(Object.fromEntries(keys.map((key) => [key, value])))

  const items = []
  for (let i = visible.first; i < visible.last; i++) {
    const block = blocks[i]
    if (block === undefined) continue
    const key = keys[i]!
    const expanded = isOpen(i)
    const forkAt = block.fork_seq
    // A block covers several events; a branch left from exactly one of them.
    const rowBranches = block.seqs.flatMap((seq) => branchesAt.get(seq) ?? [])
    items.push(
      <div
        key={key}
        ref={attach}
        data-index={i}
        data-size={`${key}|${expanded ? 1 : 0}`}
        onMouseEnter={() => {
          setHovered(i)
          onHoverBlock(forkAt)
        }}
        style={{ position: 'absolute', top: offsets[i], left: 0, right: 0 }}
        className="border-b border-border-soft/60"
      >
        <div className="flex min-w-0 items-start">
          <button
            type="button"
            onPointerDown={(e) => e.stopPropagation()}
            onClick={(e) => {
              e.stopPropagation()
              toggle(i)
            }}
            title={expanded ? 'Collapse this block' : 'Expand this block'}
            aria-expanded={expanded}
            className="w-4 shrink-0 self-stretch pt-1.5 text-center font-mono text-micro text-faint hover:text-text"
          >
            {expanded ? '▾' : '▸'}
          </button>
          <div className="min-w-0 flex-1">
            {expanded ? (
              <BlockView block={block} />
            ) : (
              <button
                type="button"
                onPointerDown={(e) => e.stopPropagation()}
                onClick={(e) => {
                  e.stopPropagation()
                  toggle(i)
                }}
                className="block w-full truncate px-1 py-0.5 text-left font-mono text-micro text-muted"
              >
                {/* The collapsed state is the projection's own line
                    (`document_model::summary`), not a rendering of the expanded
                    one: summarizing is deciding, and the shell owns no logic. */}
                {block.summary}
              </button>
            )}
          </div>
        </div>

        {/* The hover toolbar. It exists where the block has an address and
            nowhere else — the block's own `fork_seq`, which is
            `normalizer::claude_code::fork_address` applied under the projection,
            so the surface cannot offer a fork `fork_run` would refuse. **No
            snapping**: a block with no address offers nothing rather than
            rounding back to the last one that resolved. */}
        {hovered === i && forkAt !== null && (
          <div
            data-fork-ui="true"
            className="absolute top-0.5 right-2 flex items-center gap-1"
          >
            <span className="rounded-sm bg-bg/80 px-1 font-mono text-micro text-faint">
              inherits {inheritedAt(forkAt) ?? '—'} msgs
            </span>
            <button
              type="button"
              onPointerDown={(e) => e.stopPropagation()}
              onClick={(e) => {
                e.stopPropagation()
                onFork(forkAt)
              }}
              title="Fork the conversation at this message (⌘⇧F)"
              className="rounded-sm border border-alt/60 bg-panel px-1.5 font-mono text-micro text-alt hover:bg-hover"
            >
              ⑂ fork here ⌘⇧F
            </button>
          </div>
        )}

        {/* In-parent fork breadcrumbs, in the flow of the block they left from
            (carried over from the prototype). This is the parent half of the
            edge the canvas draws: the graph says a branch left here, and the
            feed says it left *from this message*.

            In the flow, and not absolutely positioned at the measured anchor,
            which is what this was first: an overlay pinned to the branch point
            lands on top of the message it is about and hides the words the
            reader forked at. The row is measured, so occupying real space is
            what keeps the two from colliding at any block height. The *edge*
            still anchors to the measured `y` — that is `marks`, untouched,
            because a wire must meet the message and not the note under it. */}
        {rowBranches.length > 0 && (
          <div className="flex flex-wrap items-center gap-1 py-1 pr-2 pl-5">
            <span className="font-mono text-micro text-faint">
              {rowBranches.length === 1
                ? 'a branch continues from here'
                : `${rowBranches.length} branches continue from here`}
            </span>
            {rowBranches.map(({ child }) => (
              <button
                key={child.id}
                type="button"
                data-fork-ui="true"
                onPointerDown={(e) => e.stopPropagation()}
                onClick={(e) => {
                  e.stopPropagation()
                  onSelect()
                }}
                title={`A branch left this message: ${nameOf(child)}`}
                className="rounded-sm border border-alt/50 px-1.5 font-mono text-micro text-alt hover:bg-hover"
              >
                ⑂ {nameOf(child)}
              </button>
            ))}
          </div>
        )}
      </div>,
    )
  }

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <div className="flex shrink-0 items-center gap-2 border-b border-border-soft px-3 py-1">
        {/* The hidden count is never not shown: a `Raw` block can be a message
            the normalizer could not classify, and "3 hidden" is the only place
            that would ever surface. */}
        {loaded && hidden > 0 ? (
          <button
            type="button"
            onPointerDown={(e) => e.stopPropagation()}
            onClick={(e) => {
              e.stopPropagation()
              setShowPlumbing(true)
            }}
            className="shrink-0 font-mono text-micro text-faint hover:text-text"
            title="run and turn lifecycle, and anything the normalizer could not classify"
          >
            {hidden} hidden
          </button>
        ) : (
          <span className="shrink-0 font-mono text-micro text-faint">
            {!loaded ? 'loading…' : showPlumbing ? 'all events' : ''}
          </span>
        )}
        {showPlumbing && (
          <button
            type="button"
            onPointerDown={(e) => e.stopPropagation()}
            onClick={(e) => {
              e.stopPropagation()
              setShowPlumbing(false)
            }}
            className="shrink-0 font-mono text-micro text-faint hover:text-text"
          >
            conversation
          </button>
        )}
        <span className="min-w-0 flex-1 truncate text-micro text-faint">
          ⌘⇧F forks at a message
        </span>
        <button
          type="button"
          onPointerDown={(e) => e.stopPropagation()}
          onClick={(e) => {
            e.stopPropagation()
            setAll(false)
          }}
          className="shrink-0 font-mono text-micro text-faint hover:text-text"
        >
          collapse all
        </button>
        <button
          type="button"
          onPointerDown={(e) => e.stopPropagation()}
          onClick={(e) => {
            e.stopPropagation()
            setAll(true)
          }}
          className="shrink-0 font-mono text-micro text-faint hover:text-text"
        >
          expand all
        </button>
      </div>

      <div
        ref={scroller}
        // Read by the canvas's wheel handler, which otherwise swallows the
        // gesture to pan and leaves this region unable to scroll at all.
        data-scrollable="true"
        onScroll={(e) => {
          const el = e.currentTarget
          pinned.current = el.scrollHeight - el.scrollTop - el.clientHeight < PINNED_WITHIN
          setScrollTop(el.scrollTop)
        }}
        onMouseLeave={() => {
          setHovered(null)
          onHoverBlock(null)
        }}
        className="relative min-h-0 flex-1 overflow-y-auto"
      >
        {blocks.length === 0 ? (
          <p className="px-3 py-2 text-micro text-faint">
            {loaded
              ? 'Nothing yet. The first block is `run.started`, once the agent process has said hello.'
              : 'Not loaded — this run’s feed is fetched when the camera reaches it.'}
          </p>
        ) : (
          <div style={{ height: total, position: 'relative' }}>
            {items}
          </div>
        )}
      </div>
    </div>
  )
}
