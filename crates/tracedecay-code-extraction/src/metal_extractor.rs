/// Metal Shading Language extractor.
///
/// Metal is a strict superset of C++14, so the C++ grammar covers its syntax
/// correctly. This extractor delegates to [`CppExtractor`] and adds the `.metal`
/// extension mapping.
use crate::CppExtractor;
use crate::types::ExtractionResult;
use tree_sitter::Tree;

pub struct MetalExtractor;

impl crate::LanguageExtractor for MetalExtractor {
    fn extensions(&self) -> &[&str] {
        &["metal"]
    }

    fn language_name(&self) -> &'static str {
        "Metal"
    }

    fn extract(&self, file_path: &str, source: &str) -> ExtractionResult {
        CppExtractor::extract_source(file_path, source)
    }

    fn extract_parsed(
        &self,
        file_path: &str,
        source: &str,
        tree: &Tree,
        scope: crate::parsed_extraction::ParsedExtractionScope<'_>,
    ) -> crate::parsed_extraction::ParsedExtraction {
        <CppExtractor as crate::LanguageExtractor>::extract_parsed(
            &CppExtractor,
            file_path,
            source,
            tree,
            scope,
        )
    }
}
