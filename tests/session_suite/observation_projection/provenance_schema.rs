use super::*;

#[tokio::test]
async fn unsupported_legacy_provenance_shape_is_rejected_before_drop() {
    let tmp = TempDir::new().unwrap();
    let runtime = profile_runtime(&tmp).await;
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let database_path = runtime
        .database_path(HostAdmissionScope::Profile)
        .unwrap()
        .to_path_buf();
    let candidate = observation(
        "session-forward-legacy",
        0,
        100,
        "receipt.forward-legacy",
        conversational_payload("message-forward-legacy", "forward legacy canary"),
    );
    persist(&store, candidate, None).await;
    drain_projection_queue(&store).await;
    drop(runtime);

    reinstall_projection_provenance_schema(&tmp, "forward_owner TEXT,").await;
    assert!(
        HostAdmissionTestRuntimeV1::profile(tmp.path().join(".tracedecay"))
            .await
            .is_err()
    );

    let raw_conn = rusqlite::Connection::open(&database_path).unwrap();
    assert!(
        raw_conn
            .query_row(
                "SELECT name FROM pragma_table_xinfo('observation_projection_provenance')
             WHERE name = 'forward_owner'",
                (),
                |_| Ok(()),
            )
            .is_ok()
    );
    drop(raw_conn);
    assert_eq!(
        table_count(&tmp, "observation_projection_provenance").await,
        1
    );
    assert_eq!(table_count(&tmp, "session_messages").await, 1);
}

#[tokio::test]
async fn unsupported_legacy_provenance_table_options_are_rejected_before_drop() {
    let tmp = TempDir::new().unwrap();
    let runtime = profile_runtime(&tmp).await;
    let database_path = runtime
        .database_path(HostAdmissionScope::Profile)
        .unwrap()
        .to_path_buf();
    drop(runtime);
    reinstall_projection_provenance_schema_with_options(&tmp, "", "STRICT").await;

    assert!(
        HostAdmissionTestRuntimeV1::profile(tmp.path().join(".tracedecay"))
            .await
            .is_err()
    );

    let raw_conn = rusqlite::Connection::open(&database_path).unwrap();
    let strict = raw_conn
        .query_row(
            "SELECT strict FROM pragma_table_list
             WHERE name = 'observation_projection_provenance'",
            (),
            |row| row.get::<_, i64>(0),
        )
        .unwrap();
    assert_eq!(strict, 1);
}

#[tokio::test]
async fn supported_legacy_provenance_trigger_survives_table_replacement() {
    let tmp = TempDir::new().unwrap();
    let runtime = profile_runtime(&tmp).await;
    let database_path = runtime
        .database_path(HostAdmissionScope::Profile)
        .unwrap()
        .to_path_buf();
    drop(runtime);
    reinstall_legacy_projection_provenance_schema(&tmp).await;
    let raw_conn = rusqlite::Connection::open(&database_path).unwrap();
    raw_conn
        .execute_batch(
            "CREATE TRIGGER projection_provenance_message_created_insert_v1
             BEFORE INSERT ON observation_projection_provenance
             WHEN NEW.message_created NOT IN (0, 1)
             BEGIN SELECT RAISE(ABORT, 'invalid projection message_created'); END;",
        )
        .unwrap();
    drop(raw_conn);

    let reopened = HostAdmissionTestRuntimeV1::profile(tmp.path().join(".tracedecay")).await;
    assert!(reopened.is_ok());
    drop(reopened);
    let raw_conn = rusqlite::Connection::open(&database_path).unwrap();
    assert!(
        raw_conn
            .query_row(
                "SELECT 1 FROM sqlite_schema
             WHERE type = 'trigger'
               AND name = 'projection_provenance_message_created_insert_v1'",
                (),
                |_| Ok(()),
            )
            .is_ok()
    );
}

#[tokio::test]
async fn unknown_legacy_provenance_trigger_is_rejected_before_drop() {
    let tmp = TempDir::new().unwrap();
    let runtime = profile_runtime(&tmp).await;
    let database_path = runtime
        .database_path(HostAdmissionScope::Profile)
        .unwrap()
        .to_path_buf();
    drop(runtime);
    reinstall_legacy_projection_provenance_schema(&tmp).await;
    let raw_conn = rusqlite::Connection::open(&database_path).unwrap();
    raw_conn
        .execute_batch(
            "CREATE TRIGGER unknown_projection_provenance_trigger
             BEFORE DELETE ON observation_projection_provenance
             BEGIN SELECT RAISE(ABORT, 'must survive failed migration'); END;",
        )
        .unwrap();
    drop(raw_conn);

    assert!(
        HostAdmissionTestRuntimeV1::profile(tmp.path().join(".tracedecay"))
            .await
            .is_err()
    );
    let raw_conn = rusqlite::Connection::open(database_path).unwrap();
    assert!(
        raw_conn
            .query_row(
                "SELECT 1 FROM sqlite_schema
             WHERE type = 'trigger' AND name = 'unknown_projection_provenance_trigger'",
                (),
                |_| Ok(()),
            )
            .is_ok()
    );
}
