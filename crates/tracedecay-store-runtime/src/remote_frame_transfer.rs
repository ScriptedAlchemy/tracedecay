//! Authenticated receiving-node frame transfer and exact effect receipts.

use crate::remote_credentials::{
    DaemonRemoteCredentialAuthorityV1, DaemonRemoteCredentialLookupV1, presented_spool_keyring,
    remote_authority_unavailable_response,
};
use std::sync::Arc;
use tracedecay_contracts::remote::auth::OpaqueRemoteCredential;
use tracedecay_contracts::remote::credential_admission::{
    RemoteCredentialAdmissionPortV1, RemoteCredentialAdmissionServiceV1, RemoteCredentialClassV1,
    RemoteSessionBoundProtocolBodyV1,
};
use tracedecay_contracts::remote::protocol::{
    RemoteProtocolExecutionControlV1, RemoteProtocolFailureV1, RemoteProtocolPortV1,
    RemoteProtocolRequestV1, RemoteProtocolResponseV1, remote_protocol_problem,
};
use tracedecay_contracts::remote::transfer::{
    REMOTE_FRAME_TRANSFER_USE_CASE_ID_V1, RemoteFrameTransferErrorV1, RemoteFrameTransferPortV1,
    RemoteFrameTransferReceiptV1, RemoteFrameTransferRequestV1,
    remote_frame_transfer_result_contract_v1,
};
use tracedecay_contracts::{
    ApplicationContractError, ApplicationEnvelope, ApplicationProblem, ApplicationProblemEnvelope,
    CancellationSignal, Deadline, EffectId, EffectReceipt, EffectResult, EffectTermination,
    IdempotencyKey, OperationBudgetUsage, OperationReceipt, ReconciliationState, RequestId,
    ResultContractRef,
};
use tracedecay_domain::{
    CurrentRemoteAuthorityStateV1, RemoteAuthorityUnavailableReasonV1, UtcMicros, canonical_sha256,
};

/// Receiving side of the reconnect transfer: this port owns no source path or
/// source store. It accepts the authenticated encrypted record, verifies it
/// against the receiving node's credential-derived key, then admits it to the
/// receiving node's own spool for canonical replay.
pub struct DaemonRemoteFrameTransferProtocolPortV1 {
    credentials: Arc<DaemonRemoteCredentialAuthorityV1>,
}

impl DaemonRemoteFrameTransferProtocolPortV1 {
    pub fn new(credentials: Arc<DaemonRemoteCredentialAuthorityV1>) -> Self {
        Self { credentials }
    }
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
            Err(_) => {
                return remote_authority_unavailable_response(
                    request.request_id,
                    request.sent_at,
                    contract,
                );
            }
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
        let now = tracedecay_contracts::clock::now_micros();
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
            tracedecay_contracts::remote::credential_admission::RemoteCredentialUseV1::TransferFrame,
            tracedecay_contracts::clock::now_micros(),
        ) {
            Ok(session) => session,
            Err(_) => return remote_authority_unavailable_response(request_id, observed_at, contract),
        };
        if <RemoteFrameTransferRequestV1 as RemoteSessionBoundProtocolBodyV1>::bind_authenticated_session(
            &session, &request,
        )
        .is_err()
        {
            return remote_authority_unavailable_response(request_id, observed_at, contract);
        }
        let session = match admission
            .reauthorize_publication(&session, tracedecay_contracts::clock::now_micros())
        {
            Ok(session) => session,
            Err(_) => {
                return remote_authority_unavailable_response(request_id, observed_at, contract);
            }
        };
        if <RemoteFrameTransferRequestV1 as RemoteSessionBoundProtocolBodyV1>::bind_authenticated_session(
            &session, &request,
        )
        .is_err()
        {
            return remote_authority_unavailable_response(request_id, observed_at, contract);
        }
        let registered = match self
            .credentials
            .storage_for_presented(RemoteCredentialClassV1::Enrollment, &credential)
        {
            Ok(registered) => registered,
            Err(_) => {
                return remote_authority_unavailable_response(request_id, observed_at, contract);
            }
        };
        let Some(keyring) = presented_spool_keyring(&credential, request.enrollment_revision)
        else {
            return remote_authority_unavailable_response(request_id, observed_at, contract);
        };
        let storage = registered.storage.with_keyring(keyring);
        let authority = match storage.current_writer_authority(&request.body.writer) {
            Ok(authority) => authority,
            Err(_) => {
                return remote_authority_unavailable_response(request_id, observed_at, contract);
            }
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
        let now = tracedecay_contracts::clock::now_micros();
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
            return remote_authority_unavailable_response(request_id, observed_at, contract);
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
    session: &tracedecay_contracts::remote::credential_admission::RemoteAuthenticatedSessionV1,
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
        tracedecay_contracts::clock::now_micros(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tracedecay_domain::{BrainId, BrainNodeId, UserProfileId};

    #[test]
    fn receiving_owner_denies_interruption_and_unknown_credentials_before_commit() {
        let brain_id = BrainId::new("brain.remote").unwrap();
        let node_id = BrainNodeId::new("node.remote").unwrap();
        let credentials = Arc::new(DaemonRemoteCredentialAuthorityV1::new(
            brain_id.clone(),
            UserProfileId::new("profile.remote").unwrap(),
        ));
        let owner = DaemonRemoteFrameTransferProtocolPortV1::new(credentials);
        let now = tracedecay_contracts::clock::now_micros();
        let expires_at = UtcMicros(now.0 + 60_000_000);
        let body: RemoteFrameTransferRequestV1 = serde_json::from_value(json!({
            "event_id": "event.remote-frame-transfer",
            "enrollment_id": "enrollment.remote", "enrollment_revision": 1,
            "node_id": node_id,
            "writer": {
                "project_id": "project.remote",
                "scope": {
                    "project_id": "project.remote", "repository_id": "repository.remote",
                    "worktree_id": "worktree.remote", "reference": "refs/heads/main",
                    "snapshot_id": "snapshot.remote"
                },
                "authority": {
                    "fence": {
                        "brain_id": brain_id, "shard_id": "shard.remote",
                        "generation_id": "generation.remote", "placement_revision": 1,
                        "authority_epoch": 1, "authority_node_id": "node.authority"
                    },
                    "credential_revision": 1, "observed_at": now
                }
            },
            "policy_revision": 1,
            "sequence": {"sequence": 1, "previous_event_id": null},
            "frame_digest": format!("sha256:{}", "a".repeat(64)),
            "key_revision": 1, "nonce": [0,0,0,0,0,0,0,0,0,0,0,0],
            "ciphertext": [1,2,3], "observed_authority_epoch": 1,
            "expires_at_micros": expires_at.0
        }))
        .unwrap();
        let request = RemoteProtocolRequestV1::new(
            RequestId::new("request.remote-transfer-denied").unwrap(),
            brain_id,
            node_id,
            1,
            Some(body.writer.authority.fence.clone()),
            now,
            body,
        )
        .unwrap();
        let contract = remote_frame_transfer_result_contract_v1().unwrap();
        for (cancelled, deadline, expected) in [
            (
                true,
                expires_at,
                ApplicationProblemEnvelope::new(
                    contract.clone(),
                    request.request_id.clone(),
                    ApplicationProblem::cancelled_before_admission(),
                )
                .unwrap(),
            ),
            (
                false,
                now,
                ApplicationProblemEnvelope::new(
                    contract.clone(),
                    request.request_id.clone(),
                    ApplicationProblem::timed_out_before_admission(),
                )
                .unwrap(),
            ),
            (
                false,
                expires_at,
                remote_protocol_problem(
                    contract.clone(),
                    request.request_id.clone(),
                    RemoteProtocolFailureV1::AuthorityUnavailable,
                )
                .unwrap(),
            ),
        ] {
            let cancellation = CancellationSignal::active("cancel.remote-transfer-denied").unwrap();
            if cancelled {
                assert!(cancellation.cancel(now));
            }
            let response = owner
                .execute_controlled(
                    request.clone(),
                    OpaqueRemoteCredential::new([9_u8; 32]).unwrap(),
                    RemoteProtocolExecutionControlV1 {
                        deadline,
                        cancellation: cancellation.clone(),
                    },
                )
                .unwrap();
            assert_eq!(response.result.unwrap_err(), expected);
            assert!(!cancellation.commit_started());
            assert_eq!(
                response.authority,
                CurrentRemoteAuthorityStateV1::Unavailable {
                    reason: RemoteAuthorityUnavailableReasonV1::PlacementUnknown,
                    observed_at: now,
                }
            );
        }
    }
}
