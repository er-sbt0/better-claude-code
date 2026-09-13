import { useCallback, useEffect, useLayoutEffect, useRef, useState } from 'react'
import type { RawEvent } from '@agent-workspace/shared-types/RawEvent'
import type { Run } from '@agent-workspace/shared-types/Run'
import type { RunId } from '@agent-workspace/shared-types/RunId'
import type { RunActivity } from '@agent-workspace/shared-types/RunActivity'
import { layout } from './layout'
import { RunNode, sizeOf } from './RunNode'

// The centre of the window: §10's run graph.
//
// "Generated layout, never freehand. The user forks, collapses and navigates;
// they do not place nodes. A layout pass plus a pan/zoom transform, no drawing
// library (§19)." That is exactly what this is — `layout.ts` returns
// coordinates, and everything here is one CSS transform over them.
//
// "Collapses" is the one word of that which has moved. `branching-surface.md`
// makes a node a conversation column and relocates §10's shape card to **map
// view**, a second representation of this same canvas reached with ⌘\. Map view
// is not built: when it is, it changes the `sizeOf` this file passes to
// `layout` and the component it renders, and nothing else here.
//
// What is *not* here yet, and is not hidden: forking. Nothing in the app
// creates a child run, so `parent_run_id` is null on every row and the graph
// draws a forest of roots. The edges are implemented because the data model
// carries them (§10: "edges are ours") — they will appear the day a fork does,
// with no change to this file.

const MIN_SCALE = 0.35
const MAX_SCALE = 1.6
const PAD = 48

export type Viewport = { x: number; y: number; scale: number }

export function Canvas({
  runs,
  events,
  activity,
  selected,
  live,
  onSelect,
  onSend,
  viewportRef,
}: {
  runs: Run[]
  events: Record<RunId, RawEvent[]>
  activity: Record<RunId, RunActivity>
  selected: RunId | null
  /** Runs this process is supervising — the ones a composer can send to. */
  live: RunId[]
  onSelect: (id: RunId) => void
  onSend: (id: RunId, text: string) => Promise<void>
  /** Filled with the imperative handle, so the palette can drive the view. */
  viewportRef: React.MutableRefObject<CanvasHandle | null>
}) {
  const frame = useRef<HTMLDivElement | null>(null)
  const [view, setView] = useState<Viewport>({ x: PAD, y: PAD, scale: 1 })
  const graph = layout(runs, sizeOf)

  const fit = useCallback(() => {
    const box = frame.current?.getBoundingClientRect()
    if (box === undefined || graph.width === 0) {
      setView({ x: PAD, y: PAD, scale: 1 })
      return
    }
    // Never magnify: a single node blown up to fill the window looks like a
    // bug. Fit means "everything is on screen", not "everything is large".
    const scale = Math.min(
      1,
      (box.width - PAD * 2) / graph.width,
      (box.height - PAD * 2) / graph.height,
    )
    const s = Math.max(MIN_SCALE, scale)
    // Anchored left, centred vertically. A graph reads left to right from its
    // roots, so centring it horizontally leaves a small one adrift in the
    // middle of the window and moves the roots every time a branch is added.
    setView({ x: PAD, y: Math.max(PAD, (box.height - graph.height * s) / 2), scale: s })
  }, [graph.width, graph.height])

  const zoomBy = useCallback((factor: number) => {
    const box = frame.current?.getBoundingClientRect()
    setView((v) => {
      const scale = clamp(v.scale * factor)
      if (box === undefined) return { ...v, scale }
      // Zoom about the centre of the viewport, which is where the user is
      // looking when they reach for a keyboard zoom rather than the wheel.
      const cx = box.width / 2
      const cy = box.height / 2
      const k = scale / v.scale
      return { x: cx - (cx - v.x) * k, y: cy - (cy - v.y) * k, scale }
    })
  }, [])

  viewportRef.current = { fit, zoomIn: () => zoomBy(1.2), zoomOut: () => zoomBy(1 / 1.2) }

  // Fit once, when there is something to fit. Not on every change: a graph
  // that re-centres itself while a run streams events is unusable.
  const fitted = useRef(false)
  useLayoutEffect(() => {
    if (fitted.current || graph.nodes.length === 0) return
    fitted.current = true
    fit()
  }, [fit, graph.nodes.length])

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
      if (e.ctrlKey || e.metaKey) {
        const box = el.getBoundingClientRect()
        const px = e.clientX - box.left
        const py = e.clientY - box.top
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
  }, [])

  const onPointerDown = (e: React.PointerEvent<HTMLDivElement>) => {
    if (e.button !== 0) return
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
                stroke="var(--color-elevated)"
                strokeWidth={1.5}
              />
            ))}
          </svg>

          {graph.nodes.map((node) => (
            <div
              key={node.run.id}
              className="absolute"
              style={{ left: node.x, top: node.y, width: node.w, height: node.h }}
            >
              <RunNode
                run={node.run}
                activity={activity[node.run.id] ?? null}
                events={events[node.run.id] ?? []}
                selected={selected === node.run.id}
                live={live.includes(node.run.id)}
                onSelect={() => onSelect(node.run.id)}
                onSend={(text) => onSend(node.run.id, text)}
              />
            </div>
          ))}
        </div>
      )}

      <Controls view={view} onFit={fit} onIn={() => zoomBy(1.2)} onOut={() => zoomBy(1 / 1.2)} />
    </div>
  )
}

export type CanvasHandle = { fit: () => void; zoomIn: () => void; zoomOut: () => void }

const clamp = (scale: number) => Math.max(MIN_SCALE, Math.min(MAX_SCALE, scale))

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
  onFit,
  onIn,
  onOut,
}: {
  view: Viewport
  onFit: () => void
  onIn: () => void
  onOut: () => void
}) {
  return (
    <div className="absolute right-3 bottom-3 flex items-center overflow-hidden rounded-md border border-border bg-panel">
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
