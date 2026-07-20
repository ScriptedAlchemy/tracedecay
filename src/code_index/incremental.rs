//! Deterministic chunk-manifest increment planning (Plan 25, "Code-search
//! chunk and projection contract").
//!
//! This module compares immutable generation chunk manifests by typed chunk
//! identity and content digest. It emits the ordered added/changed, deleted,
//! and reused partitions consumed by projection sinks. File occurrence IDs,
//! source order, and capture hints do not decide reuse. Every input chunk must
//! belong to exactly one declared generation, so mixed snapshots are rejected
//! before a change manifest can cross the projection boundary.

use std::collections::BTreeMap;

use thiserror::Error;
use tracedecay_domain::{
    ChangedCodeChunkSetV1, ChangedCodeChunkV1, CodeGenerationId, CodeSearchChunkId,
    CodeSearchChunkV1, ManifestDigest,
};

use super::chunks::{ChunkingFailureV1, CodeFileChunksV1};

/// Chunk-manifest construction and comparison failures.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum ChunkIncrementErrorV1 {
    #[error("a document or chunk belongs to a different generation")]
    MixedGeneration,
    #[error("the prior and current chunk manifests name the same generation")]
    SameGeneration,
    #[error("chunk {0} occurs more than once in a generation manifest")]
    DuplicateChunk(CodeSearchChunkId),
    #[error("a chunk manifest is not canonical: {0}")]
    NonCanonical(String),
}

/// The canonical chunks produced for one immutable code generation.
///
/// Construction validates every per-file document/chunk binding, rejects
/// mixed-generation rows, flattens files, and orders chunks by typed identity.
/// The fields stay private so downstream diffing can rely on those invariants.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GenerationChunkManifestV1 {
    generation_id: CodeGenerationId,
    chunks: Vec<CodeSearchChunkV1>,
}

impl GenerationChunkManifestV1 {
    /// Construct a canonical generation chunk manifest.
    pub fn new(
        generation_id: CodeGenerationId,
        files: Vec<CodeFileChunksV1>,
    ) -> Result<Self, ChunkIncrementErrorV1> {
        generation_id
            .validate()
            .map_err(|error| ChunkIncrementErrorV1::NonCanonical(error.to_string()))?;

        let capacity = files.iter().map(|file| file.chunks.len()).sum();
        let mut chunks = Vec::with_capacity(capacity);
        for file in files {
            file.validate().map_err(map_chunking_error)?;
            if file.document.generation_id != generation_id {
                return Err(ChunkIncrementErrorV1::MixedGeneration);
            }
            chunks.extend(file.chunks);
        }
        chunks.sort_by(|left, right| left.id.cmp(&right.id));
        if let Some(duplicate) = chunks
            .windows(2)
            .find(|pair| pair[0].id == pair[1].id)
            .map(|pair| pair[0].id.clone())
        {
            return Err(ChunkIncrementErrorV1::DuplicateChunk(duplicate));
        }

        Ok(Self {
            generation_id,
            chunks,
        })
    }

    /// The generation all chunks are anchored to.
    pub fn generation_id(&self) -> &CodeGenerationId {
        &self.generation_id
    }

    /// Chunks in canonical typed-identity order.
    pub fn chunks(&self) -> &[CodeSearchChunkV1] {
        &self.chunks
    }

    /// Look up one chunk by typed identity.
    pub fn chunk(&self, chunk_id: &CodeSearchChunkId) -> Option<&CodeSearchChunkV1> {
        self.chunks
            .binary_search_by(|chunk| chunk.id.cmp(chunk_id))
            .ok()
            .map(|index| &self.chunks[index])
    }
}

/// Compare a prior and current generation's canonical chunks.
///
/// `None` means an initial projection and classifies every current chunk as
/// added. Otherwise equal typed IDs and digests are reused, equal IDs with
/// different digests are updated, current-only IDs are added, and prior-only
/// IDs are deleted. The returned domain manifest is fully validated and its
/// digest is sealed before return.
pub fn plan_chunk_increment(
    prior: Option<&GenerationChunkManifestV1>,
    current: &GenerationChunkManifestV1,
) -> Result<ChangedCodeChunkSetV1, ChunkIncrementErrorV1> {
    if prior.is_some_and(|prior| prior.generation_id == current.generation_id) {
        return Err(ChunkIncrementErrorV1::SameGeneration);
    }

    let prior_by_id: BTreeMap<CodeSearchChunkId, &CodeSearchChunkV1> = prior
        .into_iter()
        .flat_map(|manifest| manifest.chunks.iter())
        .map(|chunk| (chunk.id.clone(), chunk))
        .collect();
    let current_by_id: BTreeMap<CodeSearchChunkId, &CodeSearchChunkV1> = current
        .chunks
        .iter()
        .map(|chunk| (chunk.id.clone(), chunk))
        .collect();

    let mut added_or_changed = Vec::new();
    let mut reused = Vec::new();
    for (chunk_id, chunk) in &current_by_id {
        let change = match prior_by_id.get(chunk_id) {
            None => ChangedCodeChunkV1 {
                chunk_id: chunk_id.clone(),
                prior_digest: None,
                current_digest: Some(chunk.content_digest.clone()),
            },
            Some(prior_chunk) => ChangedCodeChunkV1 {
                chunk_id: chunk_id.clone(),
                prior_digest: Some(prior_chunk.content_digest.clone()),
                current_digest: Some(chunk.content_digest.clone()),
            },
        };
        if change.prior_digest == change.current_digest {
            reused.push(change);
        } else {
            added_or_changed.push(change);
        }
    }

    let mut deleted = Vec::new();
    for (chunk_id, chunk) in prior_by_id {
        if !current_by_id.contains_key(&chunk_id) {
            deleted.push(ChangedCodeChunkV1 {
                chunk_id,
                prior_digest: Some(chunk.content_digest.clone()),
                current_digest: None,
            });
        }
    }

    let mut changes = ChangedCodeChunkSetV1 {
        from_generation: prior.map(|manifest| manifest.generation_id.clone()),
        to_generation: current.generation_id.clone(),
        manifest_digest: placeholder_digest(),
        added_or_changed,
        deleted,
        reused,
    };
    changes.manifest_digest = changes
        .compute_digest()
        .map_err(|error| ChunkIncrementErrorV1::NonCanonical(error.to_string()))?;
    changes
        .validate()
        .map_err(|error| ChunkIncrementErrorV1::NonCanonical(error.to_string()))?;
    Ok(changes)
}

fn map_chunking_error(error: ChunkingFailureV1) -> ChunkIncrementErrorV1 {
    match error {
        ChunkingFailureV1::GenerationMismatch => ChunkIncrementErrorV1::MixedGeneration,
        other => ChunkIncrementErrorV1::NonCanonical(other.to_string()),
    }
}

fn placeholder_digest() -> ManifestDigest {
    ManifestDigest::new(format!("sha256:{}", "0".repeat(64)))
        .expect("a zeroed sha256 digest is canonical")
}
