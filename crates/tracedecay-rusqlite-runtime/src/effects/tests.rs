use std::sync::Arc;

use rusqlite::{Connection, Savepoint};
use serde_json::json;
use tracedecay_store::{
    AdmissionConfigV1, BrainId, CommandDigestV1, CommitSequenceV1, EffectIdentityV1,
    InboxEffectDispositionV1, LocatorDigest, OutboxAcknowledgementReceiptV1, OutboxEffectStateV1,
    ProjectId, RepositoryEffectV1, RepositoryWritePayloadV1, RuntimeCancellationIdentityV1,
    RuntimeDeadlineV1, RuntimeInterruptionV1, RuntimeRequestProbeV1, RuntimeSubmitOutcomeV1,
    RuntimeSubmitRequestV1, ShardWatermarkV1, StoreAuthorityEpochV1, StoreEffectIdV1,
    StoreEffectOrderingKeyV1, StoreIncarnationV1, StoreRuntimeBindingV1, StoreShardIdV1,
    TransactionalInboxReceiptV1, TransactionalOutboxEntryV1, UserProfileId, VerifiedStoreLocatorV1,
};

use super::{
    EffectCoordinator, EffectCoordinatorError, EffectDispatchOutcome, EffectUnknownCause,
    EffectsLedgerReadExecutor, OriginDispatchPreparation, OriginEffectReplayTransactions,
    OriginEffectTransactions, SqliteOriginEffectTransactions, SqliteTargetEffectTransactions,
    TargetEffectTransactions,
};
use crate::{
    ExistingWriterLocator, PersistentWriter, StorageOperationExecutor,
    repository::ConcreteRepositoryReadExecutor,
    test_support::{metadata, request},
};
use tracedecay_store::{
    CodeReadOperationV1, CodeRecoveryRepositoriesQueryV1, EffectsInboxPageQueryV1,
    EffectsOutboxCursorV1, EffectsOutboxPageQueryV1, EffectsReadOperationV1, EffectsReadResultV1,
    RepositoryReadOperationV1,
};

fn id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    <T as TryFrom<String>>::Error: std::fmt::Debug,
{
    T::try_from(value.to_owned()).unwrap()
}

fn binding(project: &str, epoch: u64) -> StoreRuntimeBindingV1 {
    StoreRuntimeBindingV1::new(
        StoreShardIdV1::project(
            id::<BrainId>("brain.fixture"),
            id::<UserProfileId>("profile.fixture"),
            id::<ProjectId>(project),
        ),
        StoreIncarnationV1::new(1).unwrap(),
        StoreAuthorityEpochV1::new(epoch).unwrap(),
    )
}

fn session_binding(project: &str, epoch: u64) -> StoreRuntimeBindingV1 {
    StoreRuntimeBindingV1::new(
        StoreShardIdV1::project_sessions(
            id::<BrainId>("brain.fixture"),
            id::<UserProfileId>("profile.fixture"),
            id::<ProjectId>(project),
        ),
        StoreIncarnationV1::new(1).unwrap(),
        StoreAuthorityEpochV1::new(epoch).unwrap(),
    )
}

fn watermark(binding: &StoreRuntimeBindingV1, sequence: u64) -> ShardWatermarkV1 {
    ShardWatermarkV1 {
        shard_id: binding.shard_id.clone(),
        incarnation: binding.incarnation,
        authority_epoch: binding.authority_epoch,
        commit_sequence: CommitSequenceV1(sequence),
    }
}

fn fixture() -> (
    StoreRuntimeBindingV1,
    StoreRuntimeBindingV1,
    TransactionalOutboxEntryV1,
) {
    let source = binding("project.source", 7);
    let target = session_binding("project.source", 11);
    let identity = EffectIdentityV1 {
        effect_id: StoreEffectIdV1::new("effect.fixture").unwrap(),
        command_digest: CommandDigestV1::new(format!("sha256:{}", "a".repeat(64))).unwrap(),
        ordering_key: StoreEffectOrderingKeyV1::new("project.fixture:observations").unwrap(),
        source_watermark: watermark(&source, 4),
        target_watermark: watermark(&target, 8),
    };
    let entry = serde_json::from_value(json!({
        "identity": identity,
        "effect": RepositoryEffectV1::PublishObservation,
        "state": OutboxEffectStateV1::Pending,
        "acknowledgement": null,
        "enqueued_at": 100,
        "updated_at": 100
    }))
    .unwrap();
    (source, target, entry)
}

struct FakeOrigin {
    entry: TransactionalOutboxEntryV1,
    sequence: u64,
    now: i64,
}

impl FakeOrigin {
    fn new(entry: TransactionalOutboxEntryV1) -> Self {
        Self {
            entry,
            sequence: 4,
            now: 100,
        }
    }

    fn tick(&mut self) -> i64 {
        self.now += 10;
        self.now
    }
}

impl OriginEffectTransactions for FakeOrigin {
    type Error = &'static str;

    async fn prepare_dispatch(
        &mut self,
        binding: &StoreRuntimeBindingV1,
        effect_id: &StoreEffectIdV1,
    ) -> Result<Option<OriginDispatchPreparation>, Self::Error> {
        if self.entry.identity.source_watermark.shard_id != binding.shard_id {
            return Err("origin fence");
        }
        if &self.entry.identity.effect_id != effect_id {
            return Ok(None);
        }
        Ok(Some(match self.entry.state {
            OutboxEffectStateV1::Pending | OutboxEffectStateV1::EffectUnknown => {
                let now = serde_json::from_value(json!(self.tick())).unwrap();
                self.entry
                    .transition(OutboxEffectStateV1::Dispatched, now)
                    .unwrap();
                OriginDispatchPreparation::Prepared(self.entry.clone())
            }
            OutboxEffectStateV1::Dispatched => {
                OriginDispatchPreparation::InFlightWithoutReceipt(self.entry.clone())
            }
            OutboxEffectStateV1::Acknowledged => {
                OriginDispatchPreparation::Acknowledged(self.entry.acknowledgement.clone().unwrap())
            }
        }))
    }

    async fn mark_effect_unknown(
        &mut self,
        binding: &StoreRuntimeBindingV1,
        entry: &TransactionalOutboxEntryV1,
    ) -> Result<TransactionalOutboxEntryV1, Self::Error> {
        if entry.identity.source_watermark.shard_id != binding.shard_id {
            return Err("origin fence");
        }
        let now = serde_json::from_value(json!(self.tick())).unwrap();
        self.entry
            .transition(OutboxEffectStateV1::EffectUnknown, now)
            .unwrap();
        Ok(self.entry.clone())
    }

    async fn acknowledge(
        &mut self,
        binding: &StoreRuntimeBindingV1,
        inbox: &TransactionalInboxReceiptV1,
    ) -> Result<OutboxAcknowledgementReceiptV1, Self::Error> {
        if self.entry.identity.source_watermark.shard_id != binding.shard_id {
            return Err("origin fence");
        }
        self.sequence += 1;
        self.now = self.now.max(inbox.committed_at.0) + 10;
        let receipt: OutboxAcknowledgementReceiptV1 = serde_json::from_value(json!({
            "identity": self.entry.identity,
            "inbox_receipt": inbox,
            "source_commit_watermark": watermark(binding, self.sequence),
            "acknowledged_at": self.now
        }))
        .unwrap();
        self.entry.acknowledge(receipt.clone()).unwrap();
        Ok(receipt)
    }
}

impl OriginEffectReplayTransactions for FakeOrigin {
    async fn replay_candidates(
        &mut self,
        _origin_binding: &StoreRuntimeBindingV1,
        _target_binding: &StoreRuntimeBindingV1,
        limit: usize,
    ) -> Result<Vec<StoreEffectIdV1>, <Self as OriginEffectTransactions>::Error> {
        Ok(vec![
            self.entry.identity.effect_id.clone();
            limit.saturating_add(2)
        ])
    }
}

#[derive(Default)]
struct FakeTarget {
    receipt: Option<TransactionalInboxReceiptV1>,
    applied: usize,
    calls: usize,
    lose_first_receipt: bool,
}

impl TargetEffectTransactions for FakeTarget {
    type Error = &'static str;

    async fn apply_once(
        &mut self,
        binding: &StoreRuntimeBindingV1,
        entry: &TransactionalOutboxEntryV1,
    ) -> Result<TransactionalInboxReceiptV1, Self::Error> {
        self.calls += 1;
        if entry.identity.target_watermark.shard_id != binding.shard_id {
            return Err("target fence");
        }
        if let Some(receipt) = &self.receipt {
            let mut replay = receipt.clone();
            replay.disposition = InboxEffectDispositionV1::Replayed;
            return Ok(replay);
        }
        self.applied += 1;
        let receipt: TransactionalInboxReceiptV1 = serde_json::from_value(json!({
            "identity": entry.identity,
            "disposition": InboxEffectDispositionV1::Applied,
            "target_commit_watermark": watermark(binding, 9),
            "committed_at": 150
        }))
        .unwrap();
        self.receipt = Some(receipt.clone());
        if self.lose_first_receipt {
            self.lose_first_receipt = false;
            Err("receipt response lost")
        } else {
            Ok(receipt)
        }
    }
}

#[test]
fn lost_target_receipt_becomes_unknown_then_replays_effect_once() {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(async {
            let (source, target_binding, entry) = fixture();
            let effect_id = entry.identity.effect_id.clone();
            let mut origin = FakeOrigin::new(entry);
            let mut target = FakeTarget {
                lose_first_receipt: true,
                ..FakeTarget::default()
            };

            let first = EffectCoordinator
                .dispatch(
                    &effect_id,
                    &source,
                    &target_binding,
                    &mut origin,
                    &mut target,
                )
                .await
                .unwrap();
            assert!(matches!(
                first,
                EffectDispatchOutcome::EffectUnknown(unknown)
                    if matches!(unknown.cause, EffectUnknownCause::Target("receipt response lost"))
            ));
            assert_eq!(origin.entry.state, OutboxEffectStateV1::EffectUnknown);

            let second = EffectCoordinator
                .dispatch(
                    &effect_id,
                    &source,
                    &target_binding,
                    &mut origin,
                    &mut target,
                )
                .await
                .unwrap();
            assert!(matches!(second, EffectDispatchOutcome::Acknowledged { .. }));
            assert_eq!(target.applied, 1);
            assert_eq!(origin.entry.state, OutboxEffectStateV1::Acknowledged);
        });
}

#[test]
fn restart_marks_orphaned_dispatch_unknown_before_retrying() {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(async {
            let (source, target_binding, entry) = fixture();
            let effect_id = entry.identity.effect_id.clone();
            let mut origin = FakeOrigin::new(entry);
            let mut target = FakeTarget::default();
            let _crashed_after = origin.prepare_dispatch(&source, &effect_id).await.unwrap();

            let recovered = EffectCoordinator
                .dispatch(
                    &effect_id,
                    &source,
                    &target_binding,
                    &mut origin,
                    &mut target,
                )
                .await
                .unwrap();
            assert!(matches!(
                recovered,
                EffectDispatchOutcome::EffectUnknown(unknown)
                    if matches!(unknown.cause, EffectUnknownCause::RecoveredInFlight)
            ));
            assert_eq!(target.calls, 0);

            let completed = EffectCoordinator
                .dispatch(
                    &effect_id,
                    &source,
                    &target_binding,
                    &mut origin,
                    &mut target,
                )
                .await
                .unwrap();
            assert!(matches!(
                completed,
                EffectDispatchOutcome::Acknowledged { .. }
            ));
            assert_eq!(target.applied, 1);
        });
}

#[test]
fn stale_target_epoch_is_rejected_before_target_sink() {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(async {
            let (source, _target_binding, entry) = fixture();
            let effect_id = entry.identity.effect_id.clone();
            let stale_target = session_binding("project.source", 12);
            let mut origin = FakeOrigin::new(entry);
            let mut target = FakeTarget::default();

            let result = EffectCoordinator
                .dispatch(&effect_id, &source, &stale_target, &mut origin, &mut target)
                .await;
            assert!(matches!(result, Err(EffectCoordinatorError::Contract(_))));
            assert_eq!(target.calls, 0);
        });
}

#[test]
fn foreign_project_is_rejected_before_origin_transition() {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(async {
            let (source, target_binding, entry) = fixture();
            let effect_id = entry.identity.effect_id.clone();
            let foreign_project = StoreRuntimeBindingV1::new(
                StoreShardIdV1::project_sessions(
                    target_binding.shard_id.brain_id.clone(),
                    target_binding.shard_id.profile_id.clone(),
                    id::<ProjectId>("project.foreign"),
                ),
                target_binding.incarnation,
                target_binding.authority_epoch,
            );
            let mut origin = FakeOrigin::new(entry);
            let mut target = FakeTarget::default();

            let result = EffectCoordinator
                .dispatch(
                    &effect_id,
                    &source,
                    &foreign_project,
                    &mut origin,
                    &mut target,
                )
                .await;

            assert!(matches!(result, Err(EffectCoordinatorError::Contract(_))));
            assert_eq!(origin.entry.state, OutboxEffectStateV1::Pending);
            assert_eq!(target.calls, 0);
        });
}

#[test]
fn foreign_authority_root_is_rejected_before_origin_transition() {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(async {
            let (source, target_binding, entry) = fixture();
            let effect_id = entry.identity.effect_id.clone();
            let foreign_target = StoreRuntimeBindingV1::new(
                StoreShardIdV1::project_sessions(
                    id::<BrainId>("brain.foreign"),
                    target_binding.shard_id.profile_id.clone(),
                    id::<ProjectId>("project.source"),
                ),
                target_binding.incarnation,
                target_binding.authority_epoch,
            );
            let mut origin = FakeOrigin::new(entry);
            let mut target = FakeTarget::default();

            let result = EffectCoordinator
                .dispatch(
                    &effect_id,
                    &source,
                    &foreign_target,
                    &mut origin,
                    &mut target,
                )
                .await;

            assert!(matches!(result, Err(EffectCoordinatorError::Contract(_))));
            assert_eq!(origin.entry.state, OutboxEffectStateV1::Pending);
            assert_eq!(target.calls, 0);
        });
}

#[test]
fn bounded_replay_never_exceeds_the_requested_attempt_count() {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(async {
            let (source, target_binding, entry) = fixture();
            let mut origin = FakeOrigin::new(entry);
            let mut target = FakeTarget::default();

            let report = EffectCoordinator
                .replay_bounded(&source, &target_binding, &mut origin, &mut target, 1)
                .await
                .unwrap();

            assert_eq!(report.limit, 1);
            assert_eq!(report.attempts.len(), 1);
            assert!(matches!(
                &report.attempts[0].result,
                Ok(EffectDispatchOutcome::Acknowledged { .. })
            ));
            assert_eq!(target.applied, 1);
        });
}

#[derive(Default)]
struct SqliteEffectRecorder;

impl StorageOperationExecutor for SqliteEffectRecorder {
    fn execute(
        &mut self,
        savepoint: &Savepoint<'_>,
        payload: &RepositoryWritePayloadV1,
    ) -> rusqlite::Result<()> {
        match payload {
            RepositoryWritePayloadV1::EnqueueOutbox(entry)
                if entry.state == OutboxEffectStateV1::Pending =>
            {
                savepoint.execute_batch(
                    "CREATE TABLE IF NOT EXISTS source_domain_mutation (
                        effect_id TEXT NOT NULL
                    )",
                )?;
                savepoint.execute(
                    "INSERT INTO source_domain_mutation(effect_id) VALUES (?1)",
                    [entry.identity.effect_id.as_str()],
                )?;
            }
            RepositoryWritePayloadV1::ApplyInbox(entry) => {
                savepoint.execute_batch(
                    "CREATE TABLE IF NOT EXISTS applied_effect (
                        effect TEXT NOT NULL
                    )",
                )?;
                savepoint.execute(
                    "INSERT INTO applied_effect(effect) VALUES (?1)",
                    [format!("{:?}", entry.effect)],
                )?;
            }
            _ => {}
        }
        Ok(())
    }
}

struct TestProbe {
    cancellation: RuntimeCancellationIdentityV1,
    deadline: RuntimeDeadlineV1,
}

impl RuntimeRequestProbeV1 for TestProbe {
    fn cancellation_identity(&self) -> &RuntimeCancellationIdentityV1 {
        &self.cancellation
    }

    fn deadline_identity(&self) -> &RuntimeDeadlineV1 {
        &self.deadline
    }

    fn interruption(&self) -> Option<RuntimeInterruptionV1> {
        None
    }
}

fn writer(
    path: &std::path::Path,
    binding: &StoreRuntimeBindingV1,
) -> (ExistingWriterLocator, PersistentWriter) {
    if !path.exists() {
        std::fs::File::create(path).unwrap();
    }
    let locator = ExistingWriterLocator::new(
        binding.clone(),
        VerifiedStoreLocatorV1::new(
            binding.shard_id.clone(),
            binding.incarnation,
            LocatorDigest::new(format!("sha256:{}", "d".repeat(64))).unwrap(),
        ),
        path.to_owned(),
    )
    .unwrap();
    let writer = PersistentWriter::start(
        locator.clone(),
        AdmissionConfigV1::default(),
        SqliteEffectRecorder,
    )
    .unwrap();
    (locator, writer)
}

async fn seed_outbox(
    writer: &PersistentWriter,
    operation_id: &str,
    idempotency_key: &str,
    digest_byte: char,
    source_sequence: u64,
) -> TransactionalOutboxEntryV1 {
    let initial = request(metadata(operation_id, idempotency_key, digest_byte));
    let mut envelope = initial.envelope().clone();
    let entry = match &mut envelope.payload {
        RepositoryWritePayloadV1::EnqueueOutbox(entry) => {
            entry.identity.source_watermark.commit_sequence = CommitSequenceV1(source_sequence);
            (**entry).clone()
        }
        _ => unreachable!(),
    };
    let request = RuntimeSubmitRequestV1::new(
        envelope,
        initial.transaction_scope().clone(),
        initial.control().clone(),
    )
    .unwrap();
    let probe = Arc::new(TestProbe {
        cancellation: request.control().cancellation.clone(),
        deadline: request.control().deadline.clone(),
    });
    assert!(matches!(
        writer.submit(request, probe).await.unwrap(),
        RuntimeSubmitOutcomeV1::Committed { .. }
    ));
    entry
}

#[test]
fn sqlite_effects_survive_restart_replay_target_once_and_persist_ack() {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(async {
            let directory = tempfile::tempdir().unwrap();
            let source_path = directory.path().join("source.sqlite");
            let target_path = directory.path().join("target.sqlite");
            let source_metadata = metadata("operation.first", "key.first", 'a');
            let source_binding = crate::test_support::binding(&source_metadata);
            let (source_locator, source_writer) = writer(&source_path, &source_binding);
            let entry = seed_outbox(&source_writer, "operation.first", "key.first", 'a', 0).await;
            let target_binding = StoreRuntimeBindingV1::new(
                entry.identity.target_watermark.shard_id.clone(),
                entry.identity.target_watermark.incarnation,
                entry.identity.target_watermark.authority_epoch,
            );
            let (target_locator, target_writer) = writer(&target_path, &target_binding);
            let effect_id = entry.identity.effect_id.clone();
            let mut origin = SqliteOriginEffectTransactions::open(&source_writer).unwrap();
            let prepared = origin
                .prepare_dispatch(&source_binding, &effect_id)
                .await
                .unwrap()
                .unwrap();
            let dispatched = match prepared {
                OriginDispatchPreparation::Prepared(entry) => entry,
                preparation => panic!("expected prepared dispatch, got {preparation:?}"),
            };
            let mut target = SqliteTargetEffectTransactions::new(&target_writer);
            let applied = target
                .apply_once(&target_binding, &dispatched)
                .await
                .unwrap();
            assert_eq!(applied.disposition, InboxEffectDispositionV1::Applied);
            source_writer.shutdown_and_join().unwrap();
            target_writer.shutdown_and_join().unwrap();

            let source_writer = PersistentWriter::start(
                source_locator.clone(),
                AdmissionConfigV1::default(),
                SqliteEffectRecorder,
            )
            .unwrap();
            let target_writer = PersistentWriter::start(
                target_locator,
                AdmissionConfigV1::default(),
                SqliteEffectRecorder,
            )
            .unwrap();
            let mut origin = SqliteOriginEffectTransactions::open(&source_writer).unwrap();
            let mut target_effects = SqliteTargetEffectTransactions::new(&target_writer);
            let recovered = EffectCoordinator
                .dispatch(
                    &effect_id,
                    &source_binding,
                    &target_binding,
                    &mut origin,
                    &mut target_effects,
                )
                .await
                .unwrap();
            assert!(matches!(
                recovered,
                EffectDispatchOutcome::EffectUnknown(unknown)
                    if matches!(unknown.cause, EffectUnknownCause::RecoveredInFlight)
            ));
            let acknowledged = EffectCoordinator
                .dispatch(
                    &effect_id,
                    &source_binding,
                    &target_binding,
                    &mut origin,
                    &mut target_effects,
                )
                .await
                .unwrap();
            assert!(matches!(
                acknowledged,
                EffectDispatchOutcome::Acknowledged { replayed: true, .. }
            ));
            source_writer.shutdown_and_join().unwrap();
            target_writer.shutdown_and_join().unwrap();

            let source = Connection::open(&source_path).unwrap();
            let entry_json: String = source
                .query_row(
                    "SELECT entry_json FROM td_runtime_writer_outbox_v1 WHERE effect_id = ?1",
                    [effect_id.as_str()],
                    |row| row.get(0),
                )
                .unwrap();
            let persisted: TransactionalOutboxEntryV1 = serde_json::from_str(&entry_json).unwrap();
            assert_eq!(persisted.state, OutboxEffectStateV1::Acknowledged);
            assert!(persisted.acknowledgement.is_some());
            let source_mutations: i64 = source
                .query_row("SELECT COUNT(*) FROM source_domain_mutation", [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(source_mutations, 1);
            let target = Connection::open(&target_path).unwrap();
            let applications: i64 = target
                .query_row("SELECT COUNT(*) FROM applied_effect", [], |row| row.get(0))
                .unwrap();
            assert_eq!(applications, 1);
        });
}

#[test]
fn sqlite_origin_allows_only_the_head_of_an_ordering_key() {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(async {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("source.sqlite");
            let source_metadata = metadata("operation.first", "key.first", 'a');
            let binding = crate::test_support::binding(&source_metadata);
            let (_locator, writer) = writer(&path, &binding);
            let first = seed_outbox(&writer, "operation.first", "key.first", 'a', 0).await;
            let second = seed_outbox(&writer, "operation.second", "key.second", 'b', 1).await;
            let mut origin = SqliteOriginEffectTransactions::open(&writer).unwrap();
            assert!(
                origin
                    .prepare_dispatch(&binding, &second.identity.effect_id)
                    .await
                    .is_err()
            );
            assert!(matches!(
                origin
                    .prepare_dispatch(&binding, &first.identity.effect_id)
                    .await
                    .unwrap(),
                Some(OriginDispatchPreparation::Prepared(_))
            ));
            drop(origin);
            writer.shutdown_and_join().unwrap();
        });
}

#[test]
fn sqlite_target_rejects_stale_incarnation_before_domain_mutation() {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(async {
            let (_source, target_binding, entry) = fixture();
            let stale = StoreRuntimeBindingV1::new(
                target_binding.shard_id.clone(),
                StoreIncarnationV1::new(target_binding.incarnation.get() + 1).unwrap(),
                target_binding.authority_epoch,
            );
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("target.sqlite");
            let (_locator, writer) = writer(&path, &target_binding);
            let result = SqliteTargetEffectTransactions::new(&writer)
                .apply_once(&stale, &entry)
                .await;
            assert!(result.is_err());
            writer.shutdown_and_join().unwrap();
            let connection = Connection::open(path).unwrap();
            let marker_exists: i64 = connection
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master
             WHERE type = 'table' AND name = 'applied_effect'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(marker_exists, 0);
        });
}

#[test]
fn sqlite_target_rejects_replayed_effect_id_with_different_identity() {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(async {
            let (_source, target_binding, mut first) = fixture();
            first.identity.target_watermark.commit_sequence = CommitSequenceV1(0);
            first
                .transition(
                    OutboxEffectStateV1::Dispatched,
                    serde_json::from_value(json!(101)).unwrap(),
                )
                .unwrap();
            let mut collision = first.clone();
            collision.identity.ordering_key =
                StoreEffectOrderingKeyV1::new("project.fixture:collision").unwrap();
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("target.sqlite");
            let (_locator, writer) = writer(&path, &target_binding);
            let mut target = SqliteTargetEffectTransactions::new(&writer);

            assert_eq!(
                target
                    .apply_once(&target_binding, &first)
                    .await
                    .unwrap()
                    .disposition,
                InboxEffectDispositionV1::Applied
            );
            assert!(
                target
                    .apply_once(&target_binding, &collision)
                    .await
                    .is_err()
            );

            writer.shutdown_and_join().unwrap();
            let connection = Connection::open(path).unwrap();
            let applications: i64 = connection
                .query_row("SELECT COUNT(*) FROM applied_effect", [], |row| row.get(0))
                .unwrap();
            assert_eq!(applications, 1);
        });
}

#[test]
fn sqlite_origin_rejects_stale_epoch_without_transitioning_outbox() {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(async {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("source.sqlite");
            let source_metadata = metadata("operation.stale", "key.stale", 'd');
            let binding = crate::test_support::binding(&source_metadata);
            let (_locator, writer) = writer(&path, &binding);
            let entry = seed_outbox(&writer, "operation.stale", "key.stale", 'd', 0).await;
            let stale = StoreRuntimeBindingV1::new(
                entry.identity.source_watermark.shard_id.clone(),
                entry.identity.source_watermark.incarnation,
                StoreAuthorityEpochV1::new(
                    entry.identity.source_watermark.authority_epoch.get() + 1,
                )
                .unwrap(),
            );
            let mut origin = SqliteOriginEffectTransactions::open(&writer).unwrap();
            assert!(
                origin
                    .prepare_dispatch(&stale, &entry.identity.effect_id)
                    .await
                    .is_err()
            );
            drop(origin);
            writer.shutdown_and_join().unwrap();
            let connection = Connection::open(path).unwrap();
            let entry_json: String = connection
                .query_row(
                    "SELECT entry_json FROM td_runtime_writer_outbox_v1 WHERE effect_id = ?1",
                    [entry.identity.effect_id.as_str()],
                    |row| row.get(0),
                )
                .unwrap();
            let persisted: TransactionalOutboxEntryV1 = serde_json::from_str(&entry_json).unwrap();
            assert_eq!(persisted.state, OutboxEffectStateV1::Pending);
        });
}

#[test]
fn effects_read_outbox_entry_round_trips_after_write() {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(async {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("source.sqlite");
            let source_metadata = metadata("operation.read", "key.read", 'a');
            let binding = crate::test_support::binding(&source_metadata);
            let (_locator, writer) = writer(&path, &binding);
            let seeded = seed_outbox(&writer, "operation.read", "key.read", 'a', 0).await;
            writer.shutdown_and_join().unwrap();

            let mut connection = Connection::open(&path).unwrap();
            let transaction = connection.transaction().unwrap();
            let mut reader = EffectsLedgerReadExecutor;

            let hit = reader
                .execute_read(
                    &transaction,
                    &EffectsReadOperationV1::OutboxEntry {
                        binding: binding.clone(),
                        effect_id: seeded.identity.effect_id.clone(),
                    },
                )
                .unwrap();
            match hit {
                EffectsReadResultV1::OutboxEntry(Some(entry)) => assert_eq!(*entry, seeded),
                other => panic!("expected outbox entry, got {other:?}"),
            }

            let miss = reader
                .execute_read(
                    &transaction,
                    &EffectsReadOperationV1::OutboxEntry {
                        binding: binding.clone(),
                        effect_id: StoreEffectIdV1::new("effect.absent").unwrap(),
                    },
                )
                .unwrap();
            assert!(matches!(miss, EffectsReadResultV1::OutboxEntry(None)));
        });
}

#[test]
fn effects_read_outbox_page_walks_keyset_in_order() {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(async {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("source.sqlite");
            let source_metadata = metadata("operation.p0", "key.p0", 'a');
            let binding = crate::test_support::binding(&source_metadata);
            let (_locator, writer) = writer(&path, &binding);
            let first = seed_outbox(&writer, "operation.p0", "key.p0", 'a', 0).await;
            let second = seed_outbox(&writer, "operation.p1", "key.p1", 'b', 1).await;
            let third = seed_outbox(&writer, "operation.p2", "key.p2", 'c', 2).await;
            writer.shutdown_and_join().unwrap();

            let mut connection = Connection::open(&path).unwrap();
            let transaction = connection.transaction().unwrap();
            let mut reader = EffectsLedgerReadExecutor;

            let mut cursor: Option<EffectsOutboxCursorV1> = None;
            let mut walked = Vec::new();
            loop {
                let page = match reader
                    .execute_read(
                        &transaction,
                        &EffectsReadOperationV1::OutboxPage(EffectsOutboxPageQueryV1 {
                            binding: binding.clone(),
                            after: cursor.clone(),
                            limit: 1,
                        }),
                    )
                    .unwrap()
                {
                    EffectsReadResultV1::OutboxPage(page) => page,
                    other => panic!("expected outbox page, got {other:?}"),
                };
                assert!(page.entries.len() <= 1);
                walked.extend(page.entries);
                match page.next {
                    Some(next) => cursor = Some(next),
                    None => break,
                }
            }

            let ordered: Vec<u64> = walked
                .iter()
                .map(|entry| entry.identity.source_watermark.commit_sequence.0)
                .collect();
            assert_eq!(ordered, vec![0, 1, 2]);
            assert_eq!(walked, vec![first, second, third]);
        });
}

#[test]
fn effects_read_inbox_round_trips_after_apply() {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(async {
            let directory = tempfile::tempdir().unwrap();
            let source_path = directory.path().join("source.sqlite");
            let target_path = directory.path().join("target.sqlite");
            let source_metadata = metadata("operation.inbox", "key.inbox", 'a');
            let source_binding = crate::test_support::binding(&source_metadata);
            let (_source_locator, source_writer) = writer(&source_path, &source_binding);
            let entry = seed_outbox(&source_writer, "operation.inbox", "key.inbox", 'a', 0).await;
            let target_binding = StoreRuntimeBindingV1::new(
                entry.identity.target_watermark.shard_id.clone(),
                entry.identity.target_watermark.incarnation,
                entry.identity.target_watermark.authority_epoch,
            );
            let (_target_locator, target_writer) = writer(&target_path, &target_binding);
            let effect_id = entry.identity.effect_id.clone();
            let mut origin = SqliteOriginEffectTransactions::open(&source_writer).unwrap();
            let prepared = origin
                .prepare_dispatch(&source_binding, &effect_id)
                .await
                .unwrap()
                .unwrap();
            let dispatched = match prepared {
                OriginDispatchPreparation::Prepared(entry) => entry,
                preparation => panic!("expected prepared dispatch, got {preparation:?}"),
            };
            let applied = SqliteTargetEffectTransactions::new(&target_writer)
                .apply_once(&target_binding, &dispatched)
                .await
                .unwrap();
            assert_eq!(applied.disposition, InboxEffectDispositionV1::Applied);
            source_writer.shutdown_and_join().unwrap();
            target_writer.shutdown_and_join().unwrap();

            let mut connection = Connection::open(&target_path).unwrap();
            let transaction = connection.transaction().unwrap();
            let mut reader = EffectsLedgerReadExecutor;

            let point = reader
                .execute_read(
                    &transaction,
                    &EffectsReadOperationV1::InboxReceipt {
                        binding: target_binding.clone(),
                        effect_id: effect_id.clone(),
                    },
                )
                .unwrap();
            match point {
                EffectsReadResultV1::InboxReceipt(Some(receipt)) => {
                    assert_eq!(receipt.identity.effect_id, effect_id);
                    assert_eq!(
                        receipt.target_commit_watermark.shard_id,
                        target_binding.shard_id
                    );
                }
                other => panic!("expected inbox receipt, got {other:?}"),
            }

            let page = reader
                .execute_read(
                    &transaction,
                    &EffectsReadOperationV1::InboxPage(EffectsInboxPageQueryV1 {
                        binding: target_binding.clone(),
                        after: None,
                        limit: 8,
                    }),
                )
                .unwrap();
            match page {
                EffectsReadResultV1::InboxPage(page) => {
                    assert_eq!(page.receipts.len(), 1);
                    assert_eq!(page.receipts[0].identity.effect_id, effect_id);
                    assert!(page.next.is_none());
                }
                other => panic!("expected inbox page, got {other:?}"),
            }
        });
}

#[test]
fn repository_read_attachment_rejects_code_and_effects_families() {
    let mut connection = Connection::open_in_memory().unwrap();
    let transaction = connection.transaction().unwrap();
    let mut executor = ConcreteRepositoryReadExecutor::default();

    let effects = executor.execute(
        &transaction,
        &RepositoryReadOperationV1::Effects(EffectsReadOperationV1::OutboxPage(
            EffectsOutboxPageQueryV1 {
                binding: crate::test_support::binding(&metadata("operation.x", "key.x", 'a')),
                after: None,
                limit: 4,
            },
        )),
    );
    assert!(effects.is_err());

    let code = executor.execute(
        &transaction,
        &RepositoryReadOperationV1::Code(CodeReadOperationV1::RecoveryRepositories(
            CodeRecoveryRepositoriesQueryV1 {
                after: None,
                limit: 4,
            },
        )),
    );
    assert!(code.is_err());
}
