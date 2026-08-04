use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tracedecay_domain::{ProjectId, RetrievalAnchorId, SessionId, ThreadId};
use tracedecay_global_db::session_temporal::relations::{
    SessionRelationError, SessionRelationGraphStore, SessionRelationProjection,
    SessionRelationScope, SummaryRelationNode, SummarySourceRef, SummarySourceVisitKind,
    ThreadHierarchyRelation, WorkflowAgentMembership,
};
use tracedecay_graph_db::GraphCancellation;

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

fn projection(generation: u64) -> SessionRelationProjection {
    SessionRelationProjection {
        scope: SessionRelationScope::project(id::<ProjectId>("project.session-relations")),
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
    let store = SessionRelationGraphStore::memory().expect("graph");
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
    let store = SessionRelationGraphStore::memory().expect("graph");
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
    let store = SessionRelationGraphStore::memory().expect("graph");
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
    let store = SessionRelationGraphStore::memory().expect("graph");
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
fn session_context_reads_parent_and_workflow_membership_from_graph() {
    let store = SessionRelationGraphStore::memory().expect("graph");
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
