use std::fmt::Debug;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use tracedecay_domain::{
    BrainId, LocatorDigest, ProjectId, RepositoryId, UserProfileId, UtcMicros, WorktreeId,
};
use tracedecay_store::{
    CodeShardScopeV1, RuntimeLeaseIdV1, RuntimeLeaseV1, RuntimeMaintenanceStateV1, StoreClientIdV1,
    StoreIncarnationV1, StoreRuntimeBindingV1, StoreShardIdV1, StoreShardScopeV1,
    VerifiedStoreLocatorV1,
};

use super::super::*;

pub(super) fn id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    <T as TryFrom<String>>::Error: Debug,
{
    T::try_from(value.to_owned()).unwrap()
}

pub(super) fn incarnation() -> StoreIncarnationV1 {
    StoreIncarnationV1::new(1).unwrap()
}

pub(super) fn profile_shard() -> StoreShardIdV1 {
    StoreShardIdV1::profile(
        id::<BrainId>("brain.registry"),
        id::<UserProfileId>("profile.registry"),
    )
}

pub(super) fn profile_sessions_shard() -> StoreShardIdV1 {
    StoreShardIdV1::profile_sessions(
        id::<BrainId>("brain.registry"),
        id::<UserProfileId>("profile.registry"),
    )
}

fn profile_memory_shard() -> StoreShardIdV1 {
    StoreShardIdV1::profile_memory(
        id::<BrainId>("brain.registry"),
        id::<UserProfileId>("profile.registry"),
    )
}

fn project_shard(project: &str) -> StoreShardIdV1 {
    StoreShardIdV1::project(
        id::<BrainId>("brain.registry"),
        id::<UserProfileId>("profile.registry"),
        id::<ProjectId>(project),
    )
}

fn project_sessions_shard(project: &str) -> StoreShardIdV1 {
    StoreShardIdV1::project_sessions(
        id::<BrainId>("brain.registry"),
        id::<UserProfileId>("profile.registry"),
        id::<ProjectId>(project),
    )
}

fn code_shard(worktree: &str) -> StoreShardIdV1 {
    StoreShardIdV1::code(
        id::<BrainId>("brain.registry"),
        id::<UserProfileId>("profile.registry"),
        id::<ProjectId>("project.registry"),
        id::<RepositoryId>("repository.registry"),
        CodeShardScopeV1::Worktree {
            worktree_id: id::<WorktreeId>(worktree),
        },
    )
}

#[derive(Default)]
pub(super) struct TestResolver {
    pub(super) calls: AtomicUsize,
    pub(super) graph_calls: AtomicUsize,
}

impl StoreRuntimeResolver for TestResolver {
    fn resolve<'a>(
        &'a self,
        key: &'a StoreRuntimeKey,
        _mode: StoreRuntimeOpenMode,
        _database_authority: Option<&'a crate::db::DatabaseAuthority>,
    ) -> StoreRuntimeRegistryFuture<'a, Result<ResolvedStoreLocator, StoreRuntimeRegistryFailure>>
    {
        let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        let locator = VerifiedStoreLocatorV1::new(
            key.shard_id.clone(),
            key.incarnation,
            LocatorDigest::new(format!("sha256:{}", "a".repeat(64))).unwrap(),
        );
        Box::pin(async move {
            Ok(ResolvedStoreLocator::new(
                locator,
                PathBuf::from(format!("/verified/{call}")),
            ))
        })
    }

    fn resolve_graph<'a>(
        &'a self,
        key: &'a StoreRuntimeKey,
    ) -> StoreRuntimeRegistryFuture<'a, Result<ResolvedStoreLocator, StoreRuntimeRegistryFailure>>
    {
        self.graph_calls.fetch_add(1, Ordering::SeqCst);
        let locator = VerifiedStoreLocatorV1::new(
            key.shard_id.clone(),
            key.incarnation,
            LocatorDigest::new(format!("sha256:{}", "b".repeat(64))).unwrap(),
        );
        let path = PathBuf::from(format!(
            "/verified/graph/{:?}/{}",
            key.shard_id.scope,
            key.incarnation.get()
        ));
        Box::pin(async move { Ok(ResolvedStoreLocator::new(locator, path)) })
    }
}

#[derive(Default)]
pub(super) struct TestPublisher {
    pub(super) calls: AtomicUsize,
    pub(super) block: AtomicBool,
    pub(super) mode: AtomicU8,
    pub(super) release: tokio::sync::Notify,
    runtimes: Mutex<Vec<Weak<ShardRuntime>>>,
    pub(super) bindings: Mutex<Vec<StoreRuntimeBindingV1>>,
}

impl TestPublisher {
    pub(super) fn runtime(&self, index: usize) -> Arc<ShardRuntime> {
        self.runtimes
            .lock()
            .unwrap()
            .get(index)
            .unwrap()
            .upgrade()
            .unwrap()
    }
}

impl ShardRuntimePublisher for TestPublisher {
    fn publish(
        &self,
        request: ShardRuntimeBuildRequest,
    ) -> StoreRuntimeRegistryFuture<'_, Result<PublishedShardRuntime, StoreRuntimeRegistryFailure>>
    {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.bindings.lock().unwrap().push(request.binding.clone());
        Box::pin(async move {
            if self.block.load(Ordering::SeqCst) {
                self.release.notified().await;
            }
            if self.mode.load(Ordering::SeqCst) == 1 {
                return Err(StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
                    operation: "publish",
                    message: "publisher failed".to_owned(),
                });
            }
            let runtime = Arc::new(ShardRuntime::new(
                request.binding.clone(),
                matches!(request.binding.shard_id.scope, StoreShardScopeV1::Profile),
            ));
            runtime
                .transition(RuntimeMaintenanceStateV1::Opening)
                .unwrap();
            runtime
                .transition(RuntimeMaintenanceStateV1::Ready)
                .unwrap();
            self.runtimes.lock().unwrap().push(Arc::downgrade(&runtime));
            Ok(PublishedShardRuntime::new(
                runtime,
                Arc::new(EmptyPhysicalRuntimeAttachment),
            ))
        })
    }
}

pub(super) fn registry(
    config: StoreRuntimeRegistryConfig,
) -> (StoreRuntimeRegistry, Arc<TestResolver>, Arc<TestPublisher>) {
    let resolver = Arc::new(TestResolver::default());
    let publisher = Arc::new(TestPublisher::default());
    let registry =
        StoreRuntimeRegistry::with_config(resolver.clone(), publisher.clone(), config).unwrap();
    (registry, resolver, publisher)
}

pub(super) async fn open_published(
    registry: &StoreRuntimeRegistry,
    request: StoreRuntimeOpenRequest,
) -> StoreRuntimeClientLease {
    match registry.open(request).await {
        StoreRuntimeOpenResult::Published(handle) => handle,
        other @ StoreRuntimeOpenResult::Failed(_) => panic!("open failed: {other:?}"),
    }
}

pub(super) async fn profile_pin(registry: &StoreRuntimeRegistry) -> ProfileAuthorityPin {
    open_published(
        registry,
        StoreRuntimeOpenRequest::new(profile_shard(), incarnation(), None),
    )
    .await;
    match registry.profile_authority_pin(&profile_shard()) {
        ProfileAuthorityPinResult::Pinned(pin) => pin,
        other => panic!("profile was not pinned: {other:?}"),
    }
}

pub(super) fn project_request(project: &str, pin: &ProfileAuthorityPin) -> StoreRuntimeOpenRequest {
    StoreRuntimeOpenRequest::new(project_shard(project), incarnation(), Some(pin.clone()))
}

pub(super) fn profile_memory_request(pin: &ProfileAuthorityPin) -> StoreRuntimeOpenRequest {
    StoreRuntimeOpenRequest::new(profile_memory_shard(), incarnation(), Some(pin.clone()))
}

pub(super) fn project_sessions_request(
    project: &str,
    pin: &ProfileAuthorityPin,
) -> StoreRuntimeOpenRequest {
    StoreRuntimeOpenRequest::new(
        project_sessions_shard(project),
        incarnation(),
        Some(pin.clone()),
    )
}

pub(super) fn code_request(worktree: &str, pin: &ProfileAuthorityPin) -> StoreRuntimeOpenRequest {
    StoreRuntimeOpenRequest::new(code_shard(worktree), incarnation(), Some(pin.clone()))
}

pub(super) fn profile_sessions_request(pin: &ProfileAuthorityPin) -> StoreRuntimeOpenRequest {
    StoreRuntimeOpenRequest::new(profile_sessions_shard(), incarnation(), Some(pin.clone()))
}

pub(super) async fn wait_for_calls(calls: &AtomicUsize, expected: usize) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while calls.load(Ordering::SeqCst) < expected {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("publisher made progress");
}

pub(super) fn active_lease(binding: &StoreRuntimeBindingV1, lease_id: &str) -> RuntimeLeaseV1 {
    let now = utc_now();
    RuntimeLeaseV1 {
        lease_id: RuntimeLeaseIdV1::new(lease_id).unwrap(),
        binding: binding.clone(),
        holder: StoreClientIdV1::new("client.registry").unwrap(),
        acquired_at: UtcMicros(now.0.saturating_sub(1_000_000)),
        expires_at: UtcMicros(now.0.saturating_add(60_000_000)),
    }
}
