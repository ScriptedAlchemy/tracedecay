//! Hot-path extraction allocation budgets and signature identity.
//!
//! The Rust, TypeScript/JavaScript, and Python extract walks must not copy
//! the whole file into `ExtractionState` or own a full-item `String` just to
//! keep a small signature prefix. Each fixture here pairs a tiny item header
//! with a huge body (hundreds of KB of repeated statements), proves the
//! emitted signature strings byte-for-byte, and caps the bytes allocated
//! during the walk below the fixture size — a whole-file or whole-item copy
//! busts the budget immediately.
//!
//! This suite is its own test binary (not a `main.rs` module) because the
//! byte budget needs a counting `#[global_allocator]`, which is
//! binary-global.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::fmt::Write as _;
use std::time::Instant;

use sha2::{Digest, Sha256};
use tracedecay_code_extraction::incremental::{ParseChangedRange, ParsePoint};
use tracedecay_code_extraction::parsed_extraction::{ParsedExtraction, ParsedExtractionScope};
use tracedecay_code_extraction::{
    BashExtractor, BatchExtractor, CExtractor, CSharpExtractor, ClojureExtractor, CobolExtractor,
    CppExtractor, DartExtractor, DockerfileExtractor, ElixirExtractor, ErlangExtractor,
    FSharpExtractor, FortranExtractor, GlslExtractor, GoExtractor, GwBasicExtractor,
    HaskellExtractor, HlslExtractor, JavaExtractor, JuliaExtractor, KotlinExtractor,
    LanguageExtractor, LeanExtractor, LuaExtractor, MarkdownExtractor, MsBasic2Extractor,
    NixExtractor, ObjcExtractor, OcamlExtractor, PascalExtractor, PerlExtractor, PhpExtractor,
    PowerShellExtractor, ProtoExtractor, PythonExtractor, QBasicExtractor, QuintExtractor,
    RExtractor, RubyExtractor, RustExtractor, ScalaExtractor, SqlExtractor, SwiftExtractor,
    TomlExtractor, TypeScriptExtractor, VbNetExtractor, WgslExtractor, ZigExtractor, ts_provider,
};
use tracedecay_domain::{ExtractionResult, NodeKind};
use tree_sitter::{Parser, Tree};

/// Counts bytes handed out by the Rust allocator on the current thread.
/// Thread-local so parallel tests in this binary do not cross-count.
struct CountingAllocator;

thread_local! {
    static ALLOCATED_BYTES: Cell<usize> = const { Cell::new(0) };
}

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATED_BYTES.with(|counter| counter.set(counter.get() + layout.size()));
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        ALLOCATED_BYTES.with(|counter| counter.set(counter.get() + layout.size()));
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let grown = new_size.saturating_sub(layout.size());
        ALLOCATED_BYTES.with(|counter| counter.set(counter.get() + grown));
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL_ALLOCATOR: CountingAllocator = CountingAllocator;

/// Run `work` and return its value plus the bytes allocated while it ran.
fn measure_allocation<T>(work: impl FnOnce() -> T) -> (T, usize) {
    let before = ALLOCATED_BYTES.with(Cell::get);
    let value = work();
    let after = ALLOCATED_BYTES.with(Cell::get);
    (value, after - before)
}

/// Statement lines per hot body. Each line carries `PAD_WIDTH` bytes of
/// trailing comment so the body is huge in *bytes* while staying small in
/// node count — the memcpy waste under test scales with bytes, while the
/// legitimate walk cost (complexity traversal stack) scales with nodes.
const BODY_LINES: usize = 640;
const PAD_WIDTH: usize = 960;

/// Bytes in the single-line string-literal bodies below. Those items (a
/// TypeScript expression-bodied arrow, a Python one-line suite) contain no
/// `{` and no `=>` after the header, so the signature helpers must cut at
/// the body child; a helper that copies the whole item instead busts the
/// allocation budget and changes the pinned signature.
const INLINE_LITERAL_WIDTH: usize = 200_000;
const REGIONAL_SOURCE_BYTES: usize = 1_500_000;
const REGIONAL_BASE_BUDGET: usize = 64 * 1024;

fn push_inline_literal(source: &mut String) {
    for _ in 0..INLINE_LITERAL_WIDTH {
        source.push('x');
    }
}

fn push_padded_statement(source: &mut String, statement: &str, comment_marker: &str) {
    source.push_str(statement);
    source.push(' ');
    source.push_str(comment_marker);
    source.push(' ');
    for _ in 0..PAD_WIDTH {
        source.push('x');
    }
    source.push('\n');
}

fn regional_fixture(trailing_item: &str) -> String {
    regional_fixture_padded(" ", trailing_item)
}

/// Like [`regional_fixture`] but each padding line starts with `line_prefix`
/// (a line-comment marker for grammars whose scanner is super-linear over a
/// megabyte of bare blank space).
fn regional_fixture_padded(line_prefix: &str, trailing_item: &str) -> String {
    let padding_line = format!(
        "{line_prefix}{}\n",
        " ".repeat(PAD_WIDTH - line_prefix.len())
    );
    let mut source = String::with_capacity(REGIONAL_SOURCE_BYTES + trailing_item.len());
    while source.len() + padding_line.len() + trailing_item.len() <= REGIONAL_SOURCE_BYTES {
        source.push_str(&padding_line);
    }
    source.push_str(trailing_item);
    source
}

fn rust_fixture() -> String {
    let mut source = String::with_capacity(BODY_LINES * (PAD_WIDTH + 32) + 512);
    source.push_str("/// Hot fixture: tiny header, huge body.\n");
    source.push_str("pub fn hot_path(seed: u64) -> u64 {\n");
    source.push_str("    let mut acc = seed;\n");
    for _ in 0..BODY_LINES {
        push_padded_statement(&mut source, "    acc ^= 251;", "//");
    }
    source.push_str("    acc\n}\n\n");
    source.push_str("/// Marker struct after the hot function.\n");
    source.push_str("pub struct Marker;\n");
    source
}

fn typescript_fixture() -> String {
    let mut source = String::with_capacity(BODY_LINES * 2 * (PAD_WIDTH + 32) + 512);
    source.push_str("/** Hot fixture: tiny header, huge body. */\n");
    source.push_str("export function hotPath(seed: number): number {\n");
    source.push_str("  let acc = seed;\n");
    for _ in 0..BODY_LINES {
        push_padded_statement(&mut source, "  acc = (acc ^ 251) >>> 0;", "//");
    }
    source.push_str("  return acc;\n}\n\n");
    source.push_str("/** Arrow companion with the same huge body. */\n");
    source.push_str("export const hotArrow = (seed: number): number => {\n");
    source.push_str("  let acc = seed;\n");
    for _ in 0..BODY_LINES {
        push_padded_statement(&mut source, "  acc = (acc ^ 251) >>> 0;", "//");
    }
    source.push_str("  return acc;\n};\n\n");
    source.push_str("export const hotExpr = (seed: number): string => \"");
    push_inline_literal(&mut source);
    source.push_str("\";\n\n");
    source.push_str("export type MarkerAlias = number;\n");
    source
}

fn python_fixture() -> String {
    let mut source = String::with_capacity(BODY_LINES * (PAD_WIDTH + 32) + 512);
    source.push_str("def hot_path(seed):\n");
    source.push_str("    \"\"\"Hot fixture: tiny header, huge body.\"\"\"\n");
    source.push_str("    acc = seed\n");
    for _ in 0..BODY_LINES {
        push_padded_statement(&mut source, "    acc = (acc ^ 251) & 1023", "#");
    }
    source.push_str("    return acc\n\n\n");
    source.push_str("def hot_inline(seed): return \"");
    push_inline_literal(&mut source);
    source.push_str("\"\n\n\n");
    source.push_str("class Marker:\n");
    source.push_str("    \"\"\"Marker class after the hot function.\"\"\"\n\n");
    source.push_str("    def tiny(self):\n");
    source.push_str("        return 1\n");
    source
}

fn parse_with_grammar(key: &str, source: &str) -> Tree {
    let mut parser = Parser::new();
    parser
        .set_language(&ts_provider::language(key).expect("bundled grammar"))
        .expect("configure parser");
    parser.parse(source, None).expect("parse fixture")
}

/// (name, signature) rows for every non-File node, sorted for comparison.
fn signature_rows(result: &ExtractionResult) -> Vec<(String, Option<String>)> {
    let mut rows: Vec<(String, Option<String>)> = result
        .nodes
        .iter()
        .filter(|node| node.kind != NodeKind::File)
        .map(|node| (node.name.clone(), node.signature.clone()))
        .collect();
    rows.sort();
    rows
}

/// Digest canonical extraction rows while excluding runtime-only timing fields.
fn canonical_digest(result: &ExtractionResult) -> String {
    let mut stable = result.clone();
    stable.duration_ms = 0;
    for node in &mut stable.nodes {
        node.updated_at = 0;
    }
    let encoded = serde_json::to_vec(&stable).expect("serialize canonical extraction rows");
    Sha256::digest(encoded)
        .iter()
        .fold(String::with_capacity(64), |mut digest, byte| {
            write!(&mut digest, "{byte:02x}").expect("write digest");
            digest
        })
}

fn signature_of<'r>(result: &'r ExtractionResult, kind: NodeKind, name: &str) -> &'r str {
    result
        .nodes
        .iter()
        .find(|node| node.kind == kind && node.name == name)
        .unwrap_or_else(|| panic!("missing {name}"))
        .signature
        .as_deref()
        .unwrap_or_else(|| panic!("{name} has no signature"))
}

/// A byte range covering the small trailing item that starts at `needle`,
/// shaped as one incremental changed region.
fn trailing_region(source: &str, needle: &str) -> ParseChangedRange {
    let start_byte = source.find(needle).expect("fixture contains trailing item");
    let start_row = source[..start_byte].bytes().filter(|b| *b == b'\n').count();
    let end_row = source.bytes().filter(|b| *b == b'\n').count();
    ParseChangedRange {
        start_byte,
        end_byte: source.len(),
        start_position: ParsePoint {
            row: start_row,
            column: 0,
        },
        end_position: ParsePoint {
            row: end_row,
            column: 0,
        },
    }
}

/// Drive `extract` and `extract_parsed` (full document) on the same source,
/// assert both walks agree on every signature string, and return the parsed
/// walk plus the bytes it allocated.
fn extract_both_and_compare(
    extractor: &dyn LanguageExtractor,
    file_path: &str,
    source: &str,
    grammar_key: &str,
) -> (ParsedExtraction, usize) {
    let full = extractor.extract(file_path, source);
    assert!(full.errors.is_empty(), "extract errors: {:?}", full.errors);

    let tree = parse_with_grammar(grammar_key, source);
    let (parsed, walk_bytes) = measure_allocation(|| {
        extractor.extract_parsed(
            file_path,
            source,
            &tree,
            ParsedExtractionScope::FullDocument,
        )
    });
    // Informational timing: best of a few repeat walks smooths scheduler noise.
    let best_walk_time = (0..10)
        .map(|_| {
            let started = Instant::now();
            let repeat = extractor.extract_parsed(
                file_path,
                source,
                &tree,
                ParsedExtractionScope::FullDocument,
            );
            let elapsed = started.elapsed();
            assert!(repeat.result.errors.is_empty());
            elapsed
        })
        .min()
        .expect("at least one timed walk");
    println!(
        "{file_path}: source={} bytes, extract_parsed walk allocated {walk_bytes} bytes, best walk {best_walk_time:?}",
        source.len()
    );
    assert!(
        parsed.result.errors.is_empty(),
        "extract_parsed errors: {:?}",
        parsed.result.errors
    );
    assert_eq!(
        signature_rows(&full),
        signature_rows(&parsed.result),
        "extract and extract_parsed must emit identical signature strings"
    );
    (parsed, walk_bytes)
}

#[test]
fn rust_hot_walk_owns_only_signature_prefixes() {
    let source = rust_fixture();
    let (parsed, walk_bytes) = extract_both_and_compare(&RustExtractor, "hot.rs", &source, "rust");

    assert_eq!(
        signature_of(&parsed.result, NodeKind::Function, "hot_path"),
        "pub fn hot_path(seed: u64) -> u64"
    );
    assert_eq!(
        signature_of(&parsed.result, NodeKind::Struct, "Marker"),
        "pub struct Marker;"
    );
    let hot = parsed
        .result
        .nodes
        .iter()
        .find(|node| node.name == "hot_path")
        .expect("hot_path node");
    assert_eq!(
        hot.docstring.as_deref(),
        Some("Hot fixture: tiny header, huge body.")
    );
    assert!(!hot.is_async);

    assert!(
        walk_bytes < source.len(),
        "rust walk allocated {walk_bytes} bytes for a {} byte source; \
         it must not copy the file or whole items",
        source.len()
    );
}

#[test]
fn typescript_hot_walk_owns_only_signature_prefixes() {
    let source = typescript_fixture();
    let (parsed, walk_bytes) =
        extract_both_and_compare(&TypeScriptExtractor, "hot.ts", &source, "typescript");

    assert_eq!(
        signature_of(&parsed.result, NodeKind::Function, "hotPath"),
        "function hotPath(seed: number): number"
    );
    assert_eq!(
        signature_of(&parsed.result, NodeKind::ArrowFunction, "hotArrow"),
        "hotArrow = (seed: number): number =>"
    );
    assert_eq!(
        signature_of(&parsed.result, NodeKind::ArrowFunction, "hotExpr"),
        "hotExpr = (seed: number): string =>"
    );
    assert_eq!(
        signature_of(&parsed.result, NodeKind::TypeAlias, "MarkerAlias"),
        "type MarkerAlias = number;"
    );
    let hot = parsed
        .result
        .nodes
        .iter()
        .find(|node| node.name == "hotPath")
        .expect("hotPath node");
    assert_eq!(
        hot.docstring.as_deref(),
        Some("Hot fixture: tiny header, huge body.")
    );

    assert!(
        walk_bytes < source.len(),
        "typescript walk allocated {walk_bytes} bytes for a {} byte source; \
         it must not copy the file or whole items",
        source.len()
    );
}

#[test]
fn python_hot_walk_owns_only_signature_prefixes() {
    let source = python_fixture();
    let (parsed, walk_bytes) =
        extract_both_and_compare(&PythonExtractor, "hot.py", &source, "python");

    assert_eq!(
        signature_of(&parsed.result, NodeKind::Function, "hot_path"),
        "def hot_path(seed)"
    );
    assert_eq!(
        signature_of(&parsed.result, NodeKind::Function, "hot_inline"),
        "def hot_inline(seed)"
    );
    assert_eq!(
        signature_of(&parsed.result, NodeKind::Class, "Marker"),
        "class Marker"
    );
    assert_eq!(
        signature_of(&parsed.result, NodeKind::Method, "tiny"),
        "def tiny(self)"
    );
    let hot = parsed
        .result
        .nodes
        .iter()
        .find(|node| node.name == "hot_path")
        .expect("hot_path node");
    assert_eq!(
        hot.docstring.as_deref(),
        Some("Hot fixture: tiny header, huge body.")
    );

    assert!(
        walk_bytes < source.len(),
        "python walk allocated {walk_bytes} bytes for a {} byte source; \
         it must not copy the file or whole items",
        source.len()
    );
}

/// An incremental walk over one tiny trailing item must not pay for the huge
/// rest of the file: same signature strings, allocation far below the source
/// size.
#[test]
fn incremental_walk_of_tiny_item_pays_only_for_that_item() {
    struct Case {
        extractor: &'static dyn LanguageExtractor,
        file_path: &'static str,
        grammar_key: &'static str,
        source: String,
        needle: &'static str,
        expected_kind: NodeKind,
        expected_name: &'static str,
        expected_signature: &'static str,
    }
    let cases = [
        Case {
            extractor: &RustExtractor,
            file_path: "hot.rs",
            grammar_key: "rust",
            source: rust_fixture(),
            needle: "pub struct Marker;",
            expected_kind: NodeKind::Struct,
            expected_name: "Marker",
            expected_signature: "pub struct Marker;",
        },
        Case {
            extractor: &TypeScriptExtractor,
            file_path: "hot.ts",
            grammar_key: "typescript",
            source: typescript_fixture(),
            needle: "export type MarkerAlias",
            expected_kind: NodeKind::TypeAlias,
            expected_name: "MarkerAlias",
            expected_signature: "type MarkerAlias = number;",
        },
        Case {
            extractor: &PythonExtractor,
            file_path: "hot.py",
            grammar_key: "python",
            source: python_fixture(),
            needle: "class Marker:",
            expected_kind: NodeKind::Class,
            expected_name: "Marker",
            expected_signature: "class Marker",
        },
    ];

    for case in cases {
        let tree = parse_with_grammar(case.grammar_key, &case.source);
        let region = trailing_region(&case.source, case.needle);
        let regions = [region];
        let (parsed, walk_bytes) = measure_allocation(|| {
            case.extractor.extract_parsed(
                case.file_path,
                &case.source,
                &tree,
                ParsedExtractionScope::ChangedRegions(&regions),
            )
        });
        println!(
            "{}: incremental walk allocated {walk_bytes} bytes for a {} byte source",
            case.file_path,
            case.source.len()
        );
        assert!(
            parsed.result.errors.is_empty(),
            "{}: errors {:?}",
            case.file_path,
            parsed.result.errors
        );
        assert_eq!(
            signature_of(&parsed.result, case.expected_kind, case.expected_name),
            case.expected_signature,
            "{}",
            case.file_path
        );
        assert!(
            walk_bytes < case.source.len(),
            "{}: incremental walk allocated {walk_bytes} bytes for a {} byte source; \
             it must not copy the whole file to re-extract one tiny item",
            case.file_path,
            case.source.len()
        );
    }
}

/// Representative lite, medium, and full extractors must retain the caller's
/// source during a one-line incremental walk. The fixed allowance covers the
/// canonical file/item rows; the variable allowance scales only with bytes in
/// the selected syntax node, never with the 1–2 MiB source.
#[test]
fn representative_language_walks_allocate_by_changed_region() {
    struct Case {
        tier: &'static str,
        extractor: &'static dyn LanguageExtractor,
        file_path: &'static str,
        grammar_key: &'static str,
        source: String,
        needle: &'static str,
        expected_digest: &'static str,
    }

    let cases = [
        Case {
            tier: "lite",
            extractor: &GoExtractor,
            file_path: "region.go",
            grammar_key: "go",
            source: regional_fixture("func tiny() int { return 1 }\n"),
            needle: "func tiny",
            expected_digest: "7bc4d1d7e778bf2c40976672e63f12a7c5817dd8d8c34c28129105988dafef3c",
        },
        Case {
            tier: "medium",
            extractor: &BashExtractor,
            file_path: "region.sh",
            grammar_key: "bash",
            source: regional_fixture("tiny() { :; }\n"),
            needle: "tiny()",
            expected_digest: "0b6dc46dcc1aa39ee5e93ab7b7380922ca2299c3090679be0d9f8084d5dbbb3f",
        },
        Case {
            tier: "full",
            extractor: &HaskellExtractor,
            file_path: "Region.hs",
            grammar_key: "haskell",
            source: regional_fixture("tiny = 1\n"),
            needle: "tiny =",
            expected_digest: "f395f8ee10318640159cd719d4f3769e0927295902473e66a154238d1d4e6c14",
        },
    ];

    let mut over_budget = Vec::new();
    for case in cases {
        assert!(
            (1_000_000..=2_000_000).contains(&case.source.len()),
            "{} fixture must remain 1–2 MiB, got {} bytes",
            case.file_path,
            case.source.len()
        );
        let cold = case.extractor.extract(case.file_path, &case.source);
        assert!(
            cold.errors.is_empty(),
            "{} cold errors: {:?}",
            case.file_path,
            cold.errors
        );
        let tree = parse_with_grammar(case.grammar_key, &case.source);
        let region = trailing_region(&case.source, case.needle);
        let regions = [region];
        let (incremental, walk_bytes) = measure_allocation(|| {
            case.extractor.extract_parsed(
                case.file_path,
                &case.source,
                &tree,
                ParsedExtractionScope::ChangedRegions(&regions),
            )
        });
        assert!(
            incremental.result.errors.is_empty(),
            "{} incremental errors: {:?}",
            case.file_path,
            incremental.result.errors
        );

        let cold_digest = canonical_digest(&cold);
        let incremental_digest = canonical_digest(&incremental.result);
        println!(
            "{} ({}): source={} visited={} allocated={} digest={cold_digest}",
            case.file_path,
            case.tier,
            case.source.len(),
            incremental.metrics.visited_bytes,
            walk_bytes,
        );
        assert_eq!(
            cold_digest, incremental_digest,
            "{} cold and incremental canonical rows differ",
            case.file_path
        );
        assert_eq!(
            cold_digest, case.expected_digest,
            "{} canonical row bytes changed",
            case.file_path
        );

        let budget = REGIONAL_BASE_BUDGET + incremental.metrics.visited_bytes * 32;
        if walk_bytes > budget {
            over_budget.push(format!(
                "{} ({}) allocated {walk_bytes} bytes for a {} byte source and {} visited bytes; \
                 budget is {budget} bytes",
                case.file_path,
                case.tier,
                case.source.len(),
                incremental.metrics.visited_bytes,
            ));
        }
    }
    assert!(over_budget.is_empty(), "{}", over_budget.join("\n"));
}

/// One migrated language: extractor, fixture item, grammar, and the canonical
/// row digest pinned from the pre-cutover (owned-source) extractor so the
/// borrowed-source walk is proven byte-identical, not merely self-consistent.
struct MigratedLanguageCase {
    extractor: &'static dyn LanguageExtractor,
    file_path: &'static str,
    grammar_key: &'static str,
    /// Prefix for every padding line; `" "` is bare blank space.
    line_prefix: &'static str,
    trailing_item: &'static str,
    needle: &'static str,
    expected_digest: &'static str,
}

const MIGRATED_LANGUAGE_CASES: &[MigratedLanguageCase] = &[
    MigratedLanguageCase {
        extractor: &SqlExtractor,
        file_path: "region.sql",
        grammar_key: "sql",
        line_prefix: " ",
        trailing_item: "CREATE TABLE tiny (id INT);\n",
        needle: "tiny",
        expected_digest: "eb785bfcbe2a1c569ead6b50e9a088d29110bb48240ae61720682e1459494a89",
    },
    MigratedLanguageCase {
        extractor: &TomlExtractor,
        file_path: "region.toml",
        grammar_key: "toml",
        line_prefix: " ",
        trailing_item: "tiny = 1\n",
        needle: "tiny",
        expected_digest: "fae8928972c74e4920b3180ca177c1e0990363080a3ea90798f975bc93c73bea",
    },
    MigratedLanguageCase {
        extractor: &RExtractor,
        file_path: "region.r",
        grammar_key: "r",
        line_prefix: " ",
        trailing_item: "tiny <- function() 1\n",
        needle: "tiny",
        expected_digest: "cb77befd3f1d6c632e7bcfdca28b68ef7d01d7d46f1e94f89003ee593c5980ed",
    },
    MigratedLanguageCase {
        extractor: &LeanExtractor,
        file_path: "region.lean",
        grammar_key: "lean",
        line_prefix: " ",
        trailing_item: "def tiny : Nat := 1\n",
        needle: "tiny",
        expected_digest: "4213eefb07f0be0a1ef26542a9693a44d8ac4abd7eb1e17f6b7502a1e08b14d7",
    },
    MigratedLanguageCase {
        extractor: &QuintExtractor,
        file_path: "region.qnt",
        grammar_key: "quint",
        line_prefix: " ",
        trailing_item: "module Tiny {}\n",
        needle: "module Tiny",
        expected_digest: "75502dcf8fdb0e73b852bdc2a56404ee79da3a6ca311485429c2cf657a1b7bfd",
    },
    MigratedLanguageCase {
        extractor: &ErlangExtractor,
        file_path: "region.erl",
        grammar_key: "erlang",
        line_prefix: " ",
        trailing_item: "tiny() -> ok.\n",
        needle: "tiny",
        expected_digest: "5acd060968a2e002e322536fb9f219fae55ec838bc29621f14f3598b12a28a03",
    },
    MigratedLanguageCase {
        extractor: &JuliaExtractor,
        file_path: "region.jl",
        grammar_key: "julia",
        line_prefix: " ",
        trailing_item: "function tiny(); return 1; end\n",
        needle: "tiny",
        expected_digest: "6584016258527dff63e8ba31b502b0ee2f8bb1fed820541350e3cd2b3339630a",
    },
    MigratedLanguageCase {
        extractor: &BatchExtractor,
        file_path: "region.bat",
        grammar_key: "batch",
        line_prefix: " ",
        trailing_item: ":tiny\n",
        needle: "tiny",
        expected_digest: "93c349e85e7a377e55d2233792f9ef2aeab55a1b6261250926d3cbcc7cc8fd98",
    },
    MigratedLanguageCase {
        extractor: &PowerShellExtractor,
        file_path: "region.ps1",
        grammar_key: "powershell",
        line_prefix: " ",
        trailing_item: "function tiny { return 1 }\n",
        needle: "tiny",
        expected_digest: "78528b4d222753db5f385e5cedee33b44b9fefda4a86c74395083c3fdc683097",
    },
    MigratedLanguageCase {
        extractor: &ClojureExtractor,
        file_path: "region.clj",
        grammar_key: "clojure",
        line_prefix: " ",
        trailing_item: "(defn tiny [] 1)\n",
        needle: "tiny",
        expected_digest: "9c5888f25cf5e45228e5f828461f96e2b97b4950cee959fcaf5c3e2102cd152c",
    },
    MigratedLanguageCase {
        extractor: &MsBasic2Extractor,
        file_path: "region.bas",
        grammar_key: "msbasic2",
        line_prefix: " ",
        trailing_item: "10 LET TINY = 1\n",
        needle: "TINY",
        expected_digest: "2335ed1c3b35e63ab1c279ef286da13e402835c887a58a40bc936373b226d501",
    },
    MigratedLanguageCase {
        extractor: &WgslExtractor,
        file_path: "region.wgsl",
        grammar_key: "wgsl",
        line_prefix: "//",
        trailing_item: "fn tiny() -> i32 { return 1; }\n",
        needle: "tiny",
        expected_digest: "ce459f22ce582bb2dad9739151e524aca66a1e36ecbc6fc3b394c558c13eb94a",
    },
    MigratedLanguageCase {
        extractor: &LuaExtractor,
        file_path: "region.lua",
        grammar_key: "lua",
        line_prefix: " ",
        trailing_item: "function tiny() return 1 end\n",
        needle: "tiny",
        expected_digest: "fee0ccb554104e532c371846873749ec1d9c280f3e3dc82eca28235919ea106d",
    },
    MigratedLanguageCase {
        extractor: &FSharpExtractor,
        file_path: "region.fs",
        grammar_key: "fsharp",
        line_prefix: " ",
        trailing_item: "let tiny () = 1\n",
        needle: "tiny",
        expected_digest: "54b157be731f5eecdffc9618d983153bd0ad6ba29a667a17ec12e22c347cd8e2",
    },
    MigratedLanguageCase {
        extractor: &OcamlExtractor,
        file_path: "region.ml",
        grammar_key: "ocaml",
        line_prefix: " ",
        trailing_item: "let tiny () = 1\n",
        needle: "tiny",
        expected_digest: "c6925a1f647d3ba1012f1214d3e9716600e747a17f70eeb13ef604de4c1b234b",
    },
    MigratedLanguageCase {
        extractor: &ElixirExtractor,
        file_path: "region.ex",
        grammar_key: "elixir",
        line_prefix: " ",
        trailing_item: "defmodule Tiny do end\n",
        needle: "Tiny",
        expected_digest: "635fe5b81842415a9ad1a0abebd3292b7151f0ad35127f6aabf2ab6f22c04c89",
    },
    MigratedLanguageCase {
        extractor: &HlslExtractor,
        file_path: "region.hlsl",
        grammar_key: "hlsl",
        line_prefix: " ",
        trailing_item: "float tiny() { return 1.0; }\n",
        needle: "tiny",
        expected_digest: "794cb11d4ffbdcaea114ca7c88f8b86276832e69bf75009720e0e01f9b44c764",
    },
    MigratedLanguageCase {
        extractor: &GwBasicExtractor,
        file_path: "region.gw",
        grammar_key: "gwbasic",
        line_prefix: " ",
        trailing_item: "10 DEF FNTINY(X)=X\n",
        needle: "FNTINY",
        expected_digest: "a5bb1e3dc8a9f76d6cecc9ac987bfbdce9b0d66aaeed0182f49df0de7deae5e8",
    },
    MigratedLanguageCase {
        extractor: &DockerfileExtractor,
        file_path: "region.dockerfile",
        grammar_key: "dockerfile",
        line_prefix: " ",
        trailing_item: "FROM scratch AS tiny\n",
        needle: "tiny",
        expected_digest: "e8d3a694fa2c8cbf7746b7e9b4624645b564cab47abe30aaede8b48dff44fb2d",
    },
    MigratedLanguageCase {
        extractor: &CobolExtractor,
        file_path: "region.cob",
        grammar_key: "cobol",
        line_prefix: " ",
        trailing_item: "IDENTIFICATION DIVISION. PROGRAM-ID. TINY.\n",
        needle: "TINY",
        expected_digest: "7ae517bcbdeda6c086512c8854faf8783c4d6d8f7228cd8646ed3dcef0d4caeb",
    },
    MigratedLanguageCase {
        extractor: &CppExtractor,
        file_path: "region.cpp",
        grammar_key: "cpp",
        line_prefix: " ",
        trailing_item: "int tiny() { return 1; }\n",
        needle: "tiny",
        expected_digest: "dcae6b64462957ca9cc03c0593df76fa04111bbf23f6b96507ca55c80921fa90",
    },
    MigratedLanguageCase {
        extractor: &QBasicExtractor,
        file_path: "region.qb",
        grammar_key: "qbasic",
        line_prefix: " ",
        trailing_item: "CONST TINY = 1\n",
        needle: "TINY",
        expected_digest: "fa30286354331be598710c7880a38178778d2156898f15960d3abcc30564f296",
    },
    MigratedLanguageCase {
        extractor: &GlslExtractor,
        file_path: "region.glsl",
        grammar_key: "glsl",
        line_prefix: " ",
        trailing_item: "float tiny() { return 1.0; }\n",
        needle: "tiny",
        expected_digest: "3d6548d007779c84a810ceaef9f0561e7bc540e9646c7a35815ad1c53e87b18b",
    },
    MigratedLanguageCase {
        extractor: &RubyExtractor,
        file_path: "region.rb",
        grammar_key: "ruby",
        line_prefix: " ",
        trailing_item: "def tiny; 1; end\n",
        needle: "tiny",
        expected_digest: "4cbd5715b26ea0a8b892f6712fd03af55ae72d3d978f53719620cd7dbb4d388b",
    },
    MigratedLanguageCase {
        extractor: &PerlExtractor,
        file_path: "region.pl",
        grammar_key: "perl",
        line_prefix: " ",
        trailing_item: "sub tiny { return 1; }\n",
        needle: "tiny",
        expected_digest: "c08db2249671262e7feddc1762924811f8ffb2c2f8b4aa32113fc3b824ac7f49",
    },
    MigratedLanguageCase {
        extractor: &ProtoExtractor,
        file_path: "region.proto",
        grammar_key: "protobuf",
        line_prefix: "//",
        trailing_item: "syntax = \"proto3\";\nmessage Tiny { int32 value = 1; }\n",
        needle: "syntax",
        expected_digest: "cc6bda39ad1659dc6810e9a3102541a7eccc1dea52cd36619ab00274a012d454",
    },
    MigratedLanguageCase {
        extractor: &MarkdownExtractor,
        file_path: "region.md",
        grammar_key: "markdown",
        line_prefix: " ",
        trailing_item: "# Tiny\n",
        needle: "Tiny",
        expected_digest: "2364955171e61ad437bcd892ab0498a81ac95624200f566fd94c2181f8dce887",
    },
    MigratedLanguageCase {
        extractor: &ZigExtractor,
        file_path: "region.zig",
        grammar_key: "zig",
        line_prefix: " ",
        trailing_item: "pub fn tiny() void {}\n",
        needle: "fn tiny",
        expected_digest: "9f4475a7ef2967ce0dfc9544a9937787abef1b85d7528118067997508c843ff3",
    },
    MigratedLanguageCase {
        extractor: &FortranExtractor,
        file_path: "region.f90",
        grammar_key: "fortran",
        line_prefix: " ",
        trailing_item: "program tiny; end program tiny\n",
        needle: "program tiny",
        expected_digest: "4d49d15a9297ada3d94e14094ae7a54bb75f6593fdbab37dd1af2b6235a83cd5",
    },
    MigratedLanguageCase {
        extractor: &NixExtractor,
        file_path: "region.nix",
        grammar_key: "nix",
        line_prefix: " ",
        trailing_item: "{ tiny = x: x; }\n",
        needle: "tiny =",
        expected_digest: "faa55e95bc920f59ffc8e5721929a0c0b011f2c29c95d92917f457b834809b14",
    },
    MigratedLanguageCase {
        extractor: &PhpExtractor,
        file_path: "region.php",
        grammar_key: "php",
        line_prefix: " ",
        trailing_item: "<?php function tiny() {}\n",
        needle: "function tiny",
        expected_digest: "c802ec1e56f957fb5d5499b159ba3b1cb71fae8c66c5bb39294d5e28865e29dd",
    },
    MigratedLanguageCase {
        extractor: &ObjcExtractor,
        file_path: "region.m",
        grammar_key: "objc",
        line_prefix: " ",
        trailing_item: "int tiny(void) { return 1; }\n",
        needle: "int tiny",
        expected_digest: "80a6c3ef467e6de33e9dad8e528f20b03bbda86b5384a432935299b23ccd372f",
    },
    MigratedLanguageCase {
        extractor: &PascalExtractor,
        file_path: "region.pas",
        grammar_key: "pascal",
        line_prefix: " ",
        trailing_item: "program Tiny; begin end.\n",
        needle: "program Tiny",
        expected_digest: "f9e0cfb050a55c3236dff04a96fb1a827335e6db63ee302327992937a276fab6",
    },
    MigratedLanguageCase {
        extractor: &SwiftExtractor,
        file_path: "region.swift",
        grammar_key: "swift",
        line_prefix: " ",
        trailing_item: "func tiny() {}\n",
        needle: "func tiny",
        expected_digest: "0d17d79e9664bd14851802c65d2e025c6a6649912d004fee1fc7915347a0cee3",
    },
    MigratedLanguageCase {
        extractor: &JavaExtractor,
        file_path: "region.java",
        grammar_key: "java",
        line_prefix: " ",
        trailing_item: "public class Tiny {}\n",
        needle: "class Tiny",
        expected_digest: "60c39eaa78fefca1ea9ff6f61e4ed6c5d3df2b75003fd9d28640f91e40ac5c02",
    },
    MigratedLanguageCase {
        extractor: &CExtractor,
        file_path: "region.c",
        grammar_key: "c",
        line_prefix: " ",
        trailing_item: "int tiny(void) { return 1; }\n",
        needle: "int tiny",
        expected_digest: "25231cd58d0a20d9b43668f68abd43843e880cb54fad1a731f8a8f8038c73f4f",
    },
    MigratedLanguageCase {
        extractor: &VbNetExtractor,
        file_path: "region.vb",
        grammar_key: "vbnet",
        line_prefix: " ",
        trailing_item: "Class Tiny : End Class\n",
        needle: "Class Tiny",
        expected_digest: "38f0d0f067ee1209bd7527b4a0a37c5956a4d65dee3e4401326681b0d6f1be2c",
    },
    MigratedLanguageCase {
        extractor: &ScalaExtractor,
        file_path: "region.scala",
        grammar_key: "scala",
        line_prefix: " ",
        trailing_item: "def tiny(x: Int): Int = x + 1\n",
        needle: "def tiny",
        expected_digest: "ebc4206fe4a50338c50fe1fc6fbd7d6bdc795f211fd1e06fcd934c7dee7603c4",
    },
    MigratedLanguageCase {
        extractor: &KotlinExtractor,
        file_path: "region.kt",
        grammar_key: "kotlin",
        line_prefix: " ",
        trailing_item: "fun tiny() {}\n",
        needle: "fun tiny",
        expected_digest: "6a727abfc33f76c983c21fecd6012328a73bef1c029d9c5370cb25ce413548f3",
    },
    MigratedLanguageCase {
        extractor: &CSharpExtractor,
        file_path: "region.cs",
        grammar_key: "c_sharp",
        line_prefix: " ",
        trailing_item: "public class Tiny {}\n",
        needle: "class Tiny",
        expected_digest: "773a57c682b0f73ad0d5758fa34e881b1de7fc6ab83cccaa8dad12104bc3859c",
    },
    MigratedLanguageCase {
        extractor: &DartExtractor,
        file_path: "region.dart",
        grammar_key: "dart",
        line_prefix: " ",
        trailing_item: "void tiny() {}\n",
        needle: "void tiny",
        expected_digest: "ba74d8802688a67296de718fc5b93c5cd55c5b882af7cb845b744e9014bd0c13",
    },
];

/// Every extractor migrated to borrowed source must (a) emit canonical rows
/// byte-identical to the pre-cutover extractor (pinned digest), (b) agree
/// between cold and incremental walks, and (c) allocate by changed region,
/// not by the 1–2 MiB source, on a one-line incremental walk.
#[test]
fn every_migrated_language_walk_allocates_by_changed_region() {
    assert_eq!(
        MIGRATED_LANGUAGE_CASES.len(),
        41,
        "the migrated-language table must cover all 41 extractors"
    );
    let mut failures = Vec::new();
    for case in MIGRATED_LANGUAGE_CASES {
        let source = regional_fixture_padded(case.line_prefix, case.trailing_item);
        assert!(
            (1_000_000..=2_000_000).contains(&source.len()),
            "{} fixture must remain 1–2 MiB, got {} bytes",
            case.file_path,
            source.len()
        );
        let cold = case.extractor.extract(case.file_path, &source);
        let tree = parse_with_grammar(case.grammar_key, &source);
        let region = trailing_region(&source, case.needle);
        let regions = [region];
        let (incremental, walk_bytes) = measure_allocation(|| {
            case.extractor.extract_parsed(
                case.file_path,
                &source,
                &tree,
                ParsedExtractionScope::ChangedRegions(&regions),
            )
        });
        let cold_digest = canonical_digest(&cold);
        let incremental_digest = canonical_digest(&incremental.result);
        // Grammars that expose the whole file as one node make `visited`
        // equal the source, so the walk must also stay under half the source
        // or a full-file copy would pass on the visited allowance alone.
        let budget =
            (REGIONAL_BASE_BUDGET + incremental.metrics.visited_bytes * 32).min(source.len() / 2);
        println!(
            "{}: source={} visited={} allocated={} budget={budget} digest={cold_digest}",
            case.file_path,
            source.len(),
            incremental.metrics.visited_bytes,
            walk_bytes,
        );
        if !cold.errors.is_empty() {
            failures.push(format!("{} cold errors: {:?}", case.file_path, cold.errors));
        }
        if !incremental.result.errors.is_empty() {
            failures.push(format!(
                "{} incremental errors: {:?}",
                case.file_path, incremental.result.errors
            ));
        }
        if cold_digest != incremental_digest {
            failures.push(format!(
                "{} cold and incremental canonical rows differ",
                case.file_path
            ));
        }
        if cold_digest != case.expected_digest {
            failures.push(format!(
                "{} canonical row bytes changed: expected {} got {cold_digest}",
                case.file_path, case.expected_digest
            ));
        }
        if walk_bytes > budget {
            failures.push(format!(
                "{} allocated {walk_bytes} bytes for a {} byte source and {} visited bytes; \
                 budget is {budget} bytes",
                case.file_path,
                source.len(),
                incremental.metrics.visited_bytes,
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
