use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use tempfile::TempDir;
use tracedecay_domain::{
    ActorId, Confidence, FactCategoryV1, FactId, FactOwnerV1, FactRelationKindV1, ProjectId,
    ProjectMemoryGraphRelationKindV1,
};
use tracedecay_store::{
    FactReadControl, FactStoreError, FactWriteControl, ProjectMemoryFactProjectionV1,
    ProjectMemoryGraphPageV1, ProjectMemoryGraphQueryV1, ProjectMemoryGraphTargetV1,
};
use tracedecay_usecases::memory::{
    MemoryApplication, MemoryApplicationError, MemoryOperationContext,
    ProjectMemoryCurationOperation, ProjectMemoryFactAddRequest,
    ProjectMemoryFactAddRequestOutcome,
};

use super::DaemonSessionRuntimeRegistryV1;
use crate::daemon::profile_identity;
use crate::store::DatabaseFactStore;

const CORE_RELATIONS_BEFORE_CHORD: usize = 9;
const CORE_RELATIONS_AFTER_CHORD: usize = 10;
const LONG_PATH_EDGES: usize = 40;
const LONG_PATH_RELATIONS: usize = LONG_PATH_EDGES * 2 + 1;

#[derive(Clone)]
struct TestFactLifecycle {
    interrupted: Arc<AtomicBool>,
}

impl TestFactLifecycle {
    fn new() -> Self {
        Self {
            interrupted: Arc::new(AtomicBool::new(false)),
        }
    }

    fn read_control(&self) -> FactReadControl {
        let interrupted = Arc::clone(&self.interrupted);
        FactReadControl::new(Arc::new(move || interrupted.load(Ordering::Acquire)))
    }

    fn write_control(&self) -> FactWriteControl {
        let interrupted_for_read = Arc::clone(&self.interrupted);
        let interrupted_for_commit = Arc::clone(&self.interrupted);
        let commit_granted = Arc::new(AtomicBool::new(false));
        FactWriteControl::new(
            Arc::new(move || interrupted_for_read.load(Ordering::Acquire)),
            Arc::new(move || {
                !interrupted_for_commit.load(Ordering::Acquire)
                    && commit_granted
                        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                        .is_ok()
            }),
        )
    }

    fn cancel(&self) {
        self.interrupted.store(true, Ordering::Release);
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

async fn add_fact(
    database: &crate::db::Database,
    lifecycle: &TestFactLifecycle,
    owner: &FactOwnerV1,
    category: FactCategoryV1,
    label: &str,
) -> FactId {
    let memory = MemoryApplication::new(owner.clone(), DatabaseFactStore::new(database))
        .expect("owner-bound memory application");
    let actor =
        ActorId::new("actor.memory-graph-contract").expect("memory graph contract actor identity");
    let preflight = memory
        .preflight_project_memory_fact_add(
            ProjectMemoryFactAddRequest {
                content: format!(
                    "{label}: canonical graph contract payload with distinct identity material"
                ),
                category,
                source_label: Some("memory-graph-contract".to_owned()),
                tags: vec![label.to_owned()],
                entities: Vec::new(),
                trust: Some(Confidence::new(0.9).expect("fact trust")),
                metadata: serde_json::json!({"fixture": label}),
            },
            Some(actor),
        )
        .expect("preflight graph contract fact");
    let outcome = memory
        .add_preflighted_project_memory_fact(preflight, &lifecycle.write_control())
        .await
        .expect("commit graph contract fact");
    let ProjectMemoryFactAddRequestOutcome::Applied(outcome) = outcome else {
        panic!("graph contract fact was rejected by the privacy boundary: {label}");
    };
    let ProjectMemoryFactProjectionV1::Available(fact) = outcome.fact() else {
        panic!("graph contract fact payload is unavailable: {label}");
    };
    fact.fact_id().clone()
}

async fn add_facts(
    database: &crate::db::Database,
    lifecycle: &TestFactLifecycle,
    owner: &FactOwnerV1,
    category: FactCategoryV1,
    labels: impl IntoIterator<Item = String>,
) -> Vec<FactId> {
    let mut facts = Vec::new();
    for label in labels {
        facts.push(add_fact(database, lifecycle, owner, category, &label).await);
    }
    facts
}

async fn link_facts(
    database: &crate::db::Database,
    lifecycle: &TestFactLifecycle,
    owner: &FactOwnerV1,
    operation: &str,
    relations: Vec<(FactId, FactId, FactRelationKindV1)>,
) {
    let memory = MemoryApplication::new(owner.clone(), DatabaseFactStore::new(database))
        .expect("owner-bound memory application");
    let operations = relations
        .into_iter()
        .enumerate()
        .map(|(index, (source_fact_id, target_fact_id, relation))| {
            ProjectMemoryCurationOperation::LinkFacts {
                evidence_fact_ids: vec![source_fact_id.clone()],
                source_fact_id,
                target_fact_id,
                relation,
                confidence: Confidence::new(0.9).expect("relation confidence"),
                source_label: "memory-graph-contract".to_owned(),
                metadata: serde_json::json!({
                    "fixture": operation,
                    "relation_index": index,
                }),
            }
        })
        .collect();
    memory
        .apply_project_memory_curation(
            operations,
            Confidence::new(0.5).expect("curation threshold"),
            MemoryOperationContext::generated(owner, operation, None)
                .expect("graph curation operation"),
            None,
            &lifecycle.write_control(),
        )
        .await
        .expect("commit canonical graph relations");
}

async fn wait_for_reconciliation(database: &crate::db::Database) {
    let owner = database
        .memory_graph_reconciliation_task_owner()
        .expect("mounted graph reconciliation owner");
    for _ in 0..4_096 {
        if !owner.pending() && !owner.running() {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("mounted graph reconciliation did not settle");
}

async fn graph(
    database: &crate::db::Database,
    lifecycle: &TestFactLifecycle,
    owner: FactOwnerV1,
    roots: Vec<FactId>,
    max_relations: usize,
) -> Result<ProjectMemoryGraphPageV1, MemoryApplicationError> {
    let memory = MemoryApplication::new(owner.clone(), DatabaseFactStore::new(database))
        .expect("owner-bound memory application");
    memory
        .project_memory_graph(
            ProjectMemoryGraphQueryV1::new(owner, roots, max_relations)
                .expect("canonical graph query"),
            &lifecycle.read_control(),
        )
        .await
}

fn fact_target(target: &ProjectMemoryGraphTargetV1) -> Option<&FactId> {
    match target {
        ProjectMemoryGraphTargetV1::Fact(fact) => Some(fact.fact_id()),
        _ => None,
    }
}

fn has_fact_relation(
    page: &ProjectMemoryGraphPageV1,
    source: &FactId,
    target: &FactId,
    kind: ProjectMemoryGraphRelationKindV1,
) -> bool {
    page.relations().iter().any(|relation| {
        relation.kind() == kind
            && fact_target(relation.source()) == Some(source)
            && fact_target(relation.target()) == Some(target)
    })
}

fn relation_kinds(page: &ProjectMemoryGraphPageV1) -> BTreeSet<ProjectMemoryGraphRelationKindV1> {
    page.relations()
        .iter()
        .filter_map(|relation| match relation.kind() {
            ProjectMemoryGraphRelationKindV1::Supports
            | ProjectMemoryGraphRelationKindV1::Contradicts
            | ProjectMemoryGraphRelationKindV1::Supersedes
            | ProjectMemoryGraphRelationKindV1::DerivedFrom => Some(relation.kind()),
            ProjectMemoryGraphRelationKindV1::Mentions
            | ProjectMemoryGraphRelationKindV1::ActiveAssertion
            | ProjectMemoryGraphRelationKindV1::EvidenceAnchor => None,
        })
        .collect()
}

#[tokio::test]
async fn registered_memory_relation_graph_survives_restart_and_isolates_topologies() {
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

    let lifecycle = TestFactLifecycle::new();
    let identity = profile_identity::load_or_create(&profile_root).expect("profile identity");
    let registry = DaemonSessionRuntimeRegistryV1::open(identity.clone())
        .await
        .expect("first daemon registry");
    let first_database = registry
        .project_memory(first_id.clone(), [first_root.clone()])
        .await
        .expect("first project memory authority");
    let first_owner = FactOwnerV1::Project {
        project_id: first_id.clone(),
    };
    let core = add_facts(
        &first_database,
        &lifecycle,
        &first_owner,
        FactCategoryV1::Project,
        ["core-alpha", "core-beta", "core-gamma", "core-delta"]
            .into_iter()
            .map(str::to_owned),
    )
    .await;
    link_facts(
        &first_database,
        &lifecycle,
        &first_owner,
        "seed core graph without chord",
        vec![
            (
                core[0].clone(),
                core[1].clone(),
                FactRelationKindV1::Supports,
            ),
            (
                core[0].clone(),
                core[1].clone(),
                FactRelationKindV1::DerivedFrom,
            ),
            (
                core[1].clone(),
                core[2].clone(),
                FactRelationKindV1::Contradicts,
            ),
            (
                core[2].clone(),
                core[3].clone(),
                FactRelationKindV1::Supersedes,
            ),
            (
                core[3].clone(),
                core[0].clone(),
                FactRelationKindV1::Supports,
            ),
        ],
    )
    .await;
    wait_for_reconciliation(&first_database).await;

    let exact = graph(
        &first_database,
        &lifecycle,
        first_owner.clone(),
        vec![core[0].clone()],
        CORE_RELATIONS_BEFORE_CHORD,
    )
    .await
    .expect("exact relation budget succeeds");
    assert_eq!(exact.relations().len(), CORE_RELATIONS_BEFORE_CHORD);

    link_facts(
        &first_database,
        &lifecycle,
        &first_owner,
        "add core graph chord",
        vec![(
            core[0].clone(),
            core[2].clone(),
            FactRelationKindV1::DerivedFrom,
        )],
    )
    .await;
    wait_for_reconciliation(&first_database).await;
    assert!(matches!(
        graph(
            &first_database,
            &lifecycle,
            first_owner.clone(),
            vec![core[0].clone()],
            CORE_RELATIONS_BEFORE_CHORD,
        )
        .await,
        Err(MemoryApplicationError::Store(
            FactStoreError::GraphBudgetExhausted
        ))
    ));

    let chain = add_facts(
        &first_database,
        &lifecycle,
        &first_owner,
        FactCategoryV1::CodeArea,
        (0..=LONG_PATH_EDGES).map(|index| format!("long-path-node-{index:02}")),
    )
    .await;
    let chain_relations = chain
        .windows(2)
        .map(|pair| {
            (
                pair[0].clone(),
                pair[1].clone(),
                FactRelationKindV1::Supports,
            )
        })
        .collect();
    link_facts(
        &first_database,
        &lifecycle,
        &first_owner,
        "seed disconnected long path",
        chain_relations,
    )
    .await;

    let profile_database = registry
        .profile_memory()
        .await
        .expect("profile memory authority");
    let profile_owner = FactOwnerV1::Profile;
    let profile_facts = add_facts(
        &profile_database,
        &lifecycle,
        &profile_owner,
        FactCategoryV1::UserPref,
        ["profile-source", "profile-target"]
            .into_iter()
            .map(str::to_owned),
    )
    .await;
    link_facts(
        &profile_database,
        &lifecycle,
        &profile_owner,
        "seed profile relation",
        vec![(
            profile_facts[0].clone(),
            profile_facts[1].clone(),
            FactRelationKindV1::Supports,
        )],
    )
    .await;
    let second_database = registry
        .project_memory(second_id.clone(), [second_root.clone()])
        .await
        .expect("second project memory authority");

    wait_for_reconciliation(&first_database).await;
    wait_for_reconciliation(&profile_database).await;
    wait_for_reconciliation(&second_database).await;
    registry
        .shutdown_memory_graph_reconciliation_tasks()
        .await
        .expect("join first daemon graph lifecycles");
    drop((first_database, profile_database, second_database, registry));

    let restarted = DaemonSessionRuntimeRegistryV1::open(identity)
        .await
        .expect("restarted daemon registry");
    let first_database = restarted
        .project_memory(first_id.clone(), [first_root])
        .await
        .expect("reopened first project memory");
    let profile_database = restarted
        .profile_memory()
        .await
        .expect("reopened profile memory");
    let second_database = restarted
        .project_memory(second_id.clone(), [second_root])
        .await
        .expect("reopened second project memory");
    wait_for_reconciliation(&first_database).await;
    wait_for_reconciliation(&profile_database).await;
    wait_for_reconciliation(&second_database).await;

    assert!(matches!(
        graph(
            &first_database,
            &lifecycle,
            first_owner.clone(),
            Vec::new(),
            CORE_RELATIONS_AFTER_CHORD,
        )
        .await,
        Err(MemoryApplicationError::Store(
            FactStoreError::GraphBudgetExhausted
        ))
    ));
    let core_page = graph(
        &first_database,
        &lifecycle,
        first_owner.clone(),
        vec![core[0].clone()],
        CORE_RELATIONS_AFTER_CHORD,
    )
    .await
    .expect("rooted core excludes the oversized disconnected component");
    assert_eq!(core_page.owner(), &first_owner);
    assert_eq!(core_page.relations().len(), CORE_RELATIONS_AFTER_CHORD);
    assert_eq!(core_page.facts().len(), core.len());
    assert!(
        core_page
            .facts()
            .iter()
            .all(|fact| matches!(fact, ProjectMemoryFactProjectionV1::Available(_)))
    );
    assert_eq!(
        relation_kinds(&core_page),
        BTreeSet::from([
            ProjectMemoryGraphRelationKindV1::Supports,
            ProjectMemoryGraphRelationKindV1::Contradicts,
            ProjectMemoryGraphRelationKindV1::Supersedes,
            ProjectMemoryGraphRelationKindV1::DerivedFrom,
        ])
    );
    let parallel = core_page
        .relations()
        .iter()
        .filter(|relation| {
            fact_target(relation.source()) == Some(&core[0])
                && fact_target(relation.target()) == Some(&core[1])
        })
        .map(|relation| relation.kind())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        parallel,
        BTreeSet::from([
            ProjectMemoryGraphRelationKindV1::Supports,
            ProjectMemoryGraphRelationKindV1::DerivedFrom,
        ])
    );
    assert!(has_fact_relation(
        &core_page,
        &core[1],
        &core[2],
        ProjectMemoryGraphRelationKindV1::Contradicts,
    ));
    assert!(has_fact_relation(
        &core_page,
        &core[2],
        &core[3],
        ProjectMemoryGraphRelationKindV1::Supersedes,
    ));
    assert!(has_fact_relation(
        &core_page,
        &core[3],
        &core[0],
        ProjectMemoryGraphRelationKindV1::Supports,
    ));
    assert!(has_fact_relation(
        &core_page,
        &core[0],
        &core[2],
        ProjectMemoryGraphRelationKindV1::DerivedFrom,
    ));

    let long_path = graph(
        &first_database,
        &lifecycle,
        first_owner.clone(),
        vec![chain[0].clone()],
        LONG_PATH_RELATIONS,
    )
    .await
    .expect("long path is not truncated by a hidden traversal depth");
    assert_eq!(long_path.relations().len(), LONG_PATH_RELATIONS);
    assert_eq!(long_path.facts().len(), chain.len());
    assert!(
        long_path
            .facts()
            .iter()
            .any(|fact| fact.fact_id() == chain.last().expect("long path tail"))
    );

    let profile = graph(
        &profile_database,
        &lifecycle,
        FactOwnerV1::Profile,
        vec![profile_facts[0].clone()],
        3,
    )
    .await
    .expect("profile relation graph after restart");
    assert_eq!(profile.owner(), &FactOwnerV1::Profile);
    assert_eq!(profile.facts().len(), 2);
    assert_eq!(profile.relations().len(), 3);
    assert!(profile.relations().iter().any(|relation| {
        relation.kind() == ProjectMemoryGraphRelationKindV1::Supports
            && fact_target(relation.source()) == Some(&profile_facts[0])
            && fact_target(relation.target()) == Some(&profile_facts[1])
    }));

    let cancelled = TestFactLifecycle::new();
    cancelled.cancel();
    assert!(matches!(
        graph(
            &first_database,
            &cancelled,
            first_owner.clone(),
            vec![core[0].clone()],
            CORE_RELATIONS_AFTER_CHORD,
        )
        .await,
        Err(MemoryApplicationError::Store(
            FactStoreError::GraphCancelled
        ))
    ));
    assert!(matches!(
        graph(
            &profile_database,
            &cancelled,
            FactOwnerV1::Profile,
            vec![profile_facts[0].clone()],
            3,
        )
        .await,
        Err(MemoryApplicationError::Store(
            FactStoreError::GraphCancelled
        ))
    ));

    let second_owner = FactOwnerV1::Project {
        project_id: second_id,
    };
    let second = graph(
        &second_database,
        &lifecycle,
        second_owner.clone(),
        Vec::new(),
        1,
    )
    .await
    .expect("isolated second project graph");
    assert_eq!(second.owner(), &second_owner);
    assert!(second.relations().is_empty());
    assert!(second.facts().is_empty());
    restarted
        .shutdown_memory_graph_reconciliation_tasks()
        .await
        .expect("join restarted daemon graph lifecycles");
}
