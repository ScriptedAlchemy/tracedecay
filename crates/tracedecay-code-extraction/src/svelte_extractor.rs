//! Tree-sitter based Svelte source code extractor.
//!
//! Svelte single-file components mix HTML template markup with TypeScript/JavaScript
//! inside one or two `<script>` blocks. This extractor locates those blocks,
//! blanks out every line outside them (preserving line numbers), then delegates
//! to [`TypeScriptExtractor`] so all existing TS/JS symbol extraction logic is
//! reused without duplication.
//!
//! Supported block forms (Svelte 4 and 5):
//!
//! * `<script lang="ts">` — component instance script
//! * `<script>` — component instance script (plain JS)
//! * `<script module>` — module-level script (Svelte 5)
//! * `<script context="module">` — module-level script (Svelte 4)

use std::borrow::Cow;

use crate::types::ExtractionResult;
use crate::typescript_extractor::TypeScriptExtractor;
use crate::{ExtractionArtifactV1, LanguageExtractor};
use tree_sitter::Tree;

/// Extracts code graph nodes and edges from Svelte single-file components.
#[derive(Debug)]
pub struct SvelteExtractor;

impl SvelteExtractor {
    /// Extract nodes and edges from a Svelte source file.
    pub fn extract_svelte(file_path: &str, source: &str) -> ExtractionResult {
        let masked = Self::mask_non_script(source);
        TypeScriptExtractor::extract_typescript(file_path, &masked)
    }

    /// Replace every byte outside `<script>` blocks with whitespace.
    ///
    /// Keeping both byte length and line endings unchanged means the TypeScript
    /// tree and every extracted coordinate remain valid for the original
    /// `.svelte` source.
    fn mask_non_script(source: &str) -> String {
        let ranges = Self::script_content_line_ranges(source);
        let mut masked = String::with_capacity(source.len());
        let mut line = 0;
        for character in source.chars() {
            let keep = ranges
                .iter()
                .any(|&(start, end)| line >= start && line < end);
            if character == '\n' {
                masked.push(character);
                line += 1;
            } else if character == '\r' || keep {
                masked.push(character);
            } else {
                masked.extend(std::iter::repeat_n(' ', character.len_utf8()));
            }
        }
        masked
    }

    /// Return `(content_start, content_end_exclusive)` line-index pairs for
    /// every `<script>` block found in `source`. Tag lines themselves are
    /// excluded so they do not confuse the TypeScript parser.
    fn script_content_line_ranges(source: &str) -> Vec<(usize, usize)> {
        let lines: Vec<&str> = source.lines().collect();
        let mut ranges = Vec::new();
        let mut i = 0;
        while i < lines.len() {
            if Self::is_script_open(lines[i]) {
                let content_start = i + 1;
                let mut j = content_start;
                while j < lines.len() {
                    if Self::is_script_close(lines[j]) {
                        if j > content_start {
                            ranges.push((content_start, j));
                        }
                        i = j + 1;
                        break;
                    }
                    j += 1;
                }
                if j == lines.len() {
                    // Unclosed tag — treat remainder as content.
                    if content_start < lines.len() {
                        ranges.push((content_start, lines.len()));
                    }
                    break;
                }
            } else {
                i += 1;
            }
        }
        ranges
    }

    fn is_script_open(line: &str) -> bool {
        let t = line.trim_start();
        // Must start with `<script` and close its tag on the same line.
        t.starts_with("<script") && t.contains('>')
    }

    fn is_script_close(line: &str) -> bool {
        line.trim_start().starts_with("</script")
    }
}

impl LanguageExtractor for SvelteExtractor {
    fn extensions(&self) -> &[&str] {
        &["svelte"]
    }

    fn language_name(&self) -> &'static str {
        "Svelte"
    }

    fn prepare_parse_source<'a>(&self, source: &'a str) -> Cow<'a, str> {
        Cow::Owned(Self::mask_non_script(source))
    }

    fn extract(&self, file_path: &str, source: &str) -> ExtractionResult {
        Self::extract_svelte(file_path, source)
    }

    fn extract_artifact(&self, file_path: &str, source: &str) -> ExtractionArtifactV1 {
        let masked = Self::mask_non_script(source);
        TypeScriptExtractor.extract_artifact(file_path, &masked)
    }

    fn extract_parsed(
        &self,
        file_path: &str,
        source: &str,
        tree: &Tree,
        scope: crate::parsed_extraction::ParsedExtractionScope<'_>,
    ) -> crate::parsed_extraction::ParsedExtraction {
        let masked = Self::mask_non_script(source);
        TypeScriptExtractor.extract_parsed(file_path, &masked, tree, scope)
    }

    fn extract_parsed_artifact(
        &self,
        file_path: &str,
        source: &str,
        tree: &Tree,
        scope: crate::parsed_extraction::ParsedExtractionScope<'_>,
    ) -> crate::parsed_extraction::ParsedExtractionArtifactV1 {
        let masked = Self::mask_non_script(source);
        TypeScriptExtractor.extract_parsed_artifact(file_path, &masked, tree, scope)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prepared_source_preserves_non_ascii_byte_and_newline_positions() {
        let source =
            "<script lang=\"ts\">\r\nconst résumé = \"界\";\r\n</script>\r\n<h1>界</h1>\r\n";
        let prepared = SvelteExtractor.prepare_parse_source(source);

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
