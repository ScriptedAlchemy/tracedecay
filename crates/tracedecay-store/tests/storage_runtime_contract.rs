use std::fmt::Debug;
use std::future::Future;
use std::pin::pin;
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use std::task::{Context, Poll, Waker};

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::json;
use tracedecay_domain::{
    AuthorityEpoch, BrainId, CodeGenerationId, LocatorDigest, ProjectId, RepositoryId,
    UserProfileId, UtcMicros, WorktreeId,
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

fn epoch(value: u64) -> StoreAuthorityEpochV1 {
    StoreAuthorityEpochV1::new(value).unwrap()
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
            key: StoreIdempotencyKeyV1::new("command.fixture").unwrap(),
            command_digest: digest('c'),
        },
        durability,
        priority: OperationPriorityV1::Foreground,
        admission_bytes: 128,
        admitted_at: UtcMicros(1),
    }
}

fn control() -> RuntimeRequestControlV1 {
    RuntimeRequestControlV1 {
        requested_at: UtcMicros(1),
        deadline: RuntimeDeadlineV1 {
            deadline_id: RuntimeDeadlineIdV1::new("deadline.fixture").unwrap(),
        },
        cancellation: RuntimeCancellationIdentityV1 {
            cancellation_id: RuntimeCancellationIdV1::new("cancellation.fixture").unwrap(),
            generation: 1,
        },
    }
}

fn transaction_scope(metadata: &StoreOperationMetadataV1) -> RuntimeTransactionScopeV1 {
    RuntimeTransactionScopeV1 {
        transaction_id: RuntimeTransactionIdV1::new("transaction.submit").unwrap(),
        compatibility: RuntimeBatchCompatibilityV1::from_operation(metadata).unwrap(),
        opened_at: metadata.admitted_at,
    }
}

fn outbox_payload() -> RepositoryWritePayloadV1 {
    RepositoryWritePayloadV1::EnqueueOutbox(Box::new(TransactionalOutboxEntryV1 {
        identity: effect_identity(),
        effect: RepositoryEffectV1::PublishObservation,
        state: OutboxEffectStateV1::Pending,
        acknowledgement: None,
        enqueued_at: UtcMicros(1),
        updated_at: UtcMicros(1),
    }))
}

fn submit_request(metadata: StoreOperationMetadataV1) -> RuntimeSubmitRequestV1 {
    let scope = transaction_scope(&metadata);
    RuntimeSubmitRequestV1::new(
        RepositoryOperationEnvelopeV1 {
            metadata,
            payload: outbox_payload(),
        },
        scope,
        control(),
    )
    .unwrap()
}

fn block_on<F: Future>(future: F) -> F::Output {
    let waker = Waker::noop();
    let mut context = Context::from_waker(waker);
    let mut future = pin!(future);
    loop {
        if let Poll::Ready(output) = future.as_mut().poll(&mut context) {
            return output;
        }
        std::thread::yield_now();
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
fn canonical_domain_identities_are_reused_and_storage_projections_round_trip() {
    fn accepts_domain_project(_: ProjectId) {}
    fn accepts_domain_profile(_: UserProfileId) {}
    fn accepts_domain_repository(_: RepositoryId) {}
    fn accepts_domain_worktree(_: WorktreeId) {}

    let project: tracedecay_store::ProjectId = id("project.canonical");
    let profile: tracedecay_store::UserProfileId = id("profile.canonical");
    let repository: tracedecay_store::RepositoryId = id("repository.canonical");
    let worktree: tracedecay_store::WorktreeId = id("worktree.canonical");
    accepts_domain_project(project);
    accepts_domain_profile(profile);
    accepts_domain_repository(repository);
    accepts_domain_worktree(worktree);

    let canonical_epoch = AuthorityEpoch(9);
    let store_epoch = StoreAuthorityEpochV1::try_from(canonical_epoch).unwrap();
    assert_eq!(AuthorityEpoch::from(store_epoch), canonical_epoch);
    assert!(StoreAuthorityEpochV1::try_from(AuthorityEpoch(0)).is_err());

    let effect = StoreEffectIdV1::try_from("effect.canonical").unwrap();
    let effect_wire = String::from(effect.clone());
    assert_eq!(StoreEffectIdV1::try_from(effect_wire).unwrap(), effect);

    let idempotency = StoreIdempotencyKeyV1::try_from("idempotency.canonical").unwrap();
    let idempotency_wire = String::from(idempotency.clone());
    assert_eq!(
        StoreIdempotencyKeyV1::try_from(idempotency_wire).unwrap(),
        idempotency
    );

    assert_ne!(
        std::any::TypeId::of::<StoreSnapshotIdV1>(),
        std::any::TypeId::of::<tracedecay_domain::RepositoryStateSnapshotId>()
    );
    assert_ne!(
        std::any::TypeId::of::<ShardWatermarkV1>(),
        std::any::TypeId::of::<tracedecay_domain::ShardWatermark>()
    );
    assert_ne!(
        std::any::TypeId::of::<FrozenWatermarkVectorV1>(),
        std::any::TypeId::of::<tracedecay_domain::VectorWatermark>()
    );
}

#[test]
fn identity_and_budget_validation_fail_closed() {
    assert!(StoreIncarnationV1::new(0).is_err());
    assert!(StoreAuthorityEpochV1::new(0).is_err());
    assert!(StoreIdempotencyKeyV1::new(" idempotency.fixture").is_err());
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
        key: StoreIdempotencyKeyV1::new("command.fixture").unwrap(),
        command_digest: digest('a'),
    };
    let same = committed.clone();
    let different_command = IdempotencyIdentityV1 {
        key: committed.key.clone(),
        command_digest: digest('b'),
    };
    let different_key = IdempotencyIdentityV1 {
        key: StoreIdempotencyKeyV1::new("command.other").unwrap(),
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
        payload: RepositoryWritePayloadV1::Diagnostics(Box::new(
            SanitizedCleanDiagnosticSnapshotV1::new(
                id::<CodeGenerationId>("generation.fixture"),
                vec![],
            )
            .unwrap(),
        )),
    };
    valid.validate().unwrap();

    let invalid_scope = RepositoryOperationEnvelopeV1 {
        metadata: metadata(code_worktree_shard("project.one"), DurabilityClassV1::Full),
        payload: valid.payload.clone(),
    };
    assert!(matches!(
        invalid_scope.validate(),
        Err(StorageRuntimeContractErrorV1::OperationScopeMismatch { .. })
    ));

    let wrong_durability = RepositoryOperationEnvelopeV1 {
        metadata: metadata(
            project_shard("project.one"),
            DurabilityClassV1::RebuildableProjection,
        ),
        payload: valid.payload.clone(),
    };
    assert!(matches!(
        wrong_durability.validate(),
        Err(StorageRuntimeContractErrorV1::DurabilityMismatch { .. })
    ));

    let immutable_snapshot = RepositoryOperationEnvelopeV1 {
        metadata: metadata(code_snapshot_shard("project.one"), DurabilityClassV1::Full),
        payload: outbox_payload(),
    };
    assert!(matches!(
        immutable_snapshot.validate(),
        Err(StorageRuntimeContractErrorV1::ImmutableShard { .. })
    ));

    let invalid_payload = RepositoryOperationEnvelopeV1 {
        metadata: metadata(project_shard("project.one"), DurabilityClassV1::Full),
        payload: RepositoryWritePayloadV1::EnqueueOutbox(Box::new(TransactionalOutboxEntryV1 {
            identity: effect_identity(),
            effect: RepositoryEffectV1::PublishObservation,
            state: OutboxEffectStateV1::Acknowledged,
            acknowledgement: None,
            enqueued_at: UtcMicros(1),
            updated_at: UtcMicros(2),
        })),
    };
    assert!(matches!(
        invalid_payload.validate(),
        Err(StorageRuntimeContractErrorV1::AcknowledgementReceiptRequired)
    ));
    let invalid_scope = transaction_scope(&invalid_payload.metadata);
    assert!(matches!(
        RuntimeSubmitRequestV1::new(invalid_payload, invalid_scope, control()),
        Err(StorageRuntimeContractErrorV1::AcknowledgementReceiptRequired)
    ));
}

fn effect_identity() -> EffectIdentityV1 {
    let source = project_shard("project.one");
    let target = session_shard("project.one");
    EffectIdentityV1 {
        effect_id: StoreEffectIdV1::new("effect.fixture").unwrap(),
        command_digest: digest('d'),
        ordering_key: StoreEffectOrderingKeyV1::new("project.one.observations").unwrap(),
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

struct Probe {
    identity: RuntimeCancellationIdentityV1,
    deadline: RuntimeDeadlineV1,
    interruption: AtomicU8,
}

impl Probe {
    fn new(control: &RuntimeRequestControlV1, interruption: Option<RuntimeInterruptionV1>) -> Self {
        Self {
            identity: control.cancellation.clone(),
            deadline: control.deadline.clone(),
            interruption: AtomicU8::new(match interruption {
                None => 0,
                Some(RuntimeInterruptionV1::Cancelled) => 1,
                Some(RuntimeInterruptionV1::DeadlineExceeded) => 2,
            }),
        }
    }
}

impl RuntimeRequestProbeV1 for Probe {
    fn cancellation_identity(&self) -> &RuntimeCancellationIdentityV1 {
        &self.identity
    }

    fn deadline_identity(&self) -> &RuntimeDeadlineV1 {
        &self.deadline
    }

    fn interruption(&self) -> Option<RuntimeInterruptionV1> {
        match self.interruption.load(Ordering::SeqCst) {
            0 => None,
            1 => Some(RuntimeInterruptionV1::Cancelled),
            2 => Some(RuntimeInterruptionV1::DeadlineExceeded),
            _ => unreachable!("test probe has a closed interruption state"),
        }
    }
}

struct FakeReadPort {
    calls: AtomicUsize,
}

impl StorageRuntimeReadPort for FakeReadPort {
    fn dispatch_read<'a>(
        &'a self,
        request: RuntimeReadRequestV1,
        _probe: &'a dyn RuntimeRequestProbeV1,
    ) -> StorageRuntimePortFutureV1<'a, RuntimeReadOutcomeV1> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            let observed = ShardWatermarkV1 {
                shard_id: request.binding().shard_id.clone(),
                incarnation: request.binding().incarnation,
                authority_epoch: request.binding().authority_epoch,
                commit_sequence: CommitSequenceV1(9),
            };
            RuntimeReadOutcomeV1::new(
                Some(RuntimeReadResultV1::CurrentWatermark {
                    watermark: observed.clone(),
                }),
                RuntimeReadCoverageV1::Latest {
                    observed: Some(observed),
                },
            )
            .map_err(StorageRuntimePortErrorV1::InvalidResponse)
        })
    }
}

fn read_request(
    binding: StoreRuntimeBindingV1,
    consistency: ConsistencyModeV1,
    operation: RuntimeReadOperationV1,
) -> RuntimeReadRequestV1 {
    RuntimeReadRequestV1::new(
        binding,
        consistency,
        operation,
        OperationPriorityV1::Foreground,
        64,
        control(),
    )
    .unwrap()
}

#[test]
fn runtime_submit_outcomes_validate_request_identity() {
    let original = metadata(project_shard("project.one"), DurabilityClassV1::Full);
    let request = submit_request(original.clone());
    let retry = StoreOperationMetadataV1 {
        operation_id: StoreOperationIdV1::new("operation.retry").unwrap(),
        ..original.clone()
    };
    RuntimeSubmitOutcomeV1::ExactReplay {
        receipt: commit_receipt(&original),
    }
    .validate_for(&submit_request(retry))
    .unwrap();

    let mut existing = original.clone();
    existing.idempotency.command_digest = digest('e');
    RuntimeSubmitOutcomeV1::IdempotencyConflict {
        existing_receipt: commit_receipt(&existing),
    }
    .validate_for(&submit_request(original.clone()))
    .unwrap();

    RuntimeSubmitOutcomeV1::CommittedAfterCancellation {
        receipt: commit_receipt(&original),
        cancellation: request.control().cancellation.clone(),
    }
    .validate_for(&request)
    .unwrap();

    assert!(matches!(
        RuntimeSubmitOutcomeV1::Unavailable {
            reason: UnavailableReasonV1::DeadlineExceeded,
        }
        .validate_for(&request),
        Err(StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
            field: "submit decision channel"
        })
    ));
}

#[test]
fn typed_async_reads_report_latest_exact_partial_stale_and_unavailable_coverage() {
    let latest = read_request(
        binding(project_shard("project.one")),
        ConsistencyModeV1::LatestAvailable,
        RuntimeReadOperationV1::CurrentWatermark,
    );
    let probe = Probe::new(latest.control(), None);
    let read_port = FakeReadPort {
        calls: AtomicUsize::new(0),
    };
    let object_safe_port: &dyn StorageRuntimeReadPort = &read_port;
    assert!(matches!(
        block_on(object_safe_port.read(latest, &probe))
            .unwrap()
            .coverage(),
        RuntimeReadCoverageV1::Latest { .. }
    ));

    for (interruption, reason) in [
        (
            RuntimeInterruptionV1::Cancelled,
            UnavailableReasonV1::Cancelled,
        ),
        (
            RuntimeInterruptionV1::DeadlineExceeded,
            UnavailableReasonV1::DeadlineExceeded,
        ),
    ] {
        let request = read_request(
            binding(project_shard("project.one")),
            ConsistencyModeV1::LatestAvailable,
            RuntimeReadOperationV1::CurrentWatermark,
        );
        let probe = Probe::new(request.control(), Some(interruption));
        let outcome = block_on(object_safe_port.read(request, &probe)).unwrap();
        assert!(matches!(
            outcome.coverage(),
            RuntimeReadCoverageV1::Unavailable {
                coverage: None,
                reason: actual,
            } if *actual == reason
        ));
    }
    assert_eq!(read_port.calls.load(Ordering::SeqCst), 1);

    let at_least = read_request(
        binding(project_shard("project.one")),
        ConsistencyModeV1::AtLeast {
            commit_sequence: CommitSequenceV1(10),
        },
        RuntimeReadOperationV1::CurrentWatermark,
    );
    let stale = single_shard_required_coverage_v1(
        at_least.binding(),
        CommitSequenceV1(10),
        [watermark(project_shard("project.one"), 9)],
    )
    .unwrap();
    RuntimeReadOutcomeV1::new(None, RuntimeReadCoverageV1::Stale { coverage: stale })
        .unwrap()
        .validate_for(&at_least)
        .unwrap();

    let exact_watermark = watermark(project_shard("project.one"), 8);
    let exact = read_request(
        binding(project_shard("project.one")),
        ConsistencyModeV1::ExactSnapshot {
            lease: Box::new(SnapshotLeaseV1 {
                lease_id: SnapshotLeaseIdV1::new("snapshot.exact").unwrap(),
                snapshot_id: StoreSnapshotIdV1::new("snapshot.exact").unwrap(),
                watermark: exact_watermark.clone(),
                acquired_at: UtcMicros(1),
                expires_at: UtcMicros(10),
            }),
        },
        RuntimeReadOperationV1::CurrentWatermark,
    );
    let exact_coverage = single_shard_required_coverage_v1(
        exact.binding(),
        CommitSequenceV1(8),
        [exact_watermark.clone()],
    )
    .unwrap();
    RuntimeReadOutcomeV1::new(
        Some(RuntimeReadResultV1::CurrentWatermark {
            watermark: exact_watermark,
        }),
        RuntimeReadCoverageV1::Complete {
            coverage: exact_coverage,
        },
    )
    .unwrap()
    .validate_for(&exact)
    .unwrap();

    let required = FrozenWatermarkVectorV1::new([
        watermark(project_shard("project.one"), 10),
        watermark(session_shard("project.one"), 20),
    ])
    .unwrap();
    let frozen = read_request(
        binding(project_shard("project.one")),
        ConsistencyModeV1::FrozenWatermarkVector {
            vector: required.clone(),
        },
        RuntimeReadOperationV1::FrozenCoverage,
    );
    let partial = FrozenWatermarkCoverageV1::new(
        required.clone(),
        [
            watermark(project_shard("project.one"), 10),
            watermark(session_shard("project.one"), 19),
        ],
    )
    .unwrap();
    RuntimeReadOutcomeV1::new(
        Some(RuntimeReadResultV1::FrozenCoverage {
            coverage: partial.clone(),
        }),
        RuntimeReadCoverageV1::Partial { coverage: partial },
    )
    .unwrap()
    .validate_for(&frozen)
    .unwrap();

    let unavailable = FrozenWatermarkCoverageV1::new(required, []).unwrap();
    RuntimeReadOutcomeV1::new(
        None,
        RuntimeReadCoverageV1::Unavailable {
            coverage: Some(unavailable),
            reason: UnavailableReasonV1::MissingAuthority,
        },
    )
    .unwrap()
    .validate_for(&frozen)
    .unwrap();
}

fn graph_node(id: &str, name: &str) -> GraphNodeV1 {
    GraphNodeV1 {
        id: id.to_owned(),
        kind: "function".to_owned(),
        name: name.to_owned(),
        qualified_name: format!("fixture::{name}"),
        file_path: "src/fixture.rs".to_owned(),
        start_line: 1,
        attrs_start_line: 1,
        end_line: 2,
        start_column: 0,
        end_column: 1,
        signature: Some(format!("fn {name}()")),
        docstring: None,
        visibility: "public".to_owned(),
        is_async: false,
        branches: 0,
        loops: 0,
        returns: 0,
        max_nesting: 0,
        unsafe_blocks: 0,
        unchecked_calls: 0,
        assertions: 0,
        updated_at: 1,
        parent_id: None,
    }
}

#[test]
fn graph_read_contracts_preserve_backend_order_and_legacy_query_inputs() {
    let binding = binding(code_worktree_shard("project.one"));
    let search = read_request(
        binding.clone(),
        ConsistencyModeV1::LatestAvailable,
        RuntimeReadOperationV1::GraphSearch {
            query: "fixture".to_owned(),
            limit: 2,
        },
    );
    let observed = watermark(binding.shard_id.clone(), 9);
    let ordered = RuntimeReadOutcomeV1::new(
        Some(RuntimeReadResultV1::GraphSearch {
            results: vec![
                GraphSearchResultV1 {
                    node: graph_node("node.a", "alpha"),
                    score: GraphSearchScoreV1::new(2.0).unwrap(),
                },
                GraphSearchResultV1 {
                    node: graph_node("node.b", "beta"),
                    score: GraphSearchScoreV1::new(1.0).unwrap(),
                },
            ],
        }),
        RuntimeReadCoverageV1::Latest { observed: None },
    )
    .unwrap();
    ordered.validate_for(&search).unwrap();
    round_trip(&ordered);

    let unordered = RuntimeReadOutcomeV1::new(
        Some(RuntimeReadResultV1::GraphSearch {
            results: vec![
                GraphSearchResultV1 {
                    node: graph_node("node.b", "beta"),
                    score: GraphSearchScoreV1::new(1.0).unwrap(),
                },
                GraphSearchResultV1 {
                    node: graph_node("node.a", "alpha"),
                    score: GraphSearchScoreV1::new(2.0).unwrap(),
                },
            ],
        }),
        RuntimeReadCoverageV1::Latest {
            observed: Some(observed),
        },
    )
    .unwrap();
    unordered.validate_for(&search).unwrap();

    for query in [" fixture ", "fixture\nterm"] {
        RuntimeReadRequestV1::new(
            binding.clone(),
            ConsistencyModeV1::LatestAvailable,
            RuntimeReadOperationV1::GraphSearch {
                query: query.to_owned(),
                limit: 2,
            },
            OperationPriorityV1::Foreground,
            64,
            control(),
        )
        .expect("legacy graph search accepts whitespace and control characters");
    }

    assert!(GraphSearchScoreV1::new(f64::NAN).is_err());
    assert!(
        RuntimeReadRequestV1::new(
            binding,
            ConsistencyModeV1::LatestAvailable,
            RuntimeReadOperationV1::GraphSearch {
                query: "fixture".to_owned(),
                limit: RuntimeReadOperationV1::MAX_GRAPH_SEARCH_RESULTS + 1,
            },
            OperationPriorityV1::Foreground,
            64,
            control(),
        )
        .is_err()
    );
    round_trip(&UnavailableReasonV1::UnsupportedOperation);
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
    let runtime_error = StorageRuntimeErrorV1::Infrastructure {
        operation: "fixture read".to_owned(),
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
    let mut invalid_control = serde_json::to_value(control()).unwrap();
    invalid_control["cancellation"]["generation"] = json!(0);
    assert!(serde_json::from_value::<RuntimeRequestControlV1>(invalid_control).is_err());

    let mut wall_clock_deadline = serde_json::to_value(control()).unwrap();
    wall_clock_deadline["deadline"]["expires_at"] = json!(100);
    assert!(serde_json::from_value::<RuntimeRequestControlV1>(wall_clock_deadline).is_err());

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

    let frozen_request = read_request(
        binding(project_shard("project.one")),
        ConsistencyModeV1::FrozenWatermarkVector {
            vector: FrozenWatermarkVectorV1::new([watermark(project_shard("project.one"), 1)])
                .unwrap(),
        },
        RuntimeReadOperationV1::FrozenCoverage,
    );
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
