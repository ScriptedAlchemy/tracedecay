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
    CheckedDigestWriter, CheckedVecWriter, GraphGenerationManifestIdentity,
    GraphGenerationRelation, physical_namespace_projection_map, recovered_entity_ref,
    write_canonical_frame, write_generation_identity_frames,
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

    let physical_namespace = identity.physical_namespace()?;
    for (_, node) in projection_entity_nodes_sorted_checked(
        database,
        &physical_namespace,
        &identity.projection.projection,
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

    let namespace_projection = physical_namespace_projection_map(identity)?;
    let store = database.graph_store();
    let mut endpoints = EndpointIdentityCache::default();
    for (_, locator) in projection_relation_nodes_sorted_checked(
        database,
        &physical_namespace,
        &identity.projection.projection,
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
    let canonical_bytes = writer.total_bytes();
    writer.finish()?;
    Ok((encode_lowercase_hex(&digest.finalize()), canonical_bytes))
}
