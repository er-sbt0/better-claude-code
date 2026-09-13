import type { Placed } from './layout'

// ---------------------------------------------------------------------------
// Camera maths and easing for the one canvas.
//
// Presentation, deliberately and entirely. The rule recorded against
// `apps/desktop` is that anything here which *decides* belongs in a crate where
// a test reaches it without a window — forkability, status and the fork address
// all moved out for that reason. None of this is that: a viewport is a fact
// about a window's pixels, and there is no window in a crate.
//
// The numbers below are not free parameters. `docs/branching-surface.md` fixes
// fork motion as **variant B, split in place**, and the prototype's timings
// with it; they are transcribed here rather than re-tuned, and the record is
// the place to change them.
// ---------------------------------------------------------------------------

/** The whole fork motion, from ⌘⇧F to focus landing on the child. */
export const FORK_MS = 900

/** The child starts translated onto the parent at this scale, and peels out. */
export const FORK_CHILD_SCALE = 0.88

/**
 * The camera fits parent and child at **no more than** this.
 *
 * A ceiling rather than a target: a fit that needs to be smaller is smaller,
 * and a graph small enough to fit at 1.0 is still pulled back to here, because
 * the pull-back is what says *something just branched* — a fork that happens at
 * the zoom you were already at looks like a node appearing out of nowhere.
 */
export const FORK_FIT_SCALE = 0.78

/** How long ⌘\ takes to cross between the two representations. */
export const VIEW_MS = 260

/** How long ⌘[ / ⌘] take to travel to a sibling. */
export const TRAVEL_MS = 240

export type Viewport = { x: number; y: number; scale: number }

/** A rectangle in canvas coordinates — what a node occupies before the camera. */
export type Box = { x: number; y: number; w: number; h: number }

export const boxOf = (node: Placed): Box => ({ x: node.x, y: node.y, w: node.w, h: node.h })

/** The smallest box containing both. */
export function union(a: Box, b: Box): Box {
  const x = Math.min(a.x, b.x)
  const y = Math.min(a.y, b.y)
  return {
    x,
    y,
    w: Math.max(a.x + a.w, b.x + b.w) - x,
    h: Math.max(a.y + a.h, b.y + b.h) - y,
  }
}

/**
 * A camera that holds `box` centred in a viewport of `frame`, at `scale`.
 *
 * One function for every "look at this" in the app — the fork motion, ⌘\, and
 * ⌘[ / ⌘] all end here — which is the mechanical half of *two representations
 * of one canvas*: two callers cannot centre differently if there is only one
 * way to centre.
 */
export function centreOn(box: Box, frame: { width: number; height: number }, scale: number): Viewport {
  return {
    x: frame.width / 2 - (box.x + box.w / 2) * scale,
    y: frame.height / 2 - (box.y + box.h / 2) * scale,
    scale,
  }
}

/** The largest scale at which `box` fits inside `frame`, with a margin. */
export function fitScale(
  box: Box,
  frame: { width: number; height: number },
  pad: number,
  max: number,
): number {
  if (box.w === 0 || box.h === 0) return max
  return Math.min(max, (frame.width - pad * 2) / box.w, (frame.height - pad * 2) / box.h)
}

/** Fast out of the gate, settling at the end: the motion of something landing. */
export const easeOutCubic = (t: number): number => 1 - (1 - t) ** 3

/** Eased at both ends, for a camera that was already still. */
export const easeInOut = (t: number): number =>
  t < 0.5 ? 4 * t * t * t : 1 - (-2 * t + 2) ** 3 / 2

export const lerp = (a: number, b: number, t: number): number => a + (b - a) * t

export const lerpView = (a: Viewport, b: Viewport, t: number): Viewport => ({
  x: lerp(a.x, b.x, t),
  y: lerp(a.y, b.y, t),
  scale: lerp(a.scale, b.scale, t),
})
