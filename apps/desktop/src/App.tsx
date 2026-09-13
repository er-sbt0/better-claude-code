import { useEffect, useState, type FormEvent } from 'react'
import type { BootReport } from '@agent-workspace/shared-types/BootReport'
import type { RawEvent } from '@agent-workspace/shared-types/RawEvent'
import type { Run } from '@agent-workspace/shared-types/Run'
import type { RunId } from '@agent-workspace/shared-types/RunId'
import type { StartedRun } from '@agent-workspace/shared-types/StartedRun'
import type { UnlistenFn } from '@tauri-apps/api/event'
import { bootReport, endRun, interruptRun, onRunEvent, runningRuns, startRun } from './lib/ipc'
import { describe } from './lib/describe'

// ---------------------------------------------------------------------------
// THIS IS NOT §7'S RUN FEED. It is scaffolding that proves the bridge.
//
// The feed (§7, and "not started" in the roadmap's V1 table) is six block
// types plus `Raw`, virtualized, every block carrying `(runId, seqRange)`, with
// collapse/expand first-class — because §10's run graph is built out of those
// blocks and the roadmap's thesis is the graph. What is below is a flat list of
// one line per raw event, the same thing `crates/driver/examples/run.rs` prints
// to a terminal, and it exists for one reason: until something rendered a live
// event on screen, nothing had demonstrated that `Registry::start`'s callback
// reaches the webview at all.
//
// So: no block types, no virtualization, no collapse, and no `seqRange`
// addressing here. Do not extend this into the feed — replace it.
// ---------------------------------------------------------------------------

/** Events keyed by run, because one channel carries every run's stream. */
type Streams = Record<RunId, RawEvent[]>

/** The two events that mean this run's process is gone (§8: not a turn). */
const isTerminal = (event: RawEvent) =>
  event.type === 'run.finished' || event.type === 'run.failed'

export default function App() {
  const [boot, setBoot] = useState<BootReport | null>(null)
  const [error, setError] = useState<string | null>(null)

  // The event subscription, and whether it is actually live yet. Starting a run
  // before it is would lose the first events silently, so the form waits.
  const [listening, setListening] = useState(false)
  const [streams, setStreams] = useState<Streams>({})

  const [run, setRun] = useState<StartedRun | null>(null)
  const [starting, setStarting] = useState(false)
  const [rows, setRows] = useState<Run[] | null>(null)

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
        [event.run_id]: [...(prev[event.run_id] ?? []), event],
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
      .catch((e: unknown) => setError(String(e)))

    return () => {
      cancelled = true
      unlisten?.()
    }
  }, [])

  useEffect(() => {
    bootReport()
      .then(setBoot)
      .catch((e: unknown) => setError(String(e)))
  }, [])

  const events = run === null ? [] : (streams[run.run_id] ?? [])
  const finished = events.some(isTerminal)

  // The log's answer to "what is running", refreshed at the two moments it can
  // have changed. It is not the same question as the live stream above: this
  // one is about rows, and a row stops saying `Running` only once the registry
  // has reaped the process — so the two disagree for a moment on purpose.
  useEffect(() => {
    runningRuns()
      .then(setRows)
      .catch((e: unknown) => setError(String(e)))
  }, [run, finished])

  const start = async (request: Parameters<typeof startRun>[0]) => {
    setError(null)
    setStarting(true)
    try {
      setRun(await startRun(request))
    } catch (e: unknown) {
      // The commands return `Result<_, String>`, and every one of those strings
      // is the only account anyone gets of why a run did not start. Swallowing
      // it leaves a button that does nothing.
      setError(String(e))
    } finally {
      setStarting(false)
    }
  }

  const steer = async (what: 'end' | 'interrupt') => {
    if (run === null) return
    setError(null)
    try {
      await (what === 'end' ? endRun(run.run_id) : interruptRun(run.run_id))
    } catch (e: unknown) {
      setError(String(e))
    }
  }

  return (
    <main>
      <header>
        <h1>Agent Workspace</h1>
        <p className="sub">
          Scaffolding, not a surface: a run can be started from here and its
          events arrive live. The feed (§7) and the run graph (§10) are unbuilt.
        </p>
      </header>

      {error !== null && <p className="error mono">{error}</p>}

      <StartRunForm disabled={starting || !listening} onStart={start} />

      {run !== null && (
        <>
          <section>
            <h2>This run</h2>
            <dl className="facts mono">
              <dt>run</dt>
              <dd>{run.run_id}</dd>
              <dt>branch</dt>
              <dd>{run.branch}</dd>
              <dt>worktree</dt>
              {/* §13: the agent's cwd is never the user's own checkout. */}
              <dd>{run.worktree_path}</dd>
              <dt>shadow</dt>
              <dd>{run.shadow_ref}</dd>
              <dt>started</dt>
              <dd>{run.started_at}</dd>
            </dl>
            <div className="actions">
              {/* Nothing closes a run's stdin on its behalf: a run started here
                  holds a live agent process until one of these is pressed, or
                  until the app exits. */}
              <button type="button" onClick={() => void steer('end')} disabled={finished}>
                End — close stdin, let the turn finish
              </button>
              <button type="button" onClick={() => void steer('interrupt')} disabled={finished}>
                Interrupt — stop now
              </button>
            </div>
          </section>

          <section>
            <h2>
              Events {finished ? '· the process is gone' : '· live'} ({events.length})
            </h2>
            <ol className="stream mono">
              {events.map((event) => {
                const { type, summary } = describe(event)
                return (
                  <li key={event.seq}>
                    <span className="seq">{event.seq}</span>
                    <span className="kind">{type}</span>
                    <span className="summary">{summary}</span>
                  </li>
                )
              })}
            </ol>
            {events.length === 0 && (
              <p className="dim">
                Nothing yet. The first event is `run.started`, once the agent
                process has said hello.
              </p>
            )}
          </section>
        </>
      )}

      <section>
        <h2>Reconciled at startup</h2>
        {/* §8: a run left running when the app died is gone, and the user is
            entitled to be told rather than to find it quietly finished. */}
        {boot === null ? (
          <p className="dim">…</p>
        ) : boot.abandoned.length === 0 ? (
          <p className="dim">Nothing was left running.</p>
        ) : (
          <ul>
            {boot.abandoned.map((id) => (
              <li key={id} className="mono">
                {id} — abandoned, its process died with the last session
              </li>
            ))}
          </ul>
        )}
      </section>

      <section>
        <h2>Running, according to the log</h2>
        {rows === null ? (
          <p className="dim">…</p>
        ) : rows.length === 0 ? (
          <p className="dim">No run row is open.</p>
        ) : (
          <ul>
            {rows.map((r) => (
              <li key={r.id} className="mono">
                {r.id} · {r.agent} · {r.branch ?? 'no branch'}
              </li>
            ))}
          </ul>
        )}
      </section>

      <section>
        <h2>Store</h2>
        <p className="mono dim">{boot?.log_path ?? '…'}</p>
      </section>

      {/* §4 develops against WebKitGTK, the weakest of the three engines. Which
          one is actually rendering should never be a guess. */}
      <footer className="mono dim">{navigator.userAgent}</footer>
    </main>
  )
}

/**
 * Everything the shell needs to start a run, and deliberately nothing more.
 *
 * There is no permission field because there is no permission surface: §12's V1
 * dialog needs a `Decider` that is a person, and until there is one a run
 * started here hosts no workspace MCP server. **Nothing can prompt.** A tool
 * that is not listed under "allowed tools" is denied outright, silently, and
 * the run carries on without it — which is why the field says so.
 */
function StartRunForm({
  disabled,
  onStart,
}: {
  disabled: boolean
  onStart: (request: Parameters<typeof startRun>[0]) => Promise<void>
}) {
  const [repo, setRepo] = useState('')
  const [prompt, setPrompt] = useState('')
  const [model, setModel] = useState('')
  const [tools, setTools] = useState('Read')
  const [maxTurns, setMaxTurns] = useState('3')
  const [budget, setBudget] = useState('0.20')

  const submit = (e: FormEvent) => {
    e.preventDefault()
    const number = (raw: string) => (raw.trim() === '' ? null : Number(raw))
    void onStart({
      repo: repo.trim(),
      prompt,
      model: model.trim() === '' ? null : model.trim(),
      allowed_tools: tools
        .split(',')
        .map((t) => t.trim())
        .filter((t) => t !== ''),
      max_turns: number(maxTurns),
      max_budget_usd: number(budget),
    })
  }

  return (
    <section>
      <h2>Start a run</h2>
      <form onSubmit={submit}>
        <label>
          <span>Repository</span>
          {/* Never the agent's cwd: the run gets a worktree of its own (§13). */}
          <input
            value={repo}
            onChange={(e) => setRepo(e.target.value)}
            placeholder="/absolute/path/to/repo"
            required
          />
        </label>
        <label>
          <span>Prompt</span>
          <textarea
            value={prompt}
            onChange={(e) => setPrompt(e.target.value)}
            rows={3}
            required
          />
        </label>
        <div className="row">
          <label>
            <span>Model</span>
            <input
              value={model}
              onChange={(e) => setModel(e.target.value)}
              placeholder="claude-sonnet-5"
            />
          </label>
          <label>
            <span>Allowed tools</span>
            <input
              value={tools}
              onChange={(e) => setTools(e.target.value)}
              placeholder="Read, Edit"
            />
          </label>
          <label>
            <span>Max turns</span>
            <input value={maxTurns} onChange={(e) => setMaxTurns(e.target.value)} inputMode="numeric" />
          </label>
          <label>
            <span>Budget (USD)</span>
            <input value={budget} onChange={(e) => setBudget(e.target.value)} inputMode="decimal" />
          </label>
        </div>
        <div className="actions">
          <button type="submit" disabled={disabled}>
            Start
          </button>
          <span className="dim">
            Nothing can prompt: no permission dialog exists yet (§12), so a tool
            outside the list above is denied and the agent works around it.
          </span>
        </div>
      </form>
    </section>
  )
}
