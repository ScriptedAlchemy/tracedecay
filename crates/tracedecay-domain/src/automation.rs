//! Canonical domain vocabulary shared by automation producers and projections.

/// Ledger label for a session-evidence retrieval attempt that exhausted its
/// admitted budgets.
pub const SESSION_EVIDENCE_BUDGET_EXHAUSTED: &str = "session_evidence_budget_exhausted";

/// Ledger label for a scheduler tick suppressed by the evidence-budget
/// backoff without attempting another retrieval.
pub const SESSION_EVIDENCE_BUDGET_SUPPRESSED: &str = "session_evidence_budget_suppressed";
