//! Daemon-ready bridge for the feedback completed-publication ledger.
//!
//! The ledger remains an injected daemon/store authority. This module adds no
//! table, lock, cache, transport identity, or retry queue of its own.

use tracedecay_application::RequestContext;
use tracedecay_application::context::RequestAdmission;
use tracedecay_application::feedback::{
    FeedbackCompletedPublicationV1, FeedbackCycleDedupePort, FeedbackCycleDedupePublicationState,
    FeedbackCycleDedupeState, FeedbackPortFuture,
};
use tracedecay_domain::feedback::FeedbackDedupeKeyV1;

/// Restart-safe daemon ledger boundary. `record_completed` is an atomic
/// compare-and-insert keyed by `publication.dedupe_key`; it rechecks the
/// publication's runtime and authorization guards before making either the
/// completion or key visible. Any non-recorded outcome leaves no reservation
/// behind.
pub trait CompletedFeedbackPublicationLedger {
    fn lookup_completed<'a>(
        &'a self,
        context: &'a RequestContext,
        key: &'a FeedbackDedupeKeyV1,
    ) -> FeedbackPortFuture<'a, FeedbackCycleDedupeState>;

    fn record_completed<'a>(
        &'a self,
        context: &'a RequestContext,
        publication: &'a FeedbackCompletedPublicationV1,
    ) -> FeedbackPortFuture<'a, FeedbackCycleDedupePublicationState>;
}

/// Adapts one daemon-owned serialized ledger to the application feedback port.
/// Persistence, locking, transaction selection, and cancellation mechanics
/// stay behind the injected ledger contract.
pub struct SerializedFeedbackCycleDedupeAdapter<L> {
    ledger: L,
}

impl<L> SerializedFeedbackCycleDedupeAdapter<L> {
    pub fn new(ledger: L) -> Self {
        Self { ledger }
    }
}

impl<L> FeedbackCycleDedupePort for SerializedFeedbackCycleDedupeAdapter<L>
where
    L: CompletedFeedbackPublicationLedger + Sync,
{
    fn lookup_completed<'a>(
        &'a self,
        context: &'a RequestContext,
        key: &'a FeedbackDedupeKeyV1,
    ) -> FeedbackPortFuture<'a, FeedbackCycleDedupeState> {
        if key.validate().is_err() {
            return Box::pin(async { FeedbackCycleDedupeState::Unavailable });
        }
        self.ledger.lookup_completed(context, key)
    }

    fn record_completed<'a>(
        &'a self,
        context: &'a RequestContext,
        publication: &'a FeedbackCompletedPublicationV1,
    ) -> FeedbackPortFuture<'a, FeedbackCycleDedupePublicationState> {
        if publication.validate().is_err() {
            return Box::pin(async { FeedbackCycleDedupePublicationState::Unavailable });
        }
        match context.admission_at(publication.authority.revalidated_at) {
            RequestAdmission::Admitted => {}
            RequestAdmission::Cancelled => {
                return Box::pin(async { FeedbackCycleDedupePublicationState::Cancelled });
            }
            RequestAdmission::TimedOut => {
                return Box::pin(async { FeedbackCycleDedupePublicationState::TimedOut });
            }
        }
        self.ledger.record_completed(context, publication)
    }
}
