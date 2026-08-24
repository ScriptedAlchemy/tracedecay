use super::harness::RegisteredGlobalDbHarness;

#[tokio::test]
async fn session_sync_journal_survives_remount_and_compare_and_swap() {
    let harness = RegisteredGlobalDbHarness::open("session-sync-journal").await;
    let source = tracedecay_domain::ObservationSourceIdentityV1::for_provider(
        tracedecay_domain::ProviderId::new("codex").unwrap(),
        tracedecay_domain::SessionId::new("session.fixture").unwrap(),
    )
    .unwrap();
    let scope = tracedecay_domain::ObservationScopeV1::Project {
        project_id: tracedecay_domain::ProjectId::new("project.fixture").unwrap(),
    };
    let cursor = tracedecay_domain::ObservationSourceCursorV1::new(
        source.clone(),
        scope.clone(),
        tracedecay_domain::ObservationSourceGenerationV1::new(1).unwrap(),
        72,
    )
    .unwrap();
    harness
        .registered
        .writer_connection()
        .unwrap()
        .execute(
            "INSERT INTO source_cursors(source_json, scope_json, cursor_json)
             VALUES (?1, ?2, ?3)",
            tracedecay_runtime_core::db::engine::params![
                serde_json::to_string(&source).unwrap(),
                serde_json::to_string(&scope).unwrap(),
                serde_json::to_string(&cursor).unwrap(),
            ],
        )
        .await
        .unwrap();
    assert!(
        harness
            .registered
            .insert_session_sync_journal("session-sync.v1.fixture", r#"{"status":"queued"}"#)
            .await
            .unwrap()
    );
    assert!(
        !harness
            .registered
            .insert_session_sync_journal("session-sync.v1.fixture", r#"{"status":"duplicate"}"#)
            .await
            .unwrap()
    );
    assert!(
        harness
            .registered
            .compare_and_swap_session_sync_journal(
                "session-sync.v1.fixture",
                r#"{"status":"queued"}"#,
                r#"{"status":"running"}"#,
            )
            .await
            .unwrap()
    );
    let remounted = harness.mount().await;
    assert_eq!(
        remounted
            .read_session_sync_journal("session-sync.v1.fixture")
            .await
            .unwrap()
            .as_deref(),
        Some(r#"{"status":"running"}"#)
    );
    assert_eq!(
        remounted
            .list_session_sync_journals("session-sync.v1.")
            .await
            .unwrap(),
        vec![(
            "session-sync.v1.fixture".to_owned(),
            r#"{"status":"running"}"#.to_owned()
        )]
    );
    assert_eq!(
        remounted
            .list_session_sync_source_frontiers()
            .await
            .unwrap(),
        vec![(
            serde_json::to_string(&source).unwrap(),
            serde_json::to_string(&scope).unwrap(),
            serde_json::to_string(&cursor).unwrap(),
        )]
    );
}
