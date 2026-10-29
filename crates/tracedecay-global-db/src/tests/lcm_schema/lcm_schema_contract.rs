use super::*;
use tracedecay_domain::errors::TraceDecayError;

#[asupersync::test]
async fn legacy_profile_requires_reset_without_carrying_session_content_forward() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join(".tracedecay").join("sessions.db");
    create_legacy_sessions_db(&db_path).await;

    let error = match open_global_db(&db_path).await {
        Err(error) => error,
        Ok(_) => panic!("legacy session content must require an explicit profile reset"),
    };
    assert!(matches!(
        error,
        TraceDecayError::ProfileResetRequired {
            component: "LCM",
            found_version: None,
            required_version: tracedecay_lcm::LCM_SCHEMA_VERSION,
        }
    ));
    assert!(!table_exists(&db_path, "lcm_raw_messages").await);
    assert_eq!(row_count(&db_path, "session_messages").await, 1);
}

#[asupersync::test]
async fn stale_or_future_lcm_marker_requires_reset_without_rewriting_marker() {
    for found_version in [
        tracedecay_lcm::LCM_SCHEMA_VERSION - 1,
        tracedecay_lcm::LCM_SCHEMA_VERSION + 1,
    ] {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join(".tracedecay").join("sessions.db");
        let db = open_global_db(&db_path).await.expect("fresh global db");
        drop(db);
        set_migration_version(&db_path, found_version).await;

        let (lease, owner) = open_registered_test_database_fixture(
            &db_path,
            TestDatabaseRuntimeScope::ProfileSessions,
        )
        .await
        .expect("the store's other authorities stay admissible");
        for refusal in [lease.reset_required(), owner.reset_required()] {
            assert!(
                matches!(
                    refusal,
                    Some(TraceDecayError::ProfileResetRequired {
                        component: "LCM",
                        found_version: Some(actual),
                        required_version: tracedecay_lcm::LCM_SCHEMA_VERSION,
                    }) if actual == found_version
                ),
                "an incompatible LCM marker refuses session features: {refusal:?}"
            );
        }
        drop((lease, owner));
        assert_eq!(schema_version(&db_path).await, found_version);
    }
}
