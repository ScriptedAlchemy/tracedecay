//! `GET /api/remote/status` — Remote Brain operational plane for Settings.
//!
//! An admitted daemon owner injects the canonical operational read into
//! [`DashboardState`]. This module maps that one application shape onto a
//! schemars-typed dashboard DTO. A dashboard opened without an admitted
//! reader remains explicitly unavailable — never an empty success.

use axum::Json;
use axum::extract::State;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracedecay_application::doctor::{DoctorCoverageCompletenessV1, RemoteListenerReadV1};
use tracedecay_application::remote::status::{
    RemoteOperationalReadinessV1, RemoteOperationalStatusReadV1, RemoteOperationalStatusV1,
    RemoteSpoolOperationalStatusV1,
};
use tracedecay_domain::{
    CurrentRemoteAuthorityStateV1, RemoteAuthorityUnavailableReasonV1, RemoteWriterFenceV1,
    UtcMicros,
};

use super::DashboardState;
use super::read_model::{
    DashboardCoverageV1, DashboardDomainStateV1, DashboardEnvelopeV1, DashboardFreshnessV1,
    DashboardLegalActionKindV1, DashboardLegalActionRefV1, DashboardScopeV1, scope_from_state,
};

const REMOTE_STATUS_REFRESH_OPERATION: &str = "use-case.dashboard.remote.status.refresh";
const READER_UNSUPPORTED_NOTE: &str =
    "the dashboard is not attached to a daemon-owned remote operational status reader";

/// `GET /api/remote/status`
pub async fn status(
    State(state): State<DashboardState>,
) -> Json<DashboardEnvelopeV1<RemoteOperationalStatusPayloadV1>> {
    Json(status_from_reader(
        scope_from_state(&state),
        state.remote_operational_status_reader.clone(),
    ))
}

pub(crate) fn status_from_reader(
    scope: DashboardScopeV1,
    reader: Option<crate::RemoteOperationalStatusReader>,
) -> DashboardEnvelopeV1<RemoteOperationalStatusPayloadV1> {
    match reader {
        None => envelope(
            scope,
            DashboardDomainStateV1::Unsupported,
            DashboardCoverageV1::unsupported(),
            DashboardFreshnessV1::unsupported(),
            RemoteOperationalStatusPayloadV1::Unavailable {
                note: READER_UNSUPPORTED_NOTE.to_owned(),
            },
        ),
        Some(reader) => envelope_for_read(scope, map_read(reader())),
    }
}

fn envelope_for_read(
    scope: DashboardScopeV1,
    payload: RemoteOperationalStatusPayloadV1,
) -> DashboardEnvelopeV1<RemoteOperationalStatusPayloadV1> {
    match &payload {
        RemoteOperationalStatusPayloadV1::Unavailable { note } => {
            DashboardEnvelopeV1::unavailable(scope, payload.clone(), note.clone())
                .with_legal_actions(vec![refresh_action()])
        }
        RemoteOperationalStatusPayloadV1::Unconfigured => envelope(
            scope,
            DashboardDomainStateV1::Unknown,
            DashboardCoverageV1::unknown(),
            DashboardFreshnessV1::unknown(),
            payload,
        ),
        RemoteOperationalStatusPayloadV1::Observed {
            readiness,
            coverage,
            observed_at,
            ..
        } => {
            let freshness = DashboardFreshnessV1 {
                state: super::read_model::DashboardFreshnessStateV1::Fresh,
                observed_at_micros: Some(observed_at.0),
                watermark: None,
            };
            let (domain_state, dashboard_coverage) = match readiness {
                RemoteReadinessKindV1::Ready => (
                    DashboardDomainStateV1::Ready,
                    match coverage {
                        DoctorCoverageCompletenessV1::Complete => {
                            DashboardCoverageV1::complete(1, "remote_operational_status")
                        }
                        DoctorCoverageCompletenessV1::Partial => DashboardCoverageV1::partial(
                            1,
                            0,
                            "remote_operational_status",
                            vec!["remote operational coverage is incomplete".to_owned()],
                        ),
                        DoctorCoverageCompletenessV1::Unknown => DashboardCoverageV1::unknown(),
                    },
                ),
                RemoteReadinessKindV1::Partial => (
                    DashboardDomainStateV1::Partial,
                    DashboardCoverageV1::partial(
                        1,
                        0,
                        "remote_operational_status",
                        vec!["remote operational readiness is partial".to_owned()],
                    ),
                ),
                RemoteReadinessKindV1::RecoveryRequired => (
                    DashboardDomainStateV1::Error,
                    DashboardCoverageV1::partial(
                        1,
                        0,
                        "remote_operational_status",
                        vec!["remote operational recovery is required".to_owned()],
                    ),
                ),
                RemoteReadinessKindV1::Unconfigured => (
                    DashboardDomainStateV1::Unknown,
                    DashboardCoverageV1::unknown(),
                ),
            };
            envelope(scope, domain_state, dashboard_coverage, freshness, payload)
        }
    }
}

fn envelope(
    scope: DashboardScopeV1,
    domain_state: DashboardDomainStateV1,
    coverage: DashboardCoverageV1,
    freshness: DashboardFreshnessV1,
    payload: RemoteOperationalStatusPayloadV1,
) -> DashboardEnvelopeV1<RemoteOperationalStatusPayloadV1> {
    DashboardEnvelopeV1::new(scope, domain_state, coverage, freshness, payload)
        .with_legal_actions(vec![refresh_action()])
}

fn refresh_action() -> DashboardLegalActionRefV1 {
    DashboardLegalActionRefV1::new(
        DashboardLegalActionKindV1::Refresh,
        REMOTE_STATUS_REFRESH_OPERATION,
    )
}

fn map_read(read: RemoteOperationalStatusReadV1) -> RemoteOperationalStatusPayloadV1 {
    match read {
        RemoteOperationalStatusReadV1::Observed {
            listener,
            status,
            coverage,
        } => RemoteOperationalStatusPayloadV1::from_observed(listener, status, coverage),
        RemoteOperationalStatusReadV1::Unconfigured => {
            RemoteOperationalStatusPayloadV1::Unconfigured
        }
        RemoteOperationalStatusReadV1::Unavailable => {
            RemoteOperationalStatusPayloadV1::Unavailable {
                note: "the remote operational authority is unreadable".to_owned(),
            }
        }
    }
}

/// Dashboard wire DTO for the Remote Brain operational plane.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum RemoteOperationalStatusPayloadV1 {
    Observed {
        listener: RemoteListenerKindV1,
        coverage: DoctorCoverageCompletenessV1,
        readiness: RemoteReadinessKindV1,
        enrollment_configured: bool,
        authority: RemoteAuthoritySummaryV1,
        spool: RemoteSpoolSummaryV1,
        replay_coverage_complete: bool,
        current_backup_verified: bool,
        failover_in_progress: bool,
        recovery_required: bool,
        observed_at: UtcMicros,
    },
    Unconfigured,
    Unavailable {
        note: String,
    },
}

impl RemoteOperationalStatusPayloadV1 {
    fn from_observed(
        listener: RemoteListenerReadV1,
        status: RemoteOperationalStatusV1,
        coverage: DoctorCoverageCompletenessV1,
    ) -> Self {
        Self::Observed {
            listener: RemoteListenerKindV1::from_read(listener),
            coverage,
            readiness: RemoteReadinessKindV1::from_read(status.readiness),
            enrollment_configured: status.enrollment_configured,
            authority: RemoteAuthoritySummaryV1::from_state(status.authority),
            spool: RemoteSpoolSummaryV1::from_status(status.spool),
            replay_coverage_complete: status.replay_coverage_complete,
            current_backup_verified: status.current_backup_verified,
            failover_in_progress: status.failover_in_progress,
            recovery_required: status.recovery_required,
            observed_at: status.observed_at,
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RemoteListenerKindV1 {
    Serving,
    Disabled,
    Degraded,
}

impl RemoteListenerKindV1 {
    fn from_read(listener: RemoteListenerReadV1) -> Self {
        match listener {
            RemoteListenerReadV1::Serving => Self::Serving,
            RemoteListenerReadV1::Disabled => Self::Disabled,
            RemoteListenerReadV1::Degraded => Self::Degraded,
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RemoteReadinessKindV1 {
    Unconfigured,
    Partial,
    Ready,
    RecoveryRequired,
}

impl RemoteReadinessKindV1 {
    fn from_read(readiness: RemoteOperationalReadinessV1) -> Self {
        match readiness {
            RemoteOperationalReadinessV1::Unconfigured => Self::Unconfigured,
            RemoteOperationalReadinessV1::Partial => Self::Partial,
            RemoteOperationalReadinessV1::Ready => Self::Ready,
            RemoteOperationalReadinessV1::RecoveryRequired => Self::RecoveryRequired,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct RemoteFenceSummaryV1 {
    pub brain_id: String,
    pub shard_id: String,
    pub generation_id: String,
    pub placement_revision: u64,
    pub authority_epoch: u64,
    pub authority_node_id: String,
}

impl RemoteFenceSummaryV1 {
    fn from_fence(fence: &RemoteWriterFenceV1) -> Self {
        Self {
            brain_id: fence.brain_id.as_str().to_owned(),
            shard_id: fence.shard_id.as_str().to_owned(),
            generation_id: fence.generation_id.as_str().to_owned(),
            placement_revision: fence.placement_revision.get(),
            authority_epoch: fence.authority_epoch.0,
            authority_node_id: fence.authority_node_id.as_str().to_owned(),
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RemoteAuthorityMissingReasonV1 {
    RegistryUnavailable,
    PlacementUnknown,
    AuthorityUnreachable,
    AuthorityAuthenticationFailed,
    CallerAuthenticationFailed,
    EnrollmentExpired,
    EnrollmentRevoked,
    InsufficientCapability,
    ScopeMismatch,
    FenceUnverified,
    ProtocolIncompatible,
}

impl RemoteAuthorityMissingReasonV1 {
    fn from_reason(reason: RemoteAuthorityUnavailableReasonV1) -> Self {
        match reason {
            RemoteAuthorityUnavailableReasonV1::RegistryUnavailable => Self::RegistryUnavailable,
            RemoteAuthorityUnavailableReasonV1::PlacementUnknown => Self::PlacementUnknown,
            RemoteAuthorityUnavailableReasonV1::AuthorityUnreachable => Self::AuthorityUnreachable,
            RemoteAuthorityUnavailableReasonV1::AuthorityAuthenticationFailed => {
                Self::AuthorityAuthenticationFailed
            }
            RemoteAuthorityUnavailableReasonV1::CallerAuthenticationFailed => {
                Self::CallerAuthenticationFailed
            }
            RemoteAuthorityUnavailableReasonV1::EnrollmentExpired => Self::EnrollmentExpired,
            RemoteAuthorityUnavailableReasonV1::EnrollmentRevoked => Self::EnrollmentRevoked,
            RemoteAuthorityUnavailableReasonV1::InsufficientCapability => {
                Self::InsufficientCapability
            }
            RemoteAuthorityUnavailableReasonV1::ScopeMismatch => Self::ScopeMismatch,
            RemoteAuthorityUnavailableReasonV1::FenceUnverified => Self::FenceUnverified,
            RemoteAuthorityUnavailableReasonV1::ProtocolIncompatible => Self::ProtocolIncompatible,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum RemoteAuthoritySummaryV1 {
    Available {
        fence: RemoteFenceSummaryV1,
    },
    Partial {
        fence: Option<RemoteFenceSummaryV1>,
        missing: Vec<RemoteAuthorityMissingReasonV1>,
    },
    Unavailable {
        reason: RemoteAuthorityMissingReasonV1,
    },
}

impl RemoteAuthoritySummaryV1 {
    fn from_state(state: CurrentRemoteAuthorityStateV1) -> Self {
        match state {
            CurrentRemoteAuthorityStateV1::Available(authority) => Self::Available {
                fence: RemoteFenceSummaryV1::from_fence(&authority.fence),
            },
            CurrentRemoteAuthorityStateV1::Partial {
                known_fence,
                missing,
                ..
            } => Self::Partial {
                fence: known_fence.as_ref().map(RemoteFenceSummaryV1::from_fence),
                missing: missing
                    .into_iter()
                    .map(RemoteAuthorityMissingReasonV1::from_reason)
                    .collect(),
            },
            CurrentRemoteAuthorityStateV1::Unavailable { reason, .. } => Self::Unavailable {
                reason: RemoteAuthorityMissingReasonV1::from_reason(reason),
            },
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct RemoteSpoolSummaryV1 {
    pub pending_count: u64,
    pub quarantined_count: u64,
    pub has_sequence_gap: bool,
}

impl RemoteSpoolSummaryV1 {
    fn from_status(spool: RemoteSpoolOperationalStatusV1) -> Self {
        Self {
            pending_count: spool.pending_count,
            quarantined_count: spool.quarantined_count,
            has_sequence_gap: spool.has_sequence_gap,
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::sync::Arc;

    use super::*;

    fn scope() -> DashboardScopeV1 {
        DashboardScopeV1 {
            project_id: Some("project.dashboard-remote-status".to_owned()),
            storage_mode: "project_local".to_owned(),
            store_root: "fixture".to_owned(),
        }
    }

    fn available_authority() -> CurrentRemoteAuthorityStateV1 {
        serde_json::from_value(serde_json::json!({
            "state": "available",
            "value": {
                "fence": {
                    "brain_id": "brain.status",
                    "shard_id": "shard.status",
                    "generation_id": "generation.status",
                    "placement_revision": 1,
                    "authority_epoch": 1,
                    "authority_node_id": "node.authority"
                },
                "credential_revision": 1,
                "observed_at": 10
            }
        }))
        .unwrap()
    }

    fn ready_status() -> RemoteOperationalStatusV1 {
        RemoteOperationalStatusV1::compose(
            true,
            available_authority(),
            RemoteSpoolOperationalStatusV1 {
                pending_count: 0,
                quarantined_count: 0,
                has_sequence_gap: false,
            },
            true,
            true,
            false,
            false,
            UtcMicros(10),
        )
        .unwrap()
    }

    #[test]
    fn absent_reader_returns_typed_unavailable_not_empty_success() {
        let envelope = status_from_reader(scope(), None);

        assert_eq!(envelope.schema_revision, 1);
        assert_eq!(envelope.domain_state, DashboardDomainStateV1::Unsupported);
        assert_eq!(
            envelope.payload,
            RemoteOperationalStatusPayloadV1::Unavailable {
                note: READER_UNSUPPORTED_NOTE.to_owned(),
            }
        );
        assert_eq!(envelope.legal_actions, vec![refresh_action()]);
    }

    #[test]
    fn present_reader_maps_the_canonical_observed_status() {
        let status = ready_status();
        let reader: crate::RemoteOperationalStatusReader =
            Arc::new(move || RemoteOperationalStatusReadV1::Observed {
                listener: RemoteListenerReadV1::Serving,
                status: status.clone(),
                coverage: DoctorCoverageCompletenessV1::Complete,
            });

        let envelope = status_from_reader(scope(), Some(reader));

        assert_eq!(envelope.domain_state, DashboardDomainStateV1::Ready);
        assert_eq!(
            envelope.payload,
            RemoteOperationalStatusPayloadV1::Observed {
                listener: RemoteListenerKindV1::Serving,
                coverage: DoctorCoverageCompletenessV1::Complete,
                readiness: RemoteReadinessKindV1::Ready,
                enrollment_configured: true,
                authority: RemoteAuthoritySummaryV1::Available {
                    fence: RemoteFenceSummaryV1 {
                        brain_id: "brain.status".to_owned(),
                        shard_id: "shard.status".to_owned(),
                        generation_id: "generation.status".to_owned(),
                        placement_revision: 1,
                        authority_epoch: 1,
                        authority_node_id: "node.authority".to_owned(),
                    },
                },
                spool: RemoteSpoolSummaryV1 {
                    pending_count: 0,
                    quarantined_count: 0,
                    has_sequence_gap: false,
                },
                replay_coverage_complete: true,
                current_backup_verified: true,
                failover_in_progress: false,
                recovery_required: false,
                observed_at: UtcMicros(10),
            }
        );
    }

    #[test]
    fn present_reader_preserves_unconfigured_and_unavailable_kinds() {
        let unconfigured: crate::RemoteOperationalStatusReader =
            Arc::new(|| RemoteOperationalStatusReadV1::Unconfigured);
        let envelope = status_from_reader(scope(), Some(unconfigured));
        assert_eq!(
            envelope.payload,
            RemoteOperationalStatusPayloadV1::Unconfigured
        );
        assert_eq!(envelope.domain_state, DashboardDomainStateV1::Unknown);

        let unavailable: crate::RemoteOperationalStatusReader =
            Arc::new(|| RemoteOperationalStatusReadV1::Unavailable);
        let envelope = status_from_reader(scope(), Some(unavailable));
        assert_eq!(
            envelope.payload,
            RemoteOperationalStatusPayloadV1::Unavailable {
                note: "the remote operational authority is unreadable".to_owned(),
            }
        );
    }
}
