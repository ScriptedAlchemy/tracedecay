use std::path::Path;
use std::sync::Arc;

use tracedecay_domain::{ActorId, BrainId, Confidence, FactCategoryV1, FactOwnerV1, UserProfileId};
use tracedecay_store::{
    FactReadControl, FactStoreError, FactWriteControl, ProjectMemoryFactProjectionV1,
    ProjectMemoryFactSearchGraphCoverageV1, ProjectMemoryFactSearchKindV1,
    ProjectMemoryFactSearchQuery, ProjectMemoryFactStore, ProjectMemoryGraphQueryV1,
    ProjectMemoryGraphStore, StoreShardIdV1,
};
use tracedecay_usecases::memory::{
    MemoryApplication, ProjectMemoryFactAddRequest, ProjectMemoryFactAddRequestOutcome,
};

use super::{ContractFixture, project_id};
use crate::daemon::profile_identity;
use crate::errors::TraceDecayError;
use crate::store::DatabaseFactStore;

async fn publish_foreign_memory_owner(path: &Path, label: &str) -> crate::db::DatabaseOwnerV1 {
    let authority = crate::db::DatabaseAuthority::acquire_test(path, label)
        .expect("foreign memory-owner fixture authority");
    let fixture = crate::db::Database::publish_registered_test_runtime_with_retirement_control(
        path,
        &authority,
        crate::db::TestDatabaseRuntimeMode::Initialize,
        crate::db::TestDatabaseRuntimeScope::Project {
            project_id: project_id(label),
        },
    )
    .await
    .expect("foreign memory-owner fixture publication");
    let (owner, runtime, _retirement) = fixture.into_parts();
    drop(runtime);
    owner
}

#[tokio::test]
async fn writable_project_and_profile_mounts_bind_exact_relational_authority() {
    let fixture = ContractFixture::new("exact-mount-identity").await;
    let project_id = project_id("exact-mount-identity");
    let (project_database, _) = fixture.mount_unbound(&project_id).await;
    let project_operation = project_database
        .issue_memory_graph_runtime_operation()
        .expect("project memory graph operation");
    assert_eq!(
        project_operation.runtime().relational_binding(),
        project_database.registered_binding()
    );
    assert_eq!(
        project_operation.runtime().relational_verified_locator(),
        project_database.registered_verified_locator()
    );

    let profile_database = fixture
        .registry
        .profile_memory()
        .await
        .expect("profile memory database");
    let profile_operation = profile_database
        .issue_memory_graph_runtime_operation()
        .expect("profile memory graph operation");
    assert_eq!(
        profile_operation.runtime().relational_binding(),
        profile_database.registered_binding()
    );
    assert_eq!(
        profile_operation.runtime().relational_verified_locator(),
        profile_database.registered_verified_locator()
    );

    drop((project_operation, profile_operation));
    let profile_operation = profile_database
        .issue_memory_graph_runtime_operation()
        .expect("profile graph operation remains available after the prior operation ends");
    assert_eq!(
        profile_operation.runtime().relational_binding(),
        profile_database.registered_binding()
    );
}

#[tokio::test]
async fn unbound_profile_memory_denies_graph_operations() {
    let fixture = ContractFixture::new("reverse-scope-binding").await;
    let project_id = project_id("reverse-scope-binding");
    let (_project_database, _) = fixture.mount_unbound(&project_id).await;
    let profile_shard = StoreShardIdV1::profile_memory(
        fixture.registry.identity.brain_id().clone(),
        fixture.registry.identity.profile_id().clone(),
    );
    let profile_runtime = super::super::open_runtime(
        &fixture.registry.registry,
        fixture.registry.resolver.as_ref(),
        profile_shard,
        fixture.registry.incarnation,
        Some(
            fixture
                .registry
                .profile_authority_pin("open unbound profile-memory binding fixture")
                .await
                .expect("profile authority pin"),
        ),
        None,
        true,
        "open unbound profile-memory binding fixture",
    )
    .await
    .expect("unbound profile-memory runtime");
    let profile_database = crate::db::Database::publish_runtime(
        profile_runtime,
        crate::db::DatabaseAccessMode::ReadWrite,
    )
    .await
    .expect("unbound profile-memory database")
    .issue_lease()
    .expect("unbound profile-memory database client");
    crate::db::migrations::ensure_schema_current(&profile_database)
        .await
        .expect("profile-memory schema");

    assert!(matches!(
        profile_database.issue_memory_graph_runtime_operation(),
        Err(crate::db::MemoryGraphRuntimeOperationErrorV1::Unbound)
    ));
}

#[tokio::test]
async fn retained_memory_runtime_rejects_foreign_brain_and_profile_scopes() {
    let fixture = ContractFixture::new("foreign-profile-scope").await;
    let project_id = project_id("foreign-profile-scope");
    let (database, _) = fixture.mount_unbound(&project_id).await;
    let shard = &database.registered_binding().shard_id;
    let foreign_brain = StoreShardIdV1::project(
        BrainId::new("brain.foreign-graph-scope").expect("foreign brain"),
        shard.profile_id.clone(),
        project_id.clone(),
    );
    let foreign_profile = StoreShardIdV1::project(
        shard.brain_id.clone(),
        UserProfileId::new("profile.foreign-graph-scope").expect("foreign profile"),
        project_id,
    );
    let foreign_brain_owner = publish_foreign_memory_owner(
        &fixture.root.join("foreign-brain-memory.db"),
        "foreign-brain-memory-owner",
    )
    .await;
    let foreign_profile_owner = publish_foreign_memory_owner(
        &fixture.root.join("foreign-profile-memory.db"),
        "foreign-profile-memory-owner",
    )
    .await;

    for result in [
        fixture
            .registry
            .retain_memory_graph_runtime(foreign_brain, foreign_brain_owner)
            .await,
        fixture
            .registry
            .retain_memory_graph_runtime(foreign_profile, foreign_profile_owner)
            .await,
    ] {
        match result {
            Err(TraceDecayError::Database { operation, message }) => {
                assert_eq!(operation, "retain verified memory graph authority");
                assert_eq!(
                    message,
                    "memory graph scope does not match the active profile authority"
                );
            }
            Err(other) => panic!("unexpected foreign-scope rejection: {other:?}"),
            Ok(_) => panic!("foreign active-profile identity was retained"),
        }
    }
}

#[tokio::test]
async fn cold_read_only_mount_denies_publication_and_degrades_graph_assist_truthfully() {
    let temporary = tempfile::tempdir().expect("cold read-only fixture root");
    let profile_root = temporary.path().join("profile");
    let project_id = project_id("cold-read-only");
    let project_root = temporary.path().join("project");
    std::fs::create_dir_all(&project_root).expect("project root");
    crate::storage::pin_fixture_repository_identity(&project_root, project_id.as_str())
        .expect("project enrollment");
    let identity = profile_identity::load_or_create(&profile_root).expect("profile identity");
    let scope = crate::db::enter_daemon_database_scope(&profile_root, 37, "seed cold read-only")
        .expect("seed database scope");
    let registry = super::super::DaemonSessionRuntimeRegistryV1::open(identity.clone())
        .await
        .expect("seed registry");
    let database = registry
        .project_memory(project_id.clone(), [project_root.clone()])
        .await
        .expect("seed writable project memory");
    let owner = FactOwnerV1::Project {
        project_id: project_id.clone(),
    };
    let memory = MemoryApplication::new(owner.clone(), DatabaseFactStore::new(&database))
        .expect("owner-bound cold read-only fixture");
    let preflight = memory
        .preflight_project_memory_fact_add(
            ProjectMemoryFactAddRequest {
                content: "cold-read-only durable recall token".to_owned(),
                category: FactCategoryV1::Project,
                source_label: Some("cold-read-only-fixture".to_owned()),
                tags: vec!["cold-read-only".to_owned()],
                entities: Vec::new(),
                trust: Some(Confidence::new(0.9).expect("fixture trust")),
                metadata: serde_json::json!({"fixture": "cold-read-only"}),
            },
            Some(ActorId::new("actor.cold-read-only-fixture").expect("fixture actor")),
        )
        .expect("preflight cold read-only fact");
    let write_control = FactWriteControl::new(Arc::new(|| false), Arc::new(|| true));
    let added = memory
        .add_preflighted_project_memory_fact(preflight, &write_control)
        .await
        .expect("commit cold read-only fact");
    let ProjectMemoryFactAddRequestOutcome::Applied(added) = added else {
        panic!("cold read-only fixture fact was rejected")
    };
    let ProjectMemoryFactProjectionV1::Available(added) = added.fact() else {
        panic!("cold read-only fixture fact is unavailable")
    };
    let added_fact_id = added.fact_id().clone();
    registry
        .shutdown_memory_graph_reconciliation_tasks()
        .await
        .expect("join seed graph reconciliation");
    drop((database, registry, scope));

    let _restarted_scope =
        crate::db::enter_daemon_database_scope(&profile_root, 38, "cold read-only reopen")
            .expect("cold read-only database scope");
    let restarted = super::super::DaemonSessionRuntimeRegistryV1::open(identity)
        .await
        .expect("cold read-only registry");
    let read_only = restarted
        .project_memory_read_only(project_id.clone(), [project_root])
        .await
        .expect("cold read-only project memory");
    assert!(!read_only.is_writable());
    assert!(matches!(
        read_only.issue_memory_graph_runtime_operation(),
        Err(crate::db::MemoryGraphRuntimeOperationErrorV1::Unbound)
    ));
    assert!(read_only.graph_publication_storage().is_err());

    let owner = FactOwnerV1::Project { project_id };
    let read_control = FactReadControl::new(Arc::new(|| false));
    let store = DatabaseFactStore::new(&read_only);
    let graph = store
        .project_memory_graph(
            ProjectMemoryGraphQueryV1::new(owner.clone(), Vec::new(), 1)
                .expect("read-only graph query"),
            &read_control,
        )
        .await;
    assert!(matches!(graph, Err(FactStoreError::GraphUnavailable)));

    let search = store
        .search_project_memory_facts(
            ProjectMemoryFactSearchQuery::new(
                owner,
                ProjectMemoryFactSearchKindV1::Search,
                Some("durable recall token".to_owned()),
                None,
                8,
            )
            .expect("read-only ordinary search"),
            &read_control,
        )
        .await
        .expect("ordinary search remains available without Grafeo");
    assert_eq!(
        search.graph_coverage(),
        ProjectMemoryFactSearchGraphCoverageV1::NotMounted
    );
    assert!(
        search
            .hits()
            .iter()
            .any(|hit| hit.fact().fact_id() == &added_fact_id),
        "ordinary SQLite recall must remain available without Grafeo"
    );
}
