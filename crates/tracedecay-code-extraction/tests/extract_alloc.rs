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
use std::time::Instant;

use tracedecay_code_extraction::incremental::{ParseChangedRange, ParsePoint};
use tracedecay_code_extraction::parsed_extraction::{ParsedExtraction, ParsedExtractionScope};
use tracedecay_code_extraction::{
    LanguageExtractor, PythonExtractor, RustExtractor, TypeScriptExtractor, ts_provider,
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
