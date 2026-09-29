//! The shipped (mimalloc) binary's allocator release.
//!
//! The `mimalloc` crate keeps its FFI private, so the one call used here is
//! declared against the statically linked library.

#[cfg(all(
    feature = "alloc-mimalloc",
    not(feature = "alloc-jemalloc"),
    not(feature = "hotpath-alloc")
))]
mod mimalloc_v3 {
    use tracedecay_code_index::parallelism::run_on_every_installed_worker;
    use tracedecay_domain::process_heap::{
        ProcessAllocatorReleaseV1, install_process_allocator_release_v1,
    };

    unsafe extern "C" {
        fn mi_collect(force: bool);
    }

    pub(super) fn install() {
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

/// Install the process allocator's release call as the runtime's allocator
/// release. Runs once at startup, before any daemon work.
pub(crate) fn configure_process_allocator() {
    #[cfg(all(
        feature = "alloc-mimalloc",
        not(feature = "alloc-jemalloc"),
        not(feature = "hotpath-alloc")
    ))]
    mimalloc_v3::install();
}
