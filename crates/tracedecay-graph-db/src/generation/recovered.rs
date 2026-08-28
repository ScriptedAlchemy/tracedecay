use std::collections::{BTreeMap, HashMap};

use grafeo_common::types::NodeId;
use grafeo_core::graph::GraphStore;
use grafeo_engine::GrafeoDB;
use sha2::{Digest, Sha256};
use tracedecay_domain::canonical_text::encode_lowercase_hex;
use tracedecay_store::runtime::MAX_GRAPH_REPLAY_SOURCE_BYTES_V1;

use crate::schema::decode_entity;
use crate::state::{
    EndpointIdentityCache, load_relation_by_locator_cached,
    projection_entity_nodes_sorted_checked, projection_relation_nodes_sorted_checked,
};
use crate::{GraphDbError, GraphNamespace};

use super::{
    CheckedDigestWriter, CheckedVecWriter, GraphEntityRef, GraphGenerationManifestIdentity,
    GraphGenerationRelation, GraphProjectionIdentity, physical_namespace_projection_map,
    recovered_entity_ref, write_canonical_frame, write_generation_identity_frames,
};

/// Rebuilds the recovered-generation digest by streaming the stored rows.
///
/// Takes only the manifest's identity: every entity and relation frame comes
/// from the database, never from an in-memory manifest row. That is what lets
/// publication release the staged bulk rows before this proof runs.
///
/// Returns the digest and the number of canonical bytes it hashed. The byte
/// count is what the verify gauge reports and what a verified-generation
/// marker records, so a later marker hit can report the same magnitude of work
/// it avoided.
/// Each row costs exactly one storage load: entities decode straight from
/// their enumerated node, and relation endpoints memoize their identity refs
/// so a hub entity resolves once per generation instead of once per incident
/// relation. The digest comparison in `verify_recovered_generation` is the
/// content authority for this proof; per-row unique-key index round-trips
/// contributed no bytes to it and are deliberately absent. Cancellation is
/// polled at least once per row plus every hashed 64 KiB, and no second full
/// row set is ever materialized: the enumeration holds one decoded row at a
/// time plus identity-sized endpoint refs bounded by the generation's
/// distinct endpoint count.
pub(crate) fn recovered_generation_digest_from_database(
    database: &GrafeoDB,
    identity: &GraphGenerationManifestIdentity,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<(String, u64), GraphDbError> {
    let mut digest = Sha256::new();
    let mut writer = CheckedDigestWriter::new(&mut digest, check);
    let mut canonical = CheckedVecWriter::new(check, MAX_GRAPH_REPLAY_SOURCE_BYTES_V1)?;
    write_generation_identity_frames(
        &mut writer,
        &mut canonical,
        &identity.projection,
        &identity.generation,
        &identity.source_generation,
        &identity.watermark,
        &identity.dependencies,
    )?;

    let store = database.graph_store();
    let physical_namespace = identity.physical_namespace()?;
    for (sorted_identity, node) in projection_entity_nodes_sorted_checked(
        database,
        &physical_namespace,
        &identity.projection.projection,
        check,
    )? {
        check()?;
        let record = store.get_node(node).ok_or_else(|| GraphDbError::Corrupt {
            message: "recovered generation entity disappeared during verification".to_owned(),
        })?;
        let entity = decode_entity(&record)?;
        // The sort key and the decoded row are two reads of the same node;
        // a divergence means the row changed under the enumeration and the
        // frames would no longer be hashed in their sorted identity order.
        if entity.identity.as_str() != sorted_identity.as_str() {
            return Err(GraphDbError::Corrupt {
                message: "recovered generation entity identity does not match its enumeration"
                    .to_owned(),
            });
        }
        write_canonical_frame(
            &mut writer,
            &mut canonical,
            "entity",
            &entity,
            "recovered generation entity",
        )?;
    }

    let namespace_projection = physical_namespace_projection_map(identity)?;
    let mut endpoints = EndpointIdentityCache::default();
    let mut endpoint_refs = HashMap::new();
    for (sorted_identity, locator) in projection_relation_nodes_sorted_checked(
        database,
        &physical_namespace,
        &identity.projection.projection,
        check,
    )? {
        check()?;
        let stored = load_relation_by_locator_cached(database, locator, &mut endpoints)?;
        if stored.relation.identity.as_str() != sorted_identity.as_str() {
            return Err(GraphDbError::Corrupt {
                message: "recovered generation relation identity does not match its enumeration"
                    .to_owned(),
            });
        }
        let from = memoized_endpoint_ref(
            store.as_ref(),
            &mut endpoint_refs,
            stored.source,
            &namespace_projection,
        )?;
        let to = memoized_endpoint_ref(
            store.as_ref(),
            &mut endpoint_refs,
            stored.target,
            &namespace_projection,
        )?;
        let relation = GraphGenerationRelation::new(
            stored.relation.identity,
            from,
            to,
            stored.relation.kind,
            stored.relation.properties,
        )?;
        write_canonical_frame(
            &mut writer,
            &mut canonical,
            "relation",
            &relation,
            "recovered generation relation",
        )?;
    }
    let canonical_bytes = writer.total_bytes();
    writer.finish()?;
    Ok((encode_lowercase_hex(&digest.finalize()), canonical_bytes))
}

/// Resolves one relation endpoint to its `GraphEntityRef`, memoized by
/// `NodeId` for the duration of one digest enumeration.
///
/// Hub entities are endpoints of many relations; without the memo every
/// incident relation re-loads the full endpoint node — all properties
/// included — just to extract two identity strings. The memo stores
/// identity-sized refs only, never entity rows, so the verification memory
/// posture is preserved while each distinct endpoint is read at most once.
fn memoized_endpoint_ref(
    store: &dyn GraphStore,
    memo: &mut HashMap<NodeId, GraphEntityRef>,
    node: NodeId,
    namespace_projection: &BTreeMap<GraphNamespace, GraphProjectionIdentity>,
) -> Result<GraphEntityRef, GraphDbError> {
    if let Some(reference) = memo.get(&node) {
        return Ok(reference.clone());
    }
    let reference = recovered_entity_ref(store, node, namespace_projection)?;
    memo.insert(node, reference.clone());
    Ok(reference)
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::collections::{BTreeMap, BTreeSet};
    use std::sync::Arc;

    use super::recovered_generation_digest_from_database;
    use crate::{
        GraphDbError, GraphDbLocation, GraphDbOpenOptions, GraphDbOwner, GraphDurability,
        GraphEntity, GraphEntityId, GraphEntityRef, GraphFormatVersion, GraphGenerationId,
        GraphGenerationManifest, GraphGenerationRelation, GraphLabel, GraphNamespace,
        GraphProjectionId, GraphProjectionIdentity, GraphProperty, GraphPropertyName,
        GraphRelationId, GraphRelationKind, GraphWatermark, NeverCancelled, SourceGeneration,
    };

    fn property_name(name: &str) -> GraphPropertyName {
        GraphPropertyName::new(name).unwrap()
    }

    fn entity_identity(index: u32) -> GraphEntityId {
        GraphEntityId::new(format!("entity:{index:04}")).unwrap()
    }

    /// A generation whose digest exercises every frame ingredient: entities
    /// inserted in reverse identity order with domain labels and every scalar
    /// property type, plus a hub-heavy relation topology so endpoint nodes
    /// repeat across many relations.
    fn fixture_manifest() -> GraphGenerationManifest {
        let projection = GraphProjectionIdentity::new(
            GraphNamespace::new("recovered-digest-probe").unwrap(),
            GraphProjectionId::new("code").unwrap(),
        );
        let entities = (0..96_u32)
            .rev()
            .map(|index| {
                GraphEntity::new(
                    entity_identity(index),
                    BTreeSet::from([
                        GraphLabel::new("function").unwrap(),
                        GraphLabel::new(format!("bucket-{}", index % 7)).unwrap(),
                    ]),
                    BTreeMap::from([
                        (
                            property_name("name"),
                            GraphProperty::String(format!("symbol_{index}")),
                        ),
                        (property_name("arity"), GraphProperty::I64(i64::from(index % 5))),
                        (property_name("exported"), GraphProperty::Bool(index % 2 == 0)),
                        (
                            property_name("score"),
                            GraphProperty::F64(f64::from(index) / 3.0),
                        ),
                        (
                            property_name("fingerprint"),
                            GraphProperty::Bytes(index.to_be_bytes().to_vec()),
                        ),
                    ]),
                )
                .unwrap()
            })
            .collect::<Vec<_>>();
        let entity_ref =
            |index: u32| GraphEntityRef::new(projection.clone(), entity_identity(index));
        let mut relations = Vec::new();
        for index in 1..96_u32 {
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
        for index in 1..95_u32 {
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
            GraphGenerationId::new("generation-digest-probe").unwrap(),
            SourceGeneration::new("source-digest-probe").unwrap(),
            GraphWatermark::new("watermark-digest-probe").unwrap(),
            vec![],
            entities,
            relations,
        )
        .unwrap()
    }

    fn staged_database() -> (GraphDbOwner, crate::GraphDbLeaseV1, GraphGenerationManifest) {
        let manifest = fixture_manifest();
        let owner = GraphDbOwner::open(GraphDbOpenOptions {
            location: GraphDbLocation::Memory,
            expected_format: GraphFormatVersion::current(),
            durability: GraphDurability::Memory,
            cancellation: Arc::new(NeverCancelled),
        })
        .unwrap();
        let database = owner.issue_lease().unwrap();
        database
            .apply_generation_unverified(Arc::new(manifest.clone()), &|| Ok(()))
            .unwrap();
        (owner, database, manifest)
    }

    /// The manifest canonicalization in `generation.rs` is the untouched
    /// digest authority every publication pinned; the streamed enumeration
    /// must reproduce it byte for byte from the stored rows alone.
    #[test]
    fn streamed_digest_is_byte_identical_to_the_manifest_digest() {
        let (_owner, database, manifest) = staged_database();
        let expected = manifest.expected_recovered_digest(&|| Ok(())).unwrap();
        let guard = database.read_guard().unwrap();
        let native = guard.as_ref().unwrap();

        let streamed =
            recovered_generation_digest_from_database(native, &manifest.identity(), &|| Ok(()))
                .unwrap();

        assert_eq!(format!("sha256:{streamed}"), expected.as_str());
    }

    #[test]
    fn streamed_digest_cancels_mid_enumeration() {
        let (_owner, database, manifest) = staged_database();
        let identity = manifest.identity();
        let guard = database.read_guard().unwrap();
        let native = guard.as_ref().unwrap();

        let total_polls = Cell::new(0_usize);
        let counting = || {
            total_polls.set(total_polls.get() + 1);
            Ok(())
        };
        recovered_generation_digest_from_database(native, &identity, &counting).unwrap();
        let total = total_polls.get();
        // The enumeration polls at least once per row; 96 entities and 190
        // relations put any mid-stream trip point far past the handful of
        // polls the leading identity frames consume.
        assert!(total > 300, "expected row-driven poll cadence, saw {total}");

        let cancel_at = total / 2;
        let polls = Cell::new(0_usize);
        let cancelling = || {
            let poll = polls.get() + 1;
            polls.set(poll);
            if poll >= cancel_at {
                Err(GraphDbError::Cancelled)
            } else {
                Ok(())
            }
        };
        assert!(matches!(
            recovered_generation_digest_from_database(native, &identity, &cancelling),
            Err(GraphDbError::Cancelled)
        ));
        assert!(
            polls.get() < total,
            "cancellation must stop the enumeration early: {} polls of {total}",
            polls.get()
        );
    }
}
