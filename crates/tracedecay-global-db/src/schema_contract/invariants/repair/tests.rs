//! Recovery coverage for the reopen-time cursor and projection-frontier
//! repairs.
//!
//! Every one of these paths runs on an ordinary reopen and can *write*: it
//! reconstructs a missing `source_cursors` row, rewinds a frontier that ran
//! ahead of its durable evidence, or rebuilds `projection_queue` from a
//! repaired checkpoint. A wrong verdict silently replays or drops committed
//! observations, so each branch is pinned here rather than inferred from the
//! open succeeding.

use serde_json::Value;
use tracedecay_domain::{
    DurableObservationV1, ObservationSourceCursorV1, ObservationSourceRangeV1,
};
use tracedecay_store::SESSION_MESSAGE_PROJECTOR_VERSION;
use tracedecay_store::observation::{ObservationCoverageReason, ObservationCoverageV1};

use super::super::test_fixture::{open_registered, seed_observation, shift, write_cursor};
use super::{
    repair_committed_source_cursors, repair_projection_frontier,
    validate_observation_cursor_coverage,
};
use tracedecay_runtime_core::db::engine::{Executor, QueryExecutor, params};

/// The exact-advance receipt that makes a frontier ahead of the last committed
/// observation legitimate rather than lost progress.
async fn write_advance_receipt(
    conn: &impl Executor,
    committed: &ObservationSourceCursorV1,
    advanced: &ObservationSourceCursorV1,
) {
    let coverage = ObservationCoverageV1::new(
        advanced.generation(),
        advanced.ordering_domain(),
        ObservationSourceRangeV1::new(committed.position(), advanced.position()).unwrap(),
    );
    conn.execute(
        "INSERT INTO source_cursor_advances(source_json, scope_json, coverage_json, reason)
         VALUES (?1, ?2, ?3, ?4)",
        params![
            serde_json::to_string(advanced.source()).unwrap(),
            serde_json::to_string(advanced.scope()).unwrap(),
            serde_json::to_string(&coverage).unwrap(),
            ObservationCoverageReason::BlankFrame.as_str()
        ],
    )
    .await
    .expect("seed source cursor advance receipt");
}

async fn stored_cursors(conn: &impl QueryExecutor) -> Vec<ObservationSourceCursorV1> {
    let mut rows = conn
        .query("SELECT cursor_json FROM source_cursors", ())
        .await
        .expect("read stored source cursors");
    let mut cursors = Vec::new();
    while let Some(row) = rows.next().await.expect("stored source cursor row") {
        cursors.push(
            serde_json::from_str(&row.get::<String>(0).expect("cursor_json column"))
                .expect("decode stored cursor"),
        );
    }
    cursors
}

/// A store can hold committed observations with no `source_cursors` row at all
/// — an older writer, or a crash between the two writes. Repair reconstructs
/// the frontier from the commit itself instead of restarting the source.
#[tokio::test]
async fn repair_reconstructs_a_missing_committed_source_cursor() {
    let (_directory, conn) = open_registered().await;
    let (_, committed) = seed_observation(&conn, 0, "missing").await;

    repair_committed_source_cursors(&conn, 0)
        .await
        .expect("repair a missing committed source cursor");

    assert_eq!(stored_cursors(&conn).await, vec![committed]);
}

/// A frontier behind its last committed observation would replay work that is
/// already durable, so repair pulls it forward to the commit.
#[tokio::test]
async fn repair_advances_a_stale_present_committed_source_cursor() {
    let (_directory, conn) = open_registered().await;
    let (_, committed) = seed_observation(&conn, 0, "stale").await;
    write_cursor(&conn, &shift(&committed, -1)).await;

    repair_committed_source_cursors(&conn, 0)
        .await
        .expect("repair a stale committed source cursor");

    assert_eq!(stored_cursors(&conn).await, vec![committed]);
}

/// A frontier ahead of the last commit with no coverage receipt is progress an
/// older build recorded without durable evidence. Leaving it would drop the
/// uncovered suffix forever, so repair rewinds to the last canonical commit.
#[tokio::test]
async fn repair_rewinds_unreceipted_cursor_progress() {
    let (_directory, conn) = open_registered().await;
    let (_, committed) = seed_observation(&conn, 0, "unreceipted").await;
    write_cursor(&conn, &shift(&committed, 50)).await;

    repair_committed_source_cursors(&conn, 0)
        .await
        .expect("repair unreceipted cursor progress");

    assert_eq!(stored_cursors(&conn).await, vec![committed]);
}

/// The mirror case: the same forward frontier *with* an exact advance receipt
/// is real covered progress over frames that produced no observation. Rewinding
/// it would replay them, so repair must leave it untouched.
#[tokio::test]
async fn repair_preserves_receipted_nondurable_cursor_progress() {
    let (_directory, conn) = open_registered().await;
    let (_, committed) = seed_observation(&conn, 0, "receipted").await;
    let advanced = shift(&committed, 50);
    write_cursor(&conn, &advanced).await;
    write_advance_receipt(&conn, &committed, &advanced).await;

    repair_committed_source_cursors(&conn, 0)
        .await
        .expect("repair must accept receipted progress");

    assert_eq!(stored_cursors(&conn).await, vec![advanced]);
}

/// Cursor rows are keyed by serialized scope JSON. A scope written by a build
/// that ordered its keys differently must still resolve to the same row rather
/// than minting a second frontier for the same source.
#[tokio::test]
async fn repair_matches_a_reordered_scope_json_key() {
    let (_directory, conn) = open_registered().await;
    let (_, committed) = seed_observation(&conn, 0, "scope-order").await;
    let scope_json = serde_json::to_string(committed.scope()).unwrap();
    let reordered: Value = serde_json::from_str(&scope_json).unwrap();
    let reordered_json = serde_json::to_string(&reordered).unwrap();
    conn.execute(
        "INSERT INTO source_cursors(source_json, scope_json, cursor_json)
         VALUES (?1, ?2, ?3)",
        params![
            serde_json::to_string(committed.source()).unwrap(),
            reordered_json,
            serde_json::to_string(&shift(&committed, -1)).unwrap()
        ],
    )
    .await
    .expect("seed reordered-scope cursor");

    repair_committed_source_cursors(&conn, 0)
        .await
        .expect("repair a reordered scope key");

    let cursors = stored_cursors(&conn).await;
    assert_eq!(
        cursors,
        vec![committed],
        "a reordered scope key must repair in place, not fork a second frontier"
    );
}

/// Repair is not allowed to paper over a frontier it cannot explain. A cursor
/// that is neither the commit, a new-generation frontier, nor covered by a
/// receipt has to surface as an authority violation.
#[tokio::test]
async fn cursor_coverage_validation_rejects_an_unexplained_frontier() {
    let (_directory, conn) = open_registered().await;
    let (_, committed) = seed_observation(&conn, 0, "unexplained").await;
    write_cursor(&conn, &shift(&committed, 50)).await;

    let error = validate_observation_cursor_coverage(&conn, 0)
        .await
        .expect_err("an uncovered frontier must not validate");

    assert!(
        error
            .to_string()
            .contains("source cursor does not exactly match committed or non-durable authority"),
        "{error}"
    );
}

/// A committed observation with no source cursor row must be reported, not
/// tolerated, when the validator runs without a preceding repair.
#[tokio::test]
async fn cursor_coverage_validation_rejects_a_missing_cursor_row() {
    let (_directory, conn) = open_registered().await;
    seed_observation(&conn, 0, "absent").await;

    let error = validate_observation_cursor_coverage(&conn, 0)
        .await
        .expect_err("a missing cursor row must not validate");

    assert!(
        error
            .to_string()
            .contains("committed observation has no source cursor authority row"),
        "{error}"
    );
}

async fn queued_sequences(conn: &impl QueryExecutor) -> Vec<i64> {
    let mut rows = conn
        .query(
            "SELECT observation_sequence FROM projection_queue ORDER BY observation_sequence",
            (),
        )
        .await
        .expect("read projection queue");
    let mut queued = Vec::new();
    while let Some(row) = rows.next().await.expect("projection queue row") {
        queued.push(row.get::<i64>(0).expect("observation_sequence column"));
    }
    queued
}

async fn stored_checkpoint(conn: &impl QueryExecutor) -> i64 {
    let mut rows = conn
        .query(
            "SELECT last_sequence FROM observation_projection_checkpoints
             WHERE projector_version = ?1",
            params![SESSION_MESSAGE_PROJECTOR_VERSION],
        )
        .await
        .expect("read projection checkpoint");
    rows.next()
        .await
        .expect("projection checkpoint row")
        .expect("projection checkpoint value")
        .get::<i64>(0)
        .expect("last_sequence column")
}

async fn seed_disposition(conn: &impl Executor, observation: &DurableObservationV1) {
    conn.execute(
        "INSERT INTO observation_projection_dispositions
         (projector_version, observation_id, receipt_id, reason)
         VALUES (?1, ?2, ?3, 'session_metadata')",
        params![
            SESSION_MESSAGE_PROJECTOR_VERSION,
            observation.observation_id().as_str(),
            observation.receipt().receipt().receipt_id().as_str()
        ],
    )
    .await
    .expect("seed projection disposition");
}

/// The queue is derived state. Whatever it held before the crash, repair must
/// leave exactly the observations after the repaired checkpoint queued.
#[tokio::test]
async fn repair_rebuilds_the_projection_queue_from_the_checkpoint_frontier() {
    let (_directory, conn) = open_registered().await;
    for index in 0..3 {
        let (observation, _) = seed_observation(&conn, index, &format!("queue-{index}")).await;
        seed_disposition(&conn, &observation).await;
    }
    conn.execute(
        "INSERT INTO observation_projection_checkpoints (projector_version, last_sequence)
         VALUES (?1, 2)",
        params![SESSION_MESSAGE_PROJECTOR_VERSION],
    )
    .await
    .expect("seed projection checkpoint");
    conn.execute("DELETE FROM projection_queue", ())
        .await
        .expect("clear projection queue");

    let repaired = repair_projection_frontier(&conn, 0)
        .await
        .expect("repair the projection frontier");

    assert_eq!(repaired, 2);
    assert_eq!(
        queued_sequences(&conn).await,
        vec![3],
        "only the suffix past the repaired checkpoint stays queued"
    );
}

/// A checkpoint claiming coverage it cannot show has to come down. Repair walks
/// forward from the trusted point and stops at the first observation with no
/// disposition, then requeues everything from there.
#[tokio::test]
async fn repair_lowers_a_checkpoint_without_contiguous_projection_evidence() {
    let (_directory, conn) = open_registered().await;
    for index in 0..3 {
        let (observation, _) = seed_observation(&conn, index, &format!("gap-{index}")).await;
        if index == 0 {
            seed_disposition(&conn, &observation).await;
        }
    }
    conn.execute(
        "INSERT INTO observation_projection_checkpoints (projector_version, last_sequence)
         VALUES (?1, 3)",
        params![SESSION_MESSAGE_PROJECTOR_VERSION],
    )
    .await
    .expect("seed overreaching projection checkpoint");

    let repaired = repair_projection_frontier(&conn, 0)
        .await
        .expect("repair an overreaching checkpoint");

    assert_eq!(
        repaired, 1,
        "the checkpoint must fall back to the last contiguously projected observation"
    );
    assert_eq!(stored_checkpoint(&conn).await, 1);
    assert_eq!(
        queued_sequences(&conn).await,
        vec![2, 3],
        "the whole unproven suffix is requeued"
    );
}

/// A disposition missing in the middle of the audited range invalidates every
/// later claim, not just its own row.
#[tokio::test]
async fn checkpoint_with_a_missing_disposition_requeues_the_entire_suffix() {
    let (_directory, conn) = open_registered().await;
    for index in 0..4 {
        let (observation, _) = seed_observation(&conn, index, &format!("suffix-{index}")).await;
        if index != 2 {
            seed_disposition(&conn, &observation).await;
        }
    }
    conn.execute(
        "INSERT INTO observation_projection_checkpoints (projector_version, last_sequence)
         VALUES (?1, 4)",
        params![SESSION_MESSAGE_PROJECTOR_VERSION],
    )
    .await
    .expect("seed projection checkpoint");

    let repaired = repair_projection_frontier(&conn, 0)
        .await
        .expect("repair a checkpoint with an interior gap");

    assert_eq!(repaired, 2);
    assert_eq!(
        queued_sequences(&conn).await,
        vec![3, 4],
        "the suffix after the gap is requeued whole"
    );
}
