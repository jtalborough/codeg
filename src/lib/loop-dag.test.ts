import { describe, expect, it } from "vitest"

import {
  bySortId,
  foldReviews,
  type GhostPlacement,
  layoutTaskForest,
  placeGhosts,
} from "@/lib/loop-dag"
import type {
  LoopArtifactKind,
  LoopArtifactRow,
  LoopLinkKind,
  LoopLinkRow,
} from "@/lib/types"

let nextId = 1

function artifact(
  kind: LoopArtifactKind,
  extra: Partial<LoopArtifactRow> = {}
): LoopArtifactRow {
  return {
    id: nextId++,
    issue_id: 1,
    issue_seq: 1,
    kind,
    title: `${kind}-${nextId}`,
    status: "done",
    origin: "agent",
    produced_by_iteration_id: null,
    verdict: null,
    contribution_kind: "delta",
    attempt: 0,
    sort: 0,
    updated_at: "2026-06-18T00:00:00Z",
    ...extra,
  }
}

function link(
  from: number,
  to: number,
  kind: LoopLinkKind = "depends_on"
): LoopLinkRow {
  return {
    id: nextId++,
    from_artifact_id: from,
    to_artifact_id: to,
    kind,
    source_revision_id: null,
  }
}

const indexOf = (...rows: LoopArtifactRow[]) =>
  new Map(rows.map((a) => [a.id, a]))

describe("layoutTaskForest", () => {
  it("places independent tasks at depth 0 in distinct lanes", () => {
    const a = artifact("task", { sort: 0 })
    const b = artifact("task", { sort: 1 })
    const { depthOf, laneOf } = layoutTaskForest([a, b], [], indexOf(a, b))
    expect(depthOf.get(a.id)).toBe(0)
    expect(depthOf.get(b.id)).toBe(0)
    expect(laneOf.get(a.id)).not.toBe(laneOf.get(b.id))
  })

  it("runs a depends_on chain rightward (increasing depth) in a shared lane", () => {
    const a = artifact("task", { sort: 0 })
    const b = artifact("task", { sort: 1 })
    // b depends_on a → b is one chain step deeper, in a's lane.
    const { depthOf, laneOf } = layoutTaskForest(
      [a, b],
      [link(b.id, a.id, "depends_on")],
      indexOf(a, b)
    )
    expect(depthOf.get(a.id)).toBe(0)
    expect(depthOf.get(b.id)).toBe(1)
    expect(laneOf.get(b.id)).toBe(laneOf.get(a.id))
  })

  it("fans a parent's extra children into new lanes at the next depth", () => {
    const a = artifact("task", { sort: 0 })
    const b = artifact("task", { sort: 1 })
    const c = artifact("task", { sort: 2 })
    const { depthOf, laneOf } = layoutTaskForest(
      [a, b, c],
      [link(b.id, a.id, "depends_on"), link(c.id, a.id, "depends_on")],
      indexOf(a, b, c)
    )
    expect(depthOf.get(b.id)).toBe(1)
    expect(depthOf.get(c.id)).toBe(1)
    expect(laneOf.get(b.id)).toBe(laneOf.get(a.id)) // parent aligns with 1st child
    expect(laneOf.get(c.id)).not.toBe(laneOf.get(b.id)) // 2nd child its own lane
  })

  it("ignores depends_on links whose endpoints aren't both tasks", () => {
    const issue = artifact("issue")
    const task = artifact("task")
    // A (malformed) task→issue depends_on must not give the task a depth.
    const { depthOf } = layoutTaskForest(
      [task],
      [link(task.id, issue.id, "depends_on")],
      indexOf(issue, task)
    )
    expect(depthOf.get(task.id)).toBe(0)
  })
})

describe("bySortId", () => {
  it("orders by sort, then id", () => {
    const a = artifact("task", { sort: 1 })
    const b = artifact("task", { sort: 0 })
    const c = artifact("task", { sort: 0 })
    expect([a, b, c].sort(bySortId).map((x) => x.id)).toEqual([
      b.id,
      c.id,
      a.id,
    ])
  })
})

describe("foldReviews", () => {
  it("expands the latest attempt and folds older attempts into a count", () => {
    const reviews = [
      artifact("review", { attempt: 0 }),
      artifact("review", { attempt: 0 }),
      artifact("review", { attempt: 1 }),
    ]
    const { latest, olderCount } = foldReviews(reviews)
    expect(latest.every((r) => r.attempt === 1)).toBe(true)
    expect(latest).toHaveLength(1)
    expect(olderCount).toBe(2)
  })

  it("returns empty for a task with no reviews", () => {
    expect(foldReviews([])).toEqual({ latest: [], olderCount: 0 })
  })
})

describe("placeGhosts", () => {
  const geom = { pad: 8, rowPitch: 76, gap: 18 }
  const ghost = (
    iterationId: number,
    col: number,
    row: number
  ): GhostPlacement => ({ iterationId, col, row })

  it("stacks a ghost strictly below its column's measured real-node bottom", () => {
    const y = placeGhosts([ghost(1, 3, 0)], new Map([[3, 300]]), geom)
    // Below the column's pixel bottom + gap — never overlapping it, however tall
    // the real nodes (review-fold clusters) measured.
    expect(y.get(1)).toBe(300 + geom.gap)
  })

  it("places a ghost at the top pad when its column has no real nodes", () => {
    const y = placeGhosts([ghost(1, 3, 0)], new Map(), geom)
    expect(y.get(1)).toBe(geom.pad)
  })

  it("stacks multiple ghosts in one column by rowPitch", () => {
    const y = placeGhosts(
      [ghost(1, 3, 0), ghost(2, 3, 1)],
      new Map([[3, 300]]),
      geom
    )
    expect(y.get(1)).toBe(318)
    expect(y.get(2)).toBe(318 + geom.rowPitch)
  })
})
