//! Dedicated heaps charge their owners' blocks and return their pages.
//!
//! These tests share one binary because none of them routes a C library's
//! allocator: `route_c_libraries` swaps SQLite's and tree-sitter's
//! allocators globally, so any binary that calls it must own its process —
//! see `c_library_allocator` and `retained_parse_heap`. Unlike those two,
//! nothing here reads the process's RSS, so it runs on every platform the
//! owner heaps ship on.

#![cfg(not(feature = "alloc-jemalloc"))]

// This binary never routes a C library's allocator, so the routing calls
// below stay unused here; the binaries that own their process use them.
#[allow(dead_code)]
#[path = "../src/process_allocator.rs"]
mod process_allocator;

use process_allocator::mimalloc_v3;

#[global_allocator]
static MIMALLOC: mimalloc::MiMalloc = mimalloc::MiMalloc;

use tracedecay_code_extraction::LanguageRegistry;
use tracedecay_code_extraction::incremental::ParseDocumentIdentity;
use tracedecay_code_index::retained_parse::SharedRetainedParsePool;
use tracedecay_domain::RepositoryDirtyStateV1;
use tracedecay_domain::process_heap::OwnerHeapV1;
use tracedecay_domain::source_path_policy::IndexPathPolicyV1;
use tracedecay_domain::test_fixtures::id;

const BLOCKS: usize = 16 * 1024;
const BLOCK_BYTES: usize = 256;

fn blocks() -> Vec<Vec<u8>> {
    (0..BLOCKS).map(|_| vec![7; BLOCK_BYTES]).collect()
}

/// An owner heap holds exactly what its scope allocated: its pages
/// cover the owner's blocks, nothing allocated outside the scope, and
/// none once the owner dropped. Blocks that outlive the heap stay
/// valid in the process heap.
#[test]
fn an_owner_heap_charges_its_own_pages_and_returns_them_whole() {
    mimalloc_v3::install();
    let heap = OwnerHeapV1::new().expect("mimalloc provides owner heaps");
    let outside = blocks();
    assert_eq!(heap.resident_bytes(), 0);

    let owned = heap.scope(blocks);
    let charged = heap.resident_bytes();
    assert!(
        (BLOCKS * BLOCK_BYTES) as u64 <= charged && charged <= (2 * BLOCKS * BLOCK_BYTES) as u64,
        "the heap charges its {BLOCKS} blocks of {BLOCK_BYTES} B: {charged}"
    );

    drop(owned);
    assert_eq!(heap.resident_bytes(), 0);

    let escaped = heap.scope(|| Box::new([9_u8; BLOCK_BYTES]));
    drop(heap);
    assert_eq!(escaped[BLOCK_BYTES - 1], 9);
    assert_eq!(outside.len(), BLOCKS);
}

/// Pages of an owner heap whose blocks another thread freed stay
/// charged to the worker that allocated them until that worker
/// collects them, which it does whenever it leaves the heap.
#[test]
fn leaving_an_owner_heap_returns_the_pages_other_threads_emptied() {
    const SMALL_PAGE_BYTES: u64 = 64 * 1024;
    mimalloc_v3::install();
    let heap = OwnerHeapV1::new().expect("mimalloc provides owner heaps");
    let heap = &heap;
    let (to_worker, work) = std::sync::mpsc::channel::<bool>();
    let (to_main, built) = std::sync::mpsc::channel::<Vec<Vec<u8>>>();
    std::thread::scope(|threads| {
        threads.spawn(move || {
            for allocate in work {
                let blocks = heap.scope(|| if allocate { blocks() } else { Vec::new() });
                to_main.send(blocks).expect("the test receives");
            }
        });
        to_worker.send(true).expect("the worker runs");
        drop(built.recv().expect("the worker's blocks"));
        let stranded = heap.resident_bytes();
        to_worker.send(false).expect("the worker runs");
        assert!(built.recv().expect("an empty scope").is_empty());
        let returned = heap.resident_bytes();
        drop(to_worker);
        assert!(
            stranded >= SMALL_PAGE_BYTES,
            "freed on another thread, blocks stay on the worker's pages: {stranded} B"
        );
        assert_eq!(
            returned, 0,
            "leaving the heap returns the pages they emptied"
        );
    });
}

/// An owner heap can be dropped while a thread that allocated into it
/// is still exiting: the thread's teardown of its part of the heap
/// and the heap's deletion never interleave, and blocks the thread
/// built stay valid after both.
#[test]
fn an_owner_heap_drops_safely_while_its_worker_exits() {
    struct Exiting(std::sync::mpsc::Sender<()>);
    impl Drop for Exiting {
        fn drop(&mut self) {
            let _ = self.0.send(());
        }
    }
    thread_local! {
        static EXITING: std::cell::OnceCell<Exiting> = const { std::cell::OnceCell::new() };
    }
    const WORKERS: usize = 32;
    mimalloc_v3::install();
    for _ in 0..WORKERS {
        let heap = OwnerHeapV1::new().expect("mimalloc provides owner heaps");
        let (exiting, exited) = std::sync::mpsc::channel();
        let (to_main, built) = std::sync::mpsc::channel::<Vec<Vec<u8>>>();
        let worker = std::thread::scope(|threads| {
            threads.spawn(|| {
                let blocks = heap.scope(blocks);
                EXITING.with(|cell| {
                    let _ = cell.set(Exiting(exiting));
                });
                to_main.send(blocks).expect("the test receives");
            });
            built.recv().expect("the worker's blocks")
        });
        exited.recv().expect("the worker begins exiting");
        drop(heap);
        assert!(
            worker
                .iter()
                .all(|block| block.iter().all(|byte| *byte == 7)),
            "blocks built in the heap stay valid after it dropped"
        );
    }
}

/// An owner heap entered by a thread-local destructor that runs after
/// the thread finished its owner heap teardown can be dropped while
/// that thread exits, and the blocks the destructor built stay valid.
#[test]
fn an_owner_heap_entered_by_a_late_thread_destructor_drops_safely() {
    struct Late {
        heap: Option<std::sync::Arc<OwnerHeapV1>>,
        built: std::sync::mpsc::Sender<Vec<Vec<u8>>>,
    }
    impl Drop for Late {
        fn drop(&mut self) {
            let heap = self.heap.take().expect("set once");
            let built = heap.scope(blocks);
            drop(heap);
            let _ = self.built.send(built);
        }
    }
    thread_local! {
        static LATE: std::cell::OnceCell<Late> = const { std::cell::OnceCell::new() };
    }
    const WORKERS: usize = 32;
    mimalloc_v3::install();
    for _ in 0..WORKERS {
        let heap = std::sync::Arc::new(OwnerHeapV1::new().expect("mimalloc provides owner heaps"));
        let (built, late_blocks) = std::sync::mpsc::channel::<Vec<Vec<u8>>>();
        let worker = std::thread::spawn({
            let heap = std::sync::Arc::clone(&heap);
            move || {
                let entered = std::sync::Arc::clone(&heap);
                LATE.with(|cell| {
                    let _ = cell.set(Late {
                        heap: Some(heap),
                        built,
                    });
                });
                drop(entered.scope(blocks));
            }
        });
        let late = late_blocks.recv().expect("the late destructor's blocks");
        drop(heap);
        worker.join().expect("the worker exits");
        assert!(
            late.iter().all(|block| block.iter().all(|byte| *byte == 7)),
            "blocks a late destructor built in the heap stay valid after it dropped"
        );
    }
}

/// A thread-local destructor that runs after the thread's owner heap
/// teardown still builds in the heaps it enters, nested or not: each
/// heap charges the blocks it holds once that thread exited.
#[test]
fn a_late_thread_destructor_charges_its_blocks_to_its_owner_heaps() {
    type Built = (Vec<Vec<u8>>, Vec<Vec<u8>>, OwnerHeapV1);
    struct Late {
        heaps: Option<(std::sync::Arc<OwnerHeapV1>, OwnerHeapV1)>,
        built: std::sync::mpsc::Sender<Built>,
    }
    impl Drop for Late {
        fn drop(&mut self) {
            let (outer, inner) = self.heaps.take().expect("set once");
            let (outer_blocks, inner_blocks) = outer.scope(|| (blocks(), inner.scope(blocks)));
            let _ = self.built.send((outer_blocks, inner_blocks, inner));
        }
    }
    thread_local! {
        static LATE: std::cell::OnceCell<Late> = const { std::cell::OnceCell::new() };
    }
    mimalloc_v3::install();
    let outer = std::sync::Arc::new(OwnerHeapV1::new().expect("mimalloc provides owner heaps"));
    let inner = OwnerHeapV1::new().expect("mimalloc provides owner heaps");
    let (built, late_blocks) = std::sync::mpsc::channel::<Built>();
    std::thread::spawn({
        let outer = std::sync::Arc::clone(&outer);
        move || {
            let entered = std::sync::Arc::clone(&outer);
            LATE.with(|cell| {
                let _ = cell.set(Late {
                    heaps: Some((outer, inner)),
                    built,
                });
            });
            drop(entered.scope(blocks));
        }
    })
    .join()
    .expect("the worker exits");
    let (outer_blocks, inner_blocks, inner) =
        late_blocks.recv().expect("the late destructor's blocks");

    for (name, heap) in [("outer", &*outer), ("inner", &inner)] {
        let charged = heap.resident_bytes();
        assert!(
            (BLOCKS * BLOCK_BYTES) as u64 <= charged
                && charged <= (2 * BLOCKS * BLOCK_BYTES) as u64,
            "the {name} heap charges its {BLOCKS} blocks of {BLOCK_BYTES} B: {charged}"
        );
    }
    drop(inner);
    drop(outer);
    assert!(
        outer_blocks
            .iter()
            .chain(&inner_blocks)
            .all(|block| block.iter().all(|byte| *byte == 7)),
        "blocks a late destructor built stay valid after their heaps dropped"
    );
}

/// A page built on freed, not yet purged memory holds that memory
/// resident past the blocks it has extended to, and its owner is
/// charged for it, never for more than its pages span.
#[test]
fn an_owner_heap_charges_the_freed_memory_its_pages_reuse() {
    const SIZE_CLASSES: usize = 64;
    const SMALL_PAGE_BYTES: u64 = 64 * 1024;
    mimalloc_v3::install();
    let freed = OwnerHeapV1::new().expect("mimalloc provides owner heaps");
    drop(freed.scope(|| {
        (0..32 * 1024)
            .map(|_| vec![7_u8; 1_000])
            .collect::<Vec<_>>()
    }));
    drop(freed);

    let heap = OwnerHeapV1::new().expect("mimalloc provides owner heaps");
    let kept = heap.scope(|| {
        (1..=SIZE_CLASSES)
            .map(|class| vec![1_u8; class * 16])
            .collect::<Vec<_>>()
    });
    let charged = heap.resident_bytes();
    let live: u64 = kept.iter().map(|block| block.len() as u64).sum();
    let spanned = SIZE_CLASSES as u64 * SMALL_PAGE_BYTES;
    assert!(
        16 * live <= charged && charged <= spanned,
        "pages holding {live} B on reused memory charge it, within the \
         {spanned} B their pages can span: {charged} B"
    );
    assert_eq!(kept.len(), SIZE_CLASSES);
}

/// Path matching keeps nothing in the matching thread's heap: the
/// glob sets' per-thread match caches outlive any one capture, and
/// left among its transient blocks they pin its pages.
#[test]
fn path_matching_leaves_no_blocks_in_the_matching_threads_heap() {
    mimalloc_v3::install();
    let policy = IndexPathPolicyV1::new(
        vec!["**/fixtures/**".to_owned(), "*.min.js".to_owned()],
        vec!["src/kept/**".to_owned()],
    )
    .expect("valid patterns");
    let probe = OwnerHeapV1::new().expect("mimalloc provides owner heaps");
    let excluded = probe.scope(|| {
        (0..2_000)
            .filter(|index| {
                policy.excludes(&format!("src/m{index}/fixtures/case{index}/f{index}.rs"))
            })
            .count()
    });
    assert_eq!(excluded, 2_000);
    assert_eq!(probe.resident_bytes(), 0);
    assert!(!policy.excludes("src/kept/fixtures/case/f.rs"));
}

/// A retained parse charges the extraction it keeps. The retained
/// artifact shares its token streams with the extraction that built
/// it, so the extraction has to allocate in the pool's heap too.
#[test]
fn a_retained_parse_charges_the_extraction_it_keeps() {
    mimalloc_v3::install();
    let source = (0..400)
        .map(|index| {
            format!(
                "pub fn f{index}(a: u32, b: u32) -> u32 {{ let c = a * {index} + b; \
                 if c > {index} {{ c - a }} else {{ b + c }} }}\n"
            )
        })
        .collect::<String>();
    let registry = LanguageRegistry::new();
    let extractor = registry
        .extractor_for_file("src/lib.rs")
        .expect("Rust extractor");
    let identity = || ParseDocumentIdentity::Repository {
        project_id: id("project.retained"),
        repository_id: id("repository.retained"),
        worktree_id: None,
        reference: None,
        commit: None,
        tree: None,
        dirty: RepositoryDirtyStateV1::Dirty,
        logical_path: "src/lib.rs".to_owned(),
    };
    let held = |pool: &SharedRetainedParsePool| {
        pool.holding()
            .and_then(|holding| holding.bytes)
            .expect("an owner-heap measurement")
    };
    let parsed = SharedRetainedParsePool::default();
    parsed.parse(identity(), "rust", &source).expect("parse");
    let extracted = SharedRetainedParsePool::default();
    let probe = OwnerHeapV1::new().expect("mimalloc provides owner heaps");
    let edited = format!("{source}pub fn edited() -> u32 {{ 3 }}\n");
    for source in [&source, &edited] {
        let (_, extraction) = probe.scope(|| {
            extracted
                .parse_and_extract_artifact(identity(), "rust", source, extractor)
                .expect("extraction")
        });
        assert!(!extraction.artifact.clone_bodies.is_empty());
    }
    let (parsed, extracted, outside) = (held(&parsed), held(&extracted), probe.resident_bytes());
    assert!(
        extracted > parsed,
        "the pool charges the artifact it keeps: {extracted} B vs {parsed} B parsed only"
    );
    assert_eq!(outside, 0, "the extracting thread keeps none of it");
}
