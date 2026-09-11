//! Daemon-wide Remote Brain credential routing and protocol composition.
//!
//! Credential bytes are fingerprinted before lookup and never retained. The
//! only routing entries come from exact registered Remote-node runtimes; no
//! path, request body, or caller-supplied node identity can select a store.

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use axum::Router;
use tracedecay_contracts::remote::auth::{
    OpaqueRemoteCredential, RemoteEnrollmentProtocolAdapterV1,
};
use tracedecay_contracts::remote::capture::RemoteCaptureReceiptV1;
use tracedecay_contracts::remote::capture_protocol::{
    RemoteCaptureRequestV1, RemoteOfflineCaptureProtocolAdapterV1,
    RemoteOfflineCaptureProtocolServiceV1,
};
use tracedecay_contracts::remote::credential_admission::{
    RemoteCredentialAdmissionServiceV1, RemoteCredentialClassV1, RemoteSessionBoundProtocolBodyV1,
};
use tracedecay_contracts::remote::protocol::{
    EnrollmentRequestV1, RemoteEnrollmentProtocolPortV1, RemoteProtocolExecutionControlV1,
    RemoteProtocolPortV1, RemoteProtocolRequestV1, RemoteProtocolResponseV1,
    remote_capture_result_contract_v1, remote_enrollment_result_contract_v1,
    remote_replay_result_contract_v1,
};
use tracedecay_contracts::remote::protocol_owner::RemoteOperationProtocolPortsV1;
use tracedecay_contracts::remote::recovery::{
    BackupOperationStateV1, BackupRequestV1, PromotionCasReceiptV1, PromotionConfirmationV1,
    RemoteRecoveryControlPortV1, RemoteRecoveryInterruptionV1, RemoteRecoveryProtocolOwnerV1,
    StagedRestoreConfirmationV1, StagedRestoreProgressV1,
};
use tracedecay_contracts::remote::replay::{
    RemoteReplayOutcomeV1, RemoteReplayProtocolAdapterV1, RemoteReplayRequestV1,
    RemoteReplayServiceV1,
};
use tracedecay_contracts::{
    ApplicationContractError, CancellationSignal, RequestId, ResultContractRef,
};
use tracedecay_domain::{EnrollmentCredentialRecordV1, UtcMicros};
use tracedecay_tool_catalog::SchemaId;

use crate::DaemonInvocationService;
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_store_runtime::remote_credentials::{
    presented_spool_keyring, remote_authority_unavailable_response,
};
use tracedecay_store_runtime::{
    DaemonRemoteFrameTransferProtocolPortV1, DaemonRemoteReplayTransactionAuthorityV1,
};

mod observability;

use tracedecay_store_runtime::{DaemonRemoteCredentialAuthorityV1, DaemonRemoteCredentialLookupV1};

struct DaemonRemoteEnrollmentProtocolPortV1 {
    credentials: Arc<DaemonRemoteCredentialAuthorityV1>,
}

impl RemoteEnrollmentProtocolPortV1 for DaemonRemoteEnrollmentProtocolPortV1 {
    #[hotpath::measure(label = "daemon.remote.enrollment")]
    fn execute_enrollment(
        &self,
        request: RemoteProtocolRequestV1<EnrollmentRequestV1>,
        grant_credential: OpaqueRemoteCredential,
        enrollment_credential: OpaqueRemoteCredential,
    ) -> std::result::Result<
        RemoteProtocolResponseV1<EnrollmentCredentialRecordV1>,
        ApplicationContractError,
    > {
        let request_id = request.request_id.clone();
        let observed_at = request.sent_at;
        let registered = match self
            .credentials
            .storage_for_presented(RemoteCredentialClassV1::EnrollmentGrant, &grant_credential)
        {
            Ok(registered) => registered,
            Err(_) => {
                return remote_authority_unavailable_response(
                    request_id,
                    observed_at,
                    remote_enrollment_result_contract_v1(),
                );
            }
        };
        if self.credentials.ensure_accepting().is_err() {
            return remote_authority_unavailable_response(
                request_id,
                observed_at,
                remote_enrollment_result_contract_v1(),
            );
        }
        let response = RemoteEnrollmentProtocolAdapterV1::new(registered.storage)
            .execute_enrollment(request, grant_credential, enrollment_credential);
        if self
            .credentials
            .refresh_storage(&registered.node_id)
            .is_err()
        {
            return remote_authority_unavailable_response(
                request_id,
                observed_at,
                remote_enrollment_result_contract_v1(),
            );
        }
        response
    }
}

struct DaemonRemoteCaptureProtocolPortV1 {
    credentials: Arc<DaemonRemoteCredentialAuthorityV1>,
}

impl RemoteProtocolPortV1<RemoteCaptureRequestV1> for DaemonRemoteCaptureProtocolPortV1 {
    type Output = RemoteCaptureReceiptV1;

    #[hotpath::measure(label = "daemon.remote.capture")]
    fn execute(
        &self,
        request: RemoteProtocolRequestV1<RemoteCaptureRequestV1>,
        credential: OpaqueRemoteCredential,
    ) -> std::result::Result<RemoteProtocolResponseV1<Self::Output>, ApplicationContractError> {
        let request_id = request.request_id.clone();
        let observed_at = request.sent_at;
        let registered = match self
            .credentials
            .storage_for_presented(RemoteCredentialClassV1::Enrollment, &credential)
        {
            Ok(registered) => registered,
            Err(_) => {
                return remote_authority_unavailable_response(
                    request_id,
                    observed_at,
                    remote_capture_result_contract_v1(),
                );
            }
        };
        let Some(keyring) = presented_spool_keyring(&credential, request.enrollment_revision)
        else {
            return remote_authority_unavailable_response(
                request_id,
                observed_at,
                remote_capture_result_contract_v1(),
            );
        };
        let storage = registered.storage.with_keyring(keyring);
        let shared = Arc::new(storage.clone());
        RemoteOfflineCaptureProtocolAdapterV1::new(RemoteOfflineCaptureProtocolServiceV1::new(
            shared.clone(),
            shared,
            storage,
            tracedecay_contracts::clock::now_micros,
        ))
        .execute(request, credential)
    }
}

struct DaemonRemoteReplayProtocolPortV1 {
    credentials: Arc<DaemonRemoteCredentialAuthorityV1>,
    transaction: Arc<DaemonRemoteReplayTransactionAuthorityV1>,
}

impl RemoteProtocolPortV1<RemoteReplayRequestV1> for DaemonRemoteReplayProtocolPortV1 {
    type Output = RemoteReplayOutcomeV1;

    #[hotpath::measure(label = "daemon.remote.replay_protocol")]
    fn execute(
        &self,
        request: RemoteProtocolRequestV1<RemoteReplayRequestV1>,
        credential: OpaqueRemoteCredential,
    ) -> std::result::Result<RemoteProtocolResponseV1<Self::Output>, ApplicationContractError> {
        let request_id = request.request_id.clone();
        let observed_at = request.sent_at;
        let registered = match self
            .credentials
            .storage_for_presented(RemoteCredentialClassV1::Enrollment, &credential)
        {
            Ok(registered) => registered,
            Err(_) => {
                return remote_authority_unavailable_response(
                    request_id,
                    observed_at,
                    remote_replay_result_contract_v1(),
                );
            }
        };
        let Some(keyring) = presented_spool_keyring(&credential, request.enrollment_revision)
        else {
            return remote_authority_unavailable_response(
                request_id,
                observed_at,
                remote_replay_result_contract_v1(),
            );
        };
        let storage = Arc::new(registered.storage.with_keyring(keyring));
        RemoteReplayProtocolAdapterV1::new(RemoteReplayServiceV1::new(
            storage.clone(),
            storage.clone(),
            storage.clone(),
            storage.clone(),
            storage.clone(),
            storage.clone(),
            self.transaction.clone(),
            storage,
        ))
        .execute(request, credential)
    }
}

struct DaemonRemoteRecoveryControlV1 {
    credentials: Arc<DaemonRemoteCredentialAuthorityV1>,
    cancellation: CancellationSignal,
    deadline: UtcMicros,
    clock: fn() -> UtcMicros,
    interruption: AtomicU8,
}

impl RemoteRecoveryControlPortV1 for DaemonRemoteRecoveryControlV1 {
    fn interruption(&self, _request_id: &RequestId) -> Option<RemoteRecoveryInterruptionV1> {
        let observed = self.interruption.load(Ordering::Acquire);
        if observed == 1 {
            return Some(RemoteRecoveryInterruptionV1::Cancelled);
        }
        if observed == 2 {
            return Some(RemoteRecoveryInterruptionV1::DeadlineExceeded);
        }
        let next =
            if self.cancellation.is_cancelled() || self.credentials.ensure_accepting().is_err() {
                1
            } else if (self.clock)() >= self.deadline {
                2
            } else {
                return None;
            };
        let preserved =
            match self
                .interruption
                .compare_exchange(0, next, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => next,
                Err(existing) => existing,
            };
        match preserved {
            1 => Some(RemoteRecoveryInterruptionV1::Cancelled),
            2 => Some(RemoteRecoveryInterruptionV1::DeadlineExceeded),
            _ => None,
        }
    }

    fn effective_deadline(&self, _request_id: &RequestId) -> Option<UtcMicros> {
        Some(self.deadline)
    }
}

struct DaemonRemoteRecoveryProtocolPortV1 {
    credentials: Arc<DaemonRemoteCredentialAuthorityV1>,
    backup_contract: ResultContractRef,
    restore_contract: ResultContractRef,
    promotion_contract: ResultContractRef,
}

macro_rules! impl_daemon_remote_recovery_protocol {
    ($request:ty, $output:ty, $contract:ident) => {
        impl RemoteProtocolPortV1<$request> for DaemonRemoteRecoveryProtocolPortV1 {
            type Output = $output;

            fn execute(
                &self,
                request: RemoteProtocolRequestV1<$request>,
                credential: OpaqueRemoteCredential,
            ) -> std::result::Result<RemoteProtocolResponseV1<Self::Output>, ApplicationContractError> {
                let contract = self.$contract.clone();
                let Some(deadline) = request.body.execution_expires_at() else {
                    return remote_authority_unavailable_response(request.request_id, request.sent_at, contract);
                };
                let cancellation = match CancellationSignal::active(format!(
                    "cancel.remote.direct.{}",
                    request.request_id.as_str()
                )) {
                    Ok(cancellation) => cancellation,
                    Err(_) => {
                        return remote_authority_unavailable_response(request.request_id, request.sent_at, contract);
                    }
                };
                self.execute_controlled(
                    request,
                    credential,
                    RemoteProtocolExecutionControlV1 {
                        deadline,
                        cancellation,
                    },
                )
            }

            #[hotpath::measure(label = "daemon.remote.recovery")]
            fn execute_controlled(
                &self,
                request: RemoteProtocolRequestV1<$request>,
                credential: OpaqueRemoteCredential,
                control: RemoteProtocolExecutionControlV1,
            ) -> std::result::Result<RemoteProtocolResponseV1<Self::Output>, ApplicationContractError> {
                let contract = self.$contract.clone();
                let registered = match self
                    .credentials
                    .storage_for_presented(RemoteCredentialClassV1::Enrollment, &credential)
                {
                    Ok(registered) => registered,
                    Err(_) => {
                        return remote_authority_unavailable_response(request.request_id, request.sent_at, contract);
                    }
                };
                let Some(recovery) = registered.recovery else {
                    return remote_authority_unavailable_response(request.request_id, request.sent_at, contract);
                };
                let admission = Arc::new(RemoteCredentialAdmissionServiceV1::new(
                    DaemonRemoteCredentialLookupV1::new(Arc::clone(&self.credentials)),
                ));
                let owner = RemoteRecoveryProtocolOwnerV1::new(
                    admission,
                    recovery,
                    Arc::new(DaemonRemoteRecoveryControlV1 {
                        credentials: Arc::clone(&self.credentials),
                        cancellation: control.cancellation,
                        deadline: control.deadline,
                        clock: tracedecay_contracts::clock::now_micros,
                        interruption: AtomicU8::new(0),
                    }),
                    tracedecay_contracts::clock::now_micros,
                );
                owner.execute(request, credential)
            }
        }
    };
}

#[hotpath::measure(label = "daemon.remote.router_build")]
pub fn build_daemon_remote_protocol_router(
    credentials: Arc<DaemonRemoteCredentialAuthorityV1>,
    transaction: Arc<DaemonRemoteReplayTransactionAuthorityV1>,
    invocation: DaemonInvocationService,
) -> Result<Router> {
    let recovery = Arc::new(DaemonRemoteRecoveryProtocolPortV1 {
        credentials: Arc::clone(&credentials),
        backup_contract: remote_result_contract("remote.backup.result")?,
        restore_contract: remote_result_contract("remote.restore.result")?,
        promotion_contract: remote_result_contract("remote.promotion.result")?,
    });
    let enrollment = Arc::new(DaemonRemoteEnrollmentProtocolPortV1 {
        credentials: Arc::clone(&credentials),
    });
    let operations = RemoteOperationProtocolPortsV1 {
        capture: Arc::new(DaemonRemoteCaptureProtocolPortV1 {
            credentials: Arc::clone(&credentials),
        }),
        replay: Arc::new(DaemonRemoteReplayProtocolPortV1 {
            credentials: Arc::clone(&credentials),
            transaction: Arc::clone(&transaction),
        }),
        frame_transfer: Arc::new(DaemonRemoteFrameTransferProtocolPortV1::new(Arc::clone(
            &credentials,
        ))),
        query: Arc::new(observability::DaemonRemoteQueryProtocolPortV1::new(
            Arc::clone(&credentials),
            transaction,
            invocation,
        )),
        backup: recovery.clone(),
        restore: recovery.clone(),
        promotion: recovery,
    };
    let admission = Arc::new(RemoteCredentialAdmissionServiceV1::new(
        DaemonRemoteCredentialLookupV1::new(credentials),
    ));
    Ok(tracedecay_api::remote::remote_protocol_router(
        enrollment,
        operations,
        admission,
        tracedecay_contracts::clock::now_micros,
    ))
}

impl_daemon_remote_recovery_protocol!(BackupRequestV1, BackupOperationStateV1, backup_contract);
impl_daemon_remote_recovery_protocol!(
    StagedRestoreConfirmationV1,
    StagedRestoreProgressV1,
    restore_contract
);
impl_daemon_remote_recovery_protocol!(
    PromotionConfirmationV1,
    PromotionCasReceiptV1,
    promotion_contract
);

fn remote_result_contract(schema_id: &str) -> Result<ResultContractRef> {
    let schema_id = SchemaId::new(schema_id).map_err(|error| TraceDecayError::Config {
        message: format!("remote protocol result schema identity is invalid: {error}"),
    })?;
    ResultContractRef::new(schema_id, 1).map_err(|error| TraceDecayError::Config {
        message: format!("remote protocol result contract is invalid: {error}"),
    })
}

#[cfg(test)]
mod recovery_control_tests {
    use super::*;
    use tracedecay_domain::{BrainId, UserProfileId};

    fn before_deadline() -> UtcMicros {
        UtcMicros(10)
    }

    fn at_deadline() -> UtcMicros {
        UtcMicros(20)
    }

    fn credentials() -> Arc<DaemonRemoteCredentialAuthorityV1> {
        Arc::new(DaemonRemoteCredentialAuthorityV1::new(
            BrainId::new("brain.recovery-control").unwrap(),
            UserProfileId::new("profile.recovery-control").unwrap(),
        ))
    }

    #[test]
    fn recovery_control_carries_deadline_and_stable_daemon_cancellation() {
        let request_id = RequestId::new("request.recovery-control").unwrap();
        let deadline_credentials = credentials();
        let deadline = DaemonRemoteRecoveryControlV1 {
            credentials: Arc::clone(&deadline_credentials),
            cancellation: CancellationSignal::active("cancel.recovery.deadline").unwrap(),
            deadline: UtcMicros(20),
            clock: at_deadline,
            interruption: AtomicU8::new(0),
        };
        assert_eq!(
            deadline.interruption(&request_id),
            Some(RemoteRecoveryInterruptionV1::DeadlineExceeded)
        );
        deadline_credentials.cancel();
        assert_eq!(
            deadline.interruption(&request_id),
            Some(RemoteRecoveryInterruptionV1::DeadlineExceeded)
        );

        let cancellation_credentials = credentials();
        let cancellation = DaemonRemoteRecoveryControlV1 {
            credentials: Arc::clone(&cancellation_credentials),
            cancellation: CancellationSignal::active("cancel.recovery.client").unwrap(),
            deadline: UtcMicros(20),
            clock: before_deadline,
            interruption: AtomicU8::new(0),
        };
        cancellation.cancellation.cancel(UtcMicros(11));
        assert_eq!(
            cancellation.interruption(&request_id),
            Some(RemoteRecoveryInterruptionV1::Cancelled)
        );
    }
}

#[cfg(test)]
mod observation_tests {
    use tracedecay_contracts::remote::composition::{
        AuthenticityClaimV1, AuthorizationClaimV1, IntegrityClaimV1, PendingLocalEvidenceV1,
        PendingLocalObservationsV1, QueryManifestBindingV1, RemoteCompletenessV1,
        RemoteFreshnessV1, RemoteQueryCompositionV1, ShardCoverageStateV1,
        ShardQueryContributionV1,
    };
    use tracedecay_contracts::remote::query::{
        RemoteExactObservationResultV1, RemoteQueryResultV1,
    };
    use tracedecay_domain::{CoverageStateV1, ObservedTernaryV1};

    use super::observability::remote_query_result_observation;

    fn remote_query_result(
        coverage: ShardCoverageStateV1,
        pending_local: PendingLocalEvidenceV1,
    ) -> RemoteQueryResultV1 {
        RemoteQueryResultV1 {
            composition: RemoteQueryCompositionV1 {
                contributions: vec![ShardQueryContributionV1 {
                    manifest: QueryManifestBindingV1 {
                        brain_id: "brain.remote-coverage".to_owned(),
                        shard_id: "shard.remote-coverage".to_owned(),
                        generation_id: "generation.remote-coverage".to_owned(),
                        schema_digest: [1; 32],
                        watermark_sequence: 1,
                        placement_revision: 1,
                        authority_epoch: 1,
                        cache_age_millis: 0,
                        cache_lag_commits: 0,
                    },
                    integrity: IntegrityClaimV1::Verified,
                    authenticity: AuthenticityClaimV1::Authenticated,
                    freshness: RemoteFreshnessV1::Current,
                    completeness: RemoteCompletenessV1::Complete,
                    authorization: AuthorizationClaimV1::Authorized,
                    coverage,
                    authority_receipt: None,
                    value: None,
                    reason_code: (coverage != ShardCoverageStateV1::Complete)
                        .then(|| "remote_shard_degraded".to_owned()),
                }],
                pending_local,
                coverage,
            },
            observation: RemoteExactObservationResultV1::NotFound,
        }
    }

    #[test]
    fn remote_query_coverage_preserves_real_shard_and_pending_counts() {
        let result = remote_query_result(
            ShardCoverageStateV1::Stale,
            PendingLocalObservationsV1 {
                count: 3,
                oldest_age_millis: Some(9),
                has_sequence_gap: false,
                has_quarantined: false,
            }
            .into(),
        );
        result.validate().expect("valid stale remote query result");

        let observation = remote_query_result_observation(
            "request.remote-coverage",
            1,
            &result,
            ObservedTernaryV1::Yes,
        );

        assert_eq!(observation.expected_shards, Some(1));
        assert_eq!(observation.observed_shards, Some(1));
        assert_eq!(observation.pending_local_evidence, Some(3));
        assert_eq!(observation.terminal_succeeded, ObservedTernaryV1::Yes);
        assert_eq!(observation.coverage, CoverageStateV1::Stale);
        assert_eq!(
            observation.unavailable_reason.as_deref(),
            Some("pending_local_evidence")
        );
    }

    #[test]
    fn remote_query_coverage_does_not_fabricate_unavailable_pending_count() {
        let result = remote_query_result(
            ShardCoverageStateV1::Unknown,
            PendingLocalEvidenceV1::Unavailable {
                reason: tracedecay_contracts::remote::composition::PendingLocalUnavailableReasonV1::AuthorityUnavailable,
            },
        );
        result
            .validate()
            .expect("valid unavailable remote query result");

        let observation = remote_query_result_observation(
            "request.remote-coverage-unavailable",
            1,
            &result,
            ObservedTernaryV1::Unknown,
        );

        assert_eq!(observation.pending_local_evidence, None);
        assert_eq!(observation.terminal_succeeded, ObservedTernaryV1::Unknown);
        assert_eq!(observation.coverage, CoverageStateV1::Unknown);
        assert_eq!(
            observation.unavailable_reason.as_deref(),
            Some("pending_local_authority_unavailable")
        );
    }
}
