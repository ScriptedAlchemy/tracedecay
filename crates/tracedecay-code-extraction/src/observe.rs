//! File-operation instrumentation for language parse and extract.
//!
//! Spans stay at one file per measurement. Individual AST nodes are never
//! timed. Static counters use a closed family vocabulary and byte-size buckets
//! so cardinality cannot grow with path, language dialect, or exact file size.
//! Failed, timed-out, and abstained work is recorded through the same closed
//! vocabularies so success-only totals cannot hide waste. Per-family
//! `*_nanos` gauges accumulate inclusive aggregate service demand (parallel
//! workers overlap); the span totals remain the timing authority.
//! No metrics recorder is installed outside profiling sessions, so with TRACE
//! disabled the per-file measures call the operation directly and neither
//! read the clock, derive dimensions, nor count output collections.

use crate::extraction_artifact::ExtractionArtifactV1;
use crate::incremental::ParseReuse;
use crate::parsed_extraction::{ParsedExtractionArtifactV1, ParsedExtractionResetReason};
use crate::types::ExtractionResult;

use std::time::Instant;
use tree_sitter::Node as TreeSitterNode;

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
        | "TypeScript" | "typescript" | "tsx" | "TSX" => "web",
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

/// Bounded source-size label. Exact byte length is never a metric key.
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

#[inline(always)]
fn observing() -> bool {
    tracing::level_enabled!(tracing::Level::TRACE)
}

/// Count one file operation (`parse` or `traverse`) under the closed family
/// and byte-bucket vocabularies. Every label comes from a bounded table, so
/// series cannot grow with path, dialect, or exact file size.
fn record_file_dims(operation: &'static str, language: &str, source_bytes: usize) {
    let family = language_family(language);
    let bucket = file_byte_bucket(source_bytes);
}

/// Accumulate one file operation's inclusive time into the closed family
/// vocabulary. Parallel workers overlap, so this is aggregate service demand,
/// not wall time; the span totals remain the timing authority. On the batch
/// traverse path this includes the nested parse, mirroring the inclusive
/// `traverse_file` span; on the retained path it is pure walk.
fn record_family_nanos(operation: &'static str, language: &str, nanos: f64) {
    let family = language_family(language);
}

/// Closed per-file parse outcome recorded by [`measure_parse_file`].
#[derive(Clone, Copy, Debug)]
pub(crate) enum ParseFileOutcome {
    /// Tree-sitter produced a tree. `has_syntax_errors` marks recovered
    /// grammar errors, which force downstream full re-extraction.
    Parsed {
        root_children: usize,
        has_syntax_errors: bool,
    },
    /// The cooperative parse deadline elapsed before a tree was produced.
    TimedOut,
    /// Tree-sitter returned no tree for a non-deadline reason.
    NoTree,
}

impl ParseFileOutcome {
    /// Classify a successful parse from its root node. Both reads are O(1);
    /// no per-node walk happens here.
    pub(crate) fn from_parsed_root(root: TreeSitterNode<'_>) -> Self {
        Self::Parsed {
            root_children: root.named_child_count(),
            has_syntax_errors: root.has_error(),
        }
    }
}

/// Time one file parse. `outcome` classifies the result into the closed
/// [`ParseFileOutcome`] vocabulary so failures and syntax-error trees are
/// counted, never silently folded into success totals.
#[inline]
pub(crate) fn measure_parse_file<T>(
    language: &str,
    source_bytes: usize,
    f: impl FnOnce() -> T,
    outcome: impl FnOnce(&T) -> ParseFileOutcome,
) -> T {
    if !observing() {
        return f();
    }
    {
        record_file_dims("parse", language, source_bytes);
        let started = Instant::now();
        let result = {
            let _span = tracing::trace_span!("code_extraction.parse_file").entered();
            f()
        };
        record_family_nanos("parse", language, started.elapsed().as_nanos() as f64);
        match outcome(&result) {
            ParseFileOutcome::Parsed {
                root_children,
                has_syntax_errors,
            } => if has_syntax_errors {},
            ParseFileOutcome::TimedOut => {}
            ParseFileOutcome::NoTree => {}
        }
        result
    }
}

/// Time one file extract and record graph-output counts.
#[inline]
pub(crate) fn measure_extract_file<T>(
    language: &str,
    source_bytes: usize,
    f: impl FnOnce() -> T,
    counts: impl FnOnce(&T) -> ExtractOutputCounts,
) -> T {
    if !observing() {
        return f();
    }
    {
        record_file_dims("traverse", language, source_bytes);
        let started = Instant::now();
        let result = {
            let _span = tracing::trace_span!("code_extraction.traverse_file").entered();
            f()
        };
        record_family_nanos("traverse", language, started.elapsed().as_nanos() as f64);
        let counts = counts(&result);

        result
    }
}

/// Time the Markdown composite-grammar fallback without recursively recording
/// another full-file traversal.
#[inline]
pub(crate) fn measure_markdown_composite_fallback<T>(f: impl FnOnce() -> T) -> T {
    {
        {
            let _span =
                tracing::trace_span!("code_extraction.markdown_composite_fallback").entered();
            f()
        }
    }
}

/// Time grammar acquisition and language-specific source prep (masking).
#[inline]
pub(crate) fn measure_language<T>(f: impl FnOnce() -> T) -> T {
    {
        {
            let _span = tracing::trace_span!("code_extraction.language").entered();
            f()
        }
    }
}

/// Time one file-level AST walk. Per-node visitors stay unmeasured.
#[inline]
pub(crate) fn measure_query<T>(f: impl FnOnce() -> T) -> T {
    {
        {
            let _span = tracing::trace_span!("code_extraction.query").entered();
            f()
        }
    }
}

/// Time file-level graph emit / canonicalize. Not a per-token emit.
#[inline]
pub(crate) fn measure_emit<T>(f: impl FnOnce() -> T) -> T {
    {
        {
            let _span = tracing::trace_span!("code_extraction.emit").entered();
            f()
        }
    }
}

/// Time the one-time grammar table construction (every enabled tier's
/// tree-sitter `Language` conversion). This serial cost is paid once per
/// process by whichever worker first touches the table; without its own
/// label it hides inside one outlier `code_extraction.language` sample.
#[inline]
pub(crate) fn measure_grammar_table_init<T>(f: impl FnOnce() -> T) -> T {
    {
        {
            let _span = tracing::trace_span!("code_extraction.grammar_table_init").entered();
            f()
        }
    }
}

/// Time the post-parse changed-range collection and extraction-range
/// expansion (bounded tree walks that scope incremental re-extraction).
/// Runs once per edit batch, never per node.
#[inline]
pub(crate) fn measure_change_ranges<T>(f: impl FnOnce() -> T) -> T {
    {
        {
            let _span = tracing::trace_span!("code_extraction.change_ranges").entered();
            f()
        }
    }
}

/// Count a grammar-table lookup that found no bundled grammar.
#[inline]
pub(crate) fn record_grammar_lookup_miss() {
    {}
}

/// Count a bundled grammar that Tree-sitter's `set_language` rejected.
#[inline]
pub(crate) fn record_grammar_rejected() {
    {}
}

/// Count a registry dispatch that found no extractor for the file extension.
#[inline]
pub(crate) fn record_dispatch_no_extractor() {
    {}
}

/// Attribute retained-parser tree reuse. A reset- or initial-dominated mix
/// means the incremental machinery is paying full reparses at scale.
#[inline]
pub(crate) fn record_retained_parse_reuse(reuse: ParseReuse) {
    {
        match reuse {
            ParseReuse::Initial => {}
            ParseReuse::Incremental => {}
            ParseReuse::Noop => {}
            ParseReuse::Reset { .. } => {}
        }
    }
}

/// Closed reasons the retained parse pipeline refused work before or instead
/// of running Tree-sitter.
#[derive(Clone, Copy, Debug)]
pub(crate) enum RetainedParseAbstention {
    SourceTooLarge,
    PreparedSourceMismatch,
    InvalidEdit,
    IdentityMismatch,
    StaleReport,
}

/// Count one retained-pipeline abstention. Refused work is recorded with the
/// same weight as performed work so admission waste stays visible.
#[inline]
pub(crate) fn record_retained_parse_abstention(reason: RetainedParseAbstention) {
    {
        match reason {
            RetainedParseAbstention::SourceTooLarge => {}
            RetainedParseAbstention::PreparedSourceMismatch => {}
            RetainedParseAbstention::InvalidEdit => {}
            RetainedParseAbstention::IdentityMismatch => {}
            RetainedParseAbstention::StaleReport => {}
        };
    }
}

/// Count one incremental-extraction reset: the changed-region fast path
/// abstained and the document was fully re-extracted for the given closed
/// reason. Composite-grammar fallbacks additionally keep their existing
/// dedicated counter.
#[inline]
pub(crate) fn record_extraction_reset(reason: ParsedExtractionResetReason) {
    {
        match reason {
            ParsedExtractionResetReason::ChangedRootIdentity => {}
            ParsedExtractionResetReason::CompositeGrammar => {}
            ParsedExtractionResetReason::FullReplacement => {}
            ParsedExtractionResetReason::LanguageChanged => {}
            ParsedExtractionResetReason::MissingPriorExtraction => {}
            ParsedExtractionResetReason::MultilineEdit => {}
            ParsedExtractionResetReason::PartialParse => {}
        };
    }
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
        assert_eq!(language_family("TSX"), "web");
        assert_eq!(language_family("JavaScript"), "web");
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
}
