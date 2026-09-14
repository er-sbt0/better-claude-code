import { useEffect, useRef } from 'react'
import type { RunId } from '@agent-workspace/shared-types/RunId'
import { useEscape } from '../shell/Dock'

/**
 * The confirmation §13 requires before a worktree with uncommitted changes is
 * removed.
 *
 * **Why this exists rather than `window.confirm`.** The gesture behind it is
 * the only destructive one in the app, and the browser modal's behaviour here
 * is not something this path may assume: wry installs no script-dialog handler
 * and neither does `tauri-runtime-wry`, so `window.confirm` falls through to
 * whatever WebKitGTK does by default on §4's primary target. Both ways that can
 * go wrong are bad and one is silent — a falsy return makes the `force` path
 * dead code and a dirty tree uncleanable, and a truthy non-boolean (a Promise,
 * say) sails past `if (!confirmed)` and force-removes uncommitted work having
 * asked nobody, which is the exact outcome the policy exists to prevent. It
 * would also be the one browser modal in the shell, blocking the webview's main
 * thread while live runs stream events into it.
 *
 * **What it asks is decided elsewhere.** `driver::Registry::clean_up` looks at
 * `git status` and answers `Cleanup::Dirty`; this only puts that answer to a
 * person. The shell owns no logic, and "is there uncommitted work here" is not
 * a question a window gets to have an opinion about.
 *
 * Cancel is the safe action and therefore the default one: it takes focus on
 * open, Escape reaches it (`useEscape`, as everywhere else in this shell), and
 * so does a click on the backdrop. Nothing happens on any of those paths —
 * there is no partially-applied state to undo, because the refused call left
 * the tree and the run row exactly as they were.
 */
export function ConfirmCleanUpDialog({
  run,
  worktreePath,
  busy,
  onCancel,
  onConfirm,
}: {
  /** Which run — named out loud, because the palette can open this with the
   *  inspector closed and nothing else on screen would say. */
  run: RunId | null
  /** The tree that would go. From the refused call, so it is the path the
   *  driver actually looked at rather than one this window remembered. */
  worktreePath: string
  busy: boolean
  onCancel: () => void
  onConfirm: () => void
}) {
  const cancel = useRef<HTMLButtonElement | null>(null)
  const open = run !== null

  useEscape(open, onCancel)

  useEffect(() => {
    if (!open) return
    const timer = setTimeout(() => cancel.current?.focus(), 0)
    return () => clearTimeout(timer)
  }, [open])

  if (run === null) return null

  return (
    <div
      className="absolute inset-0 z-50 flex items-start justify-center bg-black/40 pt-[10vh]"
      onMouseDown={onCancel}
    >
      <div
        role="dialog"
        aria-modal="true"
        aria-labelledby="confirm-clean-up-title"
        onMouseDown={(e) => e.stopPropagation()}
        className="w-[32rem] max-w-[calc(100%-2rem)] overflow-hidden rounded-md border border-err/50 bg-elevated shadow-2xl"
      >
        <header
          id="confirm-clean-up-title"
          className="flex h-9 items-center border-b border-border px-3 font-semibold"
        >
          Remove the worktree of {run}?
        </header>

        <div className="flex flex-col gap-2 p-3">
          <p className="font-mono text-mono break-all text-muted select-text">
            {worktreePath}
          </p>
          {/* The sentence that makes the choice an informed one. §13.1 takes a
              snapshot after each of the run's tool calls, so what this removes
              irrecoverably is the window after the last of them — which is
              exactly the part nobody would think to look for. */}
          <p className="text-micro text-muted">
            This tree has uncommitted changes, and removing it deletes them. The
            shadow history holds a snapshot taken after each of the run’s tool
            calls, so anything written since the last one — by you, or by
            something the run started — has no other copy.
          </p>
          <p className="text-micro text-faint">
            The run itself is not deleted: its log stays, and so does its shadow
            history, so it remains forkable and Review keeps its attribution.
          </p>
        </div>

        <footer className="flex items-center justify-end gap-2 border-t border-border px-3 py-2">
          <button
            ref={cancel}
            type="button"
            onClick={onCancel}
            className="rounded-sm px-2 py-1 text-micro text-muted hover:bg-hover hover:text-text"
          >
            Cancel
          </button>
          <button
            type="button"
            onClick={onConfirm}
            disabled={busy}
            className="rounded-sm border border-border bg-panel px-3 py-1 text-micro text-err enabled:hover:border-err/60 disabled:text-faint"
          >
            {busy ? 'Removing…' : 'Remove it anyway'}
          </button>
        </footer>
      </div>
    </div>
  )
}
