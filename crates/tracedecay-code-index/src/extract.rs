//! Language extractor port (Plan 25 phase 3): `languages.rs` owns the
//! descriptors; this module owns the extractor contract
//! `LanguageExtractor::extract(&ReceiptBoundCodeFileV1, &LanguageDescriptorV1,
//! &CancellationToken) -> Result<ExtractionBatchV1, ExtractionFailureV1>`.
//!
//! Extraction acquires one tree-sitter parser from the descriptor's pinned
//! grammar; the in-process `ast-grep-core` structural kernel shares that
//! pinned grammar and source generation. Parse errors and unsupported
//! constructs are preserved as evidence; extraction never invents successful
//! structure.

use std::sync::Arc;

use serde::Serialize;
use tracedecay_code_extraction::{ExtractedImportEvidenceV1, ExtractionArtifactV1};
use tracedecay_domain::{
    ExtractionBatchV1, ExtractionCoverageV1, ExtractionFailureV1, LanguageDescriptorV1,
    ManifestDigest, ParseOutcomeV1, SourceSpan, ValidatedCodeFileV1, canonical_sha256,
};

use super::{
    intake::{ReceiptBoundCodeFileAuthorityV1, ReceiptBoundCodeFileV1},
    languages::canonical_language_id,
};
use tracedecay_domain::{Edge, Node, UnresolvedRef, Visibility};

const PARSER_IMPORT_ROWS_DIGEST_SEPARATOR: &str = "tracedecay.code-index-parser-import-rows.v1";

/// Cancellation checkpoint for extraction (the code-index-local spelling of
/// the Plan 25 `CancellationToken`). Application adapts its cancellation
/// token to this port; extraction checks it at deterministic boundaries and
/// never publishes partial extraction or mutation state.
pub trait ExtractionCancellation {
    /// Whether cancellation was requested.
    fn is_cancelled(&self) -> bool;
}

/// A cancellation token that never fires; the default for synchronous
/// indexing drivers and tests.
pub struct NeverCancelled;

impl ExtractionCancellation for NeverCancelled {
    fn is_cancelled(&self) -> bool {
        false
    }
}

/// One canonical extraction batch paired with the sanitized parser rows that
/// produced it. The parser rows remain code-index-local so downstream
/// chunking can reuse them without widening the durable domain contract.
#[derive(Debug)]
pub struct ExtractedCodeFileV1 {
    authority: ReceiptBoundCodeFileAuthorityV1,
    batch: ExtractionBatchV1,
    parse_artifact: ExtractionArtifactV1,
}

impl ExtractedCodeFileV1 {
    pub fn batch(&self) -> &ExtractionBatchV1 {
        &self.batch
    }

    /// Repository-relative logical path that authorized these parser rows.
    pub fn logical_path(&self) -> &str {
        &self.authority.logical_path
    }

    pub fn authority(&self) -> &ReceiptBoundCodeFileAuthorityV1 {
        &self.authority
    }

    pub(crate) fn parse_artifact(&self) -> &ExtractionArtifactV1 {
        &self.parse_artifact
    }

    pub(crate) fn into_parts(
        self,
    ) -> (
        ReceiptBoundCodeFileAuthorityV1,
        ExtractionBatchV1,
        ExtractionArtifactV1,
    ) {
        (self.authority, self.batch, self.parse_artifact)
    }

    /// Rebind carried extraction evidence onto a new generation-scoped file
    /// occurrence without re-parsing.
    ///
    /// The target file must share the original logical path and content digest
    /// that minted `rows_digest`. Identical bytes at a different path cannot
    /// reuse another file's parser rows.
    pub fn rebind_file_occurrence(
        &mut self,
        file: &ReceiptBoundCodeFileV1,
    ) -> Result<(), ExtractionFailureV1> {
        self.batch = rebind_extraction_batch(&self.authority, &self.batch, file)?;
        Ok(())
    }
}

pub(crate) fn rebind_extraction_batch(
    authority: &ReceiptBoundCodeFileAuthorityV1,
    batch: &ExtractionBatchV1,
    file: &ReceiptBoundCodeFileV1,
) -> Result<ExtractionBatchV1, ExtractionFailureV1> {
    let target_authority = file.authority();
    if authority.project_id != target_authority.project_id
        || authority.repository_id != target_authority.repository_id
        || authority.logical_path != target_authority.logical_path
        || authority.content_digest != target_authority.content_digest
        || batch.content_digest != authority.content_digest
    {
        return Err(ExtractionFailureV1::IncompatibleDescriptor {
            detail:
                "extraction authority does not match target project, repository, path, or content"
                    .to_owned(),
        });
    }
    let target = file.validated_file();
    let mut rebound = batch.clone();
    rebound.generation_id = target.generation_id.clone();
    rebound.file_occurrence_id = target.file.file_occurrence_id.clone();
    Ok(rebound)
}

/// The language extractor contract (Plan 25). Language-specific logic stays
/// behind this small interface while identity, lineage, and output contracts
/// are shared.
pub trait LanguageExtractor {
    /// Extract one canonical batch and its parser rows from one receipt-bound
    /// file under one descriptor. Identical input, registry, and extractor
    /// revisions produce stable canonical rows and digests on every supported
    /// host.
    fn extract(
        &self,
        file: &ReceiptBoundCodeFileV1,
        descriptor: &LanguageDescriptorV1,
        cancellation: &dyn ExtractionCancellation,
    ) -> Result<ExtractedCodeFileV1, ExtractionFailureV1>;
}

/// Domain separator for the canonical extraction-rows digest.
pub const EXTRACTION_ROWS_SEPARATOR: &str = "tracedecay.extraction-rows.v2";

/// Pinned maximum source prefix parsed by one extraction operation. Bytes
/// beyond the cap remain explicit unsupported evidence in the batch.
pub const MAX_EXTRACTION_SOURCE_BYTES: usize = 1024 * 1024;

/// The tree-sitter-backed extractor adapter. It reuses the established
/// `tracedecay_code_extraction` parser registry as the sole parser acquisition path
/// (Plan 25: duplicate parser acquisition paths are forbidden) and adapts its
/// rows into the canonical `ExtractionBatchV1` evidence contract.
///
/// Operational timestamps (`Node::updated_at`, `ExtractionResult::
/// duration_ms`) are excluded from the canonical rows digest, and rows are
/// canonically ordered before hashing, so identical sanitized input under
/// identical descriptor revisions produces identical digests.
pub struct TreeSitterExtractor {
    parsers: Arc<tracedecay_code_extraction::LanguageRegistry>,
}

impl TreeSitterExtractor {
    /// Create the adapter over a freshly built extraction registry.
    pub fn new() -> Self {
        Self {
            parsers: Arc::new(tracedecay_code_extraction::LanguageRegistry::new()),
        }
    }

    /// Create the adapter over an existing extraction registry.
    pub fn from_registry(parsers: tracedecay_code_extraction::LanguageRegistry) -> Self {
        Self {
            parsers: Arc::new(parsers),
        }
    }

    /// Share one generation-scoped registry with downstream chunking.
    pub fn from_shared_registry(
        parsers: Arc<tracedecay_code_extraction::LanguageRegistry>,
    ) -> Self {
        Self { parsers }
    }

    /// Resolve the parser for one file, falling back to the descriptor's
    /// declared extensions when the logical path carries no (recognized)
    /// extension.
    pub(crate) fn resolve_parser<'a>(
        &'a self,
        file: &ValidatedCodeFileV1,
        descriptor: &LanguageDescriptorV1,
    ) -> Option<&'a dyn tracedecay_code_extraction::LanguageExtractor> {
        if let Some(extractor) = self.parsers.extractor_for_file(&file.file.logical_path) {
            return Some(extractor);
        }
        descriptor.extensions.iter().find_map(|extension| {
            self.parsers
                .extractor_for_file(&format!("probe.{extension}"))
        })
    }

    pub(crate) fn extract_preparsed(
        &self,
        file: &ReceiptBoundCodeFileV1,
        descriptor: &LanguageDescriptorV1,
        artifact: ExtractionArtifactV1,
        parsed_len: usize,
        cancellation: &dyn ExtractionCancellation,
    ) -> Result<ExtractedCodeFileV1, ExtractionFailureV1> {
        if cancellation.is_cancelled() {
            return Err(ExtractionFailureV1::Cancelled);
        }
        let authority = file.authority().clone();
        let file = file.validated_file();
        validate_descriptor(file, descriptor)?;
        let admitted_prefix = file
            .sanitized_bytes
            .get(..parsed_len)
            .and_then(|bytes| std::str::from_utf8(bytes).ok());
        if admitted_prefix.is_none() {
            return Err(ExtractionFailureV1::ParseFailed {
                detail: "retained parse prefix is not an admitted UTF-8 boundary".to_owned(),
            });
        }
        finish_extraction(
            authority,
            file,
            descriptor,
            artifact,
            parsed_len,
            parsed_len < file.sanitized_bytes.len(),
            cancellation,
        )
    }
}

impl Default for TreeSitterExtractor {
    fn default() -> Self {
        Self::new()
    }
}

// Canonical rows declare fields in byte-wise alphabetical key order so the
// borrowed DTO serializes byte-identically to the legacy serde_json::Value
// object (BTreeMap key order) the pinned rows digest was minted from.
#[derive(Serialize)]
struct CanonicalNodeRow<'a> {
    assertions: u32,
    attrs_start_line: u32,
    branches: u32,
    docstring: Option<&'a str>,
    end_column: u32,
    end_line: u32,
    file_path: &'a str,
    id: &'a str,
    is_async: bool,
    kind: &'a tracedecay_domain::NodeKind,
    loops: u32,
    max_nesting: u32,
    name: &'a str,
    parent_id: Option<&'a str>,
    qualified_name: &'a str,
    returns: u32,
    signature: Option<&'a str>,
    start_column: u32,
    start_line: u32,
    unchecked_calls: u32,
    unsafe_blocks: u32,
    visibility: &'a Visibility,
}

impl<'a> From<&'a Node> for CanonicalNodeRow<'a> {
    fn from(node: &'a Node) -> Self {
        Self {
            assertions: node.assertions,
            attrs_start_line: node.attrs_start_line,
            branches: node.branches,
            docstring: node.docstring.as_deref(),
            end_column: node.end_column,
            end_line: node.end_line,
            file_path: &node.file_path,
            id: &node.id,
            is_async: node.is_async,
            kind: &node.kind,
            loops: node.loops,
            max_nesting: node.max_nesting,
            name: &node.name,
            parent_id: node.parent_id.as_deref(),
            qualified_name: &node.qualified_name,
            returns: node.returns,
            signature: node.signature.as_deref(),
            start_column: node.start_column,
            start_line: node.start_line,
            unchecked_calls: node.unchecked_calls,
            unsafe_blocks: node.unsafe_blocks,
            visibility: &node.visibility,
        }
    }
}

/// Rows are canonically ordered by their serialized canonical form, matching
/// the legacy `sort_canonical_json` byte ordering exactly.
fn sort_canonical_rows<T: Serialize>(rows: &mut [T]) {
    rows.sort_by_cached_key(|row| serde_json::to_string(row).expect("canonical row serializes"));
}

#[derive(Serialize)]
struct CanonicalEdgeRow<'a> {
    kind: tracedecay_domain::EdgeKind,
    line: Option<u32>,
    source: &'a str,
    target: &'a str,
}

impl<'a> From<&'a Edge> for CanonicalEdgeRow<'a> {
    fn from(edge: &'a Edge) -> Self {
        Self {
            kind: edge.kind,
            line: edge.line,
            source: &edge.source,
            target: &edge.target,
        }
    }
}

#[derive(Serialize)]
struct CanonicalUnresolvedRefRow<'a> {
    column: u32,
    file_path: &'a str,
    from_node_id: &'a str,
    line: u32,
    reference_kind: tracedecay_domain::EdgeKind,
    reference_name: &'a str,
}

impl<'a> From<&'a UnresolvedRef> for CanonicalUnresolvedRefRow<'a> {
    fn from(reference: &'a UnresolvedRef) -> Self {
        Self {
            column: reference.column,
            file_path: &reference.file_path,
            from_node_id: &reference.from_node_id,
            line: reference.line,
            reference_kind: reference.reference_kind,
            reference_name: &reference.reference_name,
        }
    }
}

/// Canonical digest of the extraction rows. Operational timestamps are
/// omitted through borrowed DTOs and rows are canonically ordered by their
/// serialized canonical form before one payload serialization. The borrowed
/// form stays byte-identical to the legacy `serde_json::Value` canonicalization
/// the pinned digest identity was minted from.
fn rows_digest(
    file: &ValidatedCodeFileV1,
    descriptor: &LanguageDescriptorV1,
    artifact: &ExtractionArtifactV1,
) -> Result<ManifestDigest, ExtractionFailureV1> {
    let result = &artifact.result;
    let mut nodes = result
        .nodes
        .iter()
        .map(CanonicalNodeRow::from)
        .collect::<Vec<_>>();
    let mut edges = result
        .edges
        .iter()
        .map(CanonicalEdgeRow::from)
        .collect::<Vec<_>>();
    let mut unresolved = result
        .unresolved_refs
        .iter()
        .map(CanonicalUnresolvedRefRow::from)
        .collect::<Vec<_>>();
    let mut imports = artifact.imports.clone();
    sort_canonical_rows(&mut nodes);
    sort_canonical_rows(&mut edges);
    sort_canonical_rows(&mut unresolved);
    imports.sort();

    #[derive(Serialize)]
    struct RowsPayload<'a> {
        separator: &'static str,
        logical_path: &'a str,
        language: &'a str,
        descriptor_revision: &'a str,
        grammar_revision: &'a str,
        extractor_revision: &'a str,
        imports: Vec<ExtractedImportEvidenceV1>,
        nodes: Vec<CanonicalNodeRow<'a>>,
        edges: Vec<CanonicalEdgeRow<'a>>,
        unresolved_refs: Vec<CanonicalUnresolvedRefRow<'a>>,
    }

    canonical_sha256(&RowsPayload {
        separator: EXTRACTION_ROWS_SEPARATOR,
        logical_path: &file.file.logical_path,
        language: descriptor.language.as_str(),
        descriptor_revision: descriptor.descriptor_revision.as_str(),
        grammar_revision: descriptor.grammar_revision.as_str(),
        extractor_revision: descriptor.extractor_revision.as_str(),
        imports,
        nodes,
        edges,
        unresolved_refs: unresolved,
    })
    .map_err(|error| ExtractionFailureV1::ParseFailed {
        detail: format!("canonical rows digest failed: {error}"),
    })
}

pub(crate) fn parser_import_rows_digest(
    imports: &[ExtractedImportEvidenceV1],
) -> Result<ManifestDigest, ExtractionFailureV1> {
    let mut imports = imports.to_vec();
    imports.sort();
    canonical_sha256(&(PARSER_IMPORT_ROWS_DIGEST_SEPARATOR, imports.as_slice())).map_err(|error| {
        ExtractionFailureV1::ParseFailed {
            detail: format!("canonical parser import rows digest failed: {error}"),
        }
    })
}

impl LanguageExtractor for TreeSitterExtractor {
    fn extract(
        &self,
        file: &ReceiptBoundCodeFileV1,
        descriptor: &LanguageDescriptorV1,
        cancellation: &dyn ExtractionCancellation,
    ) -> Result<ExtractedCodeFileV1, ExtractionFailureV1> {
        if cancellation.is_cancelled() {
            return Err(ExtractionFailureV1::Cancelled);
        }
        let authority = file.authority().clone();
        let file = file.validated_file();
        validate_descriptor(file, descriptor)?;

        let parser = self.resolve_parser(file, descriptor).ok_or({
            ExtractionFailureV1::GrammarUnavailable {
                language: descriptor.language.clone(),
            }
        })?;
        if canonical_language_id(parser.language_name()) != descriptor.language.as_str() {
            return Err(ExtractionFailureV1::IncompatibleDescriptor {
                detail: format!(
                    "descriptor {} resolved to a {} parser",
                    descriptor.language,
                    parser.language_name()
                ),
            });
        }

        let source = std::str::from_utf8(&file.sanitized_bytes).map_err(|error| {
            ExtractionFailureV1::ParseFailed {
                detail: format!("sanitized bytes are not valid UTF-8: {error}"),
            }
        })?;
        let mut parsed_len = source.len().min(MAX_EXTRACTION_SOURCE_BYTES);
        while !source.is_char_boundary(parsed_len) {
            parsed_len -= 1;
        }
        let extraction_source = &source[..parsed_len];
        let source_was_capped = parsed_len < source.len();

        let artifact = parser.extract_artifact(&file.file.logical_path, extraction_source);
        finish_extraction(
            authority,
            file,
            descriptor,
            artifact,
            parsed_len,
            source_was_capped,
            cancellation,
        )
    }
}

fn validate_descriptor(
    file: &ValidatedCodeFileV1,
    descriptor: &LanguageDescriptorV1,
) -> Result<(), ExtractionFailureV1> {
    if let Some(declared) = &file.file.language
        && declared != &descriptor.language
    {
        return Err(ExtractionFailureV1::IncompatibleDescriptor {
            detail: format!(
                "file declares language {} but descriptor is {}",
                declared, descriptor.language
            ),
        });
    }
    Ok(())
}

fn finish_extraction(
    authority: ReceiptBoundCodeFileAuthorityV1,
    file: &ValidatedCodeFileV1,
    descriptor: &LanguageDescriptorV1,
    mut artifact: ExtractionArtifactV1,
    parsed_len: usize,
    source_was_capped: bool,
    cancellation: &dyn ExtractionCancellation,
) -> Result<ExtractedCodeFileV1, ExtractionFailureV1> {
    artifact.result.sanitize();
    if cancellation.is_cancelled() {
        return Err(ExtractionFailureV1::Cancelled);
    }

    let result = &artifact.result;

    let parse_outcome = match (source_was_capped, result.errors.first()) {
        (false, None) => ParseOutcomeV1::Complete,
        (true, None) => ParseOutcomeV1::Partial {
            reason: format!(
                "source byte cap {MAX_EXTRACTION_SOURCE_BYTES} reached; remaining bytes unsupported"
            ),
        },
        (was_capped, Some(first)) => {
            let first: String = first.chars().take(200).collect();
            let cap_reason = if was_capped {
                format!(
                    "; source byte cap {MAX_EXTRACTION_SOURCE_BYTES} reached; remaining bytes unsupported"
                )
            } else {
                String::new()
            };
            ParseOutcomeV1::Partial {
                reason: format!(
                    "{} extraction error(s); first: {first}{cap_reason}",
                    result.errors.len()
                ),
            }
        }
    };

    let file_len = file.sanitized_bytes.len() as u64;
    let parsed_ranges = if parsed_len > 0 {
        vec![SourceSpan {
            start_byte: 0,
            end_byte: parsed_len as u64,
        }]
    } else {
        Vec::new()
    };
    let unsupported_ranges = if source_was_capped {
        vec![SourceSpan {
            start_byte: parsed_len as u64,
            end_byte: file_len,
        }]
    } else {
        Vec::new()
    };
    let coverage = ExtractionCoverageV1 {
        parsed_bytes: parsed_len as u64,
        error_bytes: 0,
        unsupported_bytes: file_len - parsed_len as u64,
        symbols_extracted: result.nodes.len() as u64,
        relations_extracted: result.edges.len() as u64,
        ambiguity_count: result.unresolved_refs.len() as u64,
    };
    let rows_digest = rows_digest(file, descriptor, &artifact)?;
    let parser_import_rows_digest = parser_import_rows_digest(&artifact.imports)?;

    Ok(ExtractedCodeFileV1 {
        authority,
        batch: ExtractionBatchV1 {
            generation_id: file.generation_id.clone(),
            file_occurrence_id: file.file.file_occurrence_id.clone(),
            language: descriptor.language.clone(),
            descriptor_revision: descriptor.descriptor_revision.clone(),
            grammar_revision: descriptor.grammar_revision.clone(),
            extractor_revision: descriptor.extractor_revision.clone(),
            content_digest: file.file.content_digest.clone(),
            parse_outcome,
            parsed_ranges,
            error_ranges: Vec::new(),
            unsupported_ranges,
            coverage,
            parser_import_rows_digest,
            rows_digest,
        },
        parse_artifact: artifact,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracedecay_domain::{
        CodeGenerationId, FileOccurrenceId, ProjectId, RepositoryId, SanitizationReceiptId,
        SanitizedCodeFileV1, SanitizedCodeSnapshotV1, SanitizerRevision, SnapshotFileDispositionV1,
        UtcMicros, ValidatedCodeFileV1, WorktreeId,
    };

    use crate::intake::{CodeIndexIntake, SanitizedCodeIntake};
    use crate::languages::{LanguageRegistry, StaticLanguageRegistry};

    struct AlwaysCancelled;

    impl ExtractionCancellation for AlwaysCancelled {
        fn is_cancelled(&self) -> bool {
            true
        }
    }

    fn validated_file(path: &str, bytes: &[u8]) -> ReceiptBoundCodeFileV1 {
        validated_file_in_worktree(path, bytes, None)
    }

    fn validated_file_in_worktree(
        path: &str,
        bytes: &[u8],
        worktree: Option<WorktreeId>,
    ) -> ReceiptBoundCodeFileV1 {
        let file = SanitizedCodeFileV1 {
            file_occurrence_id: FileOccurrenceId::new("file.fixture").expect("valid id"),
            logical_path: path.to_owned(),
            language: Some(tracedecay_domain::LanguageId::new("rust").expect("valid id")),
            content_digest: crate::chunks::content_digest(bytes),
            disposition: SnapshotFileDispositionV1::Present,
        };
        let intake = SanitizedCodeIntake::new(
            StaticLanguageRegistry::new(),
            SanitizerRevision::new("sanitizer.v1").expect("valid id"),
            UtcMicros(1_000_000),
        );
        let capability = intake
            .admit(SanitizedCodeSnapshotV1 {
                repository: RepositoryId::new("repo.fixture").expect("valid id"),
                worktree,
                reference: None,
                source_revision: None,
                sanitizer_revision: SanitizerRevision::new("sanitizer.v1").expect("valid id"),
                sanitization_receipts: vec![
                    SanitizationReceiptId::new("receipt.fixture").expect("valid id"),
                ],
                content_identity: crate::chunks::content_digest(bytes),
                captured_at: UtcMicros(1_000_000),
                files: vec![file.clone()],
            })
            .expect("snapshot capability");
        intake
            .bind_file(
                &capability,
                &ProjectId::new("project.fixture").expect("valid project"),
                ValidatedCodeFileV1 {
                    generation_id: CodeGenerationId::new("generation.fixture").expect("valid id"),
                    file,
                    snapshot_digest: capability.snapshot().intake_digest.clone(),
                    sanitized_bytes: bytes.to_vec(),
                },
            )
            .expect("receipt-bound file")
    }

    fn rust_descriptor() -> LanguageDescriptorV1 {
        StaticLanguageRegistry::new()
            .descriptor(&tracedecay_domain::LanguageId::new("rust").expect("valid id"))
            .expect("rust descriptor")
            .clone()
    }

    const RUST_SOURCE: &str = "use std::collections::HashMap;\n\n/// Doc.\npub fn alpha(x: u32) -> u32 {\n    x + 1\n}\n\nfn beta() {\n    let _ = alpha(1);\n}\n";

    #[test]
    fn extracts_a_complete_batch_with_coverage_evidence() {
        let extractor = TreeSitterExtractor::new();
        let file = validated_file("src/lib.rs", RUST_SOURCE.as_bytes());
        let extraction = extractor
            .extract(&file, &rust_descriptor(), &NeverCancelled)
            .expect("extraction succeeds");
        let batch = extraction.batch();

        assert_eq!(batch.parse_outcome, ParseOutcomeV1::Complete);
        assert_eq!(batch.language.as_str(), "rust");
        assert_eq!(
            batch.content_digest,
            crate::chunks::content_digest(RUST_SOURCE.as_bytes())
        );
        assert_eq!(batch.coverage.parsed_bytes, RUST_SOURCE.len() as u64);
        assert!(batch.coverage.symbols_extracted >= 2);
        assert!(batch.coverage.relations_extracted >= 1);
        assert_eq!(
            batch.parsed_ranges,
            vec![SourceSpan {
                start_byte: 0,
                end_byte: RUST_SOURCE.len() as u64,
            }]
        );
        batch.rows_digest.validate().expect("rows digest canonical");
    }

    #[test]
    fn identical_input_produces_identical_rows_digests() {
        let extractor = TreeSitterExtractor::new();
        let file = validated_file("src/lib.rs", RUST_SOURCE.as_bytes());
        let first = extractor
            .extract(&file, &rust_descriptor(), &NeverCancelled)
            .expect("first extraction");
        let second = extractor
            .extract(&file, &rust_descriptor(), &NeverCancelled)
            .expect("second extraction");
        // Operational timestamps differ between runs; the canonical digest
        // must not.
        assert_eq!(first.batch().rows_digest, second.batch().rows_digest);
        assert_eq!(first.batch(), second.batch());
    }

    #[test]
    fn canonical_rows_digest_matches_pinned_identity() {
        let extractor = TreeSitterExtractor::new();
        let file = validated_file("src/lib.rs", RUST_SOURCE.as_bytes());
        let extraction = extractor
            .extract(&file, &rust_descriptor(), &NeverCancelled)
            .expect("extraction succeeds");

        assert_eq!(
            extraction.batch().rows_digest.as_str(),
            "sha256:62eaaf3e43a4f9773e43c7f2385213ca6aa59bb5a0ee6c07ff44fa7e37beede3"
        );
    }

    #[test]
    fn cancellation_is_checked_at_deterministic_boundaries() {
        let extractor = TreeSitterExtractor::new();
        let file = validated_file("src/lib.rs", RUST_SOURCE.as_bytes());
        assert!(matches!(
            extractor.extract(&file, &rust_descriptor(), &AlwaysCancelled),
            Err(ExtractionFailureV1::Cancelled)
        ));
    }

    #[test]
    fn rebind_rejects_identical_bytes_at_a_different_logical_path() {
        let extractor = TreeSitterExtractor::new();
        let path_a = validated_file("src/a.rs", RUST_SOURCE.as_bytes());
        let mut extraction = extractor
            .extract(&path_a, &rust_descriptor(), &NeverCancelled)
            .expect("extraction for path A");
        let original_digest = extraction.batch().rows_digest.clone();
        let original_occurrence = extraction.batch().file_occurrence_id.clone();
        let path_b = validated_file("src/b.rs", RUST_SOURCE.as_bytes());

        assert!(matches!(
            extraction.rebind_file_occurrence(&path_b),
            Err(ExtractionFailureV1::IncompatibleDescriptor { .. })
        ));
        assert_eq!(extraction.batch().rows_digest, original_digest);
        assert_eq!(extraction.batch().file_occurrence_id, original_occurrence);
        assert_eq!(extraction.logical_path(), "src/a.rs");
    }

    #[test]
    fn rebind_allows_same_path_under_a_new_generation_occurrence() {
        let extractor = TreeSitterExtractor::new();
        let original = validated_file("src/lib.rs", RUST_SOURCE.as_bytes());
        let mut extraction = extractor
            .extract(&original, &rust_descriptor(), &NeverCancelled)
            .expect("extraction");
        let original_digest = extraction.batch().rows_digest.clone();

        let rebound = {
            let intake = SanitizedCodeIntake::new(
                StaticLanguageRegistry::new(),
                SanitizerRevision::new("sanitizer.v1").expect("valid id"),
                UtcMicros(1_000_000),
            );
            let file = SanitizedCodeFileV1 {
                file_occurrence_id: FileOccurrenceId::new("file.carried").expect("valid id"),
                logical_path: "src/lib.rs".to_owned(),
                language: Some(tracedecay_domain::LanguageId::new("rust").expect("valid id")),
                content_digest: crate::chunks::content_digest(RUST_SOURCE.as_bytes()),
                disposition: SnapshotFileDispositionV1::Present,
            };
            let capability = intake
                .admit(SanitizedCodeSnapshotV1 {
                    repository: RepositoryId::new("repo.fixture").expect("valid id"),
                    worktree: None,
                    reference: None,
                    source_revision: None,
                    sanitizer_revision: SanitizerRevision::new("sanitizer.v1").expect("valid id"),
                    sanitization_receipts: vec![
                        SanitizationReceiptId::new("receipt.fixture").expect("valid id"),
                    ],
                    content_identity: crate::chunks::content_digest(RUST_SOURCE.as_bytes()),
                    captured_at: UtcMicros(1_000_000),
                    files: vec![file.clone()],
                })
                .expect("snapshot capability");
            intake
                .bind_file(
                    &capability,
                    &ProjectId::new("project.fixture").expect("valid project"),
                    ValidatedCodeFileV1 {
                        generation_id: CodeGenerationId::new("generation.carried")
                            .expect("valid id"),
                        file,
                        snapshot_digest: capability.snapshot().intake_digest.clone(),
                        sanitized_bytes: RUST_SOURCE.as_bytes().to_vec(),
                    },
                )
                .expect("receipt-bound carried file")
        };

        extraction
            .rebind_file_occurrence(&rebound)
            .expect("same-path generation rebind");
        assert_eq!(extraction.batch().rows_digest, original_digest);
        assert_eq!(
            extraction.batch().generation_id.as_str(),
            "generation.carried"
        );
        assert_eq!(
            extraction.batch().file_occurrence_id.as_str(),
            "file.carried"
        );
        assert_eq!(extraction.logical_path(), "src/lib.rs");
    }

    #[test]
    fn rebind_allows_same_project_path_across_linked_worktrees() {
        let extractor = TreeSitterExtractor::new();
        let primary = validated_file_in_worktree(
            "src/lib.rs",
            RUST_SOURCE.as_bytes(),
            Some(WorktreeId::new("worktree.primary").expect("valid worktree")),
        );
        let linked = validated_file_in_worktree(
            "src/lib.rs",
            RUST_SOURCE.as_bytes(),
            Some(WorktreeId::new("worktree.linked").expect("valid worktree")),
        );
        let mut extraction = extractor
            .extract(&primary, &rust_descriptor(), &NeverCancelled)
            .expect("primary extraction");

        extraction
            .rebind_file_occurrence(&linked)
            .expect("same project, repository, path, and content may be physically reused");
        assert_eq!(
            extraction.batch().content_digest,
            linked.authority().content_digest
        );
    }

    #[test]
    fn unresolved_grammar_and_language_mismatch_are_typed_failures() {
        let extractor = TreeSitterExtractor::new();
        let descriptor = rust_descriptor();

        let mut unavailable = descriptor.clone();
        unavailable.extensions = vec!["unknownext".to_owned()];
        let unknown_extension = validated_file("src/data.unknownext", b"nothing");
        assert!(matches!(
            extractor.extract(&unknown_extension, &unavailable, &NeverCancelled),
            Err(ExtractionFailureV1::GrammarUnavailable {
                language
            }) if language == descriptor.language
        ));

        let python = StaticLanguageRegistry::new()
            .descriptor(&tracedecay_domain::LanguageId::new("python").expect("valid id"))
            .expect("python descriptor")
            .clone();
        let file = validated_file("src/lib.rs", RUST_SOURCE.as_bytes());
        assert!(matches!(
            extractor.extract(&file, &python, &NeverCancelled),
            Err(ExtractionFailureV1::IncompatibleDescriptor { .. })
        ));
    }
}
