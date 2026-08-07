use grafeo_engine::GrafeoDB;
use sha2::{Digest, Sha256};
use tracedecay_store::runtime::MAX_GRAPH_REPLAY_SOURCE_BYTES_V1;

use crate::GraphDbError;
use crate::state::{
    load_entity_by_node, load_relation_by_locator, projection_entity_nodes_sorted_checked,
    projection_relation_nodes_sorted_checked,
};

use super::{
    CheckedDigestWriter, GraphGenerationManifest, GraphGenerationRelation, checked_canonical_bytes,
    physical_namespace_projection_map, recovered_entity_ref, write_frame,
};

pub(crate) fn recovered_generation_digest_from_database(
    database: &GrafeoDB,
    manifest: &GraphGenerationManifest,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<String, GraphDbError> {
    let mut digest = Sha256::new();
    let mut writer = CheckedDigestWriter::new(&mut digest, check);
    for (tag, value) in [
        (
            "format",
            checked_canonical_bytes(
                "tracedecay.graph-generation.v1",
                check,
                "recovered generation format",
                MAX_GRAPH_REPLAY_SOURCE_BYTES_V1,
            ),
        ),
        (
            "projection",
            checked_canonical_bytes(
                &manifest.projection,
                check,
                "recovered generation projection",
                MAX_GRAPH_REPLAY_SOURCE_BYTES_V1,
            ),
        ),
        (
            "generation",
            checked_canonical_bytes(
                &manifest.generation,
                check,
                "recovered generation identity",
                MAX_GRAPH_REPLAY_SOURCE_BYTES_V1,
            ),
        ),
        (
            "source_generation",
            checked_canonical_bytes(
                &manifest.source_generation,
                check,
                "recovered source generation",
                MAX_GRAPH_REPLAY_SOURCE_BYTES_V1,
            ),
        ),
        (
            "watermark",
            checked_canonical_bytes(
                &manifest.watermark,
                check,
                "recovered generation watermark",
                MAX_GRAPH_REPLAY_SOURCE_BYTES_V1,
            ),
        ),
        (
            "dependencies",
            checked_canonical_bytes(
                &manifest.dependencies,
                check,
                "recovered generation dependencies",
                MAX_GRAPH_REPLAY_SOURCE_BYTES_V1,
            ),
        ),
    ] {
        write_frame(&mut writer, tag, &value?)?;
    }

    let physical_namespace = manifest.physical_namespace()?;
    for (_, node) in projection_entity_nodes_sorted_checked(
        database,
        &physical_namespace,
        &manifest.projection.projection,
        check,
    )? {
        check()?;
        let entity = load_entity_by_node(database, node)?.entity;
        let bytes = checked_canonical_bytes(
            &entity,
            check,
            "recovered generation entity",
            MAX_GRAPH_REPLAY_SOURCE_BYTES_V1,
        )?;
        write_frame(&mut writer, "entity", &bytes)?;
    }

    let namespace_projection = physical_namespace_projection_map(manifest)?;
    let store = database.graph_store();
    for (_, locator) in projection_relation_nodes_sorted_checked(
        database,
        &physical_namespace,
        &manifest.projection.projection,
        check,
    )? {
        check()?;
        let stored = load_relation_by_locator(database, locator)?;
        let edge = store
            .get_edge(stored.edge)
            .ok_or_else(|| GraphDbError::Corrupt {
                message: "recovered generation relation edge is missing".to_owned(),
            })?;
        let relation = GraphGenerationRelation::new(
            stored.relation.identity,
            recovered_entity_ref(store.as_ref(), edge.src, &namespace_projection)?,
            recovered_entity_ref(store.as_ref(), edge.dst, &namespace_projection)?,
            stored.relation.kind,
            stored.relation.properties,
        )?;
        let bytes = checked_canonical_bytes(
            &relation,
            check,
            "recovered generation relation",
            MAX_GRAPH_REPLAY_SOURCE_BYTES_V1,
        )?;
        write_frame(&mut writer, "relation", &bytes)?;
    }
    writer.finish()?;
    Ok(hex::encode(digest.finalize()))
}
