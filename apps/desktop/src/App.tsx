import { useEffect, useState } from 'react'
import type { BootReport } from '@agent-workspace/shared-types/BootReport'
import type { Run } from '@agent-workspace/shared-types/Run'
import { bootReport, runningRuns } from './lib/ipc'

export default function App() {
  const [boot, setBoot] = useState<BootReport | null>(null)
  const [runs, setRuns] = useState<Run[] | null>(null)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    Promise.all([bootReport(), runningRuns()])
      .then(([b, r]) => {
        setBoot(b)
        setRuns(r)
      })
      .catch((e: unknown) => setError(String(e)))
  }, [])

  return (
    <main>
      <header>
        <h1>Agent Workspace</h1>
        <p className="sub">
          The shell is up and talking to the spine. No surface is built yet —
          this is what the Rust side can already answer.
        </p>
      </header>

      {error && <p className="error">{error}</p>}

      <section>
        <h2>Store</h2>
        <p className="mono">{boot?.log_path ?? '…'}</p>
      </section>

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
        <h2>Running now</h2>
        {runs === null ? (
          <p className="dim">…</p>
        ) : runs.length === 0 ? (
          <p className="dim">
            No live runs. Nothing in the UI can start one yet — that needs the
            driver bridged to the webview.
          </p>
        ) : (
          <ul>
            {runs.map((r) => (
              <li key={r.id} className="mono">
                {r.id} · {r.agent} · {r.branch ?? 'no branch'}
              </li>
            ))}
          </ul>
        )}
      </section>

      {/* §4 develops against WebKitGTK, the weakest of the three engines. Which
          one is actually rendering should never be a guess. */}
      <footer className="mono dim">{navigator.userAgent}</footer>
    </main>
  )
}
