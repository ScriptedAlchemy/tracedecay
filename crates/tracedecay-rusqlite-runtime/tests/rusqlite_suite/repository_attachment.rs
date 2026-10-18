use std::sync::Arc;

use rusqlite::Connection;
use tempfile::TempDir;
use tracedecay_domain::{
    BrainId, CodeGenerationId, LocatorDigest, ProjectId, UserProfileId, UtcMicros,
};
use tracedecay_rusqlite_runtime::StorageOperationExecutor;
use tracedecay_rusqlite_runtime::native_runtime;
use tracedecay_rusqlite_runtime::repository::{
    ConcreteRepositoryWriteExecutor, RepositoryPhysicalAttachmentFactory,
    RepositoryRuntimePhysicalAttachment,
};
use tracedecay_store::{
    AdmissionConfigV1, CommitSequenceV1, ConsistencyModeV1, DiagnosticReadOperationV1,
    DiagnosticReadResultV1, GENERATION_DIAGNOSTICS_SCHEMA_DDL, OperationPriorityV1,
    ProjectReadOperationV1, ProjectReadResultV1, RepositoryReadOperationV1, RepositoryReadResultV1,
    RepositoryWritePayloadV1, RuntimeCancellationIdV1, RuntimeCancellationIdentityV1,
    RuntimeDeadlineIdV1, RuntimeDeadlineV1, RuntimeReadCoverageV1, RuntimeReadOperationV1,
    RuntimeReadOutcomeV1, RuntimeReadRequestV1, RuntimeReadResultV1, RuntimeRequestControlV1,
    RuntimeRequestProbeV1, SanitizedCleanDiagnosticSnapshotV1, StoreIncarnationV1,
    StoreRuntimeBindingV1, StoreShardIdV1, UnavailableReasonV1, VerifiedStoreLocatorV1,
    single_shard_required_coverage_v1,
};

use tracedecay_domain::test_fixtures::id;

fn binding() -> StoreRuntimeBindingV1 {
    serde_json::from_value(serde_json::json!({
        "shard_id": StoreShardIdV1::project(
            id::<BrainId>("brain.repository-attachment"),
            id::<UserProfileId>("profile.repository-attachment"),
            id::<ProjectId>("project.repository-attachment"),
        ),
        "incarnation": 1,
        "authority_epoch": 1
    }))
    .unwrap()
}

struct Probe {
    cancellation: RuntimeCancellationIdentityV1,
    deadline: RuntimeDeadlineV1,
}

/// Drives one typed read from this plain test thread, which owns no native
/// runtime, the way a CLI thread would.
fn read(
    attachment: &RepositoryRuntimePhysicalAttachment,
    request: RuntimeReadRequestV1,
    probe: Probe,
) -> RuntimeReadOutcomeV1 {
    native_runtime::block_on(attachment.dispatch_read(request, Arc::new(probe)))
        .expect("a plain test thread may block on the native runtime")
        .expect("typed read")
}

impl RuntimeRequestProbeV1 for Probe {
    fn cancellation_identity(&self) -> &RuntimeCancellationIdentityV1 {
        &self.cancellation
    }

    fn deadline_identity(&self) -> &RuntimeDeadlineV1 {
        &self.deadline
    }

    fn interruption(&self) -> Option<tracedecay_store::RuntimeInterruptionV1> {
        None
    }

    fn try_begin_commit(&self) -> bool {
        false
    }
}

fn health_request(
    binding: StoreRuntimeBindingV1,
    consistency: ConsistencyModeV1,
) -> (RuntimeReadRequestV1, Probe) {
    let cancellation = RuntimeCancellationIdentityV1 {
        cancellation_id: RuntimeCancellationIdV1::new("cancel.repository-health").unwrap(),
        generation: 1,
    };
    let deadline = RuntimeDeadlineV1 {
        deadline_id: RuntimeDeadlineIdV1::new("deadline.repository-health").unwrap(),
    };
    let control = RuntimeRequestControlV1 {
        requested_at: UtcMicros(1),
        deadline: deadline.clone(),
        cancellation: cancellation.clone(),
    };
    (
        RuntimeReadRequestV1::new(
            binding,
            consistency,
            RuntimeReadOperationV1::TemporalHealth,
            OperationPriorityV1::Health,
            1,
            control,
        )
        .unwrap(),
        Probe {
            cancellation,
            deadline,
        },
    )
}

#[test]
fn repository_factory_attaches_writer_and_reserved_reader_runtime() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("project.db");
    Connection::open(&path).unwrap();
    let path = path.canonicalize().unwrap();
    let binding = binding();
    let locator = VerifiedStoreLocatorV1::new(
        binding.shard_id.clone(),
        StoreIncarnationV1::new(1).unwrap(),
        LocatorDigest::new(format!("sha256:{}", "a".repeat(64))).unwrap(),
    );

    let attachment = RepositoryPhysicalAttachmentFactory
        .attach(
            &tracedecay_rusqlite_runtime::native_runtime::handle().expect("native runtime"),
            binding.clone(),
            locator,
            path,
            AdmissionConfigV1::default(),
        )
        .unwrap();

    assert_eq!(attachment.binding(), binding);
    let snapshot = attachment.snapshot();
    assert!(snapshot.healthy);
    assert!(snapshot.writer_present);
    assert_eq!(snapshot.reader_handles, 3);

    attachment.drain().unwrap();
    assert!(attachment.snapshot().is_drained());
    attachment.close_and_join().unwrap();
}

/// A typed read awaited on a native worker must leave that worker free: the
/// read runs as a job, and the worker only awaits its reply. A read that
/// waited on reader admission or a reader reply on the worker itself is
/// counted in `native_thread_reader_waits`.
#[test]
fn a_typed_read_awaited_on_a_native_worker_parks_no_native_thread() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("sessions.db");
    Connection::open(&path).unwrap();
    let path = path.canonicalize().unwrap();
    let binding = binding();
    let locator = VerifiedStoreLocatorV1::new(
        binding.shard_id.clone(),
        StoreIncarnationV1::new(1).unwrap(),
        LocatorDigest::new(format!("sha256:{}", "f".repeat(64))).unwrap(),
    );
    let attachment = Arc::new(
        RepositoryPhysicalAttachmentFactory
            .attach(
                &native_runtime::handle().expect("native runtime"),
                binding.clone(),
                locator,
                path,
                AdmissionConfigV1::default(),
            )
            .unwrap(),
    );
    let (request, probe) = health_request(binding, ConsistencyModeV1::LatestAvailable);
    let waits_before = tracedecay_rusqlite_runtime::exact_sql::native_thread_reader_waits();

    let reader = Arc::clone(&attachment);
    let task = native_runtime::NativeTask::spawn(async move {
        reader
            .dispatch_read(request, Arc::new(probe))
            .await
            .map(|outcome| {
                matches!(
                    outcome.value(),
                    Some(RuntimeReadResultV1::TemporalHealth { healthy: true })
                )
            })
            .map_err(|error| error.to_string())
    })
    .unwrap();
    let outcome = native_runtime::block_on(task.wait()).unwrap();

    assert!(
        matches!(
            outcome,
            native_runtime::NativeTaskOutcome::Completed(Ok(true))
        ),
        "the read must answer on the native worker"
    );
    assert_eq!(
        tracedecay_rusqlite_runtime::exact_sql::native_thread_reader_waits() - waits_before,
        0,
        "no native worker may park on a typed read"
    );
    attachment.drain().unwrap();
    attachment.close_and_join().unwrap();
}

#[test]
fn temporal_health_dispatch_uses_the_reserved_reader_lane() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("sessions.db");
    Connection::open(&path).unwrap();
    let path = path.canonicalize().unwrap();
    let binding = binding();
    let locator = VerifiedStoreLocatorV1::new(
        binding.shard_id.clone(),
        StoreIncarnationV1::new(1).unwrap(),
        LocatorDigest::new(format!("sha256:{}", "b".repeat(64))).unwrap(),
    );
    let attachment = RepositoryPhysicalAttachmentFactory
        .attach(
            &tracedecay_rusqlite_runtime::native_runtime::handle().expect("native runtime"),
            binding.clone(),
            locator,
            path,
            AdmissionConfigV1::default(),
        )
        .unwrap();
    let (request, probe) = health_request(binding, ConsistencyModeV1::LatestAvailable);

    let outcome = read(&attachment, request, probe);

    assert!(matches!(
        outcome.value(),
        Some(RuntimeReadResultV1::TemporalHealth { healthy: true })
    ));
    attachment.drain().unwrap();
    attachment.close_and_join().unwrap();
}

#[test]
fn at_least_reads_refuse_coverage_without_a_committed_watermark() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("sessions.db");
    Connection::open(&path).unwrap();
    let path = path.canonicalize().unwrap();
    let binding = binding();
    let locator = VerifiedStoreLocatorV1::new(
        binding.shard_id.clone(),
        StoreIncarnationV1::new(1).unwrap(),
        LocatorDigest::new(format!("sha256:{}", "e".repeat(64))).unwrap(),
    );
    let attachment = RepositoryPhysicalAttachmentFactory
        .attach(
            &tracedecay_rusqlite_runtime::native_runtime::handle().expect("native runtime"),
            binding.clone(),
            locator,
            path,
            AdmissionConfigV1::default(),
        )
        .unwrap();
    let commit_sequence = CommitSequenceV1(7);
    let (request, probe) = health_request(
        binding.clone(),
        ConsistencyModeV1::AtLeast { commit_sequence },
    );

    let outcome = read(&attachment, request.clone(), probe);

    assert!(
        outcome.value().is_none(),
        "an uncommitted snapshot must not serve a value"
    );
    assert_eq!(
        outcome.coverage(),
        &RuntimeReadCoverageV1::Unavailable {
            coverage: Some(
                single_shard_required_coverage_v1(&binding, commit_sequence, []).unwrap()
            ),
            reason: UnavailableReasonV1::MissingAuthority,
        }
    );
    assert_eq!(outcome.validate_for(&request), Ok(()));
    attachment.drain().unwrap();
    attachment.close_and_join().unwrap();
}

fn repository_read_request(
    binding: StoreRuntimeBindingV1,
    op: RepositoryReadOperationV1,
) -> (RuntimeReadRequestV1, Probe) {
    let cancellation = RuntimeCancellationIdentityV1 {
        cancellation_id: RuntimeCancellationIdV1::new("cancel.repository-read").unwrap(),
        generation: 1,
    };
    let deadline = RuntimeDeadlineV1 {
        deadline_id: RuntimeDeadlineIdV1::new("deadline.repository-read").unwrap(),
    };
    let control = RuntimeRequestControlV1 {
        requested_at: UtcMicros(1),
        deadline: deadline.clone(),
        cancellation: cancellation.clone(),
    };
    (
        RuntimeReadRequestV1::new(
            binding,
            ConsistencyModeV1::LatestAvailable,
            RuntimeReadOperationV1::Repository { op },
            OperationPriorityV1::Foreground,
            1,
            control,
        )
        .unwrap(),
        Probe {
            cancellation,
            deadline,
        },
    )
}

#[test]
fn repository_read_dispatch_routes_to_the_repository_executor() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("project.db");

    // Seed a current diagnostic generation through the real repository write
    // executor, then reopen the file through the runtime attachment.
    let generation = CodeGenerationId::new("generation.repository-read").unwrap();
    let mut connection = Connection::open(&path).unwrap();
    connection
        .execute_batch(GENERATION_DIAGNOSTICS_SCHEMA_DDL)
        .unwrap();
    {
        let savepoint = connection.savepoint().unwrap();
        let snapshot =
            SanitizedCleanDiagnosticSnapshotV1::new(generation.clone(), Vec::new()).unwrap();
        ConcreteRepositoryWriteExecutor::default()
            .execute(
                &savepoint,
                &RepositoryWritePayloadV1::Diagnostics(Box::new(snapshot)),
            )
            .unwrap();
        savepoint.commit().unwrap();
    }
    drop(connection);

    let path = path.canonicalize().unwrap();
    let binding = binding();
    let locator = VerifiedStoreLocatorV1::new(
        binding.shard_id.clone(),
        StoreIncarnationV1::new(1).unwrap(),
        LocatorDigest::new(format!("sha256:{}", "c".repeat(64))).unwrap(),
    );
    let attachment = RepositoryPhysicalAttachmentFactory
        .attach(
            &tracedecay_rusqlite_runtime::native_runtime::handle().expect("native runtime"),
            binding.clone(),
            locator,
            path,
            AdmissionConfigV1::default(),
        )
        .unwrap();

    let (request, probe) = repository_read_request(
        binding,
        RepositoryReadOperationV1::Project(ProjectReadOperationV1::Diagnostics(
            DiagnosticReadOperationV1::CurrentGeneration,
        )),
    );

    let outcome = read(&attachment, request, probe);

    match outcome.value() {
        Some(RuntimeReadResultV1::Repository {
            result: RepositoryReadResultV1::Project(project),
        }) => match project.as_ref() {
            ProjectReadResultV1::Diagnostics(DiagnosticReadResultV1::CurrentGeneration(Some(
                observed,
            ))) => assert_eq!(observed, &generation),
            other => panic!("unexpected project read result: {other:?}"),
        },
        other => panic!("unexpected repository read outcome: {other:?}"),
    }

    attachment.drain().unwrap();
    attachment.close_and_join().unwrap();
}
