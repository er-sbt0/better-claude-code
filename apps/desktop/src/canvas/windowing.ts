import type { Block } from '@agent-workspace/shared-types/Block'

// ---------------------------------------------------------------------------
// The arithmetic behind the virtualized feed (§9).
//
// "Lexical does not virtualize. Long runs will make the app unusable, and that
// failure arrives exactly when the user is most invested." So the feed is a
// plain list that renders only what is on screen — and this file is the part of
// that with no DOM in it, which is why it is a file and not a closure inside
// the component: the windowing rule is the thing most likely to be wrong, and
// it is testable here without a window.
//
// No library. The list is one-dimensional, vertical, and variable-height, and
// what that needs is a prefix-sum table plus a binary search over it. A
// virtualization dependency would bring a measurement strategy and a scheduler
// we would then have to hold to WebKitGTK's frame budget (§4) rather than
// simply meet.
// ---------------------------------------------------------------------------

/**
 * What a block is assumed to be tall before it has been measured.
 *
 * Estimates exist to keep the scrollbar honest for the part of the list nobody
 * has looked at yet. They are per type because the types differ by an order of
 * magnitude — a collapsed lifecycle line against a forty-line diff — and one
 * average would make the scrollbar lurch on every scroll into unmeasured
 * territory.
 *
 * Being wrong here costs a correction, never correctness: every block that
 * renders is measured, and the table below is replaced by what the DOM says.
 */
export function estimate(block: Block, collapsed: boolean): number {
  if (collapsed) return 22
  switch (block.type) {
    case 'text':
      // A user turn is a bubble at 78% width with no label above it, so it
      // wraps sooner and starts lower than an agent turn. Estimating both the
      // same way would make the scrollbar lurch on every scroll past a run's
      // prompts — which is exactly the lurch this table exists to prevent, and
      // the prompts are the blocks a reader scrolls back to find.
      return block.block.speaker === 'user'
        ? 12 + Math.ceil(block.block.text.length / 48) * 18
        : 24 + Math.ceil(block.block.text.length / 64) * 18
    case 'code':
      return 34 + block.block.code.split('\n').length * 17
    case 'tool_call':
      return 44
    case 'diff':
      return 40 + block.block.hunks.reduce((n, h) => n + h.lines.length, 0) * 17
    case 'terminal':
      return 44 + Math.min(12, block.block.output.split('\n').length) * 17
    default:
      // `raw`, and anything a newer projection sends that this build has never
      // heard of. Both render as one collapsed line (§7's invariant), so both
      // estimate as one.
      return 22
  }
}

/** Running offsets: `offsets[i]` is the top of block `i`, `offsets[n]` the end. */
export function offsetsOf(sizes: readonly number[]): number[] {
  const offsets = new Array<number>(sizes.length + 1)
  offsets[0] = 0
  for (let i = 0; i < sizes.length; i++) offsets[i + 1] = offsets[i]! + sizes[i]!
  return offsets
}

/** The half-open range of blocks to render, given where the reader is. */
export type Slice = { first: number; last: number }

/**
 * Which blocks intersect the viewport, plus `overscan` on each side.
 *
 * Overscan is not a performance trick, it is what stops a fast scroll from
 * showing a band of blank while React catches up — and on WebKitGTK, which
 * repaints a transformed subtree more slowly than the other two engines (§4),
 * that band is wider than it would be elsewhere.
 */
export function slice(
  offsets: readonly number[],
  scrollTop: number,
  viewport: number,
  overscan: number,
): Slice {
  const count = offsets.length - 1
  if (count <= 0) return { first: 0, last: 0 }
  const first = Math.max(0, search(offsets, scrollTop) - overscan)
  const last = Math.min(count, search(offsets, scrollTop + viewport) + 1 + overscan)
  return { first, last }
}

/**
 * The index of the block containing `y` — the last `i` with `offsets[i] <= y`.
 *
 * Binary search rather than a scan, because this runs on every scroll event of
 * every mounted feed: at 5k blocks a scan is 5k comparisons per frame per
 * column, which is the shape of cost that only shows up once several columns
 * are on screen — which is exactly the case §4's budget was re-drawn for.
 */
function search(offsets: readonly number[], y: number): number {
  let lo = 0
  let hi = offsets.length - 1
  while (lo < hi) {
    const mid = (lo + hi + 1) >> 1
    if (offsets[mid]! <= y) lo = mid
    else hi = mid - 1
  }
  return lo
}

/**
 * Where each of `seqs` renders inside the feed, in content pixels.
 *
 * This is what replaced walking the feed's `<li>` children by index. Step 5's
 * fork toolbar found a block by counting DOM nodes — "the *n*th child of its
 * list is `events[n]`" — which was written down as a stand-in for exactly this
 * and had two problems the moment the feed virtualized: the *n*th child is no
 * longer block *n*, and the block a wire is anchored at is usually not rendered
 * at all.
 *
 * So the position is read off the table instead of off the DOM, which answers
 * for every block rather than only for the visible ones.
 */
export function anchors(
  blocks: readonly Block[],
  offsets: readonly number[],
  sizes: readonly number[],
  seqs: readonly number[],
): Record<number, number> {
  const out: Record<number, number> = {}
  for (const seq of seqs) {
    const index = blocks.findIndex((b) => b.seqs.includes(seq))
    if (index < 0) continue
    out[seq] = offsets[index]! + (sizes[index] ?? 0) / 2
  }
  return out
}
