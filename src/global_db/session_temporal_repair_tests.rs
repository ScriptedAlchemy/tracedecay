//! Guards for the reopen-time session-temporal repair.
//!
//! The repair suspends the session-temporal immutability triggers so it can
//! rewrite interrupted refresh state, then reinstates them. Two things must
//! hold every time: it declines to touch a store that never carried the
//! authority tables, and a repair that fails partway leaves the triggers
//! standing rather than a live user store with its integrity guards stripped.

use crate::db::engine::{Executor, QueryExecutor};
use crate::global_db::tests::harness::RegisteredGlobalDbHarness;
use crate::global_db::{connection_table_exists, repair_session_temporal_store};

const SUSPENDED_TRIGGER: &str = "session_refresh_operations_delete_guard_v1";

async fn trigger_exists(conn: &impl QueryExecutor, name: &str) -> bool {
    let mut rows = conn
        .query(
            "SELECT 1 FROM sqlite_master WHERE type = 'trigger' AND name = ?1",
            crate::db::engine::params![name],
        )
        .await
        .expect("inspect trigger catalog");
    rows.next().await.expect("read trigger catalog").is_some()
}

/// A store with no authority tables was never opened for observations. Repair
/// must leave it exactly as it found it — initializing the temporal schema here
/// would mint an authority store out of an empty file.
#[tokio::test]
async fn offline_session_repair_does_not_initialize_unopened_store() {
    let harness = RegisteredGlobalDbHarness::open("session-repair-unopened").await;
    let transaction = harness
        .registered
        .begin_write_transaction()
        .await
        .expect("begin unopened-store fixture transaction");
    transaction
        .execute_batch(
            "DROP TABLE IF EXISTS session_messages;
             DROP TABLE IF EXISTS observations;
             DROP TABLE IF EXISTS session_temporal_generations;",
        )
        .await
        .expect("strip authority tables");
    transaction
        .commit()
        .await
        .expect("commit unopened-store fixture");

    repair_session_temporal_store(&harness.registered)
        .await
        .expect("an unopened store needs no temporal repair");

    let transaction = harness
        .registered
        .begin_write_transaction()
        .await
        .expect("begin unopened-store assertion transaction");
    for table in ["session_temporal_generations", "session_messages", "observations"] {
        assert!(
            !connection_table_exists(&transaction, table)
                .await
                .expect("inspect repaired schema"),
            "repair must not initialize {table} on an unopened store"
        );
    }
}

/// A transcript-only store carries `session_messages` but never committed an
/// observation. Repair still declines: the temporal authority is derived from
/// observations, so building it here would fabricate an authority with no
/// evidence behind it.
#[tokio::test]
async fn offline_session_repair_does_not_initialize_transcript_only_store() {
    let harness = RegisteredGlobalDbHarness::open("session-repair-transcript-only").await;
    let transaction = harness
        .registered
        .begin_write_transaction()
        .await
        .expect("begin transcript-only fixture transaction");
    transaction
        .execute_batch(
            "DROP TABLE IF EXISTS observations;
             DROP TABLE IF EXISTS session_temporal_generations;",
        )
        .await
        .expect("strip observation authority");
    transaction
        .commit()
        .await
        .expect("commit transcript-only fixture");

    repair_session_temporal_store(&harness.registered)
        .await
        .expect("a transcript-only store needs no temporal repair");

    let transaction = harness
        .registered
        .begin_write_transaction()
        .await
        .expect("begin transcript-only assertion transaction");
    for table in ["session_temporal_generations", "observations"] {
        assert!(
            !connection_table_exists(&transaction, table)
                .await
                .expect("inspect repaired schema"),
            "repair must not initialize {table} without observation evidence"
        );
    }
}

/// The repair drops the session-temporal immutability triggers before
/// rewriting interrupted refresh state. If it then fails, the rollback has to
/// bring them back — otherwise a live user store keeps running with its
/// integrity guards silently suspended.
#[tokio::test]
async fn offline_session_repair_rolls_back_trigger_suspension_on_failure() {
    let harness = RegisteredGlobalDbHarness::open("session-repair-rollback").await;
    let transaction = harness
        .registered
        .begin_write_transaction()
        .await
        .expect("begin rollback fixture transaction");
    transaction
        .execute_batch("DROP TABLE session_refresh_progress;")
        .await
        .expect("corrupt the repair fixture");
    transaction
        .commit()
        .await
        .expect("commit rollback fixture");

    let transaction = harness
        .registered
        .begin_write_transaction()
        .await
        .expect("begin rollback precondition transaction");
    assert!(
        trigger_exists(&transaction, SUSPENDED_TRIGGER).await,
        "the fixture must begin with its immutability trigger installed"
    );
    transaction
        .rollback()
        .await
        .expect("release rollback precondition transaction");

    repair_session_temporal_store(&harness.registered)
        .await
        .expect_err("a missing temporal table must reject repair");

    let transaction = harness
        .registered
        .begin_write_transaction()
        .await
        .expect("begin rollback assertion transaction");
    assert!(
        trigger_exists(&transaction, SUSPENDED_TRIGGER).await,
        "failed maintenance must roll back suspended immutability triggers"
    );
}
