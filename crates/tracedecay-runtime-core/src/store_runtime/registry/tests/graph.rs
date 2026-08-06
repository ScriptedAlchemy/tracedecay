use std::sync::Arc;
use std::sync::atomic::Ordering;

use tracedecay_domain::CodeGenerationId;
use tracedecay_store::RetainedGraphStoreLeaseV1;

use super::super::*;
use super::support::*;

#[tokio::test]
async fn exact_graph_scopes_publish_without_a_physical_runtime() {
    let (registry, resolver, publisher) = registry(StoreRuntimeRegistryConfig::default());
    let pin = profile_pin(&registry).await;
    let requests = [
        project_request("project.graph-scope", &pin),
        project_sessions_request("project.graph-sessions", &pin),
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

    assert_eq!(resolver.graph_calls.load(Ordering::SeqCst), 3);
    assert_eq!(
        publisher.calls.load(Ordering::SeqCst),
        1,
        "only the profile pin may open a physical runtime"
    );
    assert_eq!(registry.retained_graph_publications_for_test(), 3);
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
    drop(peer);
    assert_eq!(registry.retained_graph_publications_for_test(), 0);

    let replacement = registry.retain_graph_store(key).await.unwrap();
    assert!(replacement.binding().authority_epoch > stale.authority_epoch);
    let replacement_epoch = replacement.binding().authority_epoch;
    assert!(!registry.release_graph_store(&stale, &stale_locator, &stale_path));
    assert_eq!(registry.retained_graph_publications_for_test(), 1);
    assert_eq!(replacement.binding().authority_epoch, replacement_epoch);

    let retained: Arc<dyn RetainedGraphStoreLeaseV1> = replacement;
    drop(retained);
    assert_eq!(registry.retained_graph_publications_for_test(), 0);
}
