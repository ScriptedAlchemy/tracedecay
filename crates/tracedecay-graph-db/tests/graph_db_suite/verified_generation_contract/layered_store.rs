//! A layered sealed generation, a delta over its predecessor's sealed cold
//! container, serves exactly what a cold build of the same rows serves and
//! records the same recovered digest, while its container encodes only the
//! rows that changed.

use std::path::Path;

use tracedecay_graph_db::{
    GraphGenerationRows, GraphLabel, GraphSealedBaseAbsenceV1, GraphTraversalDirection,
    TraversalRequest, VerifiedGraphCommit, VerifiedGraphSnapshot,
};

use super::*;

const SYMBOLS: usize = 200;

fn symbol(index: usize, marker: &str) -> GraphEntity {
    GraphEntity::new(
        GraphEntityId::new(format!("entity:{index:03}")).unwrap(),
        BTreeSet::from([GraphLabel::new("Symbol").unwrap()]),
        BTreeMap::from([(
            GraphPropertyName::new("marker").unwrap(),
            GraphProperty::String(marker.to_owned()),
        )]),
    )
    .unwrap()
}

fn edge(
    identity: &GraphProjectionIdentity,
    id: String,
    from: usize,
    to: usize,
    kind: &str,
) -> GraphGenerationRelation {
    GraphGenerationRelation::new(
        GraphRelationId::new(id).unwrap(),
        GraphEntityRef::new(
            identity.clone(),
            GraphEntityId::new(format!("entity:{from:03}")).unwrap(),
        ),
        GraphEntityRef::new(
            identity.clone(),
            GraphEntityId::new(format!("entity:{to:03}")).unwrap(),
        ),
        GraphRelationKind::new(kind).unwrap(),
        BTreeMap::from([(
            GraphPropertyName::new("site").unwrap(),
            GraphProperty::I64((from * 1_000 + to) as i64),
        )]),
    )
    .unwrap()
}

/// A hub every symbol calls plus a reference chain, so fan-ins exceed small
/// budgets and traversals branch.
fn parent_rows(
    identity: &GraphProjectionIdentity,
) -> (Vec<GraphEntity>, Vec<GraphGenerationRelation>) {
    let entities = (0..SYMBOLS).map(|index| symbol(index, "parent")).collect();
    let mut relations = Vec::new();
    for index in 1..SYMBOLS {
        relations.push(edge(
            identity,
            format!("call:{index:03}"),
            index,
            0,
            "calls",
        ));
        relations.push(edge(
            identity,
            format!("ref:{index:03}"),
            index - 1,
            index,
            "references",
        ));
    }
    (entities, relations)
}

/// The parent after a small refresh: ten symbols deleted with every
/// relation they anchor, one symbol's payload changed, one reference
/// dropped, and new symbols whose relations reach unchanged ones.
fn child_rows(
    identity: &GraphProjectionIdentity,
) -> (Vec<GraphEntity>, Vec<GraphGenerationRelation>) {
    let removed = |index: usize| (10..20).contains(&index);
    let (entities, relations) = parent_rows(identity);
    let mut entities = entities
        .into_iter()
        .filter(|entity| {
            let index: usize = entity.identity.as_str()["entity:".len()..].parse().unwrap();
            !removed(index)
        })
        .map(|entity| {
            if entity.identity.as_str() == "entity:050" {
                symbol(50, "child")
            } else {
                entity
            }
        })
        .collect::<Vec<_>>();
    let endpoint = |reference: &GraphEntityRef| -> usize {
        reference.identity.as_str()["entity:".len()..]
            .parse()
            .unwrap()
    };
    let mut relations = relations
        .into_iter()
        .filter(|relation| {
            !removed(endpoint(&relation.from))
                && !removed(endpoint(&relation.to))
                && relation.identity.as_str() != "ref:100"
        })
        .collect::<Vec<_>>();
    for index in SYMBOLS..SYMBOLS + 10 {
        entities.push(symbol(index, "new"));
        relations.push(edge(
            identity,
            format!("call:{index:03}"),
            index,
            0,
            "calls",
        ));
        relations.push(edge(identity, format!("use:{index:03}"), index, 50, "uses"));
        relations.push(edge(
            identity,
            format!("tail:{index:03}"),
            199,
            index,
            "references",
        ));
    }
    relations.push(edge(
        identity,
        "ref:150-170".to_owned(),
        150,
        170,
        "references",
    ));
    (entities, relations)
}

fn manifest(
    identity: &GraphProjectionIdentity,
    generation: &str,
    (entities, relations): (Vec<GraphEntity>, Vec<GraphGenerationRelation>),
) -> GraphGenerationManifest {
    GraphGenerationManifest::new(
        identity.clone(),
        GraphGenerationId::new(generation).unwrap(),
        SourceGeneration::new(format!("source:{generation}")).unwrap(),
        GraphWatermark::new(format!("watermark:{generation}")).unwrap(),
        Vec::new(),
        entities,
        relations,
    )
    .unwrap()
}

fn replay_source(generation: &GraphGenerationId, input: char) -> SealedCodeGenerationReplay {
    SealedCodeGenerationReplay {
        repository: RepositoryId::new("repository.layered-store").unwrap(),
        generation: CodeGenerationId::new(format!("code-generation.{}", generation.as_str()))
            .unwrap(),
        sealed_state_digest: SealedGraphStateDigest::try_from(format!(
            "sha256:{}",
            input.to_string().repeat(64)
        ))
        .unwrap(),
        projector_revision: GraphProjectorRevision::try_from("projector.layered-store".to_owned())
            .unwrap(),
    }
}

fn publish_rows(
    graph: &RegisteredGraph,
    root: &Path,
    authority: &mut RelationalAuthority,
    rows: GraphGenerationRows,
    prior: Option<GraphVerifiedHeadV1>,
    input: char,
) -> VerifiedGraphCommit {
    let identity = rows.identity();
    let record = authority.stage(
        rows.relational_sealed_replay(
            graph.binding.shard_id.clone(),
            GraphIdempotencyKey::new(format!("publish:{}", identity.generation.as_str())).unwrap(),
            digest(input),
            prior,
            replay_source(&identity.generation, input),
            &|| Ok(()),
        )
        .unwrap(),
    );
    let (control, probe) = control_and_probe();
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    graph
        .registry
        .publish_verified(
            registration(graph.binding.clone(), root),
            authority,
            &context,
            &record.publication.key,
            Some(rows),
        )
        .unwrap()
}

fn publish_cold(
    graph: &RegisteredGraph,
    root: &Path,
    authority: &mut RelationalAuthority,
    manifest: &GraphGenerationManifest,
    prior: Option<GraphVerifiedHeadV1>,
    input: char,
    attachment: Option<&[u8]>,
) -> VerifiedGraphCommit {
    let mut spill = graph
        .registry
        .generation_row_spill(
            registration(graph.binding.clone(), root),
            manifest.projection.clone(),
        )
        .unwrap();
    spill
        .push_batch(
            manifest.entities.clone(),
            manifest.relations.clone(),
            &|| Ok(()),
        )
        .unwrap();
    if let Some(attachment) = attachment {
        std::fs::write(spill.attachment_path(), attachment).unwrap();
    }
    let spilled = spill.finish(manifest.identity(), &|| Ok(())).unwrap();
    publish_rows(graph, root, authority, spilled.into(), prior, input)
}

/// The sealed artifact directory whose receipt names `generation`.
fn sealed_directory_of(sealed_root: &Path, generation: &str) -> std::path::PathBuf {
    std::fs::read_dir(sealed_root)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| {
            std::fs::read(path.join("sealed.json")).is_ok_and(|receipt| {
                serde_json::from_slice::<serde_json::Value>(&receipt).unwrap()["generation"]
                    == generation
            })
        })
        .unwrap_or_else(|| panic!("no sealed artifact for {generation}"))
}

/// Opaque producer inputs a layerable cold generation seals beside its rows.
const PRODUCER_INPUTS: &[u8] = b"producer resolution inputs";

/// Publishes `child` as a delta over `parent`'s sealed store: only rows that
/// differ are pushed, every parent identity the child lacks is hidden.
/// Returns the commit and the rows the delta container encodes.
fn publish_layered(
    graph: &RegisteredGraph,
    root: &Path,
    authority: &mut RelationalAuthority,
    parent: &GraphGenerationManifest,
    child: &GraphGenerationManifest,
    prior: GraphVerifiedHeadV1,
    input: char,
) -> (VerifiedGraphCommit, (usize, usize)) {
    let base = graph
        .registry
        .sealed_generation_base(
            registration(graph.binding.clone(), root),
            parent.projection.clone(),
            parent.generation.clone(),
            &|| Ok(()),
        )
        .unwrap()
        .expect("a spilled cold generation is a layered base");
    let mut spill = graph
        .registry
        .layered_row_spill(
            registration(graph.binding.clone(), root),
            child.projection.clone(),
            base,
        )
        .unwrap();
    assert_eq!(
        std::fs::read(spill.base_attachment()).unwrap(),
        PRODUCER_INPUTS,
        "the spill pins the base's producer attachment"
    );
    let parent_entities = parent
        .entities
        .iter()
        .map(|entity| (entity.identity.clone(), entity))
        .collect::<BTreeMap<_, _>>();
    let parent_relations = parent
        .relations
        .iter()
        .map(|relation| (relation.identity.clone(), relation))
        .collect::<BTreeMap<_, _>>();
    let child_entities = child
        .entities
        .iter()
        .map(|entity| entity.identity.clone())
        .collect::<BTreeSet<_>>();
    let child_relations = child
        .relations
        .iter()
        .map(|relation| relation.identity.clone())
        .collect::<BTreeSet<_>>();
    spill
        .push_batch(
            child
                .entities
                .iter()
                .filter(|entity| parent_entities.get(&entity.identity) != Some(entity))
                .cloned()
                .collect(),
            child
                .relations
                .iter()
                .filter(|relation| parent_relations.get(&relation.identity) != Some(relation))
                .cloned()
                .collect(),
            &|| Ok(()),
        )
        .unwrap();
    spill.hide(
        parent_entities
            .keys()
            .filter(|identity| !child_entities.contains(*identity))
            .cloned(),
        parent_relations
            .keys()
            .filter(|identity| !child_relations.contains(*identity))
            .cloned(),
    );
    let layered = spill.finish(child.identity(), &|| Ok(())).unwrap();
    let delta = layered.delta_row_counts();
    (
        publish_rows(graph, root, authority, layered.into(), Some(prior), input),
        delta,
    )
}

fn projection_scan(
    snapshot: &VerifiedGraphSnapshot,
    identity: &GraphProjectionIdentity,
) -> (Vec<GraphEntity>, Vec<tracedecay_graph_db::GraphRelation>) {
    let mut entities = Vec::new();
    let mut relations = Vec::new();
    let (mut after_entity, mut after_relation) = (None, None);
    let (mut entities_done, mut relations_done) = (false, false);
    while !entities_done || !relations_done {
        let page = snapshot
            .read_projection(GraphProjectionReadRequest {
                namespace: identity.namespace.clone(),
                projection: identity.projection.clone(),
                after_entity: after_entity.clone(),
                after_relation: after_relation.clone(),
                max_entities: if entities_done { 0 } else { 17 },
                max_relations: if relations_done { 0 } else { 23 },
                cancellation: Arc::new(TestCancellation),
            })
            .unwrap();
        if !entities_done {
            entities.extend(page.entities);
            after_entity = page.next_entity;
            entities_done = after_entity.is_none();
        }
        if !relations_done {
            relations.extend(page.relations);
            after_relation = page.next_relation;
            relations_done = after_relation.is_none();
        }
    }
    (entities, relations)
}

/// Every read a verified snapshot offers, over every identity either
/// generation ever held, answered identically by both snapshots.
fn assert_same_reads(
    cold: &VerifiedGraphSnapshot,
    layered: &VerifiedGraphSnapshot,
    identity: &GraphProjectionIdentity,
) {
    let cancellation = || -> Arc<dyn GraphCancellation> { Arc::new(TestCancellation) };
    let (cold_entities, cold_relations) = projection_scan(cold, identity);
    assert_eq!(
        (cold_entities.len(), cold_relations.len()),
        // 398 parent relations less the 21 the deleted symbols anchor and
        // `ref:100`, plus 31 new ones.
        (SYMBOLS, 407),
        "the cold child serves the rows its manifest holds"
    );
    assert_eq!(
        projection_scan(layered, identity),
        (cold_entities, cold_relations.clone())
    );

    let entity_ids = (0..SYMBOLS + 10)
        .map(|index| GraphEntityId::new(format!("entity:{index:03}")).unwrap())
        .collect::<Vec<_>>();
    for id in &entity_ids {
        let reference = GraphEntityRef::new(identity.clone(), id.clone());
        assert_eq!(
            layered.entity(&reference, cancellation()).unwrap(),
            cold.entity(&reference, cancellation()).unwrap(),
            "entity {id}"
        );
    }
    let mut relation_ids = parent_rows(identity)
        .1
        .into_iter()
        .chain(child_rows(identity).1)
        .map(|relation| relation.identity)
        .collect::<Vec<_>>();
    relation_ids.sort();
    relation_ids.dedup();
    for id in &relation_ids {
        let reference = GraphRelationRef::new(identity.clone(), id.clone());
        assert_eq!(
            layered.relation(&reference, cancellation()).unwrap(),
            cold.relation(&reference, cancellation()).unwrap(),
            "relation {id}"
        );
    }

    let every_kind = BTreeSet::new();
    let calls = BTreeSet::from([GraphRelationKind::new("calls").unwrap()]);
    for kinds in [&every_kind, &calls] {
        assert_eq!(
            layered
                .outgoing_relations(&entity_ids, kinds, 10_000, cancellation())
                .unwrap(),
            cold.outgoing_relations(&entity_ids, kinds, 10_000, cancellation())
                .unwrap()
        );
        assert_eq!(
            layered
                .incoming_relations(&entity_ids, kinds, 10_000, cancellation())
                .unwrap(),
            cold.incoming_relations(&entity_ids, kinds, 10_000, cancellation())
                .unwrap()
        );
        assert_eq!(
            layered
                .outgoing_relation_ids(&entity_ids, kinds, 10_000, cancellation())
                .unwrap(),
            cold.outgoing_relation_ids(&entity_ids, kinds, 10_000, cancellation())
                .unwrap()
        );
        assert_eq!(
            layered
                .incoming_relation_ids(&entity_ids, kinds, 10_000, cancellation())
                .unwrap(),
            cold.incoming_relation_ids(&entity_ids, kinds, 10_000, cancellation())
                .unwrap()
        );
        let targets = |snapshot: &VerifiedGraphSnapshot| {
            snapshot
                .outgoing_relation_targets(&entity_ids, kinds, 10_000, cancellation())
                .unwrap()
                .into_iter()
                .map(|batch| {
                    batch
                        .into_iter()
                        .map(|target| (target.relation, target.target))
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(targets(layered), targets(cold));
    }
    let hubs = [0_usize, 50, 150, 199]
        .map(|index| GraphEntityId::new(format!("entity:{index:03}")).unwrap());
    for max in [1_usize, 4, 60, 500] {
        assert_eq!(
            layered
                .incoming_relations_truncated(&hubs, &every_kind, max, cancellation())
                .unwrap(),
            cold.incoming_relations_truncated(&hubs, &every_kind, max, cancellation())
                .unwrap(),
            "incoming truncated at {max}"
        );
        assert_eq!(
            layered
                .outgoing_relations_truncated(&hubs, &every_kind, max, cancellation())
                .unwrap(),
            cold.outgoing_relations_truncated(&hubs, &every_kind, max, cancellation())
                .unwrap(),
            "outgoing truncated at {max}"
        );
    }
    let first_page = cold
        .incoming_relation_ids_page(&hubs, &every_kind, None, 5, cancellation())
        .unwrap();
    assert_eq!(
        layered
            .incoming_relation_ids_page(&hubs, &every_kind, None, 5, cancellation())
            .unwrap(),
        first_page
    );
    let after = first_page[0].last().unwrap();
    assert_eq!(
        layered
            .incoming_relation_ids_page(&hubs, &every_kind, Some(after), 5, cancellation())
            .unwrap(),
        cold.incoming_relation_ids_page(&hubs, &every_kind, Some(after), 5, cancellation())
            .unwrap()
    );
    assert_eq!(
        layered
            .outgoing_relation_ids_page(&hubs, &every_kind, Some(after), 5, cancellation())
            .unwrap(),
        cold.outgoing_relation_ids_page(&hubs, &every_kind, Some(after), 5, cancellation())
            .unwrap()
    );
    // The hub's fan-in exceeds the budget in both: the same typed refusal.
    assert_eq!(
        layered
            .incoming_relations(&hubs[..1], &every_kind, 3, cancellation())
            .unwrap_err(),
        cold.incoming_relations(&hubs[..1], &every_kind, 3, cancellation())
            .unwrap_err()
    );

    let visit = |snapshot: &VerifiedGraphSnapshot| {
        let mut targets = Vec::new();
        let visited = snapshot
            .visit_outgoing_relation_targets(
                &GraphEntityId::new("entity:199").unwrap(),
                &every_kind,
                cancellation(),
                &mut |target| targets.push((target.relation, target.target)),
            )
            .unwrap();
        targets.sort_by(|left, right| left.0.identity.cmp(&right.0.identity));
        (visited, targets)
    };
    assert_eq!(visit(layered), visit(cold));

    for (start, direction, depth, results) in [
        (205_usize, GraphTraversalDirection::Outgoing, 3, 1_000),
        (0, GraphTraversalDirection::Incoming, 2, 1_000),
        (150, GraphTraversalDirection::Both, 4, 37),
    ] {
        let request = || TraversalRequest {
            namespace: identity.namespace.clone(),
            start: GraphEntityId::new(format!("entity:{start:03}")).unwrap(),
            relation_kinds: BTreeSet::new(),
            direction,
            max_depth: depth,
            max_visits: 10_000,
            max_results: results,
            cancellation: cancellation(),
        };
        assert_eq!(
            layered.traverse(request()).unwrap(),
            cold.traverse(request()).unwrap(),
            "traversal from {start}"
        );
    }
    let telemetry = |snapshot: &VerifiedGraphSnapshot| {
        snapshot
            .projection_telemetry(GraphProjectionTelemetryRequest {
                namespace: identity.namespace.clone(),
                projection: identity.projection.clone(),
                cancellation: cancellation(),
            })
            .unwrap()
    };
    assert_eq!(telemetry(layered), telemetry(cold));
}

/// Fails if the layered digest drifts from the cold build's (the replay
/// could not rebuild it), if any read disagrees, or if the delta container
/// encodes more than the changed rows plus the base endpoints they reach.
#[test]
fn a_layered_generation_serves_and_digests_like_its_cold_build() {
    let identity = projection("sealed-store:layered", "code");
    let parent = manifest(&identity, "layered-g1", parent_rows(&identity));
    let child = manifest(&identity, "layered-g2", child_rows(&identity));

    let cold_root = TempDir::new().unwrap();
    let cold_graph = RegisteredGraph::new_mounted(cold_root.path()).unwrap();
    let mut cold_authority = RelationalAuthority::default();
    let cold_parent = publish_cold(
        &cold_graph,
        cold_root.path(),
        &mut cold_authority,
        &parent,
        None,
        '1',
        None,
    );
    assert!(matches!(
        cold_graph
            .registry
            .sealed_generation_base(
                registration(cold_graph.binding.clone(), cold_root.path()),
                parent.projection.clone(),
                parent.generation.clone(),
                &|| Ok(()),
            )
            .unwrap(),
        Err(GraphSealedBaseAbsenceV1::NoAttachment)
    ));
    let cold_child = publish_cold(
        &cold_graph,
        cold_root.path(),
        &mut cold_authority,
        &child,
        Some(cold_parent.head.clone()),
        '2',
        None,
    );

    let layered_root = TempDir::new().unwrap();
    let layered_graph = RegisteredGraph::new_mounted(layered_root.path()).unwrap();
    let mut layered_authority = RelationalAuthority::default();
    let layered_parent = publish_cold(
        &layered_graph,
        layered_root.path(),
        &mut layered_authority,
        &parent,
        None,
        '1',
        Some(PRODUCER_INPUTS),
    );
    let (layered_child, delta) = publish_layered(
        &layered_graph,
        layered_root.path(),
        &mut layered_authority,
        &parent,
        &child,
        layered_parent.head.clone(),
        '2',
    );

    assert_eq!(
        layered_child.recovered_digest,
        child.expected_recovered_digest(&|| Ok(())).unwrap()
    );
    assert_eq!(layered_child.recovered_digest, cold_child.recovered_digest);
    assert!(layered_child.snapshot.serves_from_sealed_store());
    // 10 new symbols, the changed one, and the 4 unchanged endpoints (000,
    // 150, 170, 199) the 31 new relations reach.
    assert_eq!(delta, (15, 31));
    assert_same_reads(&cold_child.snapshot, &layered_child.snapshot, &identity);
    // The base's verify-once proof binds its container's bytes, which the
    // layer links, so the layer carries it: a restart resolves the base by
    // marker instead of re-proving every base row.
    let sealed_root = support::graph_path(layered_root.path()).with_extension("sealed");
    let parent_directory = sealed_directory_of(&sealed_root, "layered-g1");
    let layered_directory = sealed_directory_of(&sealed_root, "layered-g2");
    assert_eq!(
        std::fs::read(layered_directory.join("base.verified")).unwrap(),
        std::fs::read(parent_directory.join("generation.verified")).unwrap()
    );

    drop((layered_child, layered_parent));
    assert!(layered_graph.close().unwrap());
    drop(layered_graph);
    // Retirement deletes the parent's artifact; the layered store must not
    // depend on anything but its own directory.
    std::fs::remove_dir_all(parent_directory).unwrap();
    // Restart: the sealed-first recovery a cold daemon runs adopts the
    // layered store and re-proves its hard-linked base from disk.
    let layered_graph = RegisteredGraph::new_mounted(layered_root.path()).unwrap();
    let (control, probe) = control_and_probe();
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    let recovered = layered_graph
        .registry
        .recover_verified_sealed_snapshot(
            registration(layered_graph.binding.clone(), layered_root.path()),
            &mut layered_authority,
            &context,
            &cold_child.head.key.projection,
        )
        .unwrap();
    assert!(recovered.serves_from_sealed_store());
    assert_same_reads(&cold_child.snapshot, &recovered, &identity);
}
