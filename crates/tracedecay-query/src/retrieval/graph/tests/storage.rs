use std::sync::Arc;
use std::time::{Duration, Instant};

use tempfile::TempDir;
use tracedecay_graph_db::{
    GraphDbRegistration, GraphDbRegistry, GraphDbRegistryConfig, GraphNamespace, NeverCancelled,
};
use tracedecay_store::{
    BrainId, ProjectId, RetainedGraphStoreLeaseV1, StoreAuthorityEpochV1, StoreIncarnationV1,
    StoreRuntimeBindingV1, StoreShardIdV1, UserProfileId, VerifiedStoreLocatorV1,
    canonical_store_locator_digest,
};

use super::*;

#[derive(Debug)]
struct TestGraphLease {
    binding: StoreRuntimeBindingV1,
    verified_locator: VerifiedStoreLocatorV1,
    canonical_path: std::path::PathBuf,
}

impl RetainedGraphStoreLeaseV1 for TestGraphLease {
    fn binding(&self) -> &StoreRuntimeBindingV1 {
        &self.binding
    }

    fn verified_locator(&self) -> &VerifiedStoreLocatorV1 {
        &self.verified_locator
    }

    fn canonical_path(&self) -> &std::path::Path {
        &self.canonical_path
    }
}

fn graph_namespace(worktree: &str, generation: &str) -> GraphNamespace {
    GraphNamespace::new(format!("code-scope:{worktree}:{generation}"))
        .expect("valid exact worktree generation namespace")
}

#[test]
fn graph_projection_reopens_with_identical_ordered_output() {
    let request = graph_request(8, 2);
    let edges = vec![
        CanonicalRelationEdgeV1 {
            from_occurrence: id("symbol.seed"),
            to_occurrence: id("symbol.middle"),
            kind: RelationEdgeKindV1::Calls,
            authority: EdgeAuthorityV1::SyntaxExact,
            evidence_span: SourceSpan {
                start_byte: 0,
                end_byte: 1,
            },
        },
        CanonicalRelationEdgeV1 {
            from_occurrence: id("symbol.middle"),
            to_occurrence: id("symbol.target"),
            kind: RelationEdgeKindV1::Uses,
            authority: EdgeAuthorityV1::HeuristicCandidate,
            evidence_span: SourceSpan {
                start_byte: 1,
                end_byte: 2,
            },
        },
    ];
    let cancellation =
        CancellationSignal::active("cancellation.code-graph.reopen").expect("valid token");
    let temp = TempDir::new().expect("temporary graph directory");
    let registry =
        GraphDbRegistry::new(GraphDbRegistryConfig { max_open: 1 }).expect("valid registry");
    let binding = StoreRuntimeBindingV1::new(
        StoreShardIdV1::project(
            BrainId::try_from("brain.query-test".to_owned()).expect("valid brain"),
            UserProfileId::try_from("profile.query-test".to_owned()).expect("valid profile"),
            ProjectId::try_from("project.query-test".to_owned()).expect("valid project"),
        ),
        StoreIncarnationV1::new(1).expect("valid incarnation"),
        StoreAuthorityEpochV1::new(1).expect("valid epoch"),
    );
    let canonical_path = temp.path().join("graph.grafeo");
    let registration = GraphDbRegistration {
        authority_lease: Arc::new(TestGraphLease {
            verified_locator: VerifiedStoreLocatorV1::new(
                binding.shard_id.clone(),
                binding.incarnation,
                canonical_store_locator_digest(&canonical_path).expect("valid locator digest"),
            ),
            binding: binding.clone(),
            canonical_path,
        }),
        cancellation: Arc::new(NeverCancelled),
        lifecycle_cancellation: Arc::new(NeverCancelled),
        deadline: Instant::now() + Duration::from_secs(30),
    };
    let database = registry
        .resolve(registration.clone())
        .expect("open persistent graph");
    let namespace = graph_namespace("worktree.reopen", request.generation.as_str());
    let store = CodeGraphProjectionStore::from_graph_db(
        database,
        namespace.clone(),
        request.generation.clone(),
    );
    publish_projection(
        &store,
        &request,
        &edges,
        &["symbol.seed", "symbol.middle", "symbol.target"],
    );
    let before = read_projection(&store, &request, &cancellation);
    drop(store);
    registry
        .close(&registration)
        .expect("close persistent graph");

    let reopened = CodeGraphProjectionStore::from_graph_db(
        registry
            .reopen(registration)
            .expect("reopen persistent graph"),
        namespace,
        request.generation.clone(),
    );
    let after = read_projection(&reopened, &request, &cancellation);

    assert_eq!(after, before);
    assert_eq!(
        serde_json::to_vec(&after).expect("serialize reopened output"),
        serde_json::to_vec(&before).expect("serialize original output")
    );
    result_order(
        &after,
        &["code-graph:symbol.middle", "code-graph:symbol.target"],
    );
    assert_eq!(
        after.evidence_by_occurrence[&id("code-graph:symbol.target")].weakest_authority,
        EdgeAuthorityV1::HeuristicCandidate
    );
}

#[test]
fn linked_worktree_generations_remain_queryable_in_one_project_graph() {
    let cancellation =
        CancellationSignal::active("cancellation.code-graph.worktrees").expect("valid token");
    let temp = TempDir::new().expect("temporary graph directory");
    let registry =
        GraphDbRegistry::new(GraphDbRegistryConfig { max_open: 1 }).expect("valid registry");
    let project_binding = StoreRuntimeBindingV1::new(
        StoreShardIdV1::project(
            BrainId::try_from("brain.query-worktrees".to_owned()).expect("valid brain"),
            UserProfileId::try_from("profile.query-worktrees".to_owned()).expect("valid profile"),
            ProjectId::try_from("project.query-worktrees".to_owned()).expect("valid project"),
        ),
        StoreIncarnationV1::new(1).expect("valid incarnation"),
        StoreAuthorityEpochV1::new(1).expect("valid epoch"),
    );
    let canonical_path = temp.path().join("project.grafeo");
    let registration = GraphDbRegistration {
        authority_lease: Arc::new(TestGraphLease {
            verified_locator: VerifiedStoreLocatorV1::new(
                project_binding.shard_id.clone(),
                project_binding.incarnation,
                canonical_store_locator_digest(&canonical_path).expect("valid locator digest"),
            ),
            binding: project_binding,
            canonical_path,
        }),
        cancellation: Arc::new(NeverCancelled),
        lifecycle_cancellation: Arc::new(NeverCancelled),
        deadline: Instant::now() + Duration::from_secs(30),
    };
    let database = registry
        .resolve(registration.clone())
        .expect("open shared project graph");
    let primary_request = graph_request(8, 1);
    let primary_namespace =
        graph_namespace("worktree.primary", primary_request.generation.as_str());
    let primary = CodeGraphProjectionStore::from_graph_db(
        Arc::clone(&database),
        primary_namespace.clone(),
        primary_request.generation.clone(),
    );
    let primary_edge = CanonicalRelationEdgeV1 {
        from_occurrence: id("symbol.seed"),
        to_occurrence: id("symbol.primary"),
        kind: RelationEdgeKindV1::Calls,
        authority: EdgeAuthorityV1::SyntaxExact,
        evidence_span: SourceSpan {
            start_byte: 0,
            end_byte: 1,
        },
    };
    publish_projection(
        &primary,
        &primary_request,
        std::slice::from_ref(&primary_edge),
        &["symbol.seed", "symbol.primary"],
    );

    let mut linked_request = graph_request(8, 1);
    linked_request.generation = id("generation.linked");
    linked_request.seed_anchors = vec![binding(&linked_request, "occ.seed", "symbol.seed")];
    let linked_namespace = graph_namespace("worktree.linked", linked_request.generation.as_str());
    let linked = CodeGraphProjectionStore::from_graph_db(
        Arc::clone(&database),
        linked_namespace.clone(),
        linked_request.generation.clone(),
    );
    let linked_edge = CanonicalRelationEdgeV1 {
        from_occurrence: id("symbol.seed"),
        to_occurrence: id("symbol.linked"),
        kind: RelationEdgeKindV1::Uses,
        authority: EdgeAuthorityV1::NameResolved,
        evidence_span: SourceSpan {
            start_byte: 2,
            end_byte: 3,
        },
    };
    publish_projection(
        &linked,
        &linked_request,
        std::slice::from_ref(&linked_edge),
        &["symbol.seed", "symbol.linked"],
    );
    assert_eq!(
        primary
            .publish_code_graph(
                &linked_request.generation,
                std::slice::from_ref(&linked_edge),
                &projection_chunks(&linked_request, &["symbol.seed", "symbol.linked"]),
                &cancellation,
            )
            .unwrap_err(),
        CodeGraphProjectionError::GenerationMismatch,
        "a generation cannot publish through another worktree generation namespace"
    );

    result_order(
        &read_projection(&primary, &primary_request, &cancellation),
        &["code-graph:symbol.primary"],
    );
    result_order(
        &read_projection(&linked, &linked_request, &cancellation),
        &["code-graph:symbol.linked"],
    );
    drop(linked);
    result_order(
        &read_projection(&primary, &primary_request, &cancellation),
        &["code-graph:symbol.primary"],
    );
    drop(primary);
    drop(database);
    registry
        .close(&registration)
        .expect("close shared project graph");

    let reopened = registry
        .reopen(registration)
        .expect("reopen shared project graph");
    let primary = CodeGraphProjectionStore::from_graph_db(
        Arc::clone(&reopened),
        primary_namespace,
        primary_request.generation.clone(),
    );
    let linked = CodeGraphProjectionStore::from_graph_db(
        reopened,
        linked_namespace,
        linked_request.generation.clone(),
    );
    result_order(
        &read_projection(&primary, &primary_request, &cancellation),
        &["code-graph:symbol.primary"],
    );
    result_order(
        &read_projection(&linked, &linked_request, &cancellation),
        &["code-graph:symbol.linked"],
    );
}

#[test]
fn published_generation_replacement_keeps_existing_reader_frozen() {
    let request_one = graph_request(8, 1);
    let edge_one = CanonicalRelationEdgeV1 {
        from_occurrence: id("symbol.seed"),
        to_occurrence: id("symbol.target-one"),
        kind: RelationEdgeKindV1::Calls,
        authority: EdgeAuthorityV1::SyntaxExact,
        evidence_span: SourceSpan {
            start_byte: 0,
            end_byte: 1,
        },
    };
    let cancellation =
        CancellationSignal::active("cancellation.code-graph.generations").expect("valid token");
    let store = CodeGraphProjectionStore::memory(&cancellation).expect("open memory graph");
    publish_projection(
        &store,
        &request_one,
        std::slice::from_ref(&edge_one),
        &["symbol.seed", "symbol.target-one"],
    );
    let reader_one = store
        .evidence_reader(
            &request_one.generation,
            None,
            freshness(FreshnessCompatibilityV1::Current),
            &cancellation,
        )
        .expect("open first generation reader");
    let before = complete_batch(
        reader_one
            .read_graph_evidence(&request_one)
            .expect("read first generation"),
    );

    let mut request_two = graph_request(8, 1);
    request_two.generation = id("generation.2");
    request_two.seed_anchors = vec![binding(&request_two, "occ.seed", "symbol.seed")];
    let edge_two = CanonicalRelationEdgeV1 {
        from_occurrence: id("symbol.seed"),
        to_occurrence: id("symbol.target-two"),
        kind: RelationEdgeKindV1::Calls,
        authority: EdgeAuthorityV1::NameResolved,
        evidence_span: SourceSpan {
            start_byte: 2,
            end_byte: 3,
        },
    };
    publish_projection(
        &store,
        &request_two,
        std::slice::from_ref(&edge_two),
        &["symbol.seed", "symbol.target-two"],
    );

    let frozen = complete_batch(
        reader_one
            .read_graph_evidence(&request_one)
            .expect("frozen reader remains readable"),
    );
    let current = read_projection(&store, &request_two, &cancellation);

    assert_eq!(frozen, before);
    result_order(&frozen, &["code-graph:symbol.target-one"]);
    result_order(&current, &["code-graph:symbol.target-two"]);
    assert_eq!(
        store
            .evidence_reader(
                &request_one.generation,
                None,
                freshness(FreshnessCompatibilityV1::Current),
                &cancellation,
            )
            .unwrap_err(),
        CodeGraphProjectionError::GenerationMismatch
    );
}

#[test]
fn cancelled_generation_publication_preserves_prior_generation() {
    let request = graph_request(8, 1);
    let edge = CanonicalRelationEdgeV1 {
        from_occurrence: id("symbol.seed"),
        to_occurrence: id("symbol.target"),
        kind: RelationEdgeKindV1::Calls,
        authority: EdgeAuthorityV1::SyntaxExact,
        evidence_span: SourceSpan {
            start_byte: 0,
            end_byte: 1,
        },
    };
    let active = CancellationSignal::active("cancellation.code-graph.prior").expect("valid token");
    let store = CodeGraphProjectionStore::memory(&active).expect("open memory graph");
    publish_projection(
        &store,
        &request,
        std::slice::from_ref(&edge),
        &["symbol.seed", "symbol.target"],
    );
    let before = read_projection(&store, &request, &active);

    let mut replacement = graph_request(8, 1);
    replacement.generation = id("generation.cancelled");
    replacement.seed_anchors = vec![binding(&replacement, "occ.seed", "symbol.seed")];
    let cancelled =
        CancellationSignal::active("cancellation.code-graph.cancelled").expect("valid token");
    assert!(cancelled.cancel(UtcMicros(42)));
    let replacement_chunks = projection_chunks(&replacement, &["symbol.seed", "symbol.other"]);
    let result = store.publish_code_graph(
        &replacement.generation,
        &[],
        &replacement_chunks,
        &cancelled,
    );

    assert_eq!(result.unwrap_err(), CodeGraphProjectionError::Cancelled);
    assert_eq!(read_projection(&store, &request, &active), before);
}
