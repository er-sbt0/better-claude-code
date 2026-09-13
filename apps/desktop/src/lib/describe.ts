import type { RawEvent } from '@agent-workspace/shared-types/RawEvent'
import type { JsonValue } from '@agent-workspace/shared-types/serde_json/JsonValue'

// One line per raw event, which is all the scaffolding in `App.tsx` renders.
//
// This is the TypeScript of `crates/driver/examples/run.rs`'s `describe()`, and
// it is deliberately the same shape: a flat string per event, enough to see
// that a run is doing what it was asked to. §7's run feed is a different thing
// entirely — six block types, each one structured, virtualized and collapsible
// — and none of this survives into it. Nothing here should grow towards it.
//
// The exhaustiveness check at the bottom is the part worth keeping: `RawEvent`
// is generated from the Rust enum, so a new event type added to the protocol
// fails this file at compile time rather than rendering as a blank line.

/** Long text on one line, so a stream of them stays a stream. */
const clip = (text: string, max = 100): string => {
  const line = text.replace(/\s+/gu, ' ').trim()
  return line.length <= max ? line : `${line.slice(0, max - 1)}…`
}

const json = (value: JsonValue, max = 100): string => clip(JSON.stringify(value), max)

/**
 * A short human summary of one event, paired with its type name.
 *
 * The type name is returned rather than read off `event.type` by the caller so
 * that both halves of a row come from one place.
 */
export function describe(event: RawEvent): { type: string; summary: string } {
  return { type: event.type, summary: summarize(event) }
}

function summarize(event: RawEvent): string {
  switch (event.type) {
    case 'run.started': {
      const p = event.payload
      return `${p.agent} ${p.model ?? '-'}`
    }
    case 'run.finished':
      return `turns=${event.payload.turns} exit=${event.payload.exit_code ?? '-'}`
    case 'run.failed':
      return event.payload.reason
    case 'run.rate_limited':
      return json(event.payload.detail)
    case 'turn.started':
      return `index=${event.payload.index}`
    case 'turn.finished': {
      const p = event.payload
      return `${p.subtype} $${p.usage.cost_usd.toFixed(4)} denials=${p.permission_denials}`
    }
    case 'user.message':
      // §5: under `--replay-user-messages` the provider echoes our own stdin
      // back. Rendering that as a message shows the user their prompt twice.
      return event.payload.is_replay
        ? `(ack) ${clip(event.payload.text)}`
        : clip(event.payload.text)
    case 'assistant.message':
      return clip(event.payload.text)
    case 'assistant.thinking':
      // An empty text means "not disclosed", never "did not think" (§5).
      return event.payload.disclosed ? clip(event.payload.text) : '(not disclosed)'
    case 'tool.started':
      return `${event.payload.name} ${json(event.payload.input, 80)}`
    case 'tool.completed': {
      const p = event.payload
      const error = p.is_error ? ' ERROR' : ''
      // `non_execution` is the only reliable way to tell a refusal from a
      // failure (§5) — the prose in `content` is not machine-readable.
      const why = p.non_execution === null ? '' : ` (${p.non_execution})`
      return `${p.name ?? '?'}${error}${why}`
    }
    case 'permission.requested':
      return event.payload.tool_name
    case 'permission.resolved': {
      const p = event.payload
      return `${p.tool_name} allowed=${String(p.allowed)} ${p.reason_type ?? ''}`.trim()
    }
    case 'workspace.report':
      return `${event.payload.tool} ${json(event.payload.payload, 80)}`
    case 'process.output':
      return `${event.payload.stream} ${clip(event.payload.text)}`
    case 'raw.unclassified':
      return `${event.payload.reason} ${event.payload.source_type ?? ''}`.trim()
    default: {
      // A new variant in `agent-protocol` lands here as a type error, which is
      // the whole reason the switch is exhaustive rather than defaulted.
      const unreachable: never = event
      return JSON.stringify(unreachable)
    }
  }
}
