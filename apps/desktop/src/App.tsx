import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import type { BootReport } from '@agent-workspace/shared-types/BootReport'
import type { Feed } from '@agent-workspace/shared-types/Feed'
import type { ForkPoints } from '@agent-workspace/shared-types/ForkPoints'
import type { RawEvent } from '@agent-workspace/shared-types/RawEvent'
import type { Run } from '@agent-workspace/shared-types/Run'
import type { RunId } from '@agent-workspace/shared-types/RunId'
import type { RunActivity } from '@agent-workspace/shared-types/RunActivity'
import type { RunMetrics } from '@agent-workspace/shared-types/RunMetrics'
import type { StartRunRequest } from '@agent-workspace/shared-types/StartRunRequest'
import type { UnlistenFn } from '@tauri-apps/api/event'
import {
  bootReport,
  endRun,
  forkPoints,
  forkRun,
  interruptRun,
  liveRuns,
  onRunEvent,
  recentRuns,
  runActivity,
  runFeed,
  runMetrics,
  sendTurn,
  startRun,
} from './lib/ipc'
import { Canvas, type CanvasHandle } from './canvas/Canvas'
import { InspectorPanel } from './panels/InspectorPanel'
import { NewRunDialog } from './panels/NewRunDialog'
import { RunsPanel } from './panels/RunsPanel'
import { CommandPalette, usePaletteShortcut, type Command } from './shell/CommandPalette'
import { Dock } from './shell/Dock'
import { StatusBar } from './shell/StatusBar'
import { TitleBar } from './shell/TitleBar'
import { Toasts, toast, type Toast } from './shell/Toasts'

// The shell: title bar, two docks, a status bar, and §10's run graph between
// them. It owns the state every surface reads and none of the logic — the
// commands in `lib/ipc.ts` are the whole of what this app can do.
//
// The three-word status and the message count each node's header shows are a
// projection this file *fetches* rather than computes (`runActivity`), for the
// same reason metrics are: deriving "needs you" from the log is deciding, and
// the shell owns no logic.
//
// Forking is wired end to end from here: ⌘⇧F on a block reaches `fork_run`,
// which resolves the address in the parent's log, checks the child's worktree
// out of the parent's shadow commit at that `seq` and supervises the child like
// any other run. Which blocks *offer* the gesture is a projection this file
// fetches (`forkPoints`) for the same reason the three-word status is one: the
// rule is `normalizer::claude_code::fork_address`, and a second copy of it in
// TypeScript would be out of reach of every test in this workspace.
//
// The node bodies are §7's feed as of this step: six block types plus `Raw`,
// virtualized, every block carrying the `(runId, seq…)` address collapse, fork
// and compare resolve against. The blocks themselves are projected by
// `document-model` and fetched (`runFeed`), for the same reason the three-word
// status is fetched: deciding that three events are one block is deciding.
//
// **Which feeds are live is decided here**, and it is the question
// `branching-surface.md` left open. A feed is fetched for a run the camera can
// see, for the selected one, and for every run this session is supervising;
// feeds for runs that scroll off are dropped, and a run that comes back fetches
// again. Because the projection is a function of the log alone, coming back is
// one call — there is no delta to miss, which is what makes the cheap answer
// also the correct one.

/** Events keyed by run, because one channel carries every run's stream. */
type Streams = Record<RunId, RawEvent[]>

/** How much history the graph is willing to lay out. */
const WINDOW = 200

/**
 * How often a *live* run's feed is re-projected while it streams.
 *
 * A finished run's log is immutable, so its feed is fetched once and never
 * again; this interval only ever applies to runs this session is supervising.
 * It is a poll rather than a fetch per event because a run emits events in
 * bursts — a turn can produce a dozen in a few milliseconds — and one round
 * trip per event would re-project the same log a dozen times to answer the same
 * question.
 */
const FEED_POLL_MS = 250

/** The two events that mean this run's process is gone (§8: not a turn). */
const isTerminal = (event: RawEvent) =>
  event.type === 'run.finished' || event.type === 'run.failed'

export default function App() {
  const [boot, setBoot] = useState<BootReport | null>(null)
  const [toasts, setToasts] = useState<Toast[]>([])

  // The event subscription, and whether it is actually live yet. Starting a run
  // before it is would lose the first events silently, so the form waits.
  const [listening, setListening] = useState(false)
  const [streams, setStreams] = useState<Streams>({})

  const [runs, setRuns] = useState<Run[]>([])
  const [live, setLive] = useState<RunId[]>([])
  const [metrics, setMetrics] = useState<Record<RunId, RunMetrics>>({})
  const [activity, setActivity] = useState<Record<RunId, RunActivity>>({})
  const [forks, setForks] = useState<Record<RunId, ForkPoints>>({})
  const [feeds, setFeeds] = useState<Record<RunId, Feed>>({})
  /** The runs the camera can see, reported by the canvas. */
  const [visible, setVisible] = useState<RunId[]>([])
  const [selected, setSelected] = useState<RunId | null>(null)
  const [starting, setStarting] = useState(false)
  /** A child that has just been forked, until the canvas has landed on it. */
  const [arriving, setArriving] = useState<RunId | null>(null)
  /**
   * What this session launched each run with.
   *
   * The `runs` row stores what a run *is*, and of what it was *launched with*
   * only the model (schema V3). A fork therefore cannot inherit its parent's
   * allowed tools through the log, and storing them to make it possible would
   * be storing a launch flag to avoid passing one. So the shell remembers what
   * it asked for — for this session, which is exactly as long as the runs it
   * can fork from are supervised — and a run started before this window opened
   * forks with nothing pre-granted, which the toast says out loud.
   */
  const [launched, setLaunched] = useState<Record<RunId, StartRunRequest>>({})

  const [leftOpen, setLeftOpen] = useState(true)
  const [rightOpen, setRightOpen] = useState(true)
  const [leftWidth, setLeftWidth] = useState(240)
  const [rightWidth, setRightWidth] = useState(300)
  const [paletteOpen, setPaletteOpen] = useState(false)
  const [dialogOpen, setDialogOpen] = useState(false)

  const canvas = useRef<CanvasHandle | null>(null)

  const say = useCallback((t: Toast) => setToasts((prev) => [...prev, t]), [])
  const fail = useCallback(
    (title: string) => (e: unknown) => {
      // The commands return `Result<_, String>`, and every one of those strings
      // is the only account anyone gets of why something did not happen.
      // Swallowing it leaves a button that does nothing.
      say(toast('error', title, String(e)))
    },
    [say],
  )

  // The listener is registered on mount, before anything can start a run, and
  // it is registered *first* — `listen()` is an IPC round trip of its own, so
  // subscribing after `startRun()` resolves is a race the first events of every
  // run lose. Nothing reports that loss; the only defense is ordering.
  useEffect(() => {
    let unlisten: UnlistenFn | null = null
    let cancelled = false

    onRunEvent((event) => {
      setStreams((prev) => ({
        ...prev,
        [event.run_id]: merge(prev[event.run_id] ?? [], [event]),
      }))
    })
      .then((fn) => {
        if (cancelled) {
          fn()
          return
        }
        unlisten = fn
        setListening(true)
      })
      .catch(fail('Could not subscribe to run events'))

    return () => {
      cancelled = true
      unlisten?.()
    }
  }, [fail])

  const refreshRuns = useCallback(() => {
    recentRuns(WINDOW).then(setRuns).catch(fail('Could not read the run log'))
    liveRuns().then(setLive).catch(fail('Could not list supervised runs'))
  }, [fail])

  // One call for every node's header, not one per node: `recent_activity` is a
  // single grouped query over the same window the graph is drawn from.
  const refreshActivity = useCallback(() => {
    runActivity(WINDOW)
      .then((rows) => setActivity(Object.fromEntries(rows.map((a) => [a.run_id, a]))))
      .catch(fail('Could not read run activity'))
  }, [fail])

  // Which blocks can be forked at, for the runs a fork can be anchored in: the
  // one being read, the ones this session supervises, and the parents of the
  // children on screen (a child's inherited-context strip states a count that
  // only the parent's projection knows). Not the whole window — the answer is
  // per block, and a two-hundred-run window of it is most of the log.
  const refreshForkPoints = useCallback(() => {
    const wanted = new Set<RunId>(live)
    if (selected !== null) wanted.add(selected)
    for (const run of runs) if (run.parent_run_id !== null) wanted.add(run.parent_run_id)
    const ids = [...wanted].filter((id) => runs.some((r) => r.id === id))
    if (ids.length === 0) return
    forkPoints(ids)
      .then((rows) => setForks(Object.fromEntries(rows.map((f) => [f.run_id, f]))))
      .catch(fail('Could not read where this run can be forked'))
  }, [live, runs, selected, fail])

  useEffect(() => {
    bootReport()
      .then((report) => {
        setBoot(report)
        // §8: a run left running when the app died is gone, and the user is
        // entitled to be told rather than to find it quietly finished. A toast
        // that must be dismissed is that telling; a line in a panel nobody
        // opened is not.
        for (const id of report.abandoned) {
          say(
            toast(
              'notice',
              'A run was abandoned',
              `${id} — its process died with the last session`,
            ),
          )
        }
      })
      .catch(fail('Could not read the boot report'))
    refreshRuns()
  }, [fail, refreshRuns, say])

  // A terminal event changes the row the log will hand back, and the metrics
  // worth summing. Both are re-read at that moment and at no other.
  const terminal = useMemo(
    () =>
      Object.entries(streams)
        .filter(([, events]) => events.some(isTerminal))
        .map(([id]) => id)
        .join(','),
    [streams],
  )
  useEffect(() => {
    refreshRuns()
  }, [terminal, refreshRuns])

  // Every event changes a header: a message moves the count, a permission
  // request moves the status. The log is written before the event reaches this
  // webview (`pipeline.rs` appends, then the supervisor emits), so a refresh
  // triggered by an arrival always sees the arrival.
  const eventCount = useMemo(
    () => Object.values(streams).reduce((n, events) => n + events.length, 0),
    [streams],
  )
  useEffect(() => {
    refreshActivity()
  }, [eventCount, runs, refreshActivity])

  useEffect(() => {
    refreshForkPoints()
  }, [eventCount, refreshForkPoints])

  // ------------------------------------------------------------- the feeds --

  /**
   * Which runs need a feed: the ones the camera can see, the selected one, and
   * the ones this session is supervising.
   *
   * This is the answer to `branching-surface.md`'s "how many feeds are live at
   * once", and it is a *data* answer rather than a rendering one — every column
   * stays mounted, as the record's focus view requires, and what is bounded is
   * how many of them hold a projected run. A supervised run is included even
   * off screen, because its feed is what the header count and the fork toolbar
   * are read from the moment the camera arrives.
   */
  const wanted = useMemo(() => {
    const ids = new Set<RunId>(visible)
    if (selected !== null) ids.add(selected)
    for (const id of live) ids.add(id)
    return [...ids].filter((id) => runs.some((r) => r.id === id)).sort()
  }, [visible, selected, live, runs])
  const wantedKey = wanted.join(',')

  // Refs, because the sync below runs on a timer and must see the current
  // answer rather than the one captured when the timer was set.
  const wantedRef = useRef(wanted)
  wantedRef.current = wanted
  const liveRef = useRef(live)
  liveRef.current = live
  const streamsRef = useRef(streams)
  streamsRef.current = streams
  /** Event count each run's feed was last projected at. */
  const projectedAt = useRef<Record<RunId, number>>({})

  const syncFeeds = useCallback(() => {
    for (const id of wantedRef.current) {
      const events = streamsRef.current[id]?.length ?? 0
      const at = projectedAt.current[id]
      // Fetched once, then again only while the run is still producing events.
      // A finished run's log is immutable (§8: append-only), so its feed cannot
      // change and re-asking would be a round trip for the same answer.
      if (at !== undefined && (at === events || !liveRef.current.includes(id))) continue
      projectedAt.current[id] = events
      runFeed(id)
        .then((feed) => setFeeds((prev) => ({ ...prev, [id]: feed })))
        .catch((e: unknown) => {
          // Let the next pass try again rather than leaving the column empty
          // for the life of the session.
          delete projectedAt.current[id]
          fail('Could not read the run’s feed')(e)
        })
    }
  }, [fail])

  useEffect(() => {
    syncFeeds()
    // Drop what the camera has left behind. A feed is thousands of blocks, and
    // keeping every run the session has ever looked at is how a long-lived
    // window grows without bound. Coming back re-fetches, which is correct
    // rather than merely cheap: the projection is a function of the log.
    setFeeds((prev) => {
      const keep = new Set(wantedRef.current)
      const next = Object.fromEntries(Object.entries(prev).filter(([id]) => keep.has(id)))
      for (const id of Object.keys(prev)) if (!keep.has(id)) delete projectedAt.current[id]
      return Object.keys(next).length === Object.keys(prev).length ? prev : next
    })
  }, [wantedKey, syncFeeds])

  // A live run's feed is re-projected on a timer; nothing else is. With nothing
  // supervised there is no timer at all, so an idle window does no work.
  useEffect(() => {
    if (live.length === 0) return
    const timer = setInterval(syncFeeds, FEED_POLL_MS)
    return () => clearInterval(timer)
  }, [live.length, syncFeeds])

  const current = useMemo(
    () => runs.find((r) => r.id === selected) ?? null,
    [runs, selected],
  )

  useEffect(() => {
    if (selected === null) return
    runMetrics(selected)
      .then((m) => setMetrics((prev) => ({ ...prev, [selected]: m })))
      .catch(fail('Could not compute run metrics'))
  }, [selected, terminal, fail])

  // The newest run is the one someone just caused; selecting it saves a click
  // on the only screen where there is nothing else to look at.
  useEffect(() => {
    if (selected === null && runs.length > 0) setSelected(runs[0]!.id)
  }, [runs, selected])

  const start = async (request: StartRunRequest) => {
    setStarting(true)
    try {
      const started = await startRun(request)
      setDialogOpen(false)
      setLaunched((prev) => ({ ...prev, [started.run_id]: request }))
      setSelected(started.run_id)
      refreshRuns()
    } catch (e: unknown) {
      fail('The run did not start')(e)
    } finally {
      setStarting(false)
    }
  }

  const steer = (what: 'end' | 'interrupt') => {
    if (selected === null) return
    const call = what === 'end' ? endRun : interruptRun
    call(selected)
      .then(() => refreshRuns())
      .catch(fail(what === 'end' ? 'Could not end the run' : 'Could not interrupt the run'))
  }

  // A turn, through the process that is already running (§5). The rejection is
  // re-thrown after the toast so the composer can keep what was typed.
  const send = useCallback(
    (id: RunId, text: string) =>
      sendTurn(id, text).catch((e: unknown) => {
        fail('The turn was not sent')(e)
        throw e
      }),
    [fail],
  )

  /**
   * ⌘⇧F: branch the conversation at a block, and the worktree with it.
   *
   * Everything that makes this a fork happens below the window. What the shell
   * contributes is the two things the log cannot answer — which block (the
   * user's pointer) and what to launch the child with (what this session
   * started the parent with) — and then it gets out of the way.
   *
   * The child is announced to the canvas as `arriving` rather than selected
   * here: selection is what the fork motion *lands* on at ~900ms, and selecting
   * it now would teleport the camera to a node that has not been drawn yet.
   */
  const fork = useCallback(
    (parent: RunId, seq: number) => {
      const inherited = launched[parent]
      forkRun({
        parent,
        seq,
        // A fork is a conversation you continue, so the first thing said to it
        // is the user's next turn. Typed into the child's composer, which is
        // where every branch in the prototype begins.
        prompt: 'Continue from here.',
        // `null` inherits the parent's model, which is what a fork means.
        model: null,
        allowed_tools: inherited?.allowed_tools ?? [],
        max_turns: inherited?.max_turns ?? null,
        max_budget_usd: inherited?.max_budget_usd ?? null,
      })
        .then((child) => {
          if (inherited === undefined) {
            say(
              toast(
                'notice',
                'Forked with no tools pre-granted',
                `${parent} was not started by this session, so there is no allow-list to inherit — the branch can think and answer but not edit.`,
              ),
            )
          } else {
            setLaunched((prev) => ({ ...prev, [child.run_id]: inherited }))
          }
          setArriving(child.run_id)
          refreshRuns()
        })
        .catch(fail('The fork did not happen'))
    },
    [launched, refreshRuns, say, fail],
  )

  /** ⌘⇧F with nothing addressable under the pointer. Said, never rounded off. */
  const forkUnavailable = useCallback(
    () =>
      say(
        toast(
          'notice',
          'Nothing forkable under the pointer',
          'A fork is anchored to a message, and only where the provider gave that message an address. Hover a block that offers the toolbar — the surface will not rewind to a nearby one.',
        ),
      ),
    [say],
  )

  /** The end of the fork motion: focus lands on the child at ~900ms. */
  const arrived = useCallback((child: RunId) => {
    setSelected(child)
    setArriving(null)
  }, [])

  usePaletteShortcut(useCallback(() => setPaletteOpen((open) => !open), []))

  const isLive = selected !== null && live.includes(selected)
  const commands: Command[] = [
    { id: 'run.new', group: 'Run', title: 'Start a run…', hint: '+', run: () => setDialogOpen(true) },
    {
      id: 'run.end',
      group: 'Run',
      title: 'End the selected run — close stdin, let the turn finish',
      disabled: !isLive,
      run: () => steer('end'),
    },
    {
      id: 'run.interrupt',
      group: 'Run',
      title: 'Interrupt the selected run — stop now',
      disabled: !isLive,
      run: () => steer('interrupt'),
    },
    {
      id: 'run.fork',
      group: 'Run',
      title: 'Fork at the hovered message — ⌘⇧F',
      run: () => canvas.current?.forkHovered(),
    },
    {
      id: 'view.map',
      group: 'View',
      title: 'Switch between the columns and the map — ⌘\\',
      run: () => canvas.current?.toggleMode(),
    },
    {
      id: 'view.prev',
      group: 'View',
      title: 'Previous sibling — ⌘[',
      run: () => canvas.current?.sibling(-1),
    },
    {
      id: 'view.next',
      group: 'View',
      title: 'Next sibling — ⌘]',
      run: () => canvas.current?.sibling(1),
    },
    { id: 'view.fit', group: 'View', title: 'Fit the graph to the window', run: () => canvas.current?.fit() },
    { id: 'view.in', group: 'View', title: 'Zoom in', run: () => canvas.current?.zoomIn() },
    { id: 'view.out', group: 'View', title: 'Zoom out', run: () => canvas.current?.zoomOut() },
    { id: 'view.left', group: 'View', title: `${leftOpen ? 'Hide' : 'Show'} the runs dock`, run: () => setLeftOpen((o) => !o) },
    { id: 'view.right', group: 'View', title: `${rightOpen ? 'Hide' : 'Show'} the inspector`, run: () => setRightOpen((o) => !o) },
    { id: 'log.refresh', group: 'Log', title: 'Refresh from the log', run: refreshRuns },
  ]

  return (
    <div className="relative flex h-full flex-col overflow-hidden">
      <TitleBar
        run={current}
        left={leftOpen}
        right={rightOpen}
        onToggleLeft={() => setLeftOpen((o) => !o)}
        onToggleRight={() => setRightOpen((o) => !o)}
        onNewRun={() => setDialogOpen(true)}
        onPalette={() => setPaletteOpen(true)}
      />

      <div className="flex min-h-0 flex-1">
        <Dock side="left" open={leftOpen} width={leftWidth} onResize={setLeftWidth}>
          <RunsPanel
            runs={runs}
            selected={selected}
            onSelect={setSelected}
            onNewRun={() => setDialogOpen(true)}
          />
        </Dock>

        <Canvas
          runs={runs}
          feeds={feeds}
          activity={activity}
          forkPoints={forks}
          selected={selected}
          live={live}
          arriving={arriving}
          onVisible={setVisible}
          onSelect={setSelected}
          onSend={send}
          onFork={fork}
          onForkUnavailable={forkUnavailable}
          onArrived={arrived}
          viewportRef={canvas}
        />

        <Dock side="right" open={rightOpen} width={rightWidth} onResize={setRightWidth}>
          <InspectorPanel
            run={current}
            metrics={selected === null ? null : (metrics[selected] ?? null)}
            live={isLive}
            onEnd={() => steer('end')}
            onInterrupt={() => steer('interrupt')}
          />
        </Dock>
      </div>

      <StatusBar
        run={current}
        metrics={selected === null ? null : (metrics[selected] ?? null)}
        blocks={selected === null ? null : (feeds[selected]?.blocks.length ?? null)}
        live={listening}
        logPath={boot?.log_path ?? null}
      />

      <NewRunDialog
        open={dialogOpen}
        busy={starting || !listening}
        onClose={() => setDialogOpen(false)}
        onStart={start}
      />
      <CommandPalette
        open={paletteOpen}
        commands={commands}
        onClose={() => setPaletteOpen(false)}
      />
      <Toasts toasts={toasts} onDismiss={(id) => setToasts((p) => p.filter((t) => t.id !== id))} />
    </div>
  )
}

/**
 * Fold new events into a run's stream, by `seq`.
 *
 * Two sources reach the same array — the live channel and the log — and a run
 * that is both supervised and selected is served by both. `seq` is the log's
 * own ordering (§8), so it is also the identity that makes the overlap
 * harmless rather than a stream that shows every event twice.
 */
function merge(existing: RawEvent[], incoming: RawEvent[]): RawEvent[] {
  if (existing.length === 0) return [...incoming]
  const bySeq = new Map(existing.map((e) => [e.seq, e]))
  let changed = false
  for (const event of incoming) {
    if (bySeq.has(event.seq)) continue
    bySeq.set(event.seq, event)
    changed = true
  }
  if (!changed) return existing
  return [...bySeq.values()].sort((a, b) => Number(a.seq) - Number(b.seq))
}
