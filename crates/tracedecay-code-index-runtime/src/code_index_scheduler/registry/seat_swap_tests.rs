use crate::code_index_scheduler::CodeIndexSchedulerErrorV1;

use super::ServingSwapOutcomeV1;

/// A retryable native-graph failure must not drop the generation search will
/// serve. The swap installs that same id; graph activation retries beside it.
#[test]
fn retryable_activation_keeps_the_serving_generation_matched() {
    let error = CodeIndexSchedulerErrorV1::GraphActivation(
        "graph runtime unavailable during activation".to_owned(),
    );
    assert!(
        error.is_retryable_activation(),
        "GraphActivation is the retryable class that used to erase the seat candidate"
    );
    let prepared = Some("generation.head");
    let seated = super::serving_generation_after_activation_failure(
        prepared,
        error.is_retryable_activation(),
        false,
    );
    assert_eq!(
        seated, prepared,
        "retryable graph activation must leave the prepared generation on the seat"
    );
    let outcome = ServingSwapOutcomeV1::decide(true, true, seated.is_some());
    assert!(
        outcome.installs(),
        "the serving swap still writes the slot when the candidate survives: {outcome:?}"
    );
}

/// Clone-fingerprint backfill is still unfinished after exact and lexical
/// owners are ready. That successor is not `published_text_owner_unfinished`.
#[test]
fn unfinished_clone_fingerprint_successor_is_not_text_projection_unfinished() {
    assert!(
        !super::text_projection_unfinished_withholds_seat(true),
        "ready exact and lexical owners must still seat while the clone successor runs"
    );
    assert!(
        super::text_projection_unfinished_withholds_seat(false),
        "missing exact or lexical owners still withhold the seat"
    );
}
