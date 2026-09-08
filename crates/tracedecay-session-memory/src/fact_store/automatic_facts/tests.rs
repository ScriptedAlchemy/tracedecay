use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use serde_json::json;
use tempfile::{TempDir, tempdir};
use tracedecay_domain::{Confidence, FactCategoryV1, FactOwnerV1, ProvenanceId};
use tracedecay_store::{
    FactReadControl, FactStoreError, FactWriteControl, ProjectMemoryAutomaticFactEvidenceV1,
    ProjectMemoryFactAddCommandV1, ProjectMemoryFactAddMaterialV1, ProjectMemoryFactStore,
};

use crate::db::{Database, DatabaseAuthority, TestDatabaseRuntimeMode};
use crate::privacy::{MemoryFactSanitizationV1, sanitize_memory_fact_payload};
use crate::store::memory::DatabaseFactStore;

async fn database(label: &str) -> (TempDir, Database) {
    let directory = tempdir().expect("create automatic fact read-control fixture directory");
    let path = directory.path().join(format!("{label}.db"));
    let authority = DatabaseAuthority::acquire_test(&path, "automatic fact read-control authority")
        .expect("acquire automatic fact read-control authority");
    let (database, _) =
        Database::publish_test_runtime(&path, &authority, TestDatabaseRuntimeMode::Initialize)
            .await
            .expect("publish automatic fact read-control runtime");
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

fn automatic_fact_command(operation_id: &str) -> ProjectMemoryFactAddCommandV1 {
    let content = format!("Automatic fact receipt {operation_id} remains canonical.");
    let material = json!({
        "content": content,
        "category": "project",
        "tags": ["automatic-fact-read-control"],
        "entities": ["TraceDecay"],
        "metadata": {"fixture": "automatic-fact-read-control"},
    });
    let sanitization_receipt = match sanitize_memory_fact_payload(material.clone())
        .expect("sanitize automatic fact fixture")
    {
        MemoryFactSanitizationV1::Durable { payload, receipt } => {
            assert_eq!(payload, material);
            receipt
        }
        MemoryFactSanitizationV1::Quarantined => {
            panic!("automatic fact fixture must remain durable")
        }
    };
    ProjectMemoryFactAddMaterialV1::new(
        FactOwnerV1::Profile,
        material["content"]
            .as_str()
            .expect("automatic fact fixture content")
            .to_owned(),
        FactCategoryV1::Project,
        None,
        vec!["automatic-fact-read-control".to_owned()],
        vec!["TraceDecay".to_owned()],
        json!({"fixture": "automatic-fact-read-control"}),
        sanitization_receipt,
        None,
        Confidence::new(0.8).expect("automatic fact fixture confidence"),
        None,
    )
    .and_then(|material| {
        material.into_command(
            ProvenanceId::new(operation_id.to_owned()).expect("automatic fact operation identity"),
        )
    })
    .expect("automatic fact fixture command")
}

async fn seed_receipt(store: &DatabaseFactStore<'_>, apply_id: &ProvenanceId) {
    store
        .apply_project_memory_automatic_fact(
            apply_id.clone(),
            automatic_fact_command(&format!("{}.operation", apply_id.as_str())),
            ProjectMemoryAutomaticFactEvidenceV1::new(
                Some(format!("{}.evidence", apply_id.as_str())),
                Some(json!({"candidate": apply_id.as_str()})),
                Some(json!({"validated": true})),
            )
            .expect("automatic fact fixture evidence"),
            &write_control(),
        )
        .await
        .expect("seed automatic fact receipt");
}

#[tokio::test]
async fn get_observes_live_cancellation_after_receipt_hydration() {
    let (_directory, database) = database("get-cancellation").await;
    let store = DatabaseFactStore::new(&database);
    let apply_id = ProvenanceId::new("automatic.fact.get.cancellation".to_owned())
        .expect("automatic fact apply identity");
    seed_receipt(&store, &apply_id).await;
    let (checks, read_control) = interrupt_on_check(2);

    let result = store
        .get_project_memory_automatic_fact_receipt(FactOwnerV1::Profile, apply_id, &read_control)
        .await;

    assert!(matches!(result, Err(FactStoreError::ReadCancelled)));
    assert_eq!(checks.load(Ordering::Acquire), 2);
}

#[tokio::test]
async fn list_observes_live_cancellation_after_receipt_id_row_fetch() {
    let (_directory, database) = database("list-row-cancellation").await;
    let store = DatabaseFactStore::new(&database);
    let apply_id = ProvenanceId::new("automatic.fact.list.row.cancellation".to_owned())
        .expect("automatic fact apply identity");
    seed_receipt(&store, &apply_id).await;
    let (checks, read_control) = interrupt_on_check(5);

    let result = store
        .list_project_memory_automatic_fact_receipts(
            FactOwnerV1::Profile,
            None,
            None,
            8,
            &read_control,
        )
        .await;

    assert!(matches!(result, Err(FactStoreError::ReadCancelled)));
    assert_eq!(checks.load(Ordering::Acquire), 5);
}

#[tokio::test]
async fn list_observes_live_cancellation_after_receipt_material_hydration() {
    let (_directory, database) = database("list-hydration-cancellation").await;
    let store = DatabaseFactStore::new(&database);
    let apply_id = ProvenanceId::new("automatic.fact.list.hydration.cancellation".to_owned())
        .expect("automatic fact apply identity");
    seed_receipt(&store, &apply_id).await;
    let (checks, read_control) = interrupt_on_check(11);

    let result = store
        .list_project_memory_automatic_fact_receipts(
            FactOwnerV1::Profile,
            None,
            None,
            8,
            &read_control,
        )
        .await;

    assert!(matches!(result, Err(FactStoreError::ReadCancelled)));
    assert_eq!(checks.load(Ordering::Acquire), 11);
}
