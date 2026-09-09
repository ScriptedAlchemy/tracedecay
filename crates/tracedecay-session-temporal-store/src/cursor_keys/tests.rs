use std::time::Duration;

use tempfile::tempdir;
use tracedecay_contracts::now_micros;
use tracedecay_global_db::RegisteredGlobalDb;
use tracedecay_global_db::tests::harness::{HostAdmissionScope, HostAdmissionTestRuntimeV1};
use tracedecay_runtime_core::db::engine::{IntoParams, params};
use tracedecay_temporal_query::ports::SessionCursorAuthenticator;

use super::{GlobalDbCursorKeyProvider, GlobalDbCursorKeyProviderError};
use crate::SessionTemporalAccess;

const LOAD_DEADLINE: Duration = Duration::from_secs(5);

async fn registered_runtime(directory: &std::path::Path) -> HostAdmissionTestRuntimeV1 {
    HostAdmissionTestRuntimeV1::profile(directory)
        .await
        .expect("registered profile runtime")
}

fn database(runtime: &HostAdmissionTestRuntimeV1) -> &RegisteredGlobalDb {
    runtime
        .registered_database(HostAdmissionScope::Profile)
        .expect("registered session database")
}

async fn load(
    database: &RegisteredGlobalDb,
) -> Result<GlobalDbCursorKeyProvider, GlobalDbCursorKeyProviderError> {
    tokio::time::timeout(
        LOAD_DEADLINE,
        SessionTemporalAccess::new(database).load_session_cursor_key_provider_result(),
    )
    .await
    .expect("cursor key provider load must not block on an unrelated writer")
}

async fn key_rows(database: &RegisteredGlobalDb) -> Vec<(String, i64, Option<i64>)> {
    let snapshot = database.read_snapshot().await.expect("read snapshot");
    let mut rows = snapshot
        .query(
            "SELECT key_id, key_version, retired_at FROM session_query_cursor_keys
             ORDER BY key_version",
            (),
        )
        .await
        .expect("cursor key rows");
    let mut out = Vec::new();
    while let Some(row) = rows.next().await.expect("cursor key row") {
        out.push((
            row.get::<String>(0).expect("key id"),
            row.get::<i64>(1).expect("key version"),
            row.get::<Option<i64>>(2).expect("retired_at"),
        ));
    }
    out
}

async fn mutate(database: &RegisteredGlobalDb, sql: &str, params: impl IntoParams) {
    database
        .writer_connection()
        .expect("registered writer connection")
        .execute(sql, params)
        .await
        .expect("fixture mutation");
}

#[tokio::test]
async fn concurrent_first_use_callers_mint_exactly_one_active_key() {
    let directory = tempdir().expect("temporary session store");
    let runtime = registered_runtime(directory.path()).await;
    let database = database(&runtime);

    let (left, right) = tokio::join!(load(database), load(database));
    let left = left.expect("left first-use load");
    let right = right.expect("right first-use load");

    assert_eq!(left.active_key_ref(), right.active_key_ref());
    let rows = key_rows(database).await;
    assert_eq!(
        rows.len(),
        1,
        "concurrent first use must not mint twice: {rows:?}"
    );
    assert!(rows[0].2.is_none());
}

/// The schema's guard triggers make two simultaneously active keys
/// unreachable through SQL; disabling the rotation trigger models a corrupted
/// store so the read path's refusal can be exercised.
#[tokio::test]
async fn multiple_active_keys_refuse_without_minting() {
    let directory = tempdir().expect("temporary session store");
    let runtime = registered_runtime(directory.path()).await;
    let database = database(&runtime);
    load(database).await.expect("provision");
    mutate(
        database,
        "DROP TRIGGER session_query_cursor_keys_rotate_insert_v1",
        (),
    )
    .await;
    mutate(
        database,
        "INSERT INTO session_query_cursor_keys (
            key_id, key_version, key_material, created_at, retired_at
         ) VALUES ('cursor-key-2-second', 2, ?1, ?2, NULL)",
        params![vec![7_u8; 32], now_micros().0 + 1_000_000],
    )
    .await;
    let before = key_rows(database).await;
    assert_eq!(before.len(), 2);
    assert!(before.iter().all(|row| row.2.is_none()));

    let error = load(database).await.expect_err("two active keys refuse");
    assert!(
        matches!(
            error,
            GlobalDbCursorKeyProviderError::MultipleActiveKeys { .. }
        ),
        "{error:?}"
    );
    assert_eq!(
        key_rows(database).await,
        before,
        "refusal must not mint or retire"
    );
}

/// Key material is immutable under the retire-update guard; disabling it
/// models on-disk corruption of the active key.
#[tokio::test]
async fn invalid_active_key_material_refuses_without_minting() {
    let directory = tempdir().expect("temporary session store");
    let runtime = registered_runtime(directory.path()).await;
    let database = database(&runtime);
    load(database).await.expect("provision");
    mutate(
        database,
        "DROP TRIGGER session_query_cursor_keys_retire_update_v1",
        (),
    )
    .await;
    mutate(
        database,
        "UPDATE session_query_cursor_keys SET key_material = ?1",
        params![vec![1_u8; 3]],
    )
    .await;
    let before = key_rows(database).await;

    let error = load(database).await.expect_err("corrupt material refuses");
    assert!(
        matches!(error, GlobalDbCursorKeyProviderError::InvalidKeyMaterial),
        "{error:?}"
    );
    assert_eq!(key_rows(database).await, before, "refusal must not mint");
}

/// Rotation inserts a successor; the schema retires the predecessor. The
/// rotated store still has an active key, so loading it is read-only, and a
/// cursor frozen under the retired key keeps verifying within retention.
#[tokio::test]
async fn rotated_store_loads_read_only_and_keeps_retired_key_verifiable() {
    let directory = tempdir().expect("temporary session store");
    let runtime = registered_runtime(directory.path()).await;
    let database = database(&runtime);
    let first = load(database).await.expect("provision");
    let old_key = first.active_key_ref().clone();
    let signature = first
        .sign(&old_key, b"frozen cursor")
        .expect("sign with the first active key");

    mutate(
        database,
        "INSERT INTO session_query_cursor_keys (
            key_id, key_version, key_material, created_at, retired_at
         ) VALUES ('cursor-key-2-successor', 2, ?1, ?2, NULL)",
        params![vec![9_u8; 32], now_micros().0 + 1_000_000],
    )
    .await;
    let rows = key_rows(database).await;
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert!(rows[0].2.is_some(), "rotation retires the predecessor");
    assert!(rows[1].2.is_none());

    let held_writer = database
        .begin_write_transaction()
        .await
        .expect("unrelated writer transaction");
    let rotated = load(database).await.expect("rotated store loads read-only");
    held_writer
        .commit()
        .await
        .expect("release unrelated writer");

    assert_eq!(rotated.active_key_ref().version.value(), 2);
    assert_ne!(rotated.active_key_ref(), &old_key);
    rotated
        .verify(&old_key, b"frozen cursor", &signature)
        .expect("a cursor frozen under the retired key still verifies");
    assert!(
        rotated.sign(&old_key, b"frozen cursor").is_err(),
        "the retired key must not sign new cursors"
    );
    assert_eq!(key_rows(database).await, rows, "loading never mints");
}
