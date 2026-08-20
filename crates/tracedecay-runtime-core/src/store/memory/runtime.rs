use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use tracedecay_domain::{FactEventId, FactOwnerV1, UtcMicros, canonical_sha256};
use tracedecay_store::{
    CommandDigestV1, ConsistencyModeV1, DurabilityClassV1, FactCommitConflict, FactCommitOutcome,
    FactCommitReceipt, FactCurrentQuery, FactLineageQuery, FactReadOperationV1, FactReadResultV1,
    FactStoreError, FactStoreResult, FactWriteBatch, FactWriteControl, IdempotencyIdentityV1,
    OperationPriorityV1, ProjectReadOperationV1, ProjectReadResultV1,
    RepositoryOperationEnvelopeV1, RepositoryReadOperationV1, RepositoryReadResultV1,
    RepositoryWritePayloadV1, RuntimeBatchCompatibilityV1, RuntimeCancellationIdV1,
    RuntimeCancellationIdentityV1, RuntimeDeadlineIdV1, RuntimeDeadlineV1, RuntimeInterruptionV1,
    RuntimeReadCoverageV1, RuntimeReadOperationV1, RuntimeReadRequestV1, RuntimeReadResultV1,
    RuntimeRequestControlV1, RuntimeRequestProbeV1, RuntimeSubmitOutcomeV1, RuntimeSubmitRequestV1,
    RuntimeTransactionIdV1, RuntimeTransactionScopeV1, StoreClientIdV1, StoreIdempotencyKeyV1,
    StoreOperationIdV1, StoreOperationMetadataV1, StoreRuntimeBindingV1, StoreShardScopeV1,
    StoredFactV1, VerifiedStoreLocatorV1,
};

use super::Database;
use crate::db::DatabaseRuntimeClientLeaseV1;

const COMMIT_OPERATION: &str = "commit fact through storage runtime";
const CURRENT_OPERATION: &str = "query current fact through storage runtime";
const LINEAGE_OPERATION: &str = "query fact lineage through storage runtime";

pub(super) fn retained_fact_runtime(
    db: &Database,
) -> FactStoreResult<Option<DatabaseRuntimeClientLeaseV1>> {
    let runtime = db.runtime_client_lease();
    if !fact_capable_scope(&runtime.binding().shard_id.scope) {
        return Ok(None);
    }
    validate_mount(db, &runtime)?;
    Ok(Some(runtime))
}

fn validate_mount(db: &Database, runtime: &DatabaseRuntimeClientLeaseV1) -> FactStoreResult<()> {
    let current_file_identity = crate::db::sqlite_generation_identity(db.canonical_database_path())
        .map_err(|error| {
            runtime_error(
                "mount fact storage runtime",
                format!("could not verify SQLite file identity: {error:?}"),
            )
        })?;
    validate_mount_parts(
        db.canonical_database_path(),
        db.opened_file_identity(),
        runtime.binding(),
        runtime.verified_locator(),
        runtime.canonical_path(),
        runtime.opened_file_identity(),
        current_file_identity,
    )
}

fn validate_mount_parts(
    database_path: &std::path::Path,
    database_opened_file_identity: u64,
    binding: &StoreRuntimeBindingV1,
    locator: &VerifiedStoreLocatorV1,
    runtime_path: &std::path::Path,
    runtime_opened_file_identity: Option<u64>,
    current_file_identity: u64,
) -> FactStoreResult<()> {
    if locator.shard_id != binding.shard_id
        || locator.incarnation != binding.incarnation
        || runtime_path != database_path
    {
        return Err(runtime_error(
            "mount fact storage runtime",
            "verified runtime locator does not match the held database authority",
        ));
    }
    if runtime_opened_file_identity != Some(database_opened_file_identity)
        || current_file_identity != database_opened_file_identity
    {
        return Err(runtime_error(
            "mount fact storage runtime",
            "database and runtime do not retain the same current SQLite file identity",
        ));
    }
    if !fact_capable_scope(&binding.shard_id.scope) {
        return Err(runtime_error(
            "mount fact storage runtime",
            "typed fact runtime requires a profile-memory or project shard",
        ));
    }
    Ok(())
}

fn fact_capable_scope(scope: &StoreShardScopeV1) -> bool {
    matches!(
        scope,
        StoreShardScopeV1::ProfileMemory | StoreShardScopeV1::Project { .. }
    )
}

pub(super) fn validate_owner_binding(
    binding: &StoreRuntimeBindingV1,
    owner: &FactOwnerV1,
    operation: &'static str,
) -> FactStoreResult<()> {
    let exact = match (&binding.shard_id.scope, owner) {
        (StoreShardScopeV1::ProfileMemory, FactOwnerV1::Profile) => true,
        (
            StoreShardScopeV1::Project {
                project_id: shard_project_id,
            },
            FactOwnerV1::Project {
                project_id: owner_project_id,
            },
        ) => shard_project_id == owner_project_id,
        _ => false,
    };
    if exact {
        Ok(())
    } else {
        Err(runtime_error(
            operation,
            "fact owner does not match the mounted runtime binding",
        ))
    }
}

pub(super) async fn commit_fact(
    db: &Database,
    runtime: &DatabaseRuntimeClientLeaseV1,
    batch: FactWriteBatch,
    write_control: &FactWriteControl,
) -> FactStoreResult<FactCommitOutcome> {
    validate_owner_binding(runtime.binding(), batch.owner(), COMMIT_OPERATION)?;
    let command = fact_command(&batch);
    let digest = canonical_sha256(&command)
        .map_err(|error| runtime_error(COMMIT_OPERATION, error.to_string()))?;
    let last_event_id = batch
        .events()
        .last()
        .map(tracedecay_domain::FactLineageEventV1::event_id)
        .ok_or(FactStoreError::EmptyBatch)?
        .clone();
    let idempotency_key = format!(
        "fact.{}.{}",
        batch.fact_id().as_str(),
        last_event_id.as_str()
    );
    let request = build_submit_request(
        runtime.binding(),
        RepositoryWritePayloadV1::Fact(Box::new(batch.clone())),
        &command,
        digest.as_str(),
        &idempotency_key,
    )?;
    let probe = Arc::new(RuntimeFactProbe::for_write(
        request.control(),
        write_control.clone(),
    ));
    let write_authority = db
        .write_authority()
        .map_err(|error| runtime_error(COMMIT_OPERATION, error.to_string()))?;
    let outcome = match runtime
        .dispatch_submit_authorized(request, probe, write_authority)
        .await
    {
        Ok(outcome) => outcome,
        Err(error) => {
            let actual = query_fact_current(
                runtime,
                FactCurrentQuery::new(batch.owner().clone(), batch.fact_id().clone())?,
            )
            .ok()
            .flatten()
            .map(|fact| fact.last_event_id().clone());
            if actual.as_ref() != batch.expected_last_event_id()
                && actual.as_ref() != Some(&last_event_id)
            {
                return Ok(FactCommitOutcome::Conflict(
                    FactCommitConflict::LastEventMismatch {
                        expected: batch.expected_last_event_id().cloned(),
                        actual,
                    },
                ));
            }
            return Err(runtime_error(COMMIT_OPERATION, format!("{error:?}")));
        }
    };
    let replay = match outcome {
        RuntimeSubmitOutcomeV1::Committed { .. }
        | RuntimeSubmitOutcomeV1::CommittedAfterCancellation { .. } => false,
        RuntimeSubmitOutcomeV1::ExactReplay { .. } => true,
        other => {
            return Err(runtime_error(
                COMMIT_OPERATION,
                format!("runtime rejected fact write: {other:?}"),
            ));
        }
    };
    let current = query_fact_current(
        runtime,
        FactCurrentQuery::new(batch.owner().clone(), batch.fact_id().clone())?,
    )?;
    finish_commit_outcome(&batch, last_event_id, current, replay)
}

fn finish_commit_outcome(
    batch: &FactWriteBatch,
    last_event_id: FactEventId,
    current: Option<StoredFactV1>,
    replay: bool,
) -> FactStoreResult<FactCommitOutcome> {
    if current
        .as_ref()
        .is_some_and(|current| current.last_event_id() != &last_event_id)
    {
        return Err(runtime_error(
            COMMIT_OPERATION,
            "committed fact projection does not match the submitted lineage",
        ));
    }
    let active_assertion_id = current.map(|current| current.active_assertion_id().clone());
    let receipt = FactCommitReceipt::new(
        batch.fact_id().clone(),
        batch.owner().clone(),
        batch
            .events()
            .iter()
            .map(|event| event.event_id().clone())
            .collect(),
        last_event_id,
        active_assertion_id,
    )?;
    Ok(if replay {
        FactCommitOutcome::IdempotentReplay(receipt)
    } else {
        FactCommitOutcome::Committed(receipt)
    })
}

pub(super) fn query_fact_current(
    runtime: &DatabaseRuntimeClientLeaseV1,
    query: FactCurrentQuery,
) -> FactStoreResult<Option<StoredFactV1>> {
    validate_owner_binding(runtime.binding(), query.owner(), CURRENT_OPERATION)?;
    match dispatch_fact_read(
        runtime,
        FactReadOperationV1::Current(query),
        CURRENT_OPERATION,
    )? {
        FactReadResultV1::Current(fact) => Ok(*fact),
        other @ FactReadResultV1::Lineage(_) => Err(runtime_error(
            CURRENT_OPERATION,
            format!("runtime returned the wrong fact result: {other:?}"),
        )),
    }
}

pub(super) fn query_fact_lineage(
    runtime: &DatabaseRuntimeClientLeaseV1,
    query: FactLineageQuery,
) -> FactStoreResult<Vec<tracedecay_domain::FactLineageEventV1>> {
    validate_owner_binding(runtime.binding(), query.owner(), LINEAGE_OPERATION)?;
    match dispatch_fact_read(
        runtime,
        FactReadOperationV1::Lineage(query),
        LINEAGE_OPERATION,
    )? {
        FactReadResultV1::Lineage(events) => Ok(events),
        other @ FactReadResultV1::Current(_) => Err(runtime_error(
            LINEAGE_OPERATION,
            format!("runtime returned the wrong fact result: {other:?}"),
        )),
    }
}

fn dispatch_fact_read(
    runtime: &DatabaseRuntimeClientLeaseV1,
    operation: FactReadOperationV1,
    operation_name: &'static str,
) -> FactStoreResult<FactReadResultV1> {
    let request = build_read_request(runtime.binding(), operation, operation_name)?;
    let probe = RuntimeFactProbe::for_read(request.control());
    let outcome = runtime
        .dispatch_read(request, &probe)
        .map_err(|error| runtime_error(operation_name, format!("{error:?}")))?;
    if !matches!(
        outcome.coverage(),
        RuntimeReadCoverageV1::Latest { .. } | RuntimeReadCoverageV1::Complete { .. }
    ) {
        return Err(runtime_error(
            operation_name,
            format!(
                "runtime did not provide complete fact coverage: {:?}",
                outcome.coverage()
            ),
        ));
    }
    match outcome.value() {
        Some(RuntimeReadResultV1::Repository {
            result: RepositoryReadResultV1::Project(project),
        }) => match project.as_ref() {
            ProjectReadResultV1::Fact(result) => Ok(result.clone()),
            other => Err(runtime_error(
                operation_name,
                format!("runtime returned the wrong project result: {other:?}"),
            )),
        },
        other => Err(runtime_error(
            operation_name,
            format!("runtime returned the wrong read result: {other:?}"),
        )),
    }
}

fn build_read_request(
    binding: &StoreRuntimeBindingV1,
    operation: FactReadOperationV1,
    operation_name: &'static str,
) -> FactStoreResult<RuntimeReadRequestV1> {
    let owner = match &operation {
        FactReadOperationV1::Current(query) => query.owner(),
        FactReadOperationV1::Lineage(query) => query.owner(),
    };
    validate_owner_binding(binding, owner, operation_name)?;
    let command = serde_json::to_value(&operation)
        .map_err(|error| runtime_error(operation_name, error.to_string()))?;
    let command_bytes = serde_json::to_vec(&command)
        .map_err(|error| runtime_error(operation_name, error.to_string()))?;
    let digest = canonical_sha256(&command)
        .map_err(|error| runtime_error(operation_name, error.to_string()))?;
    let suffix = digest_suffix(digest.as_str(), operation_name)?;
    RuntimeReadRequestV1::new(
        binding.clone(),
        ConsistencyModeV1::LatestAvailable,
        RuntimeReadOperationV1::Repository {
            op: RepositoryReadOperationV1::Project(ProjectReadOperationV1::Fact(operation)),
        },
        OperationPriorityV1::Foreground,
        u64::try_from(command_bytes.len())
            .unwrap_or(u64::MAX)
            .max(1),
        request_control(suffix, runtime_now(), operation_name)?,
    )
    .map_err(|error| runtime_error(operation_name, error.to_string()))
}

fn build_submit_request(
    binding: &StoreRuntimeBindingV1,
    payload: RepositoryWritePayloadV1,
    command: &serde_json::Value,
    command_digest: &str,
    idempotency_key: &str,
) -> FactStoreResult<RuntimeSubmitRequestV1> {
    let admitted_at = runtime_now();
    let suffix = digest_suffix(command_digest, COMMIT_OPERATION)?;
    let metadata = StoreOperationMetadataV1 {
        operation_id: StoreOperationIdV1::new(format!("operation.memory-fact.{suffix}"))
            .map_err(|error| runtime_error(COMMIT_OPERATION, error.to_string()))?,
        client_id: StoreClientIdV1::new("client.memory-fact")
            .map_err(|error| runtime_error(COMMIT_OPERATION, error.to_string()))?,
        shard_id: binding.shard_id.clone(),
        incarnation: binding.incarnation,
        authority_epoch: binding.authority_epoch,
        idempotency: IdempotencyIdentityV1 {
            key: StoreIdempotencyKeyV1::new(idempotency_key)
                .map_err(|error| runtime_error(COMMIT_OPERATION, error.to_string()))?,
            command_digest: CommandDigestV1::new(command_digest)
                .map_err(|error| runtime_error(COMMIT_OPERATION, error.to_string()))?,
        },
        durability: DurabilityClassV1::Full,
        priority: OperationPriorityV1::Foreground,
        admission_bytes: u64::try_from(
            serde_json::to_vec(command)
                .map_err(|error| runtime_error(COMMIT_OPERATION, error.to_string()))?
                .len(),
        )
        .unwrap_or(u64::MAX)
        .max(1),
        admitted_at,
    };
    let batch_compatibility = RuntimeBatchCompatibilityV1::from_operation(&metadata)
        .map_err(|error| runtime_error(COMMIT_OPERATION, error.to_string()))?;
    let transaction_scope = RuntimeTransactionScopeV1 {
        transaction_id: RuntimeTransactionIdV1::new(format!(
            "transaction.{}",
            metadata.operation_id.as_str()
        ))
        .map_err(|error| runtime_error(COMMIT_OPERATION, error.to_string()))?,
        compatibility: batch_compatibility,
        opened_at: admitted_at,
    };
    RuntimeSubmitRequestV1::new(
        RepositoryOperationEnvelopeV1 { metadata, payload },
        transaction_scope,
        request_control(suffix, admitted_at, COMMIT_OPERATION)?,
    )
    .map_err(|error| runtime_error(COMMIT_OPERATION, error.to_string()))
}

fn fact_command(batch: &FactWriteBatch) -> serde_json::Value {
    serde_json::json!({
        "kind": "fact",
        "fact_id": batch.fact_id(),
        "owner": batch.owner(),
        "identity_material": batch.identity_material(),
        "assertion": batch.assertion(),
        "events": batch.events(),
        "new_anchors": batch.new_anchors(),
        "referenced_anchor_ids": batch.referenced_anchor_ids(),
        "expected_last_event_id": batch.expected_last_event_id(),
    })
}

fn request_control(
    suffix: &str,
    requested_at: UtcMicros,
    operation: &'static str,
) -> FactStoreResult<RuntimeRequestControlV1> {
    Ok(RuntimeRequestControlV1 {
        requested_at,
        deadline: RuntimeDeadlineV1 {
            deadline_id: RuntimeDeadlineIdV1::new(format!("deadline.memory-fact.{suffix}"))
                .map_err(|error| runtime_error(operation, error.to_string()))?,
        },
        cancellation: RuntimeCancellationIdentityV1 {
            cancellation_id: RuntimeCancellationIdV1::new(format!(
                "cancellation.memory-fact.{suffix}"
            ))
            .map_err(|error| runtime_error(operation, error.to_string()))?,
            generation: 1,
        },
    })
}

fn digest_suffix<'digest>(
    digest: &'digest str,
    operation: &'static str,
) -> FactStoreResult<&'digest str> {
    digest
        .strip_prefix("sha256:")
        .ok_or_else(|| runtime_error(operation, "canonical SHA-256 digest prefix missing"))
}

fn runtime_now() -> UtcMicros {
    let micros = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros();
    UtcMicros(i64::try_from(micros).unwrap_or(i64::MAX))
}

enum RuntimeFactProbeMode {
    Read,
    Write(FactWriteControl),
}

struct RuntimeFactProbe {
    cancellation: RuntimeCancellationIdentityV1,
    deadline: RuntimeDeadlineV1,
    mode: RuntimeFactProbeMode,
}

impl RuntimeFactProbe {
    fn for_read(control: &RuntimeRequestControlV1) -> Self {
        Self {
            cancellation: control.cancellation.clone(),
            deadline: control.deadline.clone(),
            mode: RuntimeFactProbeMode::Read,
        }
    }

    fn for_write(control: &RuntimeRequestControlV1, write_control: FactWriteControl) -> Self {
        Self {
            cancellation: control.cancellation.clone(),
            deadline: control.deadline.clone(),
            mode: RuntimeFactProbeMode::Write(write_control),
        }
    }
}

impl RuntimeRequestProbeV1 for RuntimeFactProbe {
    fn cancellation_identity(&self) -> &RuntimeCancellationIdentityV1 {
        &self.cancellation
    }

    fn deadline_identity(&self) -> &RuntimeDeadlineV1 {
        &self.deadline
    }

    fn interruption(&self) -> Option<RuntimeInterruptionV1> {
        match &self.mode {
            RuntimeFactProbeMode::Read => None,
            RuntimeFactProbeMode::Write(write_control) => write_control
                .interrupted()
                .then_some(RuntimeInterruptionV1::Cancelled),
        }
    }

    fn try_begin_commit(&self) -> bool {
        match &self.mode {
            RuntimeFactProbeMode::Read => false,
            RuntimeFactProbeMode::Write(write_control) => write_control.try_begin_commit(),
        }
    }

    fn requires_isolated_commit(&self) -> bool {
        matches!(&self.mode, RuntimeFactProbeMode::Write(_))
    }
}

fn runtime_error(operation: &'static str, message: impl Into<String>) -> FactStoreError {
    FactStoreError::Storage {
        operation,
        source: Box::new(std::io::Error::other(message.into())),
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use tracedecay_domain::{
        BrainId, FactId, FactIdentityMaterialV1, FactIdentitySourceV1, FactLineageEventKindV1,
        FactLineageEventV1, LocatorDigest, PayloadAccessState, ProjectId, ProvenanceId,
        UserProfileId,
    };
    use tracedecay_store::{
        StoreAuthorityEpochV1, StoreIncarnationV1, StoreShardIdV1, VerifiedStoreLocatorV1,
    };

    use super::*;

    fn binding(project_id: &ProjectId) -> StoreRuntimeBindingV1 {
        StoreRuntimeBindingV1::new(
            StoreShardIdV1::project(
                BrainId::new("brain.fact-runtime").unwrap(),
                UserProfileId::new("profile.fact-runtime").unwrap(),
                project_id.clone(),
            ),
            StoreIncarnationV1::new(1).unwrap(),
            StoreAuthorityEpochV1::new(1).unwrap(),
        )
    }

    fn profile_binding() -> StoreRuntimeBindingV1 {
        StoreRuntimeBindingV1::new(
            StoreShardIdV1::profile_memory(
                BrainId::new("brain.fact-runtime").unwrap(),
                UserProfileId::new("profile.fact-runtime").unwrap(),
            ),
            StoreIncarnationV1::new(1).unwrap(),
            StoreAuthorityEpochV1::new(1).unwrap(),
        )
    }

    fn fact_id(owner: FactOwnerV1) -> FactId {
        FactId::derive(
            &FactIdentityMaterialV1::new(
                owner,
                FactIdentitySourceV1::Application {
                    operation_id: ProvenanceId::new("operation.fact-runtime").unwrap(),
                },
            )
            .unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn mounted_runtime_requires_exact_owner_scope() {
        let project = ProjectId::new("project.fact-runtime").unwrap();
        let other = ProjectId::new("project.other").unwrap();
        let binding = binding(&project);
        assert!(
            validate_owner_binding(
                &binding,
                &FactOwnerV1::Project {
                    project_id: project,
                },
                CURRENT_OPERATION,
            )
            .is_ok()
        );
        assert!(
            validate_owner_binding(
                &binding,
                &FactOwnerV1::Project { project_id: other },
                CURRENT_OPERATION,
            )
            .is_err()
        );
        assert!(
            validate_owner_binding(&binding, &FactOwnerV1::Profile, CURRENT_OPERATION).is_err()
        );
        assert!(
            validate_owner_binding(&profile_binding(), &FactOwnerV1::Profile, CURRENT_OPERATION)
                .is_ok()
        );
    }

    #[test]
    fn mounted_runtime_requires_exact_verified_locator() {
        let project = ProjectId::new("project.fact-runtime").unwrap();
        let binding = binding(&project);
        let locator = VerifiedStoreLocatorV1::new(
            binding.shard_id.clone(),
            binding.incarnation,
            LocatorDigest::new(format!("sha256:{}", "a".repeat(64))).unwrap(),
        );
        assert!(
            validate_mount_parts(
                Path::new("/stores/project.db"),
                7,
                &binding,
                &locator,
                Path::new("/stores/project.db"),
                Some(7),
                7,
            )
            .is_ok()
        );
        assert!(
            validate_mount_parts(
                Path::new("/stores/project.db"),
                7,
                &binding,
                &locator,
                Path::new("/stores/other.db"),
                Some(7),
                7,
            )
            .is_err()
        );

        let profile_binding = profile_binding();
        let profile_locator = VerifiedStoreLocatorV1::new(
            profile_binding.shard_id.clone(),
            profile_binding.incarnation,
            LocatorDigest::new(format!("sha256:{}", "b".repeat(64))).unwrap(),
        );
        assert!(
            validate_mount_parts(
                Path::new("/stores/profile-memory.db"),
                9,
                &profile_binding,
                &profile_locator,
                Path::new("/stores/profile-memory.db"),
                Some(9),
                9,
            )
            .is_ok()
        );
    }

    #[test]
    fn mounted_runtime_rejects_replaced_or_unidentified_database_file() {
        let project = ProjectId::new("project.fact-runtime").unwrap();
        let binding = binding(&project);
        let locator = VerifiedStoreLocatorV1::new(
            binding.shard_id.clone(),
            binding.incarnation,
            LocatorDigest::new(format!("sha256:{}", "a".repeat(64))).unwrap(),
        );
        for (runtime_opened, current) in [(Some(8), 8), (Some(7), 8), (None, 7)] {
            assert!(
                validate_mount_parts(
                    Path::new("/stores/project.db"),
                    7,
                    &binding,
                    &locator,
                    Path::new("/stores/project.db"),
                    runtime_opened,
                    current,
                )
                .is_err()
            );
        }
    }

    #[test]
    fn fact_read_request_carries_exact_binding_and_semantic_metadata() {
        let project = ProjectId::new("project.fact-runtime").unwrap();
        let binding = binding(&project);
        let owner = FactOwnerV1::Project {
            project_id: project,
        };
        let query = FactCurrentQuery::new(owner.clone(), fact_id(owner)).unwrap();
        let request = build_read_request(
            &binding,
            FactReadOperationV1::Current(query),
            CURRENT_OPERATION,
        )
        .unwrap();
        assert_eq!(request.binding(), &binding);
        assert_eq!(request.consistency(), &ConsistencyModeV1::LatestAvailable);
        assert_eq!(request.priority(), OperationPriorityV1::Foreground);
        assert!(request.admission_bytes() > 1);
        assert!(matches!(
            request.operation(),
            RuntimeReadOperationV1::Repository {
                op: RepositoryReadOperationV1::Project(ProjectReadOperationV1::Fact(
                    FactReadOperationV1::Current(_)
                ))
            }
        ));
        assert!(
            request
                .control()
                .deadline
                .deadline_id
                .as_str()
                .starts_with("deadline.memory-fact.")
        );
    }

    #[test]
    fn fact_read_request_rejects_cross_project_and_profile_owners() {
        let project = ProjectId::new("project.fact-runtime").unwrap();
        let binding = binding(&project);
        for owner in [
            FactOwnerV1::Project {
                project_id: ProjectId::new("project.other").unwrap(),
            },
            FactOwnerV1::Profile,
        ] {
            let fact_id = fact_id(owner.clone());
            let operations = [
                FactReadOperationV1::Current(
                    FactCurrentQuery::new(owner.clone(), fact_id.clone()).unwrap(),
                ),
                FactReadOperationV1::Lineage(
                    FactLineageQuery::new(owner, fact_id, None, 10).unwrap(),
                ),
            ];
            for operation in operations {
                assert!(build_read_request(&binding, operation, CURRENT_OPERATION).is_err());
            }
        }
    }

    #[test]
    fn profile_fact_read_request_uses_profile_memory_binding() {
        let binding = profile_binding();
        let owner = FactOwnerV1::Profile;
        let query = FactCurrentQuery::new(owner.clone(), fact_id(owner)).unwrap();
        let request = build_read_request(
            &binding,
            FactReadOperationV1::Current(query),
            CURRENT_OPERATION,
        )
        .unwrap();

        assert_eq!(request.binding(), &binding);
        assert!(matches!(
            request.operation(),
            RuntimeReadOperationV1::Repository {
                op: RepositoryReadOperationV1::Project(ProjectReadOperationV1::Fact(
                    FactReadOperationV1::Current(_)
                ))
            }
        ));
    }

    #[test]
    fn fact_submit_request_carries_exact_binding_and_semantic_metadata() {
        let project = ProjectId::new("project.fact-runtime").unwrap();
        let binding = binding(&project);
        let owner = FactOwnerV1::Project {
            project_id: project,
        };
        let fact_id = fact_id(owner.clone());
        let event = FactLineageEventV1::new(
            fact_id.clone(),
            owner.clone(),
            FactLineageEventKindV1::PayloadAccessChanged {
                previous: PayloadAccessState::Eligible,
                current: PayloadAccessState::Deleted,
            },
            UtcMicros(1),
            None,
        )
        .unwrap();
        let batch =
            FactWriteBatch::new(fact_id, owner, None, vec![event], vec![], vec![], None).unwrap();
        let command = fact_command(&batch);
        let digest = canonical_sha256(&command).unwrap();
        let request = build_submit_request(
            &binding,
            RepositoryWritePayloadV1::Fact(Box::new(batch)),
            &command,
            digest.as_str(),
            "fact.fixture.event.fixture",
        )
        .unwrap();

        assert_eq!(request.binding(), &binding);
        assert_eq!(request.envelope().metadata.shard_id, binding.shard_id);
        assert_eq!(
            request.envelope().metadata.client_id.as_str(),
            "client.memory-fact"
        );
        assert_eq!(
            request
                .envelope()
                .metadata
                .idempotency
                .command_digest
                .as_str(),
            digest.as_str()
        );
        assert_eq!(
            request.control().requested_at,
            request.envelope().metadata.admitted_at
        );
        assert_eq!(
            request.envelope().metadata.durability,
            DurabilityClassV1::Full
        );
        assert_eq!(
            request.envelope().metadata.priority,
            OperationPriorityV1::Foreground
        );
        assert!(request.envelope().metadata.admission_bytes > 1);
    }

    #[test]
    fn fact_write_probe_uses_caller_interruption_and_commit_admission() {
        let interrupted = Arc::new(AtomicBool::new(false));
        let commit_admitted = Arc::new(AtomicBool::new(false));
        let write_control = FactWriteControl::new(
            {
                let interrupted = Arc::clone(&interrupted);
                Arc::new(move || interrupted.load(Ordering::Acquire))
            },
            {
                let commit_admitted = Arc::clone(&commit_admitted);
                Arc::new(move || commit_admitted.load(Ordering::Acquire))
            },
        );
        let request_control = request_control("probe", UtcMicros(1), COMMIT_OPERATION).unwrap();
        let probe = RuntimeFactProbe::for_write(&request_control, write_control);

        assert!(probe.interruption().is_none());
        assert!(!probe.try_begin_commit());
        assert!(probe.requires_isolated_commit());

        commit_admitted.store(true, Ordering::Release);
        interrupted.store(true, Ordering::Release);
        assert!(matches!(
            probe.interruption(),
            Some(RuntimeInterruptionV1::Cancelled)
        ));
        assert!(probe.try_begin_commit());
    }

    #[test]
    fn committed_purge_returns_receipt_without_active_assertion() {
        let owner = FactOwnerV1::Profile;
        let fact_id = fact_id(owner.clone());
        let event = FactLineageEventV1::new(
            fact_id.clone(),
            owner.clone(),
            FactLineageEventKindV1::PayloadAccessChanged {
                previous: PayloadAccessState::Eligible,
                current: PayloadAccessState::Deleted,
            },
            UtcMicros(1),
            None,
        )
        .unwrap();
        let last_event_id = event.event_id().clone();
        let batch =
            FactWriteBatch::new(fact_id, owner, None, vec![event], vec![], vec![], None).unwrap();

        let outcome = finish_commit_outcome(&batch, last_event_id, None, false).unwrap();

        let FactCommitOutcome::Committed(receipt) = outcome else {
            panic!("expected committed purge receipt");
        };
        assert_eq!(receipt.active_assertion_id(), None);
    }
}
