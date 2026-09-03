/// Tree-sitter based `QuickBasic` 4.5 source code extractor.
///
/// `QuickBasic` 4.5 is a superset of `QBasic` with separate compilation,
/// `$INCLUDE` metacommands, `REDIM`, and compiled `.EXE` output.
/// The grammar is identical to `QBasic` (parsed by `tree-sitter-qbasic`),
/// so this extractor delegates to `QBasicExtractor` for all extraction
/// and registers the QuickBasic-specific file extensions (`.bi`, `.bm`).
use crate::qbasic_extractor::QBasicExtractor;
use crate::types::ExtractionResult;
use tree_sitter::Tree;

/// Extracts code graph nodes and edges from `QuickBasic` 4.5 source files.
///
/// Uses the same tree-sitter grammar and extraction logic as
/// [`QBasicExtractor`] — the languages are syntactically identical.
pub struct QuickBasicExtractor;

impl crate::LanguageExtractor for QuickBasicExtractor {
    fn extensions(&self) -> &[&str] {
        &["bi", "bm"]
    }

    fn language_name(&self) -> &'static str {
        "QuickBASIC"
    }

    fn extract(&self, file_path: &str, source: &str) -> ExtractionResult {
        QBasicExtractor::extract_qbasic(file_path, source)
    }

    fn extract_parsed(
        &self,
        file_path: &str,
        source: &str,
        tree: &Tree,
        scope: crate::parsed_extraction::ParsedExtractionScope<'_>,
    ) -> crate::parsed_extraction::ParsedExtraction {
        <QBasicExtractor as crate::LanguageExtractor>::extract_parsed(
            &QBasicExtractor,
            file_path,
            source,
            tree,
            scope,
        )
    }
}
