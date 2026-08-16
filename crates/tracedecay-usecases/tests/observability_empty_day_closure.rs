use std::sync::Arc;
use std::time::Duration;

use tracedecay_domain::CoverageStateV1;
use tracedecay_usecases::observability::{
    BoundedObservabilityProducerV1, ObservabilityProducerIdentityV1,
};

#[tokio::test]
async fn idle_producer_publishes_one_proved_completed_quiet_day() {
    let _pin = tracedecay_runtime_core::config::PinnedUserDataDir::new();
    let project = tempfile::tempdir().expect("project");
    let scope = "project.observability.empty.v2";
    let runtime = tracedecay_global_db::tests::harness::RegisteredGlobalDbTestRuntime::project(
        tracedecay_runtime_core::storage::default_profile_root().expect("profile root"),
        project.path(),
        tracedecay_domain::ProjectId::new(scope).expect("project identifier"),
    )
    .await
    .expect("registered runtime");
    let db = runtime.project_database_arc().expect("project database");
    let completed_day = 86_400_i64;
    let transaction = db
        .begin_write_transaction()
        .await
        .expect("begin frontier fixture");
    transaction
        .execute(
            "INSERT INTO observability_rollup_frontiers
                 (scope_ref, coverage_start_day_seconds, next_day_start_seconds)
             VALUES (?1, ?2, ?2)",
            tracedecay_runtime_core::db::engine::params![scope, completed_day],
        )
        .await
        .expect("seed exact installed frontier");
    transaction.commit().await.expect("commit frontier fixture");

    let producer = BoundedObservabilityProducerV1::start(
        db.clone(),
        ObservabilityProducerIdentityV1 {
            authorized_scope_ref: scope.to_owned(),
            process_boot_id: "boot:empty-day".to_owned(),
            producer_revision: "producer.rollup.v1".to_owned(),
            configuration_revision: "configuration.rollup.v1".to_owned(),
            policy_revision: "policy.rollup.v1".to_owned(),
        },
        4,
    )
    .expect("producer");
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let page = db
                .query_observability_rollup_fragments(
                    &tracedecay_global_db::ObservabilityRollupFragmentQueryV1 {
                        authorized_scope_ref: scope.to_owned(),
                        since_day_start_seconds: completed_day,
                        until_day_start_seconds: completed_day + 86_400,
                    },
                )
                .await
                .expect("query quiet-day fragment");
            if page.coverage == CoverageStateV1::Known && page.fragments.len() == 1 {
                assert_eq!(page.fragments[0].source_watermark, 0);
                assert!(page.fragments[0].fragment_json.contains("analytics:empty"));
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("idle producer must close a proved quiet completed day");
    producer.shutdown().await.expect("producer shutdown");
}
