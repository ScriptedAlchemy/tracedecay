use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use serde::{Deserialize, Serialize};
use tracedecay_code_extraction::ExtractedSchemaEvidenceV1;
use tracedecay_domain::{
    BoundedSanitizedText, ChunkerRevision, CodeSearchChunkAnchorV1, CodeSearchChunkGrainV1,
    CodeSearchChunkId, CodeSearchChunkV1, ContentDigest, ExactTechnicalTermKindV1,
    ExactTechnicalTermV1, LanguageDescriptorRevision, SensitivityDecision, SourceSpan,
};

use crate::chunks::{
    CodeFileChunksV1, CodeIndexUnresolvedReferenceV1, CodeSearchDocumentV1, CodeSearchEligibilityV1,
};
use crate::extract::ExtractionBatchV1;
use crate::intake::content_digest;
use crate::lineage::LineageSymbolRecordV1;
use crate::parallelism;

use super::clone_rows::{PersistedCloneBodiesRefV1, PersistedCloneBodiesV1};
use super::*;

/// The partitioned generation manifest revision, which the daemon publishes.
///
/// File segments are compact (DEFLATE-compressed, clone token streams
/// interned, chunk text stored once per file, symbol identities inside the
/// segment), each described by its decoded size and an identity digest. They
/// live in the project's `code-index-v1/`, shared by every worktree scope, and
/// carry no scope identity. File occurrences are minted without the worktree,
/// so identical trees in linked worktrees derive identical artifacts. Each
/// segment descriptor holds generation-independent `symbol_identities`, and
/// restore rebinds occurrences, so a one-file seal does not SHA-256-rebind
/// every unchanged file's symbols. `full_replay_digest` is sealed as a
/// parent delta (optional parent binding). Evidence lineage, request, and
/// receipt rows leave implicit what the generation's own symbols and chunks
/// imply.
///
/// Every other revision is refused through
/// [`superseded_sealed_generation_revision`], and the generation is rebuilt
/// from source rather than migrated. Revisions through eight also predate
/// required clone-body source rows, so the rebuild keeps them from reading as
/// successful empty clone evidence.
pub const SEALED_GENERATION_FORMAT_REVISION_V1: u32 = 15;

/// The typed refusal for a sealed generation this build no longer reads.
pub fn superseded_sealed_generation_revision(revision: u32) -> CodeIndexProductionErrorV1 {
    CodeIndexProductionErrorV1::SupersededSealedGenerationRevision(revision)
}

/// The largest sealed generation file readers admit. Two GiB admits real
/// large-repository generations while keeping decode memory bounded, and the
/// graph write batch bound is sized to cover it.
pub const MAX_SEALED_CODE_GENERATION_BYTES_V1: u64 = 2 * 1024 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PersistedFileGenerationArtifactsV1 {
    pub(super) authority: ReceiptBoundCodeFileAuthorityV1,
    pub(super) extraction: ExtractionBatchV1,
    pub(super) artifacts: CodeFileIndexArtifactsV1,
}

/// The revision-2 file segment payload: the same file record with its chunk
/// rows reduced to what the file does not already say.
///
/// Every chunk of one file shares the file's generation and occurrence
/// (enforced by [`CodeFileChunksV1::validate`]) and, in production, its
/// descriptor, chunker, sanitizer, and sensitivity decision. The row form
/// therefore drops the two anchors, hoists the per-file constants into
/// `chunk_defaults` (a row still carries its own value when it differs, so
/// the form is lossless), names a same-file parent by chunk index, and omits
/// the document's chunk-id roster, which is exactly the rows' ids in order.
/// Decoding expands back into [`PersistedFileGenerationArtifactsV1`], and
/// every restored row then passes the same chunk validation as a revision-1
/// row before it can be served.
///
/// Every chunk's text is a span of the file's one sanitized source, and a
/// chunk's exact terms are spans of its text, so the file stores the source
/// its chunks cover once ([`ChunkTextBaseV1`]) and rows keep only spans. A
/// chunk's content digest is the digest of that text and is recomputed. Clone
/// bodies use the row form in [`super::clone_rows`].
#[derive(Serialize)]
pub(super) struct PersistedFileGenerationArtifactsRefV2<'a> {
    authority: PersistedFileAuthorityRefV1<'a>,
    extraction: &'a ExtractionBatchV1,
    artifacts: PersistedFileIndexArtifactsRefV2<'a>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PersistedFileGenerationArtifactsV2 {
    authority: PersistedFileAuthorityV1,
    extraction: ExtractionBatchV1,
    artifacts: PersistedFileIndexArtifactsV2,
}

/// The identity every file authority of one generation repeats, which
/// validation requires to equal the generation's manifest and snapshot.
/// Segments leave it to the generation that addresses them, so worktrees of
/// one project that seal the same file share that file's segment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct FileScopeIdentityV1 {
    project_id: ProjectId,
    repository_id: RepositoryId,
    worktree_id: Option<WorktreeId>,
    reference: Option<RefId>,
}

impl FileScopeIdentityV1 {
    pub(super) fn of(
        manifest: &CodeGenerationManifestV1,
        snapshot: &SanitizedCodeSnapshotV1,
    ) -> Self {
        Self {
            project_id: manifest.project_id.clone(),
            repository_id: snapshot.repository.clone(),
            worktree_id: snapshot.worktree.clone(),
            reference: snapshot.reference.clone(),
        }
    }

    pub(super) fn retained_bytes(&self) -> usize {
        self.project_id
            .as_str()
            .len()
            .saturating_add(self.repository_id.as_str().len())
            .saturating_add(self.worktree_id.as_ref().map_or(0, |id| id.as_str().len()))
            .saturating_add(self.reference.as_ref().map_or(0, |id| id.as_str().len()))
    }
}

#[derive(Serialize)]
struct PersistedFileAuthorityRefV1<'a> {
    logical_path: &'a str,
    content_digest: &'a ContentDigest,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedFileAuthorityV1 {
    logical_path: String,
    content_digest: ContentDigest,
}

#[derive(Serialize)]
struct PersistedFileIndexArtifactsRefV2<'a> {
    chunks: PersistedFileChunksRefV2<'a>,
    symbols: &'a [Arc<LineageSymbolRecordV1>],
    edges: &'a [CanonicalRelationEdgeV1],
    edge_abstentions: &'a [CodeIndexEdgeAbstentionV1],
    imports: &'a [CodeIndexImportEvidenceV1],
    clone_bodies: PersistedCloneBodiesRefV1<'a>,
    #[serde(skip_serializing_if = "Option::is_none")]
    schema_evidence: Option<&'a ExtractedSchemaEvidenceV1>,
    unresolved_references: &'a [CodeIndexUnresolvedReferenceV1],
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedFileIndexArtifactsV2 {
    chunks: PersistedFileChunksV2,
    symbols: Vec<Arc<LineageSymbolRecordV1>>,
    edges: Vec<CanonicalRelationEdgeV1>,
    edge_abstentions: Vec<CodeIndexEdgeAbstentionV1>,
    imports: Vec<CodeIndexImportEvidenceV1>,
    clone_bodies: PersistedCloneBodiesV1,
    schema_evidence: Option<ExtractedSchemaEvidenceV1>,
    unresolved_references: Vec<CodeIndexUnresolvedReferenceV1>,
}

#[derive(Serialize)]
struct PersistedFileChunksRefV2<'a> {
    generation_id: &'a CodeGenerationId,
    file_occurrence_id: &'a FileOccurrenceId,
    content_digest: &'a ContentDigest,
    eligibility: &'a CodeSearchEligibilityV1,
    #[serde(skip_serializing_if = "Option::is_none")]
    chunk_defaults: Option<PersistedChunkDefaultsRefV2<'a>>,
    #[serde(flatten)]
    text: ChunkTextBaseV1,
    chunks: Vec<PersistedChunkRefV2<'a>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedFileChunksV2 {
    generation_id: CodeGenerationId,
    file_occurrence_id: FileOccurrenceId,
    content_digest: ContentDigest,
    eligibility: CodeSearchEligibilityV1,
    #[serde(default)]
    chunk_defaults: Option<PersistedChunkDefaultsV2>,
    text_ranges: Vec<[u64; 2]>,
    text: String,
    chunks: Vec<PersistedChunkV2>,
}

/// The maximal source ranges a file's chunks cover, and the sanitized text of
/// those ranges concatenated in order. Overlapping chunks (a body, its
/// members, its signature) therefore share one stored copy of their bytes.
#[derive(Serialize)]
struct ChunkTextBaseV1 {
    text_ranges: Vec<[u64; 2]>,
    text: String,
}

impl ChunkTextBaseV1 {
    /// The base for `rows`, and per row whether its text is the base's bytes
    /// at its span. A row whose text is not keeps it explicitly, so the form
    /// stays lossless.
    fn build(rows: &[Arc<CodeSearchChunkV1>]) -> (Self, Vec<bool>) {
        let mut order = (0..rows.len()).collect::<Vec<_>>();
        order.sort_by_key(|&index| {
            let span = rows[index].anchor.source_span;
            (span.start_byte, span.end_byte)
        });
        let mut base = Self {
            text_ranges: Vec::new(),
            text: String::new(),
        };
        let mut derived = vec![false; rows.len()];
        for index in order {
            let chunk = &rows[index];
            derived[index] = base.admit(chunk.anchor.source_span, chunk.sanitized_text.as_str());
        }
        (base, derived)
    }

    /// Extend the base with `text` at `span`, or report that the bytes it
    /// already holds there disagree. Spans arrive in ascending start order.
    fn admit(&mut self, span: SourceSpan, text: &str) -> bool {
        let (Ok(start), Ok(end)) = (
            usize::try_from(span.start_byte),
            usize::try_from(span.end_byte),
        ) else {
            return false;
        };
        if end.checked_sub(start) != Some(text.len()) {
            return false;
        }
        let last = self
            .text_ranges
            .last()
            .copied()
            .and_then(|[range_start, range_end]| {
                Some((
                    usize::try_from(range_start).ok()?,
                    usize::try_from(range_end).ok()?,
                ))
            });
        let Some((range_start, range_end)) = last.filter(|(_, range_end)| start <= *range_end)
        else {
            self.text_ranges.push([span.start_byte, span.end_byte]);
            self.text.push_str(text);
            return true;
        };
        let Some(offset) = start.checked_sub(range_start) else {
            return false;
        };
        let covered = range_end.min(end) - start;
        let base_start = self.text.len() - (range_end - range_start) + offset;
        if self.text.as_bytes().get(base_start..base_start + covered)
            != text.as_bytes().get(..covered)
        {
            return false;
        }
        if end > range_end {
            let Some(tail) = text.get(covered..) else {
                return false;
            };
            self.text.push_str(tail);
            if let Some(range) = self.text_ranges.last_mut() {
                range[1] = span.end_byte;
            }
        }
        true
    }
}

/// Restores chunk text from a decoded [`ChunkTextBaseV1`].
struct ChunkTextSlicesV1<'a> {
    ranges: &'a [[u64; 2]],
    offsets: Vec<u64>,
    text: &'a str,
}

impl<'a> ChunkTextSlicesV1<'a> {
    fn new(ranges: &'a [[u64; 2]], text: &'a str) -> Result<Self, CodeIndexProductionErrorV1> {
        let mut offsets = Vec::with_capacity(ranges.len());
        let mut total = 0_u64;
        let mut previous_end = None;
        for [start, end] in ranges {
            if end <= start || previous_end.is_some_and(|previous| previous > *start) {
                return Err(CodeIndexProductionErrorV1::Contract(
                    "sealed file segment text ranges are not ascending and disjoint".to_owned(),
                ));
            }
            offsets.push(total);
            total = total.checked_add(end - start).ok_or_else(|| {
                CodeIndexProductionErrorV1::Contract(
                    "sealed file segment text length exceeds u64".to_owned(),
                )
            })?;
            previous_end = Some(*end);
        }
        if u64::try_from(text.len()).ok() != Some(total) {
            return Err(CodeIndexProductionErrorV1::Contract(
                "sealed file segment text does not match its ranges".to_owned(),
            ));
        }
        Ok(Self {
            ranges,
            offsets,
            text,
        })
    }

    fn slice(&self, span: SourceSpan) -> Result<&'a str, CodeIndexProductionErrorV1> {
        let text = self.text;
        self.ranges
            .partition_point(|range| range[0] <= span.start_byte)
            .checked_sub(1)
            .filter(|index| span.end_byte <= self.ranges[*index][1])
            .and_then(|index| {
                let from =
                    self.offsets[index].checked_add(span.start_byte - self.ranges[index][0])?;
                let to = from.checked_add(span.end_byte.checked_sub(span.start_byte)?)?;
                text.get(usize::try_from(from).ok()?..usize::try_from(to).ok()?)
            })
            .ok_or_else(|| {
                CodeIndexProductionErrorV1::Contract(
                    "sealed file segment chunk span is outside its stored text".to_owned(),
                )
            })
    }
}

/// An exact term row: its bytes are the chunk text at `span`.
#[derive(Serialize)]
struct PersistedExactTermRefV1<'a> {
    kind: ExactTechnicalTermKindV1,
    span: SourceSpan,
    #[serde(skip_serializing_if = "Option::is_none")]
    symbol_occurrence_id: Option<&'a SymbolOccurrenceId>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedExactTermV1 {
    kind: ExactTechnicalTermKindV1,
    span: SourceSpan,
    #[serde(default)]
    symbol_occurrence_id: Option<SymbolOccurrenceId>,
}

impl<'a> PersistedExactTermRefV1<'a> {
    fn new(
        chunk: &CodeSearchChunkV1,
        term: &'a ExactTechnicalTermV1,
    ) -> Result<Self, CodeIndexProductionErrorV1> {
        if term_bytes(
            chunk.sanitized_text.as_str(),
            chunk.anchor.source_span,
            term.span(),
        ) != Some(term.original_bytes())
        {
            return Err(CodeIndexProductionErrorV1::Contract(
                "sealed chunk exact term is not its chunk text at its span".to_owned(),
            ));
        }
        Ok(Self {
            kind: term.kind(),
            span: term.span(),
            symbol_occurrence_id: term.symbol_occurrence_id(),
        })
    }
}

fn term_bytes(text: &str, chunk_span: SourceSpan, term_span: SourceSpan) -> Option<&[u8]> {
    let from = usize::try_from(term_span.start_byte.checked_sub(chunk_span.start_byte)?).ok()?;
    let to = usize::try_from(term_span.end_byte.checked_sub(chunk_span.start_byte)?).ok()?;
    text.as_bytes().get(from..to)
}

#[derive(Serialize)]
struct PersistedChunkDefaultsRefV2<'a> {
    language_descriptor_revision: &'a LanguageDescriptorRevision,
    chunker_revision: &'a ChunkerRevision,
    sanitizer_revision: &'a SanitizerRevision,
    sensitivity: &'a SensitivityDecision,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedChunkDefaultsV2 {
    language_descriptor_revision: LanguageDescriptorRevision,
    chunker_revision: ChunkerRevision,
    sanitizer_revision: SanitizerRevision,
    sensitivity: SensitivityDecision,
}

#[derive(Serialize)]
struct PersistedChunkRefV2<'a> {
    id: &'a CodeSearchChunkId,
    #[serde(skip_serializing_if = "Option::is_none")]
    symbol_occurrence_id: Option<&'a SymbolOccurrenceId>,
    /// Index of the parent row within this file; a parent outside the file
    /// (never produced by the chunker, but representable) stays explicit.
    #[serde(skip_serializing_if = "Option::is_none")]
    parent: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    parent_chunk_id: Option<&'a CodeSearchChunkId>,
    source_span: SourceSpan,
    grain: CodeSearchChunkGrainV1,
    ordinal: u32,
    /// Present only when it is not the digest of the chunk's text.
    #[serde(skip_serializing_if = "Option::is_none")]
    content_digest: Option<&'a ContentDigest>,
    #[serde(skip_serializing_if = "Option::is_none")]
    language_descriptor_revision: Option<&'a LanguageDescriptorRevision>,
    #[serde(skip_serializing_if = "Option::is_none")]
    chunker_revision: Option<&'a ChunkerRevision>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sanitizer_revision: Option<&'a SanitizerRevision>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sensitivity: Option<&'a SensitivityDecision>,
    exact_terms: Vec<PersistedExactTermRefV1<'a>>,
    subtokens: &'a [String],
    /// Present only when the file's text base does not hold it at its span.
    #[serde(skip_serializing_if = "Option::is_none")]
    sanitized_text: Option<&'a BoundedSanitizedText>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedChunkV2 {
    id: CodeSearchChunkId,
    #[serde(default)]
    symbol_occurrence_id: Option<SymbolOccurrenceId>,
    #[serde(default)]
    parent: Option<u32>,
    #[serde(default)]
    parent_chunk_id: Option<CodeSearchChunkId>,
    source_span: SourceSpan,
    grain: CodeSearchChunkGrainV1,
    ordinal: u32,
    #[serde(default)]
    content_digest: Option<ContentDigest>,
    #[serde(default)]
    language_descriptor_revision: Option<LanguageDescriptorRevision>,
    #[serde(default)]
    chunker_revision: Option<ChunkerRevision>,
    #[serde(default)]
    sanitizer_revision: Option<SanitizerRevision>,
    #[serde(default)]
    sensitivity: Option<SensitivityDecision>,
    exact_terms: Vec<PersistedExactTermV1>,
    subtokens: Vec<String>,
    #[serde(default)]
    sanitized_text: Option<BoundedSanitizedText>,
}

impl<'a> PersistedFileGenerationArtifactsRefV2<'a> {
    /// Refuses a file whose authority names another scope than its
    /// generation, rather than persisting a row that restores differently.
    pub(super) fn new(
        scope: &FileScopeIdentityV1,
        authority: &'a ReceiptBoundCodeFileAuthorityV1,
        extraction: &'a ExtractionBatchV1,
        artifacts: &'a CodeFileIndexArtifactsV1,
    ) -> Result<Self, CodeIndexProductionErrorV1> {
        if authority.project_id != scope.project_id
            || authority.repository_id != scope.repository_id
            || authority.worktree_id != scope.worktree_id
            || authority.reference != scope.reference
        {
            return Err(CodeIndexProductionErrorV1::Contract(
                "sealed file authority names another scope than its generation".to_owned(),
            ));
        }
        let rows = &artifacts.chunks.chunks;
        let (text, derived_text) = ChunkTextBaseV1::build(rows);
        let defaults = rows.first().map(|first| PersistedChunkDefaultsRefV2 {
            language_descriptor_revision: &first.language_descriptor_revision,
            chunker_revision: &first.chunker_revision,
            sanitizer_revision: &first.sanitizer_revision,
            sensitivity: &first.sensitivity,
        });
        let row_index = rows
            .iter()
            .enumerate()
            .map(|(index, chunk)| (&chunk.id, index))
            .collect::<HashMap<_, _>>();
        let chunks = rows
            .iter()
            .zip(derived_text)
            .map(|(chunk, derived_text)| {
                let parent = chunk
                    .anchor
                    .parent_chunk_id
                    .as_ref()
                    .and_then(|parent| row_index.get(parent))
                    .and_then(|index| u32::try_from(*index).ok());
                Ok(PersistedChunkRefV2 {
                    id: &chunk.id,
                    symbol_occurrence_id: chunk.anchor.symbol_occurrence_id.as_ref(),
                    parent,
                    parent_chunk_id: if parent.is_none() {
                        chunk.anchor.parent_chunk_id.as_ref()
                    } else {
                        None
                    },
                    source_span: chunk.anchor.source_span,
                    grain: chunk.anchor.grain,
                    ordinal: chunk.anchor.ordinal,
                    content_digest: (chunk.content_digest
                        != content_digest(chunk.sanitized_text.as_str().as_bytes()))
                    .then_some(&chunk.content_digest),
                    language_descriptor_revision: own_unless_default(
                        &chunk.language_descriptor_revision,
                        defaults
                            .as_ref()
                            .map(|defaults| defaults.language_descriptor_revision),
                    ),
                    chunker_revision: own_unless_default(
                        &chunk.chunker_revision,
                        defaults.as_ref().map(|defaults| defaults.chunker_revision),
                    ),
                    sanitizer_revision: own_unless_default(
                        &chunk.sanitizer_revision,
                        defaults
                            .as_ref()
                            .map(|defaults| defaults.sanitizer_revision),
                    ),
                    sensitivity: own_unless_default(
                        &chunk.sensitivity,
                        defaults.as_ref().map(|defaults| defaults.sensitivity),
                    ),
                    exact_terms: chunk
                        .exact_terms
                        .iter()
                        .map(|term| PersistedExactTermRefV1::new(chunk, term))
                        .collect::<Result<_, _>>()?,
                    subtokens: &chunk.subtokens,
                    sanitized_text: (!derived_text).then_some(&chunk.sanitized_text),
                })
            })
            .collect::<Result<_, CodeIndexProductionErrorV1>>()?;
        Ok(Self {
            authority: PersistedFileAuthorityRefV1 {
                logical_path: &authority.logical_path,
                content_digest: &authority.content_digest,
            },
            extraction,
            artifacts: PersistedFileIndexArtifactsRefV2 {
                chunks: PersistedFileChunksRefV2 {
                    generation_id: &artifacts.chunks.document.generation_id,
                    file_occurrence_id: &artifacts.chunks.document.file_occurrence_id,
                    content_digest: &artifacts.chunks.document.content_digest,
                    eligibility: &artifacts.chunks.document.eligibility,
                    chunk_defaults: defaults,
                    text,
                    chunks,
                },
                symbols: &artifacts.symbols,
                edges: &artifacts.edges,
                edge_abstentions: &artifacts.edge_abstentions,
                imports: &artifacts.imports,
                clone_bodies: PersistedCloneBodiesRefV1::new(
                    authority,
                    extraction,
                    &clone_bodies_by_symbol_identity(artifacts),
                )?,
                schema_evidence: artifacts.schema_evidence.as_ref(),
                unresolved_references: &artifacts.unresolved_references,
            },
        })
    }
}

/// Clone bodies in memory sort by symbol occurrence, which hashes the
/// worktree's file occurrence; persisting them by symbol identity instead
/// lets identical files in linked worktrees seal to one segment. Restore
/// re-sorts by occurrence.
fn clone_bodies_by_symbol_identity(
    artifacts: &CodeFileIndexArtifactsV1,
) -> Vec<&CodeIndexCloneBodyV1> {
    let identities = artifacts
        .symbols
        .iter()
        .map(|symbol| (&symbol.occurrence, &symbol.identity))
        .collect::<HashMap<_, _>>();
    let mut bodies = artifacts.clone_bodies.iter().collect::<Vec<_>>();
    bodies.sort_by(|left, right| {
        let (left, right) = (
            &left.occurrence.symbol_occurrence_id,
            &right.occurrence.symbol_occurrence_id,
        );
        identities
            .get(left)
            .cmp(&identities.get(right))
            .then_with(|| left.cmp(right))
    });
    bodies
}

/// A row carries its own value only where it differs from the file default.
fn own_unless_default<'a, T: PartialEq>(value: &'a T, default: Option<&'a T>) -> Option<&'a T> {
    (default != Some(value)).then_some(value)
}

impl PersistedFileGenerationArtifactsV2 {
    /// Expand the row form back into the full file record. Rows are rebuilt
    /// exactly as the chunker emitted them; the caller's chunk validation
    /// then decides whether the expanded file is admissible.
    pub(super) fn expand(
        self,
        scope: &FileScopeIdentityV1,
    ) -> Result<PersistedFileGenerationArtifactsV1, CodeIndexProductionErrorV1> {
        let authority = ReceiptBoundCodeFileAuthorityV1 {
            project_id: scope.project_id.clone(),
            repository_id: scope.repository_id.clone(),
            worktree_id: scope.worktree_id.clone(),
            reference: scope.reference.clone(),
            logical_path: self.authority.logical_path,
            content_digest: self.authority.content_digest,
        };
        let artifacts = self.artifacts;
        let clone_bodies = artifacts
            .clone_bodies
            .expand(&authority, &self.extraction)?;
        let file = artifacts.chunks;
        let defaults = file.chunk_defaults;
        let text = ChunkTextSlicesV1::new(&file.text_ranges, &file.text)?;
        let ids = file
            .chunks
            .iter()
            .map(|chunk| chunk.id.clone())
            .collect::<Vec<_>>();
        let missing_default = |field: &str| {
            CodeIndexProductionErrorV1::Contract(format!(
                "sealed file segment chunk omits {field} without a file default"
            ))
        };
        let mut chunks = Vec::with_capacity(file.chunks.len());
        for chunk in file.chunks {
            let parent_chunk_id = match (chunk.parent, chunk.parent_chunk_id) {
                (Some(index), None) => Some(ids.get(index as usize).cloned().ok_or_else(|| {
                    CodeIndexProductionErrorV1::Contract(
                        "sealed file segment chunk names a parent row outside its file".to_owned(),
                    )
                })?),
                (None, explicit) => explicit,
                (Some(_), Some(_)) => {
                    return Err(CodeIndexProductionErrorV1::Contract(
                        "sealed file segment chunk names its parent twice".to_owned(),
                    ));
                }
            };
            let sanitized_text = match chunk.sanitized_text {
                Some(explicit) => explicit,
                None => BoundedSanitizedText::new(text.slice(chunk.source_span)?)
                    .map_err(|error| CodeIndexProductionErrorV1::Contract(error.to_string()))?,
            };
            let exact_terms = chunk
                .exact_terms
                .into_iter()
                .map(|term| {
                    let bytes = term_bytes(sanitized_text.as_str(), chunk.source_span, term.span)
                        .ok_or_else(|| {
                        CodeIndexProductionErrorV1::Contract(
                            "sealed chunk exact term span is outside its chunk text".to_owned(),
                        )
                    })?;
                    ExactTechnicalTermV1::from_persisted_parts(
                        term.kind,
                        bytes.to_vec(),
                        term.span,
                        term.symbol_occurrence_id,
                    )
                    .map_err(|error| CodeIndexProductionErrorV1::Contract(error.to_string()))
                })
                .collect::<Result<Vec<_>, _>>()?;
            chunks.push(Arc::new(CodeSearchChunkV1 {
                id: chunk.id,
                anchor: CodeSearchChunkAnchorV1 {
                    generation_id: file.generation_id.clone(),
                    file_occurrence_id: file.file_occurrence_id.clone(),
                    symbol_occurrence_id: chunk.symbol_occurrence_id,
                    parent_chunk_id,
                    source_span: chunk.source_span,
                    grain: chunk.grain,
                    ordinal: chunk.ordinal,
                },
                content_digest: chunk
                    .content_digest
                    .unwrap_or_else(|| content_digest(sanitized_text.as_str().as_bytes())),
                language_descriptor_revision: chunk
                    .language_descriptor_revision
                    .or_else(|| {
                        defaults
                            .as_ref()
                            .map(|defaults| defaults.language_descriptor_revision.clone())
                    })
                    .ok_or_else(|| missing_default("language_descriptor_revision"))?,
                chunker_revision: chunk
                    .chunker_revision
                    .or_else(|| {
                        defaults
                            .as_ref()
                            .map(|defaults| defaults.chunker_revision.clone())
                    })
                    .ok_or_else(|| missing_default("chunker_revision"))?,
                sanitizer_revision: chunk
                    .sanitizer_revision
                    .or_else(|| {
                        defaults
                            .as_ref()
                            .map(|defaults| defaults.sanitizer_revision.clone())
                    })
                    .ok_or_else(|| missing_default("sanitizer_revision"))?,
                sensitivity: chunk
                    .sensitivity
                    .or_else(|| {
                        defaults
                            .as_ref()
                            .map(|defaults| defaults.sensitivity.clone())
                    })
                    .ok_or_else(|| missing_default("sensitivity"))?,
                exact_terms,
                subtokens: chunk.subtokens,
                sanitized_text,
            }));
        }
        Ok(PersistedFileGenerationArtifactsV1 {
            authority,
            extraction: self.extraction,
            artifacts: CodeFileIndexArtifactsV1 {
                chunks: CodeFileChunksV1 {
                    document: CodeSearchDocumentV1 {
                        generation_id: file.generation_id,
                        file_occurrence_id: file.file_occurrence_id,
                        content_digest: file.content_digest,
                        eligibility: file.eligibility,
                        chunk_ids: ids,
                    },
                    chunks,
                },
                symbols: artifacts.symbols,
                edges: artifacts.edges,
                edge_abstentions: artifacts.edge_abstentions,
                imports: artifacts.imports,
                clone_bodies,
                schema_evidence: artifacts.schema_evidence,
                unresolved_references: artifacts.unresolved_references,
            },
        })
    }
}

pub(super) struct StreamingPersistedPublishedGenerationV1 {
    pub(super) manifest: CodeGenerationManifestV1,
    pub(super) snapshot: SanitizedCodeSnapshotV1,
    pub(super) repository_parse_identity: CodeIndexRepositoryParseIdentityV1,
    pub(super) ignored_source_admissions: Vec<CodeIndexIgnoredSourceAdmissionV1>,
    pub(super) ignored_source_admissions_digest: ManifestDigest,
    pub(super) files: Vec<PersistedFileGenerationArtifactsV1>,
    pub(super) lineage: Vec<SymbolLineageCandidateV1>,
    pub(super) coverage: CoverageSummaryV1,
    pub(super) capability: CodeIndexCapabilityManifestV1,
    pub(super) projection_request: ProjectionBatchRequestV1,
    pub(super) projection_receipt: ProjectionBatchReceiptV1,
}

/// Rebuild every file's parser-backed exact authority on the indexing pool,
/// then move each persist page into its published artifact.
///
/// The digest remints are by-ref and independent per file, so the fan-out
/// keeps the sequential failure semantics (lowest-index error) while the
/// pages themselves move, the persist corpus is never copied.
pub(super) fn restore_file_pages(
    pages: Vec<PersistedFileGenerationArtifactsV1>,
) -> Result<Vec<Arc<FileGenerationArtifactsV1>>, CodeIndexProductionErrorV1> {
    let authorities = collect_bounded_ordered(&pages, |page, _worker| {
        hotpath::measure_block!(
            "code_index.sealed_decode.file_page",
            ExactExtractionAuthorityV1::restore(&page.artifacts.chunks)
                .map_err(CodeIndexProductionErrorV1::Chunk)
        )
    })?;
    Ok(pages
        .into_iter()
        .zip(authorities)
        .map(|(page, exact_authority)| {
            Arc::new(FileGenerationArtifactsV1 {
                authority: page.authority,
                extraction: page.extraction,
                artifacts: page.artifacts,
                exact_authority,
            })
        })
        .collect())
}

pub(super) fn assemble_published_generation(
    generation: StreamingPersistedPublishedGenerationV1,
) -> Result<CodeIndexPublishedGenerationV1, CodeIndexProductionErrorV1> {
    let StreamingPersistedPublishedGenerationV1 {
        manifest,
        snapshot,
        repository_parse_identity,
        ignored_source_admissions,
        ignored_source_admissions_digest,
        files,
        lineage,
        coverage,
        capability,
        projection_request,
        projection_receipt,
    } = generation;
    let files = hotpath::measure_block!(
        "code_index.sealed_decode.page_restore",
        restore_file_pages(files)
    )?;
    let (ignored_source_roster, chunks, symbols, imports, edges, edge_abstentions, projection) =
        hotpath::measure_block!("code_index.sealed_decode.authority_restore", {
            let ignored_source_roster =
                hotpath::measure_block!("code_index.sealed_decode.ignored_roster", {
                    IgnoredSourceRosterV1::restore(
                        &snapshot,
                        &repository_parse_identity,
                        ignored_source_admissions,
                        ignored_source_admissions_digest,
                    )
                })?;
            // Persist pages moved into `files` exactly once, and chunk/symbol
            // rows are `Arc`-shared between those pages and the generation
            // aggregates: this flatten clones row pointers and per-file
            // document manifests, never a second owned copy of the corpus.
            let chunk_rows = files
                .iter()
                .map(|file| file.artifacts.chunks.clone())
                .collect::<Vec<_>>();
            let symbol_rows = files
                .iter()
                .flat_map(|file| file.artifacts.symbols.iter().cloned())
                .collect::<Vec<_>>();
            let chunks = hotpath::measure_block!("code_index.sealed_decode.chunk_manifest", {
                // Aggregate validation fans out over chunks too. Keep it on
                // the same admitted pool as file restoration instead of
                // entering Rayon's unrelated global worker pool.
                parallelism::install(|| {
                    GenerationChunkManifestV1::new(manifest.generation_id.clone(), chunk_rows)
                })
            })?
            .map_err(CodeIndexProductionErrorV1::Increment)?;
            let symbols = hotpath::measure_block!("code_index.sealed_decode.symbol_index", {
                GenerationSymbolIndexV1::new(manifest.generation_id.clone(), symbol_rows)
            })
            .map_err(CodeIndexProductionErrorV1::Lineage)?;
            let imports = hotpath::measure_block!("code_index.sealed_decode.import_evidence", {
                derive_import_evidence(&files)
            });
            let (edges, edge_abstentions) =
                hotpath::measure_block!("code_index.sealed_decode.edge_evidence", {
                    collect_edge_evidence(&files)
                })?;
            let projection =
                hotpath::measure_block!("code_index.sealed_decode.projection_handoff", {
                    ProjectionPublicationHandoffV1::restore(projection_request, projection_receipt)
                })
                .map_err(CodeIndexProductionErrorV1::Projection)?;
            Ok::<_, CodeIndexProductionErrorV1>((
                ignored_source_roster,
                chunks,
                symbols,
                imports,
                edges,
                edge_abstentions,
                projection,
            ))
        })?;
    let published = CodeIndexPublishedGenerationV1 {
        statistics: hotpath::measure_block!(
            "code_index.sealed_decode.statistics",
            CodeIndexGenerationStatisticsV1::from_generation_parts(
                &files,
                symbols.symbols.len(),
                edges.len(),
            )
        )?,
        manifest,
        snapshot,
        repository_parse_identity,
        ignored_source_roster,
        files,
        chunks,
        symbols,
        lineage,
        imports,
        edges,
        edge_abstentions,
        clone_payloads_reused: 0,
        clone_payloads_computed: 0,
        clone_stale_invalidations: 0,
        coverage,
        capability,
        projection,
        validated: OnceLock::new(),
        admitted: OnceLock::new(),
        attribution: OnceLock::new(),
        chunk_policy: OnceLock::new(),
        graph_manifest: OnceLock::new(),
    };
    hotpath::measure_block!(
        "code_index.sealed_decode.corpus_validation",
        published.validate_fresh()
    )?;
    Ok(published)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Publishing a sealed generation re-encodes its content as one canonical
    /// graph write batch, with record payloads JSON-escaped into string
    /// properties (at most doubling the bytes). A batch bound below that
    /// expansion turns sealed-admissible generations permanently
    /// unpublishable: every activation retry exhausts the graph write budget.
    #[test]
    fn graph_batch_canonical_bound_covers_sealed_admissible_generations() {
        assert!(
            u64::try_from(tracedecay_graph_db::MAX_GRAPH_BATCH_CANONICAL_BYTES)
                .expect("batch canonical bound fits u64")
                >= MAX_SEALED_CODE_GENERATION_BYTES_V1.saturating_mul(2)
        );
    }

    fn span(start_byte: u64, end_byte: u64) -> SourceSpan {
        SourceSpan {
            start_byte,
            end_byte,
        }
    }

    #[test]
    fn chunk_text_base_stores_overlapping_chunks_once_and_restores_each_span() {
        let source = "fn a() { let x = 1; }\n// gap\nfn b() {}\n";
        let text = |start: u64, end: u64| &source[start as usize..end as usize];
        let mut base = ChunkTextBaseV1 {
            text_ranges: Vec::new(),
            text: String::new(),
        };
        assert_eq!(source.len(), 39);
        let chunks = [
            span(0, 6),
            span(0, 22),
            span(7, 21),
            span(29, 38),
            span(29, 39),
        ];
        for chunk in chunks {
            assert!(base.admit(chunk, text(chunk.start_byte, chunk.end_byte)));
        }
        assert_eq!(base.text_ranges, [[0, 22], [29, 39]]);
        assert_eq!(base.text, format!("{}{}", text(0, 22), text(29, 39)));

        let slices = ChunkTextSlicesV1::new(&base.text_ranges, &base.text).expect("slices");
        for chunk in chunks {
            assert_eq!(
                slices.slice(chunk).expect("slice"),
                text(chunk.start_byte, chunk.end_byte)
            );
        }
        assert!(
            slices.slice(span(20, 31)).is_err(),
            "a span across a gap is refused"
        );
        assert!(
            slices.slice(span(38, 41)).is_err(),
            "a span past the text is refused"
        );

        assert!(
            !base.admit(span(33, 38), "XXXXX"),
            "a chunk whose text disagrees with the stored bytes keeps its own text"
        );
        assert!(
            !base.admit(span(35, 38), "ab"),
            "a text whose length is not its span keeps its own text"
        );
        assert_eq!(
            base.text_ranges,
            [[0, 22], [29, 39]],
            "refused chunks do not extend the base"
        );
    }

    #[test]
    fn chunk_text_slices_refuse_ranges_that_disagree_with_their_text() {
        assert!(ChunkTextSlicesV1::new(&[[0, 4]], "abc").is_err());
        assert!(ChunkTextSlicesV1::new(&[[0, 4], [2, 6]], "abcdefgh").is_err());
        assert!(ChunkTextSlicesV1::new(&[[4, 4]], "").is_err());
    }
}
