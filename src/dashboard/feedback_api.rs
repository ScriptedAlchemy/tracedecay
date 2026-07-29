//! PR14 dashboard projection over the canonical feedback and Plan-26 owners.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use axum::extract::State;
use axum::response::Json;
use tracedecay_application::ApplicationContractError;

use crate::application::feedback::observations::{
    FeedbackObservationReadModelV1, Plan26CoverageV1,
};
use crate::daemon::DaemonFeedbackRuntimeRegistrar;

use super::DashboardState;
use super::read_model::{
    DashboardCoverageV1, DashboardDomainStateV1, DashboardEnvelopeV1, DashboardFreshnessStateV1,
    DashboardFreshnessV1, DashboardLegalActionKindV1, DashboardLegalActionRefV1, DashboardTimeV1,
    DashboardWatermarkV1, scope_from_state,
};

pub(crate) type FeedbackStatusReadFuture = Pin<
    Box<
        dyn Future<Output = Result<FeedbackObservationReadModelV1, ApplicationContractError>>
            + Send
            + 'static,
    >,
>;
pub(crate) type FeedbackStatusReader =
    Arc<dyn Fn(PathBuf) -> FeedbackStatusReadFuture + Send + Sync + 'static>;

/// Erases the daemon registry behind a read-only, root-addressed application
/// authority. Dashboard state receives no feedback database or provider owner.
pub(crate) fn feedback_status_reader(
    runtimes: DaemonFeedbackRuntimeRegistrar,
) -> FeedbackStatusReader {
    Arc::new(move |project_root| {
        let runtimes = runtimes.clone();
        Box::pin(async move {
            let store = runtimes.doctor_read_store(&project_root).await.ok_or(
                ApplicationContractError::Inconsistent {
                    field: "dashboard feedback status authority",
                },
            )?;
            store.observation_read_model().await.map_err(|_| {
                ApplicationContractError::Inconsistent {
                    field: "dashboard feedback status projection",
                }
            })
        })
    })
}

pub(crate) async fn status(
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
    let scope = scope_from_state(state);
    let Ok(payload) = projected else {
        let Some(empty_payload) = FeedbackObservationReadModelV1::project(&[]) else {
            unreachable!("empty feedback observation projection is canonical");
        };
        return DashboardEnvelopeV1::new(
            scope,
            DashboardDomainStateV1::Unknown,
            DashboardCoverageV1::unknown(),
            DashboardFreshnessV1::unknown(),
            empty_payload,
        )
        .with_legal_actions(vec![DashboardLegalActionRefV1::new(
            DashboardLegalActionKindV1::Refresh,
            "feedback_status",
        )]);
    };

    let denominators = &payload.denominators;
    let coverage = match payload.coverage {
        Plan26CoverageV1::Known => {
            DashboardCoverageV1::complete(denominators.eligible, "feedback_observations")
        }
        Plan26CoverageV1::Partial | Plan26CoverageV1::Sampled | Plan26CoverageV1::Capped => {
            DashboardCoverageV1::partial(
                denominators.eligible,
                denominators.persisted,
                "feedback_observations",
                feedback_omission_reasons(&payload),
            )
        }
        Plan26CoverageV1::Stale | Plan26CoverageV1::Unknown => DashboardCoverageV1::unknown(),
    };
    let domain_state = match payload.coverage {
        Plan26CoverageV1::Known if payload.total_count == 0 => {
            DashboardDomainStateV1::CompleteZeroFindings
        }
        Plan26CoverageV1::Known => DashboardDomainStateV1::Ready,
        Plan26CoverageV1::Stale => DashboardDomainStateV1::Stale,
        Plan26CoverageV1::Partial | Plan26CoverageV1::Sampled | Plan26CoverageV1::Capped => {
            DashboardDomainStateV1::Partial
        }
        Plan26CoverageV1::Unknown => DashboardDomainStateV1::Unknown,
    };
    let freshness = match payload.coverage {
        Plan26CoverageV1::Stale => DashboardFreshnessV1 {
            state: DashboardFreshnessStateV1::Stale,
            observed_at_micros: payload.last_observed_at.map(|value| value.0),
            watermark: payload
                .watermark
                .producer_sequence
                .map(|sequence| sequence.to_string()),
        },
        Plan26CoverageV1::Unknown => DashboardFreshnessV1::unknown(),
        Plan26CoverageV1::Known
        | Plan26CoverageV1::Partial
        | Plan26CoverageV1::Sampled
        | Plan26CoverageV1::Capped => DashboardFreshnessV1 {
            state: DashboardFreshnessStateV1::Fresh,
            observed_at_micros: payload.last_observed_at.map(|value| value.0),
            watermark: payload
                .watermark
                .producer_sequence
                .map(|sequence| sequence.to_string()),
        },
    };
    let source_watermark =
        payload
            .watermark
            .producer_sequence
            .map(|sequence| DashboardWatermarkV1 {
                source: "feedback_observations".to_owned(),
                watermark: sequence.to_string(),
            });
    let time = DashboardTimeV1 {
        valid_time_micros: payload.last_observed_at.map(|value| value.0),
        observation_time_micros: payload
            .watermark
            .observed_through
            .or(payload.last_observed_at)
            .map_or_else(super::read_model::now_micros, |value| value.0),
    };
    let mut envelope = DashboardEnvelopeV1::new(scope, domain_state, coverage, freshness, payload)
        .with_legal_actions(vec![DashboardLegalActionRefV1::new(
            DashboardLegalActionKindV1::Refresh,
            "feedback_status",
        )]);
    envelope.source_watermark = source_watermark;
    envelope.time = time;
    envelope
}

fn feedback_omission_reasons(model: &FeedbackObservationReadModelV1) -> Vec<String> {
    let mut reasons = Vec::new();
    if model.denominators.delayed > 0 {
        reasons.push("delayed_observations".to_owned());
    }
    if model.denominators.dropped > 0 {
        reasons.push("dropped_observations".to_owned());
    }
    if model.denominators.retention_dropped > 0 {
        reasons.push("retention_capped".to_owned());
    }
    if model.denominators.incomplete_boots > 0 {
        reasons.push("incomplete_producer_boot".to_owned());
    }
    reasons
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
