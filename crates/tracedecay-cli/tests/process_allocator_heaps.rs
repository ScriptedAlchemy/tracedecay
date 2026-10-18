//! The shipped allocator's purge settings.
//!
//! This binary never routes a C library's allocator: `route_c_libraries`
//! swaps SQLite's and tree-sitter's allocators globally, so any binary that
//! calls it must own its process — see `c_library_allocator` and
//! `retained_parse_heap`.

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
