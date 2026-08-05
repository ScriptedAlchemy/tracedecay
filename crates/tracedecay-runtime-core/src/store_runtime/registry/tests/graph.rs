use std::sync::Arc;
use std::sync::atomic::Ordering;

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
        code_request("worktree.graph-scope", &pin),
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

    let graph_first_request = code_request("worktree.graph-first", &pin);
    let graph_first = registry
        .retain_graph_store(graph_first_request.key().clone())
        .await
        .unwrap();
    let relational_after = open_published(&registry, graph_first_request).await;
    assert_eq!(graph_first.binding(), relational_after.binding());

    let relational_first_request = code_request("worktree.relational-first", &pin);
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
    let request = code_request("worktree.opening", &pin);
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
    let key = code_request("worktree.graph-cas", &pin).key().clone();

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
