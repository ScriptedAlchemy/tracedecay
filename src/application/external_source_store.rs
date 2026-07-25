//! Production capture-to-store path for canonical external sources.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use thiserror::Error;
use tracedecay_application::{
    SourceCanonicalRefetchAuthorityV1, SourceCaptureAdmissionErrorV1, SourceCaptureApplicationV1,
};
use tracedecay_domain::{
    ComponentVersion, ManifestDigest, SourceAggregateFrontierV1, SourceBindingV1,
    SourceDefinitionV1, SourceObjectObservationV1, SourcePartitionFrontierV1,
    SourceProviderEnvelopeV1, SourceRefreshReceiptV1, SourceWholeRootStageV1, UtcMicros,
    canonical_sha256,
};
use tracedecay_store::{
    ExternalSourceReadOperationV1, ExternalSourceReadResultV1, RepositoryOperationEnvelopeV1,
    RepositoryReadOperationV1, RepositoryReadResultV1, RepositoryWritePayloadV1,
    RuntimeReadCoverageV1, RuntimeReadOperationV1, RuntimeReadResultV1, RuntimeSubmitOutcomeV1,
    SourceCommitReceiptV1, SourceCommitV1,
};

use crate::daemon::store_runtime::registry::StoreRuntimeHandle;

#[derive(Debug, Error)]
pub(crate) enum RuntimeExternalSourceErrorV1 {
    #[error("external source admission failed: {0}")]
    Admission(#[from] SourceCaptureAdmissionErrorV1),
    #[error("external source commit is invalid: {0}")]
    Invalid(String),
    #[error("external source runtime is unavailable")]
    Unavailable,
    #[error("external source idempotency key conflicts with a prior command")]
    IdempotencyConflict,
}

pub(crate) struct RuntimeSourceCaptureRequestV1<'a> {
    pub(crate) definition: SourceDefinitionV1,
    pub(crate) binding: SourceBindingV1,
    pub(crate) refresh: SourceRefreshReceiptV1,
    pub(crate) provider_envelope: SourceProviderEnvelopeV1,
    pub(crate) canonical_refetch: Option<&'a SourceCanonicalRefetchAuthorityV1>,
    pub(crate) expected_frontier: Option<SourceAggregateFrontierV1>,
    pub(crate) next_partition: SourcePartitionFrontierV1,
    pub(crate) previous_whole_root_stage: Option<&'a SourceWholeRootStageV1>,
    pub(crate) observations: Vec<SourceObjectObservationV1>,
    pub(crate) idempotency_key: ManifestDigest,
    pub(crate) request_digest: ManifestDigest,
}

#[derive(Clone)]
pub(crate) struct RuntimeExternalSourceStore {
    runtime: StoreRuntimeHandle,
    authority: crate::db::DatabaseAuthority,
}

impl RuntimeExternalSourceStore {
    pub(crate) fn new(
        runtime: StoreRuntimeHandle,
        authority: crate::db::DatabaseAuthority,
    ) -> Result<Self, RuntimeExternalSourceErrorV1> {
        if authority.canonical_database_path() != runtime.locator().path() {
            return Err(RuntimeExternalSourceErrorV1::Invalid(
                "external source authority is not attached to the selected runtime".to_owned(),
            ));
        }
        Ok(Self { runtime, authority })
    }

    /// The production consumer of `SourceCaptureApplicationV1::capture_sanitized`.
    ///
    /// Admission and the store reducer remain separate owners, but this method
    /// makes the handoff atomic at the daemon's canonical writer boundary.
    pub(crate) async fn capture_and_commit_sanitized(
        &self,
        capture: &SourceCaptureApplicationV1,
        request: RuntimeSourceCaptureRequestV1<'_>,
        projector: ComponentVersion,
    ) -> Result<SourceCommitReceiptV1, RuntimeExternalSourceErrorV1> {
        projector
            .validate()
            .map_err(|error| RuntimeExternalSourceErrorV1::Invalid(error.to_string()))?;
        let admission = capture.capture_sanitized(
            request.definition,
            request.binding,
            request.refresh,
            request.provider_envelope,
            request.canonical_refetch,
            request.expected_frontier,
            request.next_partition,
            request.previous_whole_root_stage,
            request.observations,
            request.idempotency_key,
            request.request_digest,
        )?;
        let (
            definition,
            binding,
            _refresh,
            envelope,
            expected_frontier,
            next_frontier,
            observations,
            _whole_root_stage,
            snapshot_completion,
            idempotency_key,
            request_digest,
        ) = admission.into_parts();
        let binding_identity = binding.immutable_identity().map_err(invalid)?;
        let commit = SourceCommitV1::new(
            definition,
            binding,
            envelope.partition().clone(),
            projector,
            idempotency_key,
            request_digest,
            expected_frontier,
            next_frontier,
            observations,
            snapshot_completion,
        )
        .map_err(invalid)?;
        let payload = RepositoryWritePayloadV1::ExternalSource(Box::new(commit.clone()));
        let request = runtime_submit_request(self.runtime.binding(), payload, &commit)?;
        let probe = Arc::new(ExternalSourceRuntimeProbe::from_control(request.control()));
        match self
            .runtime
            .dispatch_submit_authorized(request, probe, self.authority.clone())
            .await
            .map_err(|_| RuntimeExternalSourceErrorV1::Unavailable)?
        {
            RuntimeSubmitOutcomeV1::Committed { .. }
            | RuntimeSubmitOutcomeV1::CommittedAfterCancellation { .. }
            | RuntimeSubmitOutcomeV1::ExactReplay { .. } => {
                self.read_receipt(binding_identity).await
            }
            RuntimeSubmitOutcomeV1::IdempotencyConflict { .. } => {
                Err(RuntimeExternalSourceErrorV1::IdempotencyConflict)
            }
            _ => Err(RuntimeExternalSourceErrorV1::Unavailable),
        }
    }

    async fn read_receipt(
        &self,
        binding: tracedecay_domain::SourceBindingIdentityV1,
    ) -> Result<SourceCommitReceiptV1, RuntimeExternalSourceErrorV1> {
        let operation = ExternalSourceReadOperationV1::State { binding };
        let request = runtime_read_request(self.runtime.binding(), operation)?;
        let probe = ExternalSourceRuntimeProbe::from_control(request.control());
        let outcome = self
            .runtime
            .dispatch_read(request, &probe)
            .map_err(|_| RuntimeExternalSourceErrorV1::Unavailable)?;
        if !matches!(
            outcome.coverage(),
            RuntimeReadCoverageV1::Latest { .. } | RuntimeReadCoverageV1::Complete { .. }
        ) {
            return Err(RuntimeExternalSourceErrorV1::Unavailable);
        }
        match outcome.value() {
            Some(RuntimeReadResultV1::Repository {
                result:
                    RepositoryReadResultV1::ExternalSource(ExternalSourceReadResultV1::State(Some(
                        state,
                    ))),
            }) => Ok(state.receipt().clone()),
            _ => Err(RuntimeExternalSourceErrorV1::Unavailable),
        }
    }
}

fn runtime_submit_request(
    binding: &tracedecay_store::StoreRuntimeBindingV1,
    payload: RepositoryWritePayloadV1,
    commit: &SourceCommitV1,
) -> Result<tracedecay_store::RuntimeSubmitRequestV1, RuntimeExternalSourceErrorV1> {
    let command_digest = canonical_sha256(commit).map_err(invalid)?;
    let command_suffix = digest_suffix(command_digest.as_str())?;
    let identity_suffix = digest_suffix(commit.idempotency_key().as_str())?;
    let admitted_at = runtime_now();
    let metadata = tracedecay_store::StoreOperationMetadataV1 {
        operation_id: tracedecay_store::StoreOperationIdV1::new(format!(
            "operation.external-source.{command_suffix}"
        ))
        .map_err(invalid)?,
        client_id: tracedecay_store::StoreClientIdV1::new("client.external-source")
            .map_err(invalid)?,
        shard_id: binding.shard_id.clone(),
        incarnation: binding.incarnation,
        authority_epoch: binding.authority_epoch,
        idempotency: tracedecay_store::IdempotencyIdentityV1 {
            key: tracedecay_store::StoreIdempotencyKeyV1::new(format!(
                "external-source.{identity_suffix}"
            ))
            .map_err(invalid)?,
            command_digest: tracedecay_store::CommandDigestV1::new(command_digest.as_str())
                .map_err(invalid)?,
        },
        durability: tracedecay_store::DurabilityClassV1::Full,
        priority: tracedecay_store::OperationPriorityV1::Foreground,
        admission_bytes: serialized_len(commit)?,
        admitted_at,
    };
    let compatibility = tracedecay_store::RuntimeBatchCompatibilityV1::from_operation(&metadata)
        .map_err(invalid)?;
    let transaction_scope = tracedecay_store::RuntimeTransactionScopeV1 {
        transaction_id: tracedecay_store::RuntimeTransactionIdV1::new(format!(
            "transaction.{}",
            metadata.operation_id.as_str()
        ))
        .map_err(invalid)?,
        compatibility,
        opened_at: admitted_at,
    };
    tracedecay_store::RuntimeSubmitRequestV1::new(
        RepositoryOperationEnvelopeV1 { metadata, payload },
        transaction_scope,
        runtime_control(command_suffix, admitted_at)?,
    )
    .map_err(invalid)
}

fn runtime_read_request(
    binding: &tracedecay_store::StoreRuntimeBindingV1,
    operation: ExternalSourceReadOperationV1,
) -> Result<tracedecay_store::RuntimeReadRequestV1, RuntimeExternalSourceErrorV1> {
    let digest = canonical_sha256(&operation).map_err(invalid)?;
    let suffix = digest_suffix(digest.as_str())?;
    let requested_at = runtime_now();
    tracedecay_store::RuntimeReadRequestV1::new(
        binding.clone(),
        tracedecay_store::ConsistencyModeV1::LatestAvailable,
        RuntimeReadOperationV1::Repository {
            op: RepositoryReadOperationV1::ExternalSource(operation),
        },
        tracedecay_store::OperationPriorityV1::Foreground,
        1,
        runtime_control(suffix, requested_at)?,
    )
    .map_err(invalid)
}

fn runtime_control(
    suffix: &str,
    requested_at: UtcMicros,
) -> Result<tracedecay_store::RuntimeRequestControlV1, RuntimeExternalSourceErrorV1> {
    Ok(tracedecay_store::RuntimeRequestControlV1 {
        requested_at,
        deadline: tracedecay_store::RuntimeDeadlineV1 {
            deadline_id: tracedecay_store::RuntimeDeadlineIdV1::new(format!(
                "deadline.external-source.{suffix}"
            ))
            .map_err(invalid)?,
        },
        cancellation: tracedecay_store::RuntimeCancellationIdentityV1 {
            cancellation_id: tracedecay_store::RuntimeCancellationIdV1::new(format!(
                "cancellation.external-source.{suffix}"
            ))
            .map_err(invalid)?,
            generation: 1,
        },
    })
}

fn digest_suffix(digest: &str) -> Result<&str, RuntimeExternalSourceErrorV1> {
    digest.strip_prefix("sha256:").ok_or_else(|| {
        RuntimeExternalSourceErrorV1::Invalid(
            "external source runtime digest is not canonical SHA-256".to_owned(),
        )
    })
}

fn runtime_now() -> UtcMicros {
    let micros = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros();
    UtcMicros(i64::try_from(micros).unwrap_or(i64::MAX))
}

fn serialized_len<T: serde::Serialize>(value: &T) -> Result<u64, RuntimeExternalSourceErrorV1> {
    serde_json::to_vec(value)
        .map(|bytes| u64::try_from(bytes.len()).unwrap_or(u64::MAX).max(1))
        .map_err(invalid)
}

struct ExternalSourceRuntimeProbe {
    cancellation: tracedecay_store::RuntimeCancellationIdentityV1,
    deadline: tracedecay_store::RuntimeDeadlineV1,
}

impl ExternalSourceRuntimeProbe {
    fn from_control(control: &tracedecay_store::RuntimeRequestControlV1) -> Self {
        Self {
            cancellation: control.cancellation.clone(),
            deadline: control.deadline.clone(),
        }
    }
}

impl tracedecay_store::RuntimeRequestProbeV1 for ExternalSourceRuntimeProbe {
    fn cancellation_identity(&self) -> &tracedecay_store::RuntimeCancellationIdentityV1 {
        &self.cancellation
    }

    fn deadline_identity(&self) -> &tracedecay_store::RuntimeDeadlineV1 {
        &self.deadline
    }

    fn interruption(&self) -> Option<tracedecay_store::RuntimeInterruptionV1> {
        None
    }
}

fn invalid(error: impl std::fmt::Display) -> RuntimeExternalSourceErrorV1 {
    RuntimeExternalSourceErrorV1::Invalid(error.to_string())
}
