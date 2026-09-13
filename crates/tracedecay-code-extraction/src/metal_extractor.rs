/// Metal Shading Language extractor.
///
/// Metal is a strict superset of C++14, so the C++ grammar covers its syntax
/// correctly. This extractor delegates to [`CppExtractor`] and adds the `.metal`
/// extension mapping.
use crate::CppExtractor;
use tree_sitter::Tree;

pub struct MetalExtractor;

impl crate::LanguageExtractor for MetalExtractor {
    fn extensions(&self) -> &[&str] {
        &["metal"]
    }

    fn language_name(&self) -> &'static str {
        "Metal"
    }

    fn extract_parsed_artifact_prepared(
        &self,
        file_path: &str,
        source: &str,
        parsed_source: &str,
        tree: &Tree,
        scope: crate::parsed_extraction::ParsedExtractionScope<'_>,
    ) -> crate::parsed_extraction::ParsedExtractionArtifactV1 {
        CppExtractor.extract_parsed_artifact_prepared(file_path, source, parsed_source, tree, scope)
    }
}
