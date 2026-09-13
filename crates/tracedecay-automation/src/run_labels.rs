//! Crate-owned ledger label vocabulary for automation run outcomes.
//!
//! Skip, disabled, budget, and tombstone labels are terminal diagnostics with
//! different recovery stories: a config skip clears when the user re-enables
//! automation, a budget skip clears on the backoff window, and a removal
//! tombstone marks a managed skill permanently retired by the skill-overlap
//! consolidation path. Reusing one label for another state would make a
//! permanent removal look like a transient skip (or vice versa), so this
//! module owns the labels and proves pairwise distinctness in tests.

pub use crate::evidence_budget::{
    SESSION_EVIDENCE_BUDGET_EXHAUSTED, SESSION_EVIDENCE_BUDGET_SUPPRESSED,
};

/// Ledger label for a run skipped because automation is disabled in config.
pub const AUTOMATION_DISABLED: &str = "automation_disabled";

/// Label carried by applied skill-consolidation records for a managed skill
/// removed by the skill-overlap consolidation path (merge or archive of an
/// overlapping skill).
///
/// This is a removal tombstone, not a skip reason: the skill is gone and the
/// label must never be confused with `AUTOMATION_DISABLED` or the
/// session-evidence budget skips, all of which describe runs that may resume.
pub const SKILL_OVERLAP_REMOVAL_TOMBSTONE: &str = "skill_overlap_removal_tombstone";
