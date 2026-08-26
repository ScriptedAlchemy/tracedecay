use grafeo_engine::GrafeoDB;
use sha2::{Digest, Sha256};
use tracedecay_domain::canonical_text::encode_lowercase_hex;
use tracedecay_store::runtime::MAX_GRAPH_REPLAY_SOURCE_BYTES_V1;

use crate::GraphDbError;
use crate::state::{
    EndpointIdentityCache, load_entity_by_node, load_relation_by_locator_cached,
    projection_entity_nodes_sorted_checked, projection_relation_nodes_sorted_checked,
};

use super::{
    CheckedDigestWriter, CheckedVecWriter, GraphGenerationManifest, GraphGenerationRelation,
    physical_namespace_projection_map, recovered_entity_ref, write_canonical_frame,
};

pub(crate) fn recovered_generation_digest_from_database(
    database: &GrafeoDB,
    manifest: &GraphGenerationManifest,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<String, GraphDbError> {
    let mut digest = Sha256::new();
    let mut writer = CheckedDigestWriter::new(&mut digest, check);
    let mut canonical = CheckedVecWriter::new(check, MAX_GRAPH_REPLAY_SOURCE_BYTES_V1)?;
    write_canonical_frame(
        &mut writer,
        &mut canonical,
        "format",
        "tracedecay.graph-generation.v1",
        "recovered generation format",
    )?;
    write_canonical_frame(
        &mut writer,
        &mut canonical,
        "projection",
        &manifest.projection,
        "recovered generation projection",
    )?;
    write_canonical_frame(
        &mut writer,
        &mut canonical,
        "generation",
        &manifest.generation,
        "recovered generation identity",
    )?;
    write_canonical_frame(
        &mut writer,
        &mut canonical,
        "source_generation",
        &manifest.source_generation,
        "recovered source generation",
    )?;
    write_canonical_frame(
        &mut writer,
        &mut canonical,
        "watermark",
        &manifest.watermark,
        "recovered generation watermark",
    )?;
    write_canonical_frame(
        &mut writer,
        &mut canonical,
        "dependencies",
        &manifest.dependencies,
        "recovered generation dependencies",
    )?;

    let physical_namespace = manifest.physical_namespace()?;
    for (_, node) in projection_entity_nodes_sorted_checked(
        database,
        &physical_namespace,
        &manifest.projection.projection,
        check,
    )? {
        check()?;
        let entity = load_entity_by_node(database, node)?.entity;
        write_canonical_frame(
            &mut writer,
            &mut canonical,
            "entity",
            &entity,
            "recovered generation entity",
        )?;
    }

    let namespace_projection = physical_namespace_projection_map(manifest)?;
    let store = database.graph_store();
    let mut endpoints = EndpointIdentityCache::default();
    for (_, locator) in projection_relation_nodes_sorted_checked(
        database,
        &physical_namespace,
        &manifest.projection.projection,
        check,
    )? {
        check()?;
        let stored = load_relation_by_locator_cached(database, locator, &mut endpoints)?;
        let relation = GraphGenerationRelation::new(
            stored.relation.identity,
            recovered_entity_ref(store.as_ref(), stored.source, &namespace_projection)?,
            recovered_entity_ref(store.as_ref(), stored.target, &namespace_projection)?,
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
    writer.finish()?;
    Ok(encode_lowercase_hex(&digest.finalize()))
}
