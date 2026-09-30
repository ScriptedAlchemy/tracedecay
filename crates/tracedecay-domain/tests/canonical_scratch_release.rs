//! A thread's pooled canonical serialization buffers stay allocated between
//! calls until the thread releases them.
//!
//! Live bytes are counted by this binary's global allocator, process-wide, so
//! it holds a single test: no other test may allocate while it measures.

use std::alloc::{GlobalAlloc, Layout, System};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicIsize, Ordering};

use serde::Serialize;
use tracedecay_domain::research::{canonical_sha256, release_thread_canonical_scratch};

struct CountingAllocator;

static LIVE: AtomicIsize = AtomicIsize::new(0);

fn signed(bytes: usize) -> isize {
    isize::try_from(bytes).expect("allocation size")
}

// SAFETY: every call forwards to `System` with the caller's layout and only
// adjusts a counter.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        LIVE.fetch_add(signed(layout.size()), Ordering::SeqCst);
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        LIVE.fetch_sub(signed(layout.size()), Ordering::SeqCst);
        unsafe { System.dealloc(pointer, layout) }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        LIVE.fetch_add(signed(size) - signed(layout.size()), Ordering::SeqCst);
        unsafe { System.realloc(pointer, layout, size) }
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
    let released = LIVE.load(Ordering::SeqCst);

    digest_a_row();
    let pooled = LIVE.load(Ordering::SeqCst) - released;
    assert!(
        pooled > DIGEST_SINK_BYTES,
        "a digesting thread keeps its digest sink and object buffers: {pooled}"
    );

    release_thread_canonical_scratch();
    assert_eq!(LIVE.load(Ordering::SeqCst), released);

    digest_a_row();
    assert_eq!(LIVE.load(Ordering::SeqCst) - released, pooled);
}
