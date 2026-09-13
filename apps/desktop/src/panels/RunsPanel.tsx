import type { Run } from '@agent-workspace/shared-types/Run'
import type { RunId } from '@agent-workspace/shared-types/RunId'
import { DockEmpty, DockSection } from '../shell/Dock'
import { STATUS, basename, elapsed, goalOf } from '../lib/format'

// The left dock: every run, grouped by the workspace it belongs to.
//
// §10 calls this the index — "the launcher and the cross-workspace view",
// deliberately thin. It is not the graph and must not grow into a triage
// dashboard; clicking a row selects the node on the canvas, and that is all.

export function RunsPanel({
  runs,
  selected,
  onSelect,
  onNewRun,
}: {
  runs: Run[]
  selected: RunId | null
  onSelect: (id: RunId) => void
  onNewRun: () => void
}) {
  const workspaces = group(runs)

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <DockSection
        title={`Runs${runs.length === 0 ? '' : ` · ${runs.length}`}`}
        actions={
          <button
            type="button"
            onClick={onNewRun}
            title="Start a run"
            className="flex size-4 items-center justify-center rounded-sm text-muted hover:bg-hover hover:text-text"
          >
            +
          </button>
        }
      >
        {runs.length === 0 ? (
          <DockEmpty>
            No run has ever been started from this log. The first one appears
            here and on the canvas at the same moment.
          </DockEmpty>
        ) : (
          <ul>
            {workspaces.map(([workspace, rows]) => (
              <li key={workspace}>
                {workspaces.length > 1 && (
                  <p
                    className="truncate px-3 py-1 text-micro text-faint"
                    title={rows[0]?.workspace_path}
                  >
                    {workspace}
                  </p>
                )}
                <ul>
                  {rows.map((run) => (
                    <Row
                      key={run.id}
                      run={run}
                      selected={run.id === selected}
                      onSelect={() => onSelect(run.id)}
                    />
                  ))}
                </ul>
              </li>
            ))}
          </ul>
        )}
      </DockSection>
    </div>
  )
}

function Row({
  run,
  selected,
  onSelect,
}: {
  run: Run
  selected: boolean
  onSelect: () => void
}) {
  const status = STATUS[run.status]
  return (
    <li>
      <button
        type="button"
        onClick={onSelect}
        className={[
          'flex w-full items-center gap-2 px-3 py-1 text-left',
          selected ? 'bg-selected' : 'hover:bg-hover',
        ].join(' ')}
      >
        <i
          className={`size-1.5 shrink-0 rounded-full ${status.dot}`}
          aria-hidden="true"
          title={status.label}
        />
        <span className="min-w-0 flex-1 truncate text-micro" title={run.prompt ?? undefined}>
          {goalOf(run)}
        </span>
        <span className="shrink-0 font-mono text-[10px] text-faint">
          {elapsed(run.started_at, run.finished_at)}
        </span>
      </button>
    </li>
  )
}

/** Workspaces in the order their newest run appeared, rows newest first. */
function group(runs: Run[]): [string, Run[]][] {
  const out = new Map<string, Run[]>()
  for (const run of runs) {
    const key = basename(run.workspace_path)
    const rows = out.get(key)
    if (rows === undefined) out.set(key, [run])
    else rows.push(run)
  }
  return [...out.entries()]
}
