import { useEffect, useRef, useState } from 'react'
import { createRoot } from 'react-dom/client'
import { Canvas, type CanvasHandle } from '../canvas/Canvas'
import { activity, feed, forkPoints, runs } from './synthetic'
import '../styles.css'

// ---------------------------------------------------------------------------
// The WebKitGTK budget measurement (§4, §2's first item), as a page.
//
// It mounts the **real** canvas, the real conversation columns and the real
// virtualized feed, over synthetic runs big enough to ask the question §4 was
// re-priced for: not one linear feed, but N columns each holding a virtualized
// feed while the camera pans and zooms.
//
// It drives the camera by dispatching wheel events at the canvas rather than by
// calling its internals, so what is measured is the path a user's gesture takes
// — handler, layout pass, transform, and every mounted feed's response to it.
//
// Run it with `docs/verification/feed-budget-spike.sh`, which opens this page in
// a WebKitGTK window (the engine Tauri borrows on Linux, §4) and prints the
// numbers. `?columns=6&blocks=5000` is the shape of the question.
// ---------------------------------------------------------------------------

/** Frames to discard before recording: mount, first layout, first paint. */
const WARMUP = 30

type Phase = { name: string; frames: number; dx: number; dy: number; zoom: boolean }

/**
 * The gesture script.
 *
 * Pan first and zoom second, because they cost differently: panning changes one
 * transform and asks every mounted feed nothing, while zooming changes the
 * rasterization of every column at once — which is the case WebKitGTK is
 * weakest at (§4) and the one §2 said could invalidate a stack decision.
 */
const SCRIPT: Phase[] = [
  { name: 'pan-right', frames: 90, dx: 26, dy: 0, zoom: false },
  { name: 'pan-down', frames: 60, dx: 0, dy: 22, zoom: false },
  { name: 'pan-back', frames: 90, dx: -26, dy: -8, zoom: false },
  { name: 'zoom-out', frames: 60, dx: 0, dy: 14, zoom: true },
  { name: 'zoom-in', frames: 60, dx: 0, dy: -14, zoom: true },
]

type Report = {
  columns: number
  blocks: number
  mountMs: number
  frames: number
  fps: number
  meanMs: number
  medianMs: number
  p95Ms: number
  p99Ms: number
  maxMs: number
  over16ms: number
  over33ms: number
  perPhase: Record<string, { meanMs: number; p95Ms: number; over33ms: number }>
  domNodes: number
  /** True if the page was throttled behind another window while measuring. */
  hidden: boolean
  userAgent: string
}

function percentile(sorted: number[], p: number): number {
  if (sorted.length === 0) return 0
  const i = Math.min(sorted.length - 1, Math.floor((p / 100) * sorted.length))
  return sorted[i]!
}

const round = (n: number) => Math.round(n * 100) / 100

function Bench() {
  const params = new URLSearchParams(location.search)
  const columns = Number(params.get('columns') ?? 6)
  const blocks = Number(params.get('blocks') ?? 5000)

  const host = useRef<HTMLDivElement | null>(null)
  const viewportRef = useRef<CanvasHandle | null>(null)
  const [report, setReport] = useState<Report | null>(null)
  const mountedAt = useRef(performance.now())

  const all = useRef(runs(columns)).current
  const feeds = useRef(Object.fromEntries(all.map((r) => [r.id, feed(r.id, blocks)]))).current
  const acts = useRef(activity(all)).current
  const forks = useRef(forkPoints(all, blocks)).current

  useEffect(() => {
    const frame = host.current?.firstElementChild
    if (!(frame instanceof HTMLElement)) return
    const mountMs = performance.now() - mountedAt.current

    const samples: { phase: string; ms: number }[] = []
    let phase = 0
    let left = SCRIPT[0]!.frames + WARMUP
    let last = performance.now()
    let warm = WARMUP

    const step = (now: number) => {
      const ms = now - last
      last = now
      const current = SCRIPT[phase]!
      if (warm > 0) warm--
      else samples.push({ phase: current.name, ms })

      // One gesture per frame, at the element the canvas listens on. `bubbles`
      // matters: the handler is on the frame and the wheel lands on it.
      frame.dispatchEvent(
        new WheelEvent('wheel', {
          bubbles: true,
          cancelable: true,
          deltaX: current.zoom ? 0 : current.dx,
          deltaY: current.zoom ? current.dy : current.dy,
          ctrlKey: current.zoom,
          clientX: frame.clientWidth / 2,
          clientY: frame.clientHeight / 2,
        }),
      )

      left--
      if (left <= 0) {
        phase++
        if (phase >= SCRIPT.length) {
          finish(samples, mountMs)
          return
        }
        left = SCRIPT[phase]!.frames
      }
      requestAnimationFrame(step)
    }

    const finish = (taken: { phase: string; ms: number }[], mount: number) => {
      const all = taken.map((s) => s.ms).sort((a, b) => a - b)
      const perPhase: Report['perPhase'] = {}
      for (const { name } of SCRIPT) {
        const ms = taken
          .filter((s) => s.phase === name)
          .map((s) => s.ms)
          .sort((a, b) => a - b)
        perPhase[name] = {
          meanMs: round(ms.reduce((a, b) => a + b, 0) / Math.max(1, ms.length)),
          p95Ms: round(percentile(ms, 95)),
          over33ms: ms.filter((m) => m > 33.3).length,
        }
      }
      const mean = all.reduce((a, b) => a + b, 0) / Math.max(1, all.length)
      const result: Report = {
        columns,
        blocks,
        mountMs: round(mount),
        frames: all.length,
        fps: round(1000 / mean),
        meanMs: round(mean),
        medianMs: round(percentile(all, 50)),
        p95Ms: round(percentile(all, 95)),
        p99Ms: round(percentile(all, 99)),
        maxMs: round(all[all.length - 1] ?? 0),
        over16ms: all.filter((m) => m > 16.7).length,
        over33ms: all.filter((m) => m > 33.3).length,
        perPhase,
        domNodes: document.getElementsByTagName('*').length,
        hidden: document.hidden,
        userAgent: navigator.userAgent,
      }
      setReport(result)
      // The harness reads this; a plain browser shows the panel below instead.
      post(result)
    }

    const id = requestAnimationFrame(step)
    return () => cancelAnimationFrame(id)
  }, [columns, blocks])

  return (
    <div className="relative flex h-full flex-col">
      <div ref={host} className="flex min-h-0 flex-1">
        <Canvas
          runs={all}
          feeds={feeds}
          activity={acts}
          forkPoints={forks}
          selected={all[0]?.id ?? null}
          live={all.filter((r) => r.status === 'running').map((r) => r.id)}
          arriving={null}
          onVisible={noop}
          onSelect={noop}
          onSend={async () => undefined}
          onFork={noop}
          onForkUnavailable={noop}
          onArrived={noop}
          viewportRef={viewportRef}
        />
      </div>
      <pre className="max-h-64 shrink-0 overflow-auto border-t border-border bg-panel p-2 font-mono text-micro text-text">
        {report === null
          ? `measuring — ${columns} columns × ${blocks} blocks…`
          : JSON.stringify(report, null, 2)}
      </pre>
    </div>
  )
}

function noop(..._args: unknown[]): void {}

/** The harness's channel out of the page, when it is running under one. */
function post(payload: unknown): void {
  const handler = (
    window as unknown as {
      webkit?: { messageHandlers?: { bench?: { postMessage: (m: string) => void } } }
    }
  ).webkit?.messageHandlers?.bench
  handler?.postMessage(JSON.stringify(payload))
}

// A page that throws would otherwise report nothing and read as a page too slow
// to finish, which is the one confusion a performance harness must not allow.
window.addEventListener('error', (e) => post({ error: String(e.message) }))
window.addEventListener('unhandledrejection', (e) => post({ error: String(e.reason) }))

// No `StrictMode`: it mounts everything twice on purpose, and the point here is
// to measure one mount of N feeds rather than two.
const root = document.getElementById('root')
if (root !== null) createRoot(root).render(<Bench />)
