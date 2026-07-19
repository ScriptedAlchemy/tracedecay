use super::*;

#[tokio::test]
async fn unsupported_legacy_provenance_shape_is_rejected_before_drop() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let store = GlobalDbObservationStore::new(&db);
    let candidate = observation(
        "session-forward-legacy",
        0,
        100,
        "receipt.forward-legacy",
        conversational_payload("message-forward-legacy", "forward legacy canary"),
    );
    persist(&store, candidate, None).await;
    drain_projection_queue(&store).await;
    drop(db);

    reinstall_projection_provenance_schema(&tmp, "forward_owner TEXT,").await;
    assert!(
        GlobalDb::open_at(&isolated_lcm_db_path(&tmp))
            .await
            .is_none()
    );

    let raw_db = libsql::Builder::new_local(isolated_lcm_db_path(&tmp))
        .build()
        .await
        .unwrap();
    let raw_conn = raw_db.connect().unwrap();
    let mut columns = raw_conn
        .query(
            "SELECT name FROM pragma_table_xinfo('observation_projection_provenance')
             WHERE name = 'forward_owner'",
            (),
        )
        .await
        .unwrap();
    assert!(columns.next().await.unwrap().is_some());
    drop(columns);
    drop(raw_conn);
    drop(raw_db);
    assert_eq!(
        table_count(&tmp, "observation_projection_provenance").await,
        1
    );
    assert_eq!(table_count(&tmp, "session_messages").await, 1);
}

#[tokio::test]
async fn unsupported_legacy_provenance_table_options_are_rejected_before_drop() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    drop(db);
    reinstall_projection_provenance_schema_with_options(&tmp, "", "STRICT").await;

    assert!(
        GlobalDb::open_at(&isolated_lcm_db_path(&tmp))
            .await
            .is_none()
    );

    let raw_db = libsql::Builder::new_local(isolated_lcm_db_path(&tmp))
        .build()
        .await
        .unwrap();
    let raw_conn = raw_db.connect().unwrap();
    let mut rows = raw_conn
        .query(
            "SELECT strict FROM pragma_table_list
             WHERE name = 'observation_projection_provenance'",
            (),
        )
        .await
        .unwrap();
    assert_eq!(
        rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
        1
    );
}

#[tokio::test]
async fn supported_legacy_provenance_trigger_survives_table_replacement() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    drop(db);
    reinstall_legacy_projection_provenance_schema(&tmp).await;
    let raw_db = libsql::Builder::new_local(isolated_lcm_db_path(&tmp))
        .build()
        .await
        .unwrap();
    let raw_conn = raw_db.connect().unwrap();
    raw_conn
        .execute_batch(
            "CREATE TRIGGER projection_provenance_message_created_insert_v1
             BEFORE INSERT ON observation_projection_provenance
             WHEN NEW.message_created NOT IN (0, 1)
             BEGIN SELECT RAISE(ABORT, 'invalid projection message_created'); END;",
        )
        .await
        .unwrap();
    drop(raw_conn);
    drop(raw_db);

    let reopened = GlobalDb::open_at(&isolated_lcm_db_path(&tmp)).await;
    assert!(reopened.is_some());
    let raw_db = libsql::Builder::new_local(isolated_lcm_db_path(&tmp))
        .build()
        .await
        .unwrap();
    let raw_conn = raw_db.connect().unwrap();
    let mut triggers = raw_conn
        .query(
            "SELECT 1 FROM sqlite_schema
             WHERE type = 'trigger'
               AND name = 'projection_provenance_message_created_insert_v1'",
            (),
        )
        .await
        .unwrap();
    assert!(triggers.next().await.unwrap().is_some());
}

#[tokio::test]
async fn unknown_legacy_provenance_trigger_is_rejected_before_drop() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    drop(db);
    reinstall_legacy_projection_provenance_schema(&tmp).await;
    let raw_db = libsql::Builder::new_local(isolated_lcm_db_path(&tmp))
        .build()
        .await
        .unwrap();
    let raw_conn = raw_db.connect().unwrap();
    raw_conn
        .execute_batch(
            "CREATE TRIGGER unknown_projection_provenance_trigger
             BEFORE DELETE ON observation_projection_provenance
             BEGIN SELECT RAISE(ABORT, 'must survive failed migration'); END;",
        )
        .await
        .unwrap();
    drop(raw_conn);
    drop(raw_db);

    assert!(
        GlobalDb::open_at(&isolated_lcm_db_path(&tmp))
            .await
            .is_none()
    );
    let raw_db = libsql::Builder::new_local(isolated_lcm_db_path(&tmp))
        .build()
        .await
        .unwrap();
    let raw_conn = raw_db.connect().unwrap();
    let mut triggers = raw_conn
        .query(
            "SELECT 1 FROM sqlite_schema
             WHERE type = 'trigger' AND name = 'unknown_projection_provenance_trigger'",
            (),
        )
        .await
        .unwrap();
    assert!(triggers.next().await.unwrap().is_some());
}
