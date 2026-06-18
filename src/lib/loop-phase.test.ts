import { describe, expect, it } from "vitest"

import {
  artifactPhase,
  iterationPhase,
  type LoopPhase,
  PHASE_ORDER,
  phaseRank,
} from "@/lib/loop-phase"
import type { LoopArtifactKind, LoopStage } from "@/lib/types"

// ---------------------------------------------------------------------------
// Parity with src-tauri/src/models/loop_phase.rs (no codegen — hand-mirrored).
// The `Record<…, LoopPhase>` literals below are EXHAUSTIVE by type: if a new
// member is added to LoopArtifactKind / LoopStage, these objects stop compiling
// until its phase is assigned here — the TS-side counterpart of the Rust match's
// no-`_` exhaustiveness. Keep both files in sync.
// ---------------------------------------------------------------------------

const EXPECTED_ARTIFACT_PHASE: Record<LoopArtifactKind, LoopPhase> = {
  issue: "issue",
  requirement: "requirement",
  design: "design",
  task: "implement",
  review: "implement",
  result: "result",
  reflection: "reflect",
}

const EXPECTED_ITERATION_PHASE: Record<LoopStage, LoopPhase> = {
  triage: "issue",
  refine: "requirement",
  design: "design",
  plan: "implement",
  implement: "implement",
  review: "implement",
  finalize: "result",
  reflect: "reflect",
}

describe("PHASE_ORDER", () => {
  it("is the six phases in macro-pipeline order", () => {
    expect(PHASE_ORDER).toEqual([
      "issue",
      "requirement",
      "design",
      "implement",
      "result",
      "reflect",
    ])
  })

  it("phaseRank is strictly increasing along the pipeline", () => {
    expect(phaseRank("issue")).toBeLessThan(phaseRank("requirement"))
    expect(phaseRank("requirement")).toBeLessThan(phaseRank("design"))
    expect(phaseRank("design")).toBeLessThan(phaseRank("implement"))
    expect(phaseRank("implement")).toBeLessThan(phaseRank("result"))
    expect(phaseRank("result")).toBeLessThan(phaseRank("reflect"))
  })
})

describe("artifactPhase", () => {
  it("maps every known artifact kind to its phase", () => {
    for (const kind of Object.keys(
      EXPECTED_ARTIFACT_PHASE
    ) as LoopArtifactKind[]) {
      expect(artifactPhase(kind)).toBe(EXPECTED_ARTIFACT_PHASE[kind])
    }
  })

  it("returns null for an unrecognized kind (version mismatch)", () => {
    expect(
      artifactPhase("future_kind" as unknown as LoopArtifactKind)
    ).toBeNull()
  })
})

describe("iterationPhase", () => {
  it("maps every known stage to its phase", () => {
    for (const stage of Object.keys(EXPECTED_ITERATION_PHASE) as LoopStage[]) {
      expect(iterationPhase(stage)).toBe(EXPECTED_ITERATION_PHASE[stage])
    }
  })

  it("returns null for an unrecognized stage (version mismatch)", () => {
    expect(iterationPhase("future_stage" as unknown as LoopStage)).toBeNull()
  })

  it("keeps the triage/finalize split from artifactPhase", () => {
    // triage has no artifact node — it belongs to Issue, not Implement.
    expect(iterationPhase("triage")).toBe("issue")
    // finalize produces the result (its iteration target is NULL).
    expect(iterationPhase("finalize")).toBe("result")
  })
})
