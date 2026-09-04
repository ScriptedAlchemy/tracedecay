use super::harness::RegisteredGlobalDbHarness;

#[tokio::test]
async fn registered_session_message_batch_executes_json_rowset_with_exact_provider_identity() {
    let harness = RegisteredGlobalDbHarness::open("session-message-identity-batch").await;
    let writer = harness.registered.writer_connection().unwrap();
    for provider in ["cursor", "codex"] {
        writer
            .execute(
                "INSERT INTO sessions(provider, session_id, project_key, project_path)
                 VALUES (?1, 'session.fixture', '/tmp/project', '/tmp/project')",
                tracedecay_runtime_core::db::engine::params![provider],
            )
            .await
            .unwrap();
    }
    writer
        .execute(
            "INSERT INTO session_messages(provider, message_id, session_id, role, ordinal, text)
             VALUES ('cursor', 'comp:b2', 'session.fixture', 'user', 1, 'cursor'),
                    ('codex', 'comp:b1', 'session.fixture', 'user', 1, 'codex')",
            (),
        )
        .await
        .unwrap();

    let existing = harness
        .registered
        .existing_session_message_ids(
            "cursor",
            &[
                "comp:b1".to_string(),
                "comp:b2".to_string(),
                "comp:b3".to_string(),
            ],
        )
        .await
        .unwrap();
    assert_eq!(existing, vec!["comp:b2"]);
}

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

/// A profile store keeps one `source_cursors` row per observed session
/// source, so a long-lived profile holds far more rows than the `SQLite`
/// runtime materializes for a single exact-SQL query. The frontier listing
/// must page and still return the complete scan; before it paged, every
/// project open against such a profile degraded its full session upgrade.
#[tokio::test]
async fn source_frontier_listing_pages_past_the_exact_sql_row_limit() {
    const ROWS: i64 = 10_001;
    let harness = RegisteredGlobalDbHarness::open("session-sync-frontier-pages").await;
    harness
        .registered
        .writer_connection()
        .unwrap()
        .execute(
            &format!(
                "WITH RECURSIVE fixture(value) AS (
                     SELECT 1 UNION ALL SELECT value + 1 FROM fixture WHERE value < {ROWS}
                 )
                 INSERT INTO source_cursors(source_json, scope_json, cursor_json)
                 SELECT json_object('session_id', printf('session-%07d', value)),
                        json_object('kind', 'profile'),
                        json_object('position', value)
                 FROM fixture"
            ),
            tracedecay_runtime_core::db::engine::params![],
        )
        .await
        .unwrap();

    let frontiers = harness
        .registered
        .list_session_sync_source_frontiers()
        .await
        .expect("a frontier scan larger than one exact-SQL query must page, not refuse");

    assert_eq!(i64::try_from(frontiers.len()).unwrap(), ROWS);
    assert!(
        frontiers.windows(2).all(|pair| pair[0] < pair[1]),
        "the paged scan must stay sorted and free of duplicates"
    );
}

/// One recovery page must survive journal values whose combined size exceeds
/// the per-query byte budget: keys arrive in one bounded query and each
/// journal value in its own single-row query. Before that split, eight live
/// multi-megabyte journals refused the whole page and recovery could never
/// terminate them.
#[tokio::test]
async fn incomplete_journal_page_survives_values_beyond_the_query_byte_budget() {
    let harness = RegisteredGlobalDbHarness::open("session-sync-journal-page-bytes").await;
    // Eight live journals of ~9 MiB each: materialized together, one 8-row
    // page holds ~72 MiB, past the 64 MiB per-query budget.
    let pad = "x".repeat(9 * 1024 * 1024);
    for index in 0..8 {
        let key = format!("session-sync.v1.large-{index}");
        let value = format!(r#"{{"status":"running","pad":"{pad}"}}"#);
        assert!(
            harness
                .registered
                .insert_session_sync_journal(&key, &value)
                .await
                .unwrap()
        );
    }

    let page = harness
        .registered
        .list_incomplete_session_sync_journal_page("session-sync.v1.", None)
        .await
        .expect("a page of large live journals must read value-by-value, not refuse");

    assert_eq!(page.len(), 8);
    assert!(page.iter().all(|(_, value)| value.len() > 9 * 1024 * 1024));
}

/// A single journal value past the per-query byte budget is a genuine
/// over-limit row: both the direct read and the recovery page must refuse it
/// typed rather than truncate or mask it.
#[tokio::test]
async fn single_over_limit_journal_value_still_refuses_typed() {
    let harness = RegisteredGlobalDbHarness::open("session-sync-journal-over-limit").await;
    let key = "session-sync.v1.oversized";
    assert!(
        harness
            .registered
            .insert_session_sync_journal(key, &"y".repeat(32 * 1024 * 1024 - 64))
            .await
            .unwrap()
    );
    // Double the value inside SQLite to just under the 64 MiB storage ceiling
    // (the ceiling bounds the whole table row): together with the response's
    // row and column allocation overhead that single value still exceeds the
    // per-query materialization budget.
    harness
        .registered
        .writer_connection()
        .unwrap()
        .execute(
            "UPDATE session_backfill_meta SET value = value || value WHERE key = ?1",
            tracedecay_runtime_core::db::engine::params![key],
        )
        .await
        .unwrap();

    assert!(
        harness
            .registered
            .read_session_sync_journal(key)
            .await
            .is_err(),
        "a single genuinely over-limit journal row must stay a typed refusal"
    );
    assert!(
        harness
            .registered
            .list_incomplete_session_sync_journal_page("session-sync.v1.", None)
            .await
            .is_err(),
        "a recovery page holding a genuinely over-limit journal must stay a typed refusal"
    );
}

#[tokio::test]
async fn session_sync_recovery_reads_only_nonterminal_journals() {
    let harness = RegisteredGlobalDbHarness::open("session-sync-recovery-journals").await;
    for (key, value) in [
        ("session-sync.v1.complete-a", r#"{"status":"complete"}"#),
        ("session-sync.v1.invalid", "{"),
        ("session-sync.v1.queued", r#"{"status":"queued"}"#),
        ("session-sync.v1.complete-b", r#"{"status":"complete"}"#),
        ("session-sync.v1.running", r#"{"status":"running"}"#),
    ] {
        assert!(
            harness
                .registered
                .insert_session_sync_journal(key, value)
                .await
                .unwrap()
        );
    }

    assert_eq!(
        harness
            .registered
            .list_incomplete_session_sync_journal_page("session-sync.v1.", None)
            .await
            .unwrap(),
        vec![
            ("session-sync.v1.invalid".to_owned(), "{".to_owned()),
            (
                "session-sync.v1.queued".to_owned(),
                r#"{"status":"queued"}"#.to_owned(),
            ),
            (
                "session-sync.v1.running".to_owned(),
                r#"{"status":"running"}"#.to_owned(),
            ),
        ]
    );
}
