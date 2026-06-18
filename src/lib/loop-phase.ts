import type { LoopArtifactKind, LoopStage } from "@/lib/types"

/**
 * The single authoritative loop-phase taxonomy (TS mirror).
 *
 * Hand-mirrored from `src-tauri/src/models/loop_phase.rs` — there is no codegen.
 * The two `*_phase` functions there are the authority; the parity test in
 * `loop-phase.test.ts` keeps these in lockstep. **Change one side, change both.**
 *
 * A loop issue advances through six ordered macro phases. Both the DAG process
 * graph and the stage rail derive their grouping from the two classifiers here,
 * so the macro pipeline lives in exactly one place and cannot drift between views.
 *
 * Two classifiers, deliberately distinct (Codex #B2): some stages run without
 * producing a node (`triage` sets the route; `finalize` produces a `result` whose
 * iteration `target` is NULL), so an iteration's phase is not its artifact's phase.
 */
export type LoopPhase =
  | "issue"
  | "requirement"
  | "design"
  | "implement"
  | "result"
  | "reflect"

/**
 * The six phases in order. Index in this array IS the phase rank — connector
 * folding uses it to normalize every lineage edge to a canonical `earlier →
 * later` direction (mirrors Rust `LoopPhase`'s `Ord`). Never reorder.
 */
export const PHASE_ORDER: readonly LoopPhase[] = [
  "issue",
  "requirement",
  "design",
  "implement",
  "result",
  "reflect",
]

/** Phase rank (0-based) for ordering comparisons; matches {@link PHASE_ORDER}. */
export function phaseRank(phase: LoopPhase): number {
  return PHASE_ORDER.indexOf(phase)
}

/**
 * Which phase **container** an artifact node lives in. Returns `null` for a kind
 * this frontend does not recognize (version mismatch: a newer server introduced a
 * kind) — the caller counts it as an unmapped artifact rather than forcing it into
 * a phase or inventing a seventh one (spec §3.1).
 */
export function artifactPhase(kind: LoopArtifactKind): LoopPhase | null {
  switch (kind) {
    case "issue":
      return "issue"
    case "requirement":
      return "requirement"
    case "design":
      return "design"
    case "task":
    case "review":
      return "implement"
    case "result":
      return "result"
    case "reflection":
      return "reflect"
    default:
      return null
  }
}

/**
 * Which phase an **iteration / session** (ghost or sessionRef) belongs to.
 * Returns `null` for an unrecognized stage (version mismatch) — counted as an
 * unmapped iteration. Distinct from {@link artifactPhase}: `triage` → issue (no
 * artifact), `plan`/`implement`/`review` → implement, `finalize` → result.
 */
export function iterationPhase(stage: LoopStage): LoopPhase | null {
  switch (stage) {
    case "triage":
      return "issue"
    case "refine":
      return "requirement"
    case "design":
      return "design"
    case "plan":
    case "implement":
    case "review":
      return "implement"
    case "finalize":
      return "result"
    case "reflect":
      return "reflect"
    default:
      return null
  }
}
