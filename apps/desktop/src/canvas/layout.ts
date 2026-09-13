import type { Run } from '@agent-workspace/shared-types/Run'
import type { RunId } from '@agent-workspace/shared-types/RunId'

// ---------------------------------------------------------------------------
// The layout pass for §10's run graph.
//
// §19 is explicit about what this may not be: "a layout pass plus a pan/zoom
// transform, no drawing library". The user forks, collapses and navigates —
// they never place a node — so nothing here is interactive and nothing here is
// persisted. Give it runs, get back coordinates.
//
// The shape is a forest, not a general DAG: a run has at most one parent
// (`parent_run_id`), because a fork descends from exactly one point in exactly
// one run. That is what makes fifty lines enough.
//
// Nothing here knows what a node *is*. It is handed `sizeOf` — which today
// answers 560×620 for every run and under map view (⌘\, not built) will answer
// with §10's shape card — and that seam is why a second representation of this
// canvas costs no change to this file.
// ---------------------------------------------------------------------------

const GAP_X = 72
const GAP_Y = 14

/** What a node measures. A conversation column is 560 wide and a fixed height:
 *  its body scrolls rather than the node growing
 *  (`docs/branching-surface.md`). */
export type Size = { w: number; h: number }

export type Placed = {
  run: Run
  depth: number
  x: number
  y: number
  w: number
  h: number
}

export type Wire = {
  id: string
  /** An SVG path: out of the parent's right edge, into the child's left. */
  d: string
}

export type Graph = {
  nodes: Placed[]
  edges: Wire[]
  width: number
  height: number
}

/**
 * Lay a set of runs out left to right, parents before children.
 *
 * `sizeOf` is passed in rather than computed here because a node's size is a
 * fact about what the node draws, and the packing below has to know it before
 * it can promise two nodes do not overlap.
 */
export function layout(runs: Run[], sizeOf: (run: Run) => Size): Graph {
  const byId = new Map<RunId, Run>(runs.map((r) => [r.id, r]))

  // A parent outside this set is no parent at all: the run is drawn as a root
  // rather than as an edge into nothing. This is the normal case while the
  // caller is showing a window of recent runs.
  const parentOf = (run: Run): Run | null => {
    const id = run.parent_run_id
    return id === null ? null : (byId.get(id) ?? null)
  }

  const children = new Map<RunId, Run[]>()
  const roots: Run[] = []
  for (const run of runs) {
    const parent = parentOf(run)
    if (parent === null) {
      roots.push(run)
      continue
    }
    const siblings = children.get(parent.id)
    if (siblings === undefined) children.set(parent.id, [run])
    else siblings.push(run)
  }

  // Oldest first *within* the graph, though `recent_runs` hands them over
  // newest first: a branch that reads right-to-left and bottom-to-top is a
  // puzzle. The caller's ordering is for lists; this one is for the eye.
  const chronological = (a: Run, b: Run) => a.started_at.localeCompare(b.started_at)
  roots.sort(chronological)
  for (const siblings of children.values()) siblings.sort(chronological)

  // Columns are as wide as their widest node, so a node that measures
  // differently from its siblings never slides its children underneath it.
  const depthOf = new Map<RunId, number>()
  const rank = (run: Run): number => {
    const known = depthOf.get(run.id)
    if (known !== undefined) return known
    const parent = parentOf(run)
    const depth = parent === null ? 0 : rank(parent) + 1
    depthOf.set(run.id, depth)
    return depth
  }
  const columnWidth: number[] = []
  for (const run of runs) {
    const depth = rank(run)
    columnWidth[depth] = Math.max(columnWidth[depth] ?? 0, sizeOf(run).w)
  }
  const columnX: number[] = []
  let x = 0
  for (let i = 0; i < columnWidth.length; i++) {
    columnX[i] = x
    x += (columnWidth[i] ?? 0) + GAP_X
  }

  const nodes: Placed[] = []
  const placed = new Map<RunId, Placed>()
  // The lowest point already occupied in each column, so that a parent pushed
  // down to centre on its children can never land on top of another subtree's
  // node at the same depth.
  const columnBottom: number[] = []
  let cursor = 0

  const walk = (run: Run, depth: number): Placed => {
    const kids = children.get(run.id) ?? []
    const { w, h } = sizeOf(run)

    // Leaves are what actually consume vertical space; everything else is
    // centred on what it produced.
    const placedKids = kids.map((kid) => walk(kid, depth + 1))
    let y: number
    if (placedKids.length === 0) {
      y = cursor
    } else {
      const top = placedKids[0]!
      const bottom = placedKids[placedKids.length - 1]!
      y = (top.y + bottom.y + bottom.h - h) / 2
    }

    const floor = columnBottom[depth] ?? 0
    if (y < floor) y = floor
    columnBottom[depth] = y + h + GAP_Y
    cursor = Math.max(cursor, y + h + GAP_Y)

    const node: Placed = { run, depth, x: columnX[depth] ?? 0, y, w, h }
    nodes.push(node)
    placed.set(run.id, node)
    return node
  }

  for (const root of roots) walk(root, 0)

  const edges: Wire[] = []
  for (const node of nodes) {
    const parent = parentOf(node.run)
    if (parent === null) continue
    const from = placed.get(parent.id)
    if (from === undefined) continue
    const x1 = from.x + from.w
    const y1 = from.y + Math.min(from.h / 2, 26)
    const x2 = node.x
    const y2 = node.y + Math.min(node.h / 2, 26)
    // Orthogonal with a rounded turn: the same elbow §10's sketch draws, and
    // legible at the zoom levels where a curve becomes a smudge.
    const mid = x1 + GAP_X / 2
    const r = Math.min(8, Math.abs(y2 - y1) / 2)
    const dir = y2 > y1 ? 1 : -1
    const d =
      r < 1
        ? `M ${x1} ${y1} H ${x2}`
        : `M ${x1} ${y1} H ${mid - r} Q ${mid} ${y1} ${mid} ${y1 + r * dir} V ${y2 - r * dir} Q ${mid} ${y2} ${mid + r} ${y2} H ${x2}`
    edges.push({ id: `${from.run.id}->${node.run.id}`, d })
  }

  const width = nodes.reduce((max, n) => Math.max(max, n.x + n.w), 0)
  const height = nodes.reduce((max, n) => Math.max(max, n.y + n.h), 0)
  return { nodes, edges, width, height }
}
