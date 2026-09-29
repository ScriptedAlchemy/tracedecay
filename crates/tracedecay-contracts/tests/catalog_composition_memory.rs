//! Every application catalog composition in a process binds the same
//! schema-bearing snapshot. This binary counts every allocation, so a
//! composition that assembled its own copy shows up as live bytes.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicIsize, Ordering};

use tracedecay_contracts::catalog_composition::compose_application_catalog;

struct CountingAllocator;

static LIVE: AtomicIsize = AtomicIsize::new(0);

fn signed(bytes: usize) -> isize {
    isize::try_from(bytes).expect("allocation size")
}

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        LIVE.fetch_add(signed(layout.size()), Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        LIVE.fetch_sub(signed(layout.size()), Ordering::Relaxed);
        unsafe { System.dealloc(pointer, layout) }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        LIVE.fetch_add(signed(size) - signed(layout.size()), Ordering::Relaxed);
        unsafe { System.realloc(pointer, layout, size) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

#[test]
fn a_second_composition_holds_no_second_catalog() {
    let before = LIVE.load(Ordering::Relaxed);
    let first = compose_application_catalog(()).expect("first composition");
    let after_first = LIVE.load(Ordering::Relaxed);
    let second = compose_application_catalog(()).expect("second composition");
    let after_second = LIVE.load(Ordering::Relaxed);

    let first_bytes = after_first - before;
    let second_bytes = after_second - after_first;
    assert_eq!(
        first.snapshot().digest(),
        second.snapshot().digest(),
        "both compositions describe one catalog"
    );
    assert!(
        second_bytes * 10 < first_bytes,
        "the second composition holds {second_bytes} bytes beside the first's {first_bytes}"
    );
}
