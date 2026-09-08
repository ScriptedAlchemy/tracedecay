use std::sync::Arc;

use serde_json::json;
use tempfile::TempDir;
use tracedecay_domain::{
    Confidence, FactCategoryV1, FactOwnerV1, PayloadAccessState, ProvenanceId,
};
use tracedecay_store::{
    FactReadControl, FactStoreError, ProjectMemoryFactAddCommandV1, ProjectMemoryFactAddMaterialV1,
    ProjectMemoryFactListQueryV1, ProjectMemoryFactProjectionV1, ProjectMemoryFactStore,
};

use crate::db::engine::params;
use crate::db::{Database, DatabaseAuthority, TestDatabaseRuntimeMode};
use crate::privacy::{MemoryFactSanitizationV1, sanitize_memory_fact_payload};
use crate::store::memory::{DatabaseFactStore, FactWriteControl};

async fn database() -> (TempDir, Database) {
    let directory = tempfile::tempdir().expect("payload-access fixture directory");
    let path = directory.path().join("payload-access-list.db");
    let authority = DatabaseAuthority::acquire_test(&path, "payload-access list authority")
        .expect("payload-access authority");
    let (database, _) =
        Database::publish_test_runtime(&path, &authority, TestDatabaseRuntimeMode::Initialize)
            .await
            .expect("payload-access database");
    (directory, database)
}

fn write_control() -> FactWriteControl {
    FactWriteControl::new(Arc::new(|| false), Arc::new(|| true))
}

fn read_control(interrupted: bool) -> FactReadControl {
    FactReadControl::new(Arc::new(move || interrupted))
}

fn command(index: usize) -> ProjectMemoryFactAddCommandV1 {
    let content = format!("canonical payload access fixture {index}");
    let material = json!({
        "content": content,
        "category": "project",
        "tags": ["payload-access"],
        "entities": ["TraceDecay"],
        "metadata": {"index": index},
    });
    let receipt = match sanitize_memory_fact_payload(material.clone())
        .expect("sanitize payload-access fixture")
    {
        MemoryFactSanitizationV1::Durable { payload, receipt } => {
            assert_eq!(payload, material);
            receipt
        }
        MemoryFactSanitizationV1::Quarantined => panic!("fixture must remain durable"),
    };
    ProjectMemoryFactAddMaterialV1::new(
        FactOwnerV1::Profile,
        content,
        FactCategoryV1::Project,
        None,
        vec!["payload-access".to_owned()],
        vec!["TraceDecay".to_owned()],
        json!({"index": index}),
        receipt,
        None,
        Confidence::new(0.5).expect("fixture trust"),
        None,
    )
    .and_then(|material| {
        material.into_command(
            ProvenanceId::new(format!("fixture.payload-access.{index}"))
                .expect("payload-access operation"),
        )
    })
    .expect("payload-access add command")
}

#[tokio::test]
async fn category_and_trust_filters_exclude_every_noneligible_payload_state() {
    let (_directory, database) = database().await;
    let store = DatabaseFactStore::new(&database);
    let control = write_control();
    let mut fact_ids = Vec::new();
    for index in 0..5 {
        fact_ids.push(
            store
                .add_project_memory_fact(command(index), &control)
                .await
                .expect("add payload-access fixture")
                .fact()
                .fact_id()
                .clone(),
        );
    }
    let hidden = [
        ("redacted", PayloadAccessState::Redacted),
        ("unavailable", PayloadAccessState::Unavailable),
        ("retention_expired", PayloadAccessState::RetentionExpired),
        ("ambiguous", PayloadAccessState::Ambiguous),
    ];
    let transaction = database
        .begin_memory_write_transaction("install hidden payload-access fixtures")
        .await
        .expect("begin hidden payload transaction");
    for ((state, _), fact_id) in hidden.iter().zip(fact_ids.iter().skip(1)) {
        assert_eq!(
            transaction
                .execute(
                    "UPDATE memory_v2_current_facts SET payload_access = ?1 WHERE fact_id = ?2",
                    params![*state, fact_id.as_str()],
                )
                .await
                .expect("set hidden payload state"),
            1
        );
    }
    transaction
        .commit()
        .await
        .expect("commit hidden payload states");

    let active = read_control(false);
    let unfiltered = store
        .list_project_memory_facts(
            ProjectMemoryFactListQueryV1::new(FactOwnerV1::Profile, None, None, None, 10)
                .expect("unfiltered list"),
            &active,
        )
        .await
        .expect("list typed unavailable projections");
    assert_eq!(unfiltered.facts().len(), 5);
    let unavailable_states = unfiltered
        .facts()
        .iter()
        .filter_map(|projection| match projection {
            ProjectMemoryFactProjectionV1::Available(_) => None,
            ProjectMemoryFactProjectionV1::Unavailable(fact) => Some(fact.payload_access()),
        })
        .collect::<Vec<_>>();
    assert_eq!(unavailable_states.len(), hidden.len());
    for (_, state) in hidden {
        assert!(unavailable_states.contains(&state));
    }

    for query in [
        ProjectMemoryFactListQueryV1::new(
            FactOwnerV1::Profile,
            Some(FactCategoryV1::Project),
            None,
            None,
            10,
        )
        .expect("category list"),
        ProjectMemoryFactListQueryV1::new(
            FactOwnerV1::Profile,
            None,
            Some(Confidence::new(0.4).expect("minimum trust")),
            None,
            10,
        )
        .expect("trust list"),
    ] {
        let page = store
            .list_project_memory_facts(query, &active)
            .await
            .expect("filtered eligible list");
        assert_eq!(page.facts().len(), 1);
        assert_eq!(page.facts()[0].fact_id(), &fact_ids[0]);
        assert!(matches!(
            &page.facts()[0],
            ProjectMemoryFactProjectionV1::Available(_)
        ));
    }

    assert!(matches!(
        store
            .list_project_memory_facts(
                ProjectMemoryFactListQueryV1::new(
                    FactOwnerV1::Profile,
                    Some(FactCategoryV1::Project),
                    None,
                    None,
                    10,
                )
                .expect("cancelled category list"),
                &read_control(true),
            )
            .await,
        Err(FactStoreError::ReadCancelled)
    ));
}
