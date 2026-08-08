use std::path::{Path, PathBuf};

use tempfile::TempDir;
use tracedecay_domain::{FactOwnerV1, ProjectId};
use tracedecay_store::{
    ProjectMemoryFactProjectionV1, ProjectMemoryGraphQueryV1, ProjectMemoryGraphRelationKindV1,
    ProjectMemoryGraphTargetV1,
};
use tracedecay_usecases::memory::{MemoryApplication, MemoryOperationContext};

use super::DaemonSessionRuntimeRegistryV1;
use crate::daemon::profile_identity;
use crate::memory::types::{
    AddFactRequest, FactRelationKind, MemoryCategory, MemoryGroomingOperation,
};
use crate::store::DatabaseFactStore;

fn add_request(content: &str, category: MemoryCategory) -> AddFactRequest {
    AddFactRequest {
        content: content.to_owned(),
        category,
        tags: Vec::new(),
        entities: Vec::new(),
        trust: Some(0.9),
        source: Some("memory-graph-contract".to_owned()),
        metadata: serde_json::json!({}),
    }
}

fn enrolled_root(base: &Path, project_id: &ProjectId) -> PathBuf {
    let root = base.join(project_id.as_str());
    std::fs::create_dir_all(&root).expect("project root");
    crate::storage::write_enrollment_marker(
        &root,
        &crate::storage::EnrollmentMarker {
            project_id: project_id.as_str().to_owned(),
            storage_mode: crate::storage::StorageMode::ProfileSharded,
        },
    )
    .expect("project enrollment");
    root
}

async fn seed_relation(
    database: &crate::db::Database,
    owner: FactOwnerV1,
    category: MemoryCategory,
    label: &str,
) -> tracedecay_domain::FactId {
    let memory = MemoryApplication::new(owner.clone(), DatabaseFactStore::new(database))
        .expect("owner-bound memory application");
    let source = memory
        .add_fact_v1(
            add_request(
                &format!("{label} stores the canonical relation source"),
                category,
            ),
            MemoryOperationContext::generated(&owner, "seed graph source", None)
                .expect("source operation"),
        )
        .await
        .expect("source fact")
        .fact
        .expect("stored source fact");
    let target = memory
        .add_fact_v1(
            add_request(
                &format!("{label} stores the canonical relation target"),
                category,
            ),
            MemoryOperationContext::generated(&owner, "seed graph target", None)
                .expect("target operation"),
        )
        .await
        .expect("target fact")
        .fact
        .expect("stored target fact");
    memory
        .dashboard_apply_grooming_v1(
            vec![MemoryGroomingOperation::LinkFacts {
                source_fact_id: source.fact_id,
                target_fact_id: target.fact_id,
                relation: FactRelationKind::Supports,
                evidence_fact_ids: vec![source.fact_id],
                confidence: 0.9,
                source: "memory-graph-contract".to_owned(),
                metadata: serde_json::json!({"reason": "restart evidence"}),
            }],
            0.5,
            MemoryOperationContext::generated(&owner, "seed graph relation", None)
                .expect("relation operation"),
        )
        .await
        .expect("canonical relation write");
    memory
        .dashboard_overview_v1(16, 16)
        .await
        .expect("owner-bound overview")
        .facts
        .into_iter()
        .find_map(|summary| {
            (summary.fact.mapping()?.legacy_fact_id() == Some(source.fact_id))
                .then(|| summary.fact.fact_id().clone())
        })
        .expect("canonical source fact id")
}

#[tokio::test]
async fn registered_memory_relation_graph_survives_restart_and_isolates_projects() {
    let temp = TempDir::new().expect("contract fixture root");
    let profile_root = temp.path().join("profile");
    let first_id = ProjectId::new("project.memory-graph.first").expect("first project id");
    let second_id = ProjectId::new("project.memory-graph.second").expect("second project id");
    let first_root = enrolled_root(temp.path(), &first_id);
    let second_root = enrolled_root(temp.path(), &second_id);
    let _database_scope = crate::db::enter_daemon_database_scope(
        &profile_root,
        41,
        "project memory relation graph contract",
    )
    .expect("daemon database scope");

    let identity = profile_identity::load_or_create(&profile_root).expect("profile identity");
    let registry = DaemonSessionRuntimeRegistryV1::open(identity.clone())
        .await
        .expect("first daemon registry");
    let first_database = registry
        .project_memory(first_id.clone(), [first_root.clone()])
        .await
        .expect("first project memory authority");
    let source_id = seed_relation(
        &first_database,
        FactOwnerV1::Project {
            project_id: first_id.clone(),
        },
        MemoryCategory::Project,
        "project alpha",
    )
    .await;
    let profile_database = registry
        .profile_memory()
        .await
        .expect("profile memory authority");
    let profile_source_id = seed_relation(
        &profile_database,
        FactOwnerV1::Profile,
        MemoryCategory::UserPref,
        "profile memory",
    )
    .await;
    registry
        .project_memory(second_id.clone(), [second_root.clone()])
        .await
        .expect("second project memory authority");
    drop(first_database);
    drop(profile_database);
    drop(registry);

    let restarted = DaemonSessionRuntimeRegistryV1::open(identity)
        .await
        .expect("restarted daemon registry");
    let first_database = restarted
        .project_memory(first_id.clone(), [first_root])
        .await
        .expect("reopened first project memory");
    let first_owner = FactOwnerV1::Project {
        project_id: first_id.clone(),
    };
    let first =
        MemoryApplication::new(first_owner.clone(), DatabaseFactStore::new(&first_database))
            .expect("first memory application")
            .project_memory_graph(
                ProjectMemoryGraphQueryV1::new(first_owner, vec![source_id], 32)
                    .expect("first graph query"),
            )
            .await
            .expect("first project graph after restart");

    assert!(
        first
            .relations()
            .iter()
            .any(|relation| relation.kind() == ProjectMemoryGraphRelationKindV1::Supports)
    );
    assert!(first.facts().iter().all(|fact| matches!(
        fact,
        ProjectMemoryFactProjectionV1::Available(available)
            if available.payload().is_some()
    )));
    assert!(first.relations().iter().any(|relation| matches!(
        (relation.source(), relation.target(), relation.kind()),
        (
            ProjectMemoryGraphTargetV1::Fact(_),
            ProjectMemoryGraphTargetV1::Fact(_),
            ProjectMemoryGraphRelationKindV1::Supports
        )
    )));

    let profile_database = restarted
        .profile_memory()
        .await
        .expect("reopened profile memory");
    let profile = MemoryApplication::new(
        FactOwnerV1::Profile,
        DatabaseFactStore::new(&profile_database),
    )
    .expect("profile memory application")
    .project_memory_graph(
        ProjectMemoryGraphQueryV1::new(FactOwnerV1::Profile, vec![profile_source_id], 32)
            .expect("profile graph query"),
    )
    .await
    .expect("profile graph after restart");
    assert!(
        profile
            .relations()
            .iter()
            .any(|relation| relation.kind() == ProjectMemoryGraphRelationKindV1::Supports)
    );
    assert_eq!(profile.owner(), &FactOwnerV1::Profile);

    let second_database = restarted
        .project_memory(second_id.clone(), [second_root])
        .await
        .expect("reopened second project memory");
    let second_owner = FactOwnerV1::Project {
        project_id: second_id,
    };
    let second = MemoryApplication::new(
        second_owner.clone(),
        DatabaseFactStore::new(&second_database),
    )
    .expect("second memory application")
    .project_memory_graph(
        ProjectMemoryGraphQueryV1::new(second_owner, Vec::new(), 32).expect("second graph query"),
    )
    .await
    .expect("isolated second project graph");
    assert!(second.relations().is_empty());
    assert!(second.facts().is_empty());
}
