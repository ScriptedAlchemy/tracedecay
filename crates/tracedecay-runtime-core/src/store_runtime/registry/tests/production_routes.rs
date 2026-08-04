//! Behavioral production route coverage: profile/project/session shards
//! mount through [`LifecycleShardRuntimePublisher`] and serve health over the
//! reserved reader data port.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use rusqlite::Connection;
use tempfile::TempDir;
use tracedecay_domain::{BrainId, LocatorDigest, ProjectId, UserProfileId, UtcMicros};
use tracedecay_store::{
    ConsistencyModeV1, OperationPriorityV1, RuntimeCancellationIdV1, RuntimeCancellationIdentityV1,
    RuntimeDeadlineIdV1, RuntimeDeadlineV1, RuntimeReadOperationV1, RuntimeReadRequestV1,
    RuntimeReadResultV1, RuntimeRequestControlV1, RuntimeRequestProbeV1, StorageRuntimeReadPort,
    StoreShardIdV1, StoreShardScopeV1, VerifiedStoreLocatorV1,
};

use super::super::*;
use super::support::{id, incarnation, open_published, profile_shard, project_request};

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

#[derive(Default)]
struct FileResolver {
    roots: Mutex<Vec<PathBuf>>,
    calls: AtomicUsize,
}

impl FileResolver {
    fn push(&self, path: PathBuf) {
        self.roots.lock().unwrap().push(path);
    }
}

impl StoreRuntimeResolver for FileResolver {
    fn resolve<'a>(
        &'a self,
        key: &'a StoreRuntimeKey,
        _mode: StoreRuntimeOpenMode,
        _database_authority: Option<&'a crate::db::DatabaseAuthority>,
    ) -> StoreRuntimeRegistryFuture<'a, Result<ResolvedStoreLocator, StoreRuntimeRegistryFailure>>
    {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let path = self
            .roots
            .lock()
            .unwrap()
            .get(call)
            .cloned()
            .unwrap_or_else(|| PathBuf::from("/missing-runtime-route.db"));
        let locator = VerifiedStoreLocatorV1::new(
            key.shard_id.clone(),
            key.incarnation,
            LocatorDigest::new(format!("sha256:{}", "c".repeat(64))).unwrap(),
        );
        Box::pin(async move { Ok(ResolvedStoreLocator::new(locator, path)) })
    }
}

fn health_request(
    binding: &tracedecay_store::StoreRuntimeBindingV1,
) -> (RuntimeReadRequestV1, Probe) {
    let cancellation = RuntimeCancellationIdentityV1 {
        cancellation_id: RuntimeCancellationIdV1::new("cancel.runtime-production-health").unwrap(),
        generation: 1,
    };
    let deadline = RuntimeDeadlineV1 {
        deadline_id: RuntimeDeadlineIdV1::new("deadline.runtime-production-health").unwrap(),
    };
    let control = RuntimeRequestControlV1 {
        requested_at: UtcMicros(1),
        deadline: deadline.clone(),
        cancellation: cancellation.clone(),
    };
    (
        RuntimeReadRequestV1::new(
            binding.clone(),
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

fn seed_db(root: &TempDir, name: &str) -> PathBuf {
    let path = root.path().join(name);
    Connection::open(&path).unwrap();
    path.canonicalize().unwrap()
}

fn sessions_request(project: &str, pin: &ProfileAuthorityPin) -> StoreRuntimeOpenRequest {
    StoreRuntimeOpenRequest::new(
        StoreShardIdV1::project_sessions(
            id::<BrainId>("brain.registry"),
            id::<UserProfileId>("profile.registry"),
            id::<ProjectId>(project),
        ),
        incarnation(),
        Some(pin.clone()),
    )
}

async fn assert_health_route(handle: &StoreRuntimeHandle) {
    assert!(
        !matches!(
            handle.binding().shard_id.scope,
            StoreShardScopeV1::Code { .. }
        ),
        "repository routes must not mount code-shard executors"
    );
    let snapshot = handle.physical_snapshot();
    assert!(snapshot.healthy, "mounted runtime must report healthy");
    assert!(
        snapshot.writer_present,
        "mounted runtime must retain a writer"
    );
    assert!(
        snapshot.reader_handles >= 1,
        "mounted runtime must retain reserved readers"
    );

    let (request, probe) = health_request(handle.binding());
    let outcome = StorageRuntimeReadPort::read(handle, request, &probe)
        .await
        .expect("health data port must be mounted");
    assert!(matches!(
        outcome.value(),
        Some(RuntimeReadResultV1::TemporalHealth { healthy: true })
    ));
}

#[tokio::test]
async fn lifecycle_publisher_mounts_profile_project_and_session_health_routes() {
    let root = TempDir::new().unwrap();
    let resolver = Arc::new(FileResolver::default());
    resolver.push(seed_db(&root, "profile.db"));
    resolver.push(seed_db(&root, "project.db"));
    resolver.push(seed_db(&root, "sessions.db"));

    let registry = StoreRuntimeRegistry::with_config(
        resolver,
        Arc::new(LifecycleShardRuntimePublisher),
        StoreRuntimeRegistryConfig::new(2).unwrap(),
    )
    .unwrap();

    let profile = open_published(
        &registry,
        StoreRuntimeOpenRequest::new(profile_shard(), incarnation(), None),
    )
    .await;
    let pin = match registry.profile_authority_pin(&profile_shard()) {
        ProfileAuthorityPinResult::Pinned(pin) => pin,
        other => panic!("profile was not pinned: {other:?}"),
    };
    let project = open_published(&registry, project_request("project.runtime-route", &pin)).await;
    let sessions = open_published(&registry, sessions_request("project.runtime-route", &pin)).await;

    assert_health_route(&profile).await;
    assert_health_route(&project).await;
    assert_health_route(&sessions).await;
}
