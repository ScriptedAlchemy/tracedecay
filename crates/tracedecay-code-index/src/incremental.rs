//! Deterministic chunk-manifest increment planning (Plan 25, "Code-search
//! chunk and projection contract").
//!
//! This module compares immutable generation chunk manifests by typed chunk
//! identity and content digest. It emits the ordered added/changed, deleted,
//! and reused partitions consumed by projection sinks. File occurrence IDs,
//! source order, and capture hints do not decide reuse. Every input chunk must
//! belong to exactly one declared generation, so mixed snapshots are rejected
//! before a change manifest can cross the projection boundary.

use std::collections::{BTreeMap, BTreeSet};

use thiserror::Error;
use tracedecay_domain::{
    ChangedCodeChunkSetV1, ChangedCodeChunkV1, CodeGenerationId, CodeSearchChunkId,
    CodeSearchChunkV1, FileOccurrenceId, ManifestDigest, SymbolLineageCandidateV1,
    SymbolOccurrenceId,
};

use super::chunks::{ChunkingFailureV1, CodeFileChunksV1};
use super::generations::{FileExtractionActionV1, GenerationIncrementPlanV1};
use super::lineage::{
    GenerationSymbolIndexV1, LineageResolutionErrorV1, LineageSymbolRecordV1, SymbolLineageResolver,
};

/// Chunk-manifest construction and comparison failures.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum ChunkIncrementErrorV1 {
    #[error("a document or chunk belongs to a different generation")]
    MixedGeneration,
    #[error("the prior and current chunk manifests name the same generation")]
    SameGeneration,
    #[error("chunk {0} occurs more than once in a generation manifest")]
    DuplicateChunk(CodeSearchChunkId),
    #[error("file occurrence {0} occurs more than once in generation evidence")]
    DuplicateFileOccurrence(FileOccurrenceId),
    #[error("symbol occurrence {0} occurs more than once in re-extracted evidence")]
    DuplicateReextractedSymbol(SymbolOccurrenceId),
    #[error("a chunk manifest is not canonical: {0}")]
    NonCanonical(String),
    #[error("the increment plan does not match the supplied prior generation")]
    PriorGenerationMismatch,
    #[error("the increment plan references missing prior file occurrence {0}")]
    MissingPriorFile(FileOccurrenceId),
    #[error("the increment plan references missing re-extracted file occurrence {0}")]
    MissingReextractedFile(FileOccurrenceId),
    #[error("the increment plan references missing prior symbol occurrence {0}")]
    MissingPriorSymbol(SymbolOccurrenceId),
    #[error("the re-extracted chunks reference missing symbol occurrence {0}")]
    MissingReextractedSymbol(SymbolOccurrenceId),
}

/// Canonical outputs of executing one generation increment plan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GenerationIncrementMaterializationV1 {
    pub chunks: GenerationChunkManifestV1,
    pub symbols: GenerationSymbolIndexV1,
    pub lineage: Vec<SymbolLineageCandidateV1>,
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
        let mut file_occurrences = BTreeSet::new();
        for file in files {
            file.validate().map_err(map_chunking_error)?;
            if file.document.generation_id != generation_id {
                return Err(ChunkIncrementErrorV1::MixedGeneration);
            }
            if !file_occurrences.insert(file.document.file_occurrence_id.clone()) {
                return Err(ChunkIncrementErrorV1::DuplicateFileOccurrence(
                    file.document.file_occurrence_id,
                ));
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

/// Execute the storage-neutral file actions of one generation increment.
///
/// Carry-forward always rematerializes generation-local file and symbol
/// occurrences before constructing the next chunk and lineage manifests.
pub fn materialize_generation_increment(
    plan: &GenerationIncrementPlanV1,
    generation_id: CodeGenerationId,
    prior_files: &[CodeFileChunksV1],
    reextracted_files: Vec<CodeFileChunksV1>,
    prior_symbols: &GenerationSymbolIndexV1,
    reextracted_symbols: Vec<LineageSymbolRecordV1>,
) -> Result<GenerationIncrementMaterializationV1, ChunkIncrementErrorV1> {
    if prior_symbols.generation_id != plan.prior_generation
        || prior_files
            .iter()
            .any(|file| file.document.generation_id != plan.prior_generation)
    {
        return Err(ChunkIncrementErrorV1::PriorGenerationMismatch);
    }

    let mut prior_files_by_occurrence = BTreeMap::new();
    for file in prior_files {
        if prior_files_by_occurrence
            .insert(file.document.file_occurrence_id.clone(), file)
            .is_some()
        {
            return Err(ChunkIncrementErrorV1::DuplicateFileOccurrence(
                file.document.file_occurrence_id.clone(),
            ));
        }
    }
    let prior_files = prior_files_by_occurrence;
    let mut reextracted_files_by_occurrence = BTreeMap::new();
    for file in reextracted_files {
        let file_occurrence_id = file.document.file_occurrence_id.clone();
        if reextracted_files_by_occurrence
            .insert(file_occurrence_id.clone(), file)
            .is_some()
        {
            return Err(ChunkIncrementErrorV1::DuplicateFileOccurrence(
                file_occurrence_id,
            ));
        }
    }
    let mut reextracted_files = reextracted_files_by_occurrence;
    let prior_symbols = prior_symbols
        .symbols
        .iter()
        .map(|symbol| (symbol.occurrence.clone(), symbol))
        .collect::<BTreeMap<_, _>>();
    let mut reextracted_symbols_by_occurrence = BTreeMap::new();
    for symbol in reextracted_symbols {
        let occurrence = symbol.occurrence.clone();
        if reextracted_symbols_by_occurrence
            .insert(occurrence.clone(), symbol)
            .is_some()
        {
            return Err(ChunkIncrementErrorV1::DuplicateReextractedSymbol(
                occurrence,
            ));
        }
    }
    let mut reextracted_symbols = reextracted_symbols_by_occurrence;

    let mut files = Vec::new();
    let mut symbols = Vec::new();
    for file_plan in &plan.files {
        match &file_plan.action {
            FileExtractionActionV1::CarryForward {
                file_occurrence_id,
                prior_file_occurrence_id,
                content_digest,
            } => {
                let prior = prior_files.get(prior_file_occurrence_id).ok_or_else(|| {
                    ChunkIncrementErrorV1::MissingPriorFile(prior_file_occurrence_id.clone())
                })?;
                if &prior.document.content_digest != content_digest {
                    return Err(ChunkIncrementErrorV1::NonCanonical(
                        "carry-forward content digest does not match prior chunks".to_owned(),
                    ));
                }
                let current = prior
                    .rematerialize_for_generation(generation_id.clone(), file_occurrence_id.clone())
                    .map_err(map_chunking_error)?;
                let occurrence_map = prior
                    .chunks
                    .iter()
                    .zip(&current.chunks)
                    .filter_map(|(prior, current)| {
                        prior
                            .anchor
                            .symbol_occurrence_id
                            .as_ref()
                            .zip(current.anchor.symbol_occurrence_id.as_ref())
                    })
                    .map(|(prior, current)| (prior.clone(), current.clone()))
                    .collect::<BTreeMap<_, _>>();
                for (prior_occurrence, current_occurrence) in occurrence_map {
                    let prior_symbol = prior_symbols.get(&prior_occurrence).ok_or_else(|| {
                        ChunkIncrementErrorV1::MissingPriorSymbol(prior_occurrence.clone())
                    })?;
                    let mut current_symbol = (*prior_symbol).clone();
                    current_symbol.occurrence = current_occurrence;
                    symbols.push(current_symbol);
                }
                files.push(current);
            }
            FileExtractionActionV1::ReExtract { file } => {
                let current = reextracted_files
                    .remove(&file.file_occurrence_id)
                    .ok_or_else(|| {
                        ChunkIncrementErrorV1::MissingReextractedFile(
                            file.file_occurrence_id.clone(),
                        )
                    })?;
                if current.document.generation_id != generation_id
                    || current.document.content_digest != file.content_digest
                {
                    return Err(ChunkIncrementErrorV1::MixedGeneration);
                }
                let occurrences = current
                    .chunks
                    .iter()
                    .filter_map(|chunk| chunk.anchor.symbol_occurrence_id.clone())
                    .collect::<BTreeSet<_>>();
                for occurrence in occurrences {
                    let symbol = reextracted_symbols.remove(&occurrence).ok_or_else(|| {
                        ChunkIncrementErrorV1::MissingReextractedSymbol(occurrence.clone())
                    })?;
                    symbols.push(symbol);
                }
                files.push(current);
            }
            FileExtractionActionV1::Deleted { .. } => {}
        }
    }
    if !reextracted_files.is_empty() || !reextracted_symbols.is_empty() {
        return Err(ChunkIncrementErrorV1::NonCanonical(
            "unplanned re-extracted generation evidence was supplied".to_owned(),
        ));
    }

    let chunks = GenerationChunkManifestV1::new(generation_id.clone(), files)?;
    let symbols =
        GenerationSymbolIndexV1::new(generation_id, symbols).map_err(map_lineage_error)?;
    let lineage = SymbolLineageResolver::new()
        .resolve(
            &GenerationSymbolIndexV1::new(
                plan.prior_generation.clone(),
                prior_symbols
                    .values()
                    .map(|symbol| (*symbol).clone())
                    .collect(),
            )
            .map_err(map_lineage_error)?,
            &symbols,
        )
        .map_err(map_lineage_error)?;
    Ok(GenerationIncrementMaterializationV1 {
        chunks,
        symbols,
        lineage,
    })
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

fn map_lineage_error(error: LineageResolutionErrorV1) -> ChunkIncrementErrorV1 {
    ChunkIncrementErrorV1::NonCanonical(error.to_string())
}

fn placeholder_digest() -> ManifestDigest {
    ManifestDigest::new(format!("sha256:{}", "0".repeat(64)))
        .expect("a zeroed sha256 digest is canonical")
}
