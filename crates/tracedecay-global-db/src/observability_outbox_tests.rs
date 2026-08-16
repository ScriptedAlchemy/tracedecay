use crate::tests::harness::RegisteredGlobalDbHarness;
use crate::{
    AnalyticsEventInsert, CoverageStateV1, ObservabilityEmissionClaimV1,
    ObservabilityRollupRebuildV1,
};

#[tokio::test]
async fn observability_append_is_idempotent_and_rejects_changed_input() {
    let harness = RegisteredGlobalDbHarness::open("observability-idempotency").await;
    let event = AnalyticsEventInsert {
        provider: "tracedecay-observability".to_string(),
        project_id: "scope:fixture".to_string(),
        session_id: None,
        timestamp: 1,
        event_kind: "retrieval.query.completed.v1".to_string(),
        hook_name: None,
        tool_name: None,
        tool_category: None,
        skill_name: None,
        hint_category: None,
        hint_id: Some("idempotency:fixture".to_string()),
        outcome: Some("succeeded".to_string()),
        metadata_json: Some("{\"canonical\":true}".to_string()),
    };
    let first = harness
        .registered
        .append_observability_event(&event)
        .await
        .expect("first append");
    let replay = harness
        .registered
        .append_observability_event(&event)
        .await
        .expect("idempotent replay");
    assert_eq!(first, replay);

    let mut changed_timestamp = event.clone();
    changed_timestamp.timestamp = 86_401;
    let error = harness
        .registered
        .append_observability_event(&changed_timestamp)
        .await
        .expect_err("same metadata on a changed day must conflict");
    assert!(error.contains("idempotency conflict"), "{error}");

    let mut changed_kind = event.clone();
    changed_kind.event_kind = "work.execution_topology.sampled.v1".to_owned();
    let error = harness
        .registered
        .append_observability_event(&changed_kind)
        .await
        .expect_err("same metadata with a changed event kind must conflict");
    assert!(error.contains("idempotency conflict"), "{error}");

    let mut changed = event;
    changed.metadata_json = Some("{\"canonical\":false}".to_string());
    let error = harness
        .registered
        .append_observability_event(&changed)
        .await
        .expect_err("changed canonical input must conflict");
    assert!(error.contains("idempotency conflict"), "{error}");
    assert_eq!(
        harness
            .registered
            .count_analytics_events(Some("scope:fixture"), 0)
            .await
            .expect("event count"),
        1
    );
}

#[tokio::test]
async fn observability_outbox_replay_reuses_exact_delivery_and_settles_atomically() {
    let harness = RegisteredGlobalDbHarness::open("observability-outbox-replay").await;
    let project = "scope:outbox";
    let owner_event = "owner:transition:1";
    let owner_fact = r#"{"owner":"receipt:1","result":"succeeded"}"#;
    let delivery = r#"{"delivery":"boot-a:1"}"#;
    let changed_delivery = r#"{"delivery":"boot-b:99"}"#;
    assert!(matches!(
        harness
            .registered
            .claim_observability_emission(project, owner_event, owner_fact, delivery)
            .await
            .expect("claim outbox"),
        ObservabilityEmissionClaimV1::Claimed { .. }
    ));
    let replay = harness
        .registered
        .claim_observability_emission(project, owner_event, owner_fact, changed_delivery)
        .await
        .expect("pending replay");
    assert_eq!(replay.delivery_envelope_json(), delivery);

    let event = AnalyticsEventInsert {
        provider: "tracedecay-observability".to_owned(),
        project_id: project.to_owned(),
        session_id: None,
        timestamp: 1,
        event_kind: "work.integration.transition.observed.v1".to_owned(),
        hook_name: None,
        tool_name: None,
        tool_category: None,
        skill_name: None,
        hint_category: None,
        hint_id: Some(owner_event.to_owned()),
        outcome: Some("succeeded".to_owned()),
        metadata_json: Some(delivery.to_owned()),
    };
    let settled = harness
        .registered
        .settle_observability_emission(project, owner_event, owner_fact, delivery, &event)
        .await
        .expect("settle outbox");
    let replay = harness
        .registered
        .claim_observability_emission(project, owner_event, owner_fact, changed_delivery)
        .await
        .expect("settled replay");
    assert_eq!(
        replay,
        ObservabilityEmissionClaimV1::Settled {
            delivery_envelope_json: delivery.to_owned(),
            analytics_event_id: settled,
        }
    );
    assert_eq!(
        harness
            .registered
            .count_analytics_events(Some(project), 0)
            .await
            .expect("event count"),
        1
    );
}

#[tokio::test]
async fn observability_outbox_refuses_changed_owner_and_preserves_pending_on_failed_settle() {
    let harness = RegisteredGlobalDbHarness::open("observability-outbox-atomicity").await;
    let project = "scope:outbox-atomic";
    let owner_event = "owner:transition:atomic";
    let owner_fact = r#"{"owner":"receipt:atomic"}"#;
    let delivery = r#"{"delivery":"boot:1"}"#;
    harness
        .registered
        .claim_observability_emission(project, owner_event, owner_fact, delivery)
        .await
        .expect("claim outbox");
    let changed = harness
        .registered
        .claim_observability_emission(
            project,
            owner_event,
            r#"{"owner":"receipt:changed"}"#,
            delivery,
        )
        .await
        .expect_err("changed owner fact must conflict");
    assert!(changed.contains("owner fact conflict"), "{changed}");

    let invalid_event = AnalyticsEventInsert {
        provider: "wrong-provider".to_owned(),
        project_id: project.to_owned(),
        session_id: None,
        timestamp: 1,
        event_kind: "work.integration.transition.observed.v1".to_owned(),
        hook_name: None,
        tool_name: None,
        tool_category: None,
        skill_name: None,
        hint_category: None,
        hint_id: Some(owner_event.to_owned()),
        outcome: None,
        metadata_json: Some(delivery.to_owned()),
    };
    harness
        .registered
        .settle_observability_emission(project, owner_event, owner_fact, delivery, &invalid_event)
        .await
        .expect_err("invalid append rolls back settlement");
    let pending = harness
        .registered
        .pending_observability_emissions(project, 8)
        .await
        .expect("pending outbox");
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].owner_event_id, owner_event);
    assert_eq!(pending[0].delivery_envelope_json, delivery);
    assert_eq!(
        harness
            .registered
            .count_analytics_events(Some(project), 0)
            .await
            .expect("event count"),
        0
    );
}

#[tokio::test]
async fn observability_retention_expires_settled_transport_but_preserves_pending_and_product_receipts_across_restart()
 {
    let harness = RegisteredGlobalDbHarness::open("observability-outbox-retention").await;
    let project = "scope:outbox-retention";
    let pending_fact = r#"{"owner":"pending"}"#;
    let pending_delivery = r#"{"delivery":"pending","retention_class":"optional_local_detail30d"}"#;
    harness
        .registered
        .claim_observability_emission(project, "owner:pending", pending_fact, pending_delivery)
        .await
        .expect("claim pending transport");

    let settle = |owner_event: &str, retention_class: &str| {
        let delivery =
            format!(r#"{{"delivery":"{owner_event}","retention_class":"{retention_class}"}}"#);
        let event = AnalyticsEventInsert {
            provider: "tracedecay-observability".to_owned(),
            project_id: project.to_owned(),
            session_id: None,
            timestamp: 0,
            event_kind: "retrieval.query.completed.v1".to_owned(),
            hook_name: None,
            tool_name: None,
            tool_category: None,
            skill_name: None,
            hint_category: None,
            hint_id: Some(owner_event.to_owned()),
            outcome: Some("succeeded".to_owned()),
            metadata_json: Some(delivery.clone()),
        };
        (delivery, event)
    };
    let detail_fact = r#"{"owner":"detail"}"#;
    let (detail_delivery, detail_event) = settle("owner:detail", "optional_local_detail30d");
    harness
        .registered
        .claim_observability_emission(project, "owner:detail", detail_fact, &detail_delivery)
        .await
        .expect("claim detail transport");
    harness
        .registered
        .settle_observability_emission(
            project,
            "owner:detail",
            detail_fact,
            &detail_delivery,
            &detail_event,
        )
        .await
        .expect("settle detail transport");
    let product_fact = r#"{"owner":"product"}"#;
    let (product_delivery, product_event) = settle("owner:product", "product_receipt");
    harness
        .registered
        .claim_observability_emission(project, "owner:product", product_fact, &product_delivery)
        .await
        .expect("claim product transport");
    harness
        .registered
        .settle_observability_emission(
            project,
            "owner:product",
            product_fact,
            &product_delivery,
            &product_event,
        )
        .await
        .expect("settle product transport");

    let receipt = harness
        .registered
        .prune_observability_events(31 * 86_400)
        .await
        .expect("bounded observability retention");
    assert_eq!(receipt.expired_detail, 1);
    assert_eq!(receipt.expired_rollup, 0);
    assert_eq!(receipt.expired_settled_outbox, 1);
    assert!(!receipt.has_more);

    let harness = harness.restart().await;
    let pending = harness
        .registered
        .pending_observability_emissions(project, 8)
        .await
        .expect("pending transports after restart");
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].owner_event_id, "owner:pending");
    assert!(
        harness
            .registered
            .observability_emission_claim(project, "owner:detail", detail_fact)
            .await
            .expect("expired detail lookup")
            .is_none()
    );
    assert!(matches!(
        harness
            .registered
            .observability_emission_claim(project, "owner:product", product_fact)
            .await
            .expect("product receipt lookup"),
        Some(ObservabilityEmissionClaimV1::Settled { .. })
    ));
}

#[tokio::test]
async fn observability_retention_is_bounded_and_cancelled_waits_do_not_mutate() {
    let harness = RegisteredGlobalDbHarness::open("observability-retention-bounds").await;
    let events = (0..=512)
        .map(|index| AnalyticsEventInsert {
            provider: "tracedecay-observability".to_owned(),
            project_id: "scope:retention-bounds".to_owned(),
            session_id: None,
            timestamp: 0,
            event_kind: "retrieval.query.completed.v1".to_owned(),
            hook_name: None,
            tool_name: None,
            tool_category: None,
            skill_name: None,
            hint_category: None,
            hint_id: Some(format!("retention:bounded:{index}")),
            outcome: Some("succeeded".to_owned()),
            metadata_json: Some(r#"{"retention_class":"optional_local_detail30d"}"#.to_owned()),
        })
        .collect::<Vec<_>>();
    harness
        .registered
        .append_analytics_events(&events)
        .await
        .expect("append bounded retention population");
    let first = harness
        .registered
        .prune_observability_events(31 * 86_400)
        .await
        .expect("first bounded page");
    assert_eq!(first.expired_detail, 512);
    assert!(first.has_more);
    let second = harness
        .registered
        .prune_observability_events(31 * 86_400)
        .await
        .expect("second bounded page");
    assert_eq!(second.expired_detail, 1);
    assert!(!second.has_more);

    harness
        .registered
        .append_observability_event(&events[0])
        .await
        .expect("restore one cancellable event");
    let blocker = harness
        .registered
        .begin_write_transaction()
        .await
        .expect("hold registered writer");
    let database = std::sync::harness.registered.clone();
    let prune = tokio::spawn(async move { database.prune_observability_events(31 * 86_400).await });
    tokio::task::yield_now().await;
    prune.abort();
    let _ = prune.await;
    blocker.commit().await.expect("release registered writer");
    assert_eq!(
        harness
            .registered
            .count_analytics_events(Some("scope:retention-bounds"), 0)
            .await
            .expect("cancelled retention count"),
        1
    );
}

#[tokio::test]
async fn observability_retention_preserves_dirty_sources_until_rollup_publication() {
    let harness = RegisteredGlobalDbHarness::open("observability-retention-dirty-source").await;
    let scope = "scope:retention-dirty";
    let event = |kind: &str, id: &str| AnalyticsEventInsert {
        provider: "tracedecay-observability".to_owned(),
        project_id: scope.to_owned(),
        session_id: None,
        timestamp: 0,
        event_kind: kind.to_owned(),
        hook_name: None,
        tool_name: None,
        tool_category: None,
        skill_name: None,
        hint_category: None,
        hint_id: Some(id.to_owned()),
        outcome: Some("succeeded".to_owned()),
        metadata_json: Some(r#"{"retention_class":"optional_local_detail30d"}"#.to_owned()),
    };
    let source_id = harness
        .registered
        .append_observability_event(&event(
            "work.execution_topology.sampled.v1",
            "dirty:topology",
        ))
        .await
        .expect("append dirty topology source");
    harness
        .registered
        .append_observability_event(&event("retrieval.query.completed.v1", "dirty:unrelated"))
        .await
        .expect("append old unrelated detail");

    let protected = harness
        .registered
        .prune_observability_events(31 * 86_400)
        .await
        .expect("prune around dirty source");
    assert_eq!(protected.expired_detail, 1);
    assert!(!protected.has_more);
    assert_eq!(
        harness
            .registered
            .count_analytics_events(Some(scope), 0)
            .await
            .expect("count protected dirty source"),
        1
    );

    let claim = harness
        .registered
        .claim_observability_rollup_dirty_day(scope, "retention:test", 30)
        .await
        .expect("claim protected dirty day")
        .expect("dirty source must retain its marker");
    assert_eq!(claim.source_watermark, source_id);
    harness
        .registered
        .rebuild_observability_rollup(ObservabilityRollupRebuildV1 {
            authorized_scope_ref: scope.to_owned(),
            day_start_seconds: 0,
            projector_revision: "execution-topology-projector.v1".to_owned(),
            source_watermark: source_id,
            coverage: CoverageStateV1::Known,
            idempotency_key: "retention:dirty-source:1".to_owned(),
            dirty_claim: Some(claim),
            empty_day_claim: None,
            fragment_json: r#"{"kind":"execution_topology_rollup_fragment"}"#.to_owned(),
        })
        .await
        .expect("publish protected dirty day");

    let expired = harness
        .registered
        .prune_observability_events(31 * 86_400)
        .await
        .expect("prune published source");
    assert_eq!(expired.expired_detail, 1);
    assert_eq!(
        harness
            .registered
            .count_analytics_events(Some(scope), 0)
            .await
            .expect("count after rollup publication"),
        0
    );
}
