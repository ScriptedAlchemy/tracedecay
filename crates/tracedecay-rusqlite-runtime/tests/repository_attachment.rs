use std::fmt::Debug;

use rusqlite::Connection;
use tempfile::TempDir;
use tracedecay_domain::{BrainId, LocatorDigest, ProjectId, UserProfileId, UtcMicros};
use tracedecay_rusqlite_runtime::repository::RepositoryPhysicalAttachmentFactory;
use tracedecay_store::{
    AdmissionConfigV1, ConsistencyModeV1, OperationPriorityV1, RuntimeCancellationIdV1,
    RuntimeCancellationIdentityV1, RuntimeDeadlineIdV1, RuntimeDeadlineV1, RuntimeReadOperationV1,
    RuntimeReadRequestV1, RuntimeReadResultV1, RuntimeRequestControlV1, RuntimeRequestProbeV1,
    StoreIncarnationV1, StoreRuntimeBindingV1, StoreShardIdV1, VerifiedStoreLocatorV1,
};

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
