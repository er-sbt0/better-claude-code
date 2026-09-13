import { useEffect, useRef, useState, type FormEvent } from 'react'
import type { StartRunRequest } from '@agent-workspace/shared-types/StartRunRequest'
import { useEscape } from '../shell/Dock'

/**
 * Everything the shell needs to start a run, and deliberately nothing more.
 *
 * There is no permission field because there is no permission surface: §12's V1
 * dialog needs a `Decider` that is a person, and until there is one a run
 * started here hosts no workspace MCP server. **Nothing can prompt.** A tool
 * that is not listed under "allowed tools" is denied outright, silently, and
 * the run carries on without it — which is why the field says so.
 *
 * It is a dialog rather than a permanent pane because starting a run is an act,
 * not a view: a form nailed to the window is the shape the old scaffolding had,
 * and it competed with the work for the whole screen.
 */
export function NewRunDialog({
  open,
  busy,
  onClose,
  onStart,
}: {
  open: boolean
  busy: boolean
  onClose: () => void
  onStart: (request: StartRunRequest) => Promise<void>
}) {
  const [repo, setRepo] = useState('')
  const [prompt, setPrompt] = useState('')
  const [model, setModel] = useState('')
  const [tools, setTools] = useState('Read')
  const [maxTurns, setMaxTurns] = useState('3')
  const [budget, setBudget] = useState('0.20')
  const first = useRef<HTMLInputElement | null>(null)

  useEscape(open, onClose)

  useEffect(() => {
    if (!open) return
    const timer = setTimeout(() => first.current?.focus(), 0)
    return () => clearTimeout(timer)
  }, [open])

  if (!open) return null

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
    <div
      className="absolute inset-0 z-50 flex items-start justify-center bg-black/40 pt-[10vh]"
      onMouseDown={onClose}
    >
      <form
        onSubmit={submit}
        onMouseDown={(e) => e.stopPropagation()}
        className="w-[36rem] max-w-[calc(100%-2rem)] overflow-hidden rounded-md border border-border bg-elevated shadow-2xl"
      >
        <header className="flex h-9 items-center border-b border-border px-3 font-semibold">
          Start a run
        </header>

        <div className="flex flex-col gap-3 p-3">
          <Field label="Repository">
            {/* Never the agent's cwd: the run gets a worktree of its own (§13). */}
            <input
              ref={first}
              value={repo}
              onChange={(e) => setRepo(e.target.value)}
              placeholder="/absolute/path/to/repo"
              required
              className={INPUT}
            />
          </Field>

          <Field label="Prompt">
            <textarea
              value={prompt}
              onChange={(e) => setPrompt(e.target.value)}
              rows={4}
              required
              className={`${INPUT} resize-y`}
            />
          </Field>

          <div className="grid grid-cols-2 gap-3">
            <Field label="Model">
              <input
                value={model}
                onChange={(e) => setModel(e.target.value)}
                placeholder="claude-sonnet-5"
                className={INPUT}
              />
            </Field>
            <Field label="Allowed tools">
              <input
                value={tools}
                onChange={(e) => setTools(e.target.value)}
                placeholder="Read, Edit"
                className={INPUT}
              />
            </Field>
            <Field label="Max turns">
              <input
                value={maxTurns}
                onChange={(e) => setMaxTurns(e.target.value)}
                inputMode="numeric"
                className={INPUT}
              />
            </Field>
            <Field label="Budget (USD)">
              <input
                value={budget}
                onChange={(e) => setBudget(e.target.value)}
                inputMode="decimal"
                className={INPUT}
              />
            </Field>
          </div>

          <p className="text-micro text-faint">
            Nothing can prompt: no permission dialog exists yet (§12), so a tool
            outside the list above is denied and the agent works around it.
          </p>
        </div>

        <footer className="flex items-center justify-end gap-2 border-t border-border px-3 py-2">
          <button
            type="button"
            onClick={onClose}
            className="rounded-sm px-2 py-1 text-micro text-muted hover:bg-hover hover:text-text"
          >
            Cancel
          </button>
          <button
            type="submit"
            disabled={busy}
            className="rounded-sm border border-border bg-panel px-3 py-1 text-micro text-text enabled:hover:border-accent/60 disabled:text-faint"
          >
            {busy ? 'Starting…' : 'Start'}
          </button>
        </footer>
      </form>
    </div>
  )
}

const INPUT =
  'w-full rounded-sm border border-border bg-bg px-2 py-1 font-mono text-mono text-text select-text outline-none placeholder:text-faint focus:border-accent/70'

function Field({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <label className="flex min-w-0 flex-col gap-1">
      <span className="text-micro text-muted">{label}</span>
      {children}
    </label>
  )
}
