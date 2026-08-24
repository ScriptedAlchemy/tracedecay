//! Remote query coverage derived once from the canonical daemon response.

use std::sync::Arc;

use tracedecay_application::remote::auth::OpaqueRemoteCredential;
use tracedecay_application::remote::composition::{
    PendingLocalEvidenceV1, PendingLocalUnavailableReasonV1, ShardCoverageStateV1,
};
use tracedecay_application::remote::credential_admission::RemoteCredentialClassV1;
use tracedecay_application::remote::protocol::{
    RemoteProtocolPortV1, RemoteProtocolRequestV1, RemoteProtocolResponseV1,
};
use tracedecay_application::remote::query::{
    RemoteExactObservationQueryProtocolAdapterV1, RemoteExactObservationQueryServiceV1,
    RemoteQueryRequestV1, RemoteQueryResultV1,
};
use tracedecay_application::{ApplicationContractError, ApplicationOutcome, OperationTermination};
use tracedecay_domain::{
    CoverageStateV1, CurrentRemoteAuthorityStateV1, ObservedTernaryV1,
    RemoteAuthorityUnavailableReasonV1, RemoteCoverageObservedV1, RemoteOperationV1, UtcMicros,
};
use tracedecay_usecases::observability::BoundedObservabilityProducerV1;

use super::DaemonRemoteCredentialAuthorityV1;
use crate::daemon::remote_query::DaemonRemoteExactObservationQueryPortV1;
use crate::daemon::remote_replay_transaction::DaemonRemoteReplayTransactionAuthorityV1;
use crate::daemon::service::invocation::DaemonInvocationService;

pub(super) struct DaemonRemoteQueryProtocolPortV1 {
    credentials: Arc<DaemonRemoteCredentialAuthorityV1>,
    targets: Arc<DaemonRemoteReplayTransactionAuthorityV1>,
    invocation: DaemonInvocationService,
}

impl DaemonRemoteQueryProtocolPortV1 {
    pub(super) fn new(
        credentials: Arc<DaemonRemoteCredentialAuthorityV1>,
        targets: Arc<DaemonRemoteReplayTransactionAuthorityV1>,
        invocation: DaemonInvocationService,
    ) -> Self {
        Self {
            credentials,
            targets,
            invocation,
        }
    }
}

impl RemoteProtocolPortV1<RemoteQueryRequestV1> for DaemonRemoteQueryProtocolPortV1 {
    type Output = RemoteQueryResultV1;

    fn execute(
        &self,
        request: RemoteProtocolRequestV1<RemoteQueryRequestV1>,
        credential: OpaqueRemoteCredential,
    ) -> Result<RemoteProtocolResponseV1<Self::Output>, ApplicationContractError> {
        let request_id = request.request_id.clone();
        let observed_at = request.sent_at;
        let expected_shards = request.body.expected_shards.len();
        let response = match self
            .credentials
            .storage_for_presented(RemoteCredentialClassV1::Enrollment, &credential)
        {
            Ok(registered) => {
                let storage = Arc::new(registered.storage);
                RemoteExactObservationQueryProtocolAdapterV1::new(
                    RemoteExactObservationQueryServiceV1::new(
                        storage.clone(),
                        storage.clone(),
                        Arc::new(DaemonRemoteExactObservationQueryPortV1::new(
                            storage,
                            Arc::clone(&self.targets),
                        )),
                    ),
                )
                .execute(request, credential)?
            }
            Err(_) => super::unavailable_response(
                request_id,
                observed_at,
                tracedecay_application::remote::query::
                    remote_exact_observation_query_result_contract_v1(),
            )?,
        };
        if let Ok(envelope) = &response.result {
            let producer = self
                .invocation
                .observability_producer_for_brain_profile_project(
                    &self.credentials.brain_id,
                    &self.credentials.profile_id,
                    &envelope.scope.project_id,
                );
            record_remote_query_response(producer.as_deref(), expected_shards, &response);
        }
        Ok(response)
    }
}

pub(super) fn record_remote_query_response(
    producer: Option<&BoundedObservabilityProducerV1>,
    expected_shards: usize,
    response: &RemoteProtocolResponseV1<RemoteQueryResultV1>,
) {
    let Some(producer) = producer else {
        return;
    };
    let observation = remote_query_response_observation(expected_shards, response);
    let _ = tracedecay_usecases::observability::record_remote_coverage_observation(
        Some(producer),
        observation,
        remote_authority_observed_at(&response.authority),
    );
}

pub(super) fn remote_query_result_observation(
    operation_ref: &str,
    expected_shards: usize,
    result: &RemoteQueryResultV1,
    terminal_succeeded: ObservedTernaryV1,
) -> RemoteCoverageObservedV1 {
    let expected_shards = u32::try_from(expected_shards).ok();
    let observed_shards = u32::try_from(result.composition.contributions.len()).ok();
    let (pending_local_evidence, pending_reason) = pending_local(&result.composition.pending_local);
    let count_reason = (expected_shards.is_none() || observed_shards.is_none())
        .then_some("remote_shard_count_overflow");
    let mut coverage = match result.composition.coverage {
        ShardCoverageStateV1::Complete => CoverageStateV1::Known,
        ShardCoverageStateV1::Stale => CoverageStateV1::Stale,
        ShardCoverageStateV1::Partial => CoverageStateV1::Partial,
        ShardCoverageStateV1::Unknown | ShardCoverageStateV1::Unavailable => {
            CoverageStateV1::Unknown
        }
    };
    if (pending_reason.is_some() || count_reason.is_some()) && coverage == CoverageStateV1::Known {
        coverage = CoverageStateV1::Unknown;
    }
    let unavailable_reason = (coverage != CoverageStateV1::Known).then(|| {
        pending_reason
            .or(count_reason)
            .or_else(|| {
                result
                    .composition
                    .contributions
                    .iter()
                    .find_map(|contribution| contribution.reason_code.as_deref())
            })
            .unwrap_or("remote_query_coverage_incomplete")
            .to_owned()
    });
    RemoteCoverageObservedV1 {
        operation_ref: operation_ref.to_owned(),
        operation: RemoteOperationV1::Query,
        expected_shards,
        observed_shards,
        pending_local_evidence,
        terminal_succeeded,
        coverage,
        unavailable_reason,
    }
}

fn pending_local(pending: &PendingLocalEvidenceV1) -> (Option<u32>, Option<&'static str>) {
    match pending {
        PendingLocalEvidenceV1::Available { evidence } => match u32::try_from(evidence.count) {
            Ok(count) if evidence.has_sequence_gap || evidence.has_quarantined => {
                (Some(count), Some("pending_local_evidence_incomplete"))
            }
            Ok(count) if count > 0 => (Some(count), Some("pending_local_evidence")),
            Ok(count) => (Some(count), None),
            Err(_) => (None, Some("pending_local_evidence_count_overflow")),
        },
        PendingLocalEvidenceV1::Unavailable { reason } => (
            None,
            Some(match reason {
                PendingLocalUnavailableReasonV1::RequestingNodeSpoolNotSupplied => {
                    "requesting_node_spool_not_supplied"
                }
                PendingLocalUnavailableReasonV1::AuthorityUnavailable => {
                    "pending_local_authority_unavailable"
                }
            }),
        ),
    }
}

fn remote_query_response_observation(
    expected_shards: usize,
    response: &RemoteProtocolResponseV1<RemoteQueryResultV1>,
) -> RemoteCoverageObservedV1 {
    let operation_ref = response.request_id.as_str();
    match &response.result {
        Ok(envelope) => match &envelope.outcome {
            ApplicationOutcome::Evidence(packet) => match &packet.payload {
                Some(result) => {
                    let mut observation = remote_query_result_observation(
                        operation_ref,
                        expected_shards,
                        result,
                        terminal_succeeded(packet.execution.termination),
                    );
                    apply_authority(&mut observation, &response.authority);
                    observation
                }
                None => unavailable_observation(
                    operation_ref,
                    expected_shards,
                    terminal_succeeded(packet.execution.termination),
                    "remote_query_result_payload_unavailable",
                ),
            },
            ApplicationOutcome::Preview(_) | ApplicationOutcome::Effect(_) => {
                unavailable_observation(
                    operation_ref,
                    expected_shards,
                    ObservedTernaryV1::Unknown,
                    "remote_query_result_kind_unavailable",
                )
            }
        },
        Err(problem) => unavailable_observation(
            operation_ref,
            expected_shards,
            ObservedTernaryV1::No,
            &problem.problem.code,
        ),
    }
}

fn unavailable_observation(
    operation_ref: &str,
    expected_shards: usize,
    terminal_succeeded: ObservedTernaryV1,
    reason: &str,
) -> RemoteCoverageObservedV1 {
    RemoteCoverageObservedV1 {
        operation_ref: operation_ref.to_owned(),
        operation: RemoteOperationV1::Query,
        expected_shards: u32::try_from(expected_shards).ok(),
        observed_shards: None,
        pending_local_evidence: None,
        terminal_succeeded,
        coverage: CoverageStateV1::Unknown,
        unavailable_reason: Some(reason.to_owned()),
    }
}

fn apply_authority(
    observation: &mut RemoteCoverageObservedV1,
    authority: &CurrentRemoteAuthorityStateV1,
) {
    let reason = match authority {
        CurrentRemoteAuthorityStateV1::Available(_) => return,
        CurrentRemoteAuthorityStateV1::Partial { missing, .. } => {
            if observation.coverage != CoverageStateV1::Unknown {
                observation.coverage = CoverageStateV1::Partial;
            }
            missing.iter().next().map(authority_reason)
        }
        CurrentRemoteAuthorityStateV1::Unavailable { reason, .. } => {
            observation.coverage = CoverageStateV1::Unknown;
            Some(authority_reason(reason))
        }
    };
    observation.unavailable_reason =
        Some(reason.unwrap_or("remote_authority_incomplete").to_owned());
}

const fn terminal_succeeded(termination: OperationTermination) -> ObservedTernaryV1 {
    match termination {
        OperationTermination::Completed => ObservedTernaryV1::Yes,
        OperationTermination::EffectUnknown => ObservedTernaryV1::Unknown,
        OperationTermination::Cancelled
        | OperationTermination::TimedOut
        | OperationTermination::Failed
        | OperationTermination::Unavailable
        | OperationTermination::Partial => ObservedTernaryV1::No,
    }
}

const fn authority_reason(reason: &RemoteAuthorityUnavailableReasonV1) -> &'static str {
    match reason {
        RemoteAuthorityUnavailableReasonV1::RegistryUnavailable => "remote_registry_unavailable",
        RemoteAuthorityUnavailableReasonV1::PlacementUnknown => "remote_placement_unknown",
        RemoteAuthorityUnavailableReasonV1::AuthorityUnreachable => "remote_authority_unreachable",
        RemoteAuthorityUnavailableReasonV1::AuthorityAuthenticationFailed => {
            "remote_authority_authentication_failed"
        }
        RemoteAuthorityUnavailableReasonV1::CallerAuthenticationFailed => {
            "remote_caller_authentication_failed"
        }
        RemoteAuthorityUnavailableReasonV1::EnrollmentExpired => "remote_enrollment_expired",
        RemoteAuthorityUnavailableReasonV1::EnrollmentRevoked => "remote_enrollment_revoked",
        RemoteAuthorityUnavailableReasonV1::InsufficientCapability => {
            "remote_insufficient_capability"
        }
        RemoteAuthorityUnavailableReasonV1::ScopeMismatch => "remote_scope_mismatch",
        RemoteAuthorityUnavailableReasonV1::FenceUnverified => "remote_fence_unverified",
        RemoteAuthorityUnavailableReasonV1::ProtocolIncompatible => "remote_protocol_incompatible",
    }
}

const fn remote_authority_observed_at(authority: &CurrentRemoteAuthorityStateV1) -> UtcMicros {
    match authority {
        CurrentRemoteAuthorityStateV1::Available(authority) => authority.observed_at,
        CurrentRemoteAuthorityStateV1::Partial { observed_at, .. }
        | CurrentRemoteAuthorityStateV1::Unavailable { observed_at, .. } => *observed_at,
    }
}
