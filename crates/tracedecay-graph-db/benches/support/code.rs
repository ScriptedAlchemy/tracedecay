use std::collections::{BTreeMap, BTreeSet};

use tracedecay_graph_db::{
    GraphEntity, GraphEntityId, GraphGenerationId, GraphGenerationManifest,
    GraphGenerationRelation, GraphNamespace, GraphProjectionId, GraphProjectionIdentity,
    GraphProperty, GraphPropertyName, GraphRelationId, GraphRelationKind, GraphWatermark,
    SourceGeneration,
};

pub const CODE_NAMESPACE: &str = "benchmark-code";
pub const CALLS_RELATION: &str = "calls";

pub fn code_manifest(branching: usize, depth: usize, generation: usize) -> GraphGenerationManifest {
    let projection = GraphProjectionIdentity::new(
        GraphNamespace::new(CODE_NAMESPACE).expect("benchmark namespace is valid"),
        GraphProjectionId::new("symbols").expect("benchmark projection is valid"),
    );
    let mut entities = vec![entity("symbol:root", generation)];
    let mut relations = Vec::new();
    let mut previous = vec!["symbol:root".to_owned()];
    let mut ordinal = 0_usize;
    for level in 1..=depth {
        let mut current = Vec::with_capacity(previous.len() * branching);
        for parent in &previous {
            for child in 0..branching {
                let identity = format!("symbol:{level}:{ordinal:06}");
                entities.push(entity(&identity, generation));
                relations.push(
                    GraphGenerationRelation::new(
                        GraphRelationId::new(format!("call:{level}:{ordinal:06}:{child}"))
                            .expect("benchmark relation identity is valid"),
                        tracedecay_graph_db::GraphEntityRef {
                            projection: projection.clone(),
                            identity: GraphEntityId::new(parent.clone())
                                .expect("benchmark parent identity is valid"),
                        },
                        tracedecay_graph_db::GraphEntityRef {
                            projection: projection.clone(),
                            identity: GraphEntityId::new(identity.clone())
                                .expect("benchmark child identity is valid"),
                        },
                        GraphRelationKind::new(CALLS_RELATION)
                            .expect("benchmark relation kind is valid"),
                        BTreeMap::new(),
                    )
                    .expect("benchmark relation is valid"),
                );
                current.push(identity);
                ordinal += 1;
            }
        }
        previous = current;
    }
    GraphGenerationManifest::new(
        projection,
        GraphGenerationId::new(format!("generation:{generation}"))
            .expect("benchmark generation identity is valid"),
        SourceGeneration::new(format!("source:{generation}"))
            .expect("benchmark source generation is valid"),
        GraphWatermark::new(format!("watermark:{generation}"))
            .expect("benchmark watermark is valid"),
        Vec::new(),
        entities,
        relations,
    )
    .expect("benchmark generation manifest is valid")
}

fn entity(identity: &str, generation: usize) -> GraphEntity {
    GraphEntity::new(
        GraphEntityId::new(identity).expect("benchmark entity identity is valid"),
        BTreeSet::new(),
        BTreeMap::from([(
            GraphPropertyName::new("revision").expect("benchmark property name is valid"),
            GraphProperty::I64(generation as i64),
        )]),
    )
    .expect("benchmark entity is valid")
}
