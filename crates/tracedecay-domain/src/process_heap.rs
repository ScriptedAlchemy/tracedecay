//! The process allocator's release calls and per-thread heap collection.
//!
//! The binary that selects the Rust global allocator installs its release
//! calls once. Long-lived threads anywhere in the process (index workers, async
//! runtime workers, store workers) return their heap through the same calls,
//! so the lowest crate every thread owner depends on holds them.

use std::cell::Cell;
use std::num::NonZeroUsize;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use crate::research::release_thread_canonical_scratch;

/// The Rust global allocator's release calls, installed once by the
/// composition root that chose the allocator.
#[derive(Clone, Copy, Debug)]
pub struct ProcessAllocatorReleaseV1 {
    /// Return every pool worker's and the calling thread's freed pages.
    pub release: fn(),
    /// Return the calling thread's freed pages, including blocks other
    /// threads freed into its heap. A thread-caching allocator hands those
    /// back only when their owning thread next allocates or collects, which
    /// an idle pool worker never does.
    pub collect_calling_thread: fn(),
    /// Dedicated heaps for resident owners, when the allocator has them.
    pub owner_heaps: Option<OwnerHeapCallsV1>,
}

/// An allocator's first-class heap calls. Heap handles are opaque here; only
/// the installing allocator interprets them.
#[derive(Clone, Copy, Debug)]
pub struct OwnerHeapCallsV1 {
    /// A new, empty heap, or `None` when the allocator could not create one.
    pub create: fn() -> Option<NonZeroUsize>,
    /// Make `heap` the calling thread's allocation heap and return the one it
    /// replaces.
    pub enter: fn(NonZeroUsize) -> usize,
    /// Restore the calling thread's allocation heap that `enter` returned.
    pub leave: fn(usize),
    /// Collect the calling thread's part of `heap` and return the bytes of
    /// the pages `heap` occupies. Only the thread that allocated into the heap
    /// may call it, and only once it stopped allocating there.
    pub footprint: fn(NonZeroUsize) -> u64,
    /// Free `heap`'s empty pages and move any block still live to the
    /// process heap.
    pub delete: fn(NonZeroUsize),
}

/// A heap of its own for one resident owner's long-lived state.
///
/// Built inside [`Self::scope`], an owner's pages hold nothing another owner
/// or request allocated, so [`Self::resident_bytes`] is what the owner keeps
/// resident, fragmentation included, and dropping the heap after the owner
/// returns its pages whole. Without an allocator that has heaps there is no
/// owner heap and owners allocate from the process heap.
#[derive(Debug)]
pub struct OwnerHeapV1 {
    heap: NonZeroUsize,
    calls: OwnerHeapCallsV1,
}

impl OwnerHeapV1 {
    #[must_use]
    pub fn new() -> Option<Self> {
        let calls = PROCESS_ALLOCATOR_RELEASE_V1.get()?.owner_heaps?;
        Some(Self {
            heap: (calls.create)()?,
            calls,
        })
    }

    /// Run `build` with every allocation on this thread going to this heap.
    pub fn scope<R>(&self, build: impl FnOnce() -> R) -> R {
        struct Leave(fn(usize), usize);
        impl Drop for Leave {
            fn drop(&mut self) {
                (self.0)(self.1);
            }
        }
        let _leave = Leave(self.calls.leave, (self.calls.enter)(self.heap));
        build()
    }

    /// Bytes of the pages this heap occupies. Call on the thread that ran
    /// [`Self::scope`], after it returned.
    #[must_use]
    pub fn resident_bytes(&self) -> u64 {
        (self.calls.footprint)(self.heap)
    }
}

impl Drop for OwnerHeapV1 {
    fn drop(&mut self) {
        (self.calls.delete)(self.heap);
    }
}

static PROCESS_ALLOCATOR_RELEASE_V1: OnceLock<ProcessAllocatorReleaseV1> = OnceLock::new();

/// Install the calls that return the process allocator's freed pages to the
/// kernel. The binary that selects a global allocator installs them at
/// startup; a second installation is refused.
pub fn install_process_allocator_release_v1(
    release: ProcessAllocatorReleaseV1,
) -> Result<(), String> {
    PROCESS_ALLOCATOR_RELEASE_V1
        .set(release)
        .map_err(|_| "the process allocator release is already installed".to_owned())
}

/// The installed release calls, when the binary installed any.
#[must_use]
pub fn installed_process_allocator_release_v1() -> Option<ProcessAllocatorReleaseV1> {
    PROCESS_ALLOCATOR_RELEASE_V1.get().copied()
}

/// Return the calling thread's pooled serialization scratch and freed
/// allocator pages; the allocator collection is a no-op without an installed
/// release.
pub fn collect_calling_thread_allocator_v1() {
    release_thread_canonical_scratch();
    if let Some(release) = PROCESS_ALLOCATOR_RELEASE_V1.get() {
        (release.collect_calling_thread)();
    }
}

/// Advanced by the daemon's resident-memory sampler; see
/// [`collect_idle_thread_heap_v1`].
static IDLE_THREAD_COLLECTION_EPOCH_V1: AtomicU64 = AtomicU64::new(0);

/// Longest a long-lived thread blocked on its work channel waits before it
/// checks for a requested collection: the daemon's sampling cadence, so an
/// idle thread's freed heap is returned within two samples.
pub const IDLE_THREAD_COLLECTION_WAIT_V1: Duration = Duration::from_secs(30);

thread_local! {
    static COLLECTED_IDLE_EPOCH_V1: Cell<u64> = const { Cell::new(0) };
}

/// Ask every long-lived thread to return its heap the next time it goes idle.
pub fn request_idle_thread_collection_v1() {
    IDLE_THREAD_COLLECTION_EPOCH_V1.fetch_add(1, Ordering::AcqRel);
}

/// Called by a long-lived thread (an async runtime worker, a store worker)
/// as it waits for work: once per requested collection it returns the heap
/// it holds, including blocks other threads freed into it since it last
/// allocated. Without this an idle thread keeps those pages until it next
/// allocates, which a mostly idle daemon's threads may never do.
pub fn collect_idle_thread_heap_v1() {
    let requested = IDLE_THREAD_COLLECTION_EPOCH_V1.load(Ordering::Acquire);
    // An exiting thread has already dropped its epoch and returns its heap
    // on exit anyway.
    let behind = COLLECTED_IDLE_EPOCH_V1
        .try_with(|collected| collected.replace(requested) != requested)
        .unwrap_or(false);
    if behind {
        collect_calling_thread_allocator_v1();
    }
}
