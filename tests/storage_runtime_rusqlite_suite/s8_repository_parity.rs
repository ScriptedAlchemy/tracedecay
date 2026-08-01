use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Debug;

use rusqlite::Connection;
use tempfile::TempDir;
use tracedecay_domain::{BrainId, LocatorDigest, ProjectId, UserProfileId, UtcMicros};
use tracedecay_rusqlite_runtime::repository::{
    PRE_CUTOVER_ADAPTER_PARITY_FIXTURES_V1, PreCutoverRepositoryAttachmentBundle,
    RepositoryPhysicalAttachmentFactory,
};
use tracedecay_store::{
    AdmissionConfigV1, ConsistencyModeV1, OperationPriorityV1, RuntimeCancellationIdV1,
    RuntimeCancellationIdentityV1, RuntimeDeadlineIdV1, RuntimeDeadlineV1, RuntimeReadOperationV1,
    RuntimeReadRequestV1, RuntimeReadResultV1, RuntimeRequestControlV1, RuntimeRequestProbeV1,
    StoreIncarnationV1, StoreRuntimeBindingV1, StoreShardIdV1, VerifiedStoreLocatorV1,
};

use crate::cutover_support::fixture;

fn id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    <T as TryFrom<String>>::Error: Debug,
{
    T::try_from(value.to_owned()).unwrap()
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
        cancellation_id: RuntimeCancellationIdV1::new("cancel.s8-family-health").unwrap(),
        generation: 1,
    };
    let deadline = RuntimeDeadlineV1 {
        deadline_id: RuntimeDeadlineIdV1::new("deadline.s8-family-health").unwrap(),
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

fn assert_family_mount(binding: StoreRuntimeBindingV1, path: &std::path::Path) {
    let locator = VerifiedStoreLocatorV1::new(
        binding.shard_id.clone(),
        StoreIncarnationV1::new(1).unwrap(),
        LocatorDigest::new(format!("sha256:{}", "e".repeat(64))).unwrap(),
    );
    let attachment = RepositoryPhysicalAttachmentFactory
        .attach(
            binding.clone(),
            locator,
            path.to_path_buf(),
            AdmissionConfigV1::default(),
        )
        .expect("mount repository family runtime");
    let snapshot = attachment.snapshot();
    assert!(snapshot.healthy);
    assert!(snapshot.writer_present);
    assert_eq!(snapshot.reader_handles, 3);

    let (request, probe) = health_request(binding);
    let outcome = attachment.dispatch_read(request, &probe).unwrap();
    assert!(matches!(
        outcome.value(),
        Some(RuntimeReadResultV1::TemporalHealth { healthy: true })
    ));
    attachment.drain().unwrap();
    attachment.close_and_join().unwrap();
}

#[test]
fn profile_project_and_session_fixture_matches_the_public_attachment_catalog() {
    let expected = fixture()
        .s8
        .families
        .into_iter()
        .map(|family| (family.family.clone(), family))
        .collect::<BTreeMap<_, _>>();
    let observed = PRE_CUTOVER_ADAPTER_PARITY_FIXTURES_V1
        .iter()
        .map(|family| (family.family.to_owned(), family))
        .collect::<BTreeMap<_, _>>();

    assert_eq!(
        observed.keys().cloned().collect::<Vec<_>>(),
        expected.keys().cloned().collect::<Vec<_>>(),
        "public repository families must exactly match the S8 route fixture"
    );
    // The fixture pins the family partition, not the route inventory: production
    // legitimately grows write payloads and read operations within a family
    // (for example the diagnostic supersession routes) without changing which
    // families exist. Ordered vector equality against the fixture would freeze
    // that inventory, so the per-route coverage assertion lives in
    // `tests/storage_runtime_s8_cutover.rs::s8_parity_inventory_covers_every_declared_route`,
    // which checks the declared routes are a subset of the published inventory.
    for name in expected.into_keys() {
        let observed = observed
            .get(&name)
            .unwrap_or_else(|| panic!("missing public repository family {name:?}"));
        assert!(
            !observed.canonical_tables.is_empty(),
            "{name} must bind to migration-owned canonical tables"
        );
        assert_eq!(
            observed
                .canonical_tables
                .iter()
                .copied()
                .collect::<BTreeSet<_>>()
                .len(),
            observed.canonical_tables.len(),
            "{name} canonical table inventory must not contain duplicates"
        );
    }

    let _bundle = PreCutoverRepositoryAttachmentBundle::new();
}

#[test]
fn profile_project_and_session_production_mounts_serve_health_data_ports() {
    let directory = TempDir::new().unwrap();
    let families = [
        (
            "profile",
            serde_json::from_value(serde_json::json!({
                "shard_id": StoreShardIdV1::profile(
                    id::<BrainId>("brain.s8-profile"),
                    id::<UserProfileId>("profile.s8"),
                ),
                "incarnation": 1,
                "authority_epoch": 1
            }))
            .unwrap(),
        ),
        (
            "project",
            serde_json::from_value(serde_json::json!({
                "shard_id": StoreShardIdV1::project(
                    id::<BrainId>("brain.s8-project"),
                    id::<UserProfileId>("profile.s8"),
                    id::<ProjectId>("project.s8"),
                ),
                "incarnation": 1,
                "authority_epoch": 1
            }))
            .unwrap(),
        ),
        (
            "sessions",
            serde_json::from_value(serde_json::json!({
                "shard_id": StoreShardIdV1::project_sessions(
                    id::<BrainId>("brain.s8-sessions"),
                    id::<UserProfileId>("profile.s8"),
                    id::<ProjectId>("project.s8"),
                ),
                "incarnation": 1,
                "authority_epoch": 1
            }))
            .unwrap(),
        ),
    ];

    for (family, binding) in families {
        let path = directory.path().join(format!("{family}.db"));
        Connection::open(&path).unwrap();
        let path = path.canonicalize().unwrap();
        assert_family_mount(binding, &path);
    }
}
