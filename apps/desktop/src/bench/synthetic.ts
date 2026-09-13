import type { Block } from '@agent-workspace/shared-types/Block'
import type { Feed } from '@agent-workspace/shared-types/Feed'
import type { ForkPoints } from '@agent-workspace/shared-types/ForkPoints'
import type { Run } from '@agent-workspace/shared-types/Run'
import type { RunActivity } from '@agent-workspace/shared-types/RunActivity'
import type { RunId } from '@agent-workspace/shared-types/RunId'

// Synthetic runs and feeds for the WebKitGTK budget measurement
// (`docs/verification/webkitgtk-feed-budget.md`).
//
// **Why synthetic and not a recorded run.** The question §4's budget asks is
// about *scale* — several columns, each holding thousands of blocks, under
// pan/zoom — and the longest recorded fixture is twenty blocks. What has to be
// faithful is the shape of the DOM the feed produces, not the words in it, so
// the generator below emits the same block mix a real run does and every block
// goes through the same renderer the app uses.
//
// The blocks are shaped by hand rather than projected by `document-model`
// because the projection needs events and the events would then be the
// synthetic thing. This is the one place in the repository that builds a
// `Block` without deriving it, and it exists only behind the bench entry.

/** Roughly the mix a real run produces: mostly prose and tool calls. */
const MIX = ['text', 'tool_call', 'text', 'raw', 'diff', 'text', 'terminal', 'code'] as const

const LOREM =
  'Checked the refresh path and the token is read before the clock skew is applied, which is why the retry loop never terminates on a slow connection. '

export function feed(runId: RunId, count: number): Feed {
  const blocks: Block[] = []
  for (let i = 0; i < count; i++) {
    const seq = i + 1
    const kind = MIX[i % MIX.length]!
    blocks.push(block(runId, seq, kind, i))
  }
  return { run_id: runId, version: 1, blocks }
}

function block(runId: RunId, seq: number, kind: (typeof MIX)[number], i: number): Block {
  // Every third block is addressable, which is about the ratio a real run has:
  // lifecycle, permission decisions and unclassified lines carry no chain-entry
  // uuid, so the hover toolbar is offered on a minority of blocks.
  const base = {
    run_id: runId,
    seqs: [seq],
    fork_seq: i % 3 === 0 ? seq : null,
    summary: `${kind} · block ${i}`,
  }
  switch (kind) {
    case 'text':
      return {
        ...base,
        type: 'text',
        block: {
          speaker: i % 7 === 0 ? 'user' : 'assistant',
          text: LOREM.repeat(1 + (i % 3)),
          disclosed: true,
        },
      }
    case 'code':
      return {
        ...base,
        type: 'code',
        block: {
          language: 'rust',
          code: Array.from({ length: 8 }, (_, n) => `    let line_${n} = value(${n});`).join('\n'),
        },
      }
    case 'tool_call':
      return {
        ...base,
        seqs: [seq, seq + 1],
        type: 'tool_call',
        block: {
          name: 'Read',
          input: { file_path: `/repo/src/module_${i}.ts` },
          status: 'ok',
          output: `read ${20 + (i % 40)} lines`,
          non_execution: null,
          duration_ms: 12 + (i % 90),
          parent_tool_use_id: null,
        },
      }
    case 'diff':
      return {
        ...base,
        seqs: [seq, seq + 1],
        type: 'diff',
        block: {
          tool: 'Edit',
          files: [`/repo/src/module_${i}.ts`],
          hunks: [
            {
              file: `/repo/src/module_${i}.ts`,
              lines: [
                { kind: 'context', text: 'export function refresh(token: Token) {' },
                { kind: 'removed', text: '  return fetchToken(token)' },
                { kind: 'added', text: '  return fetchToken(token, { skew: CLOCK_SKEW })' },
                { kind: 'context', text: '}' },
              ],
            },
          ],
          stats: { additions: 1, deletions: 1 },
          status: 'ok',
          duration_ms: 30 + (i % 50),
        },
      }
    case 'terminal':
      return {
        ...base,
        seqs: [seq, seq + 1],
        type: 'terminal',
        block: {
          command: `cargo test -p crate_${i} -- --nocapture`,
          description: 'run the crate tests',
          output: Array.from({ length: 10 }, (_, n) => `test case_${n} ... ok`).join('\n'),
          exit_code: null,
          status: 'ok',
          duration_ms: 900 + (i % 600),
        },
      }
    default:
      return {
        ...base,
        fork_seq: null,
        type: 'raw',
        block: {
          event_type: 'turn.finished',
          payload: { index: i, subtype: 'success', ok: true },
          line: null,
        },
      }
  }
}

/** A binary tree of runs, so the layout pass has real depth and real siblings. */
export function runs(count: number): Run[] {
  const out: Run[] = []
  for (let i = 0; i < count; i++) {
    const parent = i === 0 ? null : out[Math.floor((i - 1) / 2)]!
    out.push({
      id: `bench_${i}`,
      agent: 'claude-code',
      agent_version: '2.1.270',
      provider_session_id: null,
      workspace_path: '/repo',
      worktree_path: `/repo/.worktrees/bench_${i}`,
      branch: `agent/bench_${i}`,
      shadow_ref: `refs/agent-workspace/runs/bench_${i}`,
      prompt: 'Fix the refresh path so a slow connection does not loop forever.',
      model: 'claude-sonnet-5',
      launch_flags: {
        allowed_tools: ['Read', 'Edit'],
        disallowed_tools: [],
        permission_mode: null,
        max_turns: null,
        max_budget_usd: null,
        append_system_prompt: null,
        extra: [],
      },
      status: i % 4 === 0 ? 'running' : 'finished',
      started_at: new Date(Date.UTC(2026, 8, 13, 12, i)).toISOString(),
      finished_at: null,
      parent_run_id: parent === null ? null : parent.id,
      branch_point_seq: parent === null ? null : 7,
    })
  }
  return out
}

export function activity(all: Run[]): Record<RunId, RunActivity> {
  return Object.fromEntries(
    all.map((run) => [
      run.id,
      {
        run_id: run.id,
        status: run.status === 'running' ? 'running' : 'idle',
        messages: 42,
      } satisfies RunActivity,
    ]),
  )
}

export function forkPoints(all: Run[], blocks: number): Record<RunId, ForkPoints> {
  return Object.fromEntries(
    all.map((run) => [
      run.id,
      {
        run_id: run.id,
        blocks: Array.from({ length: Math.ceil(blocks / 3) }, (_, i) => ({
          seq: i * 3 + 1,
          inherited_messages: i,
        })),
      } satisfies ForkPoints,
    ]),
  )
}
