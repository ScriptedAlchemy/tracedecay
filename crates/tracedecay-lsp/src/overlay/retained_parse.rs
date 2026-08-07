//! Session-local adapter around the canonical retained parsing leaf.

use std::sync::{Arc, OnceLock};

use tracedecay_code_extraction::incremental::{
    ParseDocumentIdentity, ParseError, ParseInputEdit, ParseLimits, ParseReport,
    RetainedParseDocument,
};
use tracedecay_code_extraction::parsed_extraction::{
    ParsedExtractionDisposition, ParsedTraversalMetrics,
};
use tracedecay_code_extraction::{LanguageExtractor, LanguageRegistry};
use tracedecay_domain::ExtractionResult;

/// Parser state exposed with an ephemeral overlay snapshot.
///
/// A ready report is boxed: it dwarfs the typed unavailable reason, and every
/// snapshot of every open document carries one of these.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OverlayParseState {
    Ready(Box<ParseReport>),
    Unavailable(OverlayParseUnavailable),
}

/// Typed reason an accepted text overlay has no current retained tree.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OverlayParseUnavailable {
    UnsupportedLanguage,
    SourceTooLarge,
    InvalidEdit,
    PreparedSourceShapeMismatch,
    IdentityMismatch,
    StaleReport,
    GrammarRejected,
    TimedOut,
    ParseFailed,
}

impl From<&ParseError> for OverlayParseUnavailable {
    fn from(error: &ParseError) -> Self {
        match error {
            ParseError::UnsupportedLanguage { .. } => Self::UnsupportedLanguage,
            ParseError::SourceTooLarge { .. } => Self::SourceTooLarge,
            ParseError::InvalidEdit { .. } => Self::InvalidEdit,
            ParseError::PreparedSourceShapeMismatch => Self::PreparedSourceShapeMismatch,
            ParseError::IdentityMismatch => Self::IdentityMismatch,
            ParseError::StaleReport => Self::StaleReport,
            ParseError::GrammarRejected { .. } => Self::GrammarRejected,
            ParseError::TimedOut { .. } => Self::TimedOut,
            ParseError::ParseFailed => Self::ParseFailed,
        }
    }
}

/// Canonical graph rows extracted from the current retained overlay tree.
///
/// The result is shared only with snapshots from this session-owned store. It
/// is never a persistence input or a clean-generation authority.
#[derive(Clone, Debug)]
pub enum OverlayExtractionState {
    Ready {
        result: Arc<ExtractionResult>,
        disposition: ParsedExtractionDisposition,
        metrics: ParsedTraversalMetrics,
    },
    Unavailable(OverlayParseUnavailable),
}

/// Canonical extraction rows are not structurally comparable, so two ready
/// states are equal only when they name the exact same shared extraction. That
/// is the property callers actually rely on: a snapshot either carries this
/// store's current extraction instance or it is stale.
impl PartialEq for OverlayExtractionState {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (
                Self::Ready {
                    result,
                    disposition,
                    metrics,
                },
                Self::Ready {
                    result: other_result,
                    disposition: other_disposition,
                    metrics: other_metrics,
                },
            ) => {
                Arc::ptr_eq(result, other_result)
                    && disposition == other_disposition
                    && metrics == other_metrics
            }
            (Self::Unavailable(reason), Self::Unavailable(other_reason)) => reason == other_reason,
            _ => false,
        }
    }
}

pub(super) struct RetainedOverlayParse {
    document: Option<RetainedParseDocument>,
    parse_state: OverlayParseState,
    prior_raw_extraction: Option<Arc<ExtractionResult>>,
    extraction_state: OverlayExtractionState,
}

impl RetainedOverlayParse {
    pub(super) fn open(identity: ParseDocumentIdentity, language_id: &str, source: &str) -> Self {
        let Some(extractor) = extractor_for(identity.logical_path()) else {
            return Self::unavailable(OverlayParseUnavailable::UnsupportedLanguage);
        };
        let grammar_key = extractor.retained_grammar_key(identity.logical_path());
        let prepared_source = extractor.prepare_parse_source(source);
        match RetainedParseDocument::open_prepared(
            identity,
            language_id,
            grammar_key,
            source,
            prepared_source,
            ParseLimits::default(),
        ) {
            Ok((document, report)) => {
                let mut retained = Self {
                    document: Some(document),
                    parse_state: OverlayParseState::Ready(Box::new(report.clone())),
                    prior_raw_extraction: None,
                    extraction_state: OverlayExtractionState::Unavailable(
                        OverlayParseUnavailable::StaleReport,
                    ),
                };
                retained.extract(extractor, &report);
                retained
            }
            Err(error) => Self::unavailable((&error).into()),
        }
    }

    pub(super) fn update(
        &mut self,
        next_identity: ParseDocumentIdentity,
        language_id: &str,
        edits: &[ParseInputEdit],
        source: &str,
        full_replacement: bool,
    ) {
        let Some(extractor) = extractor_for(next_identity.logical_path()) else {
            *self = Self::unavailable(OverlayParseUnavailable::UnsupportedLanguage);
            return;
        };
        let prepared_source = extractor.prepare_parse_source(source);
        let result = if let Some(document) = self.document.as_mut() {
            let report = if full_replacement {
                document.replace_prepared(next_identity, source, prepared_source)
            } else {
                document.apply_edits_prepared(next_identity, edits, source, prepared_source)
            };
            report.map(|report| (None, report))
        } else {
            let grammar_key = extractor.retained_grammar_key(next_identity.logical_path());
            RetainedParseDocument::open_prepared(
                next_identity,
                language_id,
                grammar_key,
                source,
                prepared_source,
                ParseLimits::default(),
            )
            .map(|(document, report)| (Some(document), report))
        };
        match result {
            Ok((replacement, report)) => {
                if let Some(document) = replacement {
                    self.document = Some(document);
                }
                self.parse_state = OverlayParseState::Ready(Box::new(report.clone()));
                self.extract(extractor, &report);
            }
            Err(error) => {
                *self = Self::unavailable((&error).into());
            }
        }
    }

    pub(super) fn parse_state(&self) -> &OverlayParseState {
        &self.parse_state
    }

    pub(super) fn extraction_state(&self) -> &OverlayExtractionState {
        &self.extraction_state
    }

    fn extract(&mut self, extractor: &dyn LanguageExtractor, report: &ParseReport) {
        let Some(document) = self.document.as_ref() else {
            self.prior_raw_extraction = None;
            self.extraction_state =
                OverlayExtractionState::Unavailable(OverlayParseUnavailable::StaleReport);
            return;
        };
        match document.extract_canonical(extractor, report, self.prior_raw_extraction.as_deref()) {
            Ok(extraction) => {
                let result = Arc::new(extraction.result);
                self.prior_raw_extraction = Some(Arc::clone(&result));
                self.extraction_state = OverlayExtractionState::Ready {
                    result,
                    disposition: extraction.disposition,
                    metrics: extraction.metrics,
                };
            }
            Err(error) => {
                self.prior_raw_extraction = None;
                self.extraction_state = OverlayExtractionState::Unavailable((&error).into());
            }
        }
    }

    fn unavailable(reason: OverlayParseUnavailable) -> Self {
        Self {
            document: None,
            parse_state: OverlayParseState::Unavailable(reason),
            prior_raw_extraction: None,
            extraction_state: OverlayExtractionState::Unavailable(reason),
        }
    }
}

fn extractor_for(path: &str) -> Option<&'static dyn LanguageExtractor> {
    static REGISTRY: OnceLock<LanguageRegistry> = OnceLock::new();
    REGISTRY
        .get_or_init(LanguageRegistry::new)
        .extractor_for_file(path)
}

#[cfg(test)]
mod tests {
    use tracedecay_code_extraction::incremental::{ParsePoint, ParseReuse};
    use tracedecay_code_extraction::parsed_extraction::{
        ParsedExtractionDisposition, ParsedExtractionResetReason,
    };
    use tracedecay_domain::{ContentDigest, canonical_sha256};

    use super::*;

    fn identity(path: &str, version: i64, source: &str) -> ParseDocumentIdentity {
        ParseDocumentIdentity::SessionOverlay {
            scope_identity: canonical_sha256(&("overlay-test-scope", path)).unwrap(),
            document_identity: canonical_sha256(&("overlay-test-document", path)).unwrap(),
            version,
            content_digest: ContentDigest::of_bytes(source.as_bytes()),
            logical_path: path.to_owned(),
        }
    }

    fn point_at(source: &str, byte: usize) -> ParsePoint {
        let prefix = &source[..byte];
        let row = prefix.bytes().filter(|byte| *byte == b'\n').count();
        let column = prefix
            .rfind('\n')
            .map_or(prefix.len(), |line_start| prefix.len() - line_start - 1);
        ParsePoint { row, column }
    }

    fn same_line_edit(
        source: &str,
        start: usize,
        old_end: usize,
        replacement: &str,
    ) -> ParseInputEdit {
        let start_position = point_at(source, start);
        ParseInputEdit {
            start_byte: start,
            old_end_byte: old_end,
            new_end_byte: start + replacement.len(),
            start_position,
            old_end_position: point_at(source, old_end),
            new_end_position: ParsePoint {
                row: start_position.row,
                column: start_position.column + replacement.len(),
            },
        }
    }

    fn names(state: &OverlayExtractionState) -> Vec<&str> {
        let OverlayExtractionState::Ready { result, .. } = state else {
            panic!("expected canonical overlay extraction");
        };
        result.nodes.iter().map(|node| node.name.as_str()).collect()
    }

    #[test]
    fn canonical_extraction_tracks_incremental_overlay_edits() {
        let before = "fn before() {}";
        let after = "fn after_() {}";
        let mut retained =
            RetainedOverlayParse::open(identity("src/lib.rs", 1, before), "rust", before);
        assert!(names(retained.extraction_state()).contains(&"before"));

        retained.update(
            identity("src/lib.rs", 2, after),
            "rust",
            &[same_line_edit(before, 3, 9, "after_")],
            after,
            false,
        );

        let OverlayParseState::Ready(report) = retained.parse_state() else {
            panic!("expected retained parse");
        };
        assert_eq!(report.reuse, ParseReuse::Incremental);
        let names = names(retained.extraction_state());
        assert!(names.contains(&"after_"));
        assert!(!names.contains(&"before"));
    }

    #[test]
    fn composite_overlays_prepare_edits_and_reset_full_replacements() {
        for (path, language_id, before, after, start, old_end, replacement) in [
            (
                "Component.svelte",
                "svelte",
                "<script>\nfunction before() {}\n</script>\n<h1>Old</h1>",
                "<script>\nfunction after_() {}\n</script>\n<h1>Old</h1>",
                18,
                24,
                "after_",
            ),
            (
                "Page.astro",
                "astro",
                "---\nfunction before() {}\n---\n<h1>Old</h1>",
                "---\nfunction after_() {}\n---\n<h1>Old</h1>",
                13,
                19,
                "after_",
            ),
        ] {
            let mut retained =
                RetainedOverlayParse::open(identity(path, 1, before), language_id, before);
            retained.update(
                identity(path, 2, after),
                language_id,
                &[same_line_edit(before, start, old_end, replacement)],
                after,
                false,
            );

            let OverlayParseState::Ready(report) = retained.parse_state() else {
                panic!("expected prepared incremental parse");
            };
            assert_eq!(report.reuse, ParseReuse::Incremental);
            let OverlayExtractionState::Ready {
                result,
                disposition,
                ..
            } = retained.extraction_state()
            else {
                panic!("expected canonical composite extraction");
            };
            assert_eq!(*disposition, ParsedExtractionDisposition::ChangedRegions);
            assert!(result.nodes.iter().any(|node| node.name == "after_"));
            assert!(!result.nodes.iter().any(|node| node.name == "before"));

            let replacement_source = after.replace("after_", "latest");
            retained.update(
                identity(path, 3, &replacement_source),
                language_id,
                &[],
                &replacement_source,
                true,
            );
            let OverlayExtractionState::Ready {
                result,
                disposition,
                ..
            } = retained.extraction_state()
            else {
                panic!("expected canonical replacement extraction");
            };
            assert_eq!(
                *disposition,
                ParsedExtractionDisposition::Reset {
                    reason: ParsedExtractionResetReason::FullReplacement,
                }
            );
            assert!(result.nodes.iter().any(|node| node.name == "latest"));
            assert!(!result.nodes.iter().any(|node| node.name == "after_"));
        }
    }

    #[test]
    fn retained_extractions_are_isolated_between_sessions() {
        let first = RetainedOverlayParse::open(
            identity("src/lib.rs", 1, "fn first() {}"),
            "rust",
            "fn first() {}",
        );
        let second = RetainedOverlayParse::open(
            identity("src/lib.rs", 1, "fn second() {}"),
            "rust",
            "fn second() {}",
        );

        assert!(names(first.extraction_state()).contains(&"first"));
        assert!(!names(first.extraction_state()).contains(&"second"));
        assert!(names(second.extraction_state()).contains(&"second"));
        assert!(!names(second.extraction_state()).contains(&"first"));
    }
}
