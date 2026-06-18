//! The single authoritative loop-phase taxonomy.
//!
//! A loop issue advances through six ordered macro phases. Both the DAG process
//! graph (`ProcessGraph`) and the stage pipeline rail derive their phase grouping
//! from the two total functions here, so the macro pipeline is defined in exactly
//! one place and can never drift between the two views.
//!
//! There are deliberately **two** classifiers — one for artifacts, one for
//! iterations — because some stages run without producing a node (`triage` sets
//! the route but yields no artifact; `finalize` produces a `result` whose
//! iteration `target` is NULL). Conflating them is the bug Codex #B2 flagged.
//!
//! This taxonomy is process-derived and is **not persisted**; it is mirrored in
//! `src/lib/loop-phase.ts` (kept in sync by parity tests on both sides — there is
//! no codegen). Each classifier is an exhaustive `match` with no `_` arm, so a new
//! `ArtifactKind`/`Stage` variant fails to compile until its phase is assigned.

use serde::{Deserialize, Serialize};

use crate::db::entities::loop_artifact::ArtifactKind;
use crate::db::entities::loop_iteration::Stage;

/// The six ordered macro phases of a loop issue. Declaration order **is** phase
/// order: `Ord` ranks `Issue < Requirement < Design < Implement < Result <
/// Reflect`, which the connector folding relies on to normalize every lineage
/// edge to a canonical `earlier → later` direction.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum LoopPhase {
    Issue,
    Requirement,
    Design,
    Implement,
    Result,
    Reflect,
}

/// Which phase **container** an artifact node lives in. Total over `ArtifactKind`.
/// `task` and `review` both land in `Implement` (a review folds into the task it
/// reviews); `reflection` closes the trace in `Reflect`.
pub fn artifact_phase(kind: ArtifactKind) -> LoopPhase {
    match kind {
        ArtifactKind::Issue => LoopPhase::Issue,
        ArtifactKind::Requirement => LoopPhase::Requirement,
        ArtifactKind::Design => LoopPhase::Design,
        ArtifactKind::Task | ArtifactKind::Review => LoopPhase::Implement,
        ArtifactKind::Result => LoopPhase::Result,
        ArtifactKind::Reflection => LoopPhase::Reflect,
    }
}

/// Which phase an **iteration / session** (ghost or sessionRef) belongs to. Total
/// over `Stage`. Deliberately distinct from [`artifact_phase`]: `triage` has no
/// artifact (sits in `Issue`), and `plan`/`implement`/`review` all advance the
/// `Implement` phase, while `finalize` produces the `Result`.
pub fn iteration_phase(stage: Stage) -> LoopPhase {
    match stage {
        Stage::Triage => LoopPhase::Issue,
        Stage::Refine => LoopPhase::Requirement,
        Stage::Design => LoopPhase::Design,
        Stage::Plan | Stage::Implement | Stage::Review => LoopPhase::Implement,
        Stage::Finalize => LoopPhase::Result,
        Stage::Reflect => LoopPhase::Reflect,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::Iterable;

    /// Declaration order must equal phase order (the connector folding depends on
    /// `Ord` to normalize lineage edges to `earlier → later`).
    #[test]
    fn phase_order_is_declaration_order() {
        assert!(LoopPhase::Issue < LoopPhase::Requirement);
        assert!(LoopPhase::Requirement < LoopPhase::Design);
        assert!(LoopPhase::Design < LoopPhase::Implement);
        assert!(LoopPhase::Implement < LoopPhase::Result);
        assert!(LoopPhase::Result < LoopPhase::Reflect);
    }

    /// Exhaustive: every `ArtifactKind` maps to the spec's phase. Iterating the
    /// enum means a newly-added kind makes this test fail (not just `artifact_phase`'s
    /// match), forcing both the mapping and its assertion to be updated together.
    #[test]
    fn artifact_phase_maps_every_kind() {
        for kind in ArtifactKind::iter() {
            let phase = artifact_phase(kind);
            let expected = match kind {
                ArtifactKind::Issue => LoopPhase::Issue,
                ArtifactKind::Requirement => LoopPhase::Requirement,
                ArtifactKind::Design => LoopPhase::Design,
                ArtifactKind::Task => LoopPhase::Implement,
                ArtifactKind::Review => LoopPhase::Implement,
                ArtifactKind::Result => LoopPhase::Result,
                ArtifactKind::Reflection => LoopPhase::Reflect,
            };
            assert_eq!(phase, expected, "artifact_phase({kind:?})");
        }
    }

    /// Exhaustive: every `Stage` maps to the spec's phase, including the
    /// artifact-less stages (`triage` → Issue, `finalize` → Result).
    #[test]
    fn iteration_phase_maps_every_stage() {
        for stage in Stage::iter() {
            let phase = iteration_phase(stage);
            let expected = match stage {
                Stage::Triage => LoopPhase::Issue,
                Stage::Refine => LoopPhase::Requirement,
                Stage::Design => LoopPhase::Design,
                Stage::Plan => LoopPhase::Implement,
                Stage::Implement => LoopPhase::Implement,
                Stage::Review => LoopPhase::Implement,
                Stage::Finalize => LoopPhase::Result,
                Stage::Reflect => LoopPhase::Reflect,
            };
            assert_eq!(phase, expected, "iteration_phase({stage:?})");
        }
    }
}
