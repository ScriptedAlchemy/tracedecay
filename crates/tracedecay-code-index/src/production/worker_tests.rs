use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use super::*;

#[derive(Debug, PartialEq, Eq)]
enum WorkerTestError {
    Mapping(usize),
    Parallelism(crate::parallelism::CodeIndexParallelismErrorV1),
}

impl From<crate::parallelism::CodeIndexParallelismErrorV1> for WorkerTestError {
    fn from(error: crate::parallelism::CodeIndexParallelismErrorV1) -> Self {
        Self::Parallelism(error)
    }
}

#[test]
fn cloned_arc_lookup_releases_the_mutex_before_downstream_work() {
    let state = Mutex::new(Some(Arc::new(7_u8)));

    let value = clone_arc_under_lock(&state, |value| value.clone()).expect("pooled value");

    let unlocked = state
        .try_lock()
        .expect("pooled lookup must not retain the mutex");
    assert_eq!(*value, 7);
    assert_eq!(unlocked.as_deref(), Some(&7));
}

#[test]
fn prior_sealed_generation_is_rejected_before_manifest_decode() {
    let prior = br#"{"generation":{"format_revision":4}}"#;

    assert!(
        !CodeIndexPublishedGenerationV1::sealed_format_is_compatible(prior)
            .expect("prior format probe")
    );
    let error = CodeIndexPublishedGenerationV1::decode_sealed(prior)
        .expect_err("prior generation must require a rebuild");
    assert!(
        error
            .to_string()
            .contains("format revision is incompatible")
    );
}

#[test]
fn parallel_collection_preserves_input_order() {
    let items = (0..1_024_usize).collect::<Vec<_>>();

    let values =
        collect_bounded_ordered(&items, |item, _worker| Ok::<_, WorkerTestError>(*item * 2))
            .expect("infallible mapping");

    assert_eq!(values.len(), items.len());
    assert!(
        values
            .iter()
            .enumerate()
            .all(|(index, value)| *value == index * 2),
        "completion order must not reorder results"
    );
}

#[test]
fn parallel_collection_returns_the_lowest_index_failure() {
    let visited = AtomicUsize::new(0);
    let items = (0..256_usize).collect::<Vec<_>>();

    let error = collect_bounded_ordered(&items, |item, _worker| {
        visited.fetch_add(1, Ordering::Relaxed);
        if *item == 2 || *item == 200 {
            Err(WorkerTestError::Mapping(*item))
        } else {
            Ok(*item)
        }
    })
    .expect_err("the mapping fails");

    assert_eq!(
        error,
        WorkerTestError::Mapping(2),
        "the reported failure must be the sequential one, not the first to finish"
    );
    assert!(visited.load(Ordering::Relaxed) > 0);
}

#[test]
fn parallel_and_sequential_collection_agree() {
    let items = (0..2_048_usize).collect::<Vec<_>>();
    let sequential_operation =
        |item: &usize| Ok::<_, WorkerTestError>(item.wrapping_mul(2_654_435_761));
    let parallel_operation = |item: &usize, _worker: &crate::hotpath_observe::WorkerBusyGuard| {
        Ok::<_, WorkerTestError>(item.wrapping_mul(2_654_435_761))
    };

    let sequential = items
        .iter()
        .map(sequential_operation)
        .collect::<Result<Vec<_>, WorkerTestError>>();
    let parallel = collect_bounded_ordered(&items, parallel_operation);

    assert_eq!(sequential, parallel);
}

/// One malformed source file must not take the whole generation down with it.
/// A panicking per-file unit is contained and reported as that unit's typed
/// failure; every other file still runs to completion.
#[test]
fn parallel_collection_contains_a_panicking_unit_without_poisoning_the_rest() {
    let completed = AtomicUsize::new(0);
    let items = (0..256_usize).collect::<Vec<_>>();

    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let outcome = collect_bounded_ordered(&items, |item, _worker| {
        assert_ne!(*item, 200, "synthetic per-file panic");
        completed.fetch_add(1, Ordering::Relaxed);
        Ok::<_, WorkerTestError>(*item)
    });
    std::panic::set_hook(previous_hook);

    let error = outcome.expect_err("a panicking unit must surface as a failure");
    assert_eq!(
        error,
        WorkerTestError::Parallelism(
            crate::parallelism::CodeIndexParallelismErrorV1::WorkerPanic {
                index: 200,
                message: "synthetic per-file panic".to_owned(),
            }
        ),
        "the panic must be reported as that unit's typed failure"
    );
    assert_eq!(
        completed.load(Ordering::Relaxed),
        items.len() - 1,
        "every non-panicking unit must still complete"
    );
}

/// A panic in a later unit must not mask an ordinary failure in an earlier
/// one: reported failure stays the lowest-index one, panic or not.
#[test]
fn parallel_collection_reports_the_lowest_index_failure_across_panics() {
    let items = (0..256_usize).collect::<Vec<_>>();

    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let error = collect_bounded_ordered(&items, |item, _worker| {
        assert_ne!(*item, 200, "synthetic per-file panic");
        if *item == 2 {
            Err(WorkerTestError::Mapping(*item))
        } else {
            Ok(*item)
        }
    })
    .expect_err("the mapping fails");
    std::panic::set_hook(previous_hook);

    assert_eq!(error, WorkerTestError::Mapping(2));
}
