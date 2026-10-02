//! Per-thread allocation accounting for the lib test binary.
//!
//! Tests run concurrently in one process, so a process-wide counter would
//! charge one test for another's allocations. Each thread counts its own
//! live bytes instead; a measurement is only meaningful for work that runs
//! entirely on the measuring thread.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

thread_local! {
    static LIVE: Cell<isize> = const { Cell::new(0) };
}

struct ThreadCountingAllocator;

fn charge(bytes: isize) {
    let _ = LIVE.try_with(|live| live.set(live.get() + bytes));
}

// SAFETY: every call delegates to `System` with the caller's layout; the
// accounting touches only a const-initialized thread-local, which never
// allocates.
unsafe impl GlobalAlloc for ThreadCountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: forwarded unchanged.
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            charge(layout.size().cast_signed());
        }
        pointer
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: forwarded unchanged.
        let pointer = unsafe { System.alloc_zeroed(layout) };
        if !pointer.is_null() {
            charge(layout.size().cast_signed());
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        // SAFETY: forwarded unchanged.
        unsafe { System.dealloc(pointer, layout) };
        charge(-layout.size().cast_signed());
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: forwarded unchanged.
        let resized = unsafe { System.realloc(pointer, layout, new_size) };
        if !resized.is_null() {
            charge(new_size.cast_signed() - layout.size().cast_signed());
        }
        resized
    }
}

#[global_allocator]
static ALLOCATOR: ThreadCountingAllocator = ThreadCountingAllocator;

/// Runs `work` and returns its result with the bytes it left live on this
/// thread.
pub(crate) fn live_after<R>(work: impl FnOnce() -> R) -> (R, isize) {
    let start = LIVE.with(Cell::get);
    let result = work();
    (result, LIVE.with(Cell::get) - start)
}
