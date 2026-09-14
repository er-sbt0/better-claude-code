import type { Run } from '@agent-workspace/shared-types/Run'
import type { RunMetrics } from '@agent-workspace/shared-types/RunMetrics'
import { DockEmpty, DockSection } from '../shell/Dock'
import { STATUS, elapsed, money } from '../lib/format'

// The right dock: everything true about the selected run that is not its
// events. §10's "Run #1842·b" panel — identity, provenance, cost, the two
// controls that end it, and the one control that takes its worktree away.

export function InspectorPanel({
  run,
  metrics,
  live,
  onEnd,
  onInterrupt,
  onCleanUp,
}: {
  run: Run | null
  metrics: RunMetrics | null
  /** Whether this process is still supervising it — not whether the row is open. */
  live: boolean
  onEnd: () => void
  onInterrupt: () => void
  /**
   * Remove this run's worktree and branch (§13).
   *
   * Offered only on a run nothing is supervising, and the driver refuses a live
   * one anyway — this is the affordance agreeing with the rule, not enforcing
   * it. Confirming a dirty tree happens in `App.tsx`, where the answer the
   * command came back with is.
   */
  onCleanUp: () => void
}) {
  if (run === null) {
    return (
      <DockSection title="Run">
        <DockEmpty>Select a run on the canvas or in the runs list.</DockEmpty>
      </DockSection>
    )
  }

  const status = STATUS[run.status]

  return (
    <div className="flex min-h-0 flex-1 flex-col overflow-y-auto">
      <DockSection title="Run">
        <div className="flex items-center gap-2 px-3 pb-2">
          <i className={`size-1.5 shrink-0 rounded-full ${status.dot}`} aria-hidden="true" />
          <span className="truncate font-mono text-mono font-semibold select-text">{run.id}</span>
          <span className={`ml-auto shrink-0 text-micro ${status.text}`}>{status.label}</span>
        </div>
        <Facts
          rows={[
            ['goal', run.prompt ?? '—'],
            ['agent', `${run.agent}${run.agent_version === null ? '' : ` ${run.agent_version}`}`],
            ['workspace', run.workspace_path],
            // §13: the agent's cwd is never the user's own checkout.
            ['worktree', run.worktree_path ?? '—'],
            ['branch', run.branch ?? '—'],
            // §13.1: no shadow ref means Review has no attribution for this
            // run, and is required to say so rather than guess.
            ['shadow', run.shadow_ref ?? 'none — Review has no attribution'],
            ['session', run.provider_session_id ?? '—'],
            ['started', run.started_at],
            ['elapsed', elapsed(run.started_at, run.finished_at)],
            // The edge the graph is drawn from. Nothing forks yet, so this
            // reads "—" on every run today; it is shown anyway because the day
            // it stops doing so is the day the thesis has something to test.
            ['forked from', run.parent_run_id ?? '— not a fork'],
          ]}
        />
      </DockSection>

      <DockSection title="Cost">
        {metrics === null ? (
          <DockEmpty>No turn has finished yet, so there is nothing to sum.</DockEmpty>
        ) : (
          <Facts
            rows={[
              ['cost', money(metrics.cost_usd)],
              ['turns', String(metrics.turns)],
              ['tools', String(metrics.tool_calls)],
              ['denials', String(metrics.permission_denials)],
              ['in', metrics.input_tokens.toLocaleString()],
              ['out', metrics.output_tokens.toLocaleString()],
              ['cache r/w', `${metrics.cache_read_tokens.toLocaleString()} / ${metrics.cache_creation_tokens.toLocaleString()}`],
            ]}
          />
        )}
      </DockSection>

      <DockSection title="Steer">
        {/* Nothing closes a run's stdin on its behalf: a run started here holds
            a live agent process until one of these is pressed, or the app
            exits. */}
        <div className="flex flex-col gap-1.5 px-3 pb-3">
          <Action onClick={onEnd} disabled={!live}>
            End — close stdin, let the turn finish
          </Action>
          <Action onClick={onInterrupt} disabled={!live} danger>
            Interrupt — stop now
          </Action>
          {!live && (
            <p className="text-micro text-faint">
              This process is not supervising this run, so there is nothing to
              steer.
            </p>
          )}
        </div>
      </DockSection>

      <DockSection title="Worktree">
        {/* The one control in this app that destroys something. Nothing here is
            ever cleaned up on a timer or at exit, so a repository accumulates a
            tree and an `agent/<id>` branch per run until this is pressed — the
            cost of §13's rule that uncommitted changes are never deleted
            without confirmation, which nothing unattended can honour.

            Disabled while this process supervises the run: the agent's cwd is
            that directory and cannot be changed after spawn, so removing it
            would not stop the run, it would detach its writes from git. The
            driver refuses it too; this is the button agreeing. */}
        <div className="flex flex-col gap-1.5 px-3 pb-3">
          <Action onClick={onCleanUp} disabled={live || run.worktree_path === null} danger>
            Clean up — remove the worktree and the branch
          </Action>
          <p className="text-micro text-faint">
            {live
              ? 'End or interrupt the run first: its agent is working in this directory.'
              : run.worktree_path === null
                ? 'This run has no worktree on disk — it has been cleaned up already.'
                : 'The shadow history survives: this run stays forkable, and Review keeps its attribution.'}
          </p>
        </div>
      </DockSection>
    </div>
  )
}

function Facts({ rows }: { rows: [string, string][] }) {
  return (
    <dl className="grid grid-cols-[auto_minmax(0,1fr)] gap-x-3 gap-y-0.5 px-3 pb-3 font-mono text-mono">
      {rows.map(([key, value]) => (
        <div key={key} className="contents">
          <dt className="text-faint">{key}</dt>
          <dd className="m-0 min-w-0 break-all text-muted select-text">{value}</dd>
        </div>
      ))}
    </dl>
  )
}

function Action({
  onClick,
  disabled,
  danger,
  children,
}: {
  onClick: () => void
  disabled: boolean
  danger?: boolean
  children: React.ReactNode
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      disabled={disabled}
      className={[
        'rounded-sm border border-border bg-elevated px-2 py-1 text-left text-micro',
        disabled ? 'text-faint' : danger === true ? 'text-err hover:border-err/60' : 'text-text hover:border-accent/60',
      ].join(' ')}
    >
      {children}
    </button>
  )
}
