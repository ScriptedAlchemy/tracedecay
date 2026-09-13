//! Shard-shaped manifests for publication-scaling measurements: each shard is
//! its own projection, so publishing them in sequence grows the accumulated
//! store the way the real code graph's shard build does.

use std::collections::{BTreeMap, BTreeSet};

use tracedecay_graph_db::{
    GraphEntity, GraphEntityId, GraphEntityRef, GraphGenerationId, GraphGenerationManifest,
    GraphGenerationRelation, GraphNamespace, GraphProjectionId, GraphProjectionIdentity,
    GraphProperty, GraphPropertyName, GraphRelationId, GraphRelationKind, GraphWatermark,
    SourceGeneration,
};

pub fn shard_manifest(
    shard: usize,
    generation: usize,
    entity_count: usize,
    relation_count: usize,
) -> GraphGenerationManifest {
    let projection = GraphProjectionIdentity::new(
        GraphNamespace::new("benchmark-code").expect("benchmark namespace is valid"),
        GraphProjectionId::new(format!("shard-{shard:03}")).expect("benchmark projection is valid"),
    );
    let identity = |index: usize| format!("symbol:{shard:03}:{index:06}");
    let entities = (0..entity_count)
        .map(|index| {
            GraphEntity::new(
                GraphEntityId::new(identity(index)).expect("benchmark entity identity is valid"),
                BTreeSet::new(),
                BTreeMap::from([
                    (
                        GraphPropertyName::new("name").expect("benchmark property name is valid"),
                        GraphProperty::String(format!("fn_{shard}_{index}")),
                    ),
                    (
                        GraphPropertyName::new("revision")
                            .expect("benchmark property name is valid"),
                        GraphProperty::I64(generation as i64),
                    ),
                ]),
            )
            .expect("benchmark entity is valid")
        })
        .collect();
    let relations = (0..relation_count.min(entity_count.saturating_sub(1)))
        .map(|index| {
            GraphGenerationRelation::new(
                GraphRelationId::new(format!("call:{shard:03}:{index:06}"))
                    .expect("benchmark relation identity is valid"),
                GraphEntityRef {
                    projection: projection.clone(),
                    identity: GraphEntityId::new(identity(index))
                        .expect("benchmark source identity is valid"),
                },
                GraphEntityRef {
                    projection: projection.clone(),
                    identity: GraphEntityId::new(identity(index + 1))
                        .expect("benchmark target identity is valid"),
                },
                GraphRelationKind::new("calls").expect("benchmark relation kind is valid"),
                BTreeMap::new(),
            )
            .expect("benchmark relation is valid")
        })
        .collect();
    GraphGenerationManifest::new(
        projection,
        GraphGenerationId::new(format!("generation:{shard:03}:{generation}"))
            .expect("benchmark generation identity is valid"),
        SourceGeneration::new(format!("source:{shard:03}:{generation}"))
            .expect("benchmark source generation is valid"),
        GraphWatermark::new(format!("watermark:{shard:03}:{generation}"))
            .expect("benchmark watermark is valid"),
        Vec::new(),
        entities,
        relations,
    )
    .expect("benchmark generation manifest is valid")
}
