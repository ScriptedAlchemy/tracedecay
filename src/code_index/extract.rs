//! Language extractor port (Plan 25 phase 3): `languages.rs` owns the
//! descriptors; this module owns the extractor contract
//! `LanguageExtractor::extract(&ValidatedCodeFileV1, &LanguageDescriptorV1,
//! &CancellationToken) -> Result<ExtractionBatchV1, ExtractionFailureV1>`.
//!
//! Extraction acquires one tree-sitter parser from the descriptor's pinned
//! grammar; the in-process `ast-grep-core` structural kernel shares that
//! pinned grammar and source generation. Parse errors and unsupported
//! constructs are preserved as evidence; extraction never invents successful
//! structure.

use serde::Serialize;
use tracedecay_domain::{
    ExtractionBatchV1, ExtractionCoverageV1, ExtractionFailureV1, LanguageDescriptorV1,
    ManifestDigest, ParseOutcomeV1, SourceSpan, ValidatedCodeFileV1, canonical_sha256,
};

use super::languages::canonical_language_id;

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

/// The language extractor contract (Plan 25). Language-specific logic stays
/// behind this small interface while identity, lineage, and output contracts
/// are shared.
pub trait LanguageExtractor {
    /// Extract one canonical batch from one validated file under one
    /// descriptor. Identical input, registry, and extractor revisions produce
    /// stable canonical rows and digests on every supported host.
    fn extract(
        &self,
        file: &ValidatedCodeFileV1,
        descriptor: &LanguageDescriptorV1,
        cancellation: &dyn ExtractionCancellation,
    ) -> Result<ExtractionBatchV1, ExtractionFailureV1>;
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
    parsers: crate::extraction::LanguageRegistry,
}

impl TreeSitterExtractor {
    /// Create the adapter over a freshly built extraction registry.
    pub fn new() -> Self {
        Self {
            parsers: crate::extraction::LanguageRegistry::new(),
        }
    }

    /// Create the adapter over an existing extraction registry.
    pub fn from_registry(parsers: crate::extraction::LanguageRegistry) -> Self {
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

/// Canonical digest of the extraction rows. Operational timestamps are
/// stripped and rows are canonically ordered before hashing.
fn rows_digest(
    file: &ValidatedCodeFileV1,
    descriptor: &LanguageDescriptorV1,
    result: &crate::types::ExtractionResult,
) -> Result<ManifestDigest, ExtractionFailureV1> {
    let mut nodes: Vec<serde_json::Value> = result
        .nodes
        .iter()
        .map(|node| {
            let mut value =
                serde_json::to_value(node).expect("extraction nodes serialize canonically");
            if let Some(object) = value.as_object_mut() {
                object.remove("updated_at");
            }
            value
        })
        .collect();
    let mut edges: Vec<serde_json::Value> = result
        .edges
        .iter()
        .map(|edge| serde_json::to_value(edge).expect("extraction edges serialize canonically"))
        .collect();
    let mut unresolved: Vec<serde_json::Value> = result
        .unresolved_refs
        .iter()
        .map(|reference| {
            serde_json::to_value(reference).expect("unresolved refs serialize canonically")
        })
        .collect();
    let canonical_order = |left: &serde_json::Value, right: &serde_json::Value| {
        serde_json::to_string(left)
            .expect("canonical value serializes")
            .cmp(&serde_json::to_string(right).expect("canonical value serializes"))
    };
    nodes.sort_by(canonical_order);
    edges.sort_by(canonical_order);
    unresolved.sort_by(canonical_order);

    #[derive(Serialize)]
    struct RowsPayload<'a> {
        separator: &'static str,
        logical_path: &'a str,
        language: &'a str,
        descriptor_revision: &'a str,
        grammar_revision: &'a str,
        extractor_revision: &'a str,
        nodes: Vec<serde_json::Value>,
        edges: Vec<serde_json::Value>,
        unresolved_refs: Vec<serde_json::Value>,
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
        file: &ValidatedCodeFileV1,
        descriptor: &LanguageDescriptorV1,
        cancellation: &dyn ExtractionCancellation,
    ) -> Result<ExtractionBatchV1, ExtractionFailureV1> {
        if cancellation.is_cancelled() {
            return Err(ExtractionFailureV1::Cancelled);
        }
        if let Some(declared) = &file.file.language {
            if declared != &descriptor.language {
                return Err(ExtractionFailureV1::IncompatibleDescriptor {
                    detail: format!(
                        "file declares language {} but descriptor is {}",
                        declared, descriptor.language
                    ),
                });
            }
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

        Ok(ExtractionBatchV1 {
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
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracedecay_domain::{
        ContentDigest, FileOccurrenceId, ManifestDigest, SanitizedCodeFileV1,
        SnapshotFileDispositionV1,
    };

    use crate::code_index::languages::{LanguageRegistry, StaticLanguageRegistry};

    struct AlwaysCancelled;

    impl ExtractionCancellation for AlwaysCancelled {
        fn is_cancelled(&self) -> bool {
            true
        }
    }

    fn digest(byte: char) -> ContentDigest {
        ContentDigest::new(format!("sha256:{}", byte.to_string().repeat(64))).expect("valid digest")
    }

    fn validated_file(path: &str, bytes: &[u8]) -> ValidatedCodeFileV1 {
        ValidatedCodeFileV1 {
            generation_id: tracedecay_domain::CodeGenerationId::new("generation.fixture")
                .expect("valid id"),
            file: SanitizedCodeFileV1 {
                file_occurrence_id: FileOccurrenceId::new("file.fixture").expect("valid id"),
                logical_path: path.to_owned(),
                language: None,
                content_digest: digest('a'),
                disposition: SnapshotFileDispositionV1::Present,
            },
            snapshot_digest: ManifestDigest::new(format!("sha256:{}", "b".repeat(64)))
                .expect("valid digest"),
            sanitized_bytes: bytes.to_vec(),
        }
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
        let batch = extractor
            .extract(&file, &rust_descriptor(), &NeverCancelled)
            .expect("extraction succeeds");

        assert_eq!(batch.parse_outcome, ParseOutcomeV1::Complete);
        assert_eq!(batch.language.as_str(), "rust");
        assert_eq!(batch.content_digest, digest('a'));
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
        assert_eq!(first.rows_digest, second.rows_digest);
        assert_eq!(first, second);
    }

    #[test]
    fn cancellation_is_checked_at_deterministic_boundaries() {
        let extractor = TreeSitterExtractor::new();
        let file = validated_file("src/lib.rs", RUST_SOURCE.as_bytes());
        assert_eq!(
            extractor.extract(&file, &rust_descriptor(), &AlwaysCancelled),
            Err(ExtractionFailureV1::Cancelled)
        );
    }

    #[test]
    fn unresolved_grammar_and_language_mismatch_are_typed_failures() {
        let extractor = TreeSitterExtractor::new();
        let descriptor = rust_descriptor();

        // A descriptor whose extensions no compiled grammar serves.
        let mut unavailable = descriptor.clone();
        unavailable.language = tracedecay_domain::LanguageId::new("cobol-nope").expect("valid id");
        unavailable.extensions = vec!["unknownext".to_owned()];
        unavailable.aliases = vec!["cobol-nope".to_owned()];
        let file = validated_file("src/data.unknownext", b"nothing");
        assert_eq!(
            extractor.extract(&file, &unavailable, &NeverCancelled),
            Err(ExtractionFailureV1::GrammarUnavailable {
                language: tracedecay_domain::LanguageId::new("cobol-nope").expect("valid id"),
            })
        );

        // Declared language disagrees with the descriptor.
        let mut mismatched = validated_file("src/lib.rs", RUST_SOURCE.as_bytes());
        mismatched.file.language =
            Some(tracedecay_domain::LanguageId::new("python").expect("valid id"));
        assert!(matches!(
            extractor.extract(&mismatched, &descriptor, &NeverCancelled),
            Err(ExtractionFailureV1::IncompatibleDescriptor { .. })
        ));
    }

    #[test]
    fn invalid_utf8_is_a_parse_failure_not_a_panic() {
        let extractor = TreeSitterExtractor::new();
        let file = validated_file("src/lib.rs", &[0x66, 0x6e, 0x20, 0xFF, 0xFE]);
        assert!(matches!(
            extractor.extract(&file, &rust_descriptor(), &NeverCancelled),
            Err(ExtractionFailureV1::ParseFailed { .. })
        ));
    }
}
