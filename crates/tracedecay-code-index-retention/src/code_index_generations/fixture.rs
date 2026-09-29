//! Sealed-generation store fixtures shared by retention tests here and in the
//! crates that drive retention (maintenance, storage reports).
#![allow(clippy::expect_used)]

use std::path::Path;

use sha2::{Digest, Sha256};
use tracedecay_code_index::production::SEALED_GENERATION_FORMAT_REVISION_V1;
use tracedecay_domain::canonical_text::encode_tagged_lowercase_hex;
use tracedecay_domain::{CodeGenerationId, sha256_hex_suffix};

use super::{
    ACTIVE_POINTER_FILE, DurableGenerationIndexEntryV1, DurablePublicationPointerV1,
    GENERATIONS_DIRECTORY, durable_generation_index_digest,
};

/// One sealed generation file a fixture store holds.
#[derive(Clone, Debug)]
pub struct GenerationStoreFixtureV1 {
    pub id: CodeGenerationId,
    pub file: String,
    pub state_digest: String,
    pub size_bytes: u64,
}

/// Write `count` sealed generations, sealed at `0..count`, into `store_root`
/// and publish the newest as the active generation. The durable index names
/// only the active generation, so every older one is superseded.
pub fn write_generation_store_fixture(
    store_root: &Path,
    count: usize,
) -> Vec<GenerationStoreFixtureV1> {
    let generations_root = store_root.join(GENERATIONS_DIRECTORY);
    std::fs::create_dir_all(&generations_root).expect("create generation directory");
    let mut generations = Vec::with_capacity(count);

    for sequence in 0..count {
        let generation_id = CodeGenerationId::new(format!("generation.v1.fixture.{sequence:08}"))
            .expect("valid generation id");
        let sealed_at = i64::try_from(sequence).expect("fixture sequence fits i64");
        let bytes = serde_json::to_vec(&serde_json::json!({
            "format_revision": SEALED_GENERATION_FORMAT_REVISION_V1,
            "manifest": {
                "generation_id": generation_id.as_str(),
                "seal": { "sealed_at": sealed_at },
            },
            "chunks": [],
        }))
        .expect("serialize generation fixture");
        let state_digest = encode_tagged_lowercase_hex("sha256:", &Sha256::digest(&bytes));
        let file = format!(
            "generation-{}.json",
            sha256_hex_suffix(&state_digest).expect("digest prefix")
        );
        let size_bytes = u64::try_from(bytes.len()).expect("fixture size fits u64");
        std::fs::write(generations_root.join(&file), bytes).expect("write generation fixture");
        generations.push(GenerationStoreFixtureV1 {
            id: generation_id,
            file,
            state_digest,
            size_bytes,
        });
    }

    let active = generations.last().expect("at least one generation");
    let active_entry = DurableGenerationIndexEntryV1 {
        generation_id: active.id.as_str().to_owned(),
        snapshot_content_identity: "snapshot.fixture".to_owned(),
        sealed_at_micros: i64::try_from(count - 1).expect("fixture sequence fits i64"),
        size_bytes: active.size_bytes,
        segment_bytes: 0,
        generation_file: active.file.clone(),
        state_digest: active.state_digest.clone(),
        source_reference: None,
        source_revision: None,
        source_tree: None,
        cardinality: None,
        text_artifact: None,
    };
    let generation_index = vec![active_entry];
    let generation_index_digest =
        durable_generation_index_digest(&generation_index, true).expect("index digest");
    let pointer = DurablePublicationPointerV1 {
        generation_id: active.id.as_str().to_owned(),
        snapshot_content_identity: "snapshot.fixture".to_owned(),
        publication_digest: "sha256:publication".to_owned(),
        sealed_at_micros: i64::try_from(count - 1).expect("fixture sequence fits i64"),
        generation_file: active.file.clone(),
        state_digest: active.state_digest.clone(),
        generation_index,
        generation_index_truncated: true,
        generation_index_digest: Some(generation_index_digest),
    };
    std::fs::write(
        store_root.join(ACTIVE_POINTER_FILE),
        serde_json::to_vec(&pointer).expect("serialize active pointer"),
    )
    .expect("write active pointer");
    generations
}
