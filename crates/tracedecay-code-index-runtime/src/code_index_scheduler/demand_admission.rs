//! The one verdict every code-index demand carries.
//!
//! Hooks, `tracedecay sync`, branch publication, and the host-admission
//! boundary all ask the same question, "did anything take this demand, and if
//! not, why?", so they all read the same answer. Earlier revisions answered
//! with a bool per layer and rebuilt the reason above it, which is how a
//! terminal park became "retryable scheduler unavailable" and a watcher-policy
//! refusal became "accepted".

use tracedecay_contracts::code_index_freshness::CodeIndexConvergenceParkedV1;

/// Who is asking, which is the only thing that changes whether a refusal is a
/// policy decision or an obstacle.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CodeIndexDemandV1 {
    /// Exact repository-relative paths a host after-edit hook touched.
    HookPaths(Vec<String>),
    /// The daemon's own whole-worktree demand: a hook effect, the server's
    /// startup catch-up, a coalesced overflow. Subject to
    /// `sync.watch_linked_worktrees`.
    Reconcile,
    /// A whole-worktree reconciliation an operator named (`tracedecay init`,
    /// `tracedecay sync`). Not subject to the watcher policy.
    OperatorReconcile,
}

impl CodeIndexDemandV1 {
    /// Whether `sync.watch_linked_worktrees` governs this demand.
    pub(super) const fn is_watcher_policy_governed(&self) -> bool {
        matches!(self, Self::HookPaths(_) | Self::Reconcile)
    }
}

/// Why nothing took a demand, when the reason is neither policy nor a park.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CodeIndexDemandUnavailableV1 {
    /// The route was retired or its activation cancelled.
    RouteRetired,
    /// The named root is not this activation's exact worktree.
    ForeignRoot,
    /// No mounted scheduler owns this route and nothing queued the demand.
    SchedulerUnmounted,
    /// Repository membership could not be decided. Retry; do not treat this as
    /// a non-repository.
    IdentityUnresolved,
    /// The bounded freshness ladder found no source change to admit.
    NoProvenChange,
}

impl CodeIndexDemandUnavailableV1 {
    pub const fn label(self) -> &'static str {
        match self {
            Self::RouteRetired => "route_retired",
            Self::ForeignRoot => "foreign_root",
            Self::SchedulerUnmounted => "scheduler_unmounted",
            Self::IdentityUnresolved => "identity_unresolved",
            Self::NoProvenChange => "no_proven_change",
        }
    }
}

/// The verdict for one code-index demand. Minted by
/// [`super::CodeIndexActivationV1::admit`] and by the mounted registry it
/// forwards to; never reconstructed from a bool by a caller above.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CodeIndexDemandAdmissionV1 {
    /// A mounted scheduler took the demand, or the bounded pre-mount queue
    /// holds it until the mount that will deliver it completes.
    Queued,
    /// The watcher policy for this route refuses the daemon's own demand. An
    /// operator-named demand for the same route is still admitted.
    RefusedByPolicy,
    /// The route is valid but has no repository identity, so code indexing
    /// does not apply and no work was queued.
    NotApplicable,
    /// The worktree is parked on a corrupt publication authority. Terminal:
    /// only an explicit index reset admits work again.
    Terminal(CodeIndexConvergenceParkedV1),
    /// Nothing took the demand, and a later one may still succeed.
    Unavailable(CodeIndexDemandUnavailableV1),
}

impl CodeIndexDemandAdmissionV1 {
    pub const fn is_queued(&self) -> bool {
        matches!(self, Self::Queued)
    }

    /// Reduce the verdicts of one batch to the strongest refusal it carries: a
    /// terminal park outranks a policy refusal, which outranks unavailability,
    /// which outranks acceptance.
    #[must_use = "the reduced admission verdict is the only answer the batch produced"]
    pub fn strongest_refusal(self, other: Self) -> Self {
        match (self, other) {
            (Self::Terminal(parked), _) | (_, Self::Terminal(parked)) => Self::Terminal(parked),
            (Self::RefusedByPolicy, _) | (_, Self::RefusedByPolicy) => Self::RefusedByPolicy,
            (Self::Unavailable(cause), _) | (_, Self::Unavailable(cause)) => {
                Self::Unavailable(cause)
            }
            (Self::Queued, Self::Queued | Self::NotApplicable)
            | (Self::NotApplicable, Self::Queued) => Self::Queued,
            (Self::NotApplicable, Self::NotApplicable) => Self::NotApplicable,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parked() -> CodeIndexConvergenceParkedV1 {
        CodeIndexConvergenceParkedV1 {
            reason: "corrupt".to_owned(),
            blocked_reason: None,
            remediation: "reset".to_owned(),
            parked_at_micros: 1,
            observed_passes: 1,
            retries_on_wake: false,
        }
    }

    #[test]
    fn the_strongest_refusal_of_a_batch_is_terminal_then_policy_then_unavailable() {
        let terminal = CodeIndexDemandAdmissionV1::Terminal(parked());
        let unavailable = CodeIndexDemandAdmissionV1::Unavailable(
            CodeIndexDemandUnavailableV1::SchedulerUnmounted,
        );
        assert_eq!(
            CodeIndexDemandAdmissionV1::Queued.strongest_refusal(terminal.clone()),
            terminal
        );
        assert_eq!(
            terminal
                .clone()
                .strongest_refusal(CodeIndexDemandAdmissionV1::RefusedByPolicy),
            terminal
        );
        assert_eq!(
            CodeIndexDemandAdmissionV1::RefusedByPolicy.strongest_refusal(unavailable.clone()),
            CodeIndexDemandAdmissionV1::RefusedByPolicy
        );
        assert_eq!(
            unavailable
                .clone()
                .strongest_refusal(CodeIndexDemandAdmissionV1::Queued),
            unavailable
        );
        assert_eq!(
            CodeIndexDemandAdmissionV1::Queued
                .strongest_refusal(CodeIndexDemandAdmissionV1::Queued),
            CodeIndexDemandAdmissionV1::Queued
        );
        assert_eq!(
            CodeIndexDemandAdmissionV1::NotApplicable
                .strongest_refusal(CodeIndexDemandAdmissionV1::Queued),
            CodeIndexDemandAdmissionV1::Queued
        );
        assert_eq!(
            CodeIndexDemandAdmissionV1::NotApplicable
                .strongest_refusal(CodeIndexDemandAdmissionV1::NotApplicable),
            CodeIndexDemandAdmissionV1::NotApplicable
        );
    }
}
