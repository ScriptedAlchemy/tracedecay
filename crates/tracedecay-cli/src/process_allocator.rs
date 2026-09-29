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
    use std::ffi::c_void;
    use std::num::NonZeroUsize;

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
        // SAFETY: the owner calls this on the thread that allocated into
        // `heap` once it stopped, which is the visit's single-allocator
        // requirement; other threads only push frees onto pages atomically.
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
        use tracedecay_domain::process_heap::OwnerHeapV1;

        const BLOCKS: usize = 16 * 1024;
        const BLOCK_BYTES: usize = 256;

        fn blocks() -> Vec<Box<[u8; BLOCK_BYTES]>> {
            (0..BLOCKS).map(|_| Box::new([7; BLOCK_BYTES])).collect()
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
