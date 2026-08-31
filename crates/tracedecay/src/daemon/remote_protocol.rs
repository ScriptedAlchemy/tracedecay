//! Daemon-wide Remote Brain credential routing and protocol composition.
//!
//! Credential bytes are fingerprinted before lookup and never retained. The
//! only routing entries come from exact registered Remote-node runtimes; no
//! path, request body, or caller-supplied node identity can select a store.

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use axum::Router;
use tracedecay_application::remote::auth::{
    OpaqueRemoteCredential, RemoteEnrollmentProtocolAdapterV1,
};
use tracedecay_application::remote::capture::RemoteCaptureReceiptV1;
use tracedecay_application::remote::capture_protocol::{
    RemoteCaptureRequestV1, RemoteOfflineCaptureProtocolAdapterV1,
    RemoteOfflineCaptureProtocolServiceV1,
};
use tracedecay_application::remote::credential_admission::{
    RemoteCredentialAdmissionPortV1, RemoteCredentialAdmissionServiceV1,
    RemoteCredentialAuthorityRecordV1, RemoteCredentialClassV1, RemoteCredentialLookupErrorV1,
    RemoteCredentialLookupPortV1, RemoteSessionBoundProtocolBodyV1,
};
use tracedecay_application::remote::protocol::{
    EnrollmentRequestV1, RemoteEnrollmentProtocolPortV1, RemoteProtocolExecutionControlV1,
    RemoteProtocolFailureV1, RemoteProtocolPortV1, RemoteProtocolRequestV1,
    RemoteProtocolResponseV1, remote_capture_result_contract_v1,
    remote_enrollment_result_contract_v1, remote_protocol_problem,
    remote_replay_result_contract_v1,
};
use tracedecay_application::remote::protocol_owner::{
    RemoteOperationProtocolPortsV1, RemoteProtocolOwnerV1,
};
use tracedecay_application::remote::recovery::{
    BackupOperationStateV1, BackupRequestV1, PromotionCasReceiptV1, PromotionConfirmationV1,
    RemoteRecoveryControlPortV1, RemoteRecoveryInterruptionV1, RemoteRecoveryProtocolOwnerV1,
    StagedRestoreConfirmationV1, StagedRestoreProgressV1,
};
use tracedecay_application::remote::replay::{
    RemoteReplayOutcomeV1, RemoteReplayProtocolAdapterV1, RemoteReplayRequestV1,
    RemoteReplayServiceV1,
};
use tracedecay_application::remote::status::RemoteOperationalStatusReadV1;
use tracedecay_application::remote::transfer::{
    REMOTE_FRAME_TRANSFER_USE_CASE_ID_V1, RemoteFrameTransferErrorV1, RemoteFrameTransferPortV1,
    RemoteFrameTransferReceiptV1, RemoteFrameTransferRequestV1,
    remote_frame_transfer_result_contract_v1,
};
use tracedecay_application::{
    ApplicationContractError, ApplicationEnvelope, ApplicationProblem, ApplicationProblemEnvelope,
    CancellationSignal, Deadline, EffectId, EffectReceipt, EffectResult, EffectTermination,
    IdempotencyKey, OperationBudgetUsage, OperationReceipt, ReconciliationState, RequestId,
    ResultContractRef,
};
use tracedecay_domain::{
    CurrentRemoteAuthorityStateV1, EnrollmentCredentialRecordV1,
    RemoteAuthorityUnavailableReasonV1, UtcMicros, canonical_sha256,
};
use tracedecay_rusqlite_runtime::remote::{
    CredentialDerivedSpoolKeyringV1, RemoteSpoolKeyringV1,
};
use tracedecay_tool_catalog::SchemaId;

use tracedecay_store_runtime::DaemonRemoteReplayTransactionAuthorityV1;
use tracedecay_daemon_service::DaemonInvocationService;
use tracedecay_domain::errors::{Result, TraceDecayError};

mod observability;

#[cfg(test)]
pub(super) fn remote_query_result_observation(
    operation_ref: &str,
    expected_shards: usize,
    result: &tracedecay_application::remote::query::RemoteQueryResultV1,
    terminal_succeeded: tracedecay_domain::ObservedTernaryV1,
) -> tracedecay_domain::RemoteCoverageObservedV1 {
    observability::remote_query_result_observation(
        operation_ref,
        expected_shards,
        result,
        terminal_succeeded,
    )
}

use tracedecay_store_runtime::{
    DaemonRemoteCredentialAuthorityV1, DaemonRemoteCredentialLookupV1,
};


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
                return unavailable_response(
                    request_id,
                    observed_at,
                    remote_enrollment_result_contract_v1(),
                );
            }
        };
        if self.credentials.ensure_accepting().is_err() {
            return unavailable_response(
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
            return unavailable_response(
                request_id,
                observed_at,
                remote_enrollment_result_contract_v1(),
            );
        }
        response
    }
}

/// Request-scoped spool keyring derived from the presented enrollment
/// credential. Spool frames stay encrypted at rest; the key exists only while
/// the authenticated request executes.
fn presented_spool_keyring(
    credential: &OpaqueRemoteCredential,
    enrollment_revision: u64,
) -> Option<Arc<dyn RemoteSpoolKeyringV1>> {
    let bytes = credential.derive_spool_key_bytes().ok()?;
    let keyring =
        CredentialDerivedSpoolKeyringV1::from_secret_bytes(enrollment_revision, bytes).ok()?;
    Some(Arc::new(keyring))
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
                return unavailable_response(
                    request_id,
                    observed_at,
                    remote_capture_result_contract_v1(),
                );
            }
        };
        let Some(keyring) = presented_spool_keyring(&credential, request.enrollment_revision)
        else {
            return unavailable_response(
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
            tracedecay_application::clock::now_micros,
        ))
        .execute(request, credential)
    }
}

/// Receiving side of the reconnect transfer: this port owns no source path or
/// source store. It accepts the authenticated encrypted record, verifies it
/// against the receiving node's credential-derived key, then admits it to the
/// receiving node's own spool for canonical replay.
struct DaemonRemoteFrameTransferProtocolPortV1 {
    credentials: Arc<DaemonRemoteCredentialAuthorityV1>,
}

impl RemoteProtocolPortV1<RemoteFrameTransferRequestV1>
    for DaemonRemoteFrameTransferProtocolPortV1
{
    type Output = RemoteFrameTransferReceiptV1;

    fn execute(
        &self,
        request: RemoteProtocolRequestV1<RemoteFrameTransferRequestV1>,
        credential: OpaqueRemoteCredential,
    ) -> std::result::Result<RemoteProtocolResponseV1<Self::Output>, ApplicationContractError> {
        let contract = remote_frame_transfer_result_contract_v1()?;
        let cancellation = match CancellationSignal::active(format!(
            "cancel.remote.frame-transfer.{}",
            request.request_id.as_str()
        )) {
            Ok(cancellation) => cancellation,
            Err(_) => return unavailable_response(request.request_id, request.sent_at, contract),
        };
        let deadline = UtcMicros(request.body.expires_at_micros);
        self.execute_controlled(
            request,
            credential,
            RemoteProtocolExecutionControlV1 {
                deadline,
                cancellation,
            },
        )
    }

    #[hotpath::measure(label = "daemon.remote.frame_transfer")]
    fn execute_controlled(
        &self,
        request: RemoteProtocolRequestV1<RemoteFrameTransferRequestV1>,
        credential: OpaqueRemoteCredential,
        control: RemoteProtocolExecutionControlV1,
    ) -> std::result::Result<RemoteProtocolResponseV1<Self::Output>, ApplicationContractError> {
        let request_id = request.request_id.clone();
        let observed_at = request.sent_at;
        let contract = remote_frame_transfer_result_contract_v1()?;
        let now = tracedecay_application::clock::now_micros();
        if control.cancellation.is_cancelled() {
            return frame_transfer_interrupted_response(
                request_id,
                observed_at,
                contract,
                ApplicationProblem::cancelled_before_admission(),
            );
        }
        if now >= control.deadline || now.0 >= request.body.expires_at_micros {
            return frame_transfer_interrupted_response(
                request_id,
                observed_at,
                contract,
                ApplicationProblem::timed_out_before_admission(),
            );
        }
        let admission = RemoteCredentialAdmissionServiceV1::new(
            DaemonRemoteCredentialLookupV1::new(Arc::clone(&self.credentials)),
        );
        let session = match admission.admit_before_body(
            &credential,
            tracedecay_application::remote::credential_admission::RemoteCredentialUseV1::TransferFrame,
            tracedecay_application::clock::now_micros(),
        ) {
            Ok(session) => session,
            Err(_) => return unavailable_response(request_id, observed_at, contract),
        };
        if <RemoteFrameTransferRequestV1 as RemoteSessionBoundProtocolBodyV1>::bind_authenticated_session(
            &session, &request,
        )
        .is_err()
        {
            return unavailable_response(request_id, observed_at, contract);
        }
        let session = match admission
            .reauthorize_publication(&session, tracedecay_application::clock::now_micros())
        {
            Ok(session) => session,
            Err(_) => return unavailable_response(request_id, observed_at, contract),
        };
        if <RemoteFrameTransferRequestV1 as RemoteSessionBoundProtocolBodyV1>::bind_authenticated_session(
            &session, &request,
        )
        .is_err()
        {
            return unavailable_response(request_id, observed_at, contract);
        }
        let registered = match self
            .credentials
            .storage_for_presented(RemoteCredentialClassV1::Enrollment, &credential)
        {
            Ok(registered) => registered,
            Err(_) => return unavailable_response(request_id, observed_at, contract),
        };
        let Some(keyring) = presented_spool_keyring(&credential, request.enrollment_revision)
        else {
            return unavailable_response(request_id, observed_at, contract);
        };
        let storage = registered.storage.with_keyring(keyring);
        let authority = match storage.current_writer_authority(&request.body.writer) {
            Ok(authority) => authority,
            Err(_) => return unavailable_response(request_id, observed_at, contract),
        };
        let CurrentRemoteAuthorityStateV1::Available(current) = &authority else {
            let problem = remote_protocol_problem(
                contract,
                request_id.clone(),
                RemoteProtocolFailureV1::AuthorityUnavailable,
            )?;
            return RemoteProtocolResponseV1::new(request_id, authority, Err(problem));
        };
        if current.fence != request.body.writer.authority.fence
            || current.fence.authority_epoch.0 != request.body.observed_authority_epoch
        {
            let problem = remote_protocol_problem(
                contract,
                request_id.clone(),
                RemoteProtocolFailureV1::StaleAuthorityFence,
            )?;
            return RemoteProtocolResponseV1::new(request_id, authority, Err(problem));
        }
        let now = tracedecay_application::clock::now_micros();
        if now >= control.deadline || now.0 >= request.body.expires_at_micros {
            return frame_transfer_interrupted_response(
                request_id,
                observed_at,
                contract,
                ApplicationProblem::timed_out_before_admission(),
            );
        }
        if !control.cancellation.try_begin_commit() {
            return frame_transfer_interrupted_response(
                request_id,
                observed_at,
                contract,
                ApplicationProblem::cancelled_before_admission(),
            );
        }
        let receipt = match storage.transfer_pending(&request.body) {
            Ok(receipt) => receipt,
            Err(error) => {
                let failure = match error {
                    RemoteFrameTransferErrorV1::StaleAuthority => {
                        RemoteProtocolFailureV1::StaleAuthorityFence
                    }
                    RemoteFrameTransferErrorV1::SequenceGap
                    | RemoteFrameTransferErrorV1::InvalidFrame
                    | RemoteFrameTransferErrorV1::InvalidReceipt
                    | RemoteFrameTransferErrorV1::Corruption => {
                        RemoteProtocolFailureV1::ScopeMismatch
                    }
                    RemoteFrameTransferErrorV1::Overflow => RemoteProtocolFailureV1::SpoolSaturated,
                    RemoteFrameTransferErrorV1::Unavailable => {
                        RemoteProtocolFailureV1::AuthorityUnavailable
                    }
                };
                let problem = remote_protocol_problem(contract, request_id.clone(), failure)?;
                return RemoteProtocolResponseV1::new(request_id, authority, Err(problem));
            }
        };
        if receipt.validate_for(&request.body).is_err() {
            return unavailable_response(request_id, observed_at, contract);
        }
        let result =
            match frame_transfer_effect_envelope(&request, &session, receipt, contract.clone()) {
                Ok(envelope) => Ok(envelope),
                Err(failure) => Err(remote_protocol_problem(
                    contract.clone(),
                    request_id.clone(),
                    failure,
                )?),
            };
        RemoteProtocolResponseV1::new(request_id, authority, result)
    }
}

fn frame_transfer_interrupted_response(
    request_id: RequestId,
    observed_at: UtcMicros,
    contract: ResultContractRef,
    problem: ApplicationProblem,
) -> std::result::Result<
    RemoteProtocolResponseV1<RemoteFrameTransferReceiptV1>,
    ApplicationContractError,
> {
    let authority = CurrentRemoteAuthorityStateV1::Unavailable {
        reason: RemoteAuthorityUnavailableReasonV1::PlacementUnknown,
        observed_at,
    };
    let problem = ApplicationProblemEnvelope::new(contract, request_id.clone(), problem)?;
    RemoteProtocolResponseV1::new(request_id, authority, Err(problem))
}

fn frame_transfer_effect_envelope(
    request: &RemoteProtocolRequestV1<RemoteFrameTransferRequestV1>,
    session: &tracedecay_application::remote::credential_admission::RemoteAuthenticatedSessionV1,
    receipt: RemoteFrameTransferReceiptV1,
    contract: ResultContractRef,
) -> std::result::Result<ApplicationEnvelope<RemoteFrameTransferReceiptV1>, RemoteProtocolFailureV1>
{
    let enrollment = session
        .enrollment_commit_receipt()
        .ok_or(RemoteProtocolFailureV1::CallerAuthenticationFailed)?;
    let admission = &enrollment.admission;
    let actor = admission.actor().clone();
    let scope = admission.scope().clone();
    let input_digest =
        canonical_sha256(request).map_err(|_| RemoteProtocolFailureV1::AuthorityUnavailable)?;
    let expected_state = canonical_sha256(&(
        "tracedecay.remote-frame-transfer.pre.v1",
        &request.body.event_id,
        request.body.sequence.sequence,
    ))
    .map_err(|_| RemoteProtocolFailureV1::AuthorityUnavailable)?;
    let committed_state =
        canonical_sha256(&receipt).map_err(|_| RemoteProtocolFailureV1::AuthorityUnavailable)?;
    let identity = input_digest
        .as_str()
        .strip_prefix("sha256:")
        .ok_or(RemoteProtocolFailureV1::AuthorityUnavailable)?;
    let deadline = Deadline::new(UtcMicros(request.body.expires_at_micros))
        .map_err(|_| RemoteProtocolFailureV1::EnrollmentExpired)?;
    let execution = OperationReceipt::completed(
        request.sent_at,
        tracedecay_application::clock::now_micros(),
        deadline,
        OperationBudgetUsage {
            units_consumed: 1,
            bytes_consumed: request.body.ciphertext.len() as u64,
            elapsed_micros: 0,
        },
    )
    .map_err(|_| RemoteProtocolFailureV1::AuthorityUnavailable)?;
    let idempotency_key = IdempotencyKey::new(format!("remote.frame-transfer.{identity}"))
        .map_err(|_| RemoteProtocolFailureV1::AuthorityUnavailable)?;
    let effect_id = EffectId::new(format!("effect.remote.frame-transfer.{identity}"))
        .map_err(|_| RemoteProtocolFailureV1::AuthorityUnavailable)?;
    let effect_receipt = EffectReceipt {
        operation: tracedecay_tool_catalog::UseCaseId::new(REMOTE_FRAME_TRANSFER_USE_CASE_ID_V1)
            .map_err(|_| RemoteProtocolFailureV1::AuthorityUnavailable)?,
        request_id: request.request_id.clone(),
        actor,
        scope: scope.clone(),
        effect_class: tracedecay_tool_catalog::EffectClass::Administrative,
        idempotency_key: idempotency_key.clone(),
        input_digest,
        expected_state: expected_state.clone(),
        policy_digest: admission.authority().policy.digest.clone(),
        configuration_digest: admission.configuration_digest().clone(),
        catalog_digest: admission.catalog_digest().clone(),
        privacy_digest: admission.privacy_digest().clone(),
        outcome: EffectTermination::Completed,
        committed_state: Some(committed_state),
        external_proof: None,
    };
    let effect = EffectResult::new(
        effect_id,
        tracedecay_tool_catalog::EffectClass::Administrative,
        idempotency_key,
        admission.authority().clone(),
        expected_state,
        execution,
        ReconciliationState::Reconciled,
        effect_receipt,
        Some(receipt),
    )
    .map_err(|_| RemoteProtocolFailureV1::AuthorityUnavailable)?;
    Ok(ApplicationEnvelope::effect(
        contract,
        request.request_id.clone(),
        scope,
        effect,
    ))
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
                return unavailable_response(
                    request_id,
                    observed_at,
                    remote_replay_result_contract_v1(),
                );
            }
        };
        let Some(keyring) = presented_spool_keyring(&credential, request.enrollment_revision)
        else {
            return unavailable_response(
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
                    return unavailable_response(request.request_id, request.sent_at, contract);
                };
                let cancellation = match CancellationSignal::active(format!(
                    "cancel.remote.direct.{}",
                    request.request_id.as_str()
                )) {
                    Ok(cancellation) => cancellation,
                    Err(_) => {
                        return unavailable_response(request.request_id, request.sent_at, contract);
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
                        return unavailable_response(request.request_id, request.sent_at, contract);
                    }
                };
                let Some(recovery) = registered.recovery else {
                    return unavailable_response(request.request_id, request.sent_at, contract);
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
                        clock: tracedecay_application::clock::now_micros,
                        interruption: AtomicU8::new(0),
                    }),
                    tracedecay_application::clock::now_micros,
                );
                owner.execute(request, credential)
            }
        }
    };
}

#[hotpath::measure(label = "daemon.remote.router_build")]
pub(crate) fn build_daemon_remote_protocol_router(
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
    let owner = RemoteProtocolOwnerV1::new(
        Arc::new(DaemonRemoteEnrollmentProtocolPortV1 {
            credentials: Arc::clone(&credentials),
        }),
        RemoteOperationProtocolPortsV1 {
            capture: Arc::new(DaemonRemoteCaptureProtocolPortV1 {
                credentials: Arc::clone(&credentials),
            }),
            replay: Arc::new(DaemonRemoteReplayProtocolPortV1 {
                credentials: Arc::clone(&credentials),
                transaction: Arc::clone(&transaction),
            }),
            frame_transfer: Arc::new(DaemonRemoteFrameTransferProtocolPortV1 {
                credentials: Arc::clone(&credentials),
            }),
            query: Arc::new(observability::DaemonRemoteQueryProtocolPortV1::new(
                Arc::clone(&credentials),
                transaction,
                invocation,
            )),
            backup: recovery.clone(),
            restore: recovery.clone(),
            promotion: recovery,
        },
    );
    let admission = Arc::new(RemoteCredentialAdmissionServiceV1::new(
        DaemonRemoteCredentialLookupV1::new(credentials),
    ));
    Ok(tracedecay_api::remote::remote_protocol_router(
        owner,
        admission,
        tracedecay_application::clock::now_micros,
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

fn unavailable_response<T>(
    request_id: RequestId,
    observed_at: UtcMicros,
    contract: ResultContractRef,
) -> std::result::Result<RemoteProtocolResponseV1<T>, ApplicationContractError> {
    let authority = CurrentRemoteAuthorityStateV1::Unavailable {
        reason: RemoteAuthorityUnavailableReasonV1::PlacementUnknown,
        observed_at,
    };
    let problem = remote_protocol_problem(
        contract,
        request_id.clone(),
        RemoteProtocolFailureV1::AuthorityUnavailable,
    )?;
    RemoteProtocolResponseV1::new(request_id, authority, Err(problem))
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
