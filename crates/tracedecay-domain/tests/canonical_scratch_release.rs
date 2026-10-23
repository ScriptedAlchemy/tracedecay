//! A thread's pooled canonical serialization buffers stay allocated between
//! calls until the thread releases them.
//!
//! Live bytes are counted on the digesting thread. The libtest coordinator
//! may allocate concurrently even when this binary contains only one test.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::collections::BTreeMap;

use serde::Serialize;
use tracedecay_domain::research::{canonical_sha256, release_thread_canonical_scratch};

struct CountingAllocator;

thread_local! {
    static LIVE: Cell<isize> = const { Cell::new(0) };
}

fn adjust_live(bytes: isize) {
    // Other thread-local destructors can free memory after this counter has
    // retired. Measurements finish before thread teardown.
    let _ = LIVE.try_with(|live| live.set(live.get() + bytes));
}

fn live_bytes() -> isize {
    LIVE.with(Cell::get)
}

fn signed(bytes: usize) -> isize {
    isize::try_from(bytes).expect("allocation size")
}

// SAFETY: every call forwards to `System` with the caller's layout and only
// adjusts a counter.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            adjust_live(signed(layout.size()));
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        adjust_live(-signed(layout.size()));
        unsafe { System.dealloc(pointer, layout) }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        let replacement = unsafe { System.realloc(pointer, layout, size) };
        if !replacement.is_null() {
            adjust_live(signed(size) - signed(layout.size()));
        }
        replacement
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

const DIGEST_SINK_BYTES: isize = 64 * 1024;

#[derive(Serialize)]
struct Row {
    name: String,
    fields: BTreeMap<String, u64>,
}

fn digest_a_row() {
    let row = Row {
        name: "row".to_owned(),
        fields: (0..16)
            .map(|index| (format!("field-{index}"), index))
            .collect(),
    };
    canonical_sha256(&row).expect("canonical digest");
}

#[test]
fn a_digesting_thread_keeps_its_scratch_until_it_releases_it() {
    digest_a_row();
    release_thread_canonical_scratch();
    let released = live_bytes();

    digest_a_row();
    let pooled = live_bytes() - released;
    assert!(
        pooled > DIGEST_SINK_BYTES,
        "a digesting thread keeps its digest sink and object buffers: {pooled}"
    );

    release_thread_canonical_scratch();
    assert_eq!(live_bytes(), released);

    digest_a_row();
    assert_eq!(live_bytes() - released, pooled);
}
