//! Production capture-to-store path for canonical external sources.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use thiserror::Error;
use tracedecay_application::{
    SourceCanonicalRefetchAuthorityV1, SourceCaptureAdmissionErrorV1, SourceCaptureApplicationV1,
};
use tracedecay_domain::{
    ComponentVersion, LocatorDigest, ManifestDigest, ObservationScopeV1, ProviderId,
    SourceAcquisitionCapabilitiesV1, SourceAcquisitionContractV1, SourceAggregateFrontierV1,
    SourceBindingOwnerV1, SourceBindingV1, SourceCaptureModeV1, SourceContentStateV1,
    SourceCoverageV1, SourceCursorV1, SourceDefinitionV1, SourceDeletionSemanticsV1,
    SourceEnvelopeKindV1, SourceInstanceId, SourceNativeObjectIdV1, SourceObjectObservationV1,
    SourceObjectRevisionV1, SourcePartitionFrontierV1, SourcePartitionIdV1,
    SourceProviderEnvelopeV1, SourceRefetchStrategyV1, SourceRefreshCauseV1,
    SourceRefreshReceiptV1, SourceSnapshotIdV1, SourceWholeRootStageV1, UtcMicros,
    canonical_sha256,
};
use tracedecay_store::{
    ExternalSourceReadOperationV1, ExternalSourceReadResultV1, RepositoryOperationEnvelopeV1,
    RepositoryReadOperationV1, RepositoryReadResultV1, RepositoryWritePayloadV1,
    RuntimeReadCoverageV1, RuntimeReadOperationV1, RuntimeReadResultV1, RuntimeSubmitOutcomeV1,
    SourceCommitReceiptV1, SourceCommitV1, SourceObjectMutationV1, SourceObjectTransitionV1,
    SourceObservationEvidenceV1, SourceStoreStateV1, StorageRuntimeReadPort,
};

use crate::request_identity::{LogicalEffectIdempotencyDomain, derive_logical_effect_idempotency};
use tracedecay_runtime_core::store_runtime::registry::StoreRuntimeHandle;

#[derive(Debug, Error)]
pub enum RuntimeExternalSourceErrorV1 {
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
    pub(crate) mutations: Vec<SourceObjectMutationV1>,
    pub(crate) idempotency_key: ManifestDigest,
    pub(crate) request_digest: ManifestDigest,
}

#[derive(Clone)]
pub struct RuntimeExternalSourceStore {
    runtime: StoreRuntimeHandle,
    authority: tracedecay_runtime_core::db::DatabaseAuthority,
}

impl RuntimeExternalSourceStore {
    pub fn new(
        runtime: StoreRuntimeHandle,
        authority: tracedecay_runtime_core::db::DatabaseAuthority,
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
        let requested_binding = request.binding.immutable_identity().map_err(invalid)?;
        if let Some(state) = self.read_state(requested_binding).await? {
            if state.definition() != &request.definition || state.binding() != &request.binding {
                return Err(RuntimeExternalSourceErrorV1::Invalid(
                    "external source replay authority differs from durable definition or binding"
                        .to_owned(),
                ));
            }
            if let Some(receipt) = state.receipt_by_idempotency_key(&request.idempotency_key) {
                return if receipt.request_digest() == &request.request_digest {
                    Ok(receipt.clone())
                } else {
                    Err(RuntimeExternalSourceErrorV1::IdempotencyConflict)
                };
            }
        }
        projector
            .validate()
            .map_err(|error| RuntimeExternalSourceErrorV1::Invalid(error.to_string()))?;
        let observations = request
            .mutations
            .iter()
            .map(|mutation| mutation.observation().clone())
            .collect();
        let admission = capture.capture_sanitized(
            request.definition,
            request.binding,
            request.refresh,
            request.provider_envelope,
            request.canonical_refetch,
            request.expected_frontier,
            request.next_partition,
            request.previous_whole_root_stage,
            observations,
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
            admitted_observations,
            _whole_root_stage,
            snapshot_completion,
            idempotency_key,
            request_digest,
        ) = admission.into_parts();
        if admitted_observations.len() != request.mutations.len()
            || admitted_observations
                .iter()
                .zip(&request.mutations)
                .any(|(observation, mutation)| observation != mutation.observation())
        {
            return Err(RuntimeExternalSourceErrorV1::Invalid(
                "external source mutations do not match the admitted sanitized observations"
                    .to_owned(),
            ));
        }
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
            request.mutations,
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

    pub(crate) async fn capture_host_observation(
        &self,
        receipt: &tracedecay_store::ObservationCommitReceipt,
    ) -> Result<SourceCommitReceiptV1, RuntimeExternalSourceErrorV1> {
        let observation = receipt.observation();
        let provider = observation.source().provider().clone();
        let definition = host_source_definition(provider.clone())?;
        let authorization = receipt.retrieval_anchor().authorization().clone();
        let binding = host_source_binding(
            &definition,
            observation,
            authorization.privacy_domain_id.clone(),
            self.runtime.binding(),
        )?;
        let binding_identity = binding.immutable_identity().map_err(invalid)?;
        let partition = SourcePartitionIdV1::new(
            canonical_sha256(&(
                "tracedecay.host-observation.partition.v1",
                observation.source(),
                observation.scope(),
            ))
            .map_err(invalid)?,
        );
        let idempotency_key = derive_logical_effect_idempotency(
            LogicalEffectIdempotencyDomain::HostObservation,
            observation.observation_id(),
        )
        .map_err(invalid)?;
        let native_object = SourceNativeObjectIdV1::new(
            canonical_sha256(&(
                "tracedecay.host-observation.native-object.v1",
                observation.observation_id(),
            ))
            .map_err(invalid)?,
        );
        let sanitized_digest =
            ManifestDigest::new(observation.payload_reference().digest().as_str())
                .map_err(invalid)?;
        let source_observation = SourceObjectObservationV1::new(
            native_object.clone(),
            SourceObjectRevisionV1::new(
                canonical_sha256(&(
                    "tracedecay.host-observation.revision.v1",
                    observation.observation_id(),
                    observation.payload_reference(),
                ))
                .map_err(invalid)?,
            ),
            sanitized_digest.clone(),
            SourceContentStateV1::Live,
        )
        .map_err(invalid)?;
        let request_digest = canonical_sha256(&(
            "tracedecay.host-observation.request.v1",
            observation.observation_id(),
            observation.payload_reference(),
            receipt.committed_cursor(),
            receipt.retrieval_anchor(),
            receipt.projection_generation(),
        ))
        .map_err(invalid)?;
        let current = self.read_state(binding_identity.clone()).await?;
        if let Some(state) = current.as_ref()
            && let Some(existing) = state.receipt_by_idempotency_key(&idempotency_key)
        {
            if state.definition() != &definition || state.binding() != &binding {
                return Err(RuntimeExternalSourceErrorV1::Invalid(
                    "host source replay authority differs from durable definition or binding"
                        .to_owned(),
                ));
            }
            return if state.projected_objects().get(&native_object) == Some(&source_observation) {
                Ok(existing.clone())
            } else {
                Err(RuntimeExternalSourceErrorV1::IdempotencyConflict)
            };
        }

        let expected_frontier = current
            .as_ref()
            .map(|state| state.source_frontier().clone());
        let previous_partition = expected_frontier
            .as_ref()
            .and_then(|frontier| frontier.partition(&partition));
        let sequence = previous_partition.map_or(1, |frontier| frontier.sequence() + 1);
        let refresh_id = canonical_sha256(&(
            "tracedecay.host-observation.refresh.v1",
            observation.observation_id(),
        ))
        .map_err(invalid)?;
        let snapshot = SourceSnapshotIdV1::new(
            canonical_sha256(&(
                "tracedecay.host-observation.snapshot.v1",
                observation.observation_id(),
                observation.payload_reference(),
            ))
            .map_err(invalid)?,
        );
        let continuation = SourceCursorV1::new(
            canonical_sha256(&(
                "tracedecay.host-observation.continuation.v1",
                receipt.committed_cursor(),
            ))
            .map_err(invalid)?,
        );
        let refresh = SourceRefreshReceiptV1::new(
            binding_identity.clone(),
            provider.clone(),
            refresh_id.clone(),
            SourceRefreshCauseV1::Poll,
            SourceCaptureModeV1::Poll,
            SourceRefetchStrategyV1::WholeRoot,
        )
        .map_err(invalid)?;
        let envelope = SourceProviderEnvelopeV1::new(
            binding_identity.clone(),
            provider,
            refresh_id,
            SourceRefreshCauseV1::Poll,
            SourceCaptureModeV1::Poll,
            SourceRefetchStrategyV1::WholeRoot,
            SourceEnvelopeKindV1::WholeRoot,
            partition.clone(),
            1,
            None,
            Some(continuation.clone()),
            Some(snapshot.clone()),
            SourceCoverageV1::Partial,
            sanitized_digest.clone(),
        )
        .map_err(invalid)?;
        let next_partition = SourcePartitionFrontierV1::new(
            binding_identity.clone(),
            partition.clone(),
            None,
            Some(snapshot),
            Some(continuation),
            SourceCoverageV1::Partial,
            sequence,
            previous_partition.and_then(SourcePartitionFrontierV1::last_complete_snapshot),
            envelope.envelope_digest().clone(),
        )
        .map_err(invalid)?;
        let evidence = SourceObservationEvidenceV1::new(
            binding_identity,
            partition,
            &source_observation,
            observation.receipt().receipt().clone(),
            receipt.retrieval_anchor_id().clone(),
            authorization.clone(),
            canonical_sha256(&(
                "tracedecay.host-observation.source-authorization.v1",
                &definition,
                &binding,
                &refresh,
                &envelope,
                &authorization,
            ))
            .map_err(invalid)?,
        )
        .map_err(invalid)?;
        let mutation = SourceObjectMutationV1::new(
            source_observation,
            None,
            SourceObjectTransitionV1::Initial,
            evidence,
        )
        .map_err(invalid)?;
        let capture = SourceCaptureApplicationV1::authorize(
            &definition,
            &binding,
            binding.binding_revision,
            ManifestDigest::new(authorization.access_policy_digest.as_str()).map_err(invalid)?,
            self.runtime.binding().authority_epoch.get(),
            canonical_sha256(&(
                "tracedecay.host-observation.sink-authority.v1",
                self.runtime.binding(),
            ))
            .map_err(invalid)?,
            &refresh,
            &envelope,
        )?;
        self.capture_and_commit_sanitized(
            &capture,
            RuntimeSourceCaptureRequestV1 {
                definition,
                binding,
                refresh,
                provider_envelope: envelope,
                canonical_refetch: None,
                expected_frontier,
                next_partition,
                previous_whole_root_stage: None,
                mutations: vec![mutation],
                idempotency_key,
                request_digest,
            },
            ComponentVersion::new("projector.host-observation.external-source.v1")
                .map_err(invalid)?,
        )
        .await
    }

    async fn read_receipt(
        &self,
        binding: tracedecay_domain::SourceBindingIdentityV1,
    ) -> Result<SourceCommitReceiptV1, RuntimeExternalSourceErrorV1> {
        self.read_state(binding)
            .await?
            .map(|state| state.receipt().clone())
            .ok_or(RuntimeExternalSourceErrorV1::Unavailable)
    }

    async fn read_state(
        &self,
        binding: tracedecay_domain::SourceBindingIdentityV1,
    ) -> Result<Option<SourceStoreStateV1>, RuntimeExternalSourceErrorV1> {
        let operation = ExternalSourceReadOperationV1::State { binding };
        let request = runtime_read_request(self.runtime.binding(), operation)?;
        let probe = ExternalSourceRuntimeProbe::from_control(request.control());
        let outcome = self
            .runtime
            .read(request, &probe)
            .await
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
            }) => Ok(Some(state.as_ref().clone())),
            Some(RuntimeReadResultV1::Repository {
                result:
                    RepositoryReadResultV1::ExternalSource(ExternalSourceReadResultV1::State(None)),
            }) => Ok(None),
            _ => Err(RuntimeExternalSourceErrorV1::Unavailable),
        }
    }
}

fn host_source_definition(
    provider: ProviderId,
) -> Result<SourceDefinitionV1, RuntimeExternalSourceErrorV1> {
    let capabilities = SourceAcquisitionCapabilitiesV1::new(
        [SourceCaptureModeV1::Poll].into_iter().collect(),
        [SourceRefetchStrategyV1::WholeRoot].into_iter().collect(),
        [SourceDeletionSemanticsV1::ExplicitOnly]
            .into_iter()
            .collect(),
    )
    .map_err(invalid)?;
    SourceDefinitionV1::new(
        SourceInstanceId::new(format!("source.host-observation.{}", provider.as_str()))
            .map_err(invalid)?,
        1,
        SourceAcquisitionContractV1::new(provider, capabilities).map_err(invalid)?,
        SourceCaptureModeV1::Poll,
        SourceRefetchStrategyV1::WholeRoot,
        SourceDeletionSemanticsV1::ExplicitOnly,
        1,
    )
    .map_err(invalid)
}

fn host_source_owner(
    scope: &ObservationScopeV1,
    profile_id: &tracedecay_domain::UserProfileId,
) -> SourceBindingOwnerV1 {
    match scope {
        ObservationScopeV1::Project { project_id } => {
            SourceBindingOwnerV1::Project(project_id.clone())
        }
        ObservationScopeV1::Profile => SourceBindingOwnerV1::Profile(profile_id.clone()),
    }
}

fn host_source_binding(
    definition: &SourceDefinitionV1,
    observation: &tracedecay_domain::DurableObservationV1,
    privacy_domain: tracedecay_domain::PrivacyDomainId,
    runtime: &tracedecay_store::StoreRuntimeBindingV1,
) -> Result<SourceBindingV1, RuntimeExternalSourceErrorV1> {
    let binding = SourceBindingV1::new(
        definition,
        host_source_owner(observation.scope(), &runtime.shard_id.profile_id),
        privacy_domain,
        host_native_root(observation)?,
        1,
    )
    .map_err(invalid)?;
    validate_host_source_shard(&binding, runtime)?;
    Ok(binding)
}

fn host_native_root(
    observation: &tracedecay_domain::DurableObservationV1,
) -> Result<LocatorDigest, RuntimeExternalSourceErrorV1> {
    LocatorDigest::new(
        canonical_sha256(&(
            "tracedecay.host-observation.native-root.v1",
            observation.source(),
            observation.scope(),
        ))
        .map_err(invalid)?
        .as_str(),
    )
    .map_err(invalid)
}

fn validate_host_source_shard(
    binding: &SourceBindingV1,
    runtime: &tracedecay_store::StoreRuntimeBindingV1,
) -> Result<(), RuntimeExternalSourceErrorV1> {
    let exact = match (&binding.owner, &runtime.shard_id.scope) {
        (
            SourceBindingOwnerV1::Project(project_id),
            tracedecay_store::StoreShardScopeV1::Project {
                project_id: shard_project,
            }
            | tracedecay_store::StoreShardScopeV1::ProjectSessions {
                project_id: shard_project,
            },
        ) => project_id == shard_project,
        (
            SourceBindingOwnerV1::Profile(profile_id),
            tracedecay_store::StoreShardScopeV1::Profile
            | tracedecay_store::StoreShardScopeV1::ProfileSessions,
        ) => profile_id == &runtime.shard_id.profile_id,
        _ => false,
    };
    if exact {
        Ok(())
    } else {
        Err(RuntimeExternalSourceErrorV1::Invalid(
            "host observation source authority does not match the selected store shard".to_owned(),
        ))
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
