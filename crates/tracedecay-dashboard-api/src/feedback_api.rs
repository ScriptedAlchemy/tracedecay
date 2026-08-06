//! dashboard projection over the canonical feedback and Plan-26 owners.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use axum::extract::State;
use axum::response::Json;
use tracedecay_api::feedback::{
    FeedbackStatusCoverageV1, FeedbackStatusDenominatorsV1, FeedbackStatusPresentationV1,
    feedback_status_envelope,
};
use tracedecay_application::ApplicationContractError;

use crate::application::feedback::observations::{
    FeedbackObservationReadModelV1, Plan26CoverageV1,
};

use super::DashboardState;
use super::read_model::{DashboardEnvelopeV1, scope_from_state};

pub type FeedbackStatusReadFuture = Pin<
    Box<
        dyn Future<Output = Result<FeedbackObservationReadModelV1, ApplicationContractError>>
            + Send
            + 'static,
    >,
>;
pub type FeedbackStatusReader =
    Arc<dyn Fn(PathBuf) -> FeedbackStatusReadFuture + Send + Sync + 'static>;

pub trait FeedbackStatusRuntime: Send + Sync {
    fn read_feedback_status(&self, project_root: PathBuf) -> FeedbackStatusReadFuture;
}

/// Erases the daemon registry behind a read-only, root-addressed application
/// authority. Dashboard state receives no feedback database or provider owner.
pub fn feedback_status_reader<R>(runtimes: R) -> FeedbackStatusReader
where
    R: FeedbackStatusRuntime + 'static,
{
    let runtimes = Arc::new(runtimes);
    Arc::new(move |project_root| {
        let runtimes = Arc::clone(&runtimes);
        runtimes.read_feedback_status(project_root)
    })
}

pub async fn status(
    State(state): State<DashboardState>,
) -> Json<DashboardEnvelopeV1<FeedbackObservationReadModelV1>> {
    let projected = match state.feedback_status_reader.as_ref() {
        Some(reader) => reader(state.project_root.clone()).await,
        None => Err(ApplicationContractError::Inconsistent {
            field: "dashboard feedback status authority",
        }),
    };
    Json(status_envelope(&state, projected))
}

fn status_envelope(
    state: &DashboardState,
    projected: Result<FeedbackObservationReadModelV1, ApplicationContractError>,
) -> DashboardEnvelopeV1<FeedbackObservationReadModelV1> {
    let presentation = projected.map(|payload| FeedbackStatusPresentationV1 {
        coverage: match payload.coverage {
            Plan26CoverageV1::Known => FeedbackStatusCoverageV1::Known,
            Plan26CoverageV1::Partial => FeedbackStatusCoverageV1::Partial,
            Plan26CoverageV1::Sampled => FeedbackStatusCoverageV1::Sampled,
            Plan26CoverageV1::Capped => FeedbackStatusCoverageV1::Capped,
            Plan26CoverageV1::Stale => FeedbackStatusCoverageV1::Stale,
            Plan26CoverageV1::Unknown => FeedbackStatusCoverageV1::Unknown,
        },
        total_count: payload.total_count,
        denominators: FeedbackStatusDenominatorsV1 {
            eligible: payload.denominators.eligible,
            persisted: payload.denominators.persisted,
            delayed: payload.denominators.delayed,
            dropped: payload.denominators.dropped,
            retention_dropped: payload.denominators.retention_dropped,
            incomplete_boots: payload.denominators.incomplete_boots,
        },
        last_observed_at_micros: payload.last_observed_at.map(|value| value.0),
        observed_through_micros: payload.watermark.observed_through.map(|value| value.0),
        producer_sequence: payload.watermark.producer_sequence,
        payload,
    });
    feedback_status_envelope(
        scope_from_state(state),
        presentation,
        FeedbackObservationReadModelV1::empty,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::feedback::observations::{
        FeedbackObservationDeliveryV1, Plan26FeedbackOperationV1, Plan26FeedbackOutcomeV1,
        Plan26FeedbackSourceEventV1, plan26_feedback_source_event_envelope_for_subject,
    };
    use tracedecay_domain::{ManifestDigest, UtcMicros, canonical_sha256};

    #[test]
    fn partial_status_preserves_drop_denominator_and_unknowns() {
        let subject = canonical_sha256(&"dashboard-feedback-status").unwrap();
        let mut event = plan26_feedback_source_event_envelope_for_subject(
            subject,
            UtcMicros(100),
            Plan26FeedbackSourceEventV1::AuthorizationRevoked {
                operation: Plan26FeedbackOperationV1::FeedbackGet,
                outcome: Plan26FeedbackOutcomeV1::Completed,
                propagation_micros: 50,
            },
        )
        .unwrap();
        event
            .assign_delivery(
                ManifestDigest::new(
                    "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                )
                .unwrap(),
                1,
                FeedbackObservationDeliveryV1::delivered(2),
            )
            .unwrap();
        let model =
            FeedbackObservationReadModelV1::project_with_accounting(&[event], 0, 0).unwrap();
        assert_eq!(model.coverage, Plan26CoverageV1::Partial);
        assert_eq!(model.denominators.eligible, 3);
        assert_eq!(
            model.system_quality.metrics.len(),
            9,
            "every Plan 37 quality dimension remains present"
        );
    }

    #[test]
    fn empty_projection_never_fabricates_available_metrics() {
        let model = FeedbackObservationReadModelV1::project(&[]).unwrap();
        assert_eq!(model.coverage, Plan26CoverageV1::Unknown);
        assert!(
            model
                .system_quality
                .metrics
                .iter()
                .all(|metric| metric.value.is_none())
        );
    }
}
