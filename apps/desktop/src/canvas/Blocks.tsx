import type { Block } from '@agent-workspace/shared-types/Block'
import type { Diff } from '@agent-workspace/shared-types/Diff'
import type { Terminal } from '@agent-workspace/shared-types/Terminal'
import type { ToolCall } from '@agent-workspace/shared-types/ToolCall'
import type { ToolStatus } from '@agent-workspace/shared-types/ToolStatus'

// ---------------------------------------------------------------------------
// §7's six block types, drawn.
//
// Plain React components over the domain model, which is §9's whole rule:
// "Run feed → plain React components in a virtualized list." Nothing here is an
// editor, nothing here holds state, and nothing here decides what a block *is*
// — that is `crates/document-model`, and this file switches on the answer.
//
// The division is worth stating because it is the one that will be violated
// first: a renderer that starts parsing `block.text`, or inferring a status
// from a string, or deciding which events belong together, has moved a decision
// into the shell where no test in this workspace can reach it (roadmap §8).
// Everything below reads fields.
// ---------------------------------------------------------------------------

/** One colour per block type and status, from the theme's status vocabulary. */
const STATUS: Record<ToolStatus, { text: string; label: string }> = {
  running: { text: 'text-accent', label: 'running' },
  ok: { text: 'text-ok', label: 'ok' },
  failed: { text: 'text-err', label: 'failed' },
  // §5: never ran, which is not the same as ran and failed — and the provider
  // reports both as an error, so this distinction only exists because
  // `non_execution` carries it.
  denied: { text: 'text-warn', label: 'denied' },
}

export function BlockView({ block }: { block: Block }) {
  switch (block.type) {
    case 'text': {
      const { speaker, text, disclosed } = block.block
      if (speaker === 'thinking') {
        return (
          <div className="px-3 py-1.5">
            <Label className="text-alt">thinking</Label>
            <p className="mt-0.5 whitespace-pre-wrap text-mono text-faint italic select-text">
              {/* §5: an empty text means the run was launched without
                  `--thinking-display summarized`, never that it did not think.
                  Saying the first is honest; showing an empty block implies the
                  second. */}
              {disclosed ? text : 'not disclosed — the run was launched without a thinking display'}
            </p>
          </div>
        )
      }
      // The one asymmetry in the feed, and the reason it is here rather than in
      // the projection: who spoke is a fact (`document-model` decides it), how
      // a speaker is drawn is not.
      //
      // A user turn is short, sparse and is the thing the reader is scanning
      // *for* when they come back to a column — it is where they last steered.
      // Everything an agent produces is left-aligned and full width, because a
      // `Diff` or a `Terminal` in a bubble is a diff you cannot read. So the
      // bubble is not "chat styling"; it is the one shape in the column that
      // means *you said this*, which is what makes the branch points findable.
      if (speaker === 'user') {
        return (
          <div className="flex justify-end px-3 py-1.5">
            <p className="max-w-[78%] rounded-lg rounded-br-sm bg-selected px-2.5 py-1.5 whitespace-pre-wrap text-ui text-text select-text">
              {text}
            </p>
          </div>
        )
      }
      // No `agent` caption. The column header already says which agent and
      // which model, on every column, all the time — repeating it above every
      // paragraph is the noise that makes a feed read as a transcript rather
      // than as the half of a conversation the reader is actually having. The
      // user's turn is the one that needs marking, and the bubble marks it.
      return (
        <div className="px-3 py-1.5">
          <p className="whitespace-pre-wrap text-ui text-text select-text">{text}</p>
        </div>
      )
    }

    case 'code': {
      const { language, code } = block.block
      return (
        <div className="px-3 py-1.5">
          <Label className="text-info">{language ?? 'code'}</Label>
          {/* No highlighter. §4 budgets CodeMirror for the *diff viewer*, one
              instance at a time, and an editor instance per feed block is the
              thing that same section forbids by name. */}
          <pre className="mt-0.5 overflow-x-auto rounded-sm border border-border-soft bg-bg px-2 py-1 font-mono text-mono text-text select-text">
            {code.replace(/\n$/u, '')}
          </pre>
        </div>
      )
    }

    case 'tool_call':
      return <ToolCallView call={block.block} />

    case 'diff':
      return <DiffView diff={block.block} />

    case 'terminal':
      return <TerminalView terminal={block.block} />

    case 'raw': {
      const { event_type, payload, line } = block.block
      return (
        <div className="px-3 py-1">
          <Label className="text-faint">{event_type}</Label>
          {/* The unprocessed transcript, one click away (§7). `line` is the
              provider's own bytes when the *normalizer* could not classify
              them; otherwise this is the event payload, which is the closest
              thing to bytes that exists for an event we produced. */}
          <pre className="mt-0.5 overflow-x-auto font-mono text-micro text-muted select-text">
            {line ?? JSON.stringify(payload, null, 1)}
          </pre>
        </div>
      )
    }
  }

  // **The degradation invariant, on this side of the boundary** (§7): "an
  // unknown `type` renders as `Raw` and is preserved verbatim on round-trip".
  //
  // Unreachable for every type this build knows — `Block` is generated from the
  // Rust enum, so the line below fails to compile the day a seventh type is
  // added and nobody wrote a renderer for it. It is reachable at *runtime*,
  // which is the case it exists for: a document projected by a newer backend
  // than this bundle, which §9.1's stored artifacts make an ordinary thing
  // rather than a hypothetical.
  const unreachable: never = block
  const unknown = unreachable as Block
  return (
    <div className="px-3 py-1">
      <Label className="text-warn">{unknown.type} — unknown block type</Label>
      <pre className="mt-0.5 overflow-x-auto font-mono text-micro text-muted select-text">
        {JSON.stringify(unknown, null, 1)}
      </pre>
    </div>
  )
}

function ToolCallView({ call }: { call: ToolCall }) {
  const status = STATUS[call.status]
  return (
    <div className="px-3 py-1.5">
      <div className="flex items-baseline gap-2">
        <Label className="text-info">{call.name}</Label>
        <span className={`font-mono text-micro ${status.text}`}>{status.label}</span>
        {call.non_execution !== null && (
          <span className="font-mono text-micro text-warn">{call.non_execution}</span>
        )}
        <Duration ms={call.duration_ms} />
        {call.parent_tool_use_id !== null && (
          // §13.1 wants this for attribution granularity and nothing has ever
          // seen it non-null. Shown rather than hidden, because the first run
          // that produces one is worth noticing.
          <span className="font-mono text-micro text-alt">subagent</span>
        )}
      </div>
      {call.input !== null && (
        <pre className="mt-0.5 overflow-x-auto font-mono text-micro text-muted select-text">
          {JSON.stringify(call.input)}
        </pre>
      )}
      {call.output !== null && call.output !== '' && (
        <Output text={call.output} />
      )}
    </div>
  )
}

function DiffView({ diff }: { diff: Diff }) {
  const status = STATUS[diff.status]
  return (
    <div className="px-3 py-1.5">
      <div className="flex items-baseline gap-2">
        <Label className="text-alt">{diff.tool}</Label>
        <span className="min-w-0 truncate font-mono text-micro text-muted">
          {diff.files.join(', ')}
        </span>
        <span className="shrink-0 font-mono text-micro text-ok">+{diff.stats.additions}</span>
        <span className="shrink-0 font-mono text-micro text-err">−{diff.stats.deletions}</span>
        <span className={`shrink-0 font-mono text-micro ${status.text}`}>{status.label}</span>
        <Duration ms={diff.duration_ms} />
      </div>
      {/* What the agent *said it was doing*, off the tool's own arguments (§6
          source 1). Not §13.1's attribution, which blames a shadow history and
          catches the formatter that ran afterwards — that is Review's surface,
          and the two disagreeing is information rather than a bug. */}
      <div className="mt-0.5 overflow-x-auto rounded-sm border border-border-soft bg-bg font-mono text-mono select-text">
        {diff.hunks.map((hunk, h) =>
          hunk.lines.map((line, i) => (
            <div
              key={`${h}:${i}`}
              className={
                line.kind === 'added'
                  ? 'bg-ok/10 text-ok'
                  : line.kind === 'removed'
                    ? 'bg-err/10 text-err'
                    : 'text-faint'
              }
            >
              <span className="inline-block w-4 text-center select-none">
                {line.kind === 'added' ? '+' : line.kind === 'removed' ? '−' : ' '}
              </span>
              {line.text}
            </div>
          )),
        )}
      </div>
    </div>
  )
}

function TerminalView({ terminal }: { terminal: Terminal }) {
  const status = STATUS[terminal.status]
  return (
    <div className="px-3 py-1.5">
      <div className="flex items-baseline gap-2">
        <span className="shrink-0 font-mono text-micro text-faint">$</span>
        <span className="min-w-0 flex-1 truncate font-mono text-mono text-text select-text">
          {terminal.command}
        </span>
        <span className={`shrink-0 font-mono text-micro ${status.text}`}>{status.label}</span>
        <Duration ms={terminal.duration_ms} />
      </div>
      {terminal.description !== null && (
        <p className="text-micro text-faint">{terminal.description}</p>
      )}
      {/* No xterm. §4 budgets xterm.js for a *terminal* — a live PTY the user
          types into — and this is a recorded command's output, which is text.
          An emulator per feed block is the per-block editor instance §4
          forbids, in a different costume. */}
      {terminal.output !== '' && <Output text={terminal.output} />}
    </div>
  )
}

/**
 * Tool and command output.
 *
 * Capped by height rather than by characters, and scrollable inside its own
 * box. `data-scrollable` is load-bearing: the canvas's wheel handler pans
 * unless the gesture started somewhere that says it scrolls, so without it this
 * region cannot be scrolled at all.
 */
function Output({ text }: { text: string }) {
  return (
    <pre
      data-scrollable="true"
      className="mt-0.5 max-h-40 overflow-auto rounded-sm border border-border-soft bg-bg px-2 py-1 font-mono text-micro text-muted select-text"
    >
      {text.replace(/\n$/u, '')}
    </pre>
  )
}

function Duration({ ms }: { ms: number | null }) {
  // A duration we could not compute is absent rather than zero: a call that
  // took no time and a call whose timestamps did not parse are different facts.
  if (ms === null) return null
  return (
    <span className="shrink-0 font-mono text-micro text-faint">
      {ms < 1000 ? `${ms}ms` : `${(ms / 1000).toFixed(1)}s`}
    </span>
  )
}

function Label({ className, children }: { className: string; children: React.ReactNode }) {
  return (
    <span className={`font-mono text-micro tracking-label uppercase ${className}`}>{children}</span>
  )
}
