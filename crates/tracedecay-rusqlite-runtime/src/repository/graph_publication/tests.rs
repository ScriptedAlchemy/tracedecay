use std::sync::{
    Arc, Barrier,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::time::Duration;

use rusqlite::Savepoint;
use tempfile::TempDir;
use tracedecay_domain::{BrainId, LocatorDigest, ProjectId, UserProfileId, UtcMicros};
use tracedecay_store::{
    AdmissionConfigV1, GraphDependencyGenerationClosureDigestV1,
    GraphDependencyGenerationIdentityV1, GraphGenerationIdV1, GraphNamespaceV1,
    GraphProjectionIdV1, GraphProjectionIdentityV1, GraphPublicationIdempotencyKeyV1,
    GraphPublicationInputDigestV1, GraphPublicationKeyV1, GraphPublicationOperationContextV1,
    GraphPublicationProjectionPageRequestV1, GraphPublicationReplayLookupV1,
    GraphPublicationReplayPageRequestV1, GraphPublicationReplayRetirementV1,
    GraphPublicationReplayV1, GraphPublicationRetiredCleanupPageRequestV1,
    GraphPublicationStoreErrorV1, GraphPublicationStoreV1, GraphRecoveredGenerationDigestV1,
    GraphReplayAppendOutcomeV1, GraphReplayRetirementOutcomeV1,
    GraphRetiredReplayCleanupFinalizeOutcomeV1, GraphVerifiedHeadCasOutcomeV1,
    GraphVerifiedHeadCompareAndSwapV1, RepositoryWritePayloadV1, RuntimeCancellationIdV1,
    RuntimeCancellationIdentityV1, RuntimeDeadlineIdV1, RuntimeDeadlineV1, RuntimeInterruptionV1,
    RuntimeReadOutcomeV1, RuntimeReadRequestV1, RuntimeRequestControlV1, RuntimeRequestProbeV1,
    StoreIncarnationV1, StoreRuntimeBindingV1, VerifiedStoreLocatorV1,
};

use crate::exact_sql::{ExactSqlHandle, ExactSqlStatement, ExactSqlValue};
use crate::reader::{ExistingReaderLocator, ReaderPool, ReaderQueryExecutor};
use crate::{ExistingWriterLocator, PersistentWriter, StorageOperationExecutor};

use super::{GRAPH_PUBLICATION_SCHEMA_V1, GraphPublicationExactSqlStorage};

struct NoWrites;

impl StorageOperationExecutor for NoWrites {
    fn execute(
        &mut self,
        _savepoint: &Savepoint<'_>,
        _payload: &RepositoryWritePayloadV1,
    ) -> rusqlite::Result<()> {
        Ok(())
    }
}

#[derive(Clone)]
struct NoReads;

impl ReaderQueryExecutor for NoReads {
    fn execute_read(
        &mut self,
        _snapshot: &rusqlite::Transaction<'_>,
        _request: &RuntimeReadRequestV1,
    ) -> Result<RuntimeReadOutcomeV1, tracedecay_store::StorageRuntimeErrorV1> {
        unreachable!("exact SQL queries bypass the closed product read executor")
    }
}

struct Fixture {
    _directory: TempDir,
    _writer: PersistentWriter,
    readers: ReaderPool<NoReads>,
    handle: ExactSqlHandle,
}

impl Fixture {
    fn new() -> Self {
        Self::new_for_shard(tracedecay_store::StoreShardIdV1::project(
            BrainId::new("brain.fixture").unwrap(),
            UserProfileId::new("profile.fixture").unwrap(),
            ProjectId::new("project.fixture").unwrap(),
        ))
    }

    fn new_for_shard(shard_id: tracedecay_store::StoreShardIdV1) -> Self {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("graph-publication.sqlite3");
        drop(rusqlite::Connection::open(&path).unwrap());
        let path = path.canonicalize().unwrap();
        let binding = StoreRuntimeBindingV1::new(
            shard_id,
            StoreIncarnationV1::new(3).unwrap(),
            tracedecay_store::StoreAuthorityEpochV1::new(11).unwrap(),
        );
        let locator = VerifiedStoreLocatorV1::new(
            binding.shard_id.clone(),
            StoreIncarnationV1::new(3).unwrap(),
            LocatorDigest::new(format!("sha256:{}", "a".repeat(64))).unwrap(),
        );
        let writer = PersistentWriter::start(
            ExistingWriterLocator::new(binding.clone(), locator.clone(), path.clone()).unwrap(),
            AdmissionConfigV1::default(),
            NoWrites,
        )
        .unwrap();
        let readers = ReaderPool::start(
            ExistingReaderLocator::new(binding, locator, path).unwrap(),
            AdmissionConfigV1::default().readers,
            NoReads,
        )
        .unwrap();
        let handle = ExactSqlHandle::attach(&writer, &readers).unwrap();
        handle
            .execute_batch(GRAPH_PUBLICATION_SCHEMA_V1.to_owned())
            .unwrap();
        Self {
            _directory: directory,
            _writer: writer,
            readers,
            handle,
        }
    }

    fn storage(&self) -> GraphPublicationExactSqlStorage {
        GraphPublicationExactSqlStorage::from_authorized_handle(self.handle.clone()).unwrap()
    }

    fn replay_count(&self) -> i64 {
        let rows = self
            .handle
            .query(
                ExactSqlStatement::new(
                    "SELECT COUNT(*) FROM graph_publication_replay_v1".to_owned(),
                    vec![],
                )
                .unwrap(),
                Duration::from_secs(1),
            )
            .unwrap();
        match &rows.rows[0].values[0] {
            ExactSqlValue::Integer(count) => *count,
            value => panic!("unexpected replay count value: {value:?}"),
        }
    }
}

struct Probe {
    cancellation: RuntimeCancellationIdentityV1,
    deadline: RuntimeDeadlineV1,
    interruption: Option<RuntimeInterruptionV1>,
    commit_started: AtomicBool,
}

impl RuntimeRequestProbeV1 for Probe {
    fn cancellation_identity(&self) -> &RuntimeCancellationIdentityV1 {
        &self.cancellation
    }

    fn deadline_identity(&self) -> &RuntimeDeadlineV1 {
        &self.deadline
    }

    fn interruption(&self) -> Option<RuntimeInterruptionV1> {
        self.interruption
    }

    fn try_begin_commit(&self) -> bool {
        self.interruption().is_none()
            && self
                .commit_started
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
    }
}

struct OneShotCommitProbe {
    inner: Probe,
    attempts: AtomicUsize,
}

struct DeniedCommitProbe(Probe);

impl RuntimeRequestProbeV1 for DeniedCommitProbe {
    fn cancellation_identity(&self) -> &RuntimeCancellationIdentityV1 {
        self.0.cancellation_identity()
    }

    fn deadline_identity(&self) -> &RuntimeDeadlineV1 {
        self.0.deadline_identity()
    }

    fn interruption(&self) -> Option<RuntimeInterruptionV1> {
        self.0.interruption()
    }

    fn try_begin_commit(&self) -> bool {
        false
    }
}

impl RuntimeRequestProbeV1 for OneShotCommitProbe {
    fn cancellation_identity(&self) -> &RuntimeCancellationIdentityV1 {
        self.inner.cancellation_identity()
    }

    fn deadline_identity(&self) -> &RuntimeDeadlineV1 {
        self.inner.deadline_identity()
    }

    fn interruption(&self) -> Option<RuntimeInterruptionV1> {
        self.inner.interruption()
    }

    fn try_begin_commit(&self) -> bool {
        self.attempts.fetch_add(1, Ordering::SeqCst) == 0
    }
}

fn control_and_probe(
    suffix: &str,
    interruption: Option<RuntimeInterruptionV1>,
) -> (RuntimeRequestControlV1, Probe) {
    let cancellation = RuntimeCancellationIdentityV1 {
        cancellation_id: RuntimeCancellationIdV1::new(format!("cancellation.{suffix}")).unwrap(),
        generation: 1,
    };
    let deadline = RuntimeDeadlineV1 {
        deadline_id: RuntimeDeadlineIdV1::new(format!("deadline.{suffix}")).unwrap(),
    };
    (
        RuntimeRequestControlV1 {
            requested_at: UtcMicros(1),
            deadline: deadline.clone(),
            cancellation: cancellation.clone(),
        },
        Probe {
            cancellation,
            deadline,
            interruption,
            commit_started: AtomicBool::new(false),
        },
    )
}

fn digest(byte: char) -> String {
    format!("sha256:{}", byte.to_string().repeat(64))
}

fn projection(name: &str) -> GraphProjectionIdentityV1 {
    projection_for_project("project.fixture", name)
}

fn projection_for_project(project: &str, name: &str) -> GraphProjectionIdentityV1 {
    GraphProjectionIdentityV1 {
        shard_id: tracedecay_store::StoreShardIdV1::project(
            BrainId::new("brain.fixture").unwrap(),
            UserProfileId::new("profile.fixture").unwrap(),
            ProjectId::new(project).unwrap(),
        ),
        namespace: GraphNamespaceV1::new("project").unwrap(),
        projection: GraphProjectionIdV1::new(name).unwrap(),
    }
}

fn dependency(name: &str, generation: &str) -> GraphDependencyGenerationIdentityV1 {
    GraphDependencyGenerationIdentityV1 {
        projection: projection(name),
        generation: GraphGenerationIdV1::new(generation).unwrap(),
    }
}

fn replay(
    projection: GraphProjectionIdentityV1,
    generation: &str,
    idempotency: &str,
    input_byte: char,
    recovered_byte: char,
    expected_prior_head: Option<tracedecay_store::GraphVerifiedHeadV1>,
    source: &[u8],
) -> GraphPublicationReplayV1 {
    replay_with_dependencies(
        projection,
        generation,
        idempotency,
        input_byte,
        recovered_byte,
        Vec::new(),
        expected_prior_head,
        source,
    )
}

#[allow(clippy::too_many_arguments)]
fn replay_with_dependencies(
    projection: GraphProjectionIdentityV1,
    generation: &str,
    idempotency: &str,
    input_byte: char,
    recovered_byte: char,
    direct_dependency_generations: Vec<GraphDependencyGenerationIdentityV1>,
    expected_prior_head: Option<tracedecay_store::GraphVerifiedHeadV1>,
    source: &[u8],
) -> GraphPublicationReplayV1 {
    GraphPublicationReplayV1::new(
        GraphPublicationKeyV1::new(
            projection,
            GraphGenerationIdV1::new(generation).unwrap(),
            GraphPublicationIdempotencyKeyV1::new(idempotency).unwrap(),
        ),
        GraphPublicationInputDigestV1::new(digest(input_byte)).unwrap(),
        GraphDependencyGenerationClosureDigestV1::new(digest('d')).unwrap(),
        direct_dependency_generations,
        expected_prior_head,
        GraphRecoveredGenerationDigestV1::new(digest(recovered_byte)).unwrap(),
        source.to_vec(),
    )
    .unwrap()
}

fn append_with_fresh_context(
    storage: &mut GraphPublicationExactSqlStorage,
    publication: &GraphPublicationReplayV1,
    suffix: &str,
) -> Result<GraphReplayAppendOutcomeV1, GraphPublicationStoreErrorV1> {
    let (control, probe) = control_and_probe(suffix, None);
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    storage.append_replay(publication, &context)
}

fn advance_head(
    storage: &mut GraphPublicationExactSqlStorage,
    publication: &GraphPublicationReplayV1,
) -> tracedecay_store::GraphVerifiedHeadV1 {
    let (control, probe) = control_and_probe(publication.key.generation.as_str(), None);
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    let request = GraphVerifiedHeadCompareAndSwapV1 {
        publication_key: publication.key.clone(),
        input_digest: publication.input_digest.clone(),
        dependency_generation_closure_digest: publication
            .dependency_generation_closure_digest
            .clone(),
        recovered_digest: publication.expected_recovered_digest.clone(),
        expected_prior_head: publication.expected_prior_head.clone(),
    };
    match storage
        .compare_and_swap_verified_head(&request, &context)
        .unwrap()
    {
        GraphVerifiedHeadCasOutcomeV1::Advanced(head) => head,
        outcome => panic!("unexpected CAS outcome: {outcome:?}"),
    }
}

fn retirement(publication: &GraphPublicationReplayV1) -> GraphPublicationReplayRetirementV1 {
    GraphPublicationReplayRetirementV1::new(
        publication.key.clone(),
        publication.input_digest.clone(),
        publication.dependency_generation_closure_digest.clone(),
        publication.direct_dependency_generations.clone(),
        publication.expected_prior_head.clone(),
        publication.expected_recovered_digest.clone(),
        publication.canonical_replay_source_digest.clone(),
    )
    .unwrap()
}

#[test]
fn exact_writer_rejects_foreign_owner_and_pages_through_covering_index() {
    let fixture = Fixture::new();
    let (control, probe) = control_and_probe("owner", None);
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    let foreign = replay(
        projection_for_project("project.foreign", "code"),
        "generation.foreign",
        "publish.foreign",
        'a',
        'b',
        None,
        b"foreign",
    );
    let mut storage = fixture.storage();
    assert!(matches!(
        storage.append_replay(&foreign, &context),
        Err(GraphPublicationStoreErrorV1::InvalidRequest(
            tracedecay_store::StorageRuntimeContractErrorV1::ShardMismatch { .. }
        ))
    ));

    let rows = fixture
        .handle
        .query(
            ExactSqlStatement::new(
                "EXPLAIN QUERY PLAN
                 SELECT sequence, length(canonical_replay_source)
                 FROM graph_publication_replay_v1
                 WHERE shard_id = ?1 AND namespace = ?2 AND projection = ?3
                   AND sequence > ?4
                 ORDER BY sequence ASC
                 LIMIT 1"
                    .to_owned(),
                vec![
                    ExactSqlValue::Text(
                        serde_json::to_string(&projection("code").shard_id).unwrap(),
                    ),
                    ExactSqlValue::Text("project".to_owned()),
                    ExactSqlValue::Text("code".to_owned()),
                    ExactSqlValue::Integer(0),
                ],
            )
            .unwrap(),
            Duration::from_secs(1),
        )
        .unwrap();
    assert!(rows.rows.iter().any(|row| {
        row.values.iter().any(|value| {
            matches!(
                value,
                ExactSqlValue::Text(detail)
                    if detail.contains("idx_graph_publication_replay_projection_sequence")
            )
        })
    }));
}

#[test]
fn exact_writer_append_is_idempotent_and_projection_isolated() {
    let fixture = Fixture::new();
    let (control, probe) = control_and_probe("append", None);
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    let code = projection("code");
    let sessions = projection("sessions");
    let first = replay_with_dependencies(
        code.clone(),
        "generation.1",
        "publish.1",
        'a',
        'b',
        Vec::new(),
        None,
        b"one",
    );
    let changed = replay_with_dependencies(
        code.clone(),
        "generation.1",
        "publish.1",
        'a',
        'b',
        Vec::new(),
        None,
        b"changed",
    );
    let isolated = replay(
        sessions,
        "generation.1",
        "publish.1",
        'c',
        'd',
        None,
        b"two",
    );
    let mut storage = fixture.storage();

    let appended = append_with_fresh_context(&mut storage, &first, "append.first").unwrap();
    assert!(matches!(
        append_with_fresh_context(&mut storage, &first, "append.first.replay").unwrap(),
        GraphReplayAppendOutcomeV1::ExactReplay(_)
    ));
    assert!(matches!(
        append_with_fresh_context(&mut storage, &changed, "append.changed").unwrap(),
        GraphReplayAppendOutcomeV1::Conflict { .. }
    ));
    assert!(matches!(
        append_with_fresh_context(&mut storage, &isolated, "append.isolated").unwrap(),
        GraphReplayAppendOutcomeV1::Appended(_)
    ));
    assert_eq!(
        storage.replay(&first.key, &context).unwrap(),
        match appended {
            GraphReplayAppendOutcomeV1::Appended(record) =>
                GraphPublicationReplayLookupV1::Active(record),
            outcome => panic!("unexpected append outcome: {outcome:?}"),
        }
    );
}

#[test]
fn replay_append_requires_the_atomic_commit_gate() {
    let fixture = Fixture::new();
    let (control, probe) = control_and_probe("append.commit-gate", None);
    let probe = DeniedCommitProbe(probe);
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    let publication = replay(
        projection("commit-gate"),
        "generation.1",
        "publish.1",
        'a',
        'b',
        None,
        b"commit-gate",
    );

    assert_eq!(
        fixture.storage().append_replay(&publication, &context),
        Err(GraphPublicationStoreErrorV1::Infrastructure)
    );
    assert_eq!(fixture.replay_count(), 0);
}

#[test]
fn append_and_verified_head_cas_each_require_a_fresh_commit_fence() {
    let fixture = Fixture::new();
    let (append_control, append_inner) = control_and_probe("append-fence", None);
    let append_probe = OneShotCommitProbe {
        inner: append_inner,
        attempts: AtomicUsize::new(0),
    };
    let append_context =
        GraphPublicationOperationContextV1::new(&append_control, &append_probe).unwrap();
    let (cas_control, cas_inner) = control_and_probe("cas-fence", None);
    let cas_probe = OneShotCommitProbe {
        inner: cas_inner,
        attempts: AtomicUsize::new(0),
    };
    let cas_context = GraphPublicationOperationContextV1::new(&cas_control, &cas_probe).unwrap();
    let projection = projection("code");
    let first = replay(
        projection.clone(),
        "generation.1",
        "publish.1",
        'a',
        'b',
        None,
        b"one",
    );
    let mut storage = fixture.storage();
    storage.append_replay(&first, &append_context).unwrap();
    assert_eq!(append_probe.attempts.load(Ordering::SeqCst), 1);

    let request = GraphVerifiedHeadCompareAndSwapV1 {
        publication_key: first.key.clone(),
        input_digest: first.input_digest.clone(),
        dependency_generation_closure_digest: first.dependency_generation_closure_digest.clone(),
        recovered_digest: first.expected_recovered_digest.clone(),
        expected_prior_head: None,
    };
    let first_head = match storage
        .compare_and_swap_verified_head(&request, &cas_context)
        .unwrap()
    {
        GraphVerifiedHeadCasOutcomeV1::Advanced(head) => head,
        outcome => panic!("unexpected CAS outcome: {outcome:?}"),
    };
    assert_eq!(cas_probe.attempts.load(Ordering::SeqCst), 1);
    assert!(!cas_context.try_begin_replay_retirement_commit());
    assert_eq!(
        cas_probe.attempts.load(Ordering::SeqCst),
        1,
        "context-owned fence must not delegate a second commit attempt"
    );

    let second = replay_with_dependencies(
        projection.clone(),
        "generation.2",
        "publish.2",
        'c',
        'e',
        Vec::new(),
        Some(first_head),
        b"two",
    );
    append_with_fresh_context(&mut storage, &second, "append-fence.second").unwrap();
    let (read_control, read_probe) = control_and_probe("append-fence.read", None);
    let read_context = GraphPublicationOperationContextV1::new(&read_control, &read_probe).unwrap();
    let first_page = storage
        .replay_page(
            &GraphPublicationReplayPageRequestV1::new(projection.clone(), None, 1).unwrap(),
            &read_context,
        )
        .unwrap();
    assert_eq!(first_page.records.len(), 1);
    let cursor = first_page
        .continuation
        .expect("second replay should continue");
    let second_page = storage
        .replay_page(
            &GraphPublicationReplayPageRequestV1::new(projection, Some(cursor), 1).unwrap(),
            &read_context,
        )
        .unwrap();
    assert_eq!(second_page.records.len(), 1);
    assert_eq!(second_page.records[0].publication, second);
    assert_eq!(second_page.continuation, None);
}

#[test]
fn fallback_read_releases_writer_before_verified_head_cas() {
    let fixture = Fixture::new();
    let publication = replay(
        projection("writer-only"),
        "generation.1",
        "publish.1",
        'a',
        'b',
        None,
        b"writer-only",
    );
    let mut storage = fixture.storage();
    append_with_fresh_context(&mut storage, &publication, "writer-only.append").unwrap();

    fixture.readers.begin_shutdown_drain();
    let (read_control, read_probe) = control_and_probe("writer-only.read", None);
    let read_context = GraphPublicationOperationContextV1::new(&read_control, &read_probe).unwrap();
    assert!(matches!(
        storage.replay(&publication.key, &read_context).unwrap(),
        GraphPublicationReplayLookupV1::Active(_)
    ));

    let (cas_control, cas_probe) = control_and_probe("writer-only.cas", None);
    let cas_context = GraphPublicationOperationContextV1::new(&cas_control, &cas_probe).unwrap();
    let outcome = storage
        .compare_and_swap_verified_head(
            &GraphVerifiedHeadCompareAndSwapV1 {
                publication_key: publication.key.clone(),
                input_digest: publication.input_digest.clone(),
                dependency_generation_closure_digest: publication
                    .dependency_generation_closure_digest
                    .clone(),
                recovered_digest: publication.expected_recovered_digest.clone(),
                expected_prior_head: None,
            },
            &cas_context,
        )
        .unwrap();

    assert!(matches!(
        outcome,
        GraphVerifiedHeadCasOutcomeV1::Advanced(_)
    ));
}

#[test]
fn operation_context_and_probe_each_fence_commit_to_one_shot() {
    let (control, probe) = control_and_probe("default-one-shot", None);
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();

    assert!(context.try_begin_verified_commit());
    assert!(!context.try_begin_replay_retirement_commit());
    assert!(!context.try_begin_retired_cleanup_finalize_commit());

    let second_context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    assert!(
        !second_context.try_begin_verified_commit(),
        "a second context sharing the request probe cannot reacquire its commit fence"
    );
}

#[test]
fn interrupted_exact_writer_operations_do_not_append_or_advance() {
    let fixture = Fixture::new();
    let projection = projection("code");
    let publication = replay(
        projection.clone(),
        "generation.1",
        "publish.1",
        'a',
        'b',
        None,
        b"one",
    );
    let (cancelled_control, cancelled_probe) =
        control_and_probe("cancelled", Some(RuntimeInterruptionV1::Cancelled));
    let cancelled =
        GraphPublicationOperationContextV1::new(&cancelled_control, &cancelled_probe).unwrap();
    let mut storage = fixture.storage();
    assert_eq!(
        storage.append_replay(&publication, &cancelled),
        Err(GraphPublicationStoreErrorV1::Interrupted(
            RuntimeInterruptionV1::Cancelled
        ))
    );
    assert_eq!(fixture.replay_count(), 0);

    let (control, probe) = control_and_probe("active", None);
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    storage.append_replay(&publication, &context).unwrap();
    let (expired_control, expired_probe) =
        control_and_probe("expired", Some(RuntimeInterruptionV1::DeadlineExceeded));
    let expired =
        GraphPublicationOperationContextV1::new(&expired_control, &expired_probe).unwrap();
    let request = GraphVerifiedHeadCompareAndSwapV1 {
        publication_key: publication.key.clone(),
        input_digest: publication.input_digest.clone(),
        dependency_generation_closure_digest: publication
            .dependency_generation_closure_digest
            .clone(),
        recovered_digest: publication.expected_recovered_digest.clone(),
        expected_prior_head: None,
    };
    assert_eq!(
        storage.compare_and_swap_verified_head(&request, &expired),
        Err(GraphPublicationStoreErrorV1::Interrupted(
            RuntimeInterruptionV1::DeadlineExceeded
        ))
    );
    assert_eq!(storage.verified_head(&projection, &context).unwrap(), None);
}

#[test]
fn concurrent_exact_writer_candidates_leave_one_pending_replay() {
    let fixture = Fixture::new();
    let barrier = Arc::new(Barrier::new(2));
    let append = |handle: ExactSqlHandle,
                  barrier: Arc<Barrier>,
                  generation: &'static str,
                  idempotency: &'static str| {
        std::thread::spawn(move || {
            let (control, probe) = control_and_probe(generation, None);
            let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
            let candidate = replay(
                projection("code"),
                generation,
                idempotency,
                'a',
                'b',
                None,
                generation.as_bytes(),
            );
            barrier.wait();
            GraphPublicationExactSqlStorage::from_authorized_handle(handle)
                .unwrap()
                .append_replay(&candidate, &context)
                .unwrap()
        })
    };
    let left = append(
        fixture.handle.clone(),
        Arc::clone(&barrier),
        "generation.left",
        "publish.left",
    );
    let right = append(
        fixture.handle.clone(),
        barrier,
        "generation.right",
        "publish.right",
    );
    let outcomes = [left.join().unwrap(), right.join().unwrap()];
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| matches!(outcome, GraphReplayAppendOutcomeV1::Appended(_)))
            .count(),
        1
    );
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| matches!(
                outcome,
                GraphReplayAppendOutcomeV1::PendingReplayConflict { .. }
            ))
            .count(),
        1
    );
    assert_eq!(fixture.replay_count(), 1);
}

#[test]
fn historical_retirement_refuses_current_and_pending_then_tombstones_exactly() {
    let fixture = Fixture::new();
    let (control, probe) = control_and_probe("retire", None);
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    let projection = projection("code");
    let first = replay(
        projection.clone(),
        "generation.1",
        "publish.1",
        'a',
        'b',
        None,
        b"one",
    );
    let mut storage = fixture.storage();
    append_with_fresh_context(&mut storage, &first, "retire.first").unwrap();
    let first_head = advance_head(&mut storage, &first);
    let second = replay(
        projection.clone(),
        "generation.2",
        "publish.2",
        'c',
        'e',
        Some(first_head),
        b"two",
    );
    append_with_fresh_context(&mut storage, &second, "retire.second").unwrap();
    let second_head = advance_head(&mut storage, &second);
    let pending = replay(
        projection,
        "generation.3",
        "publish.3",
        'f',
        'a',
        Some(second_head),
        b"three",
    );
    append_with_fresh_context(&mut storage, &pending, "retire.pending").unwrap();

    assert!(matches!(
        storage
            .retire_replay(&retirement(&second), &context)
            .unwrap(),
        GraphReplayRetirementOutcomeV1::CurrentVerifiedHead { .. }
    ));
    assert!(matches!(
        storage
            .retire_replay(&retirement(&pending), &context)
            .unwrap(),
        GraphReplayRetirementOutcomeV1::PendingReplay { .. }
    ));
    let tombstone = match storage
        .retire_replay(&retirement(&first), &context)
        .unwrap()
    {
        GraphReplayRetirementOutcomeV1::Retired(tombstone) => tombstone,
        outcome => panic!("unexpected retirement outcome: {outcome:?}"),
    };
    assert_eq!(
        storage.replay(&first.key, &context).unwrap(),
        GraphPublicationReplayLookupV1::Retired(tombstone.clone())
    );
    assert_eq!(
        storage
            .retire_replay(&retirement(&first), &context)
            .unwrap(),
        GraphReplayRetirementOutcomeV1::ExactReplay(tombstone)
    );
}

#[test]
fn retirement_rejects_changed_evidence_and_interruption_without_deleting_replay() {
    let fixture = Fixture::new();
    let (control, probe) = control_and_probe("retire-evidence", None);
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    let projection = projection("code");
    let first = replay_with_dependencies(
        projection.clone(),
        "generation.1",
        "publish.1",
        'a',
        'b',
        Vec::new(),
        None,
        b"one",
    );
    let mut storage = fixture.storage();
    append_with_fresh_context(&mut storage, &first, "retire-evidence.first").unwrap();
    let first_head = advance_head(&mut storage, &first);
    let second = replay(
        projection,
        "generation.2",
        "publish.2",
        'c',
        'e',
        Some(first_head),
        b"two",
    );
    append_with_fresh_context(&mut storage, &second, "retire-evidence.second").unwrap();
    let second_head = advance_head(&mut storage, &second);

    let mut changed = retirement(&first);
    changed.input_digest = GraphPublicationInputDigestV1::new(digest('f')).unwrap();
    assert_eq!(
        storage.retire_replay(&changed, &context).unwrap(),
        GraphReplayRetirementOutcomeV1::Conflict
    );
    let mut changed_key = retirement(&first);
    changed_key.key.idempotency_key =
        GraphPublicationIdempotencyKeyV1::new("publish.changed").unwrap();
    assert_eq!(
        storage.retire_replay(&changed_key, &context).unwrap(),
        GraphReplayRetirementOutcomeV1::Conflict
    );
    let mut changed_source = retirement(&first);
    changed_source.canonical_replay_source_digest =
        tracedecay_store::GraphCanonicalReplaySourceDigestV1::for_source(b"changed");
    assert_eq!(
        storage.retire_replay(&changed_source, &context).unwrap(),
        GraphReplayRetirementOutcomeV1::Conflict
    );
    let mut changed_recovered = retirement(&first);
    changed_recovered.expected_recovered_digest =
        GraphRecoveredGenerationDigestV1::new(digest('f')).unwrap();
    assert_eq!(
        storage.retire_replay(&changed_recovered, &context).unwrap(),
        GraphReplayRetirementOutcomeV1::Conflict
    );
    let mut changed_prior = retirement(&first);
    changed_prior.expected_prior_head = Some(second_head);
    assert_eq!(
        storage.retire_replay(&changed_prior, &context).unwrap(),
        GraphReplayRetirementOutcomeV1::Conflict
    );
    let (expired_control, expired_probe) = control_and_probe(
        "retire-expired",
        Some(RuntimeInterruptionV1::DeadlineExceeded),
    );
    let expired =
        GraphPublicationOperationContextV1::new(&expired_control, &expired_probe).unwrap();
    assert_eq!(
        storage.retire_replay(&retirement(&first), &expired),
        Err(GraphPublicationStoreErrorV1::Interrupted(
            RuntimeInterruptionV1::DeadlineExceeded
        ))
    );
    assert!(matches!(
        storage.replay(&first.key, &context).unwrap(),
        GraphPublicationReplayLookupV1::Active(_)
    ));
}

#[test]
fn project_shard_projection_inventory_uses_bounded_keyset_pages() {
    let fixture = Fixture::new();
    let (control, probe) = control_and_probe("projection-page", None);
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    let mut storage = fixture.storage();
    for name in ["sessions", "ast", "code"] {
        let publication = replay(
            projection(name),
            "generation.1",
            "publish.1",
            'a',
            'b',
            None,
            name.as_bytes(),
        );
        append_with_fresh_context(&mut storage, &publication, name).unwrap();
    }
    let shard_id = projection("code").shard_id;
    let first = storage
        .projection_page(
            &GraphPublicationProjectionPageRequestV1::new(shard_id.clone(), None, 2).unwrap(),
            &context,
        )
        .unwrap();
    assert_eq!(
        first.projections,
        vec![projection("ast"), projection("code")]
    );
    let continuation = first.continuation.expect("sessions remains");
    let second = storage
        .projection_page(
            &GraphPublicationProjectionPageRequestV1::new(shard_id, Some(continuation), 2).unwrap(),
            &context,
        )
        .unwrap();
    assert_eq!(second.projections, vec![projection("sessions")]);
    assert_eq!(second.continuation, None);
}

#[path = "tests/relational.rs"]
mod relational;
#[path = "tests/scope.rs"]
mod scope;
