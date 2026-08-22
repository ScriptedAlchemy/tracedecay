//! Crate-owned ledger label vocabulary for automation run outcomes.
//!
//! Skip, disabled, budget, and tombstone labels are terminal diagnostics with
//! different recovery stories: a config skip clears when the user re-enables
//! automation, a budget skip clears on the backoff window, and a removal
//! tombstone marks a managed skill permanently retired by the skill-overlap
//! consolidation path. Reusing one label for another state would make a
//! permanent removal look like a transient skip (or vice versa), so this
//! module owns the tombstone label and proves pairwise distinctness in tests.

pub use tracedecay_domain::{
    SESSION_EVIDENCE_BUDGET_EXHAUSTED, SESSION_EVIDENCE_BUDGET_SUPPRESSED,
};

/// Ledger label for a run skipped because automation is disabled in config.
///
/// Host crates currently spell this wire value as inline literals; owning the
/// canonical spelling here lets the distinctness tests below fence the
/// tombstone label against it without reaching into host code.
pub const AUTOMATION_DISABLED: &str = "automation_disabled";

/// Ledger label for a managed skill removed by the skill-overlap
/// consolidation path (merge or archive of an overlapping skill).
///
/// This is a removal tombstone, not a skip reason: the skill is gone and the
/// label must never be confused with `AUTOMATION_DISABLED` or the
/// session-evidence budget skips, all of which describe runs that may resume.
pub const SKILL_OVERLAP_REMOVAL_TOMBSTONE: &str = "skill_overlap_removal_tombstone";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tombstone_disabled_and_budget_labels_are_pairwise_distinct() {
        let labels = [
            (
                "skill-overlap removal tombstone",
                SKILL_OVERLAP_REMOVAL_TOMBSTONE,
            ),
            ("automation-disabled skip", AUTOMATION_DISABLED),
            ("budget-exhausted skip", SESSION_EVIDENCE_BUDGET_EXHAUSTED),
            ("budget-suppressed skip", SESSION_EVIDENCE_BUDGET_SUPPRESSED),
        ];
        for (index, (name_a, label_a)) in labels.iter().enumerate() {
            for (name_b, label_b) in &labels[index + 1..] {
                assert_ne!(
                    label_a, label_b,
                    "the {name_a} label must not reuse the {name_b} label: a \
                     removal tombstone or skill-overlap record presenting as \
                     `{label_b}` would be misread as a resumable skip"
                );
            }
        }
    }

    #[test]
    fn labels_pin_their_exact_wire_values() {
        assert_eq!(AUTOMATION_DISABLED, "automation_disabled");
        assert_eq!(
            SESSION_EVIDENCE_BUDGET_EXHAUSTED,
            "session_evidence_budget_exhausted"
        );
        assert_eq!(
            SESSION_EVIDENCE_BUDGET_SUPPRESSED,
            "session_evidence_budget_suppressed"
        );
        assert_eq!(
            SKILL_OVERLAP_REMOVAL_TOMBSTONE,
            "skill_overlap_removal_tombstone"
        );
    }
}
