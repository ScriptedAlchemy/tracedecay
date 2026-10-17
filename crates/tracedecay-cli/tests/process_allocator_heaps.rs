//! The shipped allocator's purge settings and owner-heap safety.
//!
//! These tests share one binary because none of them routes a C library's
//! allocator: `route_c_libraries` swaps SQLite's and tree-sitter's
//! allocators globally, so any binary that calls it must own its process —
//! see `c_library_allocator` and `retained_parse_heap`.

#![cfg(not(feature = "alloc-jemalloc"))]

// This binary never routes a C library's allocator, so the routing calls
// below stay unused here; the binaries that own their process use them.
#[allow(dead_code)]
#[path = "../src/process_allocator.rs"]
mod process_allocator;

use std::ffi::{c_int, c_long};

use process_allocator::mimalloc_v3;

unsafe extern "C" {
    fn mi_option_get(option: c_int) -> c_long;
}

#[global_allocator]
static MIMALLOC: mimalloc::MiMalloc = mimalloc::MiMalloc;

use tracedecay_domain::process_heap::OwnerHeapV1;

#[test]
fn process_allocator_purges_immediately_and_decommits() {
    mimalloc_v3::configure_purge();
    assert_eq!(
        // SAFETY: option 15 is mi_option_purge_delay in mimalloc 3.3.2.
        unsafe { mi_option_get(15) },
        0,
        "a daemon must purge on collect, not after the 10 ms default delay"
    );
    assert!(
        // SAFETY: option 5 is mi_option_purge_decommits in mimalloc 3.3.2.
        unsafe { mi_option_get(5) != 0 },
        "purge must decommit so macOS phys_footprint can fall"
    );
}

/// The thread that built an owner's blocks can free them after another
/// thread dropped the owner's heap, as a hibernating graph engine's state
/// is freed after its heap is released (#4152). Under mimalloc 3.3.2 the
/// deletion frees the builder's thread heap while its pages still name it,
/// and the builder's next free that empties such a page dereferences it.
#[test]
fn the_building_thread_frees_its_blocks_after_another_thread_dropped_the_heap() {
    mimalloc_v3::install();
    let (to_deleter, deletions) = std::sync::mpsc::channel::<OwnerHeapV1>();
    let deleter = std::thread::spawn(move || deletions.into_iter().for_each(drop));
    std::thread::scope(|builders| {
        for builder in 0..8_usize {
            let to_deleter = to_deleter.clone();
            builders.spawn(move || {
                for round in 0..2_000_usize {
                    let (owned, heap) = OwnerHeapV1::build(|| {
                        (0..1 + (round * 37 + builder) % 2_000)
                            .map(|index| vec![7_u8; 16 + (index * 61 + round + builder) % 4_000])
                            .collect::<Vec<_>>()
                    });
                    if let Some((heap, _bytes)) = heap {
                        to_deleter.send(heap).expect("the deleter runs");
                    }
                    assert!(
                        owned
                            .iter()
                            .all(|block| block.iter().all(|byte| *byte == 7))
                    );
                    drop(owned);
                }
            });
        }
    });
    drop(to_deleter);
    deleter.join().expect("another thread drops the heaps");
}
