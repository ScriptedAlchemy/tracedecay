use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use serde_json::json;
use tempfile::{TempDir, tempdir};
use tracedecay_domain::{
    Confidence, FactCategoryV1, FactId, FactIdentityMaterialV1, FactIdentitySourceV1, FactOwnerV1,
    ProvenanceId, UtcMicros,
};
use tracedecay_store::{
    FactReadControl, FactStore, FactStoreError, FactWriteControl,
    ProjectMemoryDashboardFactDetailQueryV1, ProjectMemoryDashboardMemoryOverviewQueryV1,
    ProjectMemoryDashboardOplogQueryV1, ProjectMemoryDashboardVectorPointsQueryV1,
    ProjectMemoryFactFeedbackHistoryQueryV1, ProjectMemoryFactIdV1, ProjectMemoryFactStore,
};

use crate::db::{Database, DatabaseAuthority, TestDatabaseRuntimeMode};
use crate::store::memory::DatabaseFactStore;
use crate::store::memory::crud::{initial_batch, sanitize_payload};

async fn database(label: &str) -> (TempDir, Database) {
    let directory = tempdir().expect("create dashboard read-control fixture directory");
    let path = directory.path().join(format!("{label}.db"));
    let authority = DatabaseAuthority::acquire_test(&path, "dashboard read-control authority")
        .expect("acquire dashboard read-control authority");
    let (database, _) =
        Database::publish_test_runtime(&path, &authority, TestDatabaseRuntimeMode::Initialize)
            .await
            .expect("publish dashboard read-control runtime");
    (directory, database)
}

fn write_control() -> FactWriteControl {
    FactWriteControl::new(Arc::new(|| false), Arc::new(|| true))
}

fn interrupt_on_check(cancel_at: usize) -> (Arc<AtomicUsize>, FactReadControl) {
    let checks = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&checks);
    let control = FactReadControl::new(Arc::new(move || {
        observed.fetch_add(1, Ordering::AcqRel) + 1 >= cancel_at
    }));
    (checks, control)
}

async fn seed_fact(database: &Database) {
    let sanitized = sanitize_payload(
        "dashboard cancellation uses canonical on-read FHRR",
        FactCategoryV1::Project,
        &["dashboard".to_owned()],
        &["FHRR".to_owned()],
        &json!({"fixture": "dashboard-read-control"}),
        None,
    )
    .expect("sanitize dashboard fixture payload")
    .expect("dashboard fixture payload remains durable");
    let batch = initial_batch(
        &FactOwnerV1::Profile,
        &ProvenanceId::new("dashboard.read-control.seed".to_owned())
            .expect("dashboard fixture operation id"),
        sanitized.payload,
        sanitized.access,
        Confidence::new(0.8).expect("dashboard fixture confidence"),
        None,
        UtcMicros(1_000_000),
    )
    .expect("dashboard fixture batch");
    DatabaseFactStore::new(database)
        .commit_fact(batch, &write_control())
        .await
        .expect("commit dashboard fixture fact");
}

#[tokio::test]
async fn overview_observes_live_cancellation_between_sql_and_projection_stages() {
    let (_directory, database) = database("overview-cancellation").await;
    let (checks, read_control) = interrupt_on_check(4);
    let query = ProjectMemoryDashboardMemoryOverviewQueryV1::new(FactOwnerV1::Profile, 8, 8)
        .expect("dashboard overview query");

    let result = DatabaseFactStore::new(&database)
        .dashboard_project_memory_overview(query, &read_control)
        .await;

    assert!(matches!(result, Err(FactStoreError::ReadCancelled)));
    assert!(checks.load(Ordering::Acquire) >= 4);
}

#[tokio::test]
async fn vector_points_observe_cancellation_after_per_fact_encoding() {
    let (_directory, database) = database("vector-cancellation").await;
    seed_fact(&database).await;
    let (checks, read_control) = interrupt_on_check(6);
    let query = ProjectMemoryDashboardVectorPointsQueryV1::new(FactOwnerV1::Profile, None, 8)
        .expect("dashboard vector query");

    let result = DatabaseFactStore::new(&database)
        .dashboard_project_memory_vector_points(query, &read_control)
        .await;

    assert!(matches!(result, Err(FactStoreError::ReadCancelled)));
    assert!(checks.load(Ordering::Acquire) >= 6);
}

#[tokio::test]
async fn status_detail_feedback_and_oplog_observe_live_read_control() {
    let (_directory, database) = database("remaining-dashboard-cancellation").await;
    let store = DatabaseFactStore::new(&database);
    let target = ProjectMemoryFactIdV1::new(
        FactOwnerV1::Profile,
        FactId::derive(
            &FactIdentityMaterialV1::new(
                FactOwnerV1::Profile,
                FactIdentitySourceV1::Application {
                    operation_id: ProvenanceId::new("dashboard.read-control.missing".to_owned())
                        .expect("dashboard fixture operation id"),
                },
            )
            .expect("dashboard fixture identity material"),
        )
        .expect("dashboard fixture fact id"),
    )
    .expect("dashboard fixture fact target");

    let (_, status_control) = interrupt_on_check(2);
    assert!(matches!(
        store
            .project_memory_status(FactOwnerV1::Profile, &status_control)
            .await,
        Err(FactStoreError::ReadCancelled)
    ));

    let (_, detail_control) = interrupt_on_check(2);
    assert!(matches!(
        store
            .dashboard_project_memory_fact_detail(
                ProjectMemoryDashboardFactDetailQueryV1::new(target.clone())
                    .expect("dashboard detail query"),
                &detail_control,
            )
            .await,
        Err(FactStoreError::ReadCancelled)
    ));

    let (_, feedback_control) = interrupt_on_check(2);
    assert!(matches!(
        store
            .project_memory_fact_feedback_history(
                ProjectMemoryFactFeedbackHistoryQueryV1::new(target, None, 8)
                    .expect("dashboard feedback query"),
                &feedback_control,
            )
            .await,
        Err(FactStoreError::ReadCancelled)
    ));

    let (_, oplog_control) = interrupt_on_check(2);
    assert!(matches!(
        store
            .dashboard_project_memory_oplog(
                ProjectMemoryDashboardOplogQueryV1::new(FactOwnerV1::Profile, 8)
                    .expect("dashboard oplog query"),
                &oplog_control,
            )
            .await,
        Err(FactStoreError::ReadCancelled)
    ));
}
