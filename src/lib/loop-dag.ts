import type { LoopArtifactRow, LoopLinkRow } from "@/lib/types"

/**
 * Pure, stable layout helpers shared by the process-graph renderer and the
 * `buildProcessGraph` model. The old artifact-level `buildDag` (read-stage
 * columns + lineage edge-soup) was retired when the renderer switched to the
 * six-phase {@link import("@/lib/loop-process-graph").ProcessGraph}; what
 * remains here is the genuinely reusable geometry: the `depends_on` task forest
 * placement, review folding, and ghost stacking.
 */

export const bySortId = (a: LoopArtifactRow, b: LoopArtifactRow) =>
  a.sort - b.sort || a.id - b.id

/**
 * Lay out the `depends_on` task forest: each task's dependency-chain depth (its
 * pipeline column offset) and lane (its vertical band among parallel chains).
 * Pure and stable — used by `buildProcessGraph` over the full task set (dead
 * included) so placement is identical whether or not dead nodes are revealed.
 *
 * `byId` must map artifact id → artifact for at least every task in `tasks`;
 * `depends_on` edges whose endpoints aren't both tasks present in `byId` are
 * ignored (v1 is a forest: ≤1 predecessor, first parent kept defensively).
 */
export function layoutTaskForest(
  tasks: LoopArtifactRow[],
  links: LoopLinkRow[],
  byId: Map<number, LoopArtifactRow>
): { depthOf: Map<number, number>; laneOf: Map<number, number> } {
  // --- Dependency forest over tasks (depends_on: from = child, to = parent). ---
  const parentOf = new Map<number, number>()
  for (const l of links) {
    if (l.kind !== "depends_on") continue
    const child = byId.get(l.from_artifact_id)
    const parent = byId.get(l.to_artifact_id)
    if (!child || child.kind !== "task") continue
    if (!parent || parent.kind !== "task") continue
    // v1 is a forest (≤1 predecessor); keep the first parent defensively.
    if (!parentOf.has(child.id)) parentOf.set(child.id, parent.id)
  }

  // depth = dependency-chain length to a root (0 = no predecessor). `seen` guards
  // against a stray cycle (the backend enforces acyclicity).
  const depthOf = new Map<number, number>()
  const depth = (id: number, seen: Set<number>): number => {
    const cached = depthOf.get(id)
    if (cached !== undefined) return cached
    const parent = parentOf.get(id)
    let d = 0
    if (parent !== undefined && !seen.has(id)) {
      seen.add(id)
      d = depth(parent, seen) + 1
    }
    depthOf.set(id, d)
    return d
  }
  for (const t of tasks) depth(t.id, new Set())

  // children index, each sorted by (sort, id) for stable lane assignment.
  const childrenOf = new Map<number, LoopArtifactRow[]>()
  for (const t of tasks) {
    const p = parentOf.get(t.id)
    if (p === undefined) continue
    const bucket = childrenOf.get(p)
    if (bucket) bucket.push(t)
    else childrenOf.set(p, [t])
  }
  for (const bucket of childrenOf.values()) bucket.sort(bySortId)

  // Tidy lane assignment: a parent shares its first child's lane (chains run
  // horizontally); extra children + independent roots take fresh lanes below.
  const laneOf = new Map<number, number>()
  let nextLane = 0
  const assignLane = (id: number, seen: Set<number>): number => {
    const existing = laneOf.get(id)
    if (existing !== undefined) return existing
    seen.add(id)
    const kids = (childrenOf.get(id) ?? []).filter((c) => !seen.has(c.id))
    const lane =
      kids.length === 0
        ? nextLane++
        : kids.map((c) => assignLane(c.id, seen))[0]
    laneOf.set(id, lane)
    return lane
  }
  for (const r of tasks.filter((t) => !parentOf.has(t.id)).sort(bySortId)) {
    assignLane(r.id, new Set())
  }
  // Defensive: any task unreached by the forest walk still gets its own lane.
  for (const t of tasks) if (!laneOf.has(t.id)) laneOf.set(t.id, nextLane++)

  return { depthOf, laneOf }
}

/**
 * A ghost's column + intra-column row — the minimal placement input
 * {@link placeGhosts} needs. The renderer derives these from a phase's pending
 * iterations; this stays decoupled from any richer ghost shape.
 */
export interface GhostPlacement {
  iterationId: number
  col: number
  row: number
}

/**
 * Final pixel `y` for each ghost, keyed by `iterationId`. A ghost stacks
 * strictly below its column's measured real-node bottom (or the top `pad` when
 * the column holds no real nodes); ghosts sharing a column stack by `rowPitch`.
 *
 * Pure — no rendering imports — so the no-overlap guarantee is unit-testable. The
 * geometry layer supplies `columnBottom` (the measured pixel bottom of every
 * column's real nodes), which it alone can compute: review-fold height and lane
 * packing make a column's extent invisible to the model.
 */
export function placeGhosts(
  pending: GhostPlacement[],
  columnBottom: Map<number, number>,
  geom: { pad: number; rowPitch: number; gap: number }
): Map<number, number> {
  const yByIteration = new Map<number, number>()
  for (const p of pending) {
    const bottom = columnBottom.get(p.col)
    const base = bottom === undefined ? geom.pad : bottom + geom.gap
    yByIteration.set(p.iterationId, base + p.row * geom.rowPitch)
  }
  return yByIteration
}

/**
 * Split a task's reviews into the latest attempt (shown expanded) and the count
 * of older-attempt reviews (folded into a "+N earlier" chip). Reviews are
 * assumed pre-sorted oldest → newest (by attempt, sort, id).
 */
export function foldReviews(reviews: LoopArtifactRow[]): {
  latest: LoopArtifactRow[]
  olderCount: number
} {
  if (reviews.length === 0) return { latest: [], olderCount: 0 }
  const latestAttempt = reviews.reduce((m, r) => Math.max(m, r.attempt), 0)
  const latest = reviews.filter((r) => r.attempt === latestAttempt)
  return { latest, olderCount: reviews.length - latest.length }
}
