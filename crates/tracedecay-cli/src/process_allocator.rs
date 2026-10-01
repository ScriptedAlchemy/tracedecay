//! The shipped (mimalloc) binary's allocator release and owner heaps.
//!
//! The `mimalloc` crate keeps its FFI private, so the calls used here are
//! declared against the statically linked library.

#[cfg(all(
    feature = "alloc-mimalloc",
    not(feature = "alloc-jemalloc"),
    not(feature = "hotpath-alloc")
))]
mod mimalloc_v3 {
    use std::ffi::{c_int, c_void};
    use std::num::NonZeroUsize;

    use rusqlite::ffi::{SQLITE_CONFIG_MALLOC, SQLITE_OK, sqlite3_config, sqlite3_mem_methods};
    use tracedecay_code_index::parallelism::run_on_every_installed_worker;
    use tracedecay_domain::process_heap::{
        OwnerHeapCallsV1, ProcessAllocatorReleaseV1, install_process_allocator_release_v1,
    };

    /// `mi_heap_area_t` from the vendored v3 `mimalloc.h`.
    #[repr(C)]
    struct HeapArea {
        blocks: *mut c_void,
        reserved: usize,
        committed: usize,
        used: usize,
        block_size: usize,
        full_block_size: usize,
        reserved1: *mut c_void,
    }

    type BlockVisitor = unsafe extern "C" fn(
        heap: *const c_void,
        area: *const HeapArea,
        block: *mut c_void,
        block_size: usize,
        arg: *mut c_void,
    ) -> bool;

    unsafe extern "C" {
        fn mi_malloc(size: usize) -> *mut c_void;
        fn mi_calloc(count: usize, size: usize) -> *mut c_void;
        fn mi_realloc(block: *mut c_void, size: usize) -> *mut c_void;
        fn mi_free(block: *mut c_void);
        fn mi_usable_size(block: *const c_void) -> usize;
        fn mi_good_size(size: usize) -> usize;
        fn mi_collect(force: bool);
        fn mi_heap_new() -> *mut c_void;
        fn mi_heap_delete(heap: *mut c_void);
        fn mi_heap_collect(heap: *mut c_void, force: bool);
        fn mi_heap_theap(heap: *mut c_void) -> *mut c_void;
        fn mi_theap_get_default() -> *mut c_void;
        // 3.3.2 declares `mi_theap_set_default` without defining it; this is
        // the definition its allocation path reads the default theap from.
        fn _mi_theap_default_set(theap: *mut c_void);
        fn mi_heap_visit_blocks(
            heap: *mut c_void,
            visit_blocks: bool,
            visitor: BlockVisitor,
            arg: *mut c_void,
        ) -> bool;
    }

    /// Point SQLite and tree-sitter at mimalloc, so the process has one heap:
    /// their pages are collected, purged and measured with Rust's instead of
    /// accumulating in glibc arenas no release reaches. Both libraries must
    /// not have allocated yet, since neither frees a block through another
    /// allocator.
    pub(super) fn route_c_libraries() {
        // SAFETY: called once at startup before any parser, tree, or query
        // exists, so every tree-sitter block is allocated and freed by
        // mimalloc; the functions are thread-safe and never unwind.
        unsafe {
            tree_sitter::set_allocator(
                Some(mi_malloc),
                Some(mi_calloc),
                Some(mi_realloc),
                Some(mi_free),
            );
        }
        let methods = sqlite3_mem_methods {
            xMalloc: Some(sqlite_malloc),
            xFree: Some(sqlite_free),
            xRealloc: Some(sqlite_realloc),
            xSize: Some(sqlite_size),
            xRoundup: Some(sqlite_roundup),
            xInit: Some(sqlite_init),
            xShutdown: Some(sqlite_shutdown),
            pAppData: std::ptr::null_mut(),
        };
        // SAFETY: SQLite copies `methods` before returning. It accepts the
        // configuration only before its first initialization and refuses it
        // with `SQLITE_MISUSE` afterwards.
        let status = unsafe { sqlite3_config(SQLITE_CONFIG_MALLOC, &raw const methods) };
        if status != SQLITE_OK {
            tracing::warn!(
                event = "sqlite_allocator_route_refused",
                status,
                "SQLite was initialized before startup routed its allocator; it keeps glibc malloc"
            );
        }
    }

    unsafe extern "C" fn sqlite_malloc(bytes: c_int) -> *mut c_void {
        usize::try_from(bytes).map_or(std::ptr::null_mut(), |bytes| {
            // SAFETY: a plain allocation; null tells SQLite it failed.
            unsafe { mi_malloc(bytes) }
        })
    }

    unsafe extern "C" fn sqlite_free(block: *mut c_void) {
        // SAFETY: SQLite frees only blocks `sqlite_malloc`/`sqlite_realloc`
        // returned, or null.
        unsafe { mi_free(block) }
    }

    unsafe extern "C" fn sqlite_realloc(block: *mut c_void, bytes: c_int) -> *mut c_void {
        usize::try_from(bytes).map_or(std::ptr::null_mut(), |bytes| {
            // SAFETY: `block` is a live mimalloc block from this table.
            unsafe { mi_realloc(block, bytes) }
        })
    }

    unsafe extern "C" fn sqlite_size(block: *mut c_void) -> c_int {
        // SAFETY: `block` is a live mimalloc block from this table. SQLite
        // needs a size at least its `c_int` request, which the clamp keeps.
        c_int::try_from(unsafe { mi_usable_size(block) }).unwrap_or(c_int::MAX)
    }

    unsafe extern "C" fn sqlite_roundup(bytes: c_int) -> c_int {
        usize::try_from(bytes)
            .ok()
            // SAFETY: a pure size-class lookup.
            .and_then(|bytes| c_int::try_from(unsafe { mi_good_size(bytes) }).ok())
            .unwrap_or(bytes)
    }

    unsafe extern "C" fn sqlite_init(_: *mut c_void) -> c_int {
        SQLITE_OK
    }

    unsafe extern "C" fn sqlite_shutdown(_: *mut c_void) {}

    pub(super) fn install() {
        if let Err(message) = install_process_allocator_release_v1(ProcessAllocatorReleaseV1 {
            release,
            collect_calling_thread: collect,
            owner_heaps: Some(OwnerHeapCallsV1 {
                create: heap_new,
                enter: heap_enter,
                leave: heap_leave,
                footprint: heap_footprint,
                delete: heap_delete,
            }),
        }) {
            tracing::warn!(event = "process_allocator_release_install_failed", %message);
        }
    }

    /// A collection frees the calling thread's empty pages and purges freed
    /// arena memory for the process; pages still owned by another thread's
    /// heap are released only on that thread. The persistent pools' workers
    /// do most allocation, so every worker collects its own heap before this
    /// returns: callers re-measure resident bytes right after.
    fn release() {
        rayon::broadcast(|_| collect());
        run_on_every_installed_worker(collect);
        collect();
    }

    fn collect() {
        // SAFETY: `mi_collect` takes no pointers; a forced collection frees
        // the calling thread's retired pages and purges freed arena memory
        // for the whole process without touching live allocations.
        unsafe { mi_collect(true) };
    }

    fn heap_new() -> Option<NonZeroUsize> {
        // SAFETY: creates an empty first-class heap; null on failure.
        NonZeroUsize::new(unsafe { mi_heap_new() } as usize)
    }

    fn heap_enter(heap: NonZeroUsize) -> usize {
        // SAFETY: `heap` came from `mi_heap_new` and is not deleted while an
        // owner heap scope runs. v3 heaps allocate from any thread through
        // that thread's theap, which `mi_heap_theap` creates on first use.
        unsafe {
            let previous = mi_theap_get_default();
            _mi_theap_default_set(mi_heap_theap(heap.get() as *mut c_void));
            previous as usize
        }
    }

    fn heap_leave(previous: usize) {
        // SAFETY: `previous` is the calling thread's default theap that
        // `heap_enter` replaced on this same thread.
        unsafe { _mi_theap_default_set(previous as *mut c_void) };
    }

    unsafe extern "C" fn add_committed(
        _heap: *const c_void,
        area: *const HeapArea,
        _block: *mut c_void,
        _block_size: usize,
        total: *mut c_void,
    ) -> bool {
        // SAFETY: mimalloc passes a valid area per page, and `total` is the
        // `u64` `heap_footprint` lends for the visit.
        unsafe {
            let total = &mut *total.cast::<u64>();
            *total = total.saturating_add((*area).committed as u64);
        }
        true
    }

    fn heap_footprint(heap: NonZeroUsize) -> u64 {
        let heap = heap.get() as *mut c_void;
        let mut total = 0_u64;
        // SAFETY: the owner calls this once every thread that allocated into
        // `heap` stopped, which is the visit's no-allocator requirement: the
        // visit walks the heap's pages in every arena, and other threads only
        // push frees onto those pages atomically.
        unsafe {
            mi_heap_collect(heap, true);
            mi_heap_visit_blocks(
                heap,
                false,
                add_committed,
                (&raw mut total).cast::<c_void>(),
            );
        }
        total
    }

    fn heap_delete(heap: NonZeroUsize) {
        // SAFETY: the owner heap is deleted once, when its owner dropped;
        // `mi_heap_delete` frees its empty pages and moves live blocks to the
        // main heap, so anything that escaped the owner stays valid.
        unsafe { mi_heap_delete(heap.get() as *mut c_void) };
    }

    #[cfg(test)]
    mod tests {
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
            super::install();
            let heap = OwnerHeapV1::new().expect("mimalloc provides owner heaps");
            let outside = blocks();
            assert_eq!(heap.resident_bytes(), 0);

            let owned = heap.scope(blocks);
            let charged = heap.resident_bytes();
            assert!(
                (BLOCKS * BLOCK_BYTES) as u64 <= charged
                    && charged <= (2 * BLOCKS * BLOCK_BYTES) as u64,
                "the heap charges its {BLOCKS} blocks of {BLOCK_BYTES} B: {charged}"
            );

            drop(owned);
            assert_eq!(heap.resident_bytes(), 0);

            let escaped = heap.scope(|| Box::new([9_u8; BLOCK_BYTES]));
            drop(heap);
            assert_eq!(escaped[BLOCK_BYTES - 1], 9);
            assert_eq!(outside.len(), BLOCKS);
        }

        /// Path matching keeps nothing in the matching thread's heap: the
        /// glob sets' per-thread match caches outlive any one capture, and
        /// left among its transient blocks they pin its pages.
        #[test]
        fn path_matching_leaves_no_blocks_in_the_matching_threads_heap() {
            super::install();
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
            super::install();
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
            let (parsed, extracted, outside) =
                (held(&parsed), held(&extracted), probe.resident_bytes());
            assert!(
                extracted > parsed,
                "the pool charges the artifact it keeps: {extracted} B vs {parsed} B parsed only"
            );
            assert_eq!(outside, 0, "the extracting thread keeps none of it");
        }
    }
}

/// Route the C libraries to the process allocator and install its release
/// call as the runtime's allocator release. Runs once at startup, before any
/// daemon work.
pub(crate) fn configure_process_allocator() {
    #[cfg(all(
        feature = "alloc-mimalloc",
        not(feature = "alloc-jemalloc"),
        not(feature = "hotpath-alloc")
    ))]
    {
        mimalloc_v3::route_c_libraries();
        mimalloc_v3::install();
    }
}
