//! File-operation Hotpath instrumentation for language parse and extract.
//!
//! Spans stay at one file per measurement. Individual AST nodes are never
//! timed. Labels use a closed family vocabulary and byte-size buckets so
//! cardinality cannot grow with path, language dialect, or exact file size.
//! Every macro expands to a no-op unless this crate's `hotpath` feature is
//! selected.

use crate::extraction_artifact::ExtractionArtifactV1;
use crate::parsed_extraction::ParsedExtractionArtifactV1;
use crate::types::ExtractionResult;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct ExtractOutputCounts {
    pub nodes: usize,
    pub edges: usize,
    pub unresolved_refs: usize,
    pub imports: usize,
}

impl ExtractOutputCounts {
    pub(crate) fn from_artifact(artifact: &ExtractionArtifactV1) -> Self {
        Self::from_result_and_imports(&artifact.result, artifact.imports.len())
    }

    pub(crate) fn from_parsed_artifact(parsed: &ParsedExtractionArtifactV1) -> Self {
        Self::from_artifact(&parsed.artifact)
    }

    pub(crate) fn from_extract_result<E>(result: &Result<ParsedExtractionArtifactV1, E>) -> Self {
        match result {
            Ok(parsed) => Self::from_parsed_artifact(parsed),
            Err(_) => Self::default(),
        }
    }

    fn from_result_and_imports(result: &ExtractionResult, imports: usize) -> Self {
        Self {
            nodes: result.nodes.len(),
            edges: result.edges.len(),
            unresolved_refs: result.unresolved_refs.len(),
            imports,
        }
    }
}

/// Closed language-family label. Accepts extractor display names and the
/// lowercase / grammar-key aliases retained parse already uses.
pub(crate) fn language_family(language: &str) -> &'static str {
    match language {
        "C" | "c" | "C++" | "cpp" | "c++" | "Metal" | "metal" | "Objective-C" | "objc"
        | "objective-c" | "Rust" | "rust" | "Zig" | "zig" => "systems",
        "Java" | "java" | "Kotlin" | "kotlin" | "Scala" | "scala" => "jvm",
        "C#" | "c#" | "csharp" | "c_sharp" | "F#" | "f#" | "fsharp" | "VB.NET" | "vb.net"
        | "vbnet" => "dotnet",
        "Astro" | "astro" | "JavaScript" | "javascript" | "jsx" | "Svelte" | "svelte"
        | "TypeScript" | "typescript" | "tsx" => "web",
        "Python" | "python" => "python",
        "Go" | "go" => "go",
        "Dart" | "dart" | "Swift" | "swift" => "managed",
        "Bash" | "bash" | "Batch" | "batch" | "Lua" | "lua" | "Nix" | "nix" | "Perl" | "perl"
        | "PHP" | "php" | "PowerShell" | "powershell" | "Ruby" | "ruby" => "scripting",
        "Clojure" | "clojure" | "Elixir" | "elixir" | "Erlang" | "erlang" | "Haskell"
        | "haskell" | "Julia" | "julia" | "Lean" | "lean" | "OCaml" | "ocaml" => "functional",
        "Protobuf" | "protobuf" | "R" | "r" | "SQL" | "sql" | "TOML" | "toml" => "data",
        "Dockerfile" | "dockerfile" | "Markdown" | "markdown" => "markup",
        "GLSL" | "glsl" | "HLSL" | "hlsl" | "WGSL" | "wgsl" => "shader",
        "COBOL" | "cobol" | "Fortran" | "fortran" | "GW-BASIC" | "gwbasic" | "gw-basic"
        | "MS BASIC 2.0" | "msbasic2" | "Pascal" | "pascal" | "QBasic" | "qbasic"
        | "QuickBASIC" | "quickbasic" => "basic",
        "Quint" | "quint" => "spec",
        _ => "other",
    }
}

/// Bounded source-size label. Exact byte length is never a Hotpath key.
pub(crate) fn file_byte_bucket(bytes: usize) -> &'static str {
    const KIB: usize = 1024;
    const MIB: usize = 1024 * 1024;
    match bytes {
        0..=KIB => "le_1kib",
        1025..=4096 => "le_4kib",
        4097..=16384 => "le_16kib",
        16385..=65536 => "le_64kib",
        65537..=262144 => "le_256kib",
        262145..=MIB => "le_1mib",
        1_048_577..=2_097_152 => "le_2mib",
        _ => "gt_2mib",
    }
}

fn record_file_dims(language: &str, source_bytes: usize) {
    hotpath::val!("code_extraction.language_family").set(&language_family(language));
    hotpath::val!("code_extraction.file_byte_bucket").set(&file_byte_bucket(source_bytes));
}

/// Time one file parse. `output_count` is a file-level count (root children),
/// never a per-node walk.
#[inline]
pub(crate) fn measure_parse_file<T>(
    language: &str,
    source_bytes: usize,
    f: impl FnOnce() -> T,
    output_count: impl FnOnce(&T) -> usize,
) -> T {
    record_file_dims(language, source_bytes);
    let result = hotpath::measure_block!("code_extraction.parse_file", f());
    hotpath::gauge!("code_extraction.parse.root_children").set(output_count(&result) as f64);
    result
}

/// Time one file extract and record graph-output counts.
#[inline]
pub(crate) fn measure_extract_file<T>(
    language: &str,
    source_bytes: usize,
    f: impl FnOnce() -> T,
    counts: impl FnOnce(&T) -> ExtractOutputCounts,
) -> T {
    record_file_dims(language, source_bytes);
    let result = hotpath::measure_block!("code_extraction.extract_file", f());
    let counts = counts(&result);
    hotpath::gauge!("code_extraction.extract.nodes").set(counts.nodes as f64);
    hotpath::gauge!("code_extraction.extract.edges").set(counts.edges as f64);
    hotpath::gauge!("code_extraction.extract.unresolved_refs").set(counts.unresolved_refs as f64);
    hotpath::gauge!("code_extraction.extract.imports").set(counts.imports as f64);
    result
}

#[cfg(test)]
mod tests {
    use super::{file_byte_bucket, language_family};

    #[test]
    fn language_family_is_closed_and_alias_stable() {
        assert_eq!(language_family("Rust"), "systems");
        assert_eq!(language_family("rust"), "systems");
        assert_eq!(language_family("TypeScript"), "web");
        assert_eq!(language_family("tsx"), "web");
        assert_eq!(language_family("c_sharp"), "dotnet");
        assert_eq!(language_family("Objective-C"), "systems");
        assert_eq!(language_family("unknown-lang"), "other");
    }

    #[test]
    fn file_byte_bucket_is_bounded() {
        assert_eq!(file_byte_bucket(0), "le_1kib");
        assert_eq!(file_byte_bucket(1024), "le_1kib");
        assert_eq!(file_byte_bucket(1025), "le_4kib");
        assert_eq!(file_byte_bucket(4096), "le_4kib");
        assert_eq!(file_byte_bucket(2 * 1024 * 1024), "le_2mib");
        assert_eq!(file_byte_bucket(2 * 1024 * 1024 + 1), "gt_2mib");
    }

    #[test]
    fn file_byte_bucket_never_uses_exact_size() {
        assert_ne!(file_byte_bucket(12345), "12345");
        assert_eq!(file_byte_bucket(12345), "le_16kib");
    }
}
