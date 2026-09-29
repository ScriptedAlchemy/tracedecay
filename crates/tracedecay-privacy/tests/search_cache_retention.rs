//! The credential detector's search caches follow the scans holding them,
//! not the threads that ever scanned. This binary counts every allocation,
//! so what a round of scans leaves live is measured, not inferred.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicIsize, Ordering};

use tracedecay_privacy::{CodeSourceShapeV1, code_source_scan_batch, sanitize_code_source_bytes};

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

const SCANNERS: usize = 8;

/// Source files dense with the keywords that gate the catalogue's rules, and
/// varied enough that each rule's lazy DFA keeps growing as it reads them.
fn corpus() -> Vec<String> {
    let mut state = 0x9e37_79b9_7f4a_7c15_u64;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    (0..64)
        .map(|file| {
            (0..160)
                .map(|line| {
                    let word = next();
                    format!(
                        "let api_key_{file}_{line} = config.token(\"secret_{:x}\"); // password auth bearer {:08x} client_id aws gh stripe slack\n",
                        word,
                        word >> 32
                    )
                })
                .collect()
        })
        .collect()
}

fn scan_all(texts: &[String]) {
    for text in texts {
        sanitize_code_source_bytes(text.as_bytes(), CodeSourceShapeV1::CodeOrProse)
            .expect("fixture sources sanitize");
    }
}

fn scan_concurrently(texts: &[String]) {
    std::thread::scope(|scope| {
        for _ in 0..SCANNERS {
            scope.spawn(|| scan_all(texts));
        }
    });
}

#[test]
fn scans_leave_one_warm_cache_set_and_a_batch_leaves_none() {
    let texts = corpus();
    let cold = LIVE.load(Ordering::Relaxed);
    scan_all(&texts);
    let settled = LIVE.load(Ordering::Relaxed);
    // Compiling the rules these texts reach, plus one warm cache set.
    let first_scan = settled - cold;

    scan_concurrently(&texts);
    let after_scans = LIVE.load(Ordering::Relaxed) - settled;
    assert!(
        after_scans * 4 < first_scan,
        "{SCANNERS} concurrent scanners left {after_scans} bytes live beyond the \
         {first_scan} bytes one scanner needed"
    );

    let batch = code_source_scan_batch();
    scan_concurrently(&texts);
    let during_batch = LIVE.load(Ordering::Relaxed) - settled;
    drop(batch);
    let after_batch = LIVE.load(Ordering::Relaxed) - settled;
    assert!(
        during_batch * 4 > first_scan,
        "a batch keeps its scanners' caches warm, but held only {during_batch} bytes"
    );
    assert!(
        after_batch <= 0,
        "the end of a batch freed its caches but {after_batch} bytes stayed live"
    );
}
