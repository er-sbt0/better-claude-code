import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react'
import type { Feed } from '@agent-workspace/shared-types/Feed'
import type { ForkPoints } from '@agent-workspace/shared-types/ForkPoints'
import type { Run } from '@agent-workspace/shared-types/Run'
import type { RunId } from '@agent-workspace/shared-types/RunId'
import type { RunActivity } from '@agent-workspace/shared-types/RunActivity'
import { layout, type Placed } from './layout'
import {
  boxOf,
  centreOn,
  easeInOut,
  easeOutCubic,
  fitScale,
  lerpView,
  union,
  FORK_CHILD_SCALE,
  FORK_FIT_SCALE,
  FORK_MS,
  TRAVEL_MS,
  VIEW_MS,
  type Box,
  type Viewport,
} from './motion'
import { RunNode, sizeOf, type Branch, type ViewMode } from './RunNode'
import { ACTIVITY, nameOf } from '../lib/format'

// The centre of the window: §10's run graph.
//
// "Generated layout, never freehand. The user forks, collapses and navigates;
// they do not place nodes. A layout pass plus a pan/zoom transform, no drawing
// library (§19)." That is exactly what this is — `layout.ts` returns
// coordinates, and everything here is one CSS transform over them.
//
// ## Two representations, one canvas
//
// `focus` is the default and `map` is ⌘\. The record is explicit that these are
// "two representations of one canvas", and this file takes that literally
// rather than structurally:
//
// - **One camera.** There is a single `view` — x, y and scale — and it is never
//   reset, stored per mode, or recomputed from scratch when the mode changes.
//   ⌘\ re-aims *that* camera at the same node and eases it there, which is why
//   the toggle reads as a zoom rather than as a screen change. Two cameras
//   would drift apart the first time either was panned.
// - **One layout pass.** `layout()` is called once per render with a `sizeOf`
//   that answers 560×620 for a column and 560×132 for a card. The width is the
//   same in both, so a node keeps its column and only the vertical packing
//   tightens; nothing slides sideways under the toggle.
// - **One node component.** `RunNode` draws a conversation column or §10's
//   shape card from the same run, the same projection and the same three-word
//   vocabulary.
//
// ## What the camera is *for*
//
// Everything that moves it — ⌘\, ⌘[ / ⌘], and the fork motion — ends in
// `centreOn` from `motion.ts`, so "look at this" has exactly one meaning here.

/** How far outside the window a node still counts as seen, in screen pixels. */
const VISIBLE_MARGIN = 600

const MIN_SCALE = 0.35
const MAX_SCALE = 1.6
const PAD = 48

export type { Viewport } from './motion'

export function Canvas({
  runs,
  feeds,
  activity,
  forkPoints,
  selected,
  live,
  arriving,
  onVisible,
  onSelect,
  onSend,
  onFork,
  onForkUnavailable,
  onArrived,
  viewportRef,
}: {
  runs: Run[]
  /** §7's blocks per run, projected by `document-model` — never derived here. */
  feeds: Record<RunId, Feed>
  activity: Record<RunId, RunActivity>
  /** Which blocks can be forked at, per run — a projection, never derived here. */
  forkPoints: Record<RunId, ForkPoints>
  selected: RunId | null
  /** Runs this process is supervising — the ones a composer can send to. */
  live: RunId[]
  /** A child that has just been forked, and is about to appear on the graph. */
  arriving: RunId | null
  /** Which runs the camera can currently see — see `visible` below. */
  onVisible: (runs: RunId[]) => void
  onSelect: (id: RunId) => void
  onSend: (id: RunId, text: string) => Promise<void>
  onFork: (parent: RunId, seq: number) => void
  /** ⌘⇧F with nothing forkable under the pointer. */
  onForkUnavailable: () => void
  /** The fork motion has landed; focus belongs to the child now. */
  onArrived: (child: RunId) => void
  /** Filled with the imperative handle, so the palette can drive the view. */
  viewportRef: React.MutableRefObject<CanvasHandle | null>
}) {
  const frame = useRef<HTMLDivElement | null>(null)
  const [box, setBox] = useState({ width: 0, height: 0 })
  const [mode, setMode] = useState<ViewMode>('focus')
  const [view, setView] = useState<Viewport>({ x: PAD, y: PAD, scale: 1 })

  // Where each run's branch points rendered inside it, in node-local pixels.
  // Measured by the node (only the DOM knows) and handed to the layout pass
  // (only the layout pass may place a wire).
  const [anchors, setAnchors] = useState<Record<RunId, Record<number, number>>>({})
  const [hovered, setHovered] = useState<{ run: RunId; seq: number } | null>(null)
  const [peel, setPeel] = useState<{ child: RunId; from: Box; landed: boolean } | null>(null)

  const viewRef = useRef(view)
  viewRef.current = view

  const anchorOf = useCallback(
    (run: Run, seq: number) => anchors[run.id]?.[seq] ?? null,
    [anchors],
  )
  // Memoized, and not only for speed: the nodes this returns are read by the
  // measurement effects inside every node, so a fresh array on every render
  // would re-measure the whole graph on every render.
  const graph = useMemo(
    () =>
      layout(
        runs,
        sizeOf(mode),
        // Map view has no feed to measure, so its wires fall back to the node
        // anchor. Reusing a focus-view measurement here would place the wire
        // against a height the card does not have.
        mode === 'map' ? undefined : anchorOf,
      ),
    [runs, mode, anchorOf],
  )
  const placedOf = useMemo(() => {
    const byId = new Map<RunId, Placed>()
    for (const node of graph.nodes) byId.set(node.run.id, node)
    return byId
  }, [graph.nodes])

  // ------------------------------------------------------------ the camera --

  const anim = useRef<number | null>(null)
  const halt = useCallback(() => {
    if (anim.current !== null) cancelAnimationFrame(anim.current)
    anim.current = null
  }, [])

  /** Ease the one camera to a new position. Nothing else writes `view`. */
  const rideTo = useCallback(
    (target: Viewport, ms: number, ease: (t: number) => number = easeInOut) => {
      halt()
      const from = viewRef.current
      const to = { ...target, scale: clamp(target.scale) }
      const t0 = performance.now()
      const step = (now: number) => {
        const t = ms <= 0 ? 1 : Math.min(1, (now - t0) / ms)
        setView(lerpView(from, to, ease(t)))
        anim.current = t < 1 ? requestAnimationFrame(step) : null
      }
      anim.current = requestAnimationFrame(step)
    },
    [halt],
  )

  useEffect(() => halt, [halt])

  const fit = useCallback(() => {
    halt()
    if (box.width === 0 || graph.width === 0) {
      setView({ x: PAD, y: PAD, scale: 1 })
      return
    }
    // Never magnify: a single node blown up to fill the window looks like a
    // bug. Fit means "everything is on screen", not "everything is large".
    const s = Math.max(MIN_SCALE, fitScale(graphBox(graph), box, PAD, 1))
    // Anchored left, centred vertically. A graph reads left to right from its
    // roots, so centring it horizontally leaves a small one adrift in the
    // middle of the window and moves the roots every time a branch is added.
    setView({ x: PAD, y: Math.max(PAD, (box.height - graph.height * s) / 2), scale: s })
  }, [box, graph.width, graph.height, halt])

  const zoomBy = useCallback(
    (factor: number) => {
      halt()
      setView((v) => {
        const scale = clamp(v.scale * factor)
        if (box.width === 0) return { ...v, scale }
        // Zoom about the centre of the viewport, which is where the user is
        // looking when they reach for a keyboard zoom rather than the wheel.
        const cx = box.width / 2
        const cy = box.height / 2
        const k = scale / v.scale
        return { x: cx - (cx - v.x) * k, y: cy - (cy - v.y) * k, scale }
      })
    },
    [box, halt],
  )

  /** Put a node in the middle of the window, at a scale this view can hold. */
  const travelTo = useCallback(
    (id: RunId | null, scale: number, ms: number) => {
      const node = id === null ? undefined : placedOf.get(id)
      if (node === undefined || box.width === 0) return
      rideTo(centreOn(boxOf(node), box, scale), ms)
    },
    [box, placedOf, rideTo],
  )

  // ----------------------------------------------------------- the two views --

  // ⌘\. The camera is not replaced — it is re-aimed at the node the reader was
  // already on, which is what makes the toggle a zoom rather than a cut. Focus
  // lands at 1:1 on that node; map pulls back far enough to hold the whole
  // graph, because the point of the card is seeing the branches beside each
  // other.
  const crossed = useRef<ViewMode>(mode)
  useLayoutEffect(() => {
    if (crossed.current === mode || box.width === 0) {
      crossed.current = mode
      return
    }
    crossed.current = mode
    const anchor = selected !== null && placedOf.has(selected) ? selected : graph.nodes[0]?.run.id
    if (anchor === undefined) return
    const scale =
      mode === 'map'
        ? Math.max(MIN_SCALE, fitScale(graphBox(graph), box, PAD, 1))
        : 1
    const node = placedOf.get(anchor)
    if (node !== undefined) rideTo(centreOn(boxOf(node), box, scale), VIEW_MS)
  }, [mode, box, graph, placedOf, rideTo, selected])

  // ------------------------------------------------------------ fork motion --

  // **Variant B, split in place** (`branching-surface.md`, from the prototype).
  // The child starts translated onto the parent at .88 and peels out of it,
  // while the camera pulls back to hold both at no more than .78; at ~900ms the
  // motion is over and focus lands on the child. The numbers are the record's.
  //
  // A CSS transition rather than a per-frame loop: the transform has exactly
  // two states — on the parent, and in place — and the browser owns everything
  // between them, which is one fewer animation clock to keep in step with the
  // camera's.
  const peeled = useRef<RunId | null>(null)
  const timers = useRef<{ frame: number; landing: number } | null>(null)
  useEffect(() => {
    if (arriving === null) {
      peeled.current = null
      return
    }
    if (peeled.current === arriving) return
    const child = placedOf.get(arriving)
    // The child's row reaches the graph a refresh after the fork returns; until
    // then there is nothing to move, and this effect runs again when it does.
    if (child === undefined) return
    peeled.current = arriving

    const parentId = child.run.parent_run_id
    const parent = parentId === null ? undefined : placedOf.get(parentId)
    if (parent === undefined || box.width === 0) {
      onArrived(arriving)
      return
    }

    setPeel({ child: arriving, from: boxOf(parent), landed: false })
    const both = union(boxOf(parent), boxOf(child))
    rideTo(centreOn(both, box, fitScale(both, box, PAD, FORK_FIT_SCALE)), FORK_MS, easeOutCubic)

    // One frame on the parent, then the transition to its own place.
    //
    // The timers live in a ref and are **not** torn down by this effect's
    // cleanup, which would be the obvious thing and is wrong here: the effect
    // re-runs whenever the graph does, and a cleanup would cancel the landing
    // of a motion that is still playing. The guard above is what makes it run
    // once; a `clearTimeout` on every re-render would make it run never.
    timers.current = {
      frame: requestAnimationFrame(() =>
        setPeel((p) => (p === null ? p : { ...p, landed: true })),
      ),
      landing: window.setTimeout(() => {
        setPeel((p) => (p !== null && p.child === arriving ? null : p))
        onArrived(arriving)
      }, FORK_MS),
    }
  }, [arriving, placedOf, box, rideTo, onArrived])

  useEffect(
    () => () => {
      if (timers.current === null) return
      cancelAnimationFrame(timers.current.frame)
      clearTimeout(timers.current.landing)
    },
    [],
  )

  // -------------------------------------------------------------- the input --

  useLayoutEffect(() => {
    const el = frame.current
    if (el === null) return
    const measure = () => {
      const r = el.getBoundingClientRect()
      setBox({ width: r.width, height: r.height })
    }
    measure()
    const observer = new ResizeObserver(measure)
    observer.observe(el)
    return () => observer.disconnect()
  }, [])

  // Fit once, when there is something to fit. Not on every change: a graph
  // that re-centres itself while a run streams events is unusable.
  const fitted = useRef(false)
  useLayoutEffect(() => {
    if (fitted.current || graph.nodes.length === 0 || box.width === 0) return
    fitted.current = true
    fit()
  }, [fit, graph.nodes.length, box.width])

  /** Siblings of the selected run, oldest first — what ⌘[ / ⌘] walk. */
  const siblings = useMemo(() => {
    const current = runs.find((r) => r.id === selected)
    if (current === undefined) return []
    return graph.nodes
      .filter((n) => n.run.parent_run_id === current.parent_run_id)
      .map((n) => n.run)
  }, [graph.nodes, runs, selected])

  const step = useCallback(
    (delta: number) => {
      if (siblings.length < 2 || selected === null) return
      const i = siblings.findIndex((r) => r.id === selected)
      if (i < 0) return
      const next = siblings[(i + delta + siblings.length) % siblings.length]
      if (next === undefined) return
      onSelect(next.id)
      travelTo(next.id, viewRef.current.scale, TRAVEL_MS)
    },
    [siblings, selected, onSelect, travelTo],
  )

  const forkHovered = useCallback(() => {
    // **Never a nearby block.** The hover state is only ever set on a block the
    // `fork_points` projection offered, so "nothing is hovered" is answered by
    // saying so rather than by picking the last address that resolved.
    if (hovered === null) {
      onForkUnavailable()
      return
    }
    onFork(hovered.run, hovered.seq)
  }, [hovered, onFork, onForkUnavailable])

  const toggleMode = useCallback(() => setMode((m) => (m === 'map' ? 'focus' : 'map')), [])

  /**
   * Which runs the camera can see, plus a margin of roughly one node.
   *
   * Every column stays mounted — the record's focus view keeps them all, and
   * this does not change that. What it reports is which of them are worth
   * *projecting*: a feed is thousands of blocks, and a two-hundred-node window
   * of them is most of the log on a surface where the reader is looking at two.
   * The margin is what stops a feed arriving only after the column is already
   * under the eye.
   */
  const seen = useMemo(() => {
    if (box.width === 0) return ''
    const ids: RunId[] = []
    for (const node of graph.nodes) {
      const x1 = view.x + node.x * view.scale
      const y1 = view.y + node.y * view.scale
      const x2 = x1 + node.w * view.scale
      const y2 = y1 + node.h * view.scale
      if (
        x2 > -VISIBLE_MARGIN &&
        x1 < box.width + VISIBLE_MARGIN &&
        y2 > -VISIBLE_MARGIN &&
        y1 < box.height + VISIBLE_MARGIN
      ) {
        ids.push(node.run.id)
      }
    }
    // Joined rather than returned as an array: this recomputes on every frame
    // of every pan, and a fresh array each time would re-run the effect below
    // sixty times a second to report the same answer.
    return ids.sort().join(',')
  }, [graph.nodes, view, box])

  useEffect(() => {
    onVisible(seen === '' ? [] : seen.split(','))
  }, [seen, onVisible])

  viewportRef.current = {
    fit,
    zoomIn: () => zoomBy(1.2),
    zoomOut: () => zoomBy(1 / 1.2),
    mode,
    toggleMode,
    forkHovered,
    sibling: step,
  }

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (!(e.metaKey || e.ctrlKey)) return
      const key = e.key.toLowerCase()
      if (key === '\\') {
        e.preventDefault()
        toggleMode()
      } else if (key === 'f' && e.shiftKey) {
        e.preventDefault()
        forkHovered()
      } else if (key === '[') {
        e.preventDefault()
        step(-1)
      } else if (key === ']') {
        e.preventDefault()
        step(1)
      }
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [toggleMode, forkHovered, step])

  // Wheel: pan, or zoom under the pointer when a modifier is held — the
  // convention every map and every canvas already shares.
  useEffect(() => {
    const el = frame.current
    if (el === null) return
    const onWheel = (e: WheelEvent) => {
      // A node is a conversation column with a scrolling body, and this
      // handler is on the frame every one of them bubbles to. Without the
      // check the canvas pans and the body never scrolls at all — the gesture
      // is swallowed one level above the element it was aimed at.
      const target = e.target
      if (
        !(e.ctrlKey || e.metaKey) &&
        target instanceof Element &&
        target.closest('[data-scrollable]') !== null
      ) {
        return
      }
      e.preventDefault()
      halt()
      if (e.ctrlKey || e.metaKey) {
        const r = el.getBoundingClientRect()
        const px = e.clientX - r.left
        const py = e.clientY - r.top
        setView((v) => {
          const scale = clamp(v.scale * Math.exp(-e.deltaY / 320))
          const k = scale / v.scale
          return { x: px - (px - v.x) * k, y: py - (py - v.y) * k, scale }
        })
      } else {
        setView((v) => ({ ...v, x: v.x - e.deltaX, y: v.y - e.deltaY }))
      }
    }
    // Not passive: the browser's own zoom and rubber-banding would otherwise
    // win the gesture before this handler ran.
    el.addEventListener('wheel', onWheel, { passive: false })
    return () => el.removeEventListener('wheel', onWheel)
  }, [halt])

  const onPointerDown = (e: React.PointerEvent<HTMLDivElement>) => {
    if (e.button !== 0) return
    halt()
    const el = e.currentTarget
    el.setPointerCapture(e.pointerId)
    const start = { x: e.clientX, y: e.clientY, vx: view.x, vy: view.y }
    const move = (ev: PointerEvent) =>
      setView((v) => ({
        ...v,
        x: start.vx + (ev.clientX - start.x),
        y: start.vy + (ev.clientY - start.y),
      }))
    const up = () => {
      el.releasePointerCapture(e.pointerId)
      el.removeEventListener('pointermove', move)
      el.removeEventListener('pointerup', up)
    }
    el.addEventListener('pointermove', move)
    el.addEventListener('pointerup', up)
  }

  // ------------------------------------------------------------- the render --

  const measurers = useRef(new Map<RunId, (a: Record<number, number>) => void>())
  const measurerFor = (id: RunId) => {
    const known = measurers.current.get(id)
    if (known !== undefined) return known
    const fn = (next: Record<number, number>) =>
      setAnchors((prev) => {
        const current = prev[id]
        if (current !== undefined && same(current, next)) return prev
        return { ...prev, [id]: next }
      })
    measurers.current.set(id, fn)
    return fn
  }

  // Memoized for the same reason the layout is: each node measures the branch
  // points it is handed, so a fresh array per render is a measurement per
  // render of a graph that did not move.
  const branchesByParent = useMemo(() => {
    const out = new Map<RunId, Branch[]>()
    for (const node of graph.nodes) {
      const parent = node.run.parent_run_id
      const seq = node.run.branch_point_seq
      if (parent === null || seq === null) continue
      const known = out.get(parent)
      if (known === undefined) out.set(parent, [{ seq, child: node.run }])
      else known.push({ seq, child: node.run })
    }
    return out
  }, [graph.nodes])

  /**
   * What a fork at a `seq` of this run would inherit, for that run's own feed.
   *
   * Read off `EventLog::fork_points` rather than counted here, exactly as the
   * strip below is: the two numbers sit beside each other on the canvas — a
   * node's header says `N msgs`, its child says it inherited M of them — and two
   * counting rules would make one of them wrong.
   */
  const inheritedIn = (id: RunId) => (seq: number) =>
    forkPoints[id]?.blocks.find((b) => b.seq === seq)?.inherited_messages ?? null

  /** What a child inherited, read off the parent's projection — never counted here. */
  const inheritedBy = (run: Run): number | null => {
    if (run.parent_run_id === null || run.branch_point_seq === null) return null
    const block = forkPoints[run.parent_run_id]?.blocks.find(
      (b) => b.seq === run.branch_point_seq,
    )
    return block?.inherited_messages ?? null
  }

  return (
    <div
      ref={frame}
      onPointerDown={onPointerDown}
      onDoubleClick={fit}
      className="relative min-w-0 flex-1 cursor-grab overflow-hidden bg-bg active:cursor-grabbing"
      style={{
        // The dot grid is what makes panning legible: without it, dragging an
        // empty region looks like nothing happening at all.
        backgroundImage: 'radial-gradient(var(--color-border) 1px, transparent 1px)',
        backgroundSize: `${24 * view.scale}px ${24 * view.scale}px`,
        backgroundPosition: `${view.x}px ${view.y}px`,
      }}
    >
      {graph.nodes.length === 0 ? (
        <Empty />
      ) : (
        <div
          className="absolute top-0 left-0 origin-top-left"
          style={{
            transform: `translate(${view.x}px, ${view.y}px) scale(${view.scale})`,
            width: graph.width,
            height: graph.height,
          }}
        >
          <svg
            className="pointer-events-none absolute inset-0 overflow-visible"
            width={graph.width}
            height={graph.height}
            aria-hidden="true"
          >
            {graph.edges.map((edge) => (
              <path
                key={edge.id}
                d={edge.d}
                fill="none"
                // A wire whose parent end was measured leaves the forked
                // message; one that was not leaves the node. They are drawn
                // differently so the difference is visible rather than implied.
                stroke={edge.measured ? 'var(--color-alt)' : 'var(--color-elevated)'}
                strokeOpacity={edge.measured ? 0.7 : 1}
                strokeWidth={1.5}
              />
            ))}
          </svg>

          {graph.nodes.map((node) => {
            const peeling = peel !== null && peel.child === node.run.id
            const onParent = peeling && !peel.landed
            return (
              <div
                key={node.run.id}
                className="absolute"
                style={{
                  left: node.x,
                  top: node.y,
                  width: node.w,
                  height: node.h,
                  transformOrigin: 'top left',
                  // Split in place: the child is drawn *on* its parent and
                  // peels out to its own position.
                  transform: onParent
                    ? `translate(${peel.from.x - node.x}px, ${peel.from.y - node.y}px) scale(${FORK_CHILD_SCALE})`
                    : undefined,
                  opacity: onParent ? 0.4 : 1,
                  transition: peeling
                    ? `transform ${FORK_MS}ms cubic-bezier(.2,.75,.2,1), opacity ${FORK_MS}ms ease-out`
                    : undefined,
                  zIndex: peeling ? 10 : undefined,
                }}
              >
                <RunNode
                  run={node.run}
                  mode={mode}
                  activity={activity[node.run.id] ?? null}
                  feed={feeds[node.run.id] ?? null}
                  inheritedAt={inheritedIn(node.run.id)}
                  branches={branchesByParent.get(node.run.id) ?? EMPTY_BRANCHES}
                  parent={
                    node.run.parent_run_id === null
                      ? null
                      : (placedOf.get(node.run.parent_run_id)?.run ?? null)
                  }
                  inherited={inheritedBy(node.run)}
                  selected={selected === node.run.id}
                  live={live.includes(node.run.id)}
                  onSelect={() => onSelect(node.run.id)}
                  onSend={(text) => onSend(node.run.id, text)}
                  onHoverBlock={(seq) =>
                    setHovered(seq === null ? null : { run: node.run.id, seq })
                  }
                  onFork={(seq) => onFork(node.run.id, seq)}
                  onMeasure={measurerFor(node.run.id)}
                />
              </div>
            )
          })}
        </div>
      )}

      <Slivers
        siblings={siblings}
        selected={selected}
        placedOf={placedOf}
        activity={activity}
        view={view}
        box={box}
        onSelect={(id) => {
          onSelect(id)
          travelTo(id, viewRef.current.scale, TRAVEL_MS)
        }}
      />

      <Controls
        view={view}
        mode={mode}
        onMode={toggleMode}
        onFit={fit}
        onIn={() => zoomBy(1.2)}
        onOut={() => zoomBy(1 / 1.2)}
      />
    </div>
  )
}

export type CanvasHandle = {
  fit: () => void
  zoomIn: () => void
  zoomOut: () => void
  mode: ViewMode
  toggleMode: () => void
  forkHovered: () => void
  sibling: (delta: number) => void
}

/** One shared empty list, so a childless node is handed the same one twice. */
const EMPTY_BRANCHES: Branch[] = []

const clamp = (scale: number) => Math.max(MIN_SCALE, Math.min(MAX_SCALE, scale))

/** The whole graph as a box, for the fits that are about all of it. */
const graphBox = (graph: { width: number; height: number }): Box => ({
  x: 0,
  y: 0,
  w: graph.width,
  h: graph.height,
})

const same = (a: Record<number, number>, b: Record<number, number>): boolean => {
  const ka = Object.keys(a)
  const kb = Object.keys(b)
  return ka.length === kb.length && ka.every((k) => a[Number(k)] === b[Number(k)])
}

/**
 * Edge slivers for off-screen siblings.
 *
 * Carried over from the prototype without argument (`branching-surface.md`),
 * and it is the part of the canvas that answers *what am I not looking at*. A
 * sibling that has been panned out of the window is a branch the reader is
 * comparing against, so its status is on the sliver: the three-word vocabulary
 * propagates to "border, pill, sliver and card", and this is the sliver.
 */
function Slivers({
  siblings,
  selected,
  placedOf,
  activity,
  view,
  box,
  onSelect,
}: {
  siblings: Run[]
  selected: RunId | null
  placedOf: Map<RunId, Placed>
  activity: Record<RunId, RunActivity>
  view: Viewport
  box: { width: number; height: number }
  onSelect: (id: RunId) => void
}) {
  if (box.width === 0) return null
  const tabs = siblings.flatMap((run) => {
    if (run.id === selected) return []
    const node = placedOf.get(run.id)
    if (node === undefined) return []
    const x1 = view.x + node.x * view.scale
    const y1 = view.y + node.y * view.scale
    const x2 = x1 + node.w * view.scale
    const y2 = y1 + node.h * view.scale
    if (x2 > 0 && x1 < box.width && y2 > 0 && y1 < box.height) return []
    const cy = Math.max(28, Math.min(box.height - 28, (y1 + y2) / 2))
    const cx = Math.max(28, Math.min(box.width - 28, (x1 + x2) / 2))
    const side = x2 <= 0 ? 'left' : x1 >= box.width ? 'right' : y2 <= 0 ? 'top' : 'bottom'
    return [{ run, side, cy, cx }]
  })

  return (
    <>
      {tabs.map(({ run, side, cy, cx }) => {
        const tone = ACTIVITY[activity[run.id]?.status ?? 'idle']
        const vertical = side === 'left' || side === 'right'
        return (
          <button
            key={run.id}
            type="button"
            onPointerDown={(e) => e.stopPropagation()}
            onClick={() => onSelect(run.id)}
            title={`${nameOf(run)} — ${tone.label} (off screen)`}
            style={
              vertical
                ? { top: cy, [side]: 0, transform: 'translateY(-50%)' }
                : { left: cx, [side]: 0, transform: 'translateX(-50%)' }
            }
            className={[
              'absolute flex items-center justify-center rounded-sm text-micro',
              vertical ? 'h-12 w-1.5' : 'h-1.5 w-12',
              tone.dot,
            ].join(' ')}
            aria-label={`Go to ${nameOf(run)}`}
          />
        )
      })}
    </>
  )
}

function Empty() {
  return (
    <div className="pointer-events-none absolute inset-0 flex flex-col items-center justify-center gap-1 text-center">
      <p className="text-muted">No runs yet</p>
      <p className="text-micro text-faint">
        Start one from the title bar, or press ⌘K. Each run becomes a node here.
      </p>
    </div>
  )
}

function Controls({
  view,
  mode,
  onMode,
  onFit,
  onIn,
  onOut,
}: {
  view: Viewport
  mode: ViewMode
  onMode: () => void
  onFit: () => void
  onIn: () => void
  onOut: () => void
}) {
  return (
    <div className="absolute right-3 bottom-3 flex items-center gap-2">
      <button
        type="button"
        onPointerDown={(e) => e.stopPropagation()}
        onClick={onMode}
        title="Switch between the conversation columns and §10’s shape cards (⌘\)"
        className="h-6 rounded-md border border-border bg-panel px-2 font-mono text-micro text-muted hover:bg-hover hover:text-text"
      >
        {mode === 'map' ? 'map ⌘\\' : 'focus ⌘\\'}
      </button>
      <div className="flex items-center overflow-hidden rounded-md border border-border bg-panel">
        <Button onClick={onOut} label="Zoom out">
          −
        </Button>
        <button
          type="button"
          onPointerDown={(e) => e.stopPropagation()}
          onClick={onFit}
          title="Fit to graph (double-click the canvas)"
          className="h-6 w-14 border-x border-border font-mono text-micro text-muted hover:bg-hover hover:text-text"
        >
          {Math.round(view.scale * 100)}%
        </button>
        <Button onClick={onIn} label="Zoom in">
          +
        </Button>
      </div>
    </div>
  )
}

function Button({
  onClick,
  label,
  children,
}: {
  onClick: () => void
  label: string
  children: React.ReactNode
}) {
  return (
    <button
      type="button"
      onPointerDown={(e) => e.stopPropagation()}
      onClick={onClick}
      title={label}
      aria-label={label}
      className="flex size-6 items-center justify-center text-muted hover:bg-hover hover:text-text"
    >
      {children}
    </button>
  )
}
