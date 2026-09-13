use crate::tests::harness::RegisteredGlobalDbHarness;
use crate::{
    AnalyticsEventInsert, CoverageStateV1, ObservabilityRollupCompactionV1,
    ObservabilityRollupEmptyDayClaimOutcomeV1, ObservabilityRollupFragmentQueryV1,
    ObservabilityRollupRebuildV1,
};

const DAY: i64 = 86_400;

fn cell(metric: &str, dimensions_json: &str, eligible: u64) -> serde_json::Value {
    serde_json::json!({
        "dimensions": dimensions_json,
        "eligible": eligible,
        "metric": metric,
    })
}

fn topology_event(scope: &str, hint_id: &str, timestamp: i64) -> AnalyticsEventInsert {
    AnalyticsEventInsert {
        provider: "tracedecay-observability".to_owned(),
        project_id: scope.to_owned(),
        session_id: None,
        timestamp,
        event_kind: "work.execution_topology.sampled.v1".to_owned(),
        hook_name: None,
        tool_name: None,
        tool_category: None,
        skill_name: None,
        hint_category: None,
        hint_id: Some(hint_id.to_owned()),
        outcome: Some("succeeded".to_owned()),
        metadata_json: Some(format!(r#"{{"event":"{hint_id}"}}"#)),
    }
}

#[tokio::test]
async fn accepted_events_mark_days_stale_and_rebuild_settles_only_its_exact_claim() {
    let harness = RegisteredGlobalDbHarness::open("observability-rollup-dirty-cas").await;
    let first_id = harness
        .registered
        .append_observability_event(&topology_event("scope:rollup", "topology:1", 10 * DAY + 1))
        .await
        .expect("append first topology event");
    let replay_id = harness
        .registered
        .append_observability_event(&topology_event("scope:rollup", "topology:1", 10 * DAY + 1))
        .await
        .expect("replay first topology event");
    assert_eq!(replay_id, first_id);
    let first_claim = harness
        .registered
        .claim_observability_rollup_dirty_day("scope:rollup", "producer:one", 30)
        .await
        .expect("claim first dirty day")
        .expect("dirty day exists");
    assert_eq!(first_claim.day_start_seconds, 10 * DAY);
    assert_eq!(first_claim.source_watermark, first_id);

    let stale = harness
        .registered
        .query_observability_rollup_fragments(&ObservabilityRollupFragmentQueryV1 {
            authorized_scope_ref: "scope:rollup".to_owned(),
            since_day_start_seconds: 10 * DAY,
            until_day_start_seconds: 11 * DAY,
        })
        .await
        .expect("query stale fragment range");
    assert_eq!(stale.coverage, CoverageStateV1::Stale);
    assert!(stale.fragments.is_empty());
    let second_id = harness
        .registered
        .append_observability_event(&topology_event("scope:rollup", "topology:2", 10 * DAY + 2))
        .await
        .expect("append late topology event");
    let mut superseded = rebuild(
        "rebuild:superseded",
        first_claim.source_watermark,
        "execution-topology-projector.v1",
        vec![cell("metric-a", "[]", 5)],
    );
    superseded.dirty_claim = Some(first_claim);
    let error = harness
        .registered
        .rebuild_observability_rollup(superseded)
        .await
        .expect_err("superseded dirty claim must not publish");
    assert!(error.contains("superseded"), "{error}");

    let current_claim = harness
        .registered
        .claim_observability_rollup_dirty_day("scope:rollup", "producer:one", 30)
        .await
        .expect("claim corrected dirty day")
        .expect("corrected dirty day exists");
    assert_eq!(current_claim.source_watermark, second_id);
    let mut current = rebuild(
        "rebuild:current",
        current_claim.source_watermark,
        "execution-topology-projector.v1",
        vec![cell("metric-a", "[]", 5)],
    );
    current.dirty_claim = Some(current_claim);
    harness
        .registered
        .rebuild_observability_rollup(current)
        .await
        .expect("publish current dirty day");

    let page = harness
        .registered
        .query_observability_rollup_fragments(&ObservabilityRollupFragmentQueryV1 {
            authorized_scope_ref: "scope:rollup".to_owned(),
            since_day_start_seconds: 10 * DAY,
            until_day_start_seconds: 11 * DAY,
        })
        .await
        .expect("query settled fragment range");
    assert_eq!(page.coverage, CoverageStateV1::Known);
    assert_eq!(page.fragments.len(), 1);
}

#[tokio::test]
async fn dirty_day_lease_is_exclusive_then_reclaimable_after_expiry() {
    let harness = RegisteredGlobalDbHarness::open("observability-rollup-dirty-lease").await;
    let watermark = harness
        .registered
        .append_observability_event(&topology_event(
            "scope:rollup",
            "topology:lease",
            10 * DAY + 1,
        ))
        .await
        .expect("append topology event");
    let first = harness
        .registered
        .claim_observability_rollup_dirty_day("scope:rollup", "producer:first", 30)
        .await
        .expect("claim dirty day")
        .expect("dirty day exists");
    assert_eq!(first.source_watermark, watermark);
    let denied = harness
        .registered
        .claim_observability_rollup_dirty_day("scope:rollup", "producer:second", 30)
        .await
        .expect("inspect leased dirty day");
    assert!(denied.is_none());
    let transaction = harness
        .registered
        .begin_write_transaction()
        .await
        .expect("begin lease expiry fixture");
    transaction
        .execute(
            "UPDATE observability_rollup_dirty_days
             SET lease_until_seconds = unixepoch() - 1
             WHERE scope_ref = ?1 AND day_start_seconds = ?2",
            tracedecay_runtime_core::db::engine::params!["scope:rollup", 10 * DAY],
        )
        .await
        .expect("expire dirty-day lease");
    transaction.commit().await.expect("commit lease expiry");

    let mut expired_publish = rebuild(
        "rebuild:expired-lease",
        first.source_watermark,
        "execution-topology-projector.v1",
        vec![cell("metric-a", "[]", 5)],
    );
    expired_publish.dirty_claim = Some(first);
    let error = harness
        .registered
        .rebuild_observability_rollup(expired_publish)
        .await
        .expect_err("expired lease must not publish");
    assert!(error.contains("superseded"), "{error}");

    let reclaimed = harness
        .registered
        .claim_observability_rollup_dirty_day("scope:rollup", "producer:second", 30)
        .await
        .expect("reclaim expired dirty day")
        .expect("expired lease is reclaimable");
    assert_eq!(reclaimed.source_watermark, watermark);
    assert_eq!(reclaimed.claimant_id, "producer:second");
    assert!(
        harness
            .registered
            .release_observability_rollup_dirty_day(&reclaimed)
            .await
            .expect("release reclaimed dirty day")
    );
    assert!(
        harness
            .registered
            .claim_observability_rollup_dirty_day("scope:rollup", "producer:first", 30)
            .await
            .expect("claim released dirty day")
            .is_some()
    );
}

#[tokio::test]
async fn empty_day_frontier_starts_after_mount_advances_atomically_and_recovers_publication() {
    let harness = RegisteredGlobalDbHarness::open("observability-rollup-empty-frontier").await;
    let frontier = harness
        .registered
        .initialize_observability_rollup_frontier("scope:rollup")
        .await
        .expect("initialize first observable day");
    assert_eq!(
        harness
            .registered
            .initialize_observability_rollup_frontier("scope:rollup")
            .await
            .expect("replay first observable day"),
        frontier,
        "later mounts must not move the original coverage boundary"
    );
    let initialized = harness
        .registered
        .claim_observability_rollup_empty_day("scope:rollup", "producer:empty", 30)
        .await
        .expect("initialize empty-day frontier");
    let ObservabilityRollupEmptyDayClaimOutcomeV1::NotReady {
        coverage_start_day_seconds,
        next_day_start_seconds,
    } = initialized
    else {
        panic!("first mount must start at a future UTC boundary");
    };
    assert_eq!(
        coverage_start_day_seconds,
        frontier.coverage_start_day_seconds
    );
    assert_eq!(coverage_start_day_seconds, next_day_start_seconds);

    let transaction = harness
        .registered
        .begin_write_transaction()
        .await
        .expect("begin completed-day frontier fixture");
    transaction
        .execute(
            "UPDATE observability_rollup_frontiers
             SET coverage_start_day_seconds = ?2, next_day_start_seconds = ?2
             WHERE scope_ref = ?1",
            tracedecay_runtime_core::db::engine::params!["scope:rollup", 10 * DAY],
        )
        .await
        .expect("move frontier to completed fixture day");
    transaction
        .commit()
        .await
        .expect("commit completed-day frontier fixture");

    let ObservabilityRollupEmptyDayClaimOutcomeV1::Claimed(first_claim) = harness
        .registered
        .claim_observability_rollup_empty_day("scope:rollup", "producer:empty", 30)
        .await
        .expect("claim first completed empty day")
    else {
        panic!("completed quiet day must be claimable");
    };
    let mut first = rebuild(
        "empty-day:10",
        0,
        "execution-topology-projector.v1",
        vec![cell("metric-a", "[]", 0)],
    );
    first.empty_day_claim = Some(first_claim);
    harness
        .registered
        .rebuild_observability_rollup(first)
        .await
        .expect("publish and advance first empty day atomically");

    // Simulate publication succeeding before the frontier CAS in a crashed
    // process. The next claim advances that already-materialized day without
    // rebuilding or scanning it again.
    let mut crash_publication = rebuild(
        "empty-day:11:crash",
        0,
        "execution-topology-projector.v1",
        vec![cell("metric-a", "[]", 0)],
    );
    crash_publication.day_start_seconds = 11 * DAY;
    harness
        .registered
        .rebuild_observability_rollup(crash_publication)
        .await
        .expect("publish crash-window empty day");
    assert_eq!(
        harness
            .registered
            .claim_observability_rollup_empty_day("scope:rollup", "producer:empty", 30)
            .await
            .expect("recover published empty day"),
        ObservabilityRollupEmptyDayClaimOutcomeV1::AdvancedExisting {
            day_start_seconds: 11 * DAY
        }
    );

    let ObservabilityRollupEmptyDayClaimOutcomeV1::Claimed(revoked_claim) = harness
        .registered
        .claim_observability_rollup_empty_day("scope:rollup", "producer:empty", 30)
        .await
        .expect("claim next quiet day")
    else {
        panic!("next completed day must be claimable");
    };
    let event_id = harness
        .registered
        .append_observability_event(&topology_event(
            "scope:rollup",
            "topology:frontier-revoke",
            12 * DAY + 1,
        ))
        .await
        .expect("append event racing empty-day publication");
    let mut superseded = rebuild(
        "empty-day:12:superseded",
        0,
        "execution-topology-projector.v1",
        vec![cell("metric-a", "[]", 0)],
    );
    superseded.day_start_seconds = 12 * DAY;
    superseded.empty_day_claim = Some(revoked_claim);
    let error = harness
        .registered
        .rebuild_observability_rollup(superseded)
        .await
        .expect_err("new source must revoke empty-day publication");
    assert!(error.contains("superseded"), "{error}");
    let dirty = harness
        .registered
        .claim_observability_rollup_dirty_day("scope:rollup", "producer:dirty", 30)
        .await
        .expect("inspect racing dirty day")
        .expect("racing event must use normal dirty rebuild");
    assert_eq!(dirty.day_start_seconds, 12 * DAY);
    assert_eq!(dirty.source_watermark, event_id);
}

fn rebuild(
    idempotency_key: &str,
    watermark: i64,
    projector_revision: &str,
    test_measurements: Vec<serde_json::Value>,
) -> ObservabilityRollupRebuildV1 {
    ObservabilityRollupRebuildV1 {
        authorized_scope_ref: "scope:rollup".to_owned(),
        day_start_seconds: 10 * DAY,
        projector_revision: projector_revision.to_owned(),
        source_watermark: watermark,
        coverage: CoverageStateV1::Known,
        idempotency_key: idempotency_key.to_owned(),
        dirty_claim: None,
        empty_day_claim: None,
        fragment_json: serde_json::to_string(&serde_json::json!({
            "kind": "execution_topology_rollup_fragment",
            "schema_revision": 1,
            "test_measurements": test_measurements,
        }))
        .expect("serialize canonical fragment fixture"),
    }
}

#[tokio::test]
async fn internal_fragment_query_returns_canonical_merge_state_without_cell_suppression() {
    let harness = RegisteredGlobalDbHarness::open("observability-rollup-fragment").await;
    harness
        .registered
        .rebuild_observability_rollup(rebuild(
            "rebuild:fragment",
            10,
            "execution-topology-projector.v1",
            vec![cell("private", "[]", 1)],
        ))
        .await
        .expect("fragment rebuild");

    let page = harness
        .registered
        .query_observability_rollup_fragments(&ObservabilityRollupFragmentQueryV1 {
            authorized_scope_ref: "scope:rollup".to_owned(),
            since_day_start_seconds: 10 * DAY,
            until_day_start_seconds: 11 * DAY,
        })
        .await
        .expect("internal fragment query");
    assert_eq!(page.coverage, CoverageStateV1::Known);
    assert_eq!(page.fragments.len(), 1);
    assert_eq!(page.fragments[0].generation, 1);
    assert_eq!(page.fragments[0].source_watermark, 10);
    assert_eq!(
        page.fragments[0].fragment_json,
        r#"{"kind":"execution_topology_rollup_fragment","schema_revision":1,"test_measurements":[{"dimensions":"[]","eligible":1,"metric":"private"}]}"#
    );
}

#[tokio::test]
async fn capped_day_is_terminal_without_payload_until_a_new_source_marks_it_stale() {
    let harness = RegisteredGlobalDbHarness::open("observability-rollup-capped-day").await;
    let watermark = harness
        .registered
        .append_observability_event(&topology_event(
            "scope:rollup",
            "topology:capped:1",
            10 * DAY + 1,
        ))
        .await
        .expect("append capped source day");
    let claim = harness
        .registered
        .claim_observability_rollup_dirty_day("scope:rollup", "producer:capped", 30)
        .await
        .expect("claim capped source day")
        .expect("capped source day is dirty");
    let mut capped = rebuild(
        "rebuild:capped",
        watermark,
        "execution-topology-projector.v1",
        Vec::new(),
    );
    capped.coverage = CoverageStateV1::Capped;
    capped.fragment_json = r#"{"fixture":"opaque-capped-application-fragment"}"#.to_owned();
    capped.dirty_claim = Some(claim);
    let receipt = harness
        .registered
        .rebuild_observability_rollup(capped)
        .await
        .expect("publish terminal capped generation");
    assert_eq!(receipt.coverage, CoverageStateV1::Capped);

    let fragments = harness
        .registered
        .query_observability_rollup_fragments(&ObservabilityRollupFragmentQueryV1 {
            authorized_scope_ref: "scope:rollup".to_owned(),
            since_day_start_seconds: 10 * DAY,
            until_day_start_seconds: 11 * DAY,
        })
        .await
        .expect("query capped fragment day");
    assert_eq!(fragments.coverage, CoverageStateV1::Capped);
    assert_eq!(fragments.fragments.len(), 1);
    assert_eq!(
        fragments.fragments[0].source_watermark, watermark,
        "typed unavailable fragments retain their exact source watermark"
    );
    harness
        .registered
        .append_observability_event(&topology_event(
            "scope:rollup",
            "topology:capped:2",
            10 * DAY + 2,
        ))
        .await
        .expect("append newer source after capped publication");
    let stale = harness
        .registered
        .query_observability_rollup_fragments(&ObservabilityRollupFragmentQueryV1 {
            authorized_scope_ref: "scope:rollup".to_owned(),
            since_day_start_seconds: 10 * DAY,
            until_day_start_seconds: 11 * DAY,
        })
        .await
        .expect("query newly stale capped day");
    assert_eq!(stale.coverage, CoverageStateV1::Stale);
    assert!(stale.fragments.is_empty());
}

#[tokio::test]
async fn detail_compaction_is_bounded_exact_cas_and_generation_monotone() {
    let harness = RegisteredGlobalDbHarness::open("observability-rollup-detail-compaction").await;
    let mut publication = rebuild(
        "rebuild:detail-compaction",
        1,
        "execution-topology-projector.v1",
        vec![cell("metric-a", "[]", 5)],
    );
    publication.day_start_seconds = 0;
    publication.fragment_json = r#"{"carry":["protected-a"],"revision":1}"#.to_owned();
    harness
        .registered
        .rebuild_observability_rollup(publication)
        .await
        .expect("publish fragment with protected correction carry");
    let candidate = harness
        .registered
        .next_observability_rollup_compaction("scope:rollup")
        .await
        .expect("query expired detail compaction")
        .expect("expired detail candidate exists");
    let compacted_day = candidate.day_start_seconds;
    let request = ObservabilityRollupCompactionV1 {
        candidate,
        fragment_json: r#"{"carry":[],"revision":1,"unknown":1}"#.to_owned(),
    };
    let receipt = harness
        .registered
        .compact_observability_rollup_fragment(request.clone())
        .await
        .expect("CAS compact protected detail");
    assert_eq!(receipt.previous_generation, 1);
    assert_eq!(receipt.generation, 2);
    assert!(receipt.changed);
    let error = harness
        .registered
        .compact_observability_rollup_fragment(request)
        .await
        .expect_err("stale compaction cannot overwrite a newer generation");
    assert!(error.contains("candidate changed"), "{error}");
    assert!(
        harness
            .registered
            .next_observability_rollup_compaction("scope:rollup")
            .await
            .expect("query settled compaction")
            .is_none()
    );
    let fragments = harness
        .registered
        .query_observability_rollup_fragments(&ObservabilityRollupFragmentQueryV1 {
            authorized_scope_ref: "scope:rollup".to_owned(),
            since_day_start_seconds: compacted_day,
            until_day_start_seconds: compacted_day + DAY,
        })
        .await
        .expect("query compacted fragment");
    assert_eq!(fragments.fragments[0].generation, 2);
    assert_eq!(
        fragments.fragments[0].fragment_json,
        r#"{"carry":[],"revision":1,"unknown":1}"#
    );
}

#[tokio::test]
async fn rollup_rebuild_is_idempotent_version_monotone_and_late_correctable() {
    let harness = RegisteredGlobalDbHarness::open("observability-rollup-correction").await;
    let initial = rebuild(
        "rebuild:initial",
        10,
        "execution-topology-projector.v1",
        vec![cell("metric-a", "[]", 5)],
    );
    let first = harness
        .registered
        .rebuild_observability_rollup(initial.clone())
        .await
        .expect("initial rebuild");
    let replay = harness
        .registered
        .rebuild_observability_rollup(initial)
        .await
        .expect("exact replay");
    assert_eq!(first, replay);
    assert_eq!(first.generation, 1);
    assert!(!first.late_correction);

    let conflict = harness
        .registered
        .rebuild_observability_rollup(rebuild(
            "rebuild:initial",
            11,
            "execution-topology-projector.v1",
            vec![cell("metric-b", "[]", 5)],
        ))
        .await
        .expect_err("changed idempotency input must conflict");
    assert!(conflict.contains("idempotency conflict"), "{conflict}");

    let corrected = harness
        .registered
        .rebuild_observability_rollup(rebuild(
            "rebuild:late",
            11,
            "execution-topology-projector.v1",
            vec![cell("metric-b", "[]", 5)],
        ))
        .await
        .expect("late correction");
    assert_eq!(corrected.generation, 2);
    assert!(corrected.late_correction);

    let stale = harness
        .registered
        .rebuild_observability_rollup(rebuild(
            "rebuild:stale",
            10,
            "execution-topology-projector.v1",
            vec![cell("metric-a", "[]", 5)],
        ))
        .await
        .expect_err("older source watermark must be refused");
    assert!(stale.contains("stale source watermark"), "{stale}");

    let projector_rebuild = harness
        .registered
        .rebuild_observability_rollup(rebuild(
            "rebuild:projector-v2",
            11,
            "execution-topology-projector.v2",
            vec![cell("metric-b", "[]", 5)],
        ))
        .await
        .expect("same-frontier projector rebuild");
    assert_eq!(projector_rebuild.generation, 3);
    assert!(!projector_rebuild.late_correction);

    let page = harness
        .registered
        .query_observability_rollup_fragments(&ObservabilityRollupFragmentQueryV1 {
            authorized_scope_ref: "scope:rollup".to_owned(),
            since_day_start_seconds: 10 * DAY,
            until_day_start_seconds: 11 * DAY,
        })
        .await
        .expect("current generation query");
    assert_eq!(page.fragments.len(), 1);
    assert_eq!(page.fragments[0].generation, 3);
    assert_eq!(
        page.fragments[0].projector_revision,
        "execution-topology-projector.v2"
    );
}

#[tokio::test]
async fn retention_expires_old_daily_fragments() {
    let harness = RegisteredGlobalDbHarness::open("observability-rollup-query-cap-retention").await;
    harness
        .registered
        .rebuild_observability_rollup(rebuild(
            "rebuild:many",
            10,
            "execution-topology-projector.v1",
            vec![cell("bounded", "[]", 5)],
        ))
        .await
        .expect("bounded rollup");

    let receipt = harness
        .registered
        .prune_observability_rollups(406 * DAY)
        .await
        .expect("rollup retention");
    assert_eq!(receipt.expired_generations, 1);
    let empty = harness
        .registered
        .query_observability_rollup_fragments(&ObservabilityRollupFragmentQueryV1 {
            authorized_scope_ref: "scope:rollup".to_owned(),
            since_day_start_seconds: 10 * DAY,
            until_day_start_seconds: 11 * DAY,
        })
        .await
        .expect("post-retention query");
    assert!(empty.fragments.is_empty());
    assert_eq!(empty.coverage, CoverageStateV1::Partial);
}
