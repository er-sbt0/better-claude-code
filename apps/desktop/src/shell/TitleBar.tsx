import type { Run } from '@agent-workspace/shared-types/Run'
import { basename } from '../lib/format'

// The top bar: what workspace is on screen, what branch the selected run is
// on, and the two ways into everything else — the palette and a new run.
//
// Not a Tauri title bar: the window still has the system's decoration (§4
// develops on GTK, where taking it over is its own project). This is the app's
// own first row.

export function TitleBar({
  run,
  left,
  right,
  onToggleLeft,
  onToggleRight,
  onNewRun,
  onPalette,
}: {
  run: Run | null
  left: boolean
  right: boolean
  onToggleLeft: () => void
  onToggleRight: () => void
  onNewRun: () => void
  onPalette: () => void
}) {
  return (
    <header className="flex h-9 shrink-0 items-center gap-1 border-b border-border bg-panel px-2">
      <PaneToggle side="left" open={left} onClick={onToggleLeft} />

      <div className="ml-1 flex min-w-0 items-center gap-2">
        <span className="truncate font-semibold">
          {run === null ? 'Agent Workspace' : basename(run.workspace_path)}
        </span>
        {run?.branch != null && (
          <span className="truncate font-mono text-mono text-muted">{run.branch}</span>
        )}
      </div>

      <div className="ml-auto flex items-center gap-1">
        <button
          type="button"
          onClick={onPalette}
          title="Command palette"
          className="flex h-6 items-center gap-2 rounded-sm px-2 text-micro text-muted hover:bg-hover hover:text-text"
        >
          <span>Commands</span>
          <kbd className="rounded-sm border border-border px-1 font-mono text-[10px] text-faint">
            ⌘K
          </kbd>
        </button>
        <button
          type="button"
          onClick={onNewRun}
          className="flex h-6 items-center rounded-sm border border-border bg-elevated px-2 text-micro text-text hover:border-accent/60"
        >
          New run
        </button>
        <PaneToggle side="right" open={right} onClick={onToggleRight} />
      </div>
    </header>
  )
}

/** The dock toggles, drawn as Zed draws them: a pane glyph, not a chevron. */
function PaneToggle({
  side,
  open,
  onClick,
}: {
  side: 'left' | 'right'
  open: boolean
  onClick: () => void
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      title={`${open ? 'Hide' : 'Show'} ${side} dock`}
      aria-pressed={open}
      className="flex size-6 items-center justify-center rounded-sm text-muted hover:bg-hover hover:text-text"
    >
      <svg viewBox="0 0 16 16" className="size-4" aria-hidden="true">
        <rect
          x="2.5"
          y="3.5"
          width="11"
          height="9"
          rx="1.5"
          fill="none"
          stroke="currentColor"
          strokeWidth="1"
        />
        <rect
          x={side === 'left' ? 2.5 : 9.5}
          y="3.5"
          width="4"
          height="9"
          fill="currentColor"
          opacity={open ? 0.9 : 0.25}
        />
      </svg>
    </button>
  )
}
