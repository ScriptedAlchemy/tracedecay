use crate::global_db::{
    AnalyticsEventInsert, AnalyticsEventQuery, CoverageStateV1, ObservabilityRollupFragmentQueryV1,
    ObservabilityRollupRebuildV1,
};

use super::*;

#[tokio::test]
async fn session_maintenance_prunes_event_detail_and_derived_daily_rollups() {
    let _pin = tracedecay_runtime_core::config::PinnedUserDataDir::new();
    let harness = crate::global_db::tests::harness::RegisteredGlobalDbHarness::open(
        "daemon-observability-retention",
    )
    .await;
    harness
        .registered
        .append_observability_event(&AnalyticsEventInsert {
            provider: "tracedecay-observability".to_owned(),
            project_id: "scope:retention".to_owned(),
            session_id: None,
            timestamp: 0,
            event_kind: "retrieval.query.completed.v1".to_owned(),
            hook_name: None,
            tool_name: None,
            tool_category: None,
            skill_name: None,
            hint_category: None,
            hint_id: Some("retention:event:1".to_owned()),
            outcome: Some("succeeded".to_owned()),
            metadata_json: Some(
                serde_json::json!({
                    "retention_class": "optional_local_detail30d"
                })
                .to_string(),
            ),
        })
        .await
        .expect("append old observability detail");
    harness
        .registered
        .rebuild_observability_rollup(ObservabilityRollupRebuildV1 {
            authorized_scope_ref: "scope:retention".to_owned(),
            day_start_seconds: 0,
            projector_revision: "execution-topology-projector.v1".to_owned(),
            source_watermark: 1,
            coverage: CoverageStateV1::Known,
            idempotency_key: "retention:rollup:1".to_owned(),
            dirty_claim: None,
            empty_day_claim: None,
            fragment_json: r#"{"kind":"execution_topology_rollup_fragment","schema_revision":1}"#
                .to_owned(),
        })
        .await
        .expect("publish old daily rollup");
    let mut config = RetentionConfig::default();
    config.session_lcm.enabled = false;
    config.observation.enabled = false;
    config.compaction = None;

    let cancellation = tracedecay_usecases::context::CancellationToken::new();
    assert!(run_session_retention(&harness.registered, &config, &cancellation).await);
    let rows = harness
        .registered
        .query_analytics_events(&AnalyticsEventQuery {
            provider: Some("tracedecay-observability".to_owned()),
            project_id: Some("scope:retention".to_owned()),
            limit: 10,
            ..AnalyticsEventQuery::default()
        })
        .await
        .expect("query retained observability detail");
    assert!(rows.is_empty(), "maintenance must invoke detail retention");
    let rollups = harness
        .registered
        .query_observability_rollup_fragments(&ObservabilityRollupFragmentQueryV1 {
            authorized_scope_ref: "scope:retention".to_owned(),
            since_day_start_seconds: 0,
            until_day_start_seconds: 86_400,
        })
        .await
        .expect("query retained observability rollups");
    assert!(
        rollups.fragments.is_empty(),
        "maintenance must invoke derived rollup retention"
    );
}

#[tokio::test]
async fn bounded_observability_page_requests_maintenance_retry_until_drained() {
    let _pin = tracedecay_runtime_core::config::PinnedUserDataDir::new();
    let harness = crate::global_db::tests::harness::RegisteredGlobalDbHarness::open(
        "daemon-observability-retention-retry",
    )
    .await;
    let events = (0..=512)
        .map(|index| AnalyticsEventInsert {
            provider: "tracedecay-observability".to_owned(),
            project_id: "scope:retention-retry".to_owned(),
            session_id: None,
            timestamp: 0,
            event_kind: "retrieval.query.completed.v1".to_owned(),
            hook_name: None,
            tool_name: None,
            tool_category: None,
            skill_name: None,
            hint_category: None,
            hint_id: Some(format!("retention:retry:{index}")),
            outcome: Some("succeeded".to_owned()),
            metadata_json: Some(
                serde_json::json!({
                    "retention_class": "optional_local_detail30d"
                })
                .to_string(),
            ),
        })
        .collect::<Vec<_>>();
    harness
        .registered
        .append_analytics_events(&events)
        .await
        .expect("append retention backlog");
    let cancellation = tracedecay_usecases::context::CancellationToken::new();

    assert!(
        !run_observability_analytics_retention(
            &harness.registered,
            "retry_fixture",
            &cancellation,
        )
        .await,
        "a remaining bounded page must select the maintenance retry cadence"
    );
    assert!(
        run_observability_analytics_retention(&harness.registered, "retry_fixture", &cancellation,)
            .await,
        "the retry must drain the final bounded page"
    );
}

#[tokio::test]
async fn cancellation_aborts_a_waiting_observability_retention_page_without_mutation() {
    let _pin = tracedecay_runtime_core::config::PinnedUserDataDir::new();
    let harness = crate::global_db::tests::harness::RegisteredGlobalDbHarness::open(
        "daemon-observability-retention-cancelled",
    )
    .await;
    let event = AnalyticsEventInsert {
        provider: "tracedecay-observability".to_owned(),
        project_id: "scope:retention-cancelled".to_owned(),
        session_id: None,
        timestamp: 0,
        event_kind: "retrieval.query.completed.v1".to_owned(),
        hook_name: None,
        tool_name: None,
        tool_category: None,
        skill_name: None,
        hint_category: None,
        hint_id: Some("retention:cancelled:1".to_owned()),
        outcome: Some("succeeded".to_owned()),
        metadata_json: Some(r#"{"retention_class":"optional_local_detail30d"}"#.to_owned()),
    };
    harness
        .registered
        .append_observability_event(&event)
        .await
        .expect("append cancellable retention event");
    let blocker = harness
        .registered
        .begin_write_transaction()
        .await
        .expect("hold registered writer");
    let database = std::sync::Arc::clone(&harness.registered);
    let cancellation = tracedecay_usecases::context::CancellationToken::new();
    let task_cancellation = cancellation.clone();
    let maintenance = tokio::spawn(async move {
        run_observability_analytics_retention(
            database.as_ref(),
            "cancelled_fixture",
            &task_cancellation,
        )
        .await
    });
    tokio::task::yield_now().await;
    cancellation.cancel();
    assert!(!maintenance.await.expect("join cancelled maintenance"));
    blocker.commit().await.expect("release registered writer");

    let retained = harness
        .registered
        .query_analytics_events(&AnalyticsEventQuery {
            provider: Some("tracedecay-observability".to_owned()),
            project_id: Some("scope:retention-cancelled".to_owned()),
            limit: 10,
            ..AnalyticsEventQuery::default()
        })
        .await
        .expect("query after cancelled maintenance");
    assert_eq!(retained.len(), 1);
}
