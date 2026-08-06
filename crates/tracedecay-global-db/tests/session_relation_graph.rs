use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tempfile::TempDir;
use tracedecay_domain::{
    CopyProofV1, MessageOccurrenceIdV1, ProjectId, RetrievalAnchorId, SessionId,
    TemporalValidityV1, ThreadId, UserProfileId, UtcMicros,
};
use tracedecay_global_db::session_temporal::relations::{
    LogicalCopyRelation, SessionRelationError, SessionRelationGraphStore,
    SessionRelationProjection, SessionRelationScope, SummaryRelationNode, SummarySourceRef,
    SummarySourceVisitKind, ThreadHierarchyRelation, WorkflowAgentMembership,
};

use sha2::{Digest, Sha256};
use tracedecay_graph_db::{
    GraphCancellation, GraphDb, GraphDbOwner, GraphDbRegistration, GraphDbRegistry,
    GraphDbRegistryConfig, NeverCancelled,
};
use tracedecay_store::{
    BrainId, RetainedGraphStoreLeaseV1, StoreAuthorityEpochV1, StoreIncarnationV1,
    StoreRuntimeBindingV1, StoreShardIdV1, VerifiedStoreLocatorV1, canonical_store_locator_digest,
};

#[derive(Debug)]
struct TestCancellation(AtomicBool);

impl GraphCancellation for TestCancellation {
    fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

fn id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    T::Error: std::fmt::Debug,
{
    T::try_from(value.to_owned()).expect("valid identity")
}

fn occurrence_id(seed: &str) -> MessageOccurrenceIdV1 {
    let digest = Sha256::digest(seed.as_bytes());
    let mut value = String::with_capacity(71);
    value.push_str("sha256:");
    for byte in digest {
        value.push_str(&format!("{byte:02x}"));
    }
    MessageOccurrenceIdV1::new(value).expect("derived occurrence identity")
}

fn memory_relation_store() -> SessionRelationGraphStore {
    let owner = GraphDbOwner::memory(Arc::new(NeverCancelled)).expect("memory relation graph");
    SessionRelationGraphStore::new(owner.handle())
}

#[derive(Debug)]
struct TestGraphLease {
    binding: StoreRuntimeBindingV1,
    verified_locator: VerifiedStoreLocatorV1,
    canonical_path: PathBuf,
}

impl RetainedGraphStoreLeaseV1 for TestGraphLease {
    fn binding(&self) -> &StoreRuntimeBindingV1 {
        &self.binding
    }

    fn verified_locator(&self) -> &VerifiedStoreLocatorV1 {
        &self.verified_locator
    }

    fn canonical_path(&self) -> &Path {
        &self.canonical_path
    }
}

fn persistent_registration(canonical_path: PathBuf) -> GraphDbRegistration {
    let binding = StoreRuntimeBindingV1::new(
        StoreShardIdV1::profile_sessions(
            id::<BrainId>("brain.session-relation-test"),
            id::<UserProfileId>("profile.restart"),
        ),
        StoreIncarnationV1::new(1).expect("incarnation"),
        StoreAuthorityEpochV1::new(1).expect("authority epoch"),
    );
    let verified_locator = VerifiedStoreLocatorV1::new(
        binding.shard_id.clone(),
        binding.incarnation,
        canonical_store_locator_digest(&canonical_path).expect("graph locator digest"),
    );
    GraphDbRegistration {
        authority_lease: Arc::new(TestGraphLease {
            binding,
            verified_locator,
            canonical_path,
        }),
        cancellation: Arc::new(TestCancellation(AtomicBool::new(false))),
        lifecycle_cancellation: Arc::new(TestCancellation(AtomicBool::new(false))),
        deadline: Instant::now() + Duration::from_secs(30),
    }
}

fn projection(generation: u64) -> SessionRelationProjection {
    SessionRelationProjection {
        scope: SessionRelationScope::project_sessions(id::<ProjectId>("project.session-relations")),
        session_id: id::<SessionId>("session.relations"),
        generation,
        summaries: vec![
            SummaryRelationNode {
                summary_id: "summary.root".to_owned(),
                sources: vec![
                    SummarySourceRef::Summary {
                        summary_id: "summary.child".to_owned(),
                    },
                    SummarySourceRef::Anchor {
                        anchor_id: id::<RetrievalAnchorId>("anchor.root"),
                    },
                ],
                predecessor_summary_id: None,
            },
            SummaryRelationNode {
                summary_id: "summary.child".to_owned(),
                sources: vec![SummarySourceRef::Anchor {
                    anchor_id: id::<RetrievalAnchorId>("anchor.child"),
                }],
                predecessor_summary_id: None,
            },
        ],
        logical_copies: Vec::new(),
        thread_hierarchy: Vec::new(),
        agent_hierarchy: Vec::new(),
        parent_session_id: None,
        workflow_agents: Vec::new(),
    }
}

#[test]
fn summary_source_walk_is_generation_scoped_ordered_and_bounded() {
    let store = memory_relation_store();
    let first = projection(1);
    let second = projection(2);
    store.replace(&first).expect("first publication");
    store.replace(&second).expect("second publication");

    let visits = store
        .summary_sources(
            &first.scope,
            &first.session_id,
            1,
            "summary.root",
            3,
            Arc::new(TestCancellation(AtomicBool::new(false))),
        )
        .expect("bounded traversal");
    assert_eq!(visits.len(), 3);
    assert!(matches!(
        visits[0].source,
        SummarySourceVisitKind::Summary { ref summary_id } if summary_id == "summary.child"
    ));
    assert_eq!(visits[0].ordinal, 0);
    assert_eq!(visits[0].depth, 1);
    assert!(matches!(
        visits[2].source,
        SummarySourceVisitKind::Anchor { ref anchor_id }
            if anchor_id.as_str() == "anchor.child"
    ));
    assert_eq!(visits[2].depth, 2);

    assert_eq!(
        store.summary_sources(
            &first.scope,
            &first.session_id,
            1,
            "summary.root",
            2,
            Arc::new(TestCancellation(AtomicBool::new(false))),
        ),
        Err(SessionRelationError::BudgetExhausted)
    );
}

#[test]
fn summary_source_walk_observes_cancellation_and_rejects_mutation() {
    let store = memory_relation_store();
    let relation_projection = projection(1);
    let watermark = store
        .replace(&relation_projection)
        .expect("first publication");
    assert_eq!(
        store.replace(&relation_projection).expect("exact replay"),
        watermark
    );

    let cancelled = Arc::new(TestCancellation(AtomicBool::new(true)));
    assert_eq!(
        store.summary_sources(
            &relation_projection.scope,
            &relation_projection.session_id,
            1,
            "summary.root",
            3,
            cancelled,
        ),
        Err(SessionRelationError::Cancelled)
    );

    let mut conflicting = relation_projection;
    conflicting.summaries[0].sources.pop();
    assert_eq!(
        store.replace(&conflicting),
        Err(SessionRelationError::Conflict)
    );
}

#[test]
fn projection_rejects_cycles_without_overwriting_the_last_good_graph() {
    let store = memory_relation_store();
    let good = projection(1);
    store.replace(&good).expect("good projection");
    let mut cyclic = good.clone();
    cyclic.summaries[1].sources.push(SummarySourceRef::Summary {
        summary_id: "summary.root".to_owned(),
    });
    assert_eq!(store.replace(&cyclic), Err(SessionRelationError::Cycle));

    assert_eq!(
        store
            .summary_sources(
                &good.scope,
                &good.session_id,
                1,
                "summary.root",
                3,
                Arc::new(TestCancellation(AtomicBool::new(false))),
            )
            .expect("last good graph")
            .len(),
        3
    );
}

#[test]
fn projection_rejects_thread_hierarchy_cycles() {
    let store = memory_relation_store();
    let mut cyclic = projection(1);
    cyclic.thread_hierarchy = vec![
        ThreadHierarchyRelation {
            parent_thread_id: id::<ThreadId>("thread.parent"),
            child_thread_id: id::<ThreadId>("thread.child"),
            ordinal: 0,
        },
        ThreadHierarchyRelation {
            parent_thread_id: id::<ThreadId>("thread.child"),
            child_thread_id: id::<ThreadId>("thread.parent"),
            ordinal: 0,
        },
    ];
    assert_eq!(store.replace(&cyclic), Err(SessionRelationError::Cycle));
}

#[test]
fn projection_rejects_missing_predecessors_and_mismatched_copy_proofs() {
    let store = memory_relation_store();
    let mut missing_predecessor = projection(1);
    missing_predecessor.summaries[0].predecessor_summary_id = Some("summary.absent".to_owned());
    assert_eq!(
        store.replace(&missing_predecessor),
        Err(SessionRelationError::Invalid)
    );

    let mut mismatched_proof = projection(2);
    mismatched_proof.logical_copies = vec![LogicalCopyRelation {
        occurrence_id: occurrence_id("occurrence.copy"),
        copied_from_occurrence_id: occurrence_id("occurrence.source"),
        proof: CopyProofV1::ParentMessageLinkage {
            source_occurrence_id: occurrence_id("occurrence.other"),
            parent_message_id: id("message.source"),
        },
        knowledge_at: UtcMicros(1),
        valid_time: TemporalValidityV1::Unknown,
    }];
    assert_eq!(
        store.replace(&mismatched_proof),
        Err(SessionRelationError::Invalid)
    );
}

#[test]
fn session_context_reads_parent_and_workflow_membership_from_graph() {
    let store = memory_relation_store();
    let mut relation_projection = projection(3);
    relation_projection.parent_session_id = Some(id::<SessionId>("session.parent"));
    relation_projection.workflow_agents = vec![
        WorkflowAgentMembership {
            run_id: "run.alpha".to_owned(),
            agent_label: "review".to_owned(),
        },
        WorkflowAgentMembership {
            run_id: "run.alpha".to_owned(),
            agent_label: "implement".to_owned(),
        },
    ];
    store.replace(&relation_projection).expect("publication");

    let context = store
        .session_context(
            &relation_projection.scope,
            &relation_projection.session_id,
            3,
            3,
            Arc::new(TestCancellation(AtomicBool::new(false))),
        )
        .expect("session context");
    assert_eq!(
        context.parent_session_id.as_ref().map(SessionId::as_str),
        Some("session.parent")
    );
    assert_eq!(
        context
            .workflow_agents
            .iter()
            .map(|membership| membership.agent_label.as_str())
            .collect::<Vec<_>>(),
        vec!["implement", "review"]
    );
}

#[test]
fn project_and_profile_scopes_do_not_alias_identical_session_generations() {
    let store = memory_relation_store();
    let project = projection(1);
    let mut profile = project.clone();
    profile.scope =
        SessionRelationScope::profile_sessions(id::<UserProfileId>("profile.session-relations"));
    profile.summaries = vec![SummaryRelationNode {
        summary_id: "summary.root".to_owned(),
        sources: vec![SummarySourceRef::Anchor {
            anchor_id: id::<RetrievalAnchorId>("anchor.profile"),
        }],
        predecessor_summary_id: None,
    }];
    store.replace(&project).expect("project publication");
    store.replace(&profile).expect("profile publication");

    let profile_visits = store
        .summary_sources(
            &profile.scope,
            &profile.session_id,
            1,
            "summary.root",
            1,
            Arc::new(TestCancellation(AtomicBool::new(false))),
        )
        .expect("profile traversal");
    assert!(matches!(
        profile_visits.as_slice(),
        [visit]
            if matches!(
                &visit.source,
                SummarySourceVisitKind::Anchor { anchor_id }
                    if anchor_id.as_str() == "anchor.profile"
            )
    ));

    let project_visits = store
        .summary_sources(
            &project.scope,
            &project.session_id,
            1,
            "summary.root",
            3,
            Arc::new(TestCancellation(AtomicBool::new(false))),
        )
        .expect("project traversal");
    assert!(project_visits.iter().any(|visit| {
        matches!(
            &visit.source,
            SummarySourceVisitKind::Anchor { anchor_id }
                if anchor_id.as_str() == "anchor.root"
        )
    }));
    assert!(project_visits.iter().all(|visit| {
        !matches!(
            &visit.source,
            SummarySourceVisitKind::Anchor { anchor_id }
                if anchor_id.as_str() == "anchor.profile"
        )
    }));
}

#[test]
fn profile_relation_projection_survives_a_persistent_graph_restart() {
    let temporary = TempDir::new().expect("temporary graph root");
    let graph_path = temporary.path().join("profile-session-relations.grafeo");
    let mut relation_projection = projection(7);
    relation_projection.scope =
        SessionRelationScope::profile_sessions(id::<UserProfileId>("profile.restart"));

    let registry =
        GraphDbRegistry::new(GraphDbRegistryConfig { max_open: 1 }).expect("test graph registry");
    let request = persistent_registration(graph_path);
    {
        let database: Arc<GraphDb> = registry
            .resolve(request.clone())
            .expect("open persistent graph");
        SessionRelationGraphStore::new(database)
            .replace(&relation_projection)
            .expect("publish profile projection");
    }
    assert!(registry.close(&request).expect("close persistent graph"));

    let reopened: Arc<GraphDb> = registry.reopen(request).expect("reopen persistent graph");
    let visits = SessionRelationGraphStore::new(reopened)
        .summary_sources(
            &relation_projection.scope,
            &relation_projection.session_id,
            7,
            "summary.root",
            3,
            Arc::new(TestCancellation(AtomicBool::new(false))),
        )
        .expect("restart traversal");
    assert_eq!(visits.len(), 3);
    assert!(visits.iter().any(|visit| {
        matches!(
            &visit.source,
            SummarySourceVisitKind::Anchor { anchor_id }
                if anchor_id.as_str() == "anchor.root"
        )
    }));
}
