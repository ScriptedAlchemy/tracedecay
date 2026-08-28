//! File-operation Hotpath instrumentation for language parse and extract.
//!
//! Spans stay at one file per measurement. Individual AST nodes are never
//! timed. Static counters use a closed family vocabulary and byte-size buckets
//! so cardinality cannot grow with path, language dialect, or exact file size.
//! Failed, timed-out, and abstained work is recorded through the same closed
//! vocabularies so success-only totals cannot hide waste. Per-family
//! `*_nanos` gauges accumulate inclusive aggregate service demand (parallel
//! workers overlap); the span totals remain the timing authority.
//! The feature-off path calls the underlying operation directly and does not
//! derive dimensions or count output collections.

use crate::extraction_artifact::ExtractionArtifactV1;
use crate::incremental::ParseReuse;
use crate::parsed_extraction::{ParsedExtractionArtifactV1, ParsedExtractionResetReason};
use crate::types::ExtractionResult;
#[cfg(feature = "hotpath")]
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
#[cfg(any(feature = "hotpath", test))]
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

/// Bounded source-size label. Exact byte length is never a Hotpath key.
#[cfg(any(feature = "hotpath", test))]
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

#[cfg(feature = "hotpath")]
fn record_parse_dims(language: &str, source_bytes: usize) {
    hotpath::gauge!("code_extraction.parse_calls").inc(1.0);
    hotpath::gauge!("code_extraction.parse_bytes").inc(source_bytes as f64);
    match language_family(language) {
        "systems" => hotpath::gauge!("code_extraction.parse_calls.systems").inc(1.0),
        "jvm" => hotpath::gauge!("code_extraction.parse_calls.jvm").inc(1.0),
        "dotnet" => hotpath::gauge!("code_extraction.parse_calls.dotnet").inc(1.0),
        "web" => hotpath::gauge!("code_extraction.parse_calls.web").inc(1.0),
        "python" => hotpath::gauge!("code_extraction.parse_calls.python").inc(1.0),
        "go" => hotpath::gauge!("code_extraction.parse_calls.go").inc(1.0),
        "managed" => hotpath::gauge!("code_extraction.parse_calls.managed").inc(1.0),
        "scripting" => hotpath::gauge!("code_extraction.parse_calls.scripting").inc(1.0),
        "functional" => hotpath::gauge!("code_extraction.parse_calls.functional").inc(1.0),
        "data" => hotpath::gauge!("code_extraction.parse_calls.data").inc(1.0),
        "markup" => hotpath::gauge!("code_extraction.parse_calls.markup").inc(1.0),
        "shader" => hotpath::gauge!("code_extraction.parse_calls.shader").inc(1.0),
        "basic" => hotpath::gauge!("code_extraction.parse_calls.basic").inc(1.0),
        "spec" => hotpath::gauge!("code_extraction.parse_calls.spec").inc(1.0),
        _ => hotpath::gauge!("code_extraction.parse_calls.other").inc(1.0),
    };
    match file_byte_bucket(source_bytes) {
        "le_1kib" => hotpath::gauge!("code_extraction.parse_calls.le_1kib").inc(1.0),
        "le_4kib" => hotpath::gauge!("code_extraction.parse_calls.le_4kib").inc(1.0),
        "le_16kib" => hotpath::gauge!("code_extraction.parse_calls.le_16kib").inc(1.0),
        "le_64kib" => hotpath::gauge!("code_extraction.parse_calls.le_64kib").inc(1.0),
        "le_256kib" => hotpath::gauge!("code_extraction.parse_calls.le_256kib").inc(1.0),
        "le_1mib" => hotpath::gauge!("code_extraction.parse_calls.le_1mib").inc(1.0),
        "le_2mib" => hotpath::gauge!("code_extraction.parse_calls.le_2mib").inc(1.0),
        _ => hotpath::gauge!("code_extraction.parse_calls.gt_2mib").inc(1.0),
    };
}

#[cfg(feature = "hotpath")]
fn record_traverse_dims(language: &str, source_bytes: usize) {
    hotpath::gauge!("code_extraction.traverse_calls").inc(1.0);
    hotpath::gauge!("code_extraction.traverse_bytes").inc(source_bytes as f64);
    match language_family(language) {
        "systems" => hotpath::gauge!("code_extraction.traverse_calls.systems").inc(1.0),
        "jvm" => hotpath::gauge!("code_extraction.traverse_calls.jvm").inc(1.0),
        "dotnet" => hotpath::gauge!("code_extraction.traverse_calls.dotnet").inc(1.0),
        "web" => hotpath::gauge!("code_extraction.traverse_calls.web").inc(1.0),
        "python" => hotpath::gauge!("code_extraction.traverse_calls.python").inc(1.0),
        "go" => hotpath::gauge!("code_extraction.traverse_calls.go").inc(1.0),
        "managed" => hotpath::gauge!("code_extraction.traverse_calls.managed").inc(1.0),
        "scripting" => hotpath::gauge!("code_extraction.traverse_calls.scripting").inc(1.0),
        "functional" => hotpath::gauge!("code_extraction.traverse_calls.functional").inc(1.0),
        "data" => hotpath::gauge!("code_extraction.traverse_calls.data").inc(1.0),
        "markup" => hotpath::gauge!("code_extraction.traverse_calls.markup").inc(1.0),
        "shader" => hotpath::gauge!("code_extraction.traverse_calls.shader").inc(1.0),
        "basic" => hotpath::gauge!("code_extraction.traverse_calls.basic").inc(1.0),
        "spec" => hotpath::gauge!("code_extraction.traverse_calls.spec").inc(1.0),
        _ => hotpath::gauge!("code_extraction.traverse_calls.other").inc(1.0),
    };
    match file_byte_bucket(source_bytes) {
        "le_1kib" => hotpath::gauge!("code_extraction.traverse_calls.le_1kib").inc(1.0),
        "le_4kib" => hotpath::gauge!("code_extraction.traverse_calls.le_4kib").inc(1.0),
        "le_16kib" => hotpath::gauge!("code_extraction.traverse_calls.le_16kib").inc(1.0),
        "le_64kib" => hotpath::gauge!("code_extraction.traverse_calls.le_64kib").inc(1.0),
        "le_256kib" => hotpath::gauge!("code_extraction.traverse_calls.le_256kib").inc(1.0),
        "le_1mib" => hotpath::gauge!("code_extraction.traverse_calls.le_1mib").inc(1.0),
        "le_2mib" => hotpath::gauge!("code_extraction.traverse_calls.le_2mib").inc(1.0),
        _ => hotpath::gauge!("code_extraction.traverse_calls.gt_2mib").inc(1.0),
    };
}

/// Accumulate one file's parse time into the closed family vocabulary.
/// Values are inclusive aggregate service demand, not wall time.
#[cfg(feature = "hotpath")]
fn record_parse_family_nanos(language: &str, nanos: f64) {
    match language_family(language) {
        "systems" => hotpath::gauge!("code_extraction.parse_nanos.systems").inc(nanos),
        "jvm" => hotpath::gauge!("code_extraction.parse_nanos.jvm").inc(nanos),
        "dotnet" => hotpath::gauge!("code_extraction.parse_nanos.dotnet").inc(nanos),
        "web" => hotpath::gauge!("code_extraction.parse_nanos.web").inc(nanos),
        "python" => hotpath::gauge!("code_extraction.parse_nanos.python").inc(nanos),
        "go" => hotpath::gauge!("code_extraction.parse_nanos.go").inc(nanos),
        "managed" => hotpath::gauge!("code_extraction.parse_nanos.managed").inc(nanos),
        "scripting" => hotpath::gauge!("code_extraction.parse_nanos.scripting").inc(nanos),
        "functional" => hotpath::gauge!("code_extraction.parse_nanos.functional").inc(nanos),
        "data" => hotpath::gauge!("code_extraction.parse_nanos.data").inc(nanos),
        "markup" => hotpath::gauge!("code_extraction.parse_nanos.markup").inc(nanos),
        "shader" => hotpath::gauge!("code_extraction.parse_nanos.shader").inc(nanos),
        "basic" => hotpath::gauge!("code_extraction.parse_nanos.basic").inc(nanos),
        "spec" => hotpath::gauge!("code_extraction.parse_nanos.spec").inc(nanos),
        _ => hotpath::gauge!("code_extraction.parse_nanos.other").inc(nanos),
    };
}

/// Accumulate one file's extract (symbol-walk) time into the closed family
/// vocabulary. On the batch path this includes the nested parse, mirroring
/// the inclusive `traverse_file` span; on the retained path it is pure walk.
#[cfg(feature = "hotpath")]
fn record_traverse_family_nanos(language: &str, nanos: f64) {
    match language_family(language) {
        "systems" => hotpath::gauge!("code_extraction.traverse_nanos.systems").inc(nanos),
        "jvm" => hotpath::gauge!("code_extraction.traverse_nanos.jvm").inc(nanos),
        "dotnet" => hotpath::gauge!("code_extraction.traverse_nanos.dotnet").inc(nanos),
        "web" => hotpath::gauge!("code_extraction.traverse_nanos.web").inc(nanos),
        "python" => hotpath::gauge!("code_extraction.traverse_nanos.python").inc(nanos),
        "go" => hotpath::gauge!("code_extraction.traverse_nanos.go").inc(nanos),
        "managed" => hotpath::gauge!("code_extraction.traverse_nanos.managed").inc(nanos),
        "scripting" => hotpath::gauge!("code_extraction.traverse_nanos.scripting").inc(nanos),
        "functional" => hotpath::gauge!("code_extraction.traverse_nanos.functional").inc(nanos),
        "data" => hotpath::gauge!("code_extraction.traverse_nanos.data").inc(nanos),
        "markup" => hotpath::gauge!("code_extraction.traverse_nanos.markup").inc(nanos),
        "shader" => hotpath::gauge!("code_extraction.traverse_nanos.shader").inc(nanos),
        "basic" => hotpath::gauge!("code_extraction.traverse_nanos.basic").inc(nanos),
        "spec" => hotpath::gauge!("code_extraction.traverse_nanos.spec").inc(nanos),
        _ => hotpath::gauge!("code_extraction.traverse_nanos.other").inc(nanos),
    };
}

/// Closed per-file parse outcome recorded by [`measure_parse_file`].
#[derive(Clone, Copy, Debug)]
pub(crate) enum ParseFileOutcome {
    /// Tree-sitter produced a tree. `has_syntax_errors` marks recovered
    /// grammar errors, which force downstream full re-extraction.
    // Feature-off, classification closures are compiled but never invoked,
    // so only the feature-on recorder reads these fields.
    #[cfg_attr(not(feature = "hotpath"), allow(dead_code))]
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
    #[cfg(feature = "hotpath")]
    {
        record_parse_dims(language, source_bytes);
        let started = Instant::now();
        let result = hotpath::measure_block!("code_extraction.parse_file", f());
        record_parse_family_nanos(language, started.elapsed().as_nanos() as f64);
        match outcome(&result) {
            ParseFileOutcome::Parsed {
                root_children,
                has_syntax_errors,
            } => {
                hotpath::gauge!("code_extraction.parse.root_children").inc(root_children as f64);
                if has_syntax_errors {
                    hotpath::gauge!("code_extraction.parse.syntax_error_trees").inc(1.0);
                }
            }
            ParseFileOutcome::TimedOut => {
                hotpath::gauge!("code_extraction.parse_failures").inc(1.0);
                hotpath::gauge!("code_extraction.parse_failures.timeout").inc(1.0);
            }
            ParseFileOutcome::NoTree => {
                hotpath::gauge!("code_extraction.parse_failures").inc(1.0);
                hotpath::gauge!("code_extraction.parse_failures.no_tree").inc(1.0);
            }
        }
        result
    }
    #[cfg(not(feature = "hotpath"))]
    {
        let _ = (language, source_bytes, outcome);
        f()
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
    #[cfg(feature = "hotpath")]
    {
        record_traverse_dims(language, source_bytes);
        let started = Instant::now();
        let result = hotpath::measure_block!("code_extraction.traverse_file", f());
        record_traverse_family_nanos(language, started.elapsed().as_nanos() as f64);
        let counts = counts(&result);
        hotpath::gauge!("code_extraction.extract.nodes").inc(counts.nodes as f64);
        hotpath::gauge!("code_extraction.extract.edges").inc(counts.edges as f64);
        hotpath::gauge!("code_extraction.extract.unresolved_refs")
            .inc(counts.unresolved_refs as f64);
        hotpath::gauge!("code_extraction.extract.imports").inc(counts.imports as f64);
        result
    }
    #[cfg(not(feature = "hotpath"))]
    {
        let _ = (language, source_bytes, counts);
        f()
    }
}

/// Time the Markdown composite-grammar fallback without recursively recording
/// another full-file traversal.
#[inline]
pub(crate) fn measure_markdown_composite_fallback<T>(f: impl FnOnce() -> T) -> T {
    #[cfg(feature = "hotpath")]
    {
        hotpath::gauge!("code_extraction.markdown_composite_fallback_calls").inc(1.0);
        hotpath::measure_block!("code_extraction.markdown_composite_fallback", f())
    }
    #[cfg(not(feature = "hotpath"))]
    {
        f()
    }
}

/// Time grammar acquisition and language-specific source prep (masking).
#[inline]
pub(crate) fn measure_language<T>(f: impl FnOnce() -> T) -> T {
    #[cfg(feature = "hotpath")]
    {
        hotpath::measure_block!("code_extraction.language", f())
    }
    #[cfg(not(feature = "hotpath"))]
    {
        f()
    }
}

/// Time one file-level AST walk. Per-node visitors stay unmeasured.
#[inline]
pub(crate) fn measure_query<T>(f: impl FnOnce() -> T) -> T {
    #[cfg(feature = "hotpath")]
    {
        hotpath::measure_block!("code_extraction.query", f())
    }
    #[cfg(not(feature = "hotpath"))]
    {
        f()
    }
}

/// Time file-level graph emit / canonicalize. Not a per-token emit.
#[inline]
pub(crate) fn measure_emit<T>(f: impl FnOnce() -> T) -> T {
    #[cfg(feature = "hotpath")]
    {
        hotpath::measure_block!("code_extraction.emit", f())
    }
    #[cfg(not(feature = "hotpath"))]
    {
        f()
    }
}

/// Time the one-time grammar table construction (every enabled tier's
/// tree-sitter `Language` conversion). This serial cost is paid once per
/// process by whichever worker first touches the table; without its own
/// label it hides inside one outlier `code_extraction.language` sample.
#[inline]
pub(crate) fn measure_grammar_table_init<T>(f: impl FnOnce() -> T) -> T {
    #[cfg(feature = "hotpath")]
    {
        hotpath::measure_block!("code_extraction.grammar_table_init", f())
    }
    #[cfg(not(feature = "hotpath"))]
    {
        f()
    }
}

/// Time the post-parse changed-range collection and extraction-range
/// expansion (bounded tree walks that scope incremental re-extraction).
/// Runs once per edit batch, never per node.
#[inline]
pub(crate) fn measure_change_ranges<T>(f: impl FnOnce() -> T) -> T {
    #[cfg(feature = "hotpath")]
    {
        hotpath::measure_block!("code_extraction.change_ranges", f())
    }
    #[cfg(not(feature = "hotpath"))]
    {
        f()
    }
}

/// Count a grammar-table lookup that found no bundled grammar.
#[inline]
pub(crate) fn record_grammar_lookup_miss() {
    #[cfg(feature = "hotpath")]
    {
        hotpath::gauge!("code_extraction.grammar.lookup_miss").inc(1.0);
    }
}

/// Count a bundled grammar that Tree-sitter's `set_language` rejected.
#[inline]
pub(crate) fn record_grammar_rejected() {
    #[cfg(feature = "hotpath")]
    {
        hotpath::gauge!("code_extraction.grammar.rejected").inc(1.0);
    }
}

/// Count a registry dispatch that found no extractor for the file extension.
#[inline]
pub(crate) fn record_dispatch_no_extractor() {
    #[cfg(feature = "hotpath")]
    {
        hotpath::gauge!("code_extraction.dispatch.no_extractor").inc(1.0);
    }
}

/// Attribute retained-parser tree reuse. A reset- or initial-dominated mix
/// means the incremental machinery is paying full reparses at scale.
#[inline]
pub(crate) fn record_retained_parse_reuse(reuse: ParseReuse) {
    #[cfg(feature = "hotpath")]
    {
        match reuse {
            ParseReuse::Initial => {
                hotpath::gauge!("code_extraction.retained.parse.initial").inc(1.0);
            }
            ParseReuse::Incremental => {
                hotpath::gauge!("code_extraction.retained.parse.incremental").inc(1.0);
            }
            ParseReuse::Noop => {
                hotpath::gauge!("code_extraction.retained.parse.noop").inc(1.0);
            }
            ParseReuse::Reset { .. } => {
                hotpath::gauge!("code_extraction.retained.parse.reset").inc(1.0);
            }
        }
    }
    #[cfg(not(feature = "hotpath"))]
    {
        let _ = reuse;
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
    #[cfg(feature = "hotpath")]
    {
        hotpath::gauge!("code_extraction.retained.abstentions").inc(1.0);
        match reason {
            RetainedParseAbstention::SourceTooLarge => {
                hotpath::gauge!("code_extraction.retained.abstain.source_too_large").inc(1.0);
            }
            RetainedParseAbstention::PreparedSourceMismatch => {
                hotpath::gauge!("code_extraction.retained.abstain.prepared_source_mismatch")
                    .inc(1.0);
            }
            RetainedParseAbstention::InvalidEdit => {
                hotpath::gauge!("code_extraction.retained.abstain.invalid_edit").inc(1.0);
            }
            RetainedParseAbstention::IdentityMismatch => {
                hotpath::gauge!("code_extraction.retained.abstain.identity_mismatch").inc(1.0);
            }
            RetainedParseAbstention::StaleReport => {
                hotpath::gauge!("code_extraction.retained.abstain.stale_report").inc(1.0);
            }
        };
    }
    #[cfg(not(feature = "hotpath"))]
    {
        let _ = reason;
    }
}

/// Count one incremental-extraction reset: the changed-region fast path
/// abstained and the document was fully re-extracted for the given closed
/// reason. Composite-grammar fallbacks additionally keep their existing
/// dedicated counter.
#[inline]
pub(crate) fn record_extraction_reset(reason: ParsedExtractionResetReason) {
    #[cfg(feature = "hotpath")]
    {
        hotpath::gauge!("code_extraction.extract.resets").inc(1.0);
        match reason {
            ParsedExtractionResetReason::ChangedRootIdentity => {
                hotpath::gauge!("code_extraction.extract.reset.changed_root_identity").inc(1.0);
            }
            ParsedExtractionResetReason::CompositeGrammar => {
                hotpath::gauge!("code_extraction.extract.reset.composite_grammar").inc(1.0);
            }
            ParsedExtractionResetReason::FullReplacement => {
                hotpath::gauge!("code_extraction.extract.reset.full_replacement").inc(1.0);
            }
            ParsedExtractionResetReason::LanguageChanged => {
                hotpath::gauge!("code_extraction.extract.reset.language_changed").inc(1.0);
            }
            ParsedExtractionResetReason::MissingPriorExtraction => {
                hotpath::gauge!("code_extraction.extract.reset.missing_prior_extraction").inc(1.0);
            }
            ParsedExtractionResetReason::MultilineEdit => {
                hotpath::gauge!("code_extraction.extract.reset.multiline_edit").inc(1.0);
            }
            ParsedExtractionResetReason::PartialParse => {
                hotpath::gauge!("code_extraction.extract.reset.partial_parse").inc(1.0);
            }
        };
    }
    #[cfg(not(feature = "hotpath"))]
    {
        let _ = reason;
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

    #[test]
    fn file_byte_bucket_never_uses_exact_size() {
        assert_ne!(file_byte_bucket(12345), "12345");
        assert_eq!(file_byte_bucket(12345), "le_16kib");
    }
}
