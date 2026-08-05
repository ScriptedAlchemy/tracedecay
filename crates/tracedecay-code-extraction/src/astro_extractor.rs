//! Tree-sitter based Astro source code extractor.
//!
//! Astro components embed TypeScript in a YAML-fenced frontmatter block at the
//! top of the file, delimited by `---` marker lines:
//!
//! ```text
//! ---
//! import Hero from './Hero.astro';
//! interface Props { title: string; }
//! const { title } = Astro.props;
//! ---
//! <html>…</html>
//! ```
//!
//! This extractor locates the frontmatter block, blanks out every line outside
//! it (preserving line numbers), then delegates to [`TypeScriptExtractor`] so
//! all existing TS/JS symbol extraction logic is reused without duplication.

use std::borrow::Cow;

use crate::LanguageExtractor;
use crate::types::ExtractionResult;
use crate::typescript_extractor::TypeScriptExtractor;
use tree_sitter::Tree;

/// Extracts code graph nodes and edges from Astro component files.
#[derive(Debug)]
pub struct AstroExtractor;

impl AstroExtractor {
    /// Extract nodes and edges from an Astro source file.
    pub fn extract_astro(file_path: &str, source: &str) -> ExtractionResult {
        let masked = Self::mask_non_frontmatter(source);
        TypeScriptExtractor::extract_typescript(file_path, &masked)
    }

    /// Replace every byte outside the `---` frontmatter block with whitespace.
    ///
    /// Keeping both byte length and line endings unchanged means the TypeScript
    /// tree and every extracted coordinate remain valid for the original
    /// `.astro` source.
    fn mask_non_frontmatter(source: &str) -> String {
        let lines: Vec<&str> = source.lines().collect();

        // Frontmatter requires the very first line to be `---`.
        if lines.first().map(|l| l.trim()) != Some("---") {
            return Self::mask_lines(source, |_| false);
        }

        // Find the closing `---` marker (first occurrence after line 0).
        let content_start = 1;
        let content_end = lines[content_start..]
            .iter()
            .position(|l| l.trim() == "---")
            .map_or(lines.len(), |rel| content_start + rel); // unclosed — include everything

        Self::mask_lines(source, |line| line >= content_start && line < content_end)
    }

    fn mask_lines(source: &str, keep: impl Fn(usize) -> bool) -> String {
        let mut masked = String::with_capacity(source.len());
        let mut line = 0;
        for character in source.chars() {
            if character == '\n' {
                masked.push(character);
                line += 1;
            } else if character == '\r' || keep(line) {
                masked.push(character);
            } else {
                masked.extend(std::iter::repeat_n(' ', character.len_utf8()));
            }
        }
        masked
    }
}

impl LanguageExtractor for AstroExtractor {
    fn extensions(&self) -> &[&str] {
        &["astro"]
    }

    fn language_name(&self) -> &'static str {
        "Astro"
    }

    fn prepare_parse_source<'a>(&self, source: &'a str) -> Cow<'a, str> {
        Cow::Owned(Self::mask_non_frontmatter(source))
    }

    fn extract(&self, file_path: &str, source: &str) -> ExtractionResult {
        Self::extract_astro(file_path, source)
    }

    fn extract_parsed(
        &self,
        file_path: &str,
        source: &str,
        tree: &Tree,
        scope: crate::parsed_extraction::ParsedExtractionScope<'_>,
    ) -> crate::parsed_extraction::ParsedExtraction {
        let masked = Self::mask_non_frontmatter(source);
        TypeScriptExtractor.extract_parsed(file_path, &masked, tree, scope)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prepared_source_preserves_non_ascii_byte_and_newline_positions() {
        let source = "---\r\nconst résumé = \"界\";\r\n---\r\n<h1>界</h1>\r\n";
        let prepared = AstroExtractor.prepare_parse_source(source);

        assert_eq!(prepared.len(), source.len());
        assert_eq!(
            prepared
                .bytes()
                .enumerate()
                .filter_map(|(index, byte)| (byte == b'\r' || byte == b'\n').then_some(index))
                .collect::<Vec<_>>(),
            source
                .bytes()
                .enumerate()
                .filter_map(|(index, byte)| (byte == b'\r' || byte == b'\n').then_some(index))
                .collect::<Vec<_>>()
        );
        assert_eq!(prepared.lines().nth(1), Some("const résumé = \"界\";"));
    }
}
