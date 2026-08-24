use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use tracedecay_domain::{FactOwnerV1, ProjectId};
use tracedecay_graph_db::{GraphDbError, GraphEntityId, GraphRelationKind};
use tracedecay_store::{FactReadControl, FactStoreError};

use super::graph::{ProjectedRelation, graph_error, validate_rooted_relations};
use super::graph_manifest::{MemoryGraphSource, build_manifest, source_watermark};

fn entity(value: &str) -> GraphEntityId {
    GraphEntityId::new(value).expect("graph entity fixture")
}

fn relation(source: &str, target: &str, kind: &str) -> ProjectedRelation {
    ProjectedRelation {
        source: entity(source),
        target: entity(target),
        kind: GraphRelationKind::new(kind).expect("graph relation fixture"),
    }
}

#[test]
fn graph_reset_required_preserves_the_exact_memory_owner() {
    let owner = FactOwnerV1::Project {
        project_id: ProjectId::new("project.graph-reset-owner").expect("project id"),
    };

    assert!(matches!(
        graph_error(
            &owner,
            GraphDbError::ResetRequired {
                message: "verified graph generation is incompatible".to_owned(),
            },
        ),
        FactStoreError::GraphResetRequired {
            owner: mapped_owner,
            reason,
        } if mapped_owner == owner && reason == "verified graph generation is incompatible"
    ));
}

#[test]
fn rooted_induced_relations_preserve_chords_parallel_kinds_and_exact_limit() {
    let accepted = BTreeSet::from([entity("a"), entity("b"), entity("c")]);
    let relations = vec![
        relation("a", "b", "memory-supports"),
        relation("a", "b", "memory-derived-from"),
        relation("a", "c", "memory-contradicts"),
    ];

    let exact = validate_rooted_relations(&accepted, relations, 3)
        .expect("exact relation limit remains complete");
    assert_eq!(exact.len(), 3);
    assert_eq!(
        exact
            .iter()
            .map(|relation| relation.kind.as_str())
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([
            "memory-contradicts",
            "memory-derived-from",
            "memory-supports",
        ])
    );

    assert!(matches!(
        validate_rooted_relations(&accepted, exact, 2),
        Err(GraphDbError::BudgetExhausted { .. })
    ));
}

#[test]
fn rooted_relation_with_an_endpoint_outside_the_reachable_set_fails_closed() {
    let accepted = BTreeSet::from([entity("a"), entity("b")]);

    assert!(matches!(
        validate_rooted_relations(
            &accepted,
            vec![relation("a", "outside", "memory-supports")],
            1,
        ),
        Err(GraphDbError::Conflict)
    ));
}

#[test]
fn source_watermark_observes_live_cancellation_inside_the_entity_loop() {
    let checks = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&checks);
    let control = FactReadControl::new(Arc::new(move || {
        observed.fetch_add(1, Ordering::AcqRel) >= 4
    }));
    let source = MemoryGraphSource {
        owner: "profile".to_owned(),
        entities: (0..10_000)
            .map(|index| format!("memory-fact:{index:08x}"))
            .collect(),
        relations: BTreeSet::new(),
    };

    assert!(matches!(
        source_watermark(&FactOwnerV1::Profile, &source, Some(&control)),
        Err(FactStoreError::ReadCancelled)
    ));
    assert!(checks.load(Ordering::Acquire) > 4);
}

#[test]
fn manifest_construction_observes_live_cancellation_inside_entity_allocation() {
    let checks = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&checks);
    let control = FactReadControl::new(Arc::new(move || {
        observed.fetch_add(1, Ordering::AcqRel) >= 12_000
    }));
    let source = MemoryGraphSource {
        owner: "profile".to_owned(),
        entities: (0..10_000)
            .map(|index| format!("memory-fact:{index:08x}"))
            .collect(),
        relations: BTreeSet::new(),
    };
    let projection = tracedecay_graph_db::GraphProjectionIdentity::new(
        tracedecay_graph_db::GraphNamespace::new("project-memory:test")
            .expect("graph namespace fixture"),
        tracedecay_graph_db::GraphProjectionId::new("project-memory-relations")
            .expect("graph projection fixture"),
    );
    let watermark = tracedecay_graph_db::GraphWatermark::new("memory-relations:test")
        .expect("graph watermark fixture");

    assert!(matches!(
        build_manifest(
            &FactOwnerV1::Profile,
            projection,
            &source,
            watermark,
            Some(&control),
        ),
        Err(FactStoreError::ReadCancelled)
    ));
    assert!(checks.load(Ordering::Acquire) > 12_000);
}

#[test]
fn manifest_validation_observes_live_cancellation_after_local_construction() {
    let checks = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&checks);
    let control = FactReadControl::new(Arc::new(move || {
        observed.fetch_add(1, Ordering::AcqRel) >= 25_000
    }));
    let source = MemoryGraphSource {
        owner: "profile".to_owned(),
        entities: (0..10_000)
            .map(|index| format!("memory-fact:{index:08x}"))
            .collect(),
        relations: BTreeSet::new(),
    };
    let projection = tracedecay_graph_db::GraphProjectionIdentity::new(
        tracedecay_graph_db::GraphNamespace::new("project-memory:test")
            .expect("graph namespace fixture"),
        tracedecay_graph_db::GraphProjectionId::new("project-memory-relations")
            .expect("graph projection fixture"),
    );
    let watermark = tracedecay_graph_db::GraphWatermark::new("memory-relations:test")
        .expect("graph watermark fixture");

    assert!(matches!(
        build_manifest(
            &FactOwnerV1::Profile,
            projection,
            &source,
            watermark,
            Some(&control),
        ),
        Err(FactStoreError::ReadCancelled)
    ));
    assert!(checks.load(Ordering::Acquire) > 25_000);
}

#[test]
fn manifest_construction_rejects_an_already_cancelled_read_before_allocation() {
    let source = MemoryGraphSource {
        owner: "profile".to_owned(),
        entities: (0..10_000)
            .map(|index| format!("memory-fact:{index:08x}"))
            .collect(),
        relations: BTreeSet::new(),
    };
    let projection = tracedecay_graph_db::GraphProjectionIdentity::new(
        tracedecay_graph_db::GraphNamespace::new("project-memory:test")
            .expect("graph namespace fixture"),
        tracedecay_graph_db::GraphProjectionId::new("project-memory-relations")
            .expect("graph projection fixture"),
    );
    let watermark = tracedecay_graph_db::GraphWatermark::new("memory-relations:test")
        .expect("graph watermark fixture");
    let control = FactReadControl::new(Arc::new(|| true));

    assert!(matches!(
        build_manifest(
            &FactOwnerV1::Profile,
            projection,
            &source,
            watermark,
            Some(&control),
        ),
        Err(FactStoreError::ReadCancelled)
    ));
}
