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
    use std::cell::Cell;
    use std::ffi::{c_int, c_void};
    use std::num::NonZeroUsize;
    use std::sync::{Mutex, PoisonError};

    use rusqlite::ffi::{SQLITE_CONFIG_MALLOC, SQLITE_OK, sqlite3_config, sqlite3_mem_methods};
    use tracedecay_code_index::parallelism::run_on_every_installed_worker;
    use tracedecay_domain::process_heap::{
        OwnerHeapCallsV1, ProcessAllocatorReleaseV1, install_process_allocator_release_v1,
    };
    #[cfg(windows)]
    use windows_sys::Win32::System::{
        ProcessStatus::{PSAPI_WORKING_SET_EX_INFORMATION, QueryWorkingSetEx},
        Threading::GetCurrentProcess,
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
        fn mi_theap_collect(theap: *mut c_void, force: bool);
        // 3.3.2 declares `mi_theap_set_default` without defining it; this is
        // the definition its allocation path reads the default theap from.
        // libmimalloc-sys compiles mimalloc as C++ for MSVC targets, so this
        // internal (non-`extern "C"`) function carries its MSVC C++ name.
        #[cfg_attr(
            target_env = "msvc",
            link_name = "?_mi_theap_default_set@@YAXPEAUmi_theap_s@@@Z"
        )]
        fn _mi_theap_default_set(theap: *mut c_void);
        fn _mi_is_main_thread() -> bool;
        fn mi_thread_done();
        // `src/prim/unix/prim.c`: stores `theap` in the pthread key whose
        // destructor (`mi_pthread_done`) calls `_mi_thread_done` on it.
        fn _mi_prim_thread_associate_default_theap(theap: *mut c_void);
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

    /// Held while an owner heap is deleted and while a thread that has a theap
    /// of an owner heap frees its theaps. In 3.3.2 `mi_heap_delete` detaches
    /// a theap from its heap while its exiting thread is still collecting it,
    /// and that thread then dereferences the detached heap.
    static OWNER_HEAP_TEARDOWN: Mutex<()> = Mutex::new(());

    /// Frees the calling thread's theaps under [`OWNER_HEAP_TEARDOWN`]. Their
    /// pages stay in their heaps, abandoned, so blocks stay valid and
    /// charged; a later allocation on the thread re-initializes it. The main
    /// thread's theaps outlive the process's exit.
    fn free_thread_theaps() {
        let _teardown = OWNER_HEAP_TEARDOWN
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        // SAFETY: the thread is exiting and past its last owner heap scope.
        unsafe {
            if !_mi_is_main_thread() {
                mi_thread_done();
                // `mi_thread_done` frees the thread's main theap but leaves
                // the pthread key (`_mi_heap_default_key`) pointing at it:
                // it resets the default to `_mi_theap_empty`, which is not
                // initialized, so the key is never re-associated. glibc runs
                // TLS destructors (this one) before pthread key destructors,
                // so `mi_pthread_done` would then dereference the freed
                // theap. Hand it NULL instead, which it ignores.
                _mi_prim_thread_associate_default_theap(std::ptr::null_mut());
            }
        }
    }

    /// Frees the exiting thread's theaps through [`free_thread_theaps`].
    /// Rust thread-local destructors run before the pthread key destructor
    /// that would otherwise free them unserialized; that one then finds the
    /// thread already done.
    struct OwnerHeapThreadExit;

    impl Drop for OwnerHeapThreadExit {
        fn drop(&mut self) {
            free_thread_theaps();
        }
    }

    thread_local! {
        static OWNER_HEAP_THREAD_EXIT: OwnerHeapThreadExit = const { OwnerHeapThreadExit };
        /// Owner heap scopes open on the thread. Has no destructor, so it
        /// stays readable while the thread runs its thread-local destructors.
        static OWNER_HEAP_SCOPES: Cell<usize> = const { Cell::new(0) };
    }

    /// Called before the calling thread gets a theap of an owner heap.
    /// False once the thread's [`OwnerHeapThreadExit`] dropped, on a thread
    /// running its later thread-local destructors.
    fn serialize_thread_exit() -> bool {
        OWNER_HEAP_THREAD_EXIT.try_with(|_| {}).is_ok()
    }

    /// Frees the theaps a thread got after its [`OwnerHeapThreadExit`]
    /// dropped, which the pthread key destructor would free unserialized,
    /// once no owner heap scope on it still uses them.
    fn free_late_theaps(serialized: bool) {
        if !serialized && OWNER_HEAP_SCOPES.get() == 0 {
            free_thread_theaps();
        }
    }

    fn heap_enter(heap: NonZeroUsize) -> usize {
        serialize_thread_exit();
        OWNER_HEAP_SCOPES.set(OWNER_HEAP_SCOPES.get() + 1);
        // SAFETY: `heap` came from `mi_heap_new` and is not deleted while an
        // owner heap scope runs. v3 heaps allocate from any thread through
        // that thread's theap, which `mi_heap_theap` creates on first use.
        unsafe {
            let previous = mi_theap_get_default();
            _mi_theap_default_set(mi_heap_theap(heap.get() as *mut c_void));
            previous as usize
        }
    }

    /// Collects the owner heap's theap of the calling thread before leaving
    /// it. Each thread owns its own pages of a shared heap, and frees other
    /// threads push onto them are folded in, and emptied pages returned, only
    /// by that thread: a worker that never enters the heap again would keep
    /// them, and the owner would be charged for them, indefinitely.
    fn heap_leave(previous: usize) {
        // SAFETY: the calling thread's default theap is the owner heap's
        // theap that `heap_enter` installed on this thread, and `previous`
        // is the theap it replaced.
        unsafe {
            mi_theap_collect(mi_theap_get_default(), false);
            _mi_theap_default_set(previous as *mut c_void);
        }
        OWNER_HEAP_SCOPES.set(OWNER_HEAP_SCOPES.get() - 1);
        free_late_theaps(serialize_thread_exit());
    }

    /// Granule both residency queries report in.
    const OS_PAGE_BYTES: usize = 4096;
    /// OS pages queried per call, sized for a stack buffer: the visitor runs
    /// inside the heap walk and must not allocate.
    const RESIDENCY_BATCH_PAGES: usize = 512;

    /// Resident bytes of `[start, start + len)`, `start` OS-page aligned.
    /// Counts a batch the kernel could not report as resident: the range is
    /// a live page of this process, so the query fails only when the kernel
    /// cannot allocate its own bookkeeping, and an owner must not read as
    /// smaller than it is.
    #[cfg(unix)]
    fn resident_page_bytes(start: usize, len: usize) -> u64 {
        let mut resident = 0_u64;
        let mut vector = [0_u8; RESIDENCY_BATCH_PAGES];
        let mut offset = 0;
        while offset < len {
            let pages = (len - offset)
                .div_ceil(OS_PAGE_BYTES)
                .min(RESIDENCY_BATCH_PAGES);
            // SAFETY: the range lies in a live mimalloc page mapping and
            // `vector` holds one byte per OS page of it.
            let status = unsafe {
                libc::mincore(
                    (start + offset) as *mut c_void,
                    pages * OS_PAGE_BYTES,
                    vector.as_mut_ptr().cast(),
                )
            };
            let present = if status == 0 {
                vector[..pages]
                    .iter()
                    .filter(|page| **page & 1 != 0)
                    .count()
            } else {
                pages
            };
            resident += (present * OS_PAGE_BYTES) as u64;
            offset += pages * OS_PAGE_BYTES;
        }
        resident
    }

    /// Resident bytes of `[start, start + len)`, `start` OS-page aligned.
    /// Counts a batch the kernel could not report as resident: the range is
    /// a live page of this process, so an owner must not read as smaller than
    /// it is.
    #[cfg(windows)]
    fn resident_page_bytes(start: usize, len: usize) -> u64 {
        let mut resident = 0_u64;
        let mut entries = [PSAPI_WORKING_SET_EX_INFORMATION::default(); RESIDENCY_BATCH_PAGES];
        let mut offset = 0;
        while offset < len {
            let pages = (len - offset)
                .div_ceil(OS_PAGE_BYTES)
                .min(RESIDENCY_BATCH_PAGES);
            for (index, entry) in entries[..pages].iter_mut().enumerate() {
                entry.VirtualAddress = (start + offset + index * OS_PAGE_BYTES) as *mut c_void;
            }
            // SAFETY: `entries` holds `pages` initialized requests for this
            // process's own addresses.
            let queried = unsafe {
                QueryWorkingSetEx(
                    GetCurrentProcess(),
                    entries.as_mut_ptr().cast(),
                    (pages * size_of::<PSAPI_WORKING_SET_EX_INFORMATION>()) as u32,
                )
            };
            let present = if queried != 0 {
                entries[..pages]
                    .iter()
                    // SAFETY: every bit pattern of the union is a valid `usize`.
                    .filter(|entry| unsafe { entry.VirtualAttributes.Flags } & 1 != 0)
                    .count()
            } else {
                pages
            };
            resident += (present * OS_PAGE_BYTES) as u64;
            offset += pages * OS_PAGE_BYTES;
        }
        resident
    }

    /// Adds the resident bytes of one page's whole extent. A page reused
    /// from freed, not yet purged arena memory keeps that memory resident
    /// past the blocks it has extended to, so the page's block capacity
    /// undercounts what the owner holds.
    unsafe extern "C" fn add_resident(
        _heap: *const c_void,
        area: *const HeapArea,
        _block: *mut c_void,
        _block_size: usize,
        total: *mut c_void,
    ) -> bool {
        // SAFETY: mimalloc passes a valid area per page, and `total` is the
        // `u64` `heap_footprint` lends for the visit.
        unsafe {
            let area = &*area;
            let blocks = area.blocks as usize;
            let start = blocks & !(OS_PAGE_BYTES - 1);
            let total = &mut *total.cast::<u64>();
            *total =
                total.saturating_add(resident_page_bytes(start, blocks + area.reserved - start));
        }
        true
    }

    fn heap_footprint(heap: NonZeroUsize) -> u64 {
        let serialized = serialize_thread_exit();
        let heap = heap.get() as *mut c_void;
        let mut total = 0_u64;
        // SAFETY: the owner calls this once every thread that allocated into
        // `heap` stopped, which is the visit's no-allocator requirement: the
        // visit walks the heap's pages in every arena, and other threads only
        // push frees onto those pages atomically.
        unsafe {
            mi_heap_collect(heap, true);
            mi_heap_visit_blocks(heap, false, add_resident, (&raw mut total).cast::<c_void>());
        }
        free_late_theaps(serialized);
        total
    }

    fn heap_delete(heap: NonZeroUsize) {
        let _teardown = OWNER_HEAP_TEARDOWN
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        // SAFETY: the owner heap is deleted once, when its owner dropped, and
        // no thread frees its theaps meanwhile; `mi_heap_delete` frees its
        // empty pages and moves live blocks to the main heap, so anything
        // that escaped the owner stays valid.
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

        /// Pages of an owner heap whose blocks another thread freed stay
        /// charged to the worker that allocated them until that worker
        /// collects them, which it does whenever it leaves the heap.
        #[test]
        fn leaving_an_owner_heap_returns_the_pages_other_threads_emptied() {
            const SMALL_PAGE_BYTES: u64 = 64 * 1024;
            super::install();
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
            super::install();
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
            super::install();
            for _ in 0..WORKERS {
                let heap =
                    std::sync::Arc::new(OwnerHeapV1::new().expect("mimalloc provides owner heaps"));
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
                    let (outer_blocks, inner_blocks) =
                        outer.scope(|| (blocks(), inner.scope(blocks)));
                    let _ = self.built.send((outer_blocks, inner_blocks, inner));
                }
            }
            thread_local! {
                static LATE: std::cell::OnceCell<Late> = const { std::cell::OnceCell::new() };
            }
            super::install();
            let outer =
                std::sync::Arc::new(OwnerHeapV1::new().expect("mimalloc provides owner heaps"));
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
            super::install();
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
