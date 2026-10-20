use super::harness::RegisteredGlobalDbHarness;
use crate::AnalyticsEventInsert;

fn event(project: &str, hint: Option<&str>) -> AnalyticsEventInsert {
    AnalyticsEventInsert {
        provider: "daemon_hook".to_owned(),
        project_id: project.to_owned(),
        session_id: None,
        timestamp: 1,
        event_kind: "hook_route".to_owned(),
        hook_name: Some("file_edit".to_owned()),
        tool_name: None,
        tool_category: None,
        skill_name: None,
        hint_category: None,
        hint_id: hint.map(str::to_owned),
        outcome: Some("observed".to_owned()),
        metadata_json: Some(r#"{"branch":"main","admission_seq":1}"#.to_owned()),
    }
}

#[asupersync::test]
async fn replay_returns_first_hook_route_row_without_mutating_observation() {
    let harness = RegisteredGlobalDbHarness::open("hook-route-first-receipt").await;
    let event = event("project.route", Some("root-a:1"));
    // Earlier binaries appended every replay. Their rows remain valid history.
    let historical = harness
        .registered
        .append_analytics_events(&[event.clone(), event.clone()])
        .await
        .unwrap();
    assert_ne!(historical[0], historical[1]);
    let mut replay = event.clone();
    replay.timestamp = 200;
    replay.metadata_json = Some(r#"{"branch":"next","admission_seq":1}"#.to_owned());
    assert_eq!(
        harness
            .registered
            .append_analytics_event(&replay)
            .await
            .unwrap(),
        historical[0]
    );
    let snapshot = harness.registered.read_snapshot().await.unwrap();
    let mut rows = snapshot
        .query(
            "SELECT timestamp, metadata_json FROM analytics_events WHERE id = ?1",
            [historical[0]],
        )
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    assert_eq!(row.get::<i64>(0).unwrap(), event.timestamp);
    assert_eq!(row.get::<String>(1).unwrap(), event.metadata_json.unwrap());
    assert_eq!(
        harness
            .registered
            .count_analytics_events(Some("project.route"), 0)
            .await
            .unwrap(),
        2
    );
}

#[asupersync::test]
async fn concurrent_registered_handles_share_one_hook_route_receipt() {
    let harness = RegisteredGlobalDbHarness::open("hook-route-concurrent").await;
    let mut handles = Vec::new();
    for _ in 0..8 {
        handles.push(harness.mount().await);
    }
    let event = event("project.route", Some("root-a:1"));
    let ids =
        futures_util::future::join_all(handles.iter().map(|db| db.append_analytics_event(&event)))
            .await;
    let ids = ids.into_iter().collect::<Result<Vec<_>, _>>().unwrap();
    assert!(ids.iter().all(|id| *id == ids[0]));
    assert_eq!(
        harness
            .registered
            .count_analytics_events(Some("project.route"), 0)
            .await
            .unwrap(),
        1
    );
}

#[asupersync::test]
async fn distinct_routes_and_sequences_append_and_other_events_keep_ordinary_semantics() {
    let harness = RegisteredGlobalDbHarness::open("hook-route-scope").await;
    let db = &harness.registered;
    let mut ids = Vec::new();
    for (project, key) in [
        ("project.route", "root-a:1"),
        ("project.route", "root-a:2"),
        ("project.route", "root-b:1"),
        ("project.other", "root-a:1"),
    ] {
        ids.push(
            db.append_analytics_event(&event(project, Some(key)))
                .await
                .unwrap(),
        );
    }
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), 4);
    let no_hint = event("project.route", None);
    let mut other_provider = event("project.route", Some("root-a:1"));
    other_provider.provider = "another-provider".to_owned();
    let mut other_kind = event("project.route", Some("root-a:1"));
    other_kind.event_kind = "another-kind".to_owned();
    for event in [no_hint, other_provider, other_kind] {
        assert_ne!(
            db.append_analytics_event(&event).await.unwrap(),
            db.append_analytics_event(&event).await.unwrap()
        );
    }
    assert_eq!(db.count_analytics_events(None, 0).await.unwrap(), 10);
}

#[asupersync::test]
async fn existing_store_defers_hook_index_build_then_preserves_earliest_receipt() {
    let harness = RegisteredGlobalDbHarness::open("hook-route-index-upgrade").await;
    let event = event("project.route", Some("root-a:1"));
    let ids = harness
        .registered
        .append_analytics_events(&[event.clone(), event.clone()])
        .await
        .unwrap();
    let transaction = harness.registered.begin_write_transaction().await.unwrap();
    transaction
        .execute_batch("DROP INDEX idx_hook_route_idempotency")
        .await
        .unwrap();
    transaction.commit().await.unwrap();
    let (harness, convergence) = harness.restart_for_daemon().await;
    // Required admission succeeds before the history-sized index build.
    // Best-effort telemetry refuses an unindexed scan until convergence lands.
    let error = harness
        .registered
        .append_analytics_event(&event)
        .await
        .unwrap_err();
    assert!(error.contains("idx_hook_route_idempotency"), "{error}");
    assert_eq!(
        harness
            .registered
            .count_analytics_events(None, 0)
            .await
            .unwrap(),
        2
    );
    crate::schema_stages::converge_registered_schema(
        harness.registered.runtime_database(),
        convergence,
    )
    .await
    .unwrap();
    assert_eq!(
        harness
            .registered
            .append_analytics_event(&event)
            .await
            .unwrap(),
        ids[0]
    );
    assert_eq!(
        harness
            .registered
            .count_analytics_events(None, 0)
            .await
            .unwrap(),
        2
    );
}
