#![cfg(feature = "test-helpers")]

//! Recovered-generation digest identity over the public verification journey.
//!
//! [`VerifiedGraphSnapshot::memory`] stages the manifest and then proves the
//! digest streamed from the stored rows against the digest pinned from the
//! manifest before installing the head. A single drifted byte in the streamed
//! enumeration fails construction with `GenerationMismatch`, so a successful
//! build plus the head equality below is a byte-exact identity proof.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use tracedecay_graph_db::{
    GraphEntity, GraphEntityId, GraphEntityRef, GraphGenerationId, GraphGenerationManifest,
    GraphGenerationRelation, GraphLabel, GraphNamespace, GraphProjectionId,
    GraphProjectionIdentity, GraphProperty, GraphPropertyName, GraphRelationId, GraphRelationKind,
    GraphWatermark, NeverCancelled, SourceGeneration, VerifiedGraphSnapshot,
};

fn property_name(name: &str) -> GraphPropertyName {
    GraphPropertyName::new(name).unwrap()
}

fn entity_identity(index: u32) -> GraphEntityId {
    GraphEntityId::new(format!("entity:{index:04}")).unwrap()
}

/// Entities carry domain labels and every scalar property type, are handed
/// over in reverse identity order, and the relations form a hub topology so
/// endpoint nodes repeat across many relations.
fn fixture_manifest() -> GraphGenerationManifest {
    let projection = GraphProjectionIdentity::new(
        GraphNamespace::new("recovered-digest-journey").unwrap(),
        GraphProjectionId::new("code").unwrap(),
    );
    let entities = (0..48_u32)
        .rev()
        .map(|index| {
            GraphEntity::new(
                entity_identity(index),
                BTreeSet::from([
                    GraphLabel::new("function").unwrap(),
                    GraphLabel::new(format!("bucket-{}", index % 5)).unwrap(),
                ]),
                BTreeMap::from([
                    (
                        property_name("name"),
                        GraphProperty::String(format!("symbol_{index}")),
                    ),
                    (
                        property_name("arity"),
                        GraphProperty::I64(i64::from(index % 4)),
                    ),
                    (
                        property_name("exported"),
                        GraphProperty::Bool(index % 2 == 0),
                    ),
                    (
                        property_name("score"),
                        GraphProperty::F64(f64::from(index) / 7.0),
                    ),
                    (
                        property_name("fingerprint"),
                        GraphProperty::Bytes(index.to_le_bytes().to_vec()),
                    ),
                ]),
            )
            .unwrap()
        })
        .collect::<Vec<_>>();
    let entity_ref = |index: u32| GraphEntityRef::new(projection.clone(), entity_identity(index));
    let mut relations = Vec::new();
    for index in 1..48_u32 {
        relations.push(
            GraphGenerationRelation::new(
                GraphRelationId::new(format!("relation:hub:{index:04}")).unwrap(),
                entity_ref(index),
                entity_ref(0),
                GraphRelationKind::new("calls").unwrap(),
                BTreeMap::from([(
                    property_name("weight"),
                    GraphProperty::I64(i64::from(index)),
                )]),
            )
            .unwrap(),
        );
    }
    for index in 1..47_u32 {
        relations.push(
            GraphGenerationRelation::new(
                GraphRelationId::new(format!("relation:chain:{index:04}")).unwrap(),
                entity_ref(index),
                entity_ref(index + 1),
                GraphRelationKind::new("references").unwrap(),
                BTreeMap::new(),
            )
            .unwrap(),
        );
    }
    GraphGenerationManifest::new(
        projection,
        GraphGenerationId::new("generation-digest-journey").unwrap(),
        SourceGeneration::new("source-digest-journey").unwrap(),
        GraphWatermark::new("watermark-digest-journey").unwrap(),
        vec![],
        entities,
        relations,
    )
    .unwrap()
}

#[test]
fn published_recovered_digest_matches_the_manifest_pinned_digest() {
    let manifest = fixture_manifest();
    let expected = manifest.expected_recovered_digest(&|| Ok(())).unwrap();

    let snapshot = VerifiedGraphSnapshot::memory(manifest, Arc::new(NeverCancelled))
        .expect("streamed row digest must reproduce the manifest digest byte for byte");

    assert_eq!(
        snapshot.verified_head().recovered_digest.as_str(),
        expected.as_str()
    );
}
