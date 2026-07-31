//! Guards for the reopen-time session-temporal repair.
//!
//! The repair suspends the session-temporal immutability triggers so it can
//! rewrite interrupted refresh state, then reinstates them. Two things must
//! hold every time: it declines to touch a store that never carried the
//! authority tables, and a repair that fails partway leaves the triggers
//! standing rather than a live user store with its integrity guards stripped.

use crate::db::engine::QueryExecutor;
use crate::tests::harness::RegisteredGlobalDbHarness;
use crate::{connection_table_exists, repair_session_temporal_store};

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

/// Drops `tables` if they are present, so a fixture can describe the store it
/// wants without assuming which of them the harness starts with.
async fn drop_tables(harness: &RegisteredGlobalDbHarness, tables: &[&str]) {
    let transaction = harness
        .registered
        .begin_write_transaction()
        .await
        .expect("begin fixture transaction");
    // Authority triggers and views reference tables across the set, so SQLite
    // refuses to drop one while a surviving dependent still names a dropped
    // peer. Clear them first; the store this fixture describes never had them.
    let mut rows = transaction
        .query(
            "SELECT type, name FROM sqlite_master WHERE type IN ('trigger', 'view')",
            (),
        )
        .await
        .expect("read fixture schema catalog");
    let mut dependents = Vec::new();
    while let Some(row) = rows.next().await.expect("fixture schema row") {
        dependents.push((
            row.get::<String>(0).expect("object type column"),
            row.get::<String>(1).expect("object name column"),
        ));
    }
    drop(rows);
    for (kind, name) in dependents {
        let keyword = if kind == "view" { "VIEW" } else { "TRIGGER" };
        transaction
            .execute_batch(&format!("DROP {keyword} IF EXISTS {name};"))
            .await
            .unwrap_or_else(|error| panic!("drop {kind} {name}: {error}"));
    }
    for table in tables {
        if connection_table_exists(&transaction, table)
            .await
            .expect("inspect fixture schema")
        {
            transaction
                .execute_batch(&format!("DROP TABLE {table};"))
                .await
                .unwrap_or_else(|error| panic!("drop {table}: {error}"));
        }
    }
    transaction.commit().await.expect("commit fixture");
}

async fn assert_tables_absent(harness: &RegisteredGlobalDbHarness, tables: &[&str], why: &str) {
    let transaction = harness
        .registered
        .begin_write_transaction()
        .await
        .expect("begin assertion transaction");
    for table in tables {
        assert!(
            !connection_table_exists(&transaction, table)
                .await
                .expect("inspect repaired schema"),
            "repair must not initialize {table}: {why}"
        );
    }
}

/// A store with no authority tables was never opened for observations. Repair
/// must leave it exactly as it found it — initializing the temporal schema here
/// would mint an authority store out of an empty file.
#[tokio::test]
async fn offline_session_repair_does_not_initialize_unopened_store() {
    let harness = RegisteredGlobalDbHarness::open("session-repair-unopened").await;
    drop_tables(
        &harness,
        &[
            "session_temporal_generations",
            "observations",
            "session_messages",
        ],
    )
    .await;

    repair_session_temporal_store(&harness.registered)
        .await
        .expect("an unopened store needs no temporal repair");

    assert_tables_absent(
        &harness,
        &[
            "session_temporal_generations",
            "session_messages",
            "observations",
        ],
        "the store was never opened for observations",
    )
    .await;
}

/// A transcript-only store carries `session_messages` but never committed an
/// observation. Repair still declines: the temporal authority is derived from
/// observations, so building it here would fabricate an authority with no
/// evidence behind it.
#[tokio::test]
async fn offline_session_repair_does_not_initialize_transcript_only_store() {
    let harness = RegisteredGlobalDbHarness::open("session-repair-transcript-only").await;
    drop_tables(&harness, &["session_temporal_generations", "observations"]).await;
    let transaction = harness
        .registered
        .begin_write_transaction()
        .await
        .expect("begin transcript-only fixture transaction");
    if !connection_table_exists(&transaction, "session_messages")
        .await
        .expect("inspect transcript schema")
    {
        transaction
            .execute_batch("CREATE TABLE session_messages (message_id TEXT PRIMARY KEY);")
            .await
            .expect("initialize transcript-only fixture");
    }
    transaction
        .commit()
        .await
        .expect("commit transcript-only fixture");

    repair_session_temporal_store(&harness.registered)
        .await
        .expect("a transcript-only store needs no temporal repair");

    assert_tables_absent(
        &harness,
        &["session_temporal_generations", "observations"],
        "no observation evidence backs a temporal authority",
    )
    .await;
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
    transaction.commit().await.expect("commit rollback fixture");

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
