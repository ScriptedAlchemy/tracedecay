//! Daemon-owned storage-runtime adapter for retrieval-anchor authority.

#![allow(dead_code)] // production retrieval-anchor authority; mounted via RegisteredGlobalDb

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use tracedecay_domain::{
    ManifestDigest, RetrievalAnchorId, UserProfileId, UtcMicros, canonical_sha256,
};
use tracedecay_store::{
    AnchorDispositionAppendOutcomeV1, RepositoryReadOperationV1, RepositoryReadResultV1,
    RepositoryWritePayloadV1, RetrievalAnchorDerivativeV1, RetrievalAnchorDispositionRecordV1,
    RetrievalAnchorDispositionStore, RetrievalAnchorOwnerV1, RetrievalAnchorReadOperationV1,
    RetrievalAnchorReadResultV1, RetrievalAnchorStoreError, RetrievalAnchorStoreResult,
    RetrievalAnchorTombstoneV1, RuntimeReadCoverageV1, RuntimeReadOperationV1, RuntimeReadResultV1,
    RuntimeSubmitOutcomeV1, StoreRuntimeBindingV1, StoreShardScopeV1,
};

use tracedecay_runtime_core::store_runtime::registry::StoreRuntimeHandle;

#[derive(Clone)]
pub(crate) struct RuntimeRetrievalAnchorStore {
    profile_id: UserProfileId,
    runtime: StoreRuntimeHandle,
    authority: tracedecay_runtime_core::db::DatabaseAuthority,
}

impl RuntimeRetrievalAnchorStore {
    pub(crate) fn new(
        profile_id: UserProfileId,
        runtime: StoreRuntimeHandle,
        authority: tracedecay_runtime_core::db::DatabaseAuthority,
    ) -> RetrievalAnchorStoreResult<Self> {
        let binding = runtime.binding();
        if !binding_matches_profile_scope(binding, &profile_id) {
            return Err(invalid(
                "retrieval-anchor runtime identity does not match the injected profile scope",
            ));
        }
        // This attaches the write capability to the selected runtime. Project
        // identity comes only from the typed shard binding above, never a path.
        if authority.canonical_database_path() != runtime.locator().path() {
            return Err(invalid(
                "retrieval-anchor write authority is not attached to the selected runtime",
            ));
        }
        Ok(Self {
            profile_id,
            runtime,
            authority,
        })
    }

    pub(crate) fn profile_id(&self) -> &UserProfileId {
        &self.profile_id
    }

    async fn publish_derivative_runtime(
        &self,
        derivative: RetrievalAnchorDerivativeV1,
    ) -> RetrievalAnchorStoreResult<AnchorDispositionAppendOutcomeV1> {
        derivative.validate()?;
        self.validate_owner(derivative.owner())?;
        let identity = canonical_sha256(&(
            derivative.owner(),
            derivative.source_anchor_id(),
            derivative.kind(),
            derivative.derivative_id(),
        ))
        .map_err(invalid)?;
        let admission_bytes = serialized_len(&derivative)?;
        self.submit(
            RepositoryWritePayloadV1::RetrievalAnchorDerivative(Box::new(derivative.clone())),
            canonical_sha256(&derivative).map_err(invalid)?,
            identity,
            admission_bytes,
        )
        .await
    }

    pub(crate) fn resolve_anchor(
        &self,
        anchor_id: &RetrievalAnchorId,
        owner: &RetrievalAnchorOwnerV1,
    ) -> RetrievalAnchorStoreResult<Option<tracedecay_store::StoredRetrievalAnchorRecordV1>> {
        match self.read(RetrievalAnchorReadOperationV1::AnchorById {
            anchor_id: anchor_id.clone(),
            owner: owner.clone(),
        })? {
            RetrievalAnchorReadResultV1::Anchor(record) => Ok(record),
            _ => Err(RetrievalAnchorStoreError::Unavailable),
        }
    }

    fn validate_owner(&self, owner: &RetrievalAnchorOwnerV1) -> RetrievalAnchorStoreResult<()> {
        owner.validate()?;
        let matches = match (&self.runtime.binding().shard_id.scope, owner) {
            (
                StoreShardScopeV1::Project { project_id }
                | StoreShardScopeV1::ProjectSessions { project_id },
                RetrievalAnchorOwnerV1::V3(owner),
            ) => owner.profile_id() == &self.profile_id && owner.project_id() == Some(project_id),
            (
                StoreShardScopeV1::Project { project_id }
                | StoreShardScopeV1::ProjectSessions { project_id },
                RetrievalAnchorOwnerV1::V2(tracedecay_domain::FactOwnerV1::Project {
                    project_id: owner_project,
                }),
            ) => owner_project == project_id,
            (StoreShardScopeV1::ProfileSessions, RetrievalAnchorOwnerV1::V3(owner)) => {
                owner.profile_id() == &self.profile_id && owner.project_id().is_none()
            }
            (
                StoreShardScopeV1::ProfileSessions,
                RetrievalAnchorOwnerV1::V2(tracedecay_domain::FactOwnerV1::Profile),
            ) => true,
            _ => false,
        };
        if !matches {
            return Err(RetrievalAnchorStoreError::Unavailable);
        }
        Ok(())
    }

    async fn submit(
        &self,
        payload: RepositoryWritePayloadV1,
        command_digest: ManifestDigest,
        identity_digest: ManifestDigest,
        admission_bytes: u64,
    ) -> RetrievalAnchorStoreResult<AnchorDispositionAppendOutcomeV1> {
        let request = runtime_submit_request(
            self.runtime.binding(),
            payload,
            command_digest,
            identity_digest,
            admission_bytes,
        )?;
        let probe = Arc::new(RetrievalAnchorRuntimeProbe::from_control(request.control()));
        match self
            .runtime
            .dispatch_submit_authorized(request, probe, self.authority.clone())
            .await
            .map_err(|_| RetrievalAnchorStoreError::Unavailable)?
        {
            RuntimeSubmitOutcomeV1::Committed { .. }
            | RuntimeSubmitOutcomeV1::CommittedAfterCancellation { .. } => {
                Ok(AnchorDispositionAppendOutcomeV1::Appended)
            }
            RuntimeSubmitOutcomeV1::ExactReplay { .. } => {
                Ok(AnchorDispositionAppendOutcomeV1::Replayed)
            }
            RuntimeSubmitOutcomeV1::IdempotencyConflict { .. } => {
                Err(RetrievalAnchorStoreError::DispositionConflict)
            }
            _ => Err(RetrievalAnchorStoreError::Unavailable),
        }
    }

    fn read(
        &self,
        operation: RetrievalAnchorReadOperationV1,
    ) -> RetrievalAnchorStoreResult<RetrievalAnchorReadResultV1> {
        let (anchor_id, owner) = read_identity(&operation);
        self.validate_owner(owner)?;
        let expected_anchor = anchor_id.clone();
        let expected_owner = owner.clone();
        let request = runtime_read_request(self.runtime.binding(), operation)?;
        let probe = RetrievalAnchorRuntimeProbe::from_control(request.control());
        let outcome = self
            .runtime
            .dispatch_read(request, &probe)
            .map_err(|_| RetrievalAnchorStoreError::Unavailable)?;
        if !matches!(
            outcome.coverage(),
            RuntimeReadCoverageV1::Latest { .. } | RuntimeReadCoverageV1::Complete { .. }
        ) {
            return Err(RetrievalAnchorStoreError::Unavailable);
        }
        let result = match outcome.value() {
            Some(RuntimeReadResultV1::Repository {
                result: RepositoryReadResultV1::Project(project),
            }) => match project.as_ref() {
                tracedecay_store::ProjectReadResultV1::RetrievalAnchor(result) => result.clone(),
                _ => return Err(RetrievalAnchorStoreError::Unavailable),
            },
            _ => return Err(RetrievalAnchorStoreError::Unavailable),
        };
        validate_read_result(&result, &expected_anchor, &expected_owner)?;
        Ok(result)
    }
}

impl RetrievalAnchorDispositionStore for RuntimeRetrievalAnchorStore {
    async fn append_disposition(
        &self,
        record: RetrievalAnchorDispositionRecordV1,
    ) -> RetrievalAnchorStoreResult<AnchorDispositionAppendOutcomeV1> {
        record.validate()?;
        self.validate_owner(record.owner())?;
        let identity =
            canonical_sha256(&(record.owner(), record.anchor_id(), record.disposition_id()))
                .map_err(invalid)?;
        let admission_bytes = serialized_len(&record)?;
        self.submit(
            RepositoryWritePayloadV1::RetrievalAnchorDisposition(Box::new(record.clone())),
            canonical_sha256(&record).map_err(invalid)?,
            identity,
            admission_bytes,
        )
        .await
    }

    fn publish_derivative(
        &self,
        derivative: RetrievalAnchorDerivativeV1,
    ) -> impl std::future::Future<
        Output = RetrievalAnchorStoreResult<AnchorDispositionAppendOutcomeV1>,
    > + Send {
        self.publish_derivative_runtime(derivative)
    }

    fn current_disposition(
        &self,
        anchor_id: &RetrievalAnchorId,
        owner: &RetrievalAnchorOwnerV1,
    ) -> impl std::future::Future<
        Output = RetrievalAnchorStoreResult<Option<RetrievalAnchorDispositionRecordV1>>,
    > + Send {
        let anchor_id = anchor_id.clone();
        let owner = owner.clone();
        async move {
            match self
                .read(RetrievalAnchorReadOperationV1::CurrentDisposition { anchor_id, owner })?
            {
                RetrievalAnchorReadResultV1::CurrentDisposition(record) => Ok(record),
                _ => Err(RetrievalAnchorStoreError::Unavailable),
            }
        }
    }

    fn tombstone(
        &self,
        anchor_id: &RetrievalAnchorId,
        owner: &RetrievalAnchorOwnerV1,
    ) -> impl std::future::Future<
        Output = RetrievalAnchorStoreResult<Option<RetrievalAnchorTombstoneV1>>,
    > + Send {
        let anchor_id = anchor_id.clone();
        let owner = owner.clone();
        async move {
            match self.read(RetrievalAnchorReadOperationV1::Tombstone { anchor_id, owner })? {
                RetrievalAnchorReadResultV1::Tombstone(record) => Ok(record),
                _ => Err(RetrievalAnchorStoreError::Unavailable),
            }
        }
    }

    fn derivatives(
        &self,
        anchor_id: &RetrievalAnchorId,
        owner: &RetrievalAnchorOwnerV1,
    ) -> impl std::future::Future<
        Output = RetrievalAnchorStoreResult<Vec<RetrievalAnchorDerivativeV1>>,
    > + Send {
        let anchor_id = anchor_id.clone();
        let owner = owner.clone();
        async move {
            match self.read(RetrievalAnchorReadOperationV1::Derivatives { anchor_id, owner })? {
                RetrievalAnchorReadResultV1::Derivatives(records) => Ok(records),
                _ => Err(RetrievalAnchorStoreError::Unavailable),
            }
        }
    }
}

fn read_identity(
    operation: &RetrievalAnchorReadOperationV1,
) -> (&RetrievalAnchorId, &RetrievalAnchorOwnerV1) {
    match operation {
        RetrievalAnchorReadOperationV1::AnchorById { anchor_id, owner }
        | RetrievalAnchorReadOperationV1::CurrentDisposition { anchor_id, owner }
        | RetrievalAnchorReadOperationV1::Derivatives { anchor_id, owner }
        | RetrievalAnchorReadOperationV1::Tombstone { anchor_id, owner } => (anchor_id, owner),
    }
}

fn validate_read_result(
    result: &RetrievalAnchorReadResultV1,
    anchor_id: &RetrievalAnchorId,
    owner: &RetrievalAnchorOwnerV1,
) -> RetrievalAnchorStoreResult<()> {
    match result {
        RetrievalAnchorReadResultV1::Anchor(Some(record)) => {
            record.validate()?;
            if record.anchor_id() != anchor_id || record.owner() != owner.clone() {
                return Err(invalid("retrieval-anchor record read identity mismatch"));
            }
            validate_source_bindings(record, owner)?;
        }
        RetrievalAnchorReadResultV1::CurrentDisposition(Some(record)) => {
            record.validate()?;
            if record.anchor_id() != anchor_id || record.owner() != owner {
                return Err(invalid(
                    "retrieval-anchor disposition read identity mismatch",
                ));
            }
        }
        RetrievalAnchorReadResultV1::Derivatives(records) => {
            for record in records {
                record.validate()?;
                if record.source_anchor_id() != anchor_id || record.owner() != owner {
                    return Err(invalid(
                        "retrieval-anchor derivative read identity mismatch",
                    ));
                }
            }
        }
        RetrievalAnchorReadResultV1::Tombstone(Some(record)) => {
            record.validate()?;
            if record.anchor_id() != anchor_id || record.owner() != owner {
                return Err(invalid("retrieval-anchor tombstone read identity mismatch"));
            }
        }
        RetrievalAnchorReadResultV1::Anchor(None)
        | RetrievalAnchorReadResultV1::CurrentDisposition(None)
        | RetrievalAnchorReadResultV1::Tombstone(None) => {}
    }
    Ok(())
}

fn binding_matches_profile_scope(
    binding: &StoreRuntimeBindingV1,
    profile_id: &UserProfileId,
) -> bool {
    &binding.shard_id.profile_id == profile_id
        && matches!(
            binding.shard_id.scope,
            StoreShardScopeV1::Project { .. }
                | StoreShardScopeV1::ProjectSessions { .. }
                | StoreShardScopeV1::ProfileSessions
        )
}

fn validate_source_bindings(
    record: &tracedecay_store::StoredRetrievalAnchorRecordV1,
    owner: &RetrievalAnchorOwnerV1,
) -> RetrievalAnchorStoreResult<()> {
    let matches_authorized_owner = match (record, owner) {
        (
            tracedecay_store::StoredRetrievalAnchorRecordV1::V2(record),
            RetrievalAnchorOwnerV1::V2(owner),
        ) => record
            .source_anchors()
            .iter()
            .all(|source| &tracedecay_domain::FactOwnerV1::from(source.owner().clone()) == owner),
        (
            tracedecay_store::StoredRetrievalAnchorRecordV1::V3(record),
            RetrievalAnchorOwnerV1::V3(owner),
        ) => record
            .source_anchors()
            .iter()
            .all(|source| source.owner() == owner),
        _ => false,
    };
    if !matches_authorized_owner {
        return Err(invalid(
            "retrieval-anchor source binding does not match the authorized owner",
        ));
    }
    Ok(())
}

fn runtime_submit_request(
    binding: &StoreRuntimeBindingV1,
    payload: RepositoryWritePayloadV1,
    command_digest: ManifestDigest,
    identity_digest: ManifestDigest,
    admission_bytes: u64,
) -> RetrievalAnchorStoreResult<tracedecay_store::RuntimeSubmitRequestV1> {
    let command_suffix = digest_suffix(command_digest.as_str())?;
    let identity_suffix = digest_suffix(identity_digest.as_str())?;
    let admitted_at = runtime_now();
    let metadata = tracedecay_store::StoreOperationMetadataV1 {
        operation_id: tracedecay_store::StoreOperationIdV1::new(format!(
            "operation.retrieval-anchor.{command_suffix}"
        ))
        .map_err(invalid)?,
        client_id: tracedecay_store::StoreClientIdV1::new("client.retrieval-anchor")
            .map_err(invalid)?,
        shard_id: binding.shard_id.clone(),
        incarnation: binding.incarnation,
        authority_epoch: binding.authority_epoch,
        idempotency: tracedecay_store::IdempotencyIdentityV1 {
            key: tracedecay_store::StoreIdempotencyKeyV1::new(format!(
                "retrieval-anchor.{identity_suffix}"
            ))
            .map_err(invalid)?,
            command_digest: tracedecay_store::CommandDigestV1::new(command_digest.as_str())
                .map_err(invalid)?,
        },
        durability: tracedecay_store::DurabilityClassV1::Full,
        priority: tracedecay_store::OperationPriorityV1::Foreground,
        admission_bytes: admission_bytes.max(1),
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
        tracedecay_store::RepositoryOperationEnvelopeV1 { metadata, payload },
        transaction_scope,
        runtime_control(command_suffix, admitted_at)?,
    )
    .map_err(invalid)
}

fn runtime_read_request(
    binding: &StoreRuntimeBindingV1,
    operation: RetrievalAnchorReadOperationV1,
) -> RetrievalAnchorStoreResult<tracedecay_store::RuntimeReadRequestV1> {
    let admission_bytes = serialized_len(&operation)?;
    let digest = canonical_sha256(&operation).map_err(invalid)?;
    let suffix = digest_suffix(digest.as_str())?;
    let requested_at = runtime_now();
    tracedecay_store::RuntimeReadRequestV1::new(
        binding.clone(),
        tracedecay_store::ConsistencyModeV1::LatestAvailable,
        RuntimeReadOperationV1::Repository {
            op: RepositoryReadOperationV1::Project(
                tracedecay_store::ProjectReadOperationV1::RetrievalAnchor(operation),
            ),
        },
        tracedecay_store::OperationPriorityV1::Foreground,
        admission_bytes,
        runtime_control(suffix, requested_at)?,
    )
    .map_err(invalid)
}

fn runtime_control(
    suffix: &str,
    requested_at: UtcMicros,
) -> RetrievalAnchorStoreResult<tracedecay_store::RuntimeRequestControlV1> {
    Ok(tracedecay_store::RuntimeRequestControlV1 {
        requested_at,
        deadline: tracedecay_store::RuntimeDeadlineV1 {
            deadline_id: tracedecay_store::RuntimeDeadlineIdV1::new(format!(
                "deadline.retrieval-anchor.{suffix}"
            ))
            .map_err(invalid)?,
        },
        cancellation: tracedecay_store::RuntimeCancellationIdentityV1 {
            cancellation_id: tracedecay_store::RuntimeCancellationIdV1::new(format!(
                "cancellation.retrieval-anchor.{suffix}"
            ))
            .map_err(invalid)?,
            generation: 1,
        },
    })
}

fn digest_suffix(digest: &str) -> RetrievalAnchorStoreResult<&str> {
    digest
        .strip_prefix("sha256:")
        .ok_or_else(|| invalid("non-SHA-256 retrieval-anchor runtime digest"))
}

fn runtime_now() -> UtcMicros {
    let micros = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros();
    UtcMicros(i64::try_from(micros).unwrap_or(i64::MAX))
}

fn serialized_len<T: serde::Serialize>(value: &T) -> RetrievalAnchorStoreResult<u64> {
    let len = serde_json::to_vec(value).map_err(invalid)?.len();
    Ok(u64::try_from(len).unwrap_or(u64::MAX).max(1))
}

struct RetrievalAnchorRuntimeProbe {
    cancellation: tracedecay_store::RuntimeCancellationIdentityV1,
    deadline: tracedecay_store::RuntimeDeadlineV1,
}

impl RetrievalAnchorRuntimeProbe {
    fn from_control(control: &tracedecay_store::RuntimeRequestControlV1) -> Self {
        Self {
            cancellation: control.cancellation.clone(),
            deadline: control.deadline.clone(),
        }
    }
}

impl tracedecay_store::RuntimeRequestProbeV1 for RetrievalAnchorRuntimeProbe {
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

fn invalid(error: impl std::fmt::Display) -> RetrievalAnchorStoreError {
    RetrievalAnchorStoreError::InvalidData(error.to_string())
}
