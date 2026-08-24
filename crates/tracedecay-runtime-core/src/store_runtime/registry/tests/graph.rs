use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use tracedecay_domain::CodeGenerationId;
use tracedecay_graph_db::{
    GraphDbOwnerRegistrationV1, GraphDbRegistration, GraphDbRegistry, GraphDbRegistryConfig,
    NeverCancelled,
};
use tracedecay_store::{
    RetainedGraphStoreLeaseV1, RetainedGraphStoreOwnerAttachmentV1,
    RetainedGraphStoreOwnerOperationLeaseErrorV1, VerifiedStoreLocatorV1,
    canonical_store_locator_digest,
};

use super::super::*;
use super::support::*;

struct TemporaryGraphPathResolver {
    graph_path: PathBuf,
}

impl TemporaryGraphPathResolver {
    fn resolve_locator(&self, key: &StoreRuntimeKey) -> ResolvedStoreLocator {
        let locator = VerifiedStoreLocatorV1::new(
            key.shard_id.clone(),
            key.incarnation,
            canonical_store_locator_digest(&self.graph_path).unwrap(),
        );
        ResolvedStoreLocator::new(locator, self.graph_path.clone())
    }
}

impl StoreRuntimeResolver for TemporaryGraphPathResolver {
    fn resolve<'a>(
        &'a self,
        key: &'a StoreRuntimeKey,
        _mode: StoreRuntimeOpenMode,
        _database_authority: Option<&'a crate::db::DatabaseAuthority>,
    ) -> StoreRuntimeRegistryFuture<'a, Result<ResolvedStoreLocator, StoreRuntimeRegistryFailure>>
    {
        let resolved = self.resolve_locator(key);
        Box::pin(async move { Ok(resolved) })
    }

    fn resolve_graph<'a>(
        &'a self,
        key: &'a StoreRuntimeKey,
    ) -> StoreRuntimeRegistryFuture<'a, Result<ResolvedStoreLocator, StoreRuntimeRegistryFailure>>
    {
        let resolved = self.resolve_locator(key);
        Box::pin(async move { Ok(resolved) })
    }
}

#[tokio::test]
async fn graph_db_map_owner_is_not_counted_as_an_ordinary_store_graph_client() {
    let directory = tempfile::tempdir().unwrap();
    let store_registry = StoreRuntimeRegistry::with_config(
        Arc::new(TemporaryGraphPathResolver {
            graph_path: directory.path().join("project.grafeo"),
        }),
        Arc::new(TestPublisher::default()),
        StoreRuntimeRegistryConfig::default(),
    )
    .unwrap();
    let pin = profile_pin(&store_registry).await;
    let key = project_request("project.graphdb-map-owner", &pin)
        .key()
        .clone();
    let proof: Arc<dyn RetainedGraphStoreLeaseV1> = store_registry
        .retain_graph_store(key.clone())
        .await
        .unwrap();
    drop(proof);
    let operation_authority: Arc<dyn RetainedGraphStoreLeaseV1> = store_registry
        .retain_graph_store(key.clone())
        .await
        .unwrap();
    let (store_owner_attachment, _store_retirement_target) = store_registry
        .attach_graph_store_owner(key.clone())
        .await
        .unwrap();
    let graph_registry = GraphDbRegistry::new(GraphDbRegistryConfig { max_open: 1 }).unwrap();
    let map_owner = graph_registry
        .resolve_owner_attachment(GraphDbOwnerRegistrationV1 {
            operation: GraphDbRegistration {
                authority_lease: operation_authority,
                cancellation: Arc::new(NeverCancelled),
                lifecycle_cancellation: Arc::new(NeverCancelled),
                deadline: Instant::now() + Duration::from_secs(30),
            },
            authority_attachment: Box::new(store_owner_attachment),
        })
        .unwrap();

    {
        let state = store_registry.lock_state();
        let publication = state
            .graph_publications
            .get(&key)
            .expect("the graph map owner must retain its store publication");
        assert_eq!(publication.binding, *map_owner.binding());
        assert_eq!(
            publication.lease_tokens.len(),
            0,
            "GraphDb map owner must not be counted as an ordinary Store graph client"
        );
    }
    assert_eq!(store_registry.retained_graph_publications_for_test(), 1);
}

#[tokio::test]
async fn graph_map_owner_retains_no_ordinary_store_graph_lease() {
    let (store_registry, _, _) = registry(StoreRuntimeRegistryConfig::default());
    let pin = profile_pin(&store_registry).await;
    let key = project_request("project.graph-map-owner-identity", &pin)
        .key()
        .clone();

    let (owner_attachment, retirement_target) = store_registry
        .attach_graph_store_owner(key.clone())
        .await
        .unwrap();

    {
        let state = store_registry.lock_state();
        let publication = state.graph_publications.get(&key).unwrap();
        assert!(publication.owner_attachment.is_some());
        assert!(publication.lease_tokens.is_empty());
    }
    assert_eq!(store_registry.retained_graph_publications_for_test(), 1);

    drop(retirement_target);
    drop(owner_attachment);
    assert_eq!(store_registry.retained_graph_publications_for_test(), 0);
}

#[tokio::test]
async fn graph_map_owner_issued_operation_lease_is_an_ordinary_token_and_releases_on_drop() {
    let (store_registry, resolver, _) = registry(StoreRuntimeRegistryConfig::default());
    let pin = profile_pin(&store_registry).await;
    let key = project_request("project.graph-map-owner-operation", &pin)
        .key()
        .clone();
    let (owner_attachment, retirement_target) = store_registry
        .attach_graph_store_owner(key.clone())
        .await
        .unwrap();

    let operation = owner_attachment.issue_operation_lease().unwrap();
    assert_eq!(
        resolver.graph_calls.load(Ordering::SeqCst),
        1,
        "issuing a map-owner operation lease must not re-resolve its graph path"
    );
    {
        let state = store_registry.lock_state();
        let publication = state.graph_publications.get(&key).unwrap();
        assert!(matches!(
            publication.owner_attachment,
            Some(GraphStoreOwnerAttachmentState::MapOwned { .. })
        ));
        assert_eq!(
            publication.lease_tokens.len(),
            1,
            "an operation issued by the map owner must block retirement as an ordinary graph lease"
        );
    }

    drop(operation);
    {
        let state = store_registry.lock_state();
        let publication = state.graph_publications.get(&key).unwrap();
        assert!(publication.lease_tokens.is_empty());
    }

    drop(retirement_target);
    drop(owner_attachment);
    assert_eq!(store_registry.retained_graph_publications_for_test(), 0);
}

#[tokio::test]
async fn reserved_graph_map_owner_cannot_issue_operation_leases() {
    let (store_registry, _, _) = registry(StoreRuntimeRegistryConfig::default());
    let pin = profile_pin(&store_registry).await;
    let key = project_request("project.graph-map-owner-reserved-operation", &pin)
        .key()
        .clone();
    let (owner_attachment, mut retirement_target) =
        store_registry.attach_graph_store_owner(key).await.unwrap();

    {
        let mut state = store_registry.lock_state();
        retirement_target
            .reserve_locked(&store_registry, &mut state)
            .unwrap();
    }
    assert!(matches!(
        owner_attachment.issue_operation_lease(),
        Err(RetainedGraphStoreOwnerOperationLeaseErrorV1::Retiring)
    ));

    {
        let mut state = store_registry.lock_state();
        assert!(retirement_target.restore_locked(&store_registry, &mut state));
    }
    let operation = owner_attachment.issue_operation_lease().unwrap();
    drop(operation);
    drop(retirement_target);
    drop(owner_attachment);
}

#[tokio::test]
async fn graph_map_owner_operation_lease_rejects_foreign_and_stale_identities() {
    let (store_registry, _, _) = registry(StoreRuntimeRegistryConfig::default());
    let pin = profile_pin(&store_registry).await;
    let key = project_request("project.graph-map-owner-stale-operation", &pin)
        .key()
        .clone();
    let (owner_attachment, retirement_target) = store_registry
        .attach_graph_store_owner(key.clone())
        .await
        .unwrap();
    let binding = owner_attachment.binding().clone();
    let locator = owner_attachment.verified_locator().clone();
    let path = owner_attachment.canonical_path().to_path_buf();
    let (owner_id, attachment_id) = {
        let state = store_registry.lock_state();
        let publication = state.graph_publications.get(&key).unwrap();
        let Some(GraphStoreOwnerAttachmentState::MapOwned {
            owner_id,
            attachment_id,
        }) = publication.owner_attachment
        else {
            panic!("the exact map owner attachment must be live");
        };
        (owner_id, attachment_id)
    };

    let foreign_owner_id = GraphStoreOwnerIdentityV1(owner_id.0.checked_add(1).unwrap());
    let foreign = store_registry.issue_graph_store_owner_operation_lease(
        &binding,
        &locator,
        &path,
        foreign_owner_id,
        attachment_id,
    );
    assert!(matches!(
        foreign,
        Err(StoreRuntimeRegistryFailure::GraphOwnerAttachmentReservationLost { .. })
    ));

    {
        let mut state = store_registry.lock_state();
        state.graph_publications.remove(&key);
    }
    let stale = store_registry.issue_graph_store_owner_operation_lease(
        &binding,
        &locator,
        &path,
        owner_id,
        attachment_id,
    );
    assert!(matches!(
        stale,
        Err(StoreRuntimeRegistryFailure::GraphOwnerAttachmentMissing { .. })
    ));

    drop(retirement_target);
    drop(owner_attachment);
}

#[tokio::test]
async fn graph_map_owner_attachment_rejects_a_second_map_owner() {
    let (store_registry, _, _) = registry(StoreRuntimeRegistryConfig::default());
    let pin = profile_pin(&store_registry).await;
    let key = project_request("project.graph-map-owner-duplicate", &pin)
        .key()
        .clone();
    let (_owner_attachment, _retirement_target) = store_registry
        .attach_graph_store_owner(key.clone())
        .await
        .unwrap();

    let error = store_registry
        .attach_graph_store_owner(key)
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        StoreRuntimeRegistryFailure::GraphOwnerAttachmentAlreadyRegistered { .. }
    ));
}

#[tokio::test]
async fn exact_graph_scopes_publish_without_a_physical_runtime() {
    let (registry, resolver, publisher) = registry(StoreRuntimeRegistryConfig::default());
    let pin = profile_pin(&registry).await;
    let requests = [
        project_request("project.graph-scope", &pin),
        project_sessions_request("project.graph-sessions", &pin),
        profile_memory_request(&pin),
        profile_sessions_request(&pin),
    ];

    let mut leases = Vec::new();
    for request in requests {
        let lease = registry
            .retain_graph_store(request.key().clone())
            .await
            .unwrap();
        assert_eq!(lease.binding().shard_id, *request.key().shard_id());
        assert_eq!(lease.verified_locator().shard_id, *request.key().shard_id());
        assert!(lease.canonical_path().is_absolute());
        leases.push(lease);
    }

    assert_eq!(resolver.graph_calls.load(Ordering::SeqCst), 4);
    assert_eq!(
        publisher.calls.load(Ordering::SeqCst),
        1,
        "only the profile pin may open a physical runtime"
    );
    assert_eq!(registry.retained_graph_publications_for_test(), 4);
    drop(leases);
    assert_eq!(registry.retained_graph_publications_for_test(), 0);
}

#[tokio::test]
async fn linked_worktree_code_scopes_share_one_project_graph_lease() {
    let (registry, resolver, publisher) = registry(StoreRuntimeRegistryConfig::default());
    let pin = profile_pin(&registry).await;
    let project_key = project_request("project.registry", &pin).key().clone();
    let primary_scope = code_request("worktree.primary", &pin)
        .key()
        .shard_id()
        .clone();
    let linked_scope = code_request("worktree.linked", &pin)
        .key()
        .shard_id()
        .clone();
    let primary_generation: CodeGenerationId = id("generation.primary");
    let linked_generation: CodeGenerationId = id("generation.linked");

    let primary = registry
        .retain_code_graph_store(
            project_key.clone(),
            primary_scope.clone(),
            primary_generation.clone(),
        )
        .await
        .unwrap();
    let linked = registry
        .retain_code_graph_store(
            project_key.clone(),
            linked_scope.clone(),
            linked_generation.clone(),
        )
        .await
        .unwrap();
    let primary_next = registry
        .retain_code_graph_store(
            project_key,
            primary_scope.clone(),
            id("generation.primary-next"),
        )
        .await
        .unwrap();

    assert_eq!(primary.binding(), linked.binding());
    assert_eq!(
        primary.binding().shard_id.scope,
        StoreShardScopeV1::Project {
            project_id: id("project.registry"),
        }
    );
    assert_eq!(primary.canonical_path(), linked.canonical_path());
    assert_eq!(primary.code_shard_id(), &primary_scope);
    assert_eq!(linked.code_shard_id(), &linked_scope);
    assert_eq!(primary.generation_id(), &primary_generation);
    assert_eq!(linked.generation_id(), &linked_generation);
    assert_ne!(primary.namespace(), linked.namespace());
    assert_ne!(primary.namespace(), primary_next.namespace());
    assert_eq!(resolver.graph_calls.load(Ordering::SeqCst), 3);
    assert_eq!(
        publisher.calls.load(Ordering::SeqCst),
        1,
        "only the profile pin may open a relational runtime"
    );
    assert_eq!(registry.retained_graph_publications_for_test(), 1);

    drop(linked);
    drop(primary_next);
    assert_eq!(
        registry.retained_graph_publications_for_test(),
        1,
        "retiring one linked worktree scope must retain the shared project graph"
    );
    drop(primary);
    assert_eq!(registry.retained_graph_publications_for_test(), 0);
}

#[tokio::test]
async fn broad_profile_graph_scope_is_rejected_before_resolution() {
    let (registry, resolver, publisher) = registry(StoreRuntimeRegistryConfig::default());
    let error = registry
        .retain_graph_store(StoreRuntimeKey::new(profile_shard(), incarnation()))
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        StoreRuntimeRegistryFailure::UnsupportedShardScope
    ));
    assert_eq!(resolver.graph_calls.load(Ordering::SeqCst), 0);
    assert_eq!(publisher.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn graph_and_relational_publications_share_one_exact_binding() {
    let (registry, _, _) = registry(StoreRuntimeRegistryConfig::default());
    let pin = profile_pin(&registry).await;

    let graph_first_request = project_request("project.graph-first", &pin);
    let graph_first = registry
        .retain_graph_store(graph_first_request.key().clone())
        .await
        .unwrap();
    let relational_after = open_published(&registry, graph_first_request).await;
    assert_eq!(graph_first.binding(), relational_after.binding());

    let relational_first_request = project_request("project.relational-first", &pin);
    let relational_first = open_published(&registry, relational_first_request.clone()).await;
    let graph_after = registry
        .retain_graph_store(relational_first_request.key().clone())
        .await
        .unwrap();
    assert_eq!(graph_after.binding(), relational_first.binding());
}

#[tokio::test]
async fn concurrent_relational_open_reuses_its_reserved_binding_for_graph() {
    let (registry, _, publisher) = registry(StoreRuntimeRegistryConfig::default());
    let pin = profile_pin(&registry).await;
    publisher.block.store(true, Ordering::SeqCst);
    let request = project_request("project.opening", &pin);
    let opening = registry.begin_or_join_open(&request);
    wait_for_calls(&publisher.calls, 2).await;

    let graph = registry
        .retain_graph_store(request.key().clone())
        .await
        .unwrap();
    let reserved = publisher.bindings.lock().unwrap().last().cloned().unwrap();
    assert_eq!(graph.binding(), &reserved);

    publisher.release.notify_one();
    let relational = match opening.wait().await {
        StoreRuntimeOpenResult::Published(handle) => handle,
        StoreRuntimeOpenResult::Failed(error) => panic!("open failed: {error:?}"),
    };
    assert_eq!(graph.binding(), relational.binding());
}

#[tokio::test]
async fn graph_lease_drop_is_counted_and_epoch_compare_and_swap_safe() {
    let (registry, _, _) = registry(StoreRuntimeRegistryConfig::default());
    let pin = profile_pin(&registry).await;
    let key = project_request("project.graph-cas", &pin).key().clone();

    let first = registry.retain_graph_store(key.clone()).await.unwrap();
    let peer = registry.retain_graph_store(key.clone()).await.unwrap();
    assert_eq!(first.binding(), peer.binding());
    assert_eq!(registry.retained_graph_publications_for_test(), 1);
    drop(first);
    assert_eq!(registry.retained_graph_publications_for_test(), 1);
    let stale = peer.binding().clone();
    let stale_locator = peer.verified_locator().clone();
    let stale_path = peer.canonical_path().to_path_buf();
    let stale_lease_token = registry.lock_state().next_graph_lease_token;
    drop(peer);
    assert_eq!(registry.retained_graph_publications_for_test(), 0);

    let replacement = registry.retain_graph_store(key).await.unwrap();
    assert!(replacement.binding().authority_epoch > stale.authority_epoch);
    let replacement_epoch = replacement.binding().authority_epoch;
    assert!(!registry.release_graph_store(&stale, &stale_locator, &stale_path, stale_lease_token,));
    assert_eq!(registry.retained_graph_publications_for_test(), 1);
    assert_eq!(replacement.binding().authority_epoch, replacement_epoch);

    let retained: Arc<dyn RetainedGraphStoreLeaseV1> = replacement;
    drop(retained);
    assert_eq!(registry.retained_graph_publications_for_test(), 0);
}
