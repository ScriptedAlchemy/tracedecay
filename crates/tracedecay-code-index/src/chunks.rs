//! Deterministic chunker port (Plan 25, "Code-search chunk and projection
//! contract"): build the five-grain chunks and their parent/child hierarchy
//! from one extraction batch.
//!
//! Every eligible sanitized byte is covered by a declared chunk or an
//! explicit unsupported/excluded range. Oversized bodies split on
//! deterministic structural boundaries; if none are available, fixed byte
//! windows with pinned size/overlap are used. Extractor enumeration order
//! and mutable line numbers cannot affect `CodeSearchChunkId`.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_domain::{
    BoundedSanitizedText, CanonicalRelationEdgeV1, ChunkLogicalIdentityV1, ChunkerRevision,
    CodeGenerationId, CodeSearchChunkAnchorV1, CodeSearchChunkGrainV1, CodeSearchChunkId,
    CodeSearchChunkV1, CodeSearchDocumentV1, CodeSearchEligibilityV1, EdgeAuthorityV1,
    ExactTechnicalTermKindV1, ExactTechnicalTermV1, ExtractionAdmittedChunkV1, ExtractionBatchV1,
    FileIdentityDigest, FileOccurrenceId, LanguageDescriptorV1, MAX_CHUNK_TEXT_BYTES,
    ParseOutcomeV1, PolicyRevisionId, RelationEdgeKindV1, RepositoryId, SanitizerRevision,
    SensitivityDecision, SensitivityLevelV1, SourceSpan, SymbolIdentityDigest, SymbolOccurrenceId,
    ValidatedCodeFileV1, canonical_sha256,
};

use super::{
    extract::{ExtractedCodeFileV1, ExtractionCancellation},
    intake::ReceiptBoundCodeFileV1,
    lineage::LineageSymbolRecordV1,
};
use tracedecay_domain::{Edge, EdgeKind, ExtractionResult, Node, NodeKind, UnresolvedRef};

/// Chunker failures. Partial coverage is evidence, not an error; errors are
/// reserved for contract violations.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum ChunkingFailureV1 {
    #[error("the descriptor does not match the extraction batch language")]
    DescriptorMismatch,
    #[error("the extraction batch is not generation-consistent with the document")]
    GenerationMismatch,
    #[error("chunking was cancelled")]
    Cancelled,
    #[error("chunk identity inputs are not canonical: {0}")]
    NonCanonicalIdentity(String),
}

/// The deterministic chunker contract (Plan 25: `src/code_index/chunks.rs`
/// builds chunks and their parent/child hierarchy).
pub trait CodeChunker {
    /// Build every chunk for one receipt-bound file plus its extraction batch,
    /// covering every eligible sanitized byte with a declared chunk or an
    /// explicit unsupported/excluded range.
    fn chunk_file(
        &self,
        file: &ReceiptBoundCodeFileV1,
        batch: &ExtractionBatchV1,
        descriptor: &LanguageDescriptorV1,
        cancellation: &dyn ExtractionCancellation,
    ) -> Result<CodeFileChunksV1, ChunkingFailureV1>;
}

/// The chunks produced for one file: the generation-bound document manifest
/// plus its chunks in deterministic order (Plan 25).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CodeFileChunksV1 {
    pub document: CodeSearchDocumentV1,
    pub chunks: Vec<CodeSearchChunkV1>,
}

/// Parser-backed evidence for one indexed file. The canonical relation rows
/// contain only relation kinds the Plan 25 graph contract can represent;
/// everything else remains a typed abstention rather than a synthetic edge.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CodeFileIndexArtifactsV1 {
    pub chunks: CodeFileChunksV1,
    pub symbols: Vec<LineageSymbolRecordV1>,
    pub edges: Vec<CanonicalRelationEdgeV1>,
    pub edge_abstentions: Vec<CodeIndexEdgeAbstentionV1>,
}

/// Why one parser relation was not promoted into the canonical graph lane.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum CodeIndexEdgeAbstentionReasonV1 {
    MissingSymbolEndpoint,
    UnsupportedRelationKind,
}

/// A parser-observed edge that remains explicitly unavailable to graph
/// traversal. This preserves the raw limitation without inventing a
/// semantically stronger relation.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(deny_unknown_fields)]
pub struct CodeIndexEdgeAbstentionV1 {
    pub source_node_id: String,
    pub target_node_id: String,
    pub legacy_kind: String,
    pub reason: CodeIndexEdgeAbstentionReasonV1,
}

/// Opaque capability proving the exact chunk bytes produced by one
/// parser-backed extraction. It has no public constructor.
///
/// ```compile_fail
/// use tracedecay_code_index::chunks::ExactExtractionAuthorityV1;
///
/// let _forged = ExactExtractionAuthorityV1::mint(&[]);
/// ```
#[derive(Clone, Debug)]
pub struct ExactExtractionAuthorityV1 {
    chunk_digests: BTreeMap<CodeSearchChunkId, String>,
}

/// One chunk re-admitted through parser-backed extraction authority.
///
/// ```compile_fail
/// use tracedecay_code_index::chunks::ExtractionAdmittedCodeSearchChunkV1;
///
/// let chunk = todo!();
/// let _forged = ExtractionAdmittedCodeSearchChunkV1 { chunk };
/// ```
#[derive(Clone, Debug)]
pub struct ExtractionAdmittedCodeSearchChunkV1 {
    chunk: CodeSearchChunkV1,
}

impl ExtractionAdmittedCodeSearchChunkV1 {
    pub fn chunk(&self) -> &CodeSearchChunkV1 {
        &self.chunk
    }

    /// Consume the authority-bearing wrapper and return its admitted chunk.
    pub fn into_chunk(self) -> CodeSearchChunkV1 {
        self.chunk
    }
}

// SAFETY: values are only created by `ExactExtractionAuthorityV1::admit`,
// after the parser-backed chunk digest has been validated.
unsafe impl ExtractionAdmittedChunkV1 for ExtractionAdmittedCodeSearchChunkV1 {
    fn into_admitted_chunk(self) -> CodeSearchChunkV1 {
        self.chunk
    }
}

/// Chunk counts below this stay on the calling thread. One canonical chunk
/// digest costs single-digit microseconds, so small files are cheaper inline
/// than split across the pool — and leaving them sequential keeps the pool free
/// for the coarser per-file fan-out above this layer.
const PARALLEL_CHUNK_THRESHOLD: usize = 16;

/// Map `operation` over every chunk, fanning out across the pool once the batch
/// is large enough. Results are returned in chunk order and the reported
/// failure is always the lowest-index one, so the outcome is identical to the
/// sequential sweep this replaces.
fn map_chunks_ordered<T, F>(
    chunks: &[CodeSearchChunkV1],
    operation: F,
) -> Result<Vec<T>, ChunkingFailureV1>
where
    T: Send,
    F: Fn(&CodeSearchChunkV1) -> Result<T, ChunkingFailureV1> + Send + Sync,
{
    if chunks.len() < PARALLEL_CHUNK_THRESHOLD {
        return chunks.iter().map(operation).collect();
    }
    let results: Vec<Result<T, ChunkingFailureV1>> =
        chunks.par_iter().map(operation).collect::<Vec<_>>();
    results.into_iter().collect()
}

/// Run `operation` over every chunk for its failure only, fanning out across
/// the pool once the batch is large enough. The lowest-index failure is
/// returned, matching the sequential sweep's short-circuit outcome.
fn try_for_each_chunk_ordered<F>(
    chunks: &[CodeSearchChunkV1],
    operation: F,
) -> Result<(), ChunkingFailureV1>
where
    F: Fn(&CodeSearchChunkV1) -> Result<(), ChunkingFailureV1> + Send + Sync,
{
    if chunks.len() < PARALLEL_CHUNK_THRESHOLD {
        return chunks.iter().try_for_each(operation);
    }
    let failure = chunks
        .par_iter()
        .enumerate()
        .filter_map(|(index, chunk)| operation(chunk).err().map(|error| (index, error)))
        .min_by_key(|(index, _)| *index);
    match failure {
        Some((_, error)) => Err(error),
        None => Ok(()),
    }
}

impl ExactExtractionAuthorityV1 {
    fn mint(chunks: &[CodeSearchChunkV1]) -> Result<Self, ChunkingFailureV1> {
        let digests = map_chunks_ordered(chunks, |chunk| {
            canonical_digest(EXACT_EXTRACTION_AUTHORITY_SEPARATOR, chunk)
        })?;
        let mut chunk_digests = BTreeMap::new();
        for (chunk, digest) in chunks.iter().zip(digests) {
            chunk_digests.insert(chunk.id.clone(), digest);
        }
        Ok(Self { chunk_digests })
    }

    /// Reconstruct parser-backed exact admission from a sealed file artifact.
    ///
    /// The durable generation decoder validates the complete extraction,
    /// chunk, manifest, receipt, and capability graph before exposing this
    /// authority. Recomputing digests here avoids persisting forgeable
    /// authority internals.
    ///
    /// ```compile_fail
    /// use tracedecay_code_index::chunks::ExactExtractionAuthorityV1;
    ///
    /// let sealed_chunks = todo!();
    /// let _forged = ExactExtractionAuthorityV1::restore(sealed_chunks);
    /// ```
    pub(crate) fn restore(chunks: &CodeFileChunksV1) -> Result<Self, ChunkingFailureV1> {
        chunks.validate()?;
        Self::mint(&chunks.chunks)
    }

    fn validate_chunk(&self, chunk: &CodeSearchChunkV1) -> Result<(), ChunkingFailureV1> {
        chunk
            .validate()
            .map_err(|error| ChunkingFailureV1::NonCanonicalIdentity(error.to_string()))?;
        let digest = canonical_digest(EXACT_EXTRACTION_AUTHORITY_SEPARATOR, chunk)?;
        if self.chunk_digests.get(&chunk.id) != Some(&digest) {
            return Err(ChunkingFailureV1::NonCanonicalIdentity(
                "chunk does not match parser-backed exact extraction authority".to_owned(),
            ));
        }
        Ok(())
    }

    pub(crate) fn validate_all(
        &self,
        chunks: &[CodeSearchChunkV1],
    ) -> Result<(), ChunkingFailureV1> {
        if chunks.len() != self.chunk_digests.len() {
            return Err(ChunkingFailureV1::NonCanonicalIdentity(
                "chunk set does not match parser-backed exact extraction authority".to_owned(),
            ));
        }
        let mut seen = BTreeSet::new();
        let repeated_at = chunks
            .iter()
            .position(|chunk| !seen.insert(&chunk.id))
            .unwrap_or(chunks.len());
        // The sequential sweep stopped at the first repeated identity, so only
        // the chunks ahead of it were ever digest-checked.
        try_for_each_chunk_ordered(&chunks[..repeated_at], |chunk| self.validate_chunk(chunk))?;
        if repeated_at < chunks.len() {
            return Err(ChunkingFailureV1::NonCanonicalIdentity(
                "chunk set repeats parser-backed exact extraction identity".to_owned(),
            ));
        }
        Ok(())
    }

    pub fn admit(
        &self,
        chunk: CodeSearchChunkV1,
    ) -> Result<ExtractionAdmittedCodeSearchChunkV1, ChunkingFailureV1> {
        self.validate_chunk(&chunk)?;
        Ok(ExtractionAdmittedCodeSearchChunkV1 { chunk })
    }

    pub fn admit_all(
        &self,
        chunks: Vec<CodeSearchChunkV1>,
    ) -> Result<Vec<ExtractionAdmittedCodeSearchChunkV1>, ChunkingFailureV1> {
        if chunks.len() < PARALLEL_CHUNK_THRESHOLD {
            return chunks.into_iter().map(|chunk| self.admit(chunk)).collect();
        }
        let admitted = chunks
            .into_par_iter()
            .map(|chunk| self.admit(chunk))
            .collect::<Vec<_>>();
        admitted.into_iter().collect()
    }

    /// Rebind an exact authority only after every prior parser-backed chunk
    /// has been verified and the carried chunks preserve logical identity and
    /// content digest. This is the restart/incremental bridge for the
    /// non-demotable exact and lexical lanes.
    pub fn rematerialize_for_generation(
        &self,
        prior: &CodeFileChunksV1,
        current: &CodeFileChunksV1,
    ) -> Result<Self, ChunkingFailureV1> {
        prior.validate()?;
        current.validate()?;
        if prior.chunks.len() != current.chunks.len()
            || prior
                .chunks
                .iter()
                .zip(&current.chunks)
                .any(|(prior, current)| {
                    prior.id != current.id || prior.content_digest != current.content_digest
                })
        {
            return Err(ChunkingFailureV1::NonCanonicalIdentity(
                "carried exact chunks changed logical identity or content".to_owned(),
            ));
        }
        self.validate_all(&prior.chunks)?;
        Self::mint(&current.chunks)
    }
}

impl CodeFileChunksV1 {
    /// Validate the generation/file binding and canonical document membership
    /// of one chunker result before it can cross the publication boundary.
    pub fn validate(&self) -> Result<(), ChunkingFailureV1> {
        self.document
            .generation_id
            .validate()
            .map_err(|error| ChunkingFailureV1::NonCanonicalIdentity(error.to_string()))?;
        self.document
            .file_occurrence_id
            .validate()
            .map_err(|error| ChunkingFailureV1::NonCanonicalIdentity(error.to_string()))?;
        self.document
            .content_digest
            .validate()
            .map_err(|error| ChunkingFailureV1::NonCanonicalIdentity(error.to_string()))?;

        if self.document.chunk_ids.len() != self.chunks.len()
            || self
                .document
                .chunk_ids
                .iter()
                .zip(&self.chunks)
                .any(|(document_id, chunk)| document_id != &chunk.id)
        {
            return Err(ChunkingFailureV1::NonCanonicalIdentity(
                "document chunk membership does not match canonical chunk order".to_owned(),
            ));
        }
        try_for_each_chunk_ordered(&self.chunks, |chunk| {
            if chunk.anchor.generation_id != self.document.generation_id
                || chunk.anchor.file_occurrence_id != self.document.file_occurrence_id
            {
                return Err(ChunkingFailureV1::GenerationMismatch);
            }
            chunk
                .validate()
                .map_err(|error| ChunkingFailureV1::NonCanonicalIdentity(error.to_string()))
        })
    }

    /// Rebind carried-forward chunks to their next generation without
    /// changing logical chunk identity or content evidence. Symbol occurrence
    /// IDs are rematerialized from the prior exact occurrence so they cannot
    /// cross the generation boundary.
    pub fn rematerialize_for_generation(
        &self,
        generation_id: CodeGenerationId,
        file_occurrence_id: FileOccurrenceId,
    ) -> Result<Self, ChunkingFailureV1> {
        self.validate()?;
        if self.document.generation_id == generation_id
            && self.document.file_occurrence_id == file_occurrence_id
        {
            return Ok(self.clone());
        }

        let mut rematerialized = self.clone();
        rematerialized.document.generation_id = generation_id.clone();
        rematerialized.document.file_occurrence_id = file_occurrence_id.clone();

        let mut occurrences: BTreeMap<SymbolOccurrenceId, SymbolOccurrenceId> = BTreeMap::new();
        for chunk in &mut rematerialized.chunks {
            chunk.anchor.generation_id = generation_id.clone();
            chunk.anchor.file_occurrence_id = file_occurrence_id.clone();
            if let Some(prior_occurrence) = chunk.anchor.symbol_occurrence_id.clone() {
                let current_occurrence = if let Some(current) = occurrences.get(&prior_occurrence) {
                    current.clone()
                } else {
                    let current = rematerialized_symbol_occurrence_id(
                        &generation_id,
                        &file_occurrence_id,
                        &prior_occurrence,
                    )?;
                    occurrences.insert(prior_occurrence, current.clone());
                    current
                };
                chunk.anchor.symbol_occurrence_id = Some(current_occurrence.clone());
                for term in &mut chunk.exact_terms {
                    if term.kind() == ExactTechnicalTermKindV1::WholeSymbol {
                        term.rebind_symbol_occurrence(current_occurrence.clone())
                            .map_err(|error| {
                                ChunkingFailureV1::NonCanonicalIdentity(error.to_string())
                            })?;
                    }
                }
            }
        }

        rematerialized.validate()?;
        Ok(rematerialized)
    }
}

impl CodeFileIndexArtifactsV1 {
    /// Verify that every lineage and graph row is bound to a parser-backed
    /// symbol occurrence in this file's immutable chunk generation.
    pub fn validate(&self) -> Result<(), ChunkingFailureV1> {
        self.chunks.validate()?;
        if self
            .symbols
            .windows(2)
            .any(|pair| pair[0].occurrence >= pair[1].occurrence)
        {
            return Err(ChunkingFailureV1::NonCanonicalIdentity(
                "lineage symbols are not in occurrence order".to_owned(),
            ));
        }
        let chunk_occurrences = self
            .chunks
            .chunks
            .iter()
            .filter_map(|chunk| chunk.anchor.symbol_occurrence_id.as_ref())
            .collect::<BTreeSet<_>>();
        let occurrences = self
            .symbols
            .iter()
            .map(|symbol| &symbol.occurrence)
            .collect::<BTreeSet<_>>();
        if !occurrences.is_subset(&chunk_occurrences) {
            return Err(ChunkingFailureV1::NonCanonicalIdentity(
                "lineage symbol is not represented by a chunk".to_owned(),
            ));
        }
        if self.edges.iter().any(|edge| {
            !occurrences.contains(&edge.from_occurrence)
                || !occurrences.contains(&edge.to_occurrence)
        }) || self
            .edges
            .windows(2)
            .any(|pair| canonical_edge_key(&pair[0]) > canonical_edge_key(&pair[1]))
            || self
                .edge_abstentions
                .windows(2)
                .any(|pair| pair[0] > pair[1])
        {
            return Err(ChunkingFailureV1::NonCanonicalIdentity(
                "file graph evidence is not canonical".to_owned(),
            ));
        }
        Ok(())
    }

    /// Carry parser-backed file evidence into a new immutable generation.
    /// Every generation-local occurrence is rematerialized; edge endpoints
    /// follow that same mapping and cannot continue to point at the prior
    /// generation.
    pub fn rematerialize_for_generation(
        &self,
        generation_id: CodeGenerationId,
        file_occurrence_id: FileOccurrenceId,
    ) -> Result<Self, ChunkingFailureV1> {
        self.validate()?;
        let chunks = self
            .chunks
            .rematerialize_for_generation(generation_id, file_occurrence_id)?;
        let mut occurrences = BTreeMap::new();
        for (prior, current) in self.chunks.chunks.iter().zip(&chunks.chunks) {
            if let Some(prior_occurrence) = &prior.anchor.symbol_occurrence_id {
                let current_occurrence = current
                    .anchor
                    .symbol_occurrence_id
                    .as_ref()
                    .ok_or_else(|| {
                        ChunkingFailureV1::NonCanonicalIdentity(
                            "rematerialized symbol occurrence is missing".to_owned(),
                        )
                    })?
                    .clone();
                match occurrences.get(prior_occurrence) {
                    Some(existing) if existing != &current_occurrence => {
                        return Err(ChunkingFailureV1::NonCanonicalIdentity(
                            "symbol occurrence rematerialized inconsistently".to_owned(),
                        ));
                    }
                    _ => {
                        occurrences.insert(prior_occurrence.clone(), current_occurrence);
                    }
                }
            }
        }

        let mut symbols = self.symbols.clone();
        for symbol in &mut symbols {
            symbol.occurrence = occurrences
                .get(&symbol.occurrence)
                .cloned()
                .ok_or_else(|| {
                    ChunkingFailureV1::NonCanonicalIdentity(
                        "lineage symbol could not be rematerialized".to_owned(),
                    )
                })?;
        }
        symbols.sort_by(|left, right| left.occurrence.cmp(&right.occurrence));

        let mut edges = self.edges.clone();
        for edge in &mut edges {
            edge.from_occurrence =
                occurrences
                    .get(&edge.from_occurrence)
                    .cloned()
                    .ok_or_else(|| {
                        ChunkingFailureV1::NonCanonicalIdentity(
                            "edge source could not be rematerialized".to_owned(),
                        )
                    })?;
            edge.to_occurrence =
                occurrences
                    .get(&edge.to_occurrence)
                    .cloned()
                    .ok_or_else(|| {
                        ChunkingFailureV1::NonCanonicalIdentity(
                            "edge target could not be rematerialized".to_owned(),
                        )
                    })?;
        }
        edges.sort_by(|left, right| canonical_edge_key(left).cmp(&canonical_edge_key(right)));

        let result = Self {
            chunks,
            symbols,
            edges,
            edge_abstentions: self.edge_abstentions.clone(),
        };
        result.validate()?;
        Ok(result)
    }
}

/// Compatibility re-export for callers that previously obtained raw source
/// digests from the chunking module.
pub use super::intake::content_digest;

/// Pinned fallback window size for oversized regions with no usable
/// structural boundary (Plan 25).
pub const FALLBACK_WINDOW_BYTES: u64 = 16 * 1024;

/// Pinned fallback window overlap (Plan 25).
pub const FALLBACK_WINDOW_OVERLAP_BYTES: u64 = 1024;

/// Domain separator for chunk logical identity digests.
pub const CHUNK_IDENTITY_SEPARATOR: &str = "tracedecay.code-search-chunk-id.v1";

/// Domain separator for file logical identity digests.
pub const FILE_IDENTITY_SEPARATOR: &str = "tracedecay.code-file-identity.v1";

/// Domain separator for symbol logical identity digests.
pub const SYMBOL_IDENTITY_SEPARATOR: &str = "tracedecay.code-symbol-identity.v1";

/// Domain separator for symbol occurrence identity digests.
pub const SYMBOL_OCCURRENCE_SEPARATOR: &str = "tracedecay.code-symbol-occurrence.v1";

/// Domain separator for carried symbol-occurrence rematerialization.
pub const SYMBOL_OCCURRENCE_REMATERIALIZATION_SEPARATOR: &str =
    "tracedecay.code-symbol-occurrence-rematerialization.v1";

/// Domain separator for parser-backed exact extraction authority.
pub const EXACT_EXTRACTION_AUTHORITY_SEPARATOR: &str = "tracedecay.exact-extraction-authority.v1";

/// The deterministic five-grain chunker.
///
/// The compatibility `CodeChunker` port accepts only extraction evidence and
/// therefore re-parses. Production indexing uses
/// [`Self::index_file_with_authority_from_extraction`] to consume the exact
/// sanitized parser rows that produced that evidence, avoiding a second parse.
/// Both paths validate batch, descriptor, and file identity before structural
/// work and share the same canonical materialization.
///
/// Construct one chunker per generation: generation identity, repository
/// identity, sanitizer revision, policy revision, and chunker revision are
/// pinned at construction.
pub struct DeterministicCodeChunker {
    generation_id: CodeGenerationId,
    repository: RepositoryId,
    sanitizer_revision: SanitizerRevision,
    policy_revision: PolicyRevisionId,
    sensitivity_level: SensitivityLevelV1,
    chunker_revision: ChunkerRevision,
    extractors: Arc<tracedecay_code_extraction::LanguageRegistry>,
}

impl DeterministicCodeChunker {
    /// Create a chunker bound to one generation. Chunks default to
    /// `SensitivityLevelV1::Public` under `policy_revision`; application
    /// policy output refines this via `with_sensitivity_level`.
    pub fn new(
        generation_id: CodeGenerationId,
        repository: RepositoryId,
        sanitizer_revision: SanitizerRevision,
        policy_revision: PolicyRevisionId,
        chunker_revision: ChunkerRevision,
        extractors: tracedecay_code_extraction::LanguageRegistry,
    ) -> Self {
        Self::from_shared_registry(
            generation_id,
            repository,
            sanitizer_revision,
            policy_revision,
            chunker_revision,
            Arc::new(extractors),
        )
    }

    /// Create a generation-bound chunker over a shared parser registry.
    pub fn from_shared_registry(
        generation_id: CodeGenerationId,
        repository: RepositoryId,
        sanitizer_revision: SanitizerRevision,
        policy_revision: PolicyRevisionId,
        chunker_revision: ChunkerRevision,
        extractors: Arc<tracedecay_code_extraction::LanguageRegistry>,
    ) -> Self {
        Self {
            generation_id,
            repository,
            sanitizer_revision,
            policy_revision,
            sensitivity_level: SensitivityLevelV1::Public,
            chunker_revision,
            extractors,
        }
    }

    /// Pin the sensitivity level recorded on every chunk of this generation.
    #[must_use]
    pub fn with_sensitivity_level(mut self, level: SensitivityLevelV1) -> Self {
        self.sensitivity_level = level;
        self
    }

    /// The generation this chunker is bound to.
    pub fn generation_id(&self) -> &CodeGenerationId {
        &self.generation_id
    }

    /// Index one receipt-bound file and retain its symbols, canonical graph
    /// edges, and typed edge abstentions alongside its chunks.
    pub fn index_file(
        &self,
        file: &ReceiptBoundCodeFileV1,
        batch: &ExtractionBatchV1,
        descriptor: &LanguageDescriptorV1,
        cancellation: &dyn ExtractionCancellation,
    ) -> Result<CodeFileIndexArtifactsV1, ChunkingFailureV1> {
        self.build_file_artifacts(file, batch, descriptor, cancellation)
    }

    /// Index one receipt-bound file and return the opaque capability required
    /// to re-admit its exact extraction evidence into lexical projection.
    pub fn index_file_with_authority(
        &self,
        file: &ReceiptBoundCodeFileV1,
        batch: &ExtractionBatchV1,
        descriptor: &LanguageDescriptorV1,
        cancellation: &dyn ExtractionCancellation,
    ) -> Result<(CodeFileIndexArtifactsV1, ExactExtractionAuthorityV1), ChunkingFailureV1> {
        let result = self.index_file(file, batch, descriptor, cancellation)?;
        let authority = ExactExtractionAuthorityV1::mint(&result.chunks.chunks)?;
        Ok((result, authority))
    }

    /// Index one file from the parser rows that produced its extraction batch.
    /// The opaque output type prevents callers from pairing a batch with rows
    /// from a different parse.
    pub fn index_file_with_authority_from_extraction(
        &self,
        file: &ReceiptBoundCodeFileV1,
        extraction: &ExtractedCodeFileV1,
        descriptor: &LanguageDescriptorV1,
        sensitivity_level: SensitivityLevelV1,
        cancellation: &dyn ExtractionCancellation,
    ) -> Result<(CodeFileIndexArtifactsV1, ExactExtractionAuthorityV1), ChunkingFailureV1> {
        let result = self.build_file_artifacts_with_parse(
            file,
            extraction.batch(),
            descriptor,
            Some(extraction.parse_artifacts()),
            sensitivity_level,
            cancellation,
        )?;
        let authority = ExactExtractionAuthorityV1::mint(&result.chunks.chunks)?;
        Ok((result, authority))
    }

    /// Chunk one receipt-bound file and return the opaque capability required
    /// to re-admit its exact extraction evidence into lexical projection.
    pub fn chunk_file_with_authority(
        &self,
        file: &ReceiptBoundCodeFileV1,
        batch: &ExtractionBatchV1,
        descriptor: &LanguageDescriptorV1,
        cancellation: &dyn ExtractionCancellation,
    ) -> Result<(CodeFileChunksV1, ExactExtractionAuthorityV1), ChunkingFailureV1> {
        let (result, authority) =
            self.index_file_with_authority(file, batch, descriptor, cancellation)?;
        Ok((result.chunks, authority))
    }

    fn file_identity(&self, logical_path: &str) -> Result<FileIdentityDigest, ChunkingFailureV1> {
        canonical_digest(
            FILE_IDENTITY_SEPARATOR,
            &(self.repository.as_str(), logical_path),
        )
        .map(|digest| {
            FileIdentityDigest::new(digest).expect("canonical digest is a valid identity digest")
        })
    }

    fn chunk_id(
        &self,
        file_identity: &FileIdentityDigest,
        symbol_identity: Option<&SymbolIdentityDigest>,
        grain: CodeSearchChunkGrainV1,
        split_path: Vec<u32>,
    ) -> Result<CodeSearchChunkId, ChunkingFailureV1> {
        let identity = ChunkLogicalIdentityV1 {
            repository: self.repository.clone(),
            file_identity: file_identity.clone(),
            symbol_identity: symbol_identity.cloned(),
            grain,
            split_path,
            chunker_revision: self.chunker_revision.clone(),
        };
        let digest = canonical_digest(CHUNK_IDENTITY_SEPARATOR, &identity)?;
        CodeSearchChunkId::new(format!("chunk.v1.{digest}"))
            .map_err(|error| ChunkingFailureV1::NonCanonicalIdentity(error.to_string()))
    }
}

/// Canonical, domain-separated SHA-256 of a serializable payload, returned as
/// the bare `sha256:<hex>` string for identity/digest newtype construction.
fn canonical_digest<T: serde::Serialize>(
    separator: &'static str,
    payload: &T,
) -> Result<String, ChunkingFailureV1> {
    canonical_sha256(&(separator, payload))
        .map(|digest| digest.as_str().to_owned())
        .map_err(|error| ChunkingFailureV1::NonCanonicalIdentity(error.to_string()))
}

fn symbol_occurrence_id(
    generation_id: &CodeGenerationId,
    file_occurrence_id: &FileOccurrenceId,
    identity: &SymbolIdentityDigest,
) -> Result<SymbolOccurrenceId, ChunkingFailureV1> {
    canonical_digest(
        SYMBOL_OCCURRENCE_SEPARATOR,
        &(
            generation_id.as_str(),
            file_occurrence_id.as_str(),
            identity.as_str(),
        ),
    )
    .and_then(|digest| {
        SymbolOccurrenceId::new(format!("symbol.v1.{digest}"))
            .map_err(|error| ChunkingFailureV1::NonCanonicalIdentity(error.to_string()))
    })
}

fn rematerialized_symbol_occurrence_id(
    generation_id: &CodeGenerationId,
    file_occurrence_id: &FileOccurrenceId,
    prior_occurrence: &SymbolOccurrenceId,
) -> Result<SymbolOccurrenceId, ChunkingFailureV1> {
    canonical_digest(
        SYMBOL_OCCURRENCE_REMATERIALIZATION_SEPARATOR,
        &(
            generation_id.as_str(),
            file_occurrence_id.as_str(),
            prior_occurrence.as_str(),
        ),
    )
    .and_then(|digest| {
        SymbolOccurrenceId::new(format!("symbol.v1.{digest}"))
            .map_err(|error| ChunkingFailureV1::NonCanonicalIdentity(error.to_string()))
    })
}

/// Kinds whose nodes never become symbol chunks: imports, preprocessor
/// lines, annotations, and other non-symbol structure. Every other kind is a
/// symbol grain candidate.
fn is_symbol_kind(kind: &NodeKind) -> bool {
    !matches!(
        kind,
        NodeKind::File
            | NodeKind::Use
            | NodeKind::Include
            | NodeKind::PreprocessorDef
            | NodeKind::GenericParam
            | NodeKind::Annotation
            | NodeKind::AnnotationUsage
            | NodeKind::StructTag
            | NodeKind::Export
            | NodeKind::Decorator
    )
}

/// One extracted symbol reduced to chunk-relevant, identity-stable facts.
struct SymbolRow {
    node_id: String,
    span: SourceSpan,
    name: String,
    qualified_name: String,
    kind: String,
    parent: Option<usize>,
    identity: SymbolIdentityDigest,
    occurrence: SymbolOccurrenceId,
}

/// Byte offset of one line start for every line in the source.
fn line_offsets(bytes: &[u8]) -> Vec<u64> {
    let mut offsets = vec![0u64];
    for (index, byte) in bytes.iter().enumerate() {
        if *byte == b'\n' {
            offsets.push(index as u64 + 1);
        }
    }
    offsets
}

/// Convert a tree-sitter row/column to a clamped byte offset.
fn byte_offset(offsets: &[u64], len: u64, row: u32, column: u32) -> u64 {
    let base = offsets.get(row as usize).copied().unwrap_or(len);
    base.saturating_add(u64::from(column)).min(len)
}

/// Snap an offset down to the nearest UTF-8 char boundary.
fn snap_down(source: &str, mut offset: usize) -> usize {
    let len = source.len();
    if offset > len {
        offset = len;
    }
    while offset > 0 && !source.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
}

/// Pinned fallback windows over `[start, end)`. Each window is
/// `(absolute_start, len)`; consecutive windows overlap by
/// `FALLBACK_WINDOW_OVERLAP_BYTES` and the union covers the whole region.
fn fallback_windows(source: &str, start: u64, end: u64) -> Vec<(u64, u64)> {
    debug_assert!(start <= end);
    if end - start <= MAX_CHUNK_TEXT_BYTES as u64 {
        return vec![(start, end - start)];
    }
    let step = FALLBACK_WINDOW_BYTES - FALLBACK_WINDOW_OVERLAP_BYTES;
    let mut windows = Vec::new();
    let mut cursor = start;
    loop {
        let raw_end = (cursor + FALLBACK_WINDOW_BYTES).min(end);
        // Snap to a char boundary, but never produce an empty window: the
        // remainder of the region is always taken by the final window.
        let mut window_end = snap_down(source, raw_end as usize) as u64;
        if window_end <= cursor {
            window_end = raw_end;
        }
        windows.push((cursor, window_end - cursor));
        if window_end >= end {
            break;
        }
        let next = snap_down(source, (cursor + step) as usize) as u64;
        cursor = if next > cursor { next } else { window_end };
    }
    windows
}

/// Encode a fallback window's split path as the pinned window start/size
/// relative to the enclosing region base (Plan 25).
fn window_split_path(base: u64, window: (u64, u64)) -> Vec<u32> {
    vec![
        u32::try_from(window.0 - base).unwrap_or(u32::MAX),
        u32::try_from(window.1).unwrap_or(u32::MAX),
    ]
}

/// Structural split of one oversized body at its member boundaries:
/// deterministic segments `[start, c0), [c0, c1), ..., [cn, end)`.
fn structural_segments(body: SourceSpan, mut member_starts: Vec<u64>) -> Vec<(u64, u64)> {
    member_starts.retain(|start| *start > body.start_byte && *start < body.end_byte);
    member_starts.sort_unstable();
    member_starts.dedup();
    let mut segments = Vec::new();
    let mut cursor = body.start_byte;
    for point in member_starts {
        if point > cursor {
            segments.push((cursor, point - cursor));
            cursor = point;
        }
    }
    if body.end_byte > cursor {
        segments.push((cursor, body.end_byte - cursor));
    }
    segments
}

/// Exact technical terms and language-profiled subtokens for one chunk's
/// sanitized text (Plan 25: whole exact terms and subtokens are distinct
/// fields; this is extraction evidence only).
fn classify_chunk_text(text: &str, base_offset: u64) -> (Vec<ExactTechnicalTermV1>, Vec<String>) {
    let is_token_char =
        |c: char| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | ':' | '.' | '/');
    let mut terms = Vec::new();
    let mut seen_terms = BTreeSet::new();
    let mut subtokens = Vec::new();
    let mut seen_subtokens = BTreeSet::new();

    let bytes = text.as_bytes();
    let mut cursor = 0usize;
    while cursor < bytes.len() {
        let ch = text[cursor..].chars().next().expect("cursor is a boundary");
        if !is_token_char(ch) {
            cursor += ch.len_utf8();
            continue;
        }
        let start = cursor;
        while cursor < bytes.len() {
            let c = text[cursor..].chars().next().expect("cursor is a boundary");
            if !is_token_char(c) {
                break;
            }
            cursor += c.len_utf8();
        }
        let token = &text[start..cursor];
        let span = SourceSpan {
            start_byte: base_offset + start as u64,
            end_byte: base_offset + cursor as u64,
        };
        for subtoken in split_subtokens(token) {
            if seen_subtokens.insert(subtoken.clone()) {
                subtokens.push(subtoken);
            }
        }
        if let Some(kind) = classify_token(token) {
            mint_exact_term(&mut terms, &mut seen_terms, kind, token.as_bytes(), span);
        }
    }
    let mut line_start = 0usize;
    for line in text.split_inclusive('\n') {
        let line_without_newline = line.strip_suffix('\n').unwrap_or(line);
        let lowercase = line_without_newline.to_ascii_lowercase();
        let marker = [
            (
                "compiler error:",
                ExactTechnicalTermKindV1::CompilerErrorText,
            ),
            ("runtime error:", ExactTechnicalTermKindV1::RuntimeErrorText),
            ("panic:", ExactTechnicalTermKindV1::RuntimeErrorText),
        ]
        .into_iter()
        .find_map(|(marker, kind)| {
            lowercase
                .find(marker)
                .map(|start| (start, marker.len(), kind))
        });
        if let Some((marker_start, marker_len, kind)) = marker {
            let mut start = marker_start + marker_len;
            while line_without_newline[start..]
                .chars()
                .next()
                .is_some_and(char::is_whitespace)
            {
                start += line_without_newline[start..]
                    .chars()
                    .next()
                    .expect("whitespace was present")
                    .len_utf8();
            }
            let end = line_without_newline.trim_end().len();
            if start < end {
                let original = &line_without_newline.as_bytes()[start..end];
                mint_exact_term(
                    &mut terms,
                    &mut seen_terms,
                    kind,
                    original,
                    SourceSpan {
                        start_byte: base_offset + (line_start + start) as u64,
                        end_byte: base_offset + (line_start + end) as u64,
                    },
                );
            }
        }
        line_start += line.len();
    }
    terms.sort_by(|left, right| {
        (
            left.span().start_byte,
            left.span().end_byte,
            left.kind(),
            left.canonical_bytes(),
            left.original_bytes(),
        )
            .cmp(&(
                right.span().start_byte,
                right.span().end_byte,
                right.kind(),
                right.canonical_bytes(),
                right.original_bytes(),
            ))
    });
    (terms, subtokens)
}

/// Mint a whole technical term only after a type-specific recognizer has
/// established its syntax. Subtokens intentionally remain broader evidence.
fn mint_exact_term(
    terms: &mut Vec<ExactTechnicalTermV1>,
    seen_terms: &mut BTreeSet<(ExactTechnicalTermKindV1, Vec<u8>)>,
    kind: ExactTechnicalTermKindV1,
    original_bytes: &[u8],
    span: SourceSpan,
) {
    let term = if matches!(
        kind,
        ExactTechnicalTermKindV1::CompilerErrorText | ExactTechnicalTermKindV1::RuntimeErrorText
    ) {
        ExactTechnicalTermV1::untrusted_contextual_text_candidate(
            kind,
            original_bytes.to_vec(),
            span,
        )
    } else {
        ExactTechnicalTermV1::technical(kind, original_bytes.to_vec(), span)
    };
    if let Ok(term) = term
        && seen_terms.insert((kind, term.canonical_bytes().to_vec()))
    {
        terms.push(term);
    }
}

/// Classify one maximal token as a whole exact technical term kind, or
/// `None` when the token is only subtoken evidence.
fn classify_token(token: &str) -> Option<ExactTechnicalTermKindV1> {
    let is_ident = |segment: &str| {
        !segment.is_empty()
            && segment
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_')
            && segment
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
    };
    if token.strip_prefix("--").is_some_and(|flag| {
        !flag.is_empty()
            && !flag.ends_with('-')
            && flag
                .chars()
                .next()
                .is_some_and(|character| character.is_ascii_alphabetic())
            && flag.chars().all(|character| {
                character.is_ascii_lowercase() || character.is_ascii_digit() || character == '-'
            })
    }) {
        return Some(ExactTechnicalTermKindV1::CliFlag);
    }
    if ["E", "TS", "CS"].into_iter().any(|prefix| {
        token.strip_prefix(prefix).is_some_and(|digits| {
            digits.len() == 4 && digits.chars().all(|character| character.is_ascii_digit())
        })
    }) {
        return Some(ExactTechnicalTermKindV1::CompilerErrorCode);
    }
    if token.strip_prefix("ERR_").is_some_and(|suffix| {
        !suffix.is_empty()
            && suffix.chars().all(|character| {
                character.is_ascii_uppercase() || character.is_ascii_digit() || character == '_'
            })
    }) {
        return Some(ExactTechnicalTermKindV1::RuntimeErrorCode);
    }
    if token.strip_prefix("commit:").is_some_and(|identifier| {
        (7..=40).contains(&identifier.len())
            && identifier
                .chars()
                .all(|character| character.is_ascii_hexdigit())
    }) {
        return Some(ExactTechnicalTermKindV1::CommitIdentifier);
    }
    if matches!(
        token.to_ascii_lowercase().as_str(),
        "cargo" | "rustc" | "tracedecay" | "pytest" | "kubectl" | "fastembed" | "ast-grep"
    ) {
        return Some(ExactTechnicalTermKindV1::ToolName);
    }
    if token.contains("::") && token.split("::").all(is_ident) {
        return Some(ExactTechnicalTermKindV1::QualifiedName);
    }
    if token.contains('/')
        && token.split('/').all(|segment| {
            !segment.is_empty()
                && segment
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
        })
        && token
            .rsplit('/')
            .next()
            .is_some_and(|filename| filename.contains('.'))
    {
        return Some(ExactTechnicalTermKindV1::Path);
    }
    if token.contains('.')
        && !token.contains('/')
        && token.split('.').count() >= 3
        && token.split('.').all(|segment| {
            !segment.is_empty()
                && segment
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        })
    {
        return Some(ExactTechnicalTermKindV1::ConfigurationKey);
    }
    None
}

fn symbol_name_span(source: &str, symbol: &SymbolRow) -> Option<SourceSpan> {
    if symbol.name.is_empty() {
        return None;
    }
    let start = usize::try_from(symbol.span.start_byte).ok()?;
    let end = usize::try_from(symbol.span.end_byte).ok()?;
    let body = source.get(start..end)?;
    let is_identifier = |byte: u8| byte.is_ascii_alphanumeric() || byte == b'_';
    body.match_indices(&symbol.name)
        .find_map(|(relative, name)| {
            let before = relative
                .checked_sub(1)
                .and_then(|index| body.as_bytes().get(index))
                .copied();
            let after = body.as_bytes().get(relative + name.len()).copied();
            if before.is_some_and(is_identifier) || after.is_some_and(is_identifier) {
                return None;
            }
            Some(SourceSpan {
                start_byte: symbol.span.start_byte + relative as u64,
                end_byte: symbol.span.start_byte + (relative + name.len()) as u64,
            })
        })
}

/// Split one token into lowercase language-profiled subtokens: path,
/// qualifier, and key separators first, then snake/camel boundaries.
fn split_subtokens(token: &str) -> Vec<String> {
    let mut subtokens = Vec::new();
    for segment in token.split([':', '.', '/', '-']) {
        let mut current = String::new();
        let mut prev: Option<char> = None;
        for c in segment.chars() {
            let boundary = match (prev, c) {
                (Some('_'), _) => false,
                (_, '_') => true,
                (Some(p), c) if p.is_lowercase() && c.is_uppercase() => true,
                (Some(p), c) if p.is_ascii_digit() != c.is_ascii_digit() => true,
                _ => false,
            };
            if boundary && !current.is_empty() {
                subtokens.push(current.to_lowercase());
                current.clear();
            }
            if c != '_' {
                current.push(c);
            }
            prev = Some(c);
        }
        if !current.is_empty() {
            subtokens.push(current.to_lowercase());
        }
    }
    subtokens
}

/// One not-yet-identified chunk: everything except the id, digest, ordinal,
/// and parent id, which are assigned during canonical materialization.
struct PendingChunk {
    grain: CodeSearchChunkGrainV1,
    symbol: Option<usize>,
    split_path: Vec<u32>,
    span: SourceSpan,
    /// `(symbol index, split path)` identifying the parent chunk.
    parent: Option<(usize, Vec<u32>)>,
}

impl CodeChunker for DeterministicCodeChunker {
    fn chunk_file(
        &self,
        file: &ReceiptBoundCodeFileV1,
        batch: &ExtractionBatchV1,
        descriptor: &LanguageDescriptorV1,
        cancellation: &dyn ExtractionCancellation,
    ) -> Result<CodeFileChunksV1, ChunkingFailureV1> {
        self.index_file(file, batch, descriptor, cancellation)
            .map(|artifacts| artifacts.chunks)
    }
}

impl DeterministicCodeChunker {
    /// Build all parser-backed file artifacts. The legacy chunk-only port
    /// delegates here so chunk, lineage, and graph evidence are always
    /// derived from the same bounded parser result.
    fn build_file_artifacts(
        &self,
        file: &ReceiptBoundCodeFileV1,
        batch: &ExtractionBatchV1,
        descriptor: &LanguageDescriptorV1,
        cancellation: &dyn ExtractionCancellation,
    ) -> Result<CodeFileIndexArtifactsV1, ChunkingFailureV1> {
        self.build_file_artifacts_with_parse(
            file,
            batch,
            descriptor,
            None,
            self.sensitivity_level,
            cancellation,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn build_file_artifacts_with_parse(
        &self,
        file: &ReceiptBoundCodeFileV1,
        batch: &ExtractionBatchV1,
        descriptor: &LanguageDescriptorV1,
        parse_artifacts: Option<&ExtractionResult>,
        sensitivity_level: SensitivityLevelV1,
        cancellation: &dyn ExtractionCancellation,
    ) -> Result<CodeFileIndexArtifactsV1, ChunkingFailureV1> {
        if cancellation.is_cancelled() {
            return Err(ChunkingFailureV1::Cancelled);
        }
        let file = file.validated_file();
        if batch.language != descriptor.language
            || batch.descriptor_revision != descriptor.descriptor_revision
            || batch.grammar_revision != descriptor.grammar_revision
            || batch.extractor_revision != descriptor.extractor_revision
        {
            return Err(ChunkingFailureV1::DescriptorMismatch);
        }
        if batch.file_occurrence_id != file.file.file_occurrence_id
            || batch.content_digest != file.file.content_digest
            || batch.generation_id != self.generation_id
            || file.generation_id != self.generation_id
        {
            return Err(ChunkingFailureV1::GenerationMismatch);
        }

        // A failed, timed-out, or cancelled extraction attests no structure:
        // the document is explicitly unsupported and every byte is covered by
        // the batch's error/unsupported evidence, not by invented chunks.
        let parse_reason = match &batch.parse_outcome {
            ParseOutcomeV1::Complete => None,
            ParseOutcomeV1::Partial { reason } => {
                return self.build_partial_artifacts(
                    file,
                    batch,
                    descriptor,
                    parse_artifacts,
                    sensitivity_level,
                    cancellation,
                    reason.clone(),
                );
            }
            other => Some(format!("{other:?}")),
        };
        if let Some(reason) = parse_reason {
            let document = CodeSearchDocumentV1 {
                generation_id: self.generation_id.clone(),
                file_occurrence_id: file.file.file_occurrence_id.clone(),
                content_digest: file.file.content_digest.clone(),
                eligibility: CodeSearchEligibilityV1::Unsupported { reason },
                chunk_ids: Vec::new(),
            };
            let artifacts = CodeFileIndexArtifactsV1 {
                chunks: CodeFileChunksV1 {
                    document,
                    chunks: Vec::new(),
                },
                symbols: Vec::new(),
                edges: Vec::new(),
                edge_abstentions: Vec::new(),
            };
            artifacts.validate()?;
            return Ok(artifacts);
        }
        self.build_partial_artifacts(
            file,
            batch,
            descriptor,
            parse_artifacts,
            sensitivity_level,
            cancellation,
            String::new(),
        )
    }

    /// Shared complete/partial chunk production. `partial_reason` is empty
    /// for complete parses.
    #[allow(clippy::too_many_arguments)]
    fn build_partial_artifacts(
        &self,
        file: &ValidatedCodeFileV1,
        batch: &ExtractionBatchV1,
        descriptor: &LanguageDescriptorV1,
        parse_artifacts: Option<&ExtractionResult>,
        sensitivity_level: SensitivityLevelV1,
        cancellation: &dyn ExtractionCancellation,
        partial_reason: String,
    ) -> Result<CodeFileIndexArtifactsV1, ChunkingFailureV1> {
        let full_source = std::str::from_utf8(&file.sanitized_bytes).map_err(|error| {
            ChunkingFailureV1::NonCanonicalIdentity(format!(
                "sanitized bytes are not valid UTF-8: {error}"
            ))
        })?;
        let full_len = full_source.len() as u64;
        let mut parsed_prefix_end = 0;
        for range in &batch.parsed_ranges {
            if range.start_byte > parsed_prefix_end {
                break;
            }
            parsed_prefix_end = parsed_prefix_end.max(range.end_byte.min(full_len));
        }
        for range in batch.error_ranges.iter().chain(&batch.unsupported_ranges) {
            if range.start_byte < parsed_prefix_end {
                parsed_prefix_end = range.start_byte;
            }
        }
        let parsed_prefix_end = usize::try_from(parsed_prefix_end).map_err(|error| {
            ChunkingFailureV1::NonCanonicalIdentity(format!(
                "parsed prefix does not fit this host: {error}"
            ))
        })?;
        if !full_source.is_char_boundary(parsed_prefix_end) {
            return Err(ChunkingFailureV1::NonCanonicalIdentity(
                "parsed prefix is not a UTF-8 boundary".to_owned(),
            ));
        }
        let source = &full_source[..parsed_prefix_end];
        let mut reparsed;
        let result = if let Some(parse_artifacts) = parse_artifacts {
            parse_artifacts
        } else {
            let extractor = self
                .extractors
                .extractor_for_file(&file.file.logical_path)
                .or_else(|| {
                    descriptor.extensions.iter().find_map(|extension| {
                        self.extractors
                            .extractor_for_file(&format!("probe.{extension}"))
                    })
                })
                .ok_or(ChunkingFailureV1::DescriptorMismatch)?;
            if cancellation.is_cancelled() {
                return Err(ChunkingFailureV1::Cancelled);
            }
            reparsed = extractor.extract(&file.file.logical_path, source);
            reparsed.sanitize();
            &reparsed
        };
        if cancellation.is_cancelled() {
            return Err(ChunkingFailureV1::Cancelled);
        }

        let len = source.len() as u64;
        let offsets = line_offsets(source.as_bytes());
        let file_identity = self.file_identity(&file.file.logical_path)?;
        let symbol_rows = self.symbol_rows(
            &file.file.file_occurrence_id,
            &file_identity,
            &result.nodes,
            &offsets,
            len,
        )?;
        let chunks = self.build_chunks(
            source,
            len,
            batch,
            descriptor,
            &file_identity,
            &symbol_rows,
            sensitivity_level,
            cancellation,
        )?;
        let symbols = self.lineage_symbols(source, &file_identity, &symbol_rows)?;
        let mut relation_edges = result.edges.clone();
        relation_edges.extend(resolve_same_file_references(
            &result.unresolved_refs,
            &symbol_rows,
        ));
        let (edges, edge_abstentions) = canonical_relation_edges(&relation_edges, &symbol_rows);

        let eligibility = if partial_reason.is_empty() {
            CodeSearchEligibilityV1::Eligible
        } else {
            CodeSearchEligibilityV1::Partial {
                reason: partial_reason,
            }
        };
        let document = CodeSearchDocumentV1 {
            generation_id: self.generation_id.clone(),
            file_occurrence_id: file.file.file_occurrence_id.clone(),
            content_digest: file.file.content_digest.clone(),
            eligibility,
            chunk_ids: chunks.iter().map(|chunk| chunk.id.clone()).collect(),
        };
        let artifacts = CodeFileIndexArtifactsV1 {
            chunks: CodeFileChunksV1 { document, chunks },
            symbols,
            edges,
            edge_abstentions,
        };
        artifacts.validate()?;
        Ok(artifacts)
    }

    /// Reduce extractor nodes to canonically ordered, identity-stable symbol
    /// rows. Sorting by span and qualified name before assigning same-name
    /// occurrence indices keeps identity independent of extractor enumeration
    /// order; identity payloads never contain line numbers.
    fn symbol_rows(
        &self,
        file_occurrence_id: &FileOccurrenceId,
        file_identity: &FileIdentityDigest,
        nodes: &[Node],
        offsets: &[u64],
        len: u64,
    ) -> Result<Vec<SymbolRow>, ChunkingFailureV1> {
        struct Raw {
            node_id: String,
            kind: String,
            name: String,
            qualified_name: String,
            span: SourceSpan,
        }

        let mut raw: Vec<Raw> = nodes
            .iter()
            .filter(|node| is_symbol_kind(&node.kind))
            .map(|node| {
                let start = if node.attrs_start_line < node.start_line {
                    byte_offset(offsets, len, node.attrs_start_line, 0)
                } else {
                    byte_offset(offsets, len, node.start_line, node.start_column)
                };
                let end = byte_offset(offsets, len, node.end_line, node.end_column);
                Raw {
                    node_id: node.id.clone(),
                    kind: node.kind.as_str().to_owned(),
                    name: node.name.clone(),
                    qualified_name: node.qualified_name.clone(),
                    span: SourceSpan {
                        start_byte: start.min(end),
                        end_byte: start.max(end),
                    },
                }
            })
            .filter(|node| !node.span.is_empty())
            .collect();
        // Outer spans first: ascending start, descending end.
        raw.sort_by(|left, right| {
            left.span
                .start_byte
                .cmp(&right.span.start_byte)
                .then(right.span.end_byte.cmp(&left.span.end_byte))
                .then(left.qualified_name.cmp(&right.qualified_name))
                .then(left.kind.cmp(&right.kind))
        });

        let mut rows = Vec::with_capacity(raw.len());
        for (index, node) in raw.iter().enumerate() {
            // Parent = the smallest strictly enclosing span among earlier
            // (outer-or-equal) rows; equal spans resolve to the earlier row.
            let parent = raw[..index]
                .iter()
                .enumerate()
                .filter(|(_, candidate)| {
                    candidate.span.start_byte <= node.span.start_byte
                        && candidate.span.end_byte >= node.span.end_byte
                })
                .min_by_key(|(_, candidate)| candidate.span.len())
                .map(|(parent_index, _)| parent_index);
            let occurrence_index = raw[..index]
                .iter()
                .filter(|candidate| {
                    candidate.qualified_name == node.qualified_name && candidate.kind == node.kind
                })
                .count() as u32;
            let identity = canonical_digest(
                SYMBOL_IDENTITY_SEPARATOR,
                &(
                    file_identity.as_str(),
                    node.qualified_name.as_str(),
                    node.kind.as_str(),
                    occurrence_index,
                ),
            )
            .map(|digest| {
                SymbolIdentityDigest::new(digest)
                    .expect("canonical digest is a valid symbol identity digest")
            })?;
            let occurrence =
                symbol_occurrence_id(&self.generation_id, file_occurrence_id, &identity)?;
            rows.push(SymbolRow {
                node_id: node.node_id.clone(),
                span: node.span,
                name: node.name.clone(),
                qualified_name: node.qualified_name.clone(),
                kind: node.kind.clone(),
                parent,
                identity,
                occurrence,
            });
        }
        Ok(rows)
    }

    fn lineage_symbols(
        &self,
        source: &str,
        file_identity: &FileIdentityDigest,
        rows: &[SymbolRow],
    ) -> Result<Vec<LineageSymbolRecordV1>, ChunkingFailureV1> {
        let mut symbols = Vec::with_capacity(rows.len());
        for row in rows {
            let start = usize::try_from(row.span.start_byte).map_err(|error| {
                ChunkingFailureV1::NonCanonicalIdentity(format!(
                    "symbol start offset does not fit this host: {error}"
                ))
            })?;
            let end = usize::try_from(row.span.end_byte).map_err(|error| {
                ChunkingFailureV1::NonCanonicalIdentity(format!(
                    "symbol end offset does not fit this host: {error}"
                ))
            })?;
            let text = source.get(start..end).ok_or_else(|| {
                ChunkingFailureV1::NonCanonicalIdentity(
                    "symbol span is not a valid UTF-8 source range".to_owned(),
                )
            })?;
            symbols.push(LineageSymbolRecordV1 {
                occurrence: row.occurrence.clone(),
                identity: row.identity.clone(),
                qualified_name: row.qualified_name.clone(),
                kind: row.kind.clone(),
                file_identity: file_identity.clone(),
                content_digest: content_digest(text.as_bytes()),
            });
        }
        symbols.sort_by(|left, right| left.occurrence.cmp(&right.occurrence));
        Ok(symbols)
    }

    /// Build, identify, and canonically order every chunk for the file.
    #[allow(clippy::too_many_arguments)]
    fn build_chunks(
        &self,
        source: &str,
        len: u64,
        batch: &ExtractionBatchV1,
        descriptor: &LanguageDescriptorV1,
        file_identity: &FileIdentityDigest,
        symbols: &[SymbolRow],
        sensitivity_level: SensitivityLevelV1,
        cancellation: &dyn ExtractionCancellation,
    ) -> Result<Vec<CodeSearchChunkV1>, ChunkingFailureV1> {
        // Per-symbol emission plan: primary grain (body or member), split
        // pieces, and signature span.
        struct Emission {
            grain: CodeSearchChunkGrainV1,
            pieces: Vec<(Vec<u32>, SourceSpan)>,
            signature: Option<SourceSpan>,
        }

        let mut emissions = Vec::with_capacity(symbols.len());
        for (index, symbol) in symbols.iter().enumerate() {
            if index % 64 == 0 && cancellation.is_cancelled() {
                return Err(ChunkingFailureV1::Cancelled);
            }
            let is_member = descriptor.stable_member_spans && symbol.parent.is_some();
            let grain = if is_member {
                CodeSearchChunkGrainV1::SymbolMember
            } else {
                CodeSearchChunkGrainV1::SymbolBody
            };
            let member_starts: Vec<u64> = symbols
                .iter()
                .filter(|candidate| candidate.parent == Some(index))
                .map(|candidate| candidate.span.start_byte)
                .collect();
            let mut pieces = Vec::new();
            if symbol.span.len() > MAX_CHUNK_TEXT_BYTES as u64 {
                // Oversized bodies split on deterministic structural
                // boundaries (member starts) when the descriptor identifies
                // stable member spans; otherwise the pinned fallback windows.
                let structural = descriptor.stable_member_spans && !member_starts.is_empty();
                let segments = if structural {
                    structural_segments(symbol.span, member_starts)
                } else {
                    vec![(symbol.span.start_byte, symbol.span.len())]
                };
                for (segment_index, (segment_start, segment_len)) in segments.iter().enumerate() {
                    for window in
                        fallback_windows(source, *segment_start, segment_start + segment_len)
                    {
                        let split_path = if structural {
                            if window.1 < *segment_len {
                                let mut path = vec![segment_index as u32];
                                path.extend(window_split_path(*segment_start, window));
                                path
                            } else {
                                vec![segment_index as u32]
                            }
                        } else {
                            window_split_path(*segment_start, window)
                        };
                        pieces.push((
                            split_path,
                            SourceSpan {
                                start_byte: window.0,
                                end_byte: window.0 + window.1,
                            },
                        ));
                    }
                }
            } else {
                pieces.push((Vec::new(), symbol.span));
            }
            let line_end = offsets_line_end(source, symbol.span.start_byte);
            let signature_end = line_end.min(symbol.span.end_byte);
            let signature = (signature_end > symbol.span.start_byte).then_some(SourceSpan {
                start_byte: symbol.span.start_byte,
                end_byte: signature_end,
            });
            emissions.push(Emission {
                grain,
                pieces,
                signature,
            });
        }

        // Pending chunks: signatures, primary grain pieces, preamble, windows.
        let mut pending: Vec<PendingChunk> = Vec::new();
        for (index, symbol) in symbols.iter().enumerate() {
            let emission = &emissions[index];
            let parent = symbol
                .parent
                .map(|parent_index| (parent_index, emissions[parent_index].pieces[0].0.clone()));
            for (split_path, span) in &emission.pieces {
                pending.push(PendingChunk {
                    grain: emission.grain,
                    symbol: Some(index),
                    split_path: split_path.clone(),
                    span: *span,
                    parent: parent.clone(),
                });
            }
            if let Some(signature) = emission.signature {
                pending.push(PendingChunk {
                    grain: CodeSearchChunkGrainV1::SymbolSignature,
                    symbol: Some(index),
                    split_path: Vec::new(),
                    span: signature,
                    parent: Some((index, emission.pieces[0].0.clone())),
                });
            }
        }

        // Preamble covers everything before the first symbol (imports,
        // module documentation); windows cover otherwise unowned ranges,
        // excluding the batch's explicit error/unsupported evidence.
        let first_symbol_start = symbols
            .iter()
            .map(|symbol| symbol.span.start_byte)
            .min()
            .unwrap_or(len);
        if first_symbol_start > 0 && !symbols.is_empty() {
            for window in fallback_windows(source, 0, first_symbol_start) {
                pending.push(PendingChunk {
                    grain: CodeSearchChunkGrainV1::FilePreamble,
                    symbol: None,
                    split_path: if window.1 < first_symbol_start {
                        window_split_path(0, window)
                    } else {
                        Vec::new()
                    },
                    span: SourceSpan {
                        start_byte: window.0,
                        end_byte: window.0 + window.1,
                    },
                    parent: None,
                });
            }
        }

        let mut covered: Vec<(u64, u64)> = symbols
            .iter()
            .map(|symbol| (symbol.span.start_byte, symbol.span.end_byte))
            .collect();
        if !symbols.is_empty() {
            covered.push((0, first_symbol_start));
        }
        covered.extend(
            batch
                .error_ranges
                .iter()
                .chain(&batch.unsupported_ranges)
                .map(|span| (span.start_byte, span.end_byte)),
        );
        covered.sort_unstable();
        let mut cursor = 0u64;
        let mut gap_ordinal = 0u64;
        for (start, end) in covered {
            if start > cursor {
                emit_windows(source, cursor, start, gap_ordinal, &mut pending);
                gap_ordinal += 1;
            }
            cursor = cursor.max(end);
        }
        if cursor < len {
            emit_windows(source, cursor, len, gap_ordinal, &mut pending);
        }

        // Canonical materialization: identify, order, and number.
        let mut chunks = Vec::with_capacity(pending.len());
        for piece in pending {
            if piece.span.is_empty() {
                continue;
            }
            let text = &source[piece.span.start_byte as usize..piece.span.end_byte as usize];
            if text.is_empty() {
                continue;
            }
            let symbol = piece.symbol.map(|index| &symbols[index]);
            let id = self.chunk_id(
                file_identity,
                symbol.map(|symbol| &symbol.identity),
                piece.grain,
                piece.split_path.clone(),
            )?;
            let parent_chunk_id = piece
                .parent
                .map(|(parent_index, parent_split)| {
                    let parent_symbol = &symbols[parent_index];
                    self.chunk_id(
                        file_identity,
                        Some(&parent_symbol.identity),
                        emissions[parent_index].grain,
                        parent_split,
                    )
                })
                .transpose()?;
            let (mut exact_terms, subtokens) = classify_chunk_text(text, piece.span.start_byte);
            if let Some(symbol) = symbol
                && let Some(span) = symbol_name_span(source, symbol)
                && span.start_byte >= piece.span.start_byte
                && span.end_byte <= piece.span.end_byte
                && let Ok(term) = ExactTechnicalTermV1::untrusted_whole_symbol_candidate(
                    source.as_bytes()[span.start_byte as usize..span.end_byte as usize].to_vec(),
                    span,
                    symbol.occurrence.clone(),
                )
            {
                exact_terms.push(term);
                exact_terms.sort_by(|left, right| {
                    (
                        left.span().start_byte,
                        left.span().end_byte,
                        left.kind(),
                        left.canonical_bytes(),
                        left.original_bytes(),
                    )
                        .cmp(&(
                            right.span().start_byte,
                            right.span().end_byte,
                            right.kind(),
                            right.canonical_bytes(),
                            right.original_bytes(),
                        ))
                });
            }
            chunks.push(CodeSearchChunkV1 {
                id,
                anchor: CodeSearchChunkAnchorV1 {
                    generation_id: self.generation_id.clone(),
                    file_occurrence_id: batch.file_occurrence_id.clone(),
                    symbol_occurrence_id: symbol.map(|symbol| symbol.occurrence.clone()),
                    parent_chunk_id,
                    source_span: piece.span,
                    grain: piece.grain,
                    ordinal: 0,
                },
                content_digest: content_digest(text.as_bytes()),
                language_descriptor_revision: descriptor.descriptor_revision.clone(),
                chunker_revision: self.chunker_revision.clone(),
                sanitizer_revision: self.sanitizer_revision.clone(),
                sensitivity: SensitivityDecision {
                    level: sensitivity_level,
                    policy_revision: self.policy_revision.clone(),
                },
                exact_terms,
                subtokens,
                sanitized_text: BoundedSanitizedText::new(text)
                    .map_err(|error| ChunkingFailureV1::NonCanonicalIdentity(error.to_string()))?,
            });
        }

        chunks.sort_by(|left, right| {
            left.anchor
                .source_span
                .start_byte
                .cmp(&right.anchor.source_span.start_byte)
                .then(
                    left.anchor
                        .source_span
                        .end_byte
                        .cmp(&right.anchor.source_span.end_byte),
                )
                .then(left.anchor.grain.cmp(&right.anchor.grain))
                .then(left.id.cmp(&right.id))
        });
        for (ordinal, chunk) in chunks.iter_mut().enumerate() {
            chunk.anchor.ordinal = ordinal as u32;
        }
        Ok(chunks)
    }
}

/// Resolve same-file symbol references (calls and other extractor
/// reference kinds) into relation edges. Only an UNAMBIGUOUS name match
/// against this file's own symbol table resolves; ambiguous or unmatched
/// references stay unresolved rather than guessing. Cross-file resolution
/// requires the dependency closure and is deliberately not attempted here.
fn resolve_same_file_references(unresolved: &[UnresolvedRef], symbols: &[SymbolRow]) -> Vec<Edge> {
    let mut by_name: BTreeMap<&str, Vec<&SymbolRow>> = BTreeMap::new();
    for symbol in symbols {
        by_name
            .entry(symbol.name.as_str())
            .or_default()
            .push(symbol);
    }
    let mut resolved = Vec::new();
    for reference in unresolved {
        let Some(candidates) = by_name.get(reference.reference_name.as_str()) else {
            continue;
        };
        let [target] = candidates.as_slice() else {
            continue;
        };
        resolved.push(Edge {
            source: reference.from_node_id.clone(),
            target: target.node_id.clone(),
            kind: reference.reference_kind,
            line: Some(reference.line),
        });
    }
    resolved
}

fn canonical_relation_edges(
    raw_edges: &[Edge],
    symbols: &[SymbolRow],
) -> (Vec<CanonicalRelationEdgeV1>, Vec<CodeIndexEdgeAbstentionV1>) {
    let mut by_node_id = BTreeMap::new();
    for symbol in symbols {
        match by_node_id.entry(symbol.node_id.as_str()) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(Some(symbol));
            }
            std::collections::btree_map::Entry::Occupied(mut entry) => {
                entry.insert(None);
            }
        }
    }

    let mut edges = Vec::new();
    let mut abstentions = Vec::new();
    for edge in raw_edges {
        let Some(Some(from)) = by_node_id.get(edge.source.as_str()) else {
            abstentions.push(edge_abstention(
                edge,
                CodeIndexEdgeAbstentionReasonV1::MissingSymbolEndpoint,
            ));
            continue;
        };
        let Some(Some(to)) = by_node_id.get(edge.target.as_str()) else {
            abstentions.push(edge_abstention(
                edge,
                CodeIndexEdgeAbstentionReasonV1::MissingSymbolEndpoint,
            ));
            continue;
        };
        let Some(kind) = canonical_relation_kind(&edge.kind) else {
            abstentions.push(edge_abstention(
                edge,
                CodeIndexEdgeAbstentionReasonV1::UnsupportedRelationKind,
            ));
            continue;
        };
        edges.push(CanonicalRelationEdgeV1 {
            from_occurrence: from.occurrence.clone(),
            to_occurrence: to.occurrence.clone(),
            kind,
            authority: EdgeAuthorityV1::SyntaxExact,
            evidence_span: from.span,
        });
    }
    edges.sort_by(|left, right| canonical_edge_key(left).cmp(&canonical_edge_key(right)));
    abstentions.sort();
    (edges, abstentions)
}

fn canonical_relation_kind(kind: &EdgeKind) -> Option<RelationEdgeKindV1> {
    match kind {
        EdgeKind::Contains => Some(RelationEdgeKindV1::Contains),
        EdgeKind::Calls => Some(RelationEdgeKindV1::Calls),
        EdgeKind::Uses => Some(RelationEdgeKindV1::Uses),
        EdgeKind::Implements => Some(RelationEdgeKindV1::Implements),
        EdgeKind::TypeOf => Some(RelationEdgeKindV1::TypeOf),
        EdgeKind::Extends => Some(RelationEdgeKindV1::Extends),
        EdgeKind::Annotates => Some(RelationEdgeKindV1::Annotates),
        EdgeKind::Returns | EdgeKind::DerivesMacro | EdgeKind::Receives => None,
    }
}

fn edge_abstention(
    edge: &Edge,
    reason: CodeIndexEdgeAbstentionReasonV1,
) -> CodeIndexEdgeAbstentionV1 {
    CodeIndexEdgeAbstentionV1 {
        source_node_id: edge.source.clone(),
        target_node_id: edge.target.clone(),
        legacy_kind: edge.kind.as_str().to_owned(),
        reason,
    }
}

fn canonical_edge_key(
    edge: &CanonicalRelationEdgeV1,
) -> (
    &SymbolOccurrenceId,
    &SymbolOccurrenceId,
    RelationEdgeKindV1,
    u64,
    u64,
) {
    (
        &edge.from_occurrence,
        &edge.to_occurrence,
        edge.kind,
        edge.evidence_span.start_byte,
        edge.evidence_span.end_byte,
    )
}

/// End offset of the line containing `start` (exclusive of the newline).
fn offsets_line_end(source: &str, start: u64) -> u64 {
    let rest = &source[start as usize..];
    start
        + rest
            .find('\n')
            .map_or(rest.len() as u64, |index| index as u64)
}

/// Emit pinned fallback windows over one unowned gap as `FileWindow` chunks.
///
/// The split path is `[gap ordinal, byte offset within the gap, window size]`:
/// gap-relative, never file-absolute, so pure line shifts outside the gap
/// leave the chunk identity unchanged (content digests still track content),
/// while the gap ordinal keeps two unowned regions from minting the same id.
fn emit_windows(
    source: &str,
    start: u64,
    end: u64,
    gap_ordinal: u64,
    pending: &mut Vec<PendingChunk>,
) {
    for window in fallback_windows(source, start, end) {
        let mut split_path = vec![u32::try_from(gap_ordinal).unwrap_or(u32::MAX)];
        split_path.extend(window_split_path(start, window));
        pending.push(PendingChunk {
            grain: CodeSearchChunkGrainV1::FileWindow,
            symbol: None,
            split_path,
            span: SourceSpan {
                start_byte: window.0,
                end_byte: window.0 + window.1,
            },
            parent: None,
        });
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use super::*;
    use tracedecay_domain::{
        BoundedSanitizedText, ChunkerRevision, CodeGenerationId, CodeSearchChunkAnchorV1,
        CodeSearchChunkGrainV1, CodeSearchChunkId, CodeSearchEligibilityV1, ContentDigest,
        ExtractionCoverageV1, FileIdentityDigest, FileOccurrenceId, GrammarRevision,
        LanguageDescriptorRevision, LanguageId, ManifestDigest, ParseOutcomeV1, PolicyRevisionId,
        ProjectId, SanitizationReceiptId, SanitizedCodeFileV1, SanitizedCodeSnapshotV1,
        SanitizerRevision, SensitivityDecision, SensitivityLevelV1, SnapshotFileDispositionV1,
        SourceSpan, SymbolIdentityDigest, SymbolOccurrenceId, UtcMicros, ValidatedCodeFileV1,
    };

    use crate::extract::{
        ExtractionCancellation, LanguageExtractor as CanonicalLanguageExtractor, NeverCancelled,
        TreeSitterExtractor,
    };
    use crate::intake::{CodeIndexIntake, SanitizedCodeIntake};
    use crate::languages::{LanguageRegistry, StaticLanguageRegistry};

    struct AlwaysCancelled;

    impl ExtractionCancellation for AlwaysCancelled {
        fn is_cancelled(&self) -> bool {
            true
        }
    }

    fn id<T>(value: &str) -> T
    where
        T: TryFrom<String>,
        <T as TryFrom<String>>::Error: std::fmt::Debug,
    {
        T::try_from(value.to_owned()).expect("valid fixture identity")
    }

    fn digest(byte: char) -> String {
        format!("sha256:{}", byte.to_string().repeat(64))
    }

    fn symbol_identity(byte: char) -> SymbolIdentityDigest {
        SymbolIdentityDigest::new(digest(byte)).expect("valid fixture symbol identity")
    }

    fn file_identity(byte: char) -> FileIdentityDigest {
        FileIdentityDigest::new(digest(byte)).expect("valid fixture file identity")
    }

    fn fixture_symbol_row(
        node_id: &str,
        occurrence: &str,
        qualified_name: &str,
        kind: &str,
        identity_byte: char,
        span: SourceSpan,
    ) -> SymbolRow {
        SymbolRow {
            node_id: node_id.to_owned(),
            span,
            name: qualified_name
                .rsplit("::")
                .next()
                .unwrap_or(qualified_name)
                .to_owned(),
            qualified_name: qualified_name.to_owned(),
            kind: kind.to_owned(),
            parent: None,
            identity: symbol_identity(identity_byte),
            occurrence: SymbolOccurrenceId::new(occurrence)
                .expect("valid fixture symbol occurrence"),
        }
    }

    fn file_chunks() -> CodeFileChunksV1 {
        let generation_id: CodeGenerationId = id("generation.fixture");
        let file_occurrence_id: FileOccurrenceId = id("file.fixture");
        let chunk_id: CodeSearchChunkId = id("chunk.fixture");
        CodeFileChunksV1 {
            document: CodeSearchDocumentV1 {
                generation_id: generation_id.clone(),
                file_occurrence_id: file_occurrence_id.clone(),
                content_digest: id::<ContentDigest>(&digest('a')),
                eligibility: CodeSearchEligibilityV1::Eligible,
                chunk_ids: vec![chunk_id.clone()],
            },
            chunks: vec![CodeSearchChunkV1 {
                id: chunk_id,
                anchor: CodeSearchChunkAnchorV1 {
                    generation_id,
                    file_occurrence_id,
                    symbol_occurrence_id: None,
                    parent_chunk_id: None,
                    source_span: SourceSpan {
                        start_byte: 0,
                        end_byte: 4,
                    },
                    grain: CodeSearchChunkGrainV1::FileWindow,
                    ordinal: 0,
                },
                content_digest: id::<ContentDigest>(&digest('b')),
                language_descriptor_revision: id::<LanguageDescriptorRevision>("descriptor.v1"),
                chunker_revision: id::<ChunkerRevision>("chunker.v1"),
                sanitizer_revision: id::<SanitizerRevision>("sanitizer.v1"),
                sensitivity: SensitivityDecision {
                    level: SensitivityLevelV1::Internal,
                    policy_revision: id::<PolicyRevisionId>("policy.v1"),
                },
                exact_terms: vec![],
                subtokens: vec!["text".to_owned()],
                sanitized_text: BoundedSanitizedText::new("text").unwrap(),
            }],
        }
    }

    #[test]
    fn file_chunks_reject_mixed_generation_or_document_membership() {
        file_chunks().validate().expect("consistent file chunks");

        let mut mixed_generation = file_chunks();
        mixed_generation.chunks[0].anchor.generation_id = id("generation.other");
        assert_eq!(
            mixed_generation.validate(),
            Err(ChunkingFailureV1::GenerationMismatch)
        );

        let mut wrong_membership = file_chunks();
        wrong_membership.document.chunk_ids[0] = id("chunk.other");
        assert!(wrong_membership.validate().is_err());
    }

    #[test]
    fn admitted_chunk_is_consumed_without_widening_mint_authority() {
        let chunks = file_chunks();
        let expected = chunks.chunks[0].clone();
        let authority = ExactExtractionAuthorityV1::restore(&chunks).expect("sealed authority");
        let admitted = authority.admit(expected.clone()).expect("exact admission");

        assert_eq!(admitted.into_chunk(), expected);
    }

    const RUST_SOURCE: &str = "//! Module documentation.\n\nuse std::collections::HashMap;\n\n/// Doc comment.\npub fn alpha(x: u32) -> u32 {\n    x + 1\n}\n\npub struct Holder {\n    map: HashMap<u32, u32>,\n}\n\nimpl Holder {\n    pub fn get(&self, key: u32) -> Option<u32> {\n        self.map.get(&key).copied()\n    }\n}\n\n// A trailing free-floating comment.\n";

    fn chunker() -> DeterministicCodeChunker {
        DeterministicCodeChunker::new(
            id("generation.fixture"),
            id("repo.fixture"),
            id("sanitizer.v1"),
            id("policy.v1"),
            id("chunker.v1"),
            tracedecay_code_extraction::LanguageRegistry::new(),
        )
    }

    fn validated_file(path: &str, bytes: &[u8]) -> ReceiptBoundCodeFileV1 {
        let file = SanitizedCodeFileV1 {
            file_occurrence_id: id("file.fixture"),
            logical_path: path.to_owned(),
            language: Some(id::<LanguageId>("rust")),
            content_digest: content_digest(bytes),
            disposition: SnapshotFileDispositionV1::Present,
        };
        let intake = SanitizedCodeIntake::new(
            StaticLanguageRegistry::new(),
            id::<SanitizerRevision>("sanitizer.v1"),
            UtcMicros(1_000_000),
        );
        let capability = intake
            .admit(SanitizedCodeSnapshotV1 {
                repository: id("repo.fixture"),
                worktree: None,
                reference: None,
                source_revision: None,
                sanitizer_revision: id("sanitizer.v1"),
                sanitization_receipts: vec![id::<SanitizationReceiptId>("receipt.fixture")],
                content_identity: content_digest(bytes),
                captured_at: UtcMicros(1_000_000),
                files: vec![file.clone()],
            })
            .expect("snapshot capability");
        intake
            .bind_file(
                &capability,
                &id::<ProjectId>("project.fixture"),
                ValidatedCodeFileV1 {
                    generation_id: id("generation.fixture"),
                    file,
                    snapshot_digest: capability.snapshot().intake_digest.clone(),
                    sanitized_bytes: bytes.to_vec(),
                },
            )
            .expect("receipt-bound file")
    }

    fn batch_for(file: &ReceiptBoundCodeFileV1, outcome: ParseOutcomeV1) -> ExtractionBatchV1 {
        let descriptor = rust_descriptor();
        ExtractionBatchV1 {
            generation_id: file.generation_id.clone(),
            file_occurrence_id: file.file.file_occurrence_id.clone(),
            language: descriptor.language.clone(),
            descriptor_revision: descriptor.descriptor_revision.clone(),
            grammar_revision: descriptor.grammar_revision.clone(),
            extractor_revision: descriptor.extractor_revision.clone(),
            content_digest: file.file.content_digest.clone(),
            parse_outcome: outcome,
            parsed_ranges: vec![SourceSpan {
                start_byte: 0,
                end_byte: file.sanitized_bytes.len() as u64,
            }],
            error_ranges: Vec::new(),
            unsupported_ranges: Vec::new(),
            coverage: ExtractionCoverageV1 {
                parsed_bytes: file.sanitized_bytes.len() as u64,
                ..ExtractionCoverageV1::default()
            },
            rows_digest: id::<ManifestDigest>(&digest('d')),
        }
    }

    fn rust_descriptor() -> tracedecay_domain::LanguageDescriptorV1 {
        StaticLanguageRegistry::new()
            .descriptor(&id::<LanguageId>("rust"))
            .expect("rust descriptor")
            .clone()
    }

    fn chunk_source(source: &str) -> CodeFileChunksV1 {
        let file = validated_file("src/lib.rs", source.as_bytes());
        let batch = batch_for(&file, ParseOutcomeV1::Complete);
        chunker()
            .chunk_file(&file, &batch, &rust_descriptor(), &NeverCancelled)
            .expect("chunking succeeds")
    }

    /// A chunk set large enough to cross `PARALLEL_CHUNK_THRESHOLD`, built from
    /// real extraction rather than hand-assembled chunks.
    fn wide_chunk_source(symbols: usize) -> String {
        let mut source =
            String::from("//! Module documentation.\n\nuse std::collections::HashMap;\n\n");
        for index in 0..symbols {
            source.push_str(&format!(
                "/// Doc comment for symbol {index}.\npub fn symbol_{index}(value: u32, label: &str) -> u32 {{\n    let mapping: HashMap<u32, &str> = HashMap::new();\n    let _ = mapping.get(&value).unwrap_or(&label);\n    value + {index}\n}}\n\n"
            ));
        }
        source
    }

    fn wide_chunks(symbols: usize) -> CodeFileChunksV1 {
        let chunks = chunk_source(&wide_chunk_source(symbols));
        assert!(
            chunks.chunks.len() > PARALLEL_CHUNK_THRESHOLD,
            "fixture must cross the parallel threshold, got {} chunks",
            chunks.chunks.len()
        );
        chunks
    }

    fn sequential_digest_reference(
        chunks: &[CodeSearchChunkV1],
    ) -> BTreeMap<CodeSearchChunkId, String> {
        let mut digests = BTreeMap::new();
        for chunk in chunks {
            digests.insert(
                chunk.id.clone(),
                canonical_digest(EXACT_EXTRACTION_AUTHORITY_SEPARATOR, chunk)
                    .expect("reference digest"),
            );
        }
        digests
    }

    /// The fanned-out digest sweep must produce byte-identical digests, in the
    /// same association, as the single-threaded reference it replaced.
    #[test]
    fn parallel_digest_sweep_matches_the_sequential_reference() {
        let chunks = wide_chunks(48);
        let reference = sequential_digest_reference(&chunks.chunks);

        let authority = ExactExtractionAuthorityV1::restore(&chunks).expect("sealed authority");
        assert_eq!(authority.chunk_digests, reference);

        authority
            .validate_all(&chunks.chunks)
            .expect("parallel validation accepts its own chunks");

        let admitted = authority
            .admit_all(chunks.chunks.clone())
            .expect("parallel admission");
        let readmitted = admitted
            .into_iter()
            .map(ExtractionAdmittedCodeSearchChunkV1::into_chunk)
            .collect::<Vec<_>>();
        assert_eq!(readmitted, chunks.chunks, "admission must preserve order");
    }

    /// The fanned-out sweeps still report the lowest-index failure, so callers
    /// observe the same error the sequential short-circuit produced.
    #[test]
    fn parallel_validation_reports_the_lowest_index_failure() {
        let baseline = wide_chunks(48);
        let early = 3usize;
        let late = baseline.chunks.len() - 2;
        assert!(early < late);

        let mut identity_first = baseline.clone();
        identity_first.chunks[early].anchor.parent_chunk_id =
            Some(identity_first.chunks[early].id.clone());
        identity_first.chunks[late].anchor.generation_id = id("generation.other");
        assert!(matches!(
            identity_first.validate(),
            Err(ChunkingFailureV1::NonCanonicalIdentity(_))
        ));

        let mut generation_first = baseline.clone();
        generation_first.chunks[early].anchor.generation_id = id("generation.other");
        generation_first.chunks[late].anchor.parent_chunk_id =
            Some(generation_first.chunks[late].id.clone());
        assert_eq!(
            generation_first.validate(),
            Err(ChunkingFailureV1::GenerationMismatch)
        );
    }

    /// Timing probe, not an assertion: prints the single-threaded canonical
    /// digest sweep against the fanned-out mint over the same fixture.
    /// `cargo test --release -p tracedecay-code-index -- --ignored --nocapture`
    #[test]
    #[ignore = "timing probe; run explicitly with --ignored --nocapture"]
    fn digest_sweep_timing_probe() {
        use std::time::{Duration, Instant};

        /// Report the fastest of `rounds` runs: on a shared build host the mean
        /// tracks neighbouring load, the minimum tracks the code.
        fn best_of(rounds: u32, mut run: impl FnMut()) -> Duration {
            run();
            (0..rounds)
                .map(|_| {
                    let started = Instant::now();
                    run();
                    started.elapsed()
                })
                .min()
                .unwrap_or_default()
        }

        let chunks = wide_chunks(400);
        let rounds = 25;

        let sequential = best_of(rounds, || {
            let _ = sequential_digest_reference(&chunks.chunks);
        });
        let parallel = best_of(rounds, || {
            let _ = ExactExtractionAuthorityV1::mint(&chunks.chunks).expect("mint");
        });
        let validate = best_of(rounds, || {
            chunks.validate().expect("validate");
        });

        println!(
            "chunks={} sequential_digest_sweep={sequential:?} parallel_mint={parallel:?} validate={validate:?}",
            chunks.chunks.len()
        );
    }

    #[test]
    fn five_grains_cover_every_eligible_byte() {
        let result = chunk_source(RUST_SOURCE);
        result.validate().expect("valid chunk set");
        assert_eq!(
            result.document.eligibility,
            CodeSearchEligibilityV1::Eligible
        );

        let grains: BTreeSet<CodeSearchChunkGrainV1> = result
            .chunks
            .iter()
            .map(|chunk| chunk.anchor.grain)
            .collect();
        for grain in [
            CodeSearchChunkGrainV1::SymbolSignature,
            CodeSearchChunkGrainV1::SymbolBody,
            CodeSearchChunkGrainV1::SymbolMember,
            CodeSearchChunkGrainV1::FilePreamble,
            CodeSearchChunkGrainV1::FileWindow,
        ] {
            assert!(grains.contains(&grain), "grain {grain:?} present");
        }

        // Union of chunk spans covers every byte of the file.
        let mut covered = vec![false; RUST_SOURCE.len()];
        for chunk in &result.chunks {
            for covered_byte in &mut covered[chunk.anchor.source_span.start_byte as usize
                ..chunk.anchor.source_span.end_byte as usize]
            {
                *covered_byte = true;
            }
        }
        assert!(covered.iter().all(|covered| *covered), "full byte coverage");

        // Member chunks link to their parent symbol's body chunk; the
        // document manifest lists chunks in canonical order.
        let member = result
            .chunks
            .iter()
            .find(|chunk| chunk.anchor.grain == CodeSearchChunkGrainV1::SymbolMember)
            .expect("member chunk");
        let parent = member
            .anchor
            .parent_chunk_id
            .as_ref()
            .expect("member parent");
        let parent_chunk = result
            .chunks
            .iter()
            .find(|chunk| &chunk.id == parent)
            .expect("parent chunk exists");
        assert_eq!(
            parent_chunk.anchor.grain,
            CodeSearchChunkGrainV1::SymbolBody
        );
        assert!(member.anchor.symbol_occurrence_id.is_some());

        // Ordinals are a canonical permutation.
        let ordinals: BTreeSet<u32> = result.chunks.iter().map(|c| c.anchor.ordinal).collect();
        assert_eq!(ordinals.len(), result.chunks.len());

        // Signature grain carries the symbol's first line.
        assert!(result.chunks.iter().any(|chunk| {
            chunk.anchor.grain == CodeSearchChunkGrainV1::SymbolSignature
                && chunk.sanitized_text.as_str().contains("fn ")
        }));
    }

    #[test]
    fn symbol_member_chunks_include_leading_attributes() {
        let result = chunk_source(
            "pub enum DomainError {\n    #[error(\"time interval start must not be after its end\")]\n    InvalidTimeInterval,\n}\n",
        );
        assert!(result.chunks.iter().any(|chunk| {
            chunk.anchor.grain == CodeSearchChunkGrainV1::SymbolMember
                && chunk
                    .sanitized_text
                    .as_str()
                    .contains("InvalidTimeInterval")
                && chunk
                    .sanitized_text
                    .as_str()
                    .contains("time interval start must not be after its end")
        }));
    }

    #[test]
    fn chunk_identity_ignores_content_and_line_numbers() {
        let baseline = chunk_source(RUST_SOURCE);
        // Same structure, same logical path, edited bodies and shifted
        // lines: chunk ids are unchanged, content digests are not.
        let edited_source = format!("\n\n{RUST_SOURCE}").replacen("x + 1", "x + 2", 1);
        let edited = chunk_source(&edited_source);

        let baseline_ids: BTreeSet<&CodeSearchChunkId> =
            baseline.chunks.iter().map(|chunk| &chunk.id).collect();
        let edited_ids: BTreeSet<&CodeSearchChunkId> =
            edited.chunks.iter().map(|chunk| &chunk.id).collect();
        assert_eq!(baseline_ids, edited_ids, "identity is content/line free");

        let digest_changed = baseline
            .chunks
            .iter()
            .zip(&edited.chunks)
            .any(|(left, right)| left.content_digest != right.content_digest);
        assert!(digest_changed, "content digests track content");
    }

    #[test]
    fn chunking_is_deterministic_across_runs() {
        let first = chunk_source(RUST_SOURCE);
        let second = chunk_source(RUST_SOURCE);
        assert_eq!(first, second);
    }

    struct CountingRustExtractor {
        calls: Arc<AtomicUsize>,
    }

    impl tracedecay_code_extraction::LanguageExtractor for CountingRustExtractor {
        fn extensions(&self) -> &[&str] {
            tracedecay_code_extraction::RustExtractor.extensions()
        }

        fn language_name(&self) -> &str {
            tracedecay_code_extraction::RustExtractor.language_name()
        }

        fn extract(&self, file_path: &str, source: &str) -> tracedecay_domain::ExtractionResult {
            self.calls.fetch_add(1, Ordering::Relaxed);
            tracedecay_code_extraction::RustExtractor.extract(file_path, source)
        }
    }

    #[test]
    fn extraction_artifacts_feed_chunking_without_a_second_parse() {
        let calls = Arc::new(AtomicUsize::new(0));
        let registry = Arc::new(
            tracedecay_code_extraction::LanguageRegistry::from_extractors_for_test(vec![Box::new(
                CountingRustExtractor {
                    calls: Arc::clone(&calls),
                },
            )]),
        );
        let extractor = TreeSitterExtractor::from_shared_registry(Arc::clone(&registry));
        let file = validated_file("src/lib.rs", RUST_SOURCE.as_bytes());
        let descriptor = rust_descriptor();
        let extraction = extractor
            .extract(&file, &descriptor, &NeverCancelled)
            .expect("extraction succeeds");
        assert_eq!(calls.load(Ordering::Relaxed), 1);

        let shared_chunker = DeterministicCodeChunker::from_shared_registry(
            id("generation.fixture"),
            id("repo.fixture"),
            id("sanitizer.v1"),
            id("policy.v1"),
            id("chunker.v1"),
            registry,
        );
        let (actual, _) = shared_chunker
            .index_file_with_authority_from_extraction(
                &file,
                &extraction,
                &descriptor,
                SensitivityLevelV1::Public,
                &NeverCancelled,
            )
            .expect("chunking reuses parse artifacts");
        assert_eq!(
            calls.load(Ordering::Relaxed),
            1,
            "chunking must not invoke the parser again"
        );

        let expected = chunker()
            .index_file(&file, extraction.batch(), &descriptor, &NeverCancelled)
            .expect("legacy chunking succeeds");
        assert_eq!(actual, expected, "parse reuse must preserve every artifact");
    }

    #[test]
    fn oversized_body_splits_on_pinned_fallback_windows() {
        use std::fmt::Write as _;
        let mut source = String::from("pub fn huge() {\n");
        for index in 0..9000 {
            writeln!(source, "    let value_{index} = {index}usize;").unwrap();
        }
        source.push_str("}\n");
        assert!(source.len() > MAX_CHUNK_TEXT_BYTES);

        let result = chunk_source(&source);
        result.validate().expect("valid chunk set");
        let body_pieces: Vec<&CodeSearchChunkV1> = result
            .chunks
            .iter()
            .filter(|chunk| chunk.anchor.grain == CodeSearchChunkGrainV1::SymbolBody)
            .collect();
        assert!(body_pieces.len() > 1, "oversized body split into windows");
        for piece in &body_pieces {
            assert!(piece.sanitized_text.as_str().len() <= MAX_CHUNK_TEXT_BYTES);
            assert_eq!(piece.anchor.parent_chunk_id, None);
        }
        // Pinned fallback split: the first window starts at the body start
        // and window texts tile the body with the pinned overlap.
        let first_path_window = &body_pieces[0];
        assert_eq!(first_path_window.anchor.source_span.start_byte, 0);
        assert!(
            first_path_window
                .sanitized_text
                .as_str()
                .starts_with("pub fn huge()")
        );

        // Union of window spans covers the whole body (pinned overlap).
        let mut ordered: Vec<(u64, u64)> = body_pieces
            .iter()
            .map(|piece| {
                (
                    piece.anchor.source_span.start_byte,
                    piece.anchor.source_span.end_byte,
                )
            })
            .collect();
        ordered.sort_unstable();
        let body_start = ordered.first().expect("pieces").0;
        let body_end = ordered.last().expect("pieces").1;
        let mut cursor = body_start;
        for (start, end) in ordered {
            assert!(start <= cursor, "windows overlap or abut (pinned overlap)");
            cursor = cursor.max(end);
        }
        assert_eq!(cursor, body_end);

        // Deterministic split across runs.
        let again = chunk_source(&source);
        assert_eq!(result, again);
    }

    #[test]
    fn oversized_impl_splits_on_member_boundaries() {
        use std::fmt::Write as _;
        let mut source = String::from("pub struct Big;\n\nimpl Big {\n");
        for index in 0..300 {
            writeln!(source, "    pub fn method_{index}() -> usize {{").unwrap();
            source.push_str("        ");
            source.push_str(&"1 + ".repeat(300));
            source.push_str("1\n    }\n");
        }
        source.push_str("}\n");
        assert!(source.len() > MAX_CHUNK_TEXT_BYTES);

        let result = chunk_source(&source);
        result.validate().expect("valid chunk set");

        // Group body pieces by symbol occurrence; the impl symbol is the one
        // split into multiple pieces.
        let mut by_occurrence: std::collections::BTreeMap<
            &tracedecay_domain::SymbolOccurrenceId,
            Vec<&CodeSearchChunkV1>,
        > = std::collections::BTreeMap::new();
        for chunk in result
            .chunks
            .iter()
            .filter(|chunk| chunk.anchor.grain == CodeSearchChunkGrainV1::SymbolBody)
        {
            by_occurrence
                .entry(
                    chunk
                        .anchor
                        .symbol_occurrence_id
                        .as_ref()
                        .expect("body symbol"),
                )
                .or_default()
                .push(chunk);
        }
        let impl_pieces = by_occurrence
            .values()
            .find(|pieces| pieces.len() > 1)
            .expect("oversized impl split at member boundaries");
        // Structural split: the first piece is the impl header and every
        // later piece starts exactly on a member boundary.
        let first_text = impl_pieces[0].sanitized_text.as_str();
        assert!(first_text.starts_with("impl Big"));
        for piece in &impl_pieces[1..] {
            let start = piece.anchor.source_span.start_byte as usize;
            assert!(
                source[start..].starts_with("pub fn method_"),
                "piece starts on a member boundary: {:?}",
                &source[start..start + 24]
            );
        }

        // Members are still declared as child chunks of the impl body.
        let members = result
            .chunks
            .iter()
            .filter(|chunk| chunk.anchor.grain == CodeSearchChunkGrainV1::SymbolMember)
            .count();
        assert_eq!(members, 300);

        // Deterministic split across runs.
        let again = chunk_source(&source);
        assert_eq!(result, again);
    }

    #[test]
    fn descriptor_and_generation_mismatch_are_typed_failures() {
        let file = validated_file("src/lib.rs", RUST_SOURCE.as_bytes());
        let batch = batch_for(&file, ParseOutcomeV1::Complete);

        // Descriptor mismatch: python descriptor against a rust batch.
        let python = StaticLanguageRegistry::new()
            .descriptor(&id::<LanguageId>("python"))
            .expect("python descriptor")
            .clone();
        assert_eq!(
            chunker().chunk_file(&file, &batch, &python, &NeverCancelled),
            Err(ChunkingFailureV1::DescriptorMismatch)
        );

        // Generation mismatch: batch attests a different content digest.
        let mut stale_batch = batch.clone();
        stale_batch.content_digest = id::<ContentDigest>(&digest('f'));
        assert_eq!(
            chunker().chunk_file(&file, &stale_batch, &rust_descriptor(), &NeverCancelled),
            Err(ChunkingFailureV1::GenerationMismatch)
        );

        // Generation mismatch: batch belongs to another generation.
        let mut other_generation = batch.clone();
        other_generation.generation_id = id("generation.other");
        assert_eq!(
            chunker().chunk_file(
                &file,
                &other_generation,
                &rust_descriptor(),
                &NeverCancelled
            ),
            Err(ChunkingFailureV1::GenerationMismatch)
        );

        // Cancellation is a typed failure.
        assert_eq!(
            chunker().chunk_file(&file, &batch, &rust_descriptor(), &AlwaysCancelled),
            Err(ChunkingFailureV1::Cancelled)
        );
    }

    #[test]
    fn failed_parse_yields_an_explicit_unsupported_document() {
        let file = validated_file("src/lib.rs", RUST_SOURCE.as_bytes());
        let batch = batch_for(
            &file,
            ParseOutcomeV1::Failed {
                reason: "grammar crashed".to_owned(),
            },
        );
        let result = chunker()
            .chunk_file(&file, &batch, &rust_descriptor(), &NeverCancelled)
            .expect("failed parse is evidence, not an error");
        assert!(result.chunks.is_empty());
        assert!(matches!(
            result.document.eligibility,
            CodeSearchEligibilityV1::Unsupported { .. }
        ));
        result.validate().expect("unsupported document validates");
    }

    #[test]
    fn partial_parse_is_declared_on_the_document() {
        let file = validated_file("src/lib.rs", RUST_SOURCE.as_bytes());
        let batch = batch_for(
            &file,
            ParseOutcomeV1::Partial {
                reason: "bounded traversal cap reached".to_owned(),
            },
        );
        let result = chunker()
            .chunk_file(&file, &batch, &rust_descriptor(), &NeverCancelled)
            .expect("partial parse still chunks");
        assert_eq!(
            result.document.eligibility,
            CodeSearchEligibilityV1::Partial {
                reason: "bounded traversal cap reached".to_owned()
            }
        );
        assert!(!result.chunks.is_empty());
    }

    #[test]
    fn exact_terms_and_subtokens_are_classified() {
        let (terms, subtokens) = classify_chunk_text(
            "std::collections::HashMap src/main.rs --release tracedecay.data.dir E0308 pub fn alpha() {} betaValue",
            0,
        );
        let kinds: BTreeSet<ExactTechnicalTermKindV1> =
            terms.iter().map(ExactTechnicalTermV1::kind).collect();
        assert!(kinds.contains(&ExactTechnicalTermKindV1::QualifiedName));
        assert!(kinds.contains(&ExactTechnicalTermKindV1::Path));
        assert!(kinds.contains(&ExactTechnicalTermKindV1::CliFlag));
        assert!(kinds.contains(&ExactTechnicalTermKindV1::ConfigurationKey));
        assert!(kinds.contains(&ExactTechnicalTermKindV1::CompilerErrorCode));
        assert!(
            !kinds.contains(&ExactTechnicalTermKindV1::WholeSymbol),
            "text tokenization cannot mint symbol authority"
        );

        let error_term = terms
            .iter()
            .find(|term| term.kind() == ExactTechnicalTermKindV1::CompilerErrorCode)
            .expect("error code term");
        assert_eq!(error_term.original_bytes(), b"E0308");
        let flag = terms
            .iter()
            .find(|term| term.kind() == ExactTechnicalTermKindV1::CliFlag)
            .expect("flag term");
        assert_eq!(flag.canonical_bytes(), b"--release");

        // Subtokens split snake/camel/qualified tokens, lowercased.
        for expected in [
            "std",
            "collections",
            "hash",
            "map",
            "src",
            "main",
            "rs",
            "alpha",
            "beta",
            "value",
            "release",
            "e",
            "0308",
        ] {
            assert!(
                subtokens.iter().any(|subtoken| subtoken == expected),
                "subtoken {expected} present in {subtokens:?}"
            );
        }
        // Deterministic.
        let again = classify_chunk_text(
            "std::collections::HashMap src/main.rs --release tracedecay.data.dir E0308 pub fn alpha() {} betaValue",
            0,
        );
        assert_eq!(terms, again.0);
        assert_eq!(subtokens, again.1);
    }

    #[test]
    fn exact_term_minting_rejects_untyped_lookalikes() {
        let (terms, _) = classify_chunk_text(
            "ordinary prose\n\
             A1234 E_NOT_A_CODE deadbeef foo.bar docs/readme\n\
             error: loose prose\n\
             compiler error: typed failure\n\
             ERR_MODULE_NOT_FOUND commit:deadbeef tracedecay.data.dir src/main.rs\n\
             // fn comment_fake() {}\n\
             let source = \"fn string_fake() {}\";\n\
             fn\nnewline_fake\n\
             fn;;;punctuation_fake\n",
            0,
        );

        for rejected in [
            "ordinary",
            "prose",
            "A1234",
            "E_NOT_A_CODE",
            "deadbeef",
            "foo.bar",
            "docs/readme",
            "loose prose",
            "comment_fake",
            "string_fake",
            "newline_fake",
            "punctuation_fake",
        ] {
            assert!(
                terms
                    .iter()
                    .all(|term| term.original_bytes() != rejected.as_bytes()),
                "{rejected:?} must remain subtoken-only evidence"
            );
        }

        for (kind, accepted) in [
            (ExactTechnicalTermKindV1::CompilerErrorText, "typed failure"),
            (
                ExactTechnicalTermKindV1::RuntimeErrorCode,
                "ERR_MODULE_NOT_FOUND",
            ),
            (
                ExactTechnicalTermKindV1::CommitIdentifier,
                "commit:deadbeef",
            ),
            (
                ExactTechnicalTermKindV1::ConfigurationKey,
                "tracedecay.data.dir",
            ),
            (ExactTechnicalTermKindV1::Path, "src/main.rs"),
        ] {
            assert!(
                terms.iter().any(|term| {
                    term.kind() == kind && term.original_bytes() == accepted.as_bytes()
                }),
                "missing typed exact term {kind:?}: {accepted}"
            );
        }
    }

    #[test]
    fn parser_evidence_mints_only_real_whole_symbols() {
        let source = r#"
// fn comment_fake() {}
const TEXT: &str = "fn string_fake() {}";
// fn
// newline_fake
// fn;;;punctuation_fake
pub fn real_symbol() {}
"#;
        let result = chunk_source(source);
        let symbols: BTreeSet<Vec<u8>> = result
            .chunks
            .iter()
            .flat_map(|chunk| {
                chunk
                    .exact_terms
                    .iter()
                    .filter(|term| term.kind() == ExactTechnicalTermKindV1::WholeSymbol)
                    .map(|term| term.original_bytes().to_vec())
            })
            .collect();

        assert!(symbols.contains(b"real_symbol".as_slice()));
        for rejected in [
            b"comment_fake".as_slice(),
            b"string_fake".as_slice(),
            b"newline_fake".as_slice(),
            b"punctuation_fake".as_slice(),
        ] {
            assert!(!symbols.contains(rejected));
        }
    }

    #[test]
    fn lineage_symbols_preserve_extracted_identity_and_canonical_order() {
        let source = "alpha\nbeta\n";
        let file_identity = file_identity('f');
        let rows = vec![
            fixture_symbol_row(
                "node.beta",
                "sym.beta",
                "crate::beta",
                "function",
                'b',
                SourceSpan {
                    start_byte: 6,
                    end_byte: 10,
                },
            ),
            fixture_symbol_row(
                "node.alpha",
                "sym.alpha",
                "crate::alpha",
                "function",
                'a',
                SourceSpan {
                    start_byte: 0,
                    end_byte: 5,
                },
            ),
        ];

        let symbols = chunker()
            .lineage_symbols(source, &file_identity, &rows)
            .expect("valid symbol lineage records");

        assert_eq!(
            symbols
                .iter()
                .map(|symbol| symbol.occurrence.as_str())
                .collect::<Vec<_>>(),
            vec!["sym.alpha", "sym.beta"],
            "lineage records must be canonically ordered by occurrence"
        );
        let alpha = symbols
            .iter()
            .find(|symbol| symbol.qualified_name == "crate::alpha")
            .expect("alpha lineage record");
        assert_eq!(alpha.identity, symbol_identity('a'));
        assert_eq!(alpha.kind, "function");
        assert_eq!(alpha.file_identity, file_identity);
        assert_eq!(alpha.content_digest, content_digest(b"alpha"));
    }

    #[test]
    fn canonical_relation_edges_bind_node_ids_sort_and_record_abstentions() {
        let symbols = vec![
            fixture_symbol_row(
                "node.alpha",
                "sym.zeta",
                "crate::alpha",
                "function",
                'a',
                SourceSpan {
                    start_byte: 20,
                    end_byte: 30,
                },
            ),
            fixture_symbol_row(
                "node.beta",
                "sym.alpha",
                "crate::beta",
                "function",
                'b',
                SourceSpan {
                    start_byte: 4,
                    end_byte: 9,
                },
            ),
            fixture_symbol_row(
                "node.gamma",
                "sym.middle",
                "crate::gamma",
                "function",
                'c',
                SourceSpan {
                    start_byte: 10,
                    end_byte: 19,
                },
            ),
        ];
        let raw_edges = vec![
            Edge {
                source: "node.alpha".to_owned(),
                target: "node.beta".to_owned(),
                kind: EdgeKind::Calls,
                line: Some(20),
            },
            Edge {
                source: "node.beta".to_owned(),
                target: "node.gamma".to_owned(),
                kind: EdgeKind::Uses,
                line: Some(4),
            },
            Edge {
                source: "node.alpha".to_owned(),
                target: "node.missing".to_owned(),
                kind: EdgeKind::Calls,
                line: None,
            },
            Edge {
                source: "node.alpha".to_owned(),
                target: "node.beta".to_owned(),
                kind: EdgeKind::Receives,
                line: None,
            },
        ];

        let (edges, abstentions) = canonical_relation_edges(&raw_edges, &symbols);

        assert_eq!(edges.len(), 2);
        assert_eq!(edges[0].from_occurrence.as_str(), "sym.alpha");
        assert_eq!(edges[0].to_occurrence.as_str(), "sym.middle");
        assert_eq!(edges[0].kind, RelationEdgeKindV1::Uses);
        assert_eq!(edges[0].authority, EdgeAuthorityV1::SyntaxExact);
        assert_eq!(
            edges[0].evidence_span,
            SourceSpan {
                start_byte: 4,
                end_byte: 9,
            }
        );
        assert_eq!(edges[1].from_occurrence.as_str(), "sym.zeta");
        assert_eq!(edges[1].to_occurrence.as_str(), "sym.alpha");
        assert_eq!(edges[1].kind, RelationEdgeKindV1::Calls);

        assert_eq!(abstentions.len(), 2);
        let missing_endpoint = abstentions
            .iter()
            .find(|abstention| abstention.target_node_id == "node.missing")
            .expect("missing endpoint abstention");
        assert!(matches!(
            &missing_endpoint.reason,
            CodeIndexEdgeAbstentionReasonV1::MissingSymbolEndpoint
        ));
        assert_eq!(missing_endpoint.legacy_kind, EdgeKind::Calls.as_str());
        let unsupported_kind = abstentions
            .iter()
            .find(|abstention| abstention.legacy_kind == EdgeKind::Receives.as_str())
            .expect("unsupported kind abstention");
        assert!(matches!(
            &unsupported_kind.reason,
            CodeIndexEdgeAbstentionReasonV1::UnsupportedRelationKind
        ));
    }

    #[test]
    fn rematerialization_rebinds_whole_symbol_term_authority() {
        let prior = chunk_source("pub fn real_symbol() {}\n");
        let prior_chunk = prior
            .chunks
            .iter()
            .find(|chunk| {
                chunk.anchor.grain == CodeSearchChunkGrainV1::SymbolBody
                    && chunk.sanitized_text.as_str().contains("real_symbol")
            })
            .expect("symbol body chunk");
        let prior_occurrence = prior_chunk
            .anchor
            .symbol_occurrence_id
            .as_ref()
            .expect("prior symbol occurrence")
            .clone();
        assert!(
            prior_chunk
                .exact_terms
                .iter()
                .any(|term| term.kind() == ExactTechnicalTermKindV1::WholeSymbol)
        );

        let current = prior
            .rematerialize_for_generation(
                id::<CodeGenerationId>("generation.carried"),
                id::<FileOccurrenceId>("file.carried"),
            )
            .expect("rematerialized chunks");
        let current_chunk = current
            .chunks
            .iter()
            .find(|chunk| chunk.id == prior_chunk.id)
            .expect("carried chunk");
        let current_occurrence = current_chunk
            .anchor
            .symbol_occurrence_id
            .as_ref()
            .expect("current symbol occurrence");

        assert_ne!(current_occurrence, &prior_occurrence);
        assert!(
            current_chunk
                .exact_terms
                .iter()
                .filter(|term| term.kind() == ExactTechnicalTermKindV1::WholeSymbol)
                .all(|term| term.symbol_occurrence_id() == Some(current_occurrence))
        );
        current.validate().expect("rematerialized chunks validate");
    }

    #[test]
    fn extraction_authority_rejects_matching_occurrence_forgery() {
        let source = "pub fn real_symbol() {\n    // comment_fake\n}\n";
        let file = validated_file("src/lib.rs", source.as_bytes());
        let batch = batch_for(&file, ParseOutcomeV1::Complete);
        let (result, authority) = chunker()
            .chunk_file_with_authority(&file, &batch, &rust_descriptor(), &NeverCancelled)
            .expect("parser-backed chunks");
        let mut chunk = result
            .chunks
            .into_iter()
            .find(|chunk| {
                chunk.anchor.grain == CodeSearchChunkGrainV1::SymbolBody
                    && chunk.sanitized_text.as_str().contains("comment_fake")
            })
            .expect("symbol body chunk");
        authority
            .admit(chunk.clone())
            .expect("unchanged parser output is admitted");

        let relative = chunk
            .sanitized_text
            .as_str()
            .find("comment_fake")
            .expect("forged name bytes");
        let span = SourceSpan {
            start_byte: chunk.anchor.source_span.start_byte + relative as u64,
            end_byte: chunk.anchor.source_span.start_byte
                + (relative + "comment_fake".len()) as u64,
        };
        chunk.exact_terms.push(
            ExactTechnicalTermV1::untrusted_whole_symbol_candidate(
                b"comment_fake".to_vec(),
                span,
                chunk
                    .anchor
                    .symbol_occurrence_id
                    .clone()
                    .expect("symbol occurrence"),
            )
            .expect("raw matching-occurrence candidate"),
        );
        chunk.exact_terms.sort_by_key(|term| {
            (
                term.span().start_byte,
                term.span().end_byte,
                term.kind(),
                term.canonical_bytes().to_vec(),
                term.original_bytes().to_vec(),
            )
        });
        chunk
            .validate()
            .expect("raw structural validation cannot establish parser authority");
        assert!(
            authority.admit(chunk).is_err(),
            "opaque authority must reject modified exact evidence"
        );
    }

    #[test]
    fn grammar_revision_in_descriptor_must_match_the_batch() {
        let file = validated_file("src/lib.rs", RUST_SOURCE.as_bytes());
        let mut batch = batch_for(&file, ParseOutcomeV1::Complete);
        batch.grammar_revision = GrammarRevision::new("grammar.other.v1").expect("valid id");
        assert_eq!(
            chunker().chunk_file(&file, &batch, &rust_descriptor(), &NeverCancelled),
            Err(ChunkingFailureV1::DescriptorMismatch)
        );
    }
}
