//! Retry real table measurements after logical reclamation, without compaction.

#![allow(clippy::expect_used, clippy::panic)]

use std::collections::BTreeSet;

use tracedecay_contracts::now_micros;
use tracedecay_contracts::storage::{
    StorageTelemetryReadV1, StoreSizeTelemetryPort, TableGrowthTelemetryReadV1,
    TableGrowthUnavailableReasonV1,
};
use tracedecay_runtime_core::cancellation::CancellationToken;

use super::table_growth_tests::growth_fixture;
use super::{
    StoreTelemetrySamplingRegistry, TableSizeWalk, settle_table_size_walk,
    storage_telemetry_request_context,
};

#[tokio::test]
async fn logical_reclamation_retries_a_timed_out_census_without_shrinking_the_file() {
    let temporary = tempfile::tempdir().expect("telemetry reclamation fixture");
    let database_path = temporary.path().join("project.db");
    let (database, scope) = growth_fixture(&database_path).await;
    database
        .execute_write_batch(
            "grow reclaimable payload",
            "WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM n WHERE x < 32) \
             INSERT INTO growth_payload SELECT zeroblob(262144) FROM n;",
        )
        .await
        .expect("grow a real table before its baseline");

    let registry = StoreTelemetrySamplingRegistry::default();
    assert!(registry.register_port(&database_path, &scope, || {
        database.storage_telemetry_handle()
    }));
    let paths = BTreeSet::from([database_path.clone()]);
    let cancellation = CancellationToken::new();
    let baseline = registry
        .advance_registered(&paths, &paths, &cancellation)
        .await;
    assert_eq!(baseline.observed, 1);
    assert_eq!(baseline.unavailable, 0);
    let (store, port) = registry
        .registered_port(&database_path, &scope)
        .expect("registered measurement port");
    let context = storage_telemetry_request_context(scope.clone()).expect("reader context");
    let TableGrowthTelemetryReadV1::BaselineEstablished { observed_at, .. } =
        port.latest_table_growth(&context, &store)
    else {
        panic!("the first pass must retain a real baseline");
    };
    let StorageTelemetryReadV1::Observed { sample: before } =
        port.store_size(&context, &store).await
    else {
        panic!("the pre-reclamation size must be measured");
    };
    let occupied_before = before.page_count - before.freelist_pages;
    assert!(occupied_before > 0);

    // Seed the production timeout transition deterministically; reproducing a
    // wall-clock timeout would make this correctness test depend on host speed.
    let TableSizeWalk::OverBudget(mark) = settle_table_size_walk(
        Err(TableGrowthUnavailableReasonV1::DeadlineExceeded),
        occupied_before,
        now_micros(),
    )
    .unwrap() else {
        panic!("a deadline must produce a retry-suppression mark");
    };
    port.table_growth.lock().unwrap().over_budget = Some(mark);
    let unchanged = registry
        .advance_registered(&paths, &paths, &cancellation)
        .await;
    assert_eq!(unchanged.observed, 0);
    assert_eq!(unchanged.unavailable, 1);
    assert!(matches!(
        port.latest_table_growth(&context, &store),
        TableGrowthTelemetryReadV1::Unknown {
            reason: TableGrowthUnavailableReasonV1::ExceedsMeasurementBudget,
            ..
        }
    ));

    database
        .execute_write_batch(
            "reclaim table payload through its writer",
            "DELETE FROM growth_payload WHERE rowid > 1;",
        )
        .await
        .expect("logical deletion without VACUUM or restart");
    let context = storage_telemetry_request_context(scope.clone()).expect("reader context");
    let StorageTelemetryReadV1::Observed { sample: after } =
        port.store_size(&context, &store).await
    else {
        panic!("the post-reclamation size must be measured");
    };
    assert!(
        after.page_count >= before.page_count,
        "the old allocated-page guard must still suppress this fixture"
    );
    assert!(after.freelist_pages > before.freelist_pages);
    assert!(after.page_count - after.freelist_pages < occupied_before);

    let retried = registry
        .advance_registered(&paths, &paths, &cancellation)
        .await;
    assert_eq!(retried.observed, 1);
    assert_eq!(retried.unavailable, 0);
    let TableGrowthTelemetryReadV1::Observed { samples, .. } =
        port.latest_table_growth(&context, &store)
    else {
        panic!("retry must compare with the original baseline, not replace it");
    };
    let retained = samples
        .iter()
        .find(|sample| sample.table.as_str() == "growth_payload")
        .expect("the retained table remains measured");
    assert_eq!(retained.previous_observed_at, observed_at);
    assert!(retained.previous_bytes.get() >= 8 * 1024 * 1024);
    assert!(retained.current_bytes.get() >= 262_144);
    assert!(retained.current_bytes.get() < retained.previous_bytes.get());
    let retained_bytes = retained.current_bytes;
    assert_eq!(port.table_growth_state().over_budget, None);

    database
        .execute_write_batch(
            "grow the retained table again",
            "INSERT INTO growth_payload VALUES (zeroblob(262144));",
        )
        .await
        .expect("growth after reclamation");
    let resumed = registry
        .advance_registered(&paths, &paths, &cancellation)
        .await;
    assert_eq!(resumed.observed, 1);
    assert_eq!(resumed.unavailable, 0);
    let context = storage_telemetry_request_context(scope).expect("reader context");
    let TableGrowthTelemetryReadV1::Observed { samples, .. } =
        port.latest_table_growth(&context, &store)
    else {
        panic!("subsequent growth must remain observable");
    };
    let grown = samples
        .iter()
        .find(|sample| sample.table.as_str() == "growth_payload")
        .expect("the regrown table is measured");
    assert_eq!(grown.previous_bytes, retained_bytes);
    assert!(grown.growth_bytes().get() >= 262_144);
}
