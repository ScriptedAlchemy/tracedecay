//! Deterministic chunker port (Plan 25, "Code-search chunk and projection
//! contract"): build the five-grain chunks and their parent/child hierarchy
//! from one extraction batch.
//!
//! Every eligible sanitized byte is covered by a declared chunk or an
//! explicit unsupported/excluded range. Oversized bodies split on
//! deterministic structural boundaries; if none are available, fixed byte
//! windows with pinned size/overlap are used. Extractor enumeration order
//! and mutable line numbers cannot affect `CodeSearchChunkId`.

use thiserror::Error;
use tracedecay_domain::{
    CodeSearchChunkV1, CodeSearchDocumentV1, ExtractionBatchV1, LanguageDescriptorV1,
    ValidatedCodeFileV1,
};

use super::extract::ExtractionCancellation;

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
    /// Build every chunk for one validated file plus its extraction batch,
    /// covering every eligible sanitized byte with a declared chunk or an
    /// explicit unsupported/excluded range.
    fn chunk_file(
        &self,
        file: &ValidatedCodeFileV1,
        batch: &ExtractionBatchV1,
        descriptor: &LanguageDescriptorV1,
        cancellation: &dyn ExtractionCancellation,
    ) -> Result<CodeFileChunksV1, ChunkingFailureV1>;
}

/// The chunks produced for one file: the generation-bound document manifest
/// plus its chunks in deterministic order (Plan 25).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodeFileChunksV1 {
    pub document: CodeSearchDocumentV1,
    pub chunks: Vec<CodeSearchChunkV1>,
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
        for chunk in &self.chunks {
            if chunk.anchor.generation_id != self.document.generation_id
                || chunk.anchor.file_occurrence_id != self.document.file_occurrence_id
            {
                return Err(ChunkingFailureV1::GenerationMismatch);
            }
            chunk
                .validate()
                .map_err(|error| ChunkingFailureV1::NonCanonicalIdentity(error.to_string()))?;
        }
        Ok(())
    }
}

use std::collections::BTreeSet;
use std::fmt::Write as _;

use sha2::{Digest, Sha256};
use tracedecay_domain::{
    BoundedSanitizedText, ChunkLogicalIdentityV1, ChunkerRevision, CodeGenerationId,
    CodeSearchChunkAnchorV1, CodeSearchChunkGrainV1, CodeSearchChunkId, CodeSearchEligibilityV1,
    ContentDigest, ExactTechnicalTermKindV1, ExactTechnicalTermV1, FileIdentityDigest,
    FileOccurrenceId, MAX_CHUNK_TEXT_BYTES, ParseOutcomeV1, PolicyRevisionId, RepositoryId,
    SanitizerRevision, SensitivityDecision, SensitivityLevelV1, SourceSpan, SymbolIdentityDigest,
    SymbolOccurrenceId, canonical_sha256,
};

use crate::types::{Node, NodeKind};

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

/// The deterministic five-grain chunker.
///
/// The frozen `CodeChunker` port receives only the extraction *evidence*
/// batch (`ExtractionBatchV1` carries digests, ranges, and coverage, never
/// rows), so structural symbol spans are re-derived through the established
/// `crate::extraction` parser registry — the same parser acquisition path the
/// extractor adapter uses — rather than a parallel parse. Batch consistency
/// is verified before any structural work: a batch that does not match the
/// descriptor and file content identity is rejected, so the re-derived
/// structure is always the structure the batch attests to.
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
    extractors: crate::extraction::LanguageRegistry,
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
        extractors: crate::extraction::LanguageRegistry,
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
    pub fn with_sensitivity_level(mut self, level: SensitivityLevelV1) -> Self {
        self.sensitivity_level = level;
        self
    }

    /// The generation this chunker is bound to.
    pub fn generation_id(&self) -> &CodeGenerationId {
        &self.generation_id
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

/// Content digest over raw chunk text bytes.
pub fn content_digest(bytes: &[u8]) -> ContentDigest {
    let digest = Sha256::digest(bytes);
    let mut encoded = String::with_capacity("sha256:".len() + 64);
    encoded.push_str("sha256:");
    for byte in digest {
        write!(&mut encoded, "{byte:02x}").expect("writing to a string cannot fail");
    }
    ContentDigest::new(encoded).expect("sha256 hex is a valid content digest")
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
    span: SourceSpan,
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
            let canonical = match kind {
                ExactTechnicalTermKindV1::CliFlag
                | ExactTechnicalTermKindV1::ConfigurationKey
                | ExactTechnicalTermKindV1::ToolName => token.to_lowercase().into_bytes(),
                _ => token.as_bytes().to_vec(),
            };
            if seen_terms.insert((kind, canonical.clone())) {
                terms.push(ExactTechnicalTermV1 {
                    kind,
                    original_bytes: token.as_bytes().to_vec(),
                    canonical_bytes: canonical,
                    span,
                });
            }
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
            ("error:", ExactTechnicalTermKindV1::CompilerErrorText),
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
                let canonical = original.to_vec();
                if seen_terms.insert((kind, canonical.clone())) {
                    terms.push(ExactTechnicalTermV1 {
                        kind,
                        original_bytes: original.to_vec(),
                        canonical_bytes: canonical,
                        span: SourceSpan {
                            start_byte: base_offset + (line_start + start) as u64,
                            end_byte: base_offset + (line_start + end) as u64,
                        },
                    });
                }
            }
        }
        line_start += line.len();
    }
    terms.sort_by(|left, right| {
        (
            left.span.start_byte,
            left.span.end_byte,
            left.kind,
            &left.canonical_bytes,
            &left.original_bytes,
        )
            .cmp(&(
                right.span.start_byte,
                right.span.end_byte,
                right.kind,
                &right.canonical_bytes,
                &right.original_bytes,
            ))
    });
    (terms, subtokens)
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
    if token.starts_with("--")
        && token.len() > 2
        && token[2..]
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic())
    {
        return Some(ExactTechnicalTermKindV1::CliFlag);
    }
    let head_digits = token.len() > 4
        && token[..1]
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_uppercase())
        && token[1..].chars().all(|c| c.is_ascii_digit());
    if head_digits {
        return Some(ExactTechnicalTermKindV1::CompilerErrorCode);
    }
    if token.starts_with("ERR_")
        || (token.starts_with('E')
            && token.len() > 1
            && token[1..]
                .chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_'))
    {
        return Some(ExactTechnicalTermKindV1::RuntimeErrorCode);
    }
    if token.len() >= 7 && token.len() <= 40 && token.chars().all(|c| c.is_ascii_hexdigit()) {
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
    {
        return Some(ExactTechnicalTermKindV1::Path);
    }
    if token.contains('.')
        && !token.contains('/')
        && token.split('.').count() >= 2
        && token.split('.').all(|segment| {
            !segment.is_empty()
                && segment
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        })
    {
        return Some(ExactTechnicalTermKindV1::ConfigurationKey);
    }
    if is_ident(token) {
        return Some(ExactTechnicalTermKindV1::WholeSymbol);
    }
    None
}

/// Split one token into lowercase language-profiled subtokens: path,
/// qualifier, and key separators first, then snake/camel boundaries.
fn split_subtokens(token: &str) -> Vec<String> {
    let mut subtokens = Vec::new();
    for segment in token.split(|c: char| matches!(c, ':' | '.' | '/' | '-')) {
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
        file: &ValidatedCodeFileV1,
        batch: &ExtractionBatchV1,
        descriptor: &LanguageDescriptorV1,
        cancellation: &dyn ExtractionCancellation,
    ) -> Result<CodeFileChunksV1, ChunkingFailureV1> {
        if cancellation.is_cancelled() {
            return Err(ChunkingFailureV1::Cancelled);
        }
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
                return self.chunk_partial(file, batch, descriptor, cancellation, reason.clone());
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
            let result = CodeFileChunksV1 {
                document,
                chunks: Vec::new(),
            };
            result.validate()?;
            return Ok(result);
        }
        self.chunk_partial(file, batch, descriptor, cancellation, String::new())
    }
}

impl DeterministicCodeChunker {
    /// Shared complete/partial chunk production. `partial_reason` is empty
    /// for complete parses.
    fn chunk_partial(
        &self,
        file: &ValidatedCodeFileV1,
        batch: &ExtractionBatchV1,
        descriptor: &LanguageDescriptorV1,
        cancellation: &dyn ExtractionCancellation,
        partial_reason: String,
    ) -> Result<CodeFileChunksV1, ChunkingFailureV1> {
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

        let mut result = extractor.extract(&file.file.logical_path, source);
        result.sanitize();
        if cancellation.is_cancelled() {
            return Err(ChunkingFailureV1::Cancelled);
        }

        let len = source.len() as u64;
        let offsets = line_offsets(source.as_bytes());
        let file_identity = self.file_identity(&file.file.logical_path)?;
        let symbols = self.symbol_rows(
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
            &symbols,
            cancellation,
        )?;

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
        let result = CodeFileChunksV1 { document, chunks };
        result.validate()?;
        Ok(result)
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
            kind: &'static str,
            qualified_name: String,
            span: SourceSpan,
        }

        let mut raw: Vec<Raw> = nodes
            .iter()
            .filter(|node| is_symbol_kind(&node.kind))
            .map(|node| {
                let start = byte_offset(offsets, len, node.start_line, node.start_column);
                let end = byte_offset(offsets, len, node.end_line, node.end_column);
                Raw {
                    kind: node.kind.as_str(),
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
                .then(left.kind.cmp(right.kind))
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
                    node.kind,
                    occurrence_index,
                ),
            )
            .map(|digest| {
                SymbolIdentityDigest::new(digest)
                    .expect("canonical digest is a valid symbol identity digest")
            })?;
            let occurrence = canonical_digest(
                SYMBOL_OCCURRENCE_SEPARATOR,
                &(file_occurrence_id.as_str(), identity.as_str()),
            )
            .and_then(|digest| {
                SymbolOccurrenceId::new(format!("symbol.v1.{digest}"))
                    .map_err(|error| ChunkingFailureV1::NonCanonicalIdentity(error.to_string()))
            })?;
            rows.push(SymbolRow {
                span: node.span,
                parent,
                identity,
                occurrence,
            });
        }
        Ok(rows)
    }

    /// Build, identify, and canonically order every chunk for the file.
    fn build_chunks(
        &self,
        source: &str,
        len: u64,
        batch: &ExtractionBatchV1,
        descriptor: &LanguageDescriptorV1,
        file_identity: &FileIdentityDigest,
        symbols: &[SymbolRow],
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
            let signature = (signature_end > symbol.span.start_byte).then(|| SourceSpan {
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
            let (exact_terms, subtokens) = classify_chunk_text(text, piece.span.start_byte);
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
                    level: self.sensitivity_level,
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

/// End offset of the line containing `start` (exclusive of the newline).
fn offsets_line_end(source: &str, start: u64) -> u64 {
    let rest = &source[start as usize..];
    start
        + rest
            .find('\n')
            .map(|index| index as u64)
            .unwrap_or(rest.len() as u64)
}

/// Emit pinned fallback windows over one unowned gap as `FileWindow` chunks.
///
/// The split path is `[gap ordinal, byte offset within the gap, window size]`:
/// gap-relative, never file-absolute, so pure line shifts outside the gap
/// leave the chunk identity unchanged (content digests still track content),
/// while the gap ordinal keeps two unowned regions from minting the same id.
fn emit_windows(source: &str, start: u64, end: u64, gap_ordinal: u64, pending: &mut Vec<PendingChunk>) {
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
    use super::*;
    use tracedecay_domain::{
        BoundedSanitizedText, ChunkerRevision, CodeGenerationId, CodeSearchChunkAnchorV1,
        CodeSearchChunkGrainV1, CodeSearchChunkId, CodeSearchEligibilityV1, ContentDigest,
        ExtractionCoverageV1, FileOccurrenceId, GrammarRevision, LanguageDescriptorRevision,
        LanguageId, ManifestDigest, ParseOutcomeV1, PolicyRevisionId, SanitizedCodeFileV1,
        SanitizerRevision, SensitivityDecision, SensitivityLevelV1, SnapshotFileDispositionV1,
        SourceSpan,
    };

    use crate::code_index::extract::{ExtractionCancellation, NeverCancelled};
    use crate::code_index::languages::{LanguageRegistry, StaticLanguageRegistry};

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

    const RUST_SOURCE: &str = "//! Module documentation.\n\nuse std::collections::HashMap;\n\n/// Doc comment.\npub fn alpha(x: u32) -> u32 {\n    x + 1\n}\n\npub struct Holder {\n    map: HashMap<u32, u32>,\n}\n\nimpl Holder {\n    pub fn get(&self, key: u32) -> Option<u32> {\n        self.map.get(&key).copied()\n    }\n}\n\n// A trailing free-floating comment.\n";

    fn chunker() -> DeterministicCodeChunker {
        DeterministicCodeChunker::new(
            id("generation.fixture"),
            id("repo.fixture"),
            id("sanitizer.v1"),
            id("policy.v1"),
            id("chunker.v1"),
            crate::extraction::LanguageRegistry::new(),
        )
    }

    fn validated_file(path: &str, bytes: &[u8]) -> ValidatedCodeFileV1 {
        ValidatedCodeFileV1 {
            generation_id: id("generation.fixture"),
            file: SanitizedCodeFileV1 {
                file_occurrence_id: id("file.fixture"),
                logical_path: path.to_owned(),
                language: None,
                content_digest: content_digest(bytes),
                disposition: SnapshotFileDispositionV1::Present,
            },
            snapshot_digest: id::<ManifestDigest>(&digest('c')),
            sanitized_bytes: bytes.to_vec(),
        }
    }

    fn batch_for(file: &ValidatedCodeFileV1, outcome: ParseOutcomeV1) -> ExtractionBatchV1 {
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
            for byte in chunk.anchor.source_span.start_byte as usize
                ..chunk.anchor.source_span.end_byte as usize
            {
                covered[byte] = true;
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
        let signature = result
            .chunks
            .iter()
            .find(|chunk| chunk.anchor.grain == CodeSearchChunkGrainV1::SymbolSignature)
            .expect("signature chunk");
        assert!(signature.sanitized_text.as_str().contains("fn "));
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

    #[test]
    fn oversized_body_splits_on_pinned_fallback_windows() {
        let mut source = String::from("pub fn huge() {\n");
        for index in 0..9000 {
            source.push_str(&format!("    let value_{index} = {index}usize;\n"));
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
        let mut source = String::from("pub struct Big;\n\nimpl Big {\n");
        for index in 0..300 {
            source.push_str(&format!("    pub fn method_{index}() -> usize {{\n"));
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
            "std::collections::HashMap src/main.rs --release tracedecay.data.dir E0308 alpha betaValue",
            0,
        );
        let kinds: BTreeSet<ExactTechnicalTermKindV1> =
            terms.iter().map(|term| term.kind).collect();
        assert!(kinds.contains(&ExactTechnicalTermKindV1::QualifiedName));
        assert!(kinds.contains(&ExactTechnicalTermKindV1::Path));
        assert!(kinds.contains(&ExactTechnicalTermKindV1::CliFlag));
        assert!(kinds.contains(&ExactTechnicalTermKindV1::ConfigurationKey));
        assert!(kinds.contains(&ExactTechnicalTermKindV1::CompilerErrorCode));
        assert!(kinds.contains(&ExactTechnicalTermKindV1::WholeSymbol));

        let error_term = terms
            .iter()
            .find(|term| term.kind == ExactTechnicalTermKindV1::CompilerErrorCode)
            .expect("error code term");
        assert_eq!(error_term.original_bytes, b"E0308");
        let flag = terms
            .iter()
            .find(|term| term.kind == ExactTechnicalTermKindV1::CliFlag)
            .expect("flag term");
        assert_eq!(flag.canonical_bytes, b"--release");

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
            "std::collections::HashMap src/main.rs --release tracedecay.data.dir E0308 alpha betaValue",
            0,
        );
        assert_eq!(terms, again.0);
        assert_eq!(subtokens, again.1);
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
