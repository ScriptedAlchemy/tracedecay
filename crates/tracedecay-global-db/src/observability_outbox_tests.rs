use crate::tests::harness::RegisteredGlobalDbHarness;
use crate::{
    AnalyticsEventInsert, CoverageStateV1, ObservabilityEmissionClaimV1,
    ObservabilityOwnerEmissionWriteOutcomeV1, ObservabilityOwnerEmissionWriteV1,
    ObservabilityRollupRebuildV1, PreparedObservabilityEmissionV1,
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
async fn observability_batch_appends_every_event() {
    const APPENDS: usize = 8;

    let harness = RegisteredGlobalDbHarness::open("observability-append-batch").await;
    let events = (0..APPENDS)
        .map(|index| AnalyticsEventInsert {
            provider: "tracedecay-observability".to_owned(),
            project_id: "scope:batch".to_owned(),
            session_id: None,
            timestamp: 1,
            event_kind: "retrieval.query.completed.v1".to_owned(),
            hook_name: None,
            tool_name: None,
            tool_category: None,
            skill_name: None,
            hint_category: None,
            hint_id: Some(format!("batch:{index}")),
            outcome: Some("succeeded".to_owned()),
            metadata_json: Some(format!("{{\"index\":{index}}}")),
        })
        .collect::<Vec<_>>();
    let ids = harness
        .registered
        .append_observability_events(&events)
        .await
        .expect("append observability batch");

    assert_eq!(ids.len(), APPENDS);
    assert_eq!(
        harness
            .registered
            .count_analytics_events(Some("scope:batch"), 0)
            .await
            .expect("count appended events"),
        i64::try_from(APPENDS).unwrap()
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
    let database = harness.registered.clone();
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

fn owner_write(
    project: &str,
    owner_event_id: &str,
    owner_fact_json: &str,
) -> ObservabilityOwnerEmissionWriteV1 {
    ObservabilityOwnerEmissionWriteV1 {
        project_id: project.to_owned(),
        owner_event_id: owner_event_id.to_owned(),
        owner_fact_json: owner_fact_json.to_owned(),
    }
}

fn prepared_delivery(
    project: &str,
    owner_event_id: &str,
    delivery: &str,
) -> PreparedObservabilityEmissionV1 {
    PreparedObservabilityEmissionV1 {
        delivery_envelope_json: delivery.to_owned(),
        event: AnalyticsEventInsert {
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
            hint_id: Some(owner_event_id.to_owned()),
            outcome: Some("succeeded".to_owned()),
            metadata_json: Some(delivery.to_owned()),
        },
    }
}

#[tokio::test]
async fn owner_fact_run_preserves_siblings_when_an_owner_fact_conflicts() {
    let harness = RegisteredGlobalDbHarness::open("observability-owner-run").await;
    let project = "scope:owner-run";
    let first = owner_write(project, "owner:first", r#"{"owner":"first"}"#);
    let second = owner_write(project, "owner:second", r#"{"owner":"second"}"#);
    let outcomes = harness
        .registered
        .claim_and_settle_observability_emissions(&[first.clone(), second.clone()], |index| {
            let owner_event_id = if index == 0 {
                "owner:first"
            } else {
                "owner:second"
            };
            Ok(prepared_delivery(
                project,
                owner_event_id,
                &format!(r#"{{"delivery":"{owner_event_id}"}}"#),
            ))
        })
        .await
        .expect("settle new owner facts");
    assert!(matches!(
        outcomes.first(),
        Some(ObservabilityOwnerEmissionWriteOutcomeV1::Settled { .. })
    ));
    assert!(matches!(
        outcomes.get(1),
        Some(ObservabilityOwnerEmissionWriteOutcomeV1::Settled { .. })
    ));

    let sibling = owner_write(project, "owner:sibling", r#"{"owner":"sibling"}"#);
    let changed = owner_write(project, "owner:first", r#"{"owner":"changed"}"#);
    let conflict = harness
        .registered
        .claim_and_settle_observability_emissions(
            &[first.clone(), sibling.clone(), changed],
            |_| {
                Ok(prepared_delivery(
                    project,
                    "owner:sibling",
                    r#"{"delivery":"sibling"}"#,
                ))
            },
        )
        .await
        .expect("independent facts commit");
    assert!(matches!(
        conflict[0],
        ObservabilityOwnerEmissionWriteOutcomeV1::Replayed
    ));
    assert!(matches!(
        conflict[1],
        ObservabilityOwnerEmissionWriteOutcomeV1::Settled { .. }
    ));
    assert!(matches!(
        &conflict[2],
        ObservabilityOwnerEmissionWriteOutcomeV1::Rejected { error }
            if error.contains("owner fact conflict")
    ));
    assert!(
        harness
            .registered
            .read_observability_event(project, "owner:sibling")
            .await
            .expect("sibling lookup")
            .is_some(),
        "a rejected owner fact must not discard its valid sibling"
    );
    let stored = harness
        .registered
        .read_observability_event(project, "owner:first")
        .await
        .expect("first lookup")
        .expect("first delivery remains");
    assert_eq!(
        stored.metadata_json.as_deref(),
        Some(r#"{"delivery":"owner:first"}"#)
    );

    let fresh = owner_write(project, "owner:fresh", r#"{"owner":"fresh"}"#);
    let mut prepared_fresh = false;
    let replayed = harness
        .registered
        .claim_and_settle_observability_emissions(&[first, second, fresh], |index| {
            assert_eq!(index, 2, "replays must not prepare a new delivery");
            prepared_fresh = true;
            Ok(prepared_delivery(
                project,
                "owner:fresh",
                r#"{"delivery":"fresh"}"#,
            ))
        })
        .await
        .expect("replay settled facts and settle the new one");
    assert!(matches!(
        replayed.first(),
        Some(ObservabilityOwnerEmissionWriteOutcomeV1::Replayed)
    ));
    assert!(prepared_fresh);
    assert_eq!(
        harness
            .registered
            .read_observability_event(project, "owner:first")
            .await
            .expect("replay lookup")
            .expect("replayed delivery")
            .metadata_json
            .as_deref(),
        Some(r#"{"delivery":"owner:first"}"#)
    );
}

#[tokio::test]
async fn owner_fact_storage_failure_rolls_back_the_entire_transaction() {
    let harness = RegisteredGlobalDbHarness::open("observability-owner-storage-failure").await;
    let project = "scope:owner-storage-failure";
    let emissions = [
        owner_write(project, "owner:first", r#"{"owner":"first"}"#),
        owner_write(project, "owner:second", r#"{"owner":"second"}"#),
    ];
    let transaction = harness.registered.begin_write_transaction().await.unwrap();
    transaction
        .execute_batch(
            "CREATE TRIGGER fail_owner_insert BEFORE INSERT ON observability_emission_outbox
             WHEN NEW.owner_event_id = 'owner:second'
             BEGIN SELECT RAISE(ABORT, 'test owner storage failure'); END;",
        )
        .await
        .unwrap();
    transaction.commit().await.unwrap();

    let error = harness
        .registered
        .claim_and_settle_observability_emissions(&emissions, |index| {
            Ok(prepared_delivery(
                project,
                &emissions[index].owner_event_id,
                r#"{"delivery":"owner"}"#,
            ))
        })
        .await
        .expect_err("storage failure aborts the batch");
    assert_eq!(
        error,
        "failed to settle observability outbox events: SQLite execute failed: test owner storage failure"
    );
    for emission in &emissions {
        assert!(
            harness
                .registered
                .read_observability_event(project, &emission.owner_event_id)
                .await
                .unwrap()
                .is_none(),
            "neither the earlier sibling nor the failed insertion may survive rollback"
        );
        assert!(
            harness
                .registered
                .observability_emission_claim(
                    project,
                    &emission.owner_event_id,
                    &emission.owner_fact_json,
                )
                .await
                .unwrap()
                .is_none(),
            "rollback must leave no replay claim"
        );
    }
}

#[tokio::test]
async fn owner_fact_run_resolves_repeats_claims_and_stored_events_within_one_write() {
    let harness = RegisteredGlobalDbHarness::open("observability-owner-run-resolution").await;
    let project = "scope:owner-run-resolution";
    harness
        .registered
        .claim_observability_emission(
            project,
            "owner:pending",
            r#"{"owner":"pending"}"#,
            r#"{"delivery":"pending"}"#,
        )
        .await
        .expect("pending claim");
    let adopted = harness
        .registered
        .append_observability_event(
            &prepared_delivery(project, "owner:adopted", r#"{"delivery":"adopted"}"#).event,
        )
        .await
        .expect("stored event without an outbox row");
    harness
        .registered
        .append_observability_event(
            &prepared_delivery(project, "owner:foreign", r#"{"delivery":"foreign"}"#).event,
        )
        .await
        .expect("foreign stored event");

    let run = [
        owner_write(project, "owner:new", r#"{"owner":"new"}"#),
        owner_write(project, "owner:new", r#"{"owner":"new"}"#),
        owner_write(project, "owner:new", r#"{"owner":"changed"}"#),
        owner_write(project, "owner:pending", r#"{"owner":"pending"}"#),
        owner_write(project, "owner:pending", r#"{"owner":"changed"}"#),
        owner_write(project, "owner:adopted", r#"{"owner":"adopted"}"#),
        owner_write(project, "owner:foreign", r#"{"owner":"foreign"}"#),
        owner_write(project, "", r#"{"owner":"invalid"}"#),
    ];
    let mut prepared = Vec::new();
    let outcomes = harness
        .registered
        .claim_and_settle_observability_emissions(&run, |index| {
            prepared.push(index);
            let owner_event_id = run[index].owner_event_id.as_str();
            let delivery = match owner_event_id {
                "owner:adopted" => r#"{"delivery":"adopted"}"#.to_owned(),
                _ => format!(r#"{{"delivery":"{owner_event_id}:run"}}"#),
            };
            Ok(prepared_delivery(project, owner_event_id, &delivery))
        })
        .await
        .expect("owner fact run");

    assert_eq!(prepared, vec![0, 5, 6]);
    let new_id = harness
        .registered
        .read_observability_event(project, "owner:new")
        .await
        .expect("new lookup")
        .expect("new delivery")
        .id;
    let rejected = |error: &str| ObservabilityOwnerEmissionWriteOutcomeV1::Rejected {
        error: error.to_owned(),
    };
    assert_eq!(
        outcomes,
        vec![
            ObservabilityOwnerEmissionWriteOutcomeV1::Settled {
                analytics_event_id: new_id,
            },
            ObservabilityOwnerEmissionWriteOutcomeV1::Replayed,
            rejected("observability owner fact conflict"),
            ObservabilityOwnerEmissionWriteOutcomeV1::Replayed,
            rejected("observability owner fact conflict"),
            ObservabilityOwnerEmissionWriteOutcomeV1::Settled {
                analytics_event_id: adopted,
            },
            rejected("observability idempotency conflict"),
            rejected("invalid observability outbox input"),
        ]
    );
    assert!(matches!(
        harness
            .registered
            .observability_emission_claim(project, "owner:adopted", r#"{"owner":"adopted"}"#)
            .await
            .expect("adopted claim"),
        Some(ObservabilityEmissionClaimV1::Settled { .. })
    ));
    assert!(matches!(
        harness
            .registered
            .observability_emission_claim(project, "owner:pending", r#"{"owner":"pending"}"#)
            .await
            .expect("pending claim"),
        Some(ObservabilityEmissionClaimV1::Pending { .. })
    ));
    assert_eq!(
        harness
            .registered
            .observability_emission_claim(project, "owner:foreign", r#"{"owner":"foreign"}"#)
            .await
            .expect("foreign claim"),
        None
    );
    assert_eq!(
        harness
            .registered
            .count_analytics_events(Some(project), 0)
            .await
            .expect("event count"),
        3
    );
}
