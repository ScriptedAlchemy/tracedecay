use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;

#[test]
fn legacy_sealed_generation_is_rejected_before_manifest_decode() {
    let legacy = br#"{"generation":{"format_revision":2}}"#;

    assert!(
        !CodeIndexPublishedGenerationV1::sealed_format_is_compatible(legacy)
            .expect("legacy format probe")
    );
    let error = CodeIndexPublishedGenerationV1::decode_sealed(legacy)
        .expect_err("legacy generation must require a rebuild");
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
        collect_bounded_ordered(&items, |item| Ok::<_, ()>(*item * 2)).expect("infallible mapping");

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

    let error = collect_bounded_ordered(&items, |item| {
        visited.fetch_add(1, Ordering::Relaxed);
        if *item == 2 || *item == 200 {
            Err(*item)
        } else {
            Ok(*item)
        }
    })
    .expect_err("the mapping fails");

    assert_eq!(
        error, 2,
        "the reported failure must be the sequential one, not the first to finish"
    );
    assert!(visited.load(Ordering::Relaxed) > 0);
}

#[test]
fn parallel_and_sequential_collection_agree() {
    let items = (0..2_048_usize).collect::<Vec<_>>();
    let operation = |item: &usize| Ok::<_, ()>(item.wrapping_mul(2_654_435_761));

    let sequential = items.iter().map(operation).collect::<Result<Vec<_>, ()>>();
    let parallel = collect_bounded_ordered(&items, operation);

    assert_eq!(sequential, parallel);
}
