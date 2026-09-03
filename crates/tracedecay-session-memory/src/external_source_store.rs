//! Production capture-to-store path for canonical external sources.

use std::sync::Arc;

use thiserror::Error;
use tracedecay_application::{
    SourceCanonicalRefetchAuthorityV1, SourceCaptureAdmissionErrorV1, SourceCaptureApplicationV1,
    try_now_micros,
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
    SourceCommitApplyOutcomeV1, SourceCommitReceiptV1, SourceCommitV1, SourceObjectMutationV1,
    SourceObjectTransitionV1, SourceObservationEvidenceV1, SourcePendingProjectionV1,
    SourceProjectionCommitV1, SourceStoreStateV1, apply_source_commit, build_source_projection,
};

use tracedecay_application::request_identity::{
    LogicalEffectIdempotencyDomain, derive_logical_effect_idempotency,
};
use tracedecay_runtime_core::db::DatabaseRuntimeClientV1;

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

const HOST_EXTERNAL_SOURCE_PROJECTOR: &str = "projector.host-observation.external-source.v1";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RuntimeSourceCaptureOutcomeV1 {
    Projected(SourceCommitReceiptV1),
    ProjectionPending(SourceCommitReceiptV1),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RuntimeProjectionReplayOutcomeV1 {
    pub projected: usize,
    pub deferred: bool,
}

pub(crate) enum RuntimeSourceCaptureAuthorityV1 {
    Poll,
}

impl RuntimeSourceCaptureAuthorityV1 {
    fn canonical_refetch(&self) -> Option<&SourceCanonicalRefetchAuthorityV1> {
        match self {
            Self::Poll => None,
        }
    }
}

pub(crate) struct RuntimeSourceCaptureRequestV1<'a> {
    pub(crate) definition: SourceDefinitionV1,
    pub(crate) binding: SourceBindingV1,
    pub(crate) refresh: SourceRefreshReceiptV1,
    pub(crate) provider_envelope: SourceProviderEnvelopeV1,
    pub(crate) authority: RuntimeSourceCaptureAuthorityV1,
    pub(crate) expected_frontier: Option<SourceAggregateFrontierV1>,
    pub(crate) next_partition: SourcePartitionFrontierV1,
    pub(crate) previous_whole_root_stage: Option<&'a SourceWholeRootStageV1>,
    pub(crate) mutations: Vec<SourceObjectMutationV1>,
    pub(crate) idempotency_key: ManifestDigest,
    pub(crate) request_digest: ManifestDigest,
}

fn prepare_sanitized_commit(
    capture: &SourceCaptureApplicationV1,
    request: RuntimeSourceCaptureRequestV1<'_>,
) -> Result<
    (tracedecay_domain::SourceBindingIdentityV1, SourceCommitV1),
    RuntimeExternalSourceErrorV1,
> {
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
        request.authority.canonical_refetch(),
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
            "external source mutations do not match the admitted sanitized observations".to_owned(),
        ));
    }
    let binding_identity = binding.immutable_identity().map_err(invalid)?;
    let commit = SourceCommitV1::new(
        definition,
        binding,
        envelope.partition().clone(),
        idempotency_key,
        request_digest,
        expected_frontier,
        next_frontier,
        request.mutations,
        snapshot_completion,
    )
    .map_err(invalid)?;
    Ok((binding_identity, commit))
}

fn host_source_authority(
    receipt: &tracedecay_store::ObservationCommitReceipt,
    runtime: &tracedecay_store::StoreRuntimeBindingV1,
) -> Result<
    (
        SourceDefinitionV1,
        SourceBindingV1,
        tracedecay_domain::SourceBindingIdentityV1,
    ),
    RuntimeExternalSourceErrorV1,
> {
    let observation = receipt.observation();
    let definition = host_source_definition(observation.source().provider().clone())?;
    let authorization = receipt.retrieval_anchor().authorization();
    let binding = host_source_binding(
        &definition,
        observation,
        authorization.privacy_domain_id.clone(),
        runtime,
    )?;
    let identity = binding.immutable_identity().map_err(invalid)?;
    Ok((definition, binding, identity))
}

fn prepare_host_source_commit(
    receipt: &tracedecay_store::ObservationCommitReceipt,
    current: Option<&SourceStoreStateV1>,
    runtime: &tracedecay_store::StoreRuntimeBindingV1,
) -> Result<
    (tracedecay_domain::SourceBindingIdentityV1, SourceCommitV1),
    RuntimeExternalSourceErrorV1,
> {
    let observation = receipt.observation();
    let provider = observation.source().provider().clone();
    let (definition, binding, binding_identity) = host_source_authority(receipt, runtime)?;
    if let Some(state) = current
        && (state.definition() != &definition || state.binding() != &binding)
    {
        return Err(RuntimeExternalSourceErrorV1::Invalid(
            "host source replay authority differs from durable definition or binding".to_owned(),
        ));
    }
    let authorization = receipt.retrieval_anchor().authorization().clone();
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
        ManifestDigest::new(observation.payload_reference().digest().as_str()).map_err(invalid)?;
    let source_observation = SourceObjectObservationV1::new(
        native_object,
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
        receipt.sanitization_receipt().receipt(),
        receipt.committed_cursor(),
        receipt.retrieval_anchor(),
        receipt.retrieval_anchor_id(),
        receipt.projection_generation(),
        &authorization,
    ))
    .map_err(invalid)?;
    let expected_frontier = current.map(|state| state.source_frontier().clone());
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
        sanitized_digest,
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
        binding_identity.clone(),
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
        runtime.authority_epoch.get(),
        canonical_sha256(&("tracedecay.host-observation.sink-authority.v1", runtime))
            .map_err(invalid)?,
        &refresh,
        &envelope,
    )?;
    prepare_sanitized_commit(
        &capture,
        RuntimeSourceCaptureRequestV1 {
            definition,
            binding,
            refresh,
            provider_envelope: envelope,
            authority: RuntimeSourceCaptureAuthorityV1::Poll,
            expected_frontier,
            next_partition,
            previous_whole_root_stage: None,
            mutations: vec![mutation],
            idempotency_key,
            request_digest,
        },
    )
}

#[derive(Clone)]
pub struct RuntimeExternalSourceStore {
    runtime: DatabaseRuntimeClientV1,
}

impl RuntimeExternalSourceStore {
    pub fn new(runtime: DatabaseRuntimeClientV1) -> Self {
        Self { runtime }
    }

    #[hotpath::skip]
    pub async fn capture_host_observations(
        &self,
        receipts: &[tracedecay_store::ObservationCommitReceipt],
    ) -> Result<Vec<RuntimeSourceCaptureOutcomeV1>, RuntimeExternalSourceErrorV1> {
        if receipts.is_empty() {
            return Ok(Vec::new());
        }
        let mut states = std::collections::BTreeMap::new();
        let mut settled = vec![None; receipts.len()];
        let mut pending_commits = Vec::new();
        for (slot, receipt) in receipts.iter().enumerate() {
            let (_, _, binding_identity) = host_source_authority(receipt, self.runtime.binding())?;
            if !states.contains_key(&binding_identity) {
                let state = self.read_state(binding_identity.clone()).await?;
                states.insert(binding_identity.clone(), state);
            }
            let current = states.get(&binding_identity).and_then(Option::as_ref);
            let (_, commit) = prepare_host_source_commit(receipt, current, self.runtime.binding())?;
            if let Some(existing) = self
                .read_receipt(binding_identity.clone(), commit.idempotency_key().clone())
                .await?
            {
                let source_observation = commit
                    .mutations()
                    .first()
                    .map(SourceObjectMutationV1::observation)
                    .ok_or_else(|| {
                        RuntimeExternalSourceErrorV1::Invalid(
                            "host source commit contains no observation mutation".to_owned(),
                        )
                    })?;
                if existing.request_digest() != commit.request_digest()
                    || current.is_none_or(|state| {
                        state
                            .observed_objects()
                            .get(source_observation.native_object())
                            != Some(source_observation)
                    })
                {
                    return Err(RuntimeExternalSourceErrorV1::IdempotencyConflict);
                }
                settled[slot] = Some((binding_identity, existing));
                continue;
            }
            match apply_source_commit(current, commit.clone()).map_err(invalid)? {
                SourceCommitApplyOutcomeV1::Committed(state) => {
                    settled[slot] = Some((binding_identity.clone(), state.receipt().clone()));
                    states.insert(binding_identity.clone(), Some(*state));
                }
                SourceCommitApplyOutcomeV1::ExactDuplicate(receipt) => {
                    settled[slot] = Some((binding_identity, *receipt));
                    continue;
                }
            }
            pending_commits.push(commit);
        }
        if !pending_commits.is_empty() {
            let request = if let [commit] = pending_commits.as_slice() {
                runtime_submit_request(
                    self.runtime.binding(),
                    RepositoryWritePayloadV1::ExternalSource(Box::new(commit.clone())),
                    commit,
                    commit.idempotency_key(),
                    tracedecay_store::OperationPriorityV1::Foreground,
                )?
            } else {
                let batch_identity = canonical_sha256(&(
                    "tracedecay.host-observation.external-source-batch.v1",
                    pending_commits
                        .iter()
                        .map(|commit| (commit.idempotency_key(), commit.request_digest()))
                        .collect::<Vec<_>>(),
                ))
                .map_err(invalid)?;
                runtime_submit_request(
                    self.runtime.binding(),
                    RepositoryWritePayloadV1::ExternalSourceBatch(
                        pending_commits.clone().into_boxed_slice(),
                    ),
                    &pending_commits,
                    &batch_identity,
                    tracedecay_store::OperationPriorityV1::Background,
                )?
            };
            let probe = Arc::new(ExternalSourceRuntimeProbe::from_control(request.control()));
            match self
                .runtime
                .dispatch_submit(request, probe)
                .await
                .map_err(|_| RuntimeExternalSourceErrorV1::Unavailable)?
            {
                RuntimeSubmitOutcomeV1::Committed { .. }
                | RuntimeSubmitOutcomeV1::CommittedAfterCancellation { .. }
                | RuntimeSubmitOutcomeV1::ExactReplay { .. } => {}
                RuntimeSubmitOutcomeV1::IdempotencyConflict { .. } => {
                    return Err(RuntimeExternalSourceErrorV1::IdempotencyConflict);
                }
                _ => return Err(RuntimeExternalSourceErrorV1::Unavailable),
            }
        }
        let mut projection_pending = std::collections::BTreeMap::new();
        for settled in settled.iter().flatten() {
            if !projection_pending.contains_key(&settled.0) {
                let pending = self
                    .read_pending_projection(Some(settled.0.clone()))
                    .await?
                    .is_some();
                projection_pending.insert(settled.0.clone(), pending);
            }
        }
        settled
            .into_iter()
            .map(|settled| {
                let (binding, receipt) =
                    settled.ok_or(RuntimeExternalSourceErrorV1::Unavailable)?;
                Ok(
                    if projection_pending.get(&binding).copied().unwrap_or(false) {
                        RuntimeSourceCaptureOutcomeV1::ProjectionPending(receipt)
                    } else {
                        RuntimeSourceCaptureOutcomeV1::Projected(receipt)
                    },
                )
            })
            .collect()
    }

    #[hotpath::skip]
    pub async fn capture_host_observation(
        &self,
        receipt: &tracedecay_store::ObservationCommitReceipt,
    ) -> Result<RuntimeSourceCaptureOutcomeV1, RuntimeExternalSourceErrorV1> {
        self.capture_host_observations(std::slice::from_ref(receipt))
            .await?
            .into_iter()
            .next()
            .ok_or(RuntimeExternalSourceErrorV1::Unavailable)
    }

    /// The daemon-owned host-admission drain invokes this bounded operation;
    /// capture never creates detached replay tasks. Restart resumes from the
    /// durable predecessor chain on the next admission drain.
    #[hotpath::skip]
    pub async fn drain_host_projection_replay(
        &self,
        max: usize,
        cancellation: &tracedecay_sessions::observation::ObservationCancellation,
    ) -> Result<RuntimeProjectionReplayOutcomeV1, RuntimeExternalSourceErrorV1> {
        self.drain_projection_replay_outcome(
            None,
            host_external_source_projector()?,
            max,
            cancellation,
        )
        .await
    }

    #[hotpath::skip]
    async fn drain_projection_replay_outcome(
        &self,
        binding: Option<tracedecay_domain::SourceBindingIdentityV1>,
        projector: ComponentVersion,
        max: usize,
        cancellation: &tracedecay_sessions::observation::ObservationCancellation,
    ) -> Result<RuntimeProjectionReplayOutcomeV1, RuntimeExternalSourceErrorV1> {
        projector
            .validate()
            .map_err(|error| RuntimeExternalSourceErrorV1::Invalid(error.to_string()))?;
        let mut projected = 0;
        let mut exhausted = false;
        while projected < max && !cancellation.is_cancelled() {
            let Some(pending) = self.read_pending_projection(binding.clone()).await? else {
                exhausted = true;
                break;
            };
            let projection =
                build_source_projection(&pending, projector.clone()).map_err(invalid)?;
            self.submit_projection(projection).await?;
            projected = projected.saturating_add(1);
        }
        let deferred = !exhausted
            && !cancellation.is_cancelled()
            && self.read_pending_projection(binding).await?.is_some();
        Ok(RuntimeProjectionReplayOutcomeV1 {
            projected,
            deferred,
        })
    }

    #[hotpath::skip]
    async fn read_receipt(
        &self,
        binding: tracedecay_domain::SourceBindingIdentityV1,
        idempotency_key: ManifestDigest,
    ) -> Result<Option<SourceCommitReceiptV1>, RuntimeExternalSourceErrorV1> {
        let operation = ExternalSourceReadOperationV1::CommitReceipt {
            binding,
            idempotency_key,
        };
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
                    RepositoryReadResultV1::ExternalSource(ExternalSourceReadResultV1::CommitReceipt(
                        receipt,
                    )),
            }) => Ok(receipt.as_ref().map(|receipt| receipt.as_ref().clone())),
            _ => Err(RuntimeExternalSourceErrorV1::Unavailable),
        }
    }

    #[hotpath::skip]
    async fn read_pending_projection(
        &self,
        binding: Option<tracedecay_domain::SourceBindingIdentityV1>,
    ) -> Result<Option<SourcePendingProjectionV1>, RuntimeExternalSourceErrorV1> {
        let operation = ExternalSourceReadOperationV1::NextPendingProjection { binding };
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
                    RepositoryReadResultV1::ExternalSource(
                        ExternalSourceReadResultV1::PendingProjection(pending),
                    ),
            }) => Ok(pending.as_ref().map(|pending| pending.as_ref().clone())),
            _ => Err(RuntimeExternalSourceErrorV1::Unavailable),
        }
    }

    #[hotpath::skip]
    async fn submit_projection(
        &self,
        projection: SourceProjectionCommitV1,
    ) -> Result<(), RuntimeExternalSourceErrorV1> {
        let payload =
            RepositoryWritePayloadV1::ExternalSourceProjection(Box::new(projection.clone()));
        let request = runtime_submit_request(
            self.runtime.binding(),
            payload,
            &projection,
            projection.receipt_digest(),
            tracedecay_store::OperationPriorityV1::Foreground,
        )?;
        let probe = Arc::new(ExternalSourceRuntimeProbe::from_control(request.control()));
        match self
            .runtime
            .dispatch_submit(request, probe)
            .await
            .map_err(|_| RuntimeExternalSourceErrorV1::Unavailable)?
        {
            RuntimeSubmitOutcomeV1::Committed { .. }
            | RuntimeSubmitOutcomeV1::CommittedAfterCancellation { .. }
            | RuntimeSubmitOutcomeV1::ExactReplay { .. } => Ok(()),
            RuntimeSubmitOutcomeV1::IdempotencyConflict { .. } => {
                Err(RuntimeExternalSourceErrorV1::IdempotencyConflict)
            }
            _ => Err(RuntimeExternalSourceErrorV1::Unavailable),
        }
    }

    #[hotpath::skip]
    pub(crate) async fn read_state(
        &self,
        binding: tracedecay_domain::SourceBindingIdentityV1,
    ) -> Result<Option<SourceStoreStateV1>, RuntimeExternalSourceErrorV1> {
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

fn runtime_submit_request<T: serde::Serialize>(
    binding: &tracedecay_store::StoreRuntimeBindingV1,
    payload: RepositoryWritePayloadV1,
    command: &T,
    idempotency_key: &ManifestDigest,
    priority: tracedecay_store::OperationPriorityV1,
) -> Result<tracedecay_store::RuntimeSubmitRequestV1, RuntimeExternalSourceErrorV1> {
    let command_digest = canonical_sha256(command).map_err(invalid)?;
    let command_suffix = digest_suffix(command_digest.as_str())?;
    let identity_suffix = digest_suffix(idempotency_key.as_str())?;
    let admitted_at = runtime_now()?;
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
        priority,
        admission_bytes: serialized_len(command)?,
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
    let requested_at = runtime_now()?;
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

fn runtime_now() -> Result<UtcMicros, RuntimeExternalSourceErrorV1> {
    try_now_micros().map_err(invalid)
}

fn serialized_len<T: serde::Serialize>(value: &T) -> Result<u64, RuntimeExternalSourceErrorV1> {
    serde_json::to_vec(value)
        .map_err(invalid)
        .and_then(|bytes| u64::try_from(bytes.len()).map_err(invalid))
        .map(|length| length.max(1))
}

struct ExternalSourceRuntimeProbe {
    cancellation: tracedecay_store::RuntimeCancellationIdentityV1,
    deadline: tracedecay_store::RuntimeDeadlineV1,
    commit_started: std::sync::atomic::AtomicBool,
}

impl ExternalSourceRuntimeProbe {
    fn from_control(control: &tracedecay_store::RuntimeRequestControlV1) -> Self {
        Self {
            cancellation: control.cancellation.clone(),
            deadline: control.deadline.clone(),
            commit_started: std::sync::atomic::AtomicBool::new(false),
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

    fn try_begin_commit(&self) -> bool {
        self.commit_started
            .compare_exchange(
                false,
                true,
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
            )
            .is_ok()
    }
}

fn invalid(error: impl std::fmt::Display) -> RuntimeExternalSourceErrorV1 {
    RuntimeExternalSourceErrorV1::Invalid(error.to_string())
}

fn host_external_source_projector() -> Result<ComponentVersion, RuntimeExternalSourceErrorV1> {
    ComponentVersion::new(HOST_EXTERNAL_SOURCE_PROJECTOR).map_err(invalid)
}
