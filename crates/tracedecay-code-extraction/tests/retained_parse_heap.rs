//! A retained document holds its tree and no parser.
//!
//! Tree-sitter allocates through C `malloc`, so this binary routes the
//! library's allocator through a counter before anything parses. The counter
//! is process-wide, so the binary holds a single test.

use std::alloc::Layout;
use std::ffi::c_void;
use std::sync::atomic::{AtomicUsize, Ordering};

use tracedecay_code_extraction::incremental::{
    ParseDocumentIdentity, ParseLimits, ParseReuse, RetainedParseDocument,
};
use tracedecay_code_extraction::ts_provider;
use tracedecay_domain::RepositoryDirtyStateV1;
use tracedecay_domain::test_fixtures::id;
use tree_sitter::{Parser, Tree};

const HEADER: usize = 16;
static LIVE: AtomicUsize = AtomicUsize::new(0);

fn layout(size: usize) -> Layout {
    Layout::from_size_align(size + HEADER, HEADER).expect("C allocation layout")
}

unsafe extern "C" fn counted_malloc(size: usize) -> *mut c_void {
    // SAFETY: a non-zero layout; the size header is written before use.
    unsafe {
        let base = std::alloc::alloc(layout(size));
        if base.is_null() {
            return base.cast();
        }
        base.cast::<usize>().write(size);
        LIVE.fetch_add(size, Ordering::SeqCst);
        base.add(HEADER).cast()
    }
}

unsafe extern "C" fn counted_calloc(count: usize, size: usize) -> *mut c_void {
    let Some(total) = count.checked_mul(size) else {
        return std::ptr::null_mut();
    };
    // SAFETY: `counted_malloc` returns `total` writable bytes or null.
    unsafe {
        let block = counted_malloc(total).cast::<u8>();
        if !block.is_null() {
            block.write_bytes(0, total);
        }
        block.cast()
    }
}

unsafe extern "C" fn counted_realloc(block: *mut c_void, size: usize) -> *mut c_void {
    if block.is_null() {
        // SAFETY: plain allocation.
        return unsafe { counted_malloc(size) };
    }
    // SAFETY: `block` came from `counted_malloc`, so its header precedes it.
    unsafe {
        let base = block.cast::<u8>().sub(HEADER);
        let old = base.cast::<usize>().read();
        let moved = std::alloc::realloc(base, layout(old), size + HEADER);
        if moved.is_null() {
            return moved.cast();
        }
        moved.cast::<usize>().write(size);
        LIVE.fetch_sub(old, Ordering::SeqCst);
        LIVE.fetch_add(size, Ordering::SeqCst);
        moved.add(HEADER).cast()
    }
}

unsafe extern "C" fn counted_free(block: *mut c_void) {
    if block.is_null() {
        return;
    }
    // SAFETY: `block` came from `counted_malloc`, so its header precedes it.
    unsafe {
        let base = block.cast::<u8>().sub(HEADER);
        let size = base.cast::<usize>().read();
        LIVE.fetch_sub(size, Ordering::SeqCst);
        std::alloc::dealloc(base, layout(size));
    }
}

fn identity(path: &str) -> ParseDocumentIdentity {
    ParseDocumentIdentity::Repository {
        project_id: id("project.retained-heap"),
        repository_id: id("repository.retained-heap"),
        worktree_id: None,
        reference: None,
        commit: None,
        tree: None,
        dirty: RepositoryDirtyStateV1::Dirty,
        logical_path: path.to_owned(),
    }
}

fn source(document: usize) -> String {
    // Exact allocation accounting needs a nontrivial tree, not a large source
    // whose parse deadline competes with concurrent compiler work.
    (0..8)
        .map(|item| {
            format!(
                "pub fn item_{document}_{item}(value: u64) -> u64 {{ \
                 if value > {item} {{ match value {{ 0 => 1, _ => value * {item} }} }} else {{ 0 }} }}\n"
            )
        })
        .collect()
}

fn tree_with_parser(source: &str) -> (Parser, Tree) {
    let language = ts_provider::try_language("rust").expect("rust grammar");
    let mut parser = Parser::new();
    parser.set_language(&language).expect("rust grammar loads");
    let tree = parser.parse(source, None).expect("rust source parses");
    (parser, tree)
}

#[test]
fn retained_documents_hold_their_trees_and_no_parser() {
    // SAFETY: installed before this binary's first tree-sitter allocation;
    // every call pairs with the counted functions above.
    unsafe {
        tree_sitter::set_allocator(
            Some(counted_malloc),
            Some(counted_calloc),
            Some(counted_realloc),
            Some(counted_free),
        );
    }
    let sources: Vec<String> = (0..8).map(source).collect();

    let before_trees = LIVE.load(Ordering::SeqCst);
    let (parsers, trees): (Vec<Parser>, Vec<Tree>) = sources
        .iter()
        .map(|source| tree_with_parser(source))
        .unzip();
    let parser_and_tree_bytes = LIVE.load(Ordering::SeqCst) - before_trees;
    drop(parsers);
    let trees_bytes = LIVE.load(Ordering::SeqCst) - before_trees;
    assert!(
        parser_and_tree_bytes > trees_bytes,
        "the fixture must detect the additional heap of retained parsers"
    );
    drop(trees);

    let before_documents = LIVE.load(Ordering::SeqCst);
    let mut documents: Vec<RetainedParseDocument> = sources
        .iter()
        .enumerate()
        .map(|(index, source)| {
            RetainedParseDocument::open(
                identity(&format!("src/document_{index}.rs")),
                "rust",
                source.clone(),
                ParseLimits::default(),
            )
            .expect("retained document opens")
            .0
        })
        .collect();
    let documents_bytes = LIVE.load(Ordering::SeqCst) - before_documents;
    assert_eq!(
        documents_bytes, trees_bytes,
        "retained documents hold {documents_bytes} bytes of C heap against {trees_bytes} for their trees"
    );

    let edited = sources[0].replacen("value * 0", "value * 7", 1);
    let report = documents[0]
        .reparse(identity("src/document_0.rs"), edited)
        .expect("incremental reparse");
    assert_eq!(report.reuse, ParseReuse::Incremental);
}
