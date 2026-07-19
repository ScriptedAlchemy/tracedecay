use super::*;

#[tokio::test]
async fn temporal_schema_persists_cursor_keys_without_read_creation() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join(".tracedecay").join("sessions.db");
    let db = GlobalDb::try_open_at(&db_path)
        .await
        .expect("temporal schema initialization should not error")
        .expect("global database should open");
    drop(db);
    assert!(
        table_exists(&db_path, "session_query_cursor_keys").await,
        "the temporal schema must create the cursor-key authority table"
    );
    assert_eq!(row_count(&db_path, "session_query_cursor_keys").await, 0);

    let raw_db = libsql::Builder::new_local(&db_path).build().await.unwrap();
    let conn = raw_db.connect().unwrap();
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
    let reopened = GlobalDb::try_open_at(&restart_path)
        .await
        .expect("writer reopen should preserve a persisted cursor key")
        .expect("global database should reopen");
    drop(reopened);
    assert_eq!(
        row_count(&restart_path, "session_query_cursor_keys").await,
        1
    );

    let missing_path = tmp.path().join(".tracedecay").join("missing.db");
    assert!(GlobalDb::open_read_only_at(&missing_path).await.is_none());
    assert!(
        !missing_path.exists(),
        "a read-only open must not create an absent store"
    );

    let reader = GlobalDb::open_read_only_at(&restart_path)
        .await
        .expect("existing temporal schema should open read-only");
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
    let db = GlobalDb::try_open_at(&db_path)
        .await
        .expect("temporal schema initialization should not error")
        .expect("global database should open");
    drop(db);

    let raw_db = libsql::Builder::new_local(&db_path).build().await.unwrap();
    let conn = raw_db.connect().unwrap();
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
    let db = GlobalDb::try_open_at(&db_path)
        .await
        .expect("temporal schema initialization should not error")
        .expect("global database should open");
    drop(db);

    let raw_db = libsql::Builder::new_local(&db_path).build().await.unwrap();
    let conn = raw_db.connect().unwrap();
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
    let reopened = GlobalDb::try_open_at(&restart_path)
        .await
        .expect("rotated cursor key authority must pass restart validation")
        .expect("global database should reopen");
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
    let db = GlobalDb::try_open_at(&db_path)
        .await
        .expect("temporal schema initialization should not error")
        .expect("global database should open");
    drop(db);

    let raw_db = libsql::Builder::new_local(&db_path).build().await.unwrap();
    let conn = raw_db.connect().unwrap();
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
        GlobalDb::try_open_at(&restart_path).await.is_err(),
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
        let db = GlobalDb::try_open_at(&db_path)
            .await
            .expect("temporal schema initialization should not error")
            .expect("global database should open");
        drop(db);

        let raw_db = libsql::Builder::new_local(&db_path).build().await.unwrap();
        let conn = raw_db.connect().unwrap();
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
            libsql::params![versions[0], versions[1], versions[2]],
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
            GlobalDb::try_open_at(&restart_path).await.is_err(),
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
        let db = GlobalDb::try_open_at(&db_path)
            .await
            .expect("temporal schema initialization should not error")
            .expect("global database should open");
        drop(db);

        let raw_db = libsql::Builder::new_local(&db_path).build().await.unwrap();
        let conn = raw_db.connect().unwrap();
        for (ordinal, version) in versions.into_iter().enumerate() {
            let created_at = ((ordinal + 1) * 100) as i64;
            conn.execute(
                "INSERT INTO session_query_cursor_keys (
                    key_id, key_version, key_material, created_at, retired_at
                 ) VALUES (?1, ?2, X'01', ?3, NULL)",
                libsql::params![format!("{fixture}-key-{version}"), version, created_at],
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
        let reopened = GlobalDb::try_open_at(&restart_path)
            .await
            .expect("valid immediate-successor chain must pass restart audit")
            .expect("global database should reopen");
        drop(reopened);
        assert_valid_cursor_chain(&cursor_key_history(&restart_path).await);
    }
}

#[tokio::test]
async fn temporal_schema_concurrent_cursor_rotations_serialize_safely() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join(".tracedecay").join("sessions.db");
    let db = GlobalDb::try_open_at(&db_path)
        .await
        .expect("temporal schema initialization should not error")
        .expect("global database should open");
    drop(db);

    let seed_db = libsql::Builder::new_local(&db_path).build().await.unwrap();
    let initial = seed_db.connect().unwrap();
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

    // Separate Database/Connection handles so the holder and competitors contend
    // at the SQLite file lock, not within a shared in-process writer queue.
    let holder_db = libsql::Builder::new_local(&db_path).build().await.unwrap();
    let holder = holder_db.connect().unwrap();
    let lower_db = libsql::Builder::new_local(&db_path).build().await.unwrap();
    let higher_db = libsql::Builder::new_local(&db_path).build().await.unwrap();
    let lower_conn = lower_db.connect().unwrap();
    let higher_conn = higher_db.connect().unwrap();
    lower_conn
        .busy_timeout(Duration::from_millis(1))
        .expect("competitor busy_timeout");
    higher_conn
        .busy_timeout(Duration::from_millis(1))
        .expect("competitor busy_timeout");

    let (lock_held_tx, lock_held_rx) = oneshot::channel::<()>();
    let (contention_tx, contention_rx) = oneshot::channel::<()>();
    let (release_tx, release_rx) = oneshot::channel::<()>();
    let probe = Arc::new(ContentionProbe::new(contention_tx));

    let lower_sql = "INSERT INTO session_query_cursor_keys (
        key_id, key_version, key_material, created_at, retired_at
     ) VALUES ('concurrent-key-2', 2, X'02', 200, NULL)";
    let higher_sql = "INSERT INTO session_query_cursor_keys (
        key_id, key_version, key_material, created_at, retired_at
     ) VALUES ('concurrent-key-3', 3, X'03', 300, NULL)";

    let holder_fut = async {
        holder
            .execute("BEGIN IMMEDIATE", ())
            .await
            .expect("holder must acquire a write transaction");
        // Prove the reserved lock is live with a no-op write under the txn.
        holder
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
            Err(_) => panic!("timed out waiting to release holder after contention"),
        }
        holder
            .execute("ROLLBACK", ())
            .await
            .expect("holder must release the write lock");
    };

    let competitors_fut = async {
        timeout(Duration::from_secs(2), lock_held_rx)
            .await
            .expect("timed out waiting for holder write lock")
            .expect("lock-held signal dropped");
        let lower_probe = Arc::clone(&probe);
        let higher_probe = Arc::clone(&probe);
        tokio::join!(
            execute_with_busy_retry(&lower_conn, lower_sql, Some(lower_probe.as_ref())),
            execute_with_busy_retry(&higher_conn, higher_sql, Some(higher_probe.as_ref()))
        )
    };

    let coordinator_fut = async {
        timeout(Duration::from_secs(3), contention_rx)
            .await
            .expect("must observe at least one BUSY/LOCKED retry under held write lock")
            .expect("contention signal dropped");
        assert!(
            probe.busy_retries() >= 1,
            "BUSY/LOCKED retry path must run at least once before release, got {}",
            probe.busy_retries()
        );
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
    if let Err(error) = lower_result {
        assert!(
            error.contains("strictly monotonic") || error.contains("UNIQUE"),
            "lower rotation may fail only after a higher rotation commits: {error}"
        );
    }
    assert!(
        probe.busy_retries() >= 1,
        "BUSY/LOCKED retry path must have run, got {}",
        probe.busy_retries()
    );

    drop(lower_conn);
    drop(higher_conn);
    drop(holder);
    drop(lower_db);
    drop(higher_db);
    drop(holder_db);

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
    let reopened = GlobalDb::try_open_at(&restart_path)
        .await
        .expect("serialized concurrent rotations must pass restart audit")
        .expect("global database should reopen");
    drop(reopened);
    assert_valid_cursor_chain(&cursor_key_history(&restart_path).await);
}
