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
use std::sync::Arc;

use rayon::prelude::*;
use thiserror::Error;
use tracedecay_domain::{
    ChangedCodeChunkSetV1, ChangedCodeChunkV1, CodeGenerationId, CodeSearchChunkId,
    CodeSearchChunkV1, FileOccurrenceId, ManifestDigest, SymbolOccurrenceId,
};

use super::chunks::{ChunkingFailureV1, CodeFileChunksV1, symbol_occurrence_id};
use super::generations::{FileExtractionActionV1, GenerationIncrementPlanV1};
use super::lineage::{
    GenerationSymbolIndexV1, LineageResolutionErrorV1, LineageSymbolRecordV1,
    SymbolLineageCandidateV1, SymbolLineageResolver,
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
    NonCanonical(crate::noncanonical::NonCanonicalCauseV1),
    #[error("code-index parallel worker runtime failed: {0}")]
    Parallelism(#[from] crate::parallelism::CodeIndexParallelismErrorV1),
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
/// Rows are `Arc`-shared with the per-file artifacts they were flattened
/// from, so the aggregate orders pointers instead of copying the corpus.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GenerationChunkManifestV1 {
    generation_id: CodeGenerationId,
    chunks: Vec<Arc<CodeSearchChunkV1>>,
}

impl GenerationChunkManifestV1 {
    /// Construct a canonical generation chunk manifest from file pages that
    /// have already been validated at extract/rematerialize/share time.
    ///
    /// Skips the corpus-wide `file.validate()` fan-out. File-page
    /// `generation_id` is extraction provenance and may predate this publish.
    pub(crate) fn from_validated_files(
        generation_id: CodeGenerationId,
        files: Vec<CodeFileChunksV1>,
    ) -> Result<Self, ChunkIncrementErrorV1> {
        generation_id
            .validate()
            .map_err(|error| {
                ChunkIncrementErrorV1::NonCanonical(crate::noncanonical::noncanonical_from_domain(
                    error,
                ))
            })?;

        let capacity = files.iter().map(|file| file.chunks.len()).sum();
        let mut chunks = Vec::with_capacity(capacity);
        let mut file_occurrences = BTreeSet::new();
        for file in files {
            file.document
                .generation_id
                .validate()
                .map_err(|error| {
                ChunkIncrementErrorV1::NonCanonical(crate::noncanonical::noncanonical_from_domain(
                    error,
                ))
            })?;
            if !file_occurrences.insert(file.document.file_occurrence_id.clone()) {
                return Err(ChunkIncrementErrorV1::DuplicateFileOccurrence(
                    file.document.file_occurrence_id,
                ));
            }
            chunks.extend(file.chunks);
        }
        crate::parallelism::install(|| chunks.par_sort_by(|left, right| left.id.cmp(&right.id)))?;
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

    /// Wrap an already-sorted, duplicate-free Arc chunk list under a serving
    /// generation id. Callers must keep extraction provenance on the rows.
    pub(crate) fn from_sorted_arcs(
        generation_id: CodeGenerationId,
        chunks: Vec<Arc<CodeSearchChunkV1>>,
    ) -> Result<Self, ChunkIncrementErrorV1> {
        generation_id
            .validate()
            .map_err(|error| {
                ChunkIncrementErrorV1::NonCanonical(crate::noncanonical::noncanonical_from_domain(
                    error,
                ))
            })?;
        if let Some(duplicate) = chunks
            .windows(2)
            .find(|pair| pair[0].id >= pair[1].id)
            .map(|pair| pair[0].id.clone())
        {
            return Err(ChunkIncrementErrorV1::DuplicateChunk(duplicate));
        }
        Ok(Self {
            generation_id,
            chunks,
        })
    }

    /// Construct a canonical generation chunk manifest.
    pub fn new(
        generation_id: CodeGenerationId,
        files: Vec<CodeFileChunksV1>,
    ) -> Result<Self, ChunkIncrementErrorV1> {
        generation_id
            .validate()
            .map_err(|error| ChunkIncrementErrorV1::NonCanonical(crate::noncanonical::noncanonical_from_domain(error)))?;

        // Per-file validation is independent work and dominates a
        // corpus-sized aggregate, so it fans out over the indexing pool
        // instead of running as one serial loop; the first failure in file
        // order is still the one reported. Each file holds one background
        // CPU unit for its whole validation so the nested per-chunk admission
        // inside `validate` reuses it inline instead of taking the process
        // budget lock once per chunk.
        let validated = crate::parallelism::install(|| {
            files
                .par_iter()
                .map(|file| {
                    crate::parallelism::with_background_cpu_permit(|| {
                        file.validate().map_err(map_chunking_error)
                    })
                })
                .collect::<Vec<_>>()
        })?;
        validated.into_iter().collect::<Result<(), _>>()?;

        Self::from_validated_files(generation_id, files)
    }

    /// The generation all chunks are anchored to.
    pub fn generation_id(&self) -> &CodeGenerationId {
        &self.generation_id
    }

    /// Chunks in canonical typed-identity order.
    pub fn chunks(&self) -> &[Arc<CodeSearchChunkV1>] {
        &self.chunks
    }

    /// Look up one chunk by typed identity.
    pub fn chunk(&self, chunk_id: &CodeSearchChunkId) -> Option<&CodeSearchChunkV1> {
        self.chunks
            .binary_search_by(|chunk| chunk.id.cmp(chunk_id))
            .ok()
            .map(|index| self.chunks[index].as_ref())
    }
}

/// Execute the storage-neutral file actions of one generation increment.
///
/// Carry-forward always rematerializes generation-local file and symbol
/// occurrences before constructing the next chunk and lineage manifests.
#[hotpath::measure(label = "code_index.build.increment_materialize")]
pub fn materialize_generation_increment(
    plan: &GenerationIncrementPlanV1,
    generation_id: CodeGenerationId,
    prior_files: &[CodeFileChunksV1],
    reextracted_files: Vec<CodeFileChunksV1>,
    prior_symbols: &GenerationSymbolIndexV1,
    reextracted_symbols: Vec<Arc<LineageSymbolRecordV1>>,
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
    let prior_symbols_by_occurrence = prior_symbols
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
                        crate::noncanonical::NonCanonicalCauseV1::new(
                            crate::noncanonical::NonCanonicalReasonCodeV1::CarryForwardDigestMismatch,
                        ),
                    ));
                }
                let rematerialized_occurrences = prior
                    .chunks
                    .iter()
                    .filter_map(|chunk| chunk.anchor.symbol_occurrence_id.as_ref())
                    .map(|prior_occurrence| {
                        let symbol = prior_symbols_by_occurrence
                            .get(prior_occurrence)
                            .ok_or_else(|| {
                                ChunkIncrementErrorV1::MissingPriorSymbol(prior_occurrence.clone())
                            })?;
                        symbol_occurrence_id(file_occurrence_id, &symbol.identity)
                            .map(|current| (prior_occurrence.clone(), current))
                            .map_err(map_chunking_error)
                    })
                    .collect::<Result<BTreeMap<_, _>, _>>()?;
                let current = prior
                    .rematerialize_for_generation(
                        generation_id.clone(),
                        file_occurrence_id.clone(),
                        &rematerialized_occurrences,
                    )
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
                    let prior_symbol = prior_symbols_by_occurrence
                        .get(&prior_occurrence)
                        .ok_or_else(|| {
                            ChunkIncrementErrorV1::MissingPriorSymbol(prior_occurrence.clone())
                        })?;
                    // The occurrence is rewritten for the new generation, so
                    // this record genuinely diverges from the shared prior row.
                    let mut current_symbol = LineageSymbolRecordV1::clone(prior_symbol);
                    current_symbol.occurrence = current_occurrence;
                    symbols.push(Arc::new(current_symbol));
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
        return Err(ChunkIncrementErrorV1::NonCanonical(crate::noncanonical::NonCanonicalCauseV1::new(crate::noncanonical::NonCanonicalReasonCodeV1::UnplannedReextractedEvidence)));
    }

    let chunks = GenerationChunkManifestV1::new(generation_id.clone(), files)?;
    let symbols =
        GenerationSymbolIndexV1::new(generation_id, symbols).map_err(map_lineage_error)?;
    let lineage = SymbolLineageResolver::new()
        .resolve(prior_symbols, &symbols)
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
#[hotpath::measure(label = "code_index.build.plan_chunk_increment")]
pub fn plan_chunk_increment(
    prior: Option<&GenerationChunkManifestV1>,
    current: &GenerationChunkManifestV1,
) -> Result<ChangedCodeChunkSetV1, ChunkIncrementErrorV1> {
    if prior.is_some_and(|prior| prior.generation_id == current.generation_id) {
        return Err(ChunkIncrementErrorV1::SameGeneration);
    }

    // Manifests already enforce sorted, unique chunk IDs. Merge those rows
    // directly instead of allocating and ordering a second copy of both keys.
    let mut previous = prior
        .into_iter()
        .flat_map(|manifest| &manifest.chunks)
        .peekable();
    let mut added_or_changed = Vec::new();
    let mut reused_pairs = Vec::new();
    let mut deleted = Vec::new();
    for chunk in &current.chunks {
        while let Some(removed) = previous.next_if(|prior| prior.id < chunk.id) {
            deleted.push(ChangedCodeChunkV1 {
                chunk_id: removed.id.clone(),
                prior_digest: Some(removed.content_digest.clone()),
                current_digest: None,
            });
        }
        let prior_digest = previous
            .next_if(|prior| prior.id == chunk.id)
            .map(|prior| prior.content_digest.clone());
        if prior_digest.as_ref() == Some(&chunk.content_digest) {
            reused_pairs.push((chunk.id.clone(), chunk.content_digest.clone()));
        } else {
            added_or_changed.push(ChangedCodeChunkV1 {
                chunk_id: chunk.id.clone(),
                prior_digest,
                current_digest: Some(chunk.content_digest.clone()),
            });
        }
    }
    deleted.extend(previous.map(|removed| ChangedCodeChunkV1 {
        chunk_id: removed.id.clone(),
        prior_digest: Some(removed.content_digest.clone()),
        current_digest: None,
    }));

    let (reused_count, reused_digest) = ChangedCodeChunkSetV1::seal_reused_partition(&reused_pairs)
        .map_err(|error| {
            ChunkIncrementErrorV1::NonCanonical(crate::noncanonical::noncanonical_from_domain(
                error,
            ))
        })?;
    let mut changes = ChangedCodeChunkSetV1 {
        from_generation: prior.map(|manifest| manifest.generation_id.clone()),
        to_generation: current.generation_id.clone(),
        manifest_digest: placeholder_digest(),
        added_or_changed,
        deleted,
        reused_count,
        reused_digest,
    };
    changes.manifest_digest = changes
        .compute_digest()
        .map_err(|error| {
            ChunkIncrementErrorV1::NonCanonical(crate::noncanonical::noncanonical_from_domain(
                error,
            ))
        })?;
    changes
        .validate()
        .map_err(|error| {
            ChunkIncrementErrorV1::NonCanonical(crate::noncanonical::noncanonical_from_domain(
                error,
            ))
        })?;
    Ok(changes)
}

/// Plan an increment when unchanged file pages are Arc-shared from `prior`.
///
/// Shared occurrences reuse by pointer identity (no digest clone on the match
/// path). The reused seal is the parent full-replay attestation plus reused
/// cardinality — parent publish already authenticated those bytes.
#[hotpath::measure(label = "code_index.build.plan_chunk_increment_arc_shared")]
pub(crate) fn plan_chunk_increment_arc_shared(
    prior: &GenerationChunkManifestV1,
    current: &GenerationChunkManifestV1,
    shared_occurrences: &BTreeSet<FileOccurrenceId>,
    parent_full_replay_digest: &ManifestDigest,
) -> Result<ChangedCodeChunkSetV1, ChunkIncrementErrorV1> {
    if prior.generation_id == current.generation_id {
        return Err(ChunkIncrementErrorV1::SameGeneration);
    }
    if shared_occurrences.is_empty() {
        return Err(ChunkIncrementErrorV1::NonCanonical(
            "arc-share increment requires shared file pages".to_owned(),
        ));
    }

    let shared_files = shared_occurrences
        .iter()
        .cloned()
        .collect::<std::collections::HashSet<_>>();
    let mut previous = prior.chunks.iter().peekable();
    let mut added_or_changed = Vec::new();
    let mut reused_count = 0_u64;
    let mut deleted = Vec::new();
    for chunk in &current.chunks {
        while let Some(removed) = previous.next_if(|prior| prior.id < chunk.id) {
            deleted.push(ChangedCodeChunkV1 {
                chunk_id: removed.id.clone(),
                prior_digest: Some(removed.content_digest.clone()),
                current_digest: None,
            });
        }
        let matched = previous.next_if(|prior| prior.id == chunk.id);
        let shared = shared_files.contains(&chunk.anchor.file_occurrence_id);
        match matched {
            Some(prior_chunk)
                if shared
                    || Arc::ptr_eq(prior_chunk, chunk)
                    || prior_chunk.content_digest == chunk.content_digest =>
            {
                reused_count = reused_count.saturating_add(1);
            }
            Some(prior_chunk) => {
                added_or_changed.push(ChangedCodeChunkV1 {
                    chunk_id: chunk.id.clone(),
                    prior_digest: Some(prior_chunk.content_digest.clone()),
                    current_digest: Some(chunk.content_digest.clone()),
                });
            }
            None => {
                added_or_changed.push(ChangedCodeChunkV1 {
                    chunk_id: chunk.id.clone(),
                    prior_digest: None,
                    current_digest: Some(chunk.content_digest.clone()),
                });
            }
        }
    }
    deleted.extend(previous.map(|removed| ChangedCodeChunkV1 {
        chunk_id: removed.id.clone(),
        prior_digest: Some(removed.content_digest.clone()),
        current_digest: None,
    }));

    let shared_file_count = u64::try_from(shared_occurrences.len()).map_err(|_| {
        ChunkIncrementErrorV1::NonCanonical(crate::noncanonical::noncanonical_detail(
            crate::noncanonical::NonCanonicalReasonCodeV1::IdentityValidation,
            "shared file count exceeds u64",
        ))
    })?;
    let (reused_count, reused_digest) = ChangedCodeChunkSetV1::seal_arc_shared_reused_partition(
        parent_full_replay_digest,
        &prior.generation_id,
        &current.generation_id,
        reused_count,
        shared_file_count,
    )
    .map_err(|error| {
        ChunkIncrementErrorV1::NonCanonical(crate::noncanonical::noncanonical_from_domain(error))
    })?;
    let mut changes = ChangedCodeChunkSetV1 {
        from_generation: Some(prior.generation_id.clone()),
        to_generation: current.generation_id.clone(),
        manifest_digest: placeholder_digest(),
        added_or_changed,
        deleted,
        reused_count,
        reused_digest,
    };
    changes.manifest_digest = changes
        .compute_digest()
        .map_err(|error| ChunkIncrementErrorV1::NonCanonical(crate::noncanonical::noncanonical_from_domain(error)))?;
    changes
        .validate()
        .map_err(|error| ChunkIncrementErrorV1::NonCanonical(crate::noncanonical::noncanonical_from_domain(error)))?;
    Ok(changes)
}

fn map_chunking_error(error: ChunkingFailureV1) -> ChunkIncrementErrorV1 {
    match error {
        ChunkingFailureV1::GenerationMismatch => ChunkIncrementErrorV1::MixedGeneration,
        ChunkingFailureV1::NonCanonicalIdentity(cause) => {
            ChunkIncrementErrorV1::NonCanonical(cause)
        }
        other => ChunkIncrementErrorV1::NonCanonical(
            crate::noncanonical::NonCanonicalCauseV1::new(
                crate::noncanonical::NonCanonicalReasonCodeV1::IdentityValidation,
            )
            .with(
                crate::noncanonical::NonCanonicalDetailKeyV1::Detail,
                other.to_string(),
            ),
        ),
    }
}

fn map_lineage_error(error: LineageResolutionErrorV1) -> ChunkIncrementErrorV1 {
    ChunkIncrementErrorV1::NonCanonical(
        crate::noncanonical::NonCanonicalCauseV1::new(
            crate::noncanonical::NonCanonicalReasonCodeV1::IdentityValidation,
        )
        .with(
            crate::noncanonical::NonCanonicalDetailKeyV1::Detail,
            error.to_string(),
        ),
    )
}

fn placeholder_digest() -> ManifestDigest {
    ManifestDigest::new(format!("sha256:{}", "0".repeat(64)))
        .expect("a zeroed sha256 digest is canonical")
}
