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

use std::{cmp::Ordering, sync::Arc};

use serde::Serialize;
use tracedecay_domain::{
    ExtractionBatchV1, ExtractionCoverageV1, ExtractionFailureV1, LanguageDescriptorV1,
    ManifestDigest, ParseOutcomeV1, SourceSpan, ValidatedCodeFileV1, canonical_sha256,
};

use super::{intake::ReceiptBoundCodeFileV1, languages::canonical_language_id};
use crate::types::{Edge, ExtractionResult, Node, UnresolvedRef, Visibility};

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
    batch: ExtractionBatchV1,
    parse_artifacts: ExtractionResult,
}

impl ExtractedCodeFileV1 {
    pub fn batch(&self) -> &ExtractionBatchV1 {
        &self.batch
    }

    pub(crate) fn parse_artifacts(&self) -> &ExtractionResult {
        &self.parse_artifacts
    }

    pub(crate) fn into_parts(self) -> (ExtractionBatchV1, ExtractionResult) {
        (self.batch, self.parse_artifacts)
    }
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
pub const EXTRACTION_ROWS_SEPARATOR: &str = "tracedecay.extraction-rows.v1";

/// Pinned maximum source prefix parsed by one extraction operation. Bytes
/// beyond the cap remain explicit unsupported evidence in the batch.
pub const MAX_EXTRACTION_SOURCE_BYTES: usize = 1024 * 1024;

/// The tree-sitter-backed extractor adapter. It reuses the established
/// `crate::extraction` parser registry as the sole parser acquisition path
/// (Plan 25: duplicate parser acquisition paths are forbidden) and adapts its
/// rows into the canonical `ExtractionBatchV1` evidence contract.
///
/// Operational timestamps (`Node::updated_at`, `ExtractionResult::
/// duration_ms`) are excluded from the canonical rows digest, and rows are
/// canonically ordered before hashing, so identical sanitized input under
/// identical descriptor revisions produces identical digests.
pub struct TreeSitterExtractor {
    parsers: Arc<crate::extraction::LanguageRegistry>,
}

impl TreeSitterExtractor {
    /// Create the adapter over a freshly built extraction registry.
    pub fn new() -> Self {
        Self {
            parsers: Arc::new(crate::extraction::LanguageRegistry::new()),
        }
    }

    /// Create the adapter over an existing extraction registry.
    pub fn from_registry(parsers: crate::extraction::LanguageRegistry) -> Self {
        Self {
            parsers: Arc::new(parsers),
        }
    }

    /// Share one generation-scoped registry with downstream chunking.
    pub fn from_shared_registry(parsers: Arc<crate::extraction::LanguageRegistry>) -> Self {
        Self { parsers }
    }

    /// Resolve the parser for one file, falling back to the descriptor's
    /// declared extensions when the logical path carries no (recognized)
    /// extension.
    fn resolve_parser<'a>(
        &'a self,
        file: &ValidatedCodeFileV1,
        descriptor: &LanguageDescriptorV1,
    ) -> Option<&'a dyn crate::extraction::LanguageExtractor> {
        if let Some(extractor) = self.parsers.extractor_for_file(&file.file.logical_path) {
            return Some(extractor);
        }
        descriptor.extensions.iter().find_map(|extension| {
            self.parsers
                .extractor_for_file(&format!("probe.{extension}"))
        })
    }
}

impl Default for TreeSitterExtractor {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Serialize)]
struct CanonicalNodeRow<'a> {
    id: &'a str,
    kind: &'a crate::types::NodeKind,
    name: &'a str,
    qualified_name: &'a str,
    file_path: &'a str,
    start_line: u32,
    attrs_start_line: u32,
    end_line: u32,
    start_column: u32,
    end_column: u32,
    signature: Option<&'a str>,
    docstring: Option<&'a str>,
    visibility: &'a Visibility,
    is_async: bool,
    branches: u32,
    loops: u32,
    returns: u32,
    max_nesting: u32,
    unsafe_blocks: u32,
    unchecked_calls: u32,
    assertions: u32,
    parent_id: Option<&'a str>,
}

impl<'a> From<&'a Node> for CanonicalNodeRow<'a> {
    fn from(node: &'a Node) -> Self {
        Self {
            id: &node.id,
            kind: &node.kind,
            name: &node.name,
            qualified_name: &node.qualified_name,
            file_path: &node.file_path,
            start_line: node.start_line,
            attrs_start_line: node.attrs_start_line,
            end_line: node.end_line,
            start_column: node.start_column,
            end_column: node.end_column,
            signature: node.signature.as_deref(),
            docstring: node.docstring.as_deref(),
            visibility: &node.visibility,
            is_async: node.is_async,
            branches: node.branches,
            loops: node.loops,
            returns: node.returns,
            max_nesting: node.max_nesting,
            unsafe_blocks: node.unsafe_blocks,
            unchecked_calls: node.unchecked_calls,
            assertions: node.assertions,
            parent_id: node.parent_id.as_deref(),
        }
    }
}

fn compare_canonical_nodes(left: &CanonicalNodeRow<'_>, right: &CanonicalNodeRow<'_>) -> Ordering {
    left.id
        .cmp(right.id)
        .then_with(|| left.kind.as_str().cmp(right.kind.as_str()))
        .then_with(|| left.name.cmp(right.name))
        .then_with(|| left.qualified_name.cmp(right.qualified_name))
        .then_with(|| left.file_path.cmp(right.file_path))
        .then_with(|| left.start_line.cmp(&right.start_line))
        .then_with(|| left.attrs_start_line.cmp(&right.attrs_start_line))
        .then_with(|| left.end_line.cmp(&right.end_line))
        .then_with(|| left.start_column.cmp(&right.start_column))
        .then_with(|| left.end_column.cmp(&right.end_column))
        .then_with(|| left.signature.cmp(&right.signature))
        .then_with(|| left.docstring.cmp(&right.docstring))
        .then_with(|| left.visibility.as_str().cmp(right.visibility.as_str()))
        .then_with(|| left.is_async.cmp(&right.is_async))
        .then_with(|| left.branches.cmp(&right.branches))
        .then_with(|| left.loops.cmp(&right.loops))
        .then_with(|| left.returns.cmp(&right.returns))
        .then_with(|| left.max_nesting.cmp(&right.max_nesting))
        .then_with(|| left.unsafe_blocks.cmp(&right.unsafe_blocks))
        .then_with(|| left.unchecked_calls.cmp(&right.unchecked_calls))
        .then_with(|| left.assertions.cmp(&right.assertions))
        .then_with(|| left.parent_id.cmp(&right.parent_id))
}

#[derive(Serialize)]
struct CanonicalEdgeRow<'a> {
    source: &'a str,
    target: &'a str,
    kind: crate::types::EdgeKind,
    line: Option<u32>,
}

impl<'a> From<&'a Edge> for CanonicalEdgeRow<'a> {
    fn from(edge: &'a Edge) -> Self {
        Self {
            source: &edge.source,
            target: &edge.target,
            kind: edge.kind,
            line: edge.line,
        }
    }
}

fn compare_canonical_edges(left: &CanonicalEdgeRow<'_>, right: &CanonicalEdgeRow<'_>) -> Ordering {
    left.source
        .cmp(right.source)
        .then_with(|| left.target.cmp(right.target))
        .then_with(|| left.kind.as_str().cmp(right.kind.as_str()))
        .then_with(|| left.line.cmp(&right.line))
}

#[derive(Serialize)]
struct CanonicalUnresolvedRefRow<'a> {
    from_node_id: &'a str,
    reference_name: &'a str,
    reference_kind: crate::types::EdgeKind,
    line: u32,
    column: u32,
    file_path: &'a str,
}

impl<'a> From<&'a UnresolvedRef> for CanonicalUnresolvedRefRow<'a> {
    fn from(reference: &'a UnresolvedRef) -> Self {
        Self {
            from_node_id: &reference.from_node_id,
            reference_name: &reference.reference_name,
            reference_kind: reference.reference_kind,
            line: reference.line,
            column: reference.column,
            file_path: &reference.file_path,
        }
    }
}

fn compare_canonical_unresolved_refs(
    left: &CanonicalUnresolvedRefRow<'_>,
    right: &CanonicalUnresolvedRefRow<'_>,
) -> Ordering {
    left.from_node_id
        .cmp(right.from_node_id)
        .then_with(|| left.reference_name.cmp(right.reference_name))
        .then_with(|| {
            left.reference_kind
                .as_str()
                .cmp(right.reference_kind.as_str())
        })
        .then_with(|| left.line.cmp(&right.line))
        .then_with(|| left.column.cmp(&right.column))
        .then_with(|| left.file_path.cmp(right.file_path))
}

/// Canonical digest of the extraction rows. Operational timestamps are
/// omitted through borrowed DTOs and rows are canonically ordered by stable
/// typed fields before one payload serialization.
fn rows_digest(
    file: &ValidatedCodeFileV1,
    descriptor: &LanguageDescriptorV1,
    result: &ExtractionResult,
) -> Result<ManifestDigest, ExtractionFailureV1> {
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
    nodes.sort_by(compare_canonical_nodes);
    edges.sort_by(compare_canonical_edges);
    unresolved.sort_by(compare_canonical_unresolved_refs);

    #[derive(Serialize)]
    struct RowsPayload<'a> {
        separator: &'static str,
        logical_path: &'a str,
        language: &'a str,
        descriptor_revision: &'a str,
        grammar_revision: &'a str,
        extractor_revision: &'a str,
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
        nodes,
        edges,
        unresolved_refs: unresolved,
    })
    .map_err(|error| ExtractionFailureV1::ParseFailed {
        detail: format!("canonical rows digest failed: {error}"),
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
        let file = file.validated_file();
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

        let mut result = parser.extract(&file.file.logical_path, extraction_source);
        result.sanitize();

        if cancellation.is_cancelled() {
            return Err(ExtractionFailureV1::Cancelled);
        }

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
        let rows_digest = rows_digest(file, descriptor, &result)?;

        Ok(ExtractedCodeFileV1 {
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
                rows_digest,
            },
            parse_artifacts: result,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracedecay_domain::{
        CodeGenerationId, FileOccurrenceId, RepositoryId, SanitizationReceiptId,
        SanitizedCodeFileV1, SanitizedCodeSnapshotV1, SanitizerRevision, SnapshotFileDispositionV1,
        UtcMicros, ValidatedCodeFileV1,
    };

    use crate::code_index::intake::{CodeIndexIntake, SanitizedCodeIntake};
    use crate::code_index::languages::{LanguageRegistry, StaticLanguageRegistry};

    struct AlwaysCancelled;

    impl ExtractionCancellation for AlwaysCancelled {
        fn is_cancelled(&self) -> bool {
            true
        }
    }

    fn validated_file(path: &str, bytes: &[u8]) -> ReceiptBoundCodeFileV1 {
        let file = SanitizedCodeFileV1 {
            file_occurrence_id: FileOccurrenceId::new("file.fixture").expect("valid id"),
            logical_path: path.to_owned(),
            language: Some(tracedecay_domain::LanguageId::new("rust").expect("valid id")),
            content_digest: crate::code_index::chunks::content_digest(bytes),
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
                worktree: None,
                reference: None,
                source_revision: None,
                sanitizer_revision: SanitizerRevision::new("sanitizer.v1").expect("valid id"),
                sanitization_receipts: vec![
                    SanitizationReceiptId::new("receipt.fixture").expect("valid id"),
                ],
                content_identity: crate::code_index::chunks::content_digest(bytes),
                captured_at: UtcMicros(1_000_000),
                files: vec![file.clone()],
            })
            .expect("snapshot capability");
        intake
            .bind_file(
                &capability,
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
            crate::code_index::chunks::content_digest(RUST_SOURCE.as_bytes())
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
            "sha256:e9812b169bc0d1bdbfc013132ec4a808dfdd7d982822b78ee929986a19d940d3"
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
