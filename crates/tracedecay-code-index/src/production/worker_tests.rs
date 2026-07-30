use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;

#[test]
fn code_index_extraction_parallelism_is_bounded_by_files_and_capacity() {
    let available = std::thread::available_parallelism()
        .map_or(1, usize::from)
        .clamp(1, MAX_CODE_INDEX_EXTRACTION_WORKERS);

    assert_eq!(bounded_code_index_extraction_workers(0), 1);
    assert_eq!(bounded_code_index_extraction_workers(1), 1);
    assert_eq!(bounded_code_index_extraction_workers(usize::MAX), available);
    assert!(bounded_code_index_extraction_workers(4) <= 4);
}

#[test]
fn bounded_parallel_collection_stops_after_the_failing_batch() {
    let visited = AtomicUsize::new(0);
    let items = (0..32).collect::<Vec<_>>();

    let error = collect_bounded_ordered(&items, |item| {
        visited.fetch_add(1, Ordering::Relaxed);
        if *item == 2 { Err(*item) } else { Ok(*item) }
    })
    .expect_err("the first batch contains a fatal error");

    assert_eq!(error, 2);
    assert!(
        visited.load(Ordering::Relaxed) <= MAX_CODE_INDEX_EXTRACTION_WORKERS,
        "work after the failing batch must not start"
    );
}
