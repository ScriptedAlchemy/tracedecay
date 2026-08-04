use std::{error::Error, fmt::Debug};

use rusqlite::Connection;
use tempfile::TempDir;
use tracedecay_domain::{
    BrainId, CodeGenerationId, LocatorDigest, ProjectId, UserProfileId, UtcMicros,
};
use tracedecay_rusqlite_runtime::repository::{
    ConcreteRepositoryWriteExecutor, RepositoryAttachmentStartError,
    RepositoryPhysicalAttachmentFactory,
};
use tracedecay_rusqlite_runtime::{OpenedDatabaseFileError, StorageOperationExecutor};
use tracedecay_store::{
    AdmissionConfigV1, ConsistencyModeV1, DiagnosticReadOperationV1, DiagnosticReadResultV1,
    OperationPriorityV1, ProjectReadOperationV1, ProjectReadResultV1, RepositoryReadOperationV1,
    RepositoryReadResultV1, RepositoryWritePayloadV1, RuntimeCancellationIdV1,
    RuntimeCancellationIdentityV1, RuntimeDeadlineIdV1, RuntimeDeadlineV1, RuntimeReadOperationV1,
    RuntimeReadRequestV1, RuntimeReadResultV1, RuntimeRequestControlV1, RuntimeRequestProbeV1,
    SanitizedCleanDiagnosticSnapshotV1, StoreIncarnationV1, StoreRuntimeBindingV1, StoreShardIdV1,
    VerifiedStoreLocatorV1,
};

/// Minimal canonical schema for the diagnostic family, mirroring the
/// migration-owned tables the executors read and write.
const DIAGNOSTIC_SCHEMA: &str = "
    CREATE TABLE generation_diagnostics (
        diagnostic_anchor TEXT PRIMARY KEY,
        generation_id TEXT NOT NULL,
        repository TEXT NOT NULL,
        worktree TEXT,
        reference TEXT,
        source_revision TEXT,
        file_occurrence_id TEXT NOT NULL,
        content_digest TEXT NOT NULL,
        symbol_occurrence_id TEXT,
        span_start INTEGER NOT NULL,
        span_end INTEGER NOT NULL,
        code TEXT NOT NULL,
        severity TEXT NOT NULL,
        message TEXT NOT NULL,
        message_digest TEXT NOT NULL,
        producer_kind TEXT NOT NULL,
        producer TEXT NOT NULL,
        analyzer_revision TEXT NOT NULL,
        configuration_revision TEXT NOT NULL,
        sanitization_receipt TEXT,
        evidence_class TEXT NOT NULL,
        collected_at INTEGER NOT NULL,
        record_state TEXT NOT NULL DEFAULT 'current',
        state_generation TEXT,
        persisted_at INTEGER NOT NULL DEFAULT 0
    );
    CREATE TABLE diagnostic_generation_publications (
        generation_id TEXT PRIMARY KEY,
        record_state TEXT NOT NULL,
        state_generation TEXT,
        published_at INTEGER NOT NULL
    );
";

fn id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    <T as TryFrom<String>>::Error: Debug,
{
    T::try_from(value.to_owned()).unwrap()
}

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
}

fn health_request(binding: StoreRuntimeBindingV1) -> (RuntimeReadRequestV1, Probe) {
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
            ConsistencyModeV1::LatestAvailable,
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
fn repository_attachment_identity_error_preserves_the_public_source() {
    let source = OpenedDatabaseFileError::Open;
    let repository = RepositoryAttachmentStartError::Identity(source);

    assert!(repository.source().is_some());
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
        .attach(binding.clone(), locator, path, AdmissionConfigV1::default())
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
        .attach(binding.clone(), locator, path, AdmissionConfigV1::default())
        .unwrap();
    let (request, probe) = health_request(binding);

    let outcome = attachment.dispatch_read(request, &probe).unwrap();

    assert!(matches!(
        outcome.value(),
        Some(RuntimeReadResultV1::TemporalHealth { healthy: true })
    ));
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
    connection.execute_batch(DIAGNOSTIC_SCHEMA).unwrap();
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
        .attach(binding.clone(), locator, path, AdmissionConfigV1::default())
        .unwrap();

    let (request, probe) = repository_read_request(
        binding,
        RepositoryReadOperationV1::Project(ProjectReadOperationV1::Diagnostics(
            DiagnosticReadOperationV1::CurrentGeneration,
        )),
    );

    let outcome = attachment.dispatch_read(request, &probe).unwrap();

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
