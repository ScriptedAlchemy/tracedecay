//! The shipped (mimalloc) binary's allocator release and owner heaps.
//!
//! The `mimalloc` crate keeps its FFI private, so the calls used here are
//! declared against the statically linked library.

#[cfg(all(feature = "alloc-mimalloc", not(feature = "alloc-jemalloc")))]
pub(crate) mod mimalloc_v3 {
    use std::cell::Cell;
    use std::ffi::{c_int, c_long, c_void};
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
        #[cfg_attr(target_env = "msvc", link_name = "?_mi_os_page_size@@YA_KXZ")]
        fn _mi_os_page_size() -> usize;
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
        #[cfg_attr(target_env = "msvc", link_name = "?_mi_is_main_thread@@YA_NXZ")]
        fn _mi_is_main_thread() -> bool;
        fn mi_thread_done();
        // `src/prim/unix/prim.c`: stores `theap` in the pthread key whose
        // destructor (`mi_pthread_done`) calls `_mi_thread_done` on it.
        #[cfg_attr(
            target_env = "msvc",
            link_name = "?_mi_prim_thread_associate_default_theap@@YAXPEAUmi_theap_s@@@Z"
        )]
        fn _mi_prim_thread_associate_default_theap(theap: *mut c_void);
        fn mi_heap_visit_blocks(
            heap: *mut c_void,
            visit_blocks: bool,
            visitor: BlockVisitor,
            arg: *mut c_void,
        ) -> bool;
        fn mi_option_get(option: c_int) -> c_long;
        fn mi_option_set(option: c_int, value: c_long);
    }

    /// `mi_option_purge_decommits` in vendored mimalloc 3.3.2.
    const OPTION_PURGE_DECOMMITS: c_int = 5;
    /// `mi_option_purge_delay` in vendored mimalloc 3.3.2.
    const OPTION_PURGE_DELAY: c_int = 15;

    /// Immediate purge so a long-lived daemon's RSS follows live data.
    ///
    /// The shipped default is a 10 ms delay, and arenas multiply that by 10.
    /// An idle ingest loop that frees a page then waits for the next sample
    /// therefore keeps the page resident. `0` purges on collect; decommit
    /// stays on so macOS uses `MADV_FREE_REUSABLE` and drops phys_footprint.
    pub(crate) fn configure_purge() {
        // SAFETY: option ids are the vendored 3.3.2 enum; both may be set
        // after the first allocation and only change later purge behavior.
        unsafe {
            mi_option_set(OPTION_PURGE_DELAY, 0);
            mi_option_set(OPTION_PURGE_DECOMMITS, 1);
        }
    }

    #[cfg(test)]
    pub(crate) fn purge_delay_ms() -> i64 {
        // SAFETY: a pure option read.
        unsafe { mi_option_get(OPTION_PURGE_DELAY) as i64 }
    }

    #[cfg(test)]
    pub(crate) fn purge_decommits() -> bool {
        // SAFETY: a pure option read.
        unsafe { mi_option_get(OPTION_PURGE_DECOMMITS) != 0 }
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

    pub(crate) fn install() {
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

    /// OS pages queried per call, sized for a stack buffer: the visitor runs
    /// inside the heap walk and must not allocate.
    const RESIDENCY_BATCH_PAGES: usize = 512;

    /// Resident bytes of `[start, start + len)`, `start` OS-page aligned.
    /// Counts a batch the kernel could not report as resident: the range is
    /// a live page of this process, so the query fails only when the kernel
    /// cannot allocate its own bookkeeping, and an owner must not read as
    /// smaller than it is.
    #[cfg(unix)]
    fn resident_page_bytes(start: usize, len: usize, page_bytes: usize) -> u64 {
        let mut resident = 0_u64;
        let mut vector = [0_u8; RESIDENCY_BATCH_PAGES];
        let mut offset = 0;
        while offset < len {
            let pages = (len - offset)
                .div_ceil(page_bytes)
                .min(RESIDENCY_BATCH_PAGES);
            // SAFETY: the range lies in a live mimalloc page mapping and
            // `vector` holds one byte per OS page of it.
            let status = unsafe {
                libc::mincore(
                    (start + offset) as *mut c_void,
                    pages * page_bytes,
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
            resident += (present * page_bytes) as u64;
            offset += pages * page_bytes;
        }
        resident
    }

    /// Resident bytes of `[start, start + len)`, `start` OS-page aligned.
    /// Counts a batch the kernel could not report as resident: the range is
    /// a live page of this process, so an owner must not read as smaller than
    /// it is.
    #[cfg(windows)]
    fn resident_page_bytes(start: usize, len: usize, page_bytes: usize) -> u64 {
        let mut resident = 0_u64;
        let mut entries = [PSAPI_WORKING_SET_EX_INFORMATION::default(); RESIDENCY_BATCH_PAGES];
        let mut offset = 0;
        while offset < len {
            let pages = (len - offset)
                .div_ceil(page_bytes)
                .min(RESIDENCY_BATCH_PAGES);
            for (index, entry) in entries[..pages].iter_mut().enumerate() {
                entry.VirtualAddress = (start + offset + index * page_bytes) as *mut c_void;
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
            resident += (present * page_bytes) as u64;
            offset += pages * page_bytes;
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
            // The allocator has initialized the native OS granule before
            // creating this heap; mincore and QueryWorkingSetEx use it too.
            let page_bytes = _mi_os_page_size();
            let start = blocks & !(page_bytes - 1);
            let total = &mut *total.cast::<u64>();
            *total = total.saturating_add(resident_page_bytes(
                start,
                blocks + area.reserved - start,
                page_bytes,
            ));
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

    // The owner-heap tests live in `tests/process_allocator_heaps.rs`: every
    // binary that calls `route_c_libraries` must own its process, so no
    // allocation-sweeping test may share one with it.
}

/// Route the C libraries to the process allocator and install its release
/// call as the runtime's allocator release. Runs once at startup, before any
/// daemon work.
pub(crate) fn configure_process_allocator() {
    #[cfg(all(feature = "alloc-mimalloc", not(feature = "alloc-jemalloc")))]
    {
        mimalloc_v3::route_c_libraries();
        mimalloc_v3::configure_purge();
        mimalloc_v3::install();
    }
}
