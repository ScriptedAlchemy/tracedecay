//! The process allocator releases return C-library heap.
//!
//! Measures this process's own anonymous RSS, so it is its own binary with a
//! single test: nothing else may allocate while it measures.

#![cfg(all(target_os = "linux", target_env = "gnu"))]

use tracedecay_runtime_core::resident_memory::{
    ProcessAllocatorTrimV1, install_process_allocator_release_v1, release_c_library_heap_v1,
    release_process_allocator_memory_v1, sampled_process_resident_bytes_v1,
};

const MIB: u64 = 1024 * 1024;
/// Below glibc's mmap threshold, so each chunk comes from a malloc arena.
const CHUNK_BYTES: usize = 64 * 1024;
const CHUNKS: usize = 4096;
/// Survivors pin the freed chunks between them, as long-lived `SQLite` and
/// parser state does, so glibc cannot return the heap by shrinking its top.
const KEPT_EVERY: usize = 32;

fn rust_allocator_release() {}

struct CHeapChunk(*mut u8);

// SAFETY: the pointer is an owned `malloc` allocation freed exactly once.
unsafe impl Send for CHeapChunk {}

impl Drop for CHeapChunk {
    fn drop(&mut self) {
        // SAFETY: `self.0` came from `libc::malloc` and is freed only here.
        unsafe { libc::free(self.0.cast()) };
    }
}

fn anon_bytes() -> u64 {
    sampled_process_resident_bytes_v1().expect("this kernel reports RssAnon")
}

/// Allocate 256 MiB of C heap on a worker thread and free all but every
/// 32nd chunk, returning the survivors.
fn fragmented_c_heap() -> Vec<CHeapChunk> {
    std::thread::spawn(|| {
        let chunks: Vec<CHeapChunk> = (0..CHUNKS)
            .map(|_| {
                // SAFETY: a plain allocation, checked non-null and fully
                // written before use.
                let chunk = unsafe { libc::malloc(CHUNK_BYTES) }.cast::<u8>();
                assert!(!chunk.is_null(), "malloc failed");
                unsafe { chunk.write_bytes(1, CHUNK_BYTES) };
                CHeapChunk(chunk)
            })
            .collect();
        chunks
            .into_iter()
            .enumerate()
            .filter_map(|(index, chunk)| (index % KEPT_EVERY == 0).then_some(chunk))
            .collect()
    })
    .join()
    .expect("allocating worker")
}

fn assert_release_returns_freed_c_heap(label: &str, release: fn() -> ProcessAllocatorTrimV1) {
    let baseline = anon_bytes();
    let kept = fragmented_c_heap();
    let retained = anon_bytes();
    assert!(
        retained >= baseline + 200 * MIB,
        "{label}: freed C heap stays resident before the release: \
         baseline {baseline}, retained {retained}"
    );

    let trim = release();
    let released = anon_bytes();
    assert!(trim.trimmed, "{label}: glibc reported nothing to release");
    assert!(
        released <= baseline + 32 * MIB,
        "{label}: the release returns freed C heap: \
         baseline {baseline}, before {retained}, after {released}"
    );
    drop(kept);
}

/// A mimalloc daemon installs its release for Rust allocations; `SQLite`,
/// tree-sitter, and libgit2 still allocate through glibc `malloc` on worker
/// threads. Both the full release and the sampling-cadence release must hand
/// that freed heap back.
#[test]
fn releases_return_freed_c_heap_while_a_rust_allocator_release_is_installed() {
    install_process_allocator_release_v1(rust_allocator_release).expect("first installation");
    assert_release_returns_freed_c_heap("full release", release_process_allocator_memory_v1);
    assert_release_returns_freed_c_heap("sampling release", release_c_library_heap_v1);
}
