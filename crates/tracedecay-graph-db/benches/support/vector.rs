use std::collections::{BTreeMap, BTreeSet};
use tracedecay_graph_db::{
    GraphEntity, GraphEntityId, GraphGenerationId, GraphGenerationManifest, GraphNamespace,
    GraphProjectionId, GraphProjectionIdentity, GraphProperty, GraphPropertyName, GraphVector,
    GraphWatermark, SourceGeneration, VectorMetric,
};

pub const VECTOR_NAMESPACE: &str = "benchmark-vector";
pub const VECTOR_PROJECTION: &str = "chunks";
pub const VECTOR_PROPERTY: &str = "embedding";

pub fn vector_manifest(
    entity_count: usize,
    dimension: usize,
    generation: usize,
) -> GraphGenerationManifest {
    let property =
        GraphPropertyName::new(VECTOR_PROPERTY).expect("benchmark vector property name is valid");
    let entities = (0..entity_count)
        .map(|index| {
            GraphEntity::new(
                GraphEntityId::new(format!("chunk:{index:07}"))
                    .expect("benchmark vector entity identity is valid"),
                BTreeSet::new(),
                BTreeMap::from([(
                    property.clone(),
                    GraphProperty::Vector(
                        GraphVector::new(
                            deterministic_vector(index, dimension),
                            dimension,
                            VectorMetric::Cosine,
                        )
                        .expect("benchmark vector is valid"),
                    ),
                )]),
            )
            .expect("benchmark vector entity is valid")
        })
        .collect();
    GraphGenerationManifest::new(
        GraphProjectionIdentity::new(
            GraphNamespace::new(VECTOR_NAMESPACE).expect("benchmark vector namespace is valid"),
            GraphProjectionId::new(VECTOR_PROJECTION)
                .expect("benchmark vector projection is valid"),
        ),
        GraphGenerationId::new(format!("vector-generation:{generation}"))
            .expect("benchmark vector generation identity is valid"),
        SourceGeneration::new(format!("vector-source:{generation}"))
            .expect("benchmark vector source generation is valid"),
        GraphWatermark::new(format!("vector-watermark:{generation}"))
            .expect("benchmark vector watermark is valid"),
        Vec::new(),
        entities,
        Vec::new(),
    )
    .expect("benchmark vector manifest is valid")
}

pub fn deterministic_vector(seed: usize, dimension: usize) -> Vec<f32> {
    // SplitMix64 keeps the million-row fixture reproducible without collapsing
    // it into a small set of repeated vectors that would distort HNSW work.
    let mut state = (seed as u64).wrapping_add(0x9e37_79b9_7f4a_7c15);
    let mut values = (0..dimension)
        .map(|_| {
            state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let mut mixed = state;
            mixed = (mixed ^ (mixed >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            mixed = (mixed ^ (mixed >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            mixed ^= mixed >> 31;
            ((mixed >> 40) as f32 + 1.0) / 16_777_217.0
        })
        .collect::<Vec<_>>();
    let norm = values.iter().map(|value| value * value).sum::<f32>().sqrt();
    for value in &mut values {
        *value /= norm;
    }
    values
}
