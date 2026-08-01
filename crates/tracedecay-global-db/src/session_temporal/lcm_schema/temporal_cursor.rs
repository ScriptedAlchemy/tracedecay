use super::*;

#[tokio::test]
async fn temporal_schema_persists_cursor_keys_without_read_creation() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join(".tracedecay").join("sessions.db");
    let db = open_global_db(&db_path)
        .await
        .expect("temporal schema initialization should not error");
    drop(db);
    assert!(
        table_exists(&db_path, "session_query_cursor_keys").await,
        "the temporal schema must create the cursor-key authority table"
    );
    assert_eq!(row_count(&db_path, "session_query_cursor_keys").await, 0);

    let raw_db = TestConnection::open(&db_path);
    let conn = (*raw_db).clone();
    conn.execute(
        "INSERT INTO session_query_cursor_keys (
            key_id, key_version, key_material, created_at, retired_at
         )
         VALUES ('key-1', 1, X'01', 100, NULL)",
        (),
    )
    .await
    .unwrap();
    drop(conn);
    drop(raw_db);

    let restart_path = tmp.path().join(".tracedecay").join("restart.db");
    copy_database_for_temporal_restart(&db_path, &restart_path).await;
    let reopened = open_global_db(&restart_path)
        .await
        .expect("writer reopen should preserve a persisted cursor key");
    drop(reopened);
    assert_eq!(
        row_count(&restart_path, "session_query_cursor_keys").await,
        1
    );

    let missing_path = tmp.path().join(".tracedecay").join("missing.db");
    assert!(
        open_read_only_global_db(&missing_path)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        !missing_path.exists(),
        "a read-only open must not create an absent store"
    );

    let reader = open_read_only_global_db(&restart_path)
        .await
        .expect("existing temporal schema should open read-only")
        .expect("existing database should have a read-only runtime");
    drop(reader);
    assert_eq!(
        row_count(&restart_path, "session_query_cursor_keys").await,
        1,
        "read-only opens must not create or rotate cursor keys"
    );
}

#[tokio::test]
async fn temporal_schema_rejects_direct_cursor_retirement() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join(".tracedecay").join("sessions.db");
    let db = open_global_db(&db_path)
        .await
        .expect("temporal schema initialization should not error");
    drop(db);

    let raw_db = TestConnection::open(&db_path);
    let conn = (*raw_db).clone();
    conn.execute(
        "INSERT INTO session_query_cursor_keys (
            key_id, key_version, key_material, created_at, retired_at
         ) VALUES ('cursor-key-only', 5, X'0102', 100, NULL)",
        (),
    )
    .await
    .unwrap();
    assert!(
        conn.execute(
            "UPDATE session_query_cursor_keys
             SET retired_at = 200
             WHERE key_id = 'cursor-key-only'",
            (),
        )
        .await
        .is_err(),
        "the sole active cursor key cannot be retired directly"
    );
}

#[tokio::test]
async fn temporal_schema_rotates_cursor_keys_atomically_and_survives_restart() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join(".tracedecay").join("sessions.db");
    let db = open_global_db(&db_path)
        .await
        .expect("temporal schema initialization should not error");
    drop(db);

    let raw_db = TestConnection::open(&db_path);
    let conn = (*raw_db).clone();
    conn.execute(
        "INSERT INTO session_query_cursor_keys (
            key_id, key_version, key_material, created_at, retired_at
         ) VALUES ('cursor-key-1', 5, X'0102', 100, NULL)",
        (),
    )
    .await
    .unwrap();
    assert!(
        conn.execute(
            "UPDATE session_query_cursor_keys SET key_material = X'0304'
             WHERE key_id = 'cursor-key-1'",
            (),
        )
        .await
        .is_err(),
        "key material must be immutable"
    );
    assert!(
        conn.execute(
            "INSERT INTO session_query_cursor_keys (
                key_id, key_version, key_material, created_at, retired_at
             ) VALUES ('cursor-version-regression', 4, X'03', 200, NULL)",
            (),
        )
        .await
        .is_err(),
        "cursor key versions must strictly increase"
    );
    assert!(
        conn.execute(
            "INSERT INTO session_query_cursor_keys (
                key_id, key_version, key_material, created_at, retired_at
             ) VALUES ('cursor-time-regression', 6, X'03', 100, NULL)",
            (),
        )
        .await
        .is_err(),
        "cursor key creation time must strictly increase"
    );
    conn.execute(
        "INSERT INTO session_query_cursor_keys (
            key_id, key_version, key_material, created_at, retired_at
         ) VALUES ('cursor-key-2', 6, X'0304', 200, NULL)",
        (),
    )
    .await
    .expect("one insert must atomically activate the new key and retire the prior key");
    assert!(
        conn.execute(
            "UPDATE session_query_cursor_keys SET retired_at = 300
             WHERE key_id = 'cursor-key-2'",
            (),
        )
        .await
        .is_err(),
        "the newly active key cannot be retired without a newer replacement"
    );
    assert!(
        conn.execute(
            "UPDATE session_query_cursor_keys SET retired_at = 201
             WHERE key_id = 'cursor-key-1'",
            (),
        )
        .await
        .is_err(),
        "retirement is one-way and cannot be rewritten"
    );
    assert!(
        conn.execute(
            "DELETE FROM session_query_cursor_keys WHERE key_id = 'cursor-key-1'",
            (),
        )
        .await
        .is_err(),
        "cursor key history is durable"
    );

    let mut active = conn
        .query(
            "SELECT COUNT(*) FROM session_query_cursor_keys WHERE retired_at IS NULL",
            (),
        )
        .await
        .unwrap();
    let active_count: i64 = active.next().await.unwrap().unwrap().get(0).unwrap();
    assert_eq!(active_count, 1);
    let mut key_rows = conn
        .query(
            "SELECT key_id, key_version, created_at, retired_at
             FROM session_query_cursor_keys
             ORDER BY key_version",
            (),
        )
        .await
        .unwrap();
    let retired = key_rows.next().await.unwrap().unwrap();
    assert_eq!(retired.get::<String>(0).unwrap(), "cursor-key-1");
    assert_eq!(retired.get::<i64>(3).unwrap(), 200);
    let active = key_rows.next().await.unwrap().unwrap();
    assert_eq!(active.get::<String>(0).unwrap(), "cursor-key-2");
    assert_eq!(active.get::<i64>(1).unwrap(), 6);
    assert!(active.get::<Option<i64>>(3).unwrap().is_none());
    drop(conn);
    drop(raw_db);

    let restart_path = tmp.path().join(".tracedecay").join("cursor-restart.db");
    copy_database_for_temporal_restart(&db_path, &restart_path).await;
    let reopened = open_global_db(&restart_path)
        .await
        .expect("rotated cursor key authority must pass restart validation");
    drop(reopened);
    assert_eq!(
        row_count(&restart_path, "session_query_cursor_keys").await,
        2
    );
}

#[tokio::test]
async fn temporal_schema_cursor_audit_rejects_nonmax_active_key() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join(".tracedecay").join("sessions.db");
    let db = open_global_db(&db_path)
        .await
        .expect("temporal schema initialization should not error");
    drop(db);

    let raw_db = TestConnection::open(&db_path);
    let conn = (*raw_db).clone();
    conn.execute(
        "INSERT INTO session_query_cursor_keys (
            key_id, key_version, key_material, created_at, retired_at
         ) VALUES ('audit-key-1', 1, X'01', 100, NULL)",
        (),
    )
    .await
    .unwrap();
    conn.execute_batch(
        "DROP TRIGGER IF EXISTS session_query_cursor_keys_insert_guard_v1;
         DROP TRIGGER IF EXISTS session_query_cursor_keys_retire_update_v1;
         DROP TRIGGER IF EXISTS session_query_cursor_keys_rotate_insert_v1;
         UPDATE session_query_cursor_keys SET retired_at = 200 WHERE key_id = 'audit-key-1';
         INSERT INTO session_query_cursor_keys (
            key_id, key_version, key_material, created_at, retired_at
         ) VALUES ('audit-key-2', 2, X'02', 200, NULL);
         UPDATE session_query_cursor_keys SET retired_at = NULL WHERE key_id = 'audit-key-1';
         UPDATE session_query_cursor_keys SET retired_at = 300 WHERE key_id = 'audit-key-2';",
    )
    .await
    .unwrap();
    drop(conn);
    drop(raw_db);

    let restart_path = tmp.path().join(".tracedecay").join("cursor-audit.db");
    copy_database_for_temporal_restart(&db_path, &restart_path).await;
    assert!(
        open_global_db(&restart_path).await.is_err(),
        "restart audit must reject an active key that is not the monotonic maximum"
    );
}

#[tokio::test]
async fn temporal_schema_cursor_audit_rejects_skipped_successor_chain() {
    let tmp = TempDir::new().unwrap();
    for (fixture, versions) in [
        ("contiguous", [1_i64, 2, 3]),
        ("version-gaps", [1_i64, 3, 7]),
    ] {
        let db_path = tmp
            .path()
            .join(fixture)
            .join(".tracedecay")
            .join("sessions.db");
        let db = open_global_db(&db_path)
            .await
            .expect("temporal schema initialization should not error");
        drop(db);

        let raw_db = TestConnection::open(&db_path);
        let conn = (*raw_db).clone();
        conn.execute_batch(
            "DROP TRIGGER IF EXISTS session_query_cursor_keys_insert_guard_v1;
             DROP TRIGGER IF EXISTS session_query_cursor_keys_retire_update_v1;
             DROP TRIGGER IF EXISTS session_query_cursor_keys_rotate_insert_v1;",
        )
        .await
        .unwrap();
        conn.execute(
            "INSERT INTO session_query_cursor_keys (
                key_id, key_version, key_material, created_at, retired_at
             ) VALUES
                ('broken-v1', ?1, X'01', 100, 300),
                ('broken-v2', ?2, X'02', 200, 300),
                ('broken-v3', ?3, X'03', 300, NULL)",
            params![versions[0], versions[1], versions[2]],
        )
        .await
        .unwrap();
        drop(conn);
        drop(raw_db);

        let restart_path = tmp
            .path()
            .join(fixture)
            .join(".tracedecay")
            .join("restart.db");
        copy_database_for_temporal_restart(&db_path, &restart_path).await;
        assert!(
            open_global_db(&restart_path).await.is_err(),
            "{fixture}: a later key must not satisfy a skipped immediate-successor retirement"
        );
    }
}

#[tokio::test]
async fn temporal_schema_cursor_audit_accepts_valid_successor_chains() {
    let tmp = TempDir::new().unwrap();
    for (fixture, versions) in [
        ("contiguous", [1_i64, 2, 3]),
        ("version-gaps", [1_i64, 3, 7]),
    ] {
        let db_path = tmp
            .path()
            .join(fixture)
            .join(".tracedecay")
            .join("sessions.db");
        let db = open_global_db(&db_path)
            .await
            .expect("temporal schema initialization should not error");
        drop(db);

        let raw_db = TestConnection::open(&db_path);
        let conn = (*raw_db).clone();
        for (ordinal, version) in versions.into_iter().enumerate() {
            let created_at = ((ordinal + 1) * 100) as i64;
            conn.execute(
                "INSERT INTO session_query_cursor_keys (
                    key_id, key_version, key_material, created_at, retired_at
                 ) VALUES (?1, ?2, X'01', ?3, NULL)",
                params![format!("{fixture}-key-{version}"), version, created_at],
            )
            .await
            .unwrap();
        }
        drop(conn);
        drop(raw_db);
        assert_valid_cursor_chain(&cursor_key_history(&db_path).await);

        let restart_path = tmp
            .path()
            .join(fixture)
            .join(".tracedecay")
            .join("valid-restart.db");
        copy_database_for_temporal_restart(&db_path, &restart_path).await;
        let reopened = open_global_db(&restart_path)
            .await
            .expect("valid immediate-successor chain must pass restart audit");
        drop(reopened);
        assert_valid_cursor_chain(&cursor_key_history(&restart_path).await);
    }
}

#[tokio::test]
async fn temporal_schema_concurrent_cursor_rotations_serialize_safely() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join(".tracedecay").join("sessions.db");
    let db = open_global_db(&db_path)
        .await
        .expect("temporal schema initialization should not error");
    drop(db);

    let seed_db = TestConnection::open(&db_path);
    let initial = (*seed_db).clone();
    initial
        .execute(
            "INSERT INTO session_query_cursor_keys (
                key_id, key_version, key_material, created_at, retired_at
             ) VALUES ('concurrent-key-1', 1, X'01', 100, NULL)",
            (),
        )
        .await
        .unwrap();
    drop(initial);
    drop(seed_db);

    // All contenders submit through one physical writer runtime. S11 removes
    // file-lock retry behavior from callers; the runtime owns serialization.
    let runtime = TestConnection::open(&db_path);
    let holder = (*runtime).clone();
    let lower_conn = (*runtime).clone();
    let higher_conn = (*runtime).clone();

    let (lock_held_tx, lock_held_rx) = oneshot::channel::<()>();
    let (submitted_tx, submitted_rx) = oneshot::channel::<()>();
    let (release_tx, release_rx) = oneshot::channel::<()>();

    let lower_sql = "INSERT INTO session_query_cursor_keys (
        key_id, key_version, key_material, created_at, retired_at
     ) VALUES ('concurrent-key-2', 2, X'02', 200, NULL)";
    let higher_sql = "INSERT INTO session_query_cursor_keys (
        key_id, key_version, key_material, created_at, retired_at
     ) VALUES ('concurrent-key-3', 3, X'03', 300, NULL)";

    let holder_fut = async {
        let transaction = holder
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .expect("holder must acquire a write transaction");
        // Prove the reserved lock is live with a no-op write under the txn.
        transaction
            .execute(
                "UPDATE session_temporal_schema_migrations
                 SET version = version
                 WHERE name = 'session-temporal'",
                (),
            )
            .await
            .expect("holder must keep the write lock with an in-txn mutation");
        let _ = lock_held_tx.send(());
        match timeout(Duration::from_secs(5), release_rx).await {
            Ok(Ok(())) => {}
            Ok(Err(_)) => panic!("release signal dropped before holder cleanup"),
            Err(error) => panic!("timed out waiting to release holder after contention: {error}"),
        }
        transaction
            .rollback()
            .await
            .expect("holder must release the write lock");
    };

    let competitors_fut = async {
        timeout(Duration::from_secs(2), lock_held_rx)
            .await
            .expect("timed out waiting for holder write lock")
            .expect("lock-held signal dropped");
        submitted_tx
            .send(())
            .expect("competitor submission signal must remain live");
        tokio::join!(
            lower_conn.execute(lower_sql, ()),
            higher_conn.execute(higher_sql, ())
        )
    };

    let coordinator_fut = async {
        timeout(Duration::from_secs(3), submitted_rx)
            .await
            .expect("competitors must submit while the writer transaction is held")
            .expect("competitor submission signal dropped");
        tokio::task::yield_now().await;
        release_tx
            .send(())
            .expect("holder must still be waiting for release");
    };

    let ((), (lower_result, higher_result), ()) = timeout(Duration::from_secs(10), async {
        tokio::join!(holder_fut, competitors_fut, coordinator_fut)
    })
    .await
    .expect("cursor-key contention test deadlocked or exceeded bound");

    assert!(
        higher_result.is_ok(),
        "highest monotonic rotation must commit after bounded serialization: {higher_result:?}"
    );
    let successful_rotations =
        usize::from(lower_result.is_ok()) + usize::from(higher_result.is_ok());
    assert!(
        successful_rotations >= 1,
        "the one-writer runtime must commit at least one submitted rotation"
    );
    for result in [&lower_result, &higher_result] {
        let message = result
            .as_ref()
            .err()
            .map(ToString::to_string)
            .unwrap_or_default()
            .to_ascii_lowercase();
        assert!(
            !message.contains("busy") && !message.contains("locked"),
            "writer serialization must not leak SQLite lock retries: {message}"
        );
    }
    if let Err(error) = lower_result {
        let error = error.to_string();
        assert!(
            error.contains("strictly monotonic") || error.contains("UNIQUE"),
            "lower rotation may fail only after a higher rotation commits: {error}"
        );
    }

    drop(lower_conn);
    drop(higher_conn);
    drop(holder);
    drop(runtime);

    let history = cursor_key_history(&db_path).await;
    assert_eq!(history.last().unwrap().0, 3);
    assert_valid_cursor_chain(&history);
    assert_eq!(
        history
            .iter()
            .filter(|(_, _, retired_at)| retired_at.is_none())
            .count(),
        1,
        "exactly one active cursor key maximum must remain"
    );

    let restart_path = tmp.path().join(".tracedecay").join("concurrent-restart.db");
    copy_database_for_temporal_restart(&db_path, &restart_path).await;
    let reopened = open_global_db(&restart_path)
        .await
        .expect("serialized concurrent rotations must pass restart audit");
    drop(reopened);
    assert_valid_cursor_chain(&cursor_key_history(&restart_path).await);
}
