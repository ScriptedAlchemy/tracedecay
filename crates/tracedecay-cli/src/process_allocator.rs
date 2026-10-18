//! The shipped (mimalloc) binary's allocator release and C library routing.
//!
//! The `mimalloc` crate keeps its FFI private, so the calls used here are
//! declared against the statically linked library.

#[cfg(all(feature = "alloc-mimalloc", not(feature = "alloc-jemalloc")))]
pub(crate) mod mimalloc_v3 {
    use std::ffi::{c_int, c_long, c_void};

    use tracedecay_code_index::parallelism::run_on_every_installed_worker;
    use tracedecay_domain::process_heap::{
        ProcessAllocatorReleaseV1, install_process_allocator_release_v1,
    };
    use tracedecay_rusqlite_runtime::{SqliteAllocatorMethodsV1, install_sqlite_allocator};

    unsafe extern "C" {
        fn mi_malloc(size: usize) -> *mut c_void;
        fn mi_calloc(count: usize, size: usize) -> *mut c_void;
        fn mi_realloc(block: *mut c_void, size: usize) -> *mut c_void;
        fn mi_free(block: *mut c_void);
        fn mi_usable_size(block: *const c_void) -> usize;
        fn mi_good_size(size: usize) -> usize;
        fn mi_collect(force: bool);
        fn mi_option_set(option: c_int, value: c_long);
    }

    /// `mi_option_purge_decommits` in vendored mimalloc 3.3.2.
    const OPTION_PURGE_DECOMMITS: c_int = 5;
    /// `mi_option_purge_delay` in vendored mimalloc 3.3.2.
    const OPTION_PURGE_DELAY: c_int = 15;
    const SQLITE_OK: c_int = 0;

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
            tree_sitter::set_allocator(Some(tree_sitter::Allocator {
                malloc: mi_malloc,
                calloc: mi_calloc,
                realloc: mi_realloc,
                free: mi_free,
            }));
        }
        let methods = SqliteAllocatorMethodsV1 {
            malloc: sqlite_malloc,
            free: sqlite_free,
            realloc: sqlite_realloc,
            size: sqlite_size,
            roundup: sqlite_roundup,
            init: sqlite_init,
            shutdown: sqlite_shutdown,
        };
        // SAFETY: these callbacks form one mimalloc method table, and startup
        // calls this before any SQLite connection exists.
        if let Err(status) = unsafe { install_sqlite_allocator(methods) } {
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

    /// First-class heaps stay unused: in mimalloc 3.3.2 `mi_heap_delete`
    /// frees every thread's part of the heap while those threads' pages still
    /// name it, so a block the building thread frees afterwards dereferences
    /// the freed part (#4152).
    pub(crate) fn install() {
        if let Err(message) = install_process_allocator_release_v1(ProcessAllocatorReleaseV1 {
            release,
            collect_calling_thread: collect,
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
