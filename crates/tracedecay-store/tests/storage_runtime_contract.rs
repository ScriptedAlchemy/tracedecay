use std::fmt::Debug;

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::json;
use tracedecay_domain::{
    BrainId, LocatorDigest, ProjectId, RepositoryId, UserProfileId, UtcMicros, WorktreeId,
};
use tracedecay_store::*;

fn id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    <T as TryFrom<String>>::Error: Debug,
{
    T::try_from(value.to_owned()).expect("fixture id is canonical")
}

fn digest(byte: char) -> CommandDigestV1 {
    CommandDigestV1::new(format!("sha256:{}", byte.to_string().repeat(64))).unwrap()
}

fn locator_digest(byte: char) -> LocatorDigest {
    LocatorDigest::new(format!("sha256:{}", byte.to_string().repeat(64))).unwrap()
}

fn epoch(value: u64) -> AuthorityEpochV1 {
    AuthorityEpochV1::new(value).unwrap()
}

fn incarnation(value: u64) -> StoreIncarnationV1 {
    StoreIncarnationV1::new(value).unwrap()
}

fn project_shard(project: &str) -> StoreShardIdV1 {
    StoreShardIdV1::project(
        id::<BrainId>("brain.primary"),
        id::<UserProfileId>("profile.primary"),
        id::<ProjectId>(project),
    )
}

fn session_shard(project: &str) -> StoreShardIdV1 {
    StoreShardIdV1::project_sessions(
        id::<BrainId>("brain.primary"),
        id::<UserProfileId>("profile.primary"),
        id::<ProjectId>(project),
    )
}

fn code_worktree_shard(project: &str) -> StoreShardIdV1 {
    StoreShardIdV1::code(
        id::<BrainId>("brain.primary"),
        id::<UserProfileId>("profile.primary"),
        id::<ProjectId>(project),
        id::<RepositoryId>("repository.tracedecay"),
        CodeShardScopeV1::Worktree {
            worktree_id: id::<WorktreeId>("worktree.main"),
        },
    )
}

fn code_snapshot_shard(project: &str) -> StoreShardIdV1 {
    StoreShardIdV1::code(
        id::<BrainId>("brain.primary"),
        id::<UserProfileId>("profile.primary"),
        id::<ProjectId>(project),
        id::<RepositoryId>("repository.tracedecay"),
        CodeShardScopeV1::Snapshot {
            worktree_id: None,
            snapshot_id: StoreSnapshotIdV1::new("snapshot.fixture").unwrap(),
        },
    )
}

fn watermark(shard_id: StoreShardIdV1, sequence: u64) -> ShardWatermarkV1 {
    ShardWatermarkV1 {
        shard_id,
        incarnation: incarnation(1),
        authority_epoch: epoch(7),
        commit_sequence: CommitSequenceV1(sequence),
    }
}

fn binding(shard_id: StoreShardIdV1) -> StoreRuntimeBindingV1 {
    StoreRuntimeBindingV1::new(shard_id, incarnation(1), epoch(7))
}

fn metadata(shard_id: StoreShardIdV1, durability: DurabilityClassV1) -> StoreOperationMetadataV1 {
    StoreOperationMetadataV1 {
        operation_id: StoreOperationIdV1::new("operation.fixture").unwrap(),
        client_id: StoreClientIdV1::new("client.fixture").unwrap(),
        shard_id,
        incarnation: incarnation(1),
        authority_epoch: epoch(7),
        idempotency: IdempotencyIdentityV1 {
            key: IdempotencyKeyV1::new("command.fixture").unwrap(),
            command_digest: digest('c'),
        },
        durability,
        priority: OperationPriorityV1::Foreground,
        estimated_bytes: 128,
        admitted_at: UtcMicros(1),
    }
}

fn commit_receipt(metadata: &StoreOperationMetadataV1) -> StoreCommitReceiptV1 {
    StoreCommitReceiptV1 {
        operation_id: metadata.operation_id.clone(),
        idempotency: metadata.idempotency.clone(),
        shard_id: metadata.shard_id.clone(),
        incarnation: metadata.incarnation,
        authority_epoch: metadata.authority_epoch,
        commit_sequence: CommitSequenceV1(1),
        committed_at: UtcMicros(2),
    }
}

fn round_trip<T>(value: &T)
where
    T: Serialize + DeserializeOwned + PartialEq + Debug,
{
    let encoded = serde_json::to_vec(value).unwrap();
    let decoded: T = serde_json::from_slice(&encoded).unwrap();
    assert_eq!(&decoded, value);
}

#[test]
fn canonical_identity_is_independent_of_locators_and_alias_labels() {
    let canonical = code_worktree_shard("project.tracedecay");

    // A resolver may encounter multiple path/display-name aliases. Only its
    // verified digest changes; those aliases cannot alter canonical ownership.
    let checkout_alias =
        VerifiedStoreLocatorV1::new(canonical.clone(), incarnation(1), locator_digest('a'));
    let symlink_alias =
        VerifiedStoreLocatorV1::new(canonical.clone(), incarnation(1), locator_digest('b'));

    assert_eq!(checkout_alias.shard_id, symlink_alias.shard_id);
    assert_ne!(checkout_alias.locator_digest, symlink_alias.locator_digest);
    assert_eq!(checkout_alias.shard_id, canonical);
    assert!(!code_snapshot_shard("project.tracedecay").is_mutable());
    assert!(code_worktree_shard("project.tracedecay").is_mutable());
    assert!(serde_json::from_str::<CodeShardScopeV1>(r#"{"kind":"repository"}"#).is_err());
}

#[test]
fn identity_and_budget_validation_fail_closed() {
    assert!(StoreIncarnationV1::new(0).is_err());
    assert!(AuthorityEpochV1::new(0).is_err());
    assert!(IdempotencyKeyV1::new(" idempotency.fixture").is_err());
    assert!(CommandDigestV1::new("sha256:ABC").is_err());
    assert!(serde_json::from_str::<StoreSnapshotIdV1>("\" bad\"").is_err());
    assert!(FrozenWatermarkVectorV1::new([]).is_err());

    let invalid = AdmissionConfigV1 {
        global_queue_max_bytes: WORKSTATION_GLOBAL_QUEUE_BYTES,
        ..AdmissionConfigV1::default()
    };
    assert!(matches!(
        invalid.validate(),
        Err(StorageRuntimeContractErrorV1::LimitExceeded {
            field: "global queue bytes",
            ..
        })
    ));

    AdmissionConfigV1 {
        global_queue_max_bytes: WORKSTATION_GLOBAL_QUEUE_BYTES,
        global_queue_profile: GlobalQueueProfileV1::ExplicitWorkstation,
        ..AdmissionConfigV1::default()
    }
    .validate()
    .unwrap();

    let mut invalid_wire = serde_json::to_value(AdmissionConfigV1::default()).unwrap();
    invalid_wire["per_shard_queue"]["max_bytes"] = json!(1);
    assert!(serde_json::from_value::<AdmissionConfigV1>(invalid_wire).is_err());
}

#[test]
fn identical_idempotency_replays_and_changed_commands_conflict() {
    let committed = IdempotencyIdentityV1 {
        key: IdempotencyKeyV1::new("command.fixture").unwrap(),
        command_digest: digest('a'),
    };
    let same = committed.clone();
    let different_command = IdempotencyIdentityV1 {
        key: committed.key.clone(),
        command_digest: digest('b'),
    };
    let different_key = IdempotencyIdentityV1 {
        key: IdempotencyKeyV1::new("command.other").unwrap(),
        command_digest: committed.command_digest.clone(),
    };

    assert_eq!(committed.check_replay(&same), Ok(true));
    assert_eq!(committed.check_replay(&different_key), Ok(false));
    assert_eq!(
        committed.check_replay(&different_command),
        Err(StorageRuntimeContractErrorV1::IdempotencyConflict)
    );
}

#[test]
fn consistency_status_is_derived_from_full_fenced_watermarks() {
    let project = project_shard("project.one");
    let sessions = session_shard("project.one");
    let required_project = watermark(project.clone(), 10);
    let required_sessions = watermark(sessions.clone(), 20);
    let vector =
        FrozenWatermarkVectorV1::new([required_sessions.clone(), required_project.clone()])
            .unwrap();

    let coverage = FrozenWatermarkCoverageV1::new(
        vector.clone(),
        [
            watermark(project.clone(), 11),
            watermark(sessions.clone(), 19),
        ],
    )
    .unwrap();
    assert_eq!(
        coverage.status_for(&project),
        WatermarkCoverageStatusV1::Satisfied
    );
    assert_eq!(
        coverage.status_for(&sessions),
        WatermarkCoverageStatusV1::Stale
    );
    assert!(coverage.is_partial());
    assert!(!coverage.is_complete());

    let wrong_epoch = ShardWatermarkV1 {
        authority_epoch: epoch(8),
        commit_sequence: CommitSequenceV1(999),
        ..required_project.clone()
    };
    assert_eq!(
        FrozenWatermarkCoverageV1::new(vector.clone(), [wrong_epoch])
            .unwrap()
            .status_for(&project),
        WatermarkCoverageStatusV1::Unavailable
    );
    let wrong_incarnation = ShardWatermarkV1 {
        incarnation: incarnation(2),
        commit_sequence: CommitSequenceV1(999),
        ..required_project.clone()
    };
    assert_eq!(
        FrozenWatermarkCoverageV1::new(vector.clone(), [wrong_incarnation])
            .unwrap()
            .status_for(&project),
        WatermarkCoverageStatusV1::Unavailable
    );

    let unavailable = FrozenWatermarkCoverageV1::new(vector, []).unwrap();
    assert_eq!(
        unavailable.status_for(&project),
        WatermarkCoverageStatusV1::Unavailable
    );

    let lease = SnapshotLeaseV1 {
        lease_id: SnapshotLeaseIdV1::new("lease.fixture").unwrap(),
        snapshot_id: StoreSnapshotIdV1::new("snapshot.fixture").unwrap(),
        watermark: required_sessions,
        acquired_at: UtcMicros(50),
        expires_at: UtcMicros(100),
    };
    assert!(!lease.is_expired_at(UtcMicros(99)));
    assert!(lease.is_expired_at(UtcMicros(100)));
}

#[test]
fn frozen_coverage_uses_canonical_json_vectors_and_rejects_invalid_wire_data() {
    let project = project_shard("project.one");
    let sessions = session_shard("project.one");
    let required_project = watermark(project.clone(), 10);
    let required_sessions = watermark(sessions, 20);
    let first = FrozenWatermarkVectorV1::new([required_project.clone(), required_sessions.clone()])
        .unwrap();
    let second =
        FrozenWatermarkVectorV1::new([required_sessions, required_project.clone()]).unwrap();
    assert_eq!(
        serde_json::to_string(&first).unwrap(),
        serde_json::to_string(&second).unwrap()
    );

    let coverage =
        FrozenWatermarkCoverageV1::new(first.clone(), [required_project.clone()]).unwrap();
    let reversed_coverage = FrozenWatermarkCoverageV1::new(
        first.clone(),
        [
            watermark(session_shard("project.one"), 20),
            required_project.clone(),
        ],
    )
    .unwrap();
    let ordered_coverage = FrozenWatermarkCoverageV1::new(
        first.clone(),
        [
            required_project.clone(),
            watermark(session_shard("project.one"), 20),
        ],
    )
    .unwrap();
    assert_eq!(
        serde_json::to_string(&reversed_coverage).unwrap(),
        serde_json::to_string(&ordered_coverage).unwrap()
    );
    let wire = serde_json::to_value(&coverage).unwrap();
    assert!(wire["required"].is_array());
    assert!(wire["observed"].is_array());
    round_trip(&coverage);

    let duplicate_observed = json!({
        "required": serde_json::to_value(&first).unwrap(),
        "observed": [
            serde_json::to_value(&required_project).unwrap(),
            serde_json::to_value(&required_project).unwrap(),
        ],
    });
    assert!(serde_json::from_value::<FrozenWatermarkCoverageV1>(duplicate_observed).is_err());
}

#[test]
fn selected_admission_and_maintenance_defaults_are_exact_and_valid() {
    let defaults = AdmissionConfigV1::default();
    defaults.validate().unwrap();

    assert_eq!(defaults.per_shard_queue.max_operations, 2_048);
    assert_eq!(defaults.per_shard_queue.max_bytes, 16 * 1024 * 1024);
    assert_eq!(defaults.global_queue_max_bytes, 64 * 1024 * 1024);
    assert_eq!(
        defaults.foreground_batch,
        BatchBudgetV1 {
            max_operations: 128,
            max_bytes: 1024 * 1024,
            max_delay_ms: 2,
        }
    );
    assert_eq!(
        defaults.background_batch,
        BatchBudgetV1 {
            max_operations: 512,
            max_bytes: 4 * 1024 * 1024,
            max_delay_ms: 10,
        }
    );
    assert_eq!(defaults.wal.soft_limit_bytes, 32 * 1024 * 1024);
    assert_eq!(defaults.wal.hard_limit_bytes, 256 * 1024 * 1024);
    assert_eq!(defaults.readers.idle_burst_retire_ms, 60_000);
}

#[test]
fn operation_envelopes_enforce_scope_and_per_operation_durability() {
    let valid = RepositoryOperationEnvelopeV1 {
        metadata: metadata(project_shard("project.one"), DurabilityClassV1::Full),
        operation: RepositoryOperationV1::Project(ProjectOperationV1::CommitFacts),
    };
    valid.validate().unwrap();

    let invalid_scope = RepositoryOperationEnvelopeV1 {
        metadata: valid.metadata.clone(),
        operation: RepositoryOperationV1::Code(CodeOperationV1::IndexRepository),
    };
    assert!(matches!(
        invalid_scope.validate(),
        Err(StorageRuntimeContractErrorV1::OperationScopeMismatch { .. })
    ));

    let wrong_durability = RepositoryOperationEnvelopeV1 {
        metadata: metadata(code_worktree_shard("project.one"), DurabilityClassV1::Full),
        operation: RepositoryOperationV1::Code(CodeOperationV1::IndexRepository),
    };
    assert!(matches!(
        wrong_durability.validate(),
        Err(StorageRuntimeContractErrorV1::DurabilityMismatch { .. })
    ));

    let valid_projection = RepositoryOperationEnvelopeV1 {
        metadata: metadata(
            code_worktree_shard("project.one"),
            DurabilityClassV1::RebuildableProjection,
        ),
        operation: RepositoryOperationV1::Code(CodeOperationV1::PublishProjection),
    };
    valid_projection.validate().unwrap();

    let immutable_snapshot = RepositoryOperationEnvelopeV1 {
        metadata: metadata(
            code_snapshot_shard("project.one"),
            DurabilityClassV1::RebuildableProjection,
        ),
        operation: RepositoryOperationV1::Code(CodeOperationV1::PublishProjection),
    };
    assert!(matches!(
        immutable_snapshot.validate(),
        Err(StorageRuntimeContractErrorV1::ImmutableShard { .. })
    ));
    round_trip(&valid);
}

fn effect_identity() -> EffectIdentityV1 {
    let source = project_shard("project.one");
    let target = session_shard("project.one");
    EffectIdentityV1 {
        effect_id: EffectIdV1::new("effect.fixture").unwrap(),
        command_digest: digest('d'),
        ordering_key: EffectOrderingKeyV1::new("project.one.observations").unwrap(),
        source_watermark: watermark(source, 30),
        target_watermark: watermark(target, 40),
    }
}

#[test]
fn outbox_identity_and_acknowledgements_bind_target_history() {
    let identity = effect_identity();
    identity.validate().unwrap();
    identity.enforce_epochs(epoch(7), epoch(7)).unwrap();
    identity
        .enforce_histories(
            &ShardWatermarkV1 {
                commit_sequence: CommitSequenceV1(31),
                ..identity.source_watermark.clone()
            },
            &ShardWatermarkV1 {
                commit_sequence: CommitSequenceV1(40),
                ..identity.target_watermark.clone()
            },
        )
        .unwrap();
    assert_eq!(
        identity.enforce_epochs(epoch(8), epoch(7)),
        Err(StorageRuntimeContractErrorV1::EffectEpochMismatch { side: "source" })
    );

    let mut outbox = TransactionalOutboxEntryV1 {
        identity: identity.clone(),
        effect: RepositoryEffectV1::PublishObservation,
        state: OutboxEffectStateV1::Pending,
        acknowledgement: None,
        enqueued_at: UtcMicros(1),
        updated_at: UtcMicros(1),
    };
    outbox
        .transition(OutboxEffectStateV1::Dispatched, UtcMicros(2))
        .unwrap();
    outbox
        .transition(OutboxEffectStateV1::EffectUnknown, UtcMicros(3))
        .unwrap();
    assert_eq!(outbox.state, OutboxEffectStateV1::EffectUnknown);
    assert!(
        outbox
            .transition(OutboxEffectStateV1::Dispatched, UtcMicros(2))
            .is_err()
    );
    assert_eq!(outbox.state, OutboxEffectStateV1::EffectUnknown);
    assert!(
        outbox
            .transition(OutboxEffectStateV1::Pending, UtcMicros(4))
            .is_err()
    );

    let receipt = TransactionalInboxReceiptV1 {
        target_commit_watermark: ShardWatermarkV1 {
            commit_sequence: CommitSequenceV1(41),
            ..identity.target_watermark.clone()
        },
        identity: identity.clone(),
        disposition: InboxEffectDispositionV1::Applied,
        committed_at: UtcMicros(5),
    };
    receipt.validate().unwrap();
    assert!(
        TransactionalInboxReceiptV1 {
            target_commit_watermark: identity.target_watermark.clone(),
            ..receipt.clone()
        }
        .validate()
        .is_err()
    );
    let acknowledgement = OutboxAcknowledgementReceiptV1 {
        identity: identity.clone(),
        inbox_receipt: receipt.clone(),
        source_commit_watermark: ShardWatermarkV1 {
            commit_sequence: CommitSequenceV1(31),
            ..identity.source_watermark.clone()
        },
        acknowledged_at: UtcMicros(6),
    };
    acknowledgement.validate().unwrap();
    assert!(
        OutboxAcknowledgementReceiptV1 {
            source_commit_watermark: identity.source_watermark.clone(),
            ..acknowledgement.clone()
        }
        .validate()
        .is_err()
    );
    outbox.acknowledge(acknowledgement).unwrap();
    assert_eq!(outbox.state, OutboxEffectStateV1::Acknowledged);
    assert!(outbox.acknowledgement.is_some());

    let wrong_target_history = TransactionalInboxReceiptV1 {
        target_commit_watermark: ShardWatermarkV1 {
            incarnation: incarnation(2),
            commit_sequence: CommitSequenceV1(41),
            ..identity.target_watermark.clone()
        },
        identity,
        disposition: InboxEffectDispositionV1::Applied,
        committed_at: UtcMicros(5),
    };
    assert!(matches!(
        wrong_target_history.validate(),
        Err(StorageRuntimeContractErrorV1::EffectIncarnationMismatch { side: "target" })
    ));
    round_trip(&outbox);
    round_trip(&receipt);
}

struct CommitPort;

impl StorageRuntimeSubmitPort for CommitPort {
    fn dispatch_submit(
        &self,
        request: RuntimeSubmitRequestV1,
    ) -> StorageRuntimePortResultV1<RuntimeSubmitOutcomeV1> {
        Ok(RuntimeSubmitOutcomeV1::Committed {
            receipt: commit_receipt(&request.envelope().metadata),
        })
    }
}

struct InvalidCommitPort;

impl StorageRuntimeSubmitPort for InvalidCommitPort {
    fn dispatch_submit(
        &self,
        request: RuntimeSubmitRequestV1,
    ) -> StorageRuntimePortResultV1<RuntimeSubmitOutcomeV1> {
        let mut receipt = commit_receipt(&request.envelope().metadata);
        receipt.incarnation = incarnation(2);
        Ok(RuntimeSubmitOutcomeV1::Committed { receipt })
    }
}

struct WatermarkPort;

impl StorageRuntimeReadPort for WatermarkPort {
    fn dispatch_read(
        &self,
        request: RuntimeReadRequestV1,
    ) -> StorageRuntimePortResultV1<RuntimeReadOutcomeV1> {
        Ok(RuntimeReadOutcomeV1::CurrentWatermark {
            watermark: ShardWatermarkV1 {
                shard_id: request.binding().shard_id.clone(),
                incarnation: request.binding().incarnation,
                authority_epoch: request.binding().authority_epoch,
                commit_sequence: CommitSequenceV1(9),
            },
        })
    }
}

#[test]
fn typed_runtime_ports_validate_requests_outcomes_and_receipt_binding() {
    let envelope = RepositoryOperationEnvelopeV1 {
        metadata: metadata(project_shard("project.one"), DurabilityClassV1::Full),
        operation: RepositoryOperationV1::Project(ProjectOperationV1::CommitFacts),
    };
    let request = RuntimeSubmitRequestV1::new(envelope).unwrap();
    assert!(matches!(
        CommitPort.submit(request.clone()).unwrap(),
        RuntimeSubmitOutcomeV1::Committed { .. }
    ));
    assert!(matches!(
        InvalidCommitPort.submit(request),
        Err(StorageRuntimePortErrorV1::InvalidResponse(
            StorageRuntimeContractErrorV1::IncarnationMismatch { .. }
        ))
    ));

    let original = metadata(project_shard("project.one"), DurabilityClassV1::Full);
    let retry = StoreOperationMetadataV1 {
        operation_id: StoreOperationIdV1::new("operation.retry").unwrap(),
        ..original.clone()
    };
    let replay_request = RuntimeSubmitRequestV1::new(RepositoryOperationEnvelopeV1 {
        metadata: retry,
        operation: RepositoryOperationV1::Project(ProjectOperationV1::CommitFacts),
    })
    .unwrap();
    RuntimeSubmitOutcomeV1::Replayed {
        receipt: commit_receipt(&original),
    }
    .validate_for(&replay_request)
    .unwrap();

    let read_request = RuntimeReadRequestV1::new(
        binding(project_shard("project.one")),
        ConsistencyModeV1::LatestAvailable,
        RuntimeReadOperationV1::CurrentWatermark,
    )
    .unwrap();
    assert!(matches!(
        WatermarkPort.read(read_request).unwrap(),
        RuntimeReadOutcomeV1::CurrentWatermark { .. }
    ));

    let at_least = RuntimeReadRequestV1::new(
        binding(project_shard("project.one")),
        ConsistencyModeV1::AtLeast {
            commit_sequence: CommitSequenceV1(10),
        },
        RuntimeReadOperationV1::CurrentWatermark,
    )
    .unwrap();
    assert!(matches!(
        WatermarkPort.read(at_least),
        Err(StorageRuntimePortErrorV1::InvalidResponse(
            StorageRuntimeContractErrorV1::ReceiptBindingMismatch { .. }
        ))
    ));

    let exact = RuntimeReadRequestV1::new(
        binding(project_shard("project.one")),
        ConsistencyModeV1::ExactSnapshot {
            lease: Box::new(SnapshotLeaseV1 {
                lease_id: SnapshotLeaseIdV1::new("snapshot.exact").unwrap(),
                snapshot_id: StoreSnapshotIdV1::new("snapshot.exact").unwrap(),
                watermark: watermark(project_shard("project.one"), 8),
                acquired_at: UtcMicros(1),
                expires_at: UtcMicros(10),
            }),
        },
        RuntimeReadOperationV1::CurrentWatermark,
    )
    .unwrap();
    assert!(matches!(
        WatermarkPort.read(exact),
        Err(StorageRuntimePortErrorV1::InvalidResponse(
            StorageRuntimeContractErrorV1::ReceiptBindingMismatch { .. }
        ))
    ));

    let exact_lease = SnapshotLeaseV1 {
        lease_id: SnapshotLeaseIdV1::new("snapshot.lookup").unwrap(),
        snapshot_id: StoreSnapshotIdV1::new("snapshot.lookup").unwrap(),
        watermark: watermark(project_shard("project.one"), 8),
        acquired_at: UtcMicros(1),
        expires_at: UtcMicros(10),
    };
    let exact_lookup = RuntimeReadRequestV1::new(
        binding(project_shard("project.one")),
        ConsistencyModeV1::ExactSnapshot {
            lease: Box::new(exact_lease.clone()),
        },
        RuntimeReadOperationV1::SnapshotLease {
            lease_id: exact_lease.lease_id,
        },
    )
    .unwrap();
    assert!(
        RuntimeReadOutcomeV1::SnapshotLease { lease: None }
            .validate_for(&exact_lookup)
            .is_err()
    );

    let mut existing = metadata(project_shard("project.one"), DurabilityClassV1::Full);
    existing.idempotency.command_digest = digest('e');
    let conflict_request = RuntimeSubmitRequestV1::new(RepositoryOperationEnvelopeV1 {
        metadata: metadata(project_shard("project.one"), DurabilityClassV1::Full),
        operation: RepositoryOperationV1::Project(ProjectOperationV1::CommitFacts),
    })
    .unwrap();
    RuntimeSubmitOutcomeV1::Conflict {
        existing_receipt: commit_receipt(&existing),
    }
    .validate_for(&conflict_request)
    .unwrap();
}

#[test]
fn lifecycle_permits_and_batch_contracts_are_fenced() {
    let runtime = binding(project_shard("project.one"));
    let publication = StoreRuntimeRegistryPublicationV1 {
        publication_id: RuntimePublicationIdV1::new("publication.fixture").unwrap(),
        binding: runtime.clone(),
        published_at: UtcMicros(1),
    };
    let lease = RuntimeLeaseV1 {
        lease_id: RuntimeLeaseIdV1::new("runtime.lease").unwrap(),
        binding: runtime.clone(),
        holder: StoreClientIdV1::new("client.fixture").unwrap(),
        acquired_at: UtcMicros(1),
        expires_at: UtcMicros(10),
    };
    lease.validate().unwrap();
    let health_lease = ReaderHealthLeaseV1 {
        lease_id: ReaderHealthLeaseIdV1::new("reader.health.lease").unwrap(),
        binding: runtime.clone(),
        holder: StoreClientIdV1::new("client.fixture").unwrap(),
        lane: ReaderLaneV1::ReservedHealth,
        acquired_at: UtcMicros(2),
        expires_at: UtcMicros(9),
    };
    health_lease.validate().unwrap();
    let invalid_health = ReaderHealthLeaseV1 {
        lane: ReaderLaneV1::General,
        ..health_lease.clone()
    };
    assert!(matches!(
        invalid_health.validate(),
        Err(StorageRuntimeContractErrorV1::ReaderHealthLaneRequired)
    ));

    let transition = RuntimeMaintenanceTransitionV1 {
        transition_id: RuntimeMaintenanceTransitionIdV1::new("transition.fixture").unwrap(),
        binding: runtime.clone(),
        lease,
        from: RuntimeMaintenanceStateV1::Draining,
        to: RuntimeMaintenanceStateV1::ExclusiveMaintenance,
        requested_at: UtcMicros(3),
    };
    transition.validate().unwrap();
    let transition_before_lease = RuntimeMaintenanceTransitionV1 {
        requested_at: UtcMicros(0),
        ..transition.clone()
    };
    assert!(matches!(
        transition_before_lease.validate(),
        Err(StorageRuntimeContractErrorV1::InvalidLeaseInterval { .. })
    ));
    let invalid_transition = RuntimeMaintenanceTransitionV1 {
        to: RuntimeMaintenanceStateV1::Opening,
        ..transition
    };
    assert!(matches!(
        invalid_transition.validate(),
        Err(StorageRuntimeContractErrorV1::InvalidMaintenanceTransition { .. })
    ));
    assert!(!RuntimeMaintenanceTransitionV1::is_allowed(
        RuntimeMaintenanceStateV1::Ready,
        RuntimeMaintenanceStateV1::ExclusiveMaintenance,
    ));
    assert!(!RuntimeMaintenanceTransitionV1::is_allowed(
        RuntimeMaintenanceStateV1::Faulted,
        RuntimeMaintenanceStateV1::Opening,
    ));

    let first = metadata(project_shard("project.one"), DurabilityClassV1::Full);
    let second = StoreOperationMetadataV1 {
        operation_id: StoreOperationIdV1::new("operation.second").unwrap(),
        ..first.clone()
    };
    let compatibility = RuntimeBatchCompatibilityV1::for_batch([&first, &second]).unwrap();
    let scope = RuntimeTransactionScopeV1 {
        transaction_id: RuntimeTransactionIdV1::new("transaction.fixture").unwrap(),
        compatibility,
        opened_at: UtcMicros(3),
    };
    let permit = RuntimeOperationPermitV1 {
        permit_id: RuntimeOperationPermitIdV1::new("permit.fixture").unwrap(),
        transaction_scope: scope,
        operation_id: first.operation_id.clone(),
        issued_at: UtcMicros(3),
        expires_at: UtcMicros(4),
    };
    permit.validate_for(&first).unwrap();
    let incompatible = StoreOperationMetadataV1 {
        priority: OperationPriorityV1::Background,
        ..second
    };
    assert!(matches!(
        permit.transaction_scope.validate_operation(&incompatible),
        Err(StorageRuntimeContractErrorV1::BatchIncompatible { field: "priority" })
    ));
    round_trip(&publication);
    round_trip(&health_lease);
    round_trip(&permit);
}

#[test]
fn public_wire_dtos_round_trip_without_driver_values() {
    let consistency = ConsistencyModeV1::FrozenWatermarkVector {
        vector: FrozenWatermarkVectorV1::new([watermark(project_shard("project.one"), 12)])
            .unwrap(),
    };
    let runtime_error = StorageRuntimeErrorV1::Fenced {
        expected: epoch(8),
        actual: epoch(7),
    };
    let admission = AdmissionConfigV1::default();
    let telemetry = MaintenanceTelemetryV1 {
        shard_id: project_shard("project.one"),
        incarnation: incarnation(1),
        authority_epoch: epoch(7),
        state: RuntimeMaintenanceStateV1::Ready,
        wal_bytes: WAL_SOFT_LIMIT_BYTES,
        wal_pressure: WalPressureV1::SoftLimit,
        blocked_snapshots: 1,
        checkpoint_count: 2,
        checkpoint_busy_count: 0,
        last_checkpoint_at: Some(UtcMicros(10)),
    };
    let commit_telemetry = CommitTelemetryV1 {
        shard_id: project_shard("project.one"),
        incarnation: incarnation(1),
        authority_epoch: epoch(7),
        commit_sequence: CommitSequenceV1(1),
        priority: OperationPriorityV1::Foreground,
        durability: DurabilityClassV1::Full,
        batch_operations: 1,
        batch_bytes: 128,
        queue_wait_micros: 1,
        transaction_micros: 2,
        committed_at: UtcMicros(10),
    };
    let reader_telemetry = ReaderTelemetryV1 {
        shard_id: project_shard("project.one"),
        incarnation: incarnation(1),
        authority_epoch: epoch(7),
        general_active: 1,
        general_idle: 1,
        general_waiters: 0,
        health_active: true,
        retained_snapshots: 0,
        longest_snapshot_age_ms: 0,
        wait_micros: 0,
    };

    round_trip(&consistency);
    round_trip(&runtime_error);
    round_trip(&admission);
    round_trip(&telemetry);
    round_trip(&commit_telemetry);
    round_trip(&reader_telemetry);
}

#[test]
fn semantic_serde_boundaries_reject_scope_durability_history_and_receipt_mismatches() {
    let valid = RepositoryOperationEnvelopeV1 {
        metadata: metadata(project_shard("project.one"), DurabilityClassV1::Full),
        operation: RepositoryOperationV1::Project(ProjectOperationV1::CommitFacts),
    };
    let mut wrong_durability = serde_json::to_value(&valid).unwrap();
    wrong_durability["metadata"]["durability"] = json!("rebuildable_projection");
    assert!(serde_json::from_value::<RepositoryOperationEnvelopeV1>(wrong_durability).is_err());

    let request = RuntimeSubmitRequestV1::new(valid).unwrap();
    let mut request_scope = serde_json::to_value(&request).unwrap();
    request_scope["metadata"]["shard_id"]["scope"]["kind"] = json!("code");
    assert!(serde_json::from_value::<RuntimeSubmitRequestV1>(request_scope).is_err());

    let identity = effect_identity();
    let receipt = TransactionalInboxReceiptV1 {
        identity: identity.clone(),
        disposition: InboxEffectDispositionV1::Applied,
        target_commit_watermark: ShardWatermarkV1 {
            commit_sequence: CommitSequenceV1(41),
            ..identity.target_watermark.clone()
        },
        committed_at: UtcMicros(2),
    };
    let mut wrong_receipt = serde_json::to_value(&receipt).unwrap();
    wrong_receipt["target_commit_watermark"]["authority_epoch"] = json!(8);
    assert!(serde_json::from_value::<TransactionalInboxReceiptV1>(wrong_receipt).is_err());

    let frozen_request = RuntimeReadRequestV1::new(
        binding(project_shard("project.one")),
        ConsistencyModeV1::FrozenWatermarkVector {
            vector: FrozenWatermarkVectorV1::new([watermark(project_shard("project.one"), 1)])
                .unwrap(),
        },
        RuntimeReadOperationV1::FrozenCoverage,
    )
    .unwrap();
    let mut wrong_frozen_request = serde_json::to_value(&frozen_request).unwrap();
    wrong_frozen_request["binding"]["authority_epoch"] = json!(8);
    assert!(serde_json::from_value::<RuntimeReadRequestV1>(wrong_frozen_request).is_err());

    let health_lease = ReaderHealthLeaseV1 {
        lease_id: ReaderHealthLeaseIdV1::new("reader.health.serde").unwrap(),
        binding: binding(project_shard("project.one")),
        holder: StoreClientIdV1::new("client.fixture").unwrap(),
        lane: ReaderLaneV1::ReservedHealth,
        acquired_at: UtcMicros(1),
        expires_at: UtcMicros(2),
    };
    let mut wrong_health_lease = serde_json::to_value(&health_lease).unwrap();
    wrong_health_lease["lane"] = json!("general");
    assert!(serde_json::from_value::<ReaderHealthLeaseV1>(wrong_health_lease).is_err());

    let invalid_snapshot_lease = json!({
        "lease_id": "snapshot.lease",
        "snapshot_id": "snapshot.fixture",
        "watermark": watermark(project_shard("project.one"), 1),
        "acquired_at": 2,
        "expires_at": 2,
    });
    assert!(serde_json::from_value::<SnapshotLeaseV1>(invalid_snapshot_lease).is_err());
}

#[test]
fn runtime_contract_source_has_no_concrete_driver_or_async_runtime_dependency() {
    assert!(
        serde_json::from_value::<RepositoryOperationV1>(json!({
            "family": "sql",
            "operation": "select"
        }))
        .is_err()
    );
    let runtime_dir = format!("{}/src/runtime", env!("CARGO_MANIFEST_DIR"));
    for entry in std::fs::read_dir(runtime_dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|value| value.to_str()) != Some("rs") {
            continue;
        }
        let source = std::fs::read_to_string(&path).unwrap();
        for forbidden in ["rusqlite", "libsql", "tokio::", "std::path::", "PathBuf"] {
            assert!(
                !source.contains(forbidden),
                "{} imports forbidden runtime detail {forbidden}",
                path.display()
            );
        }
    }
}
