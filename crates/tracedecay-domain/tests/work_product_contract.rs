use std::collections::BTreeSet;

use tracedecay_domain::{
    AcceptanceCriterionId, InitiativeId, ManifestDigest, MilestoneId, ProposalId,
    RetrievalAnchorId, TaskEvidenceLinkId, TaskId, UtcMicros, WorkAcceptanceCriterionV1,
    WorkGraphChangeV1, WorkGraphVersionV1, WorkHierarchyV1, WorkInitiativeV1, WorkItemInputV1,
    WorkItemV1, WorkPlanId, WorkPlanV1, WorkProductContractError, WorkProductGraphV1,
    WorkProductProjectionBundleV1, WorkProductRelationV1, WorkProposalV1, WorkProposedChildV1,
    WorkRouteDecisionV1, WorkScoreKindV1, WorkShapeAssessmentV1, WorkSizingV1,
    WorkTaskEvidenceCoverageV1, WorkTaskEvidenceV1, WorkTimelineLaneV1,
};

fn id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    T::Error: std::fmt::Debug,
{
    T::try_from(value.to_owned()).unwrap()
}

fn digest(byte: char) -> ManifestDigest {
    ManifestDigest::new(format!("sha256:{}", byte.to_string().repeat(64))).unwrap()
}

fn hierarchy() -> WorkHierarchyV1 {
    WorkHierarchyV1::new(
        id::<InitiativeId>("initiative.release"),
        id::<WorkPlanId>("plan.release"),
        id::<MilestoneId>("milestone.release"),
    )
}

fn criterion(task: &str) -> WorkAcceptanceCriterionV1 {
    WorkAcceptanceCriterionV1::new(
        id::<AcceptanceCriterionId>(&format!("criterion.{task}")),
        format!("{task} has independently reviewed evidence"),
        true,
    )
    .unwrap()
}

fn item(task: &str, dependencies: &[&str], effort: u32) -> WorkItemV1 {
    WorkItemV1::new(WorkItemInputV1 {
        task_id: id::<TaskId>(task),
        hierarchy: hierarchy(),
        title: format!("Deliver {task}"),
        dependencies: dependencies
            .iter()
            .map(|value| id::<TaskId>(value))
            .collect(),
        informational_relations: BTreeSet::new(),
        causal_candidates: BTreeSet::new(),
        acceptance_criteria: vec![criterion(task)],
        effort,
        scheduled_at: None,
        deadline: Some(UtcMicros(1_000)),
        created_at: UtcMicros(10),
        updated_at: UtcMicros(10),
    })
    .unwrap()
}

fn graph(items: Vec<WorkItemV1>) -> WorkProductGraphV1 {
    WorkProductGraphV1::new(
        WorkGraphVersionV1::initial(),
        vec![
            WorkInitiativeV1::new(
                id("initiative.release"),
                "Release initiative".to_owned(),
                UtcMicros(1),
            )
            .unwrap(),
        ],
        vec![
            WorkPlanV1::new(
                id("plan.release"),
                id("initiative.release"),
                "Release plan".to_owned(),
                UtcMicros(2),
            )
            .unwrap(),
        ],
        vec![
            tracedecay_domain::WorkMilestoneV1::new(
                id("milestone.release"),
                id("plan.release"),
                "Release milestone".to_owned(),
                UtcMicros(3),
            )
            .unwrap(),
        ],
        items,
    )
    .unwrap()
}

#[test]
fn hierarchy_and_gating_dag_are_validated_as_one_graph() {
    let valid = graph(vec![
        item("task.a", &[], 3),
        item("task.b", &["task.a"], 5),
        item("task.c", &["task.a"], 2),
        item("task.d", &["task.b", "task.c"], 4),
    ]);
    assert_eq!(valid.items().len(), 4);
    assert!(valid.relations().contains(&WorkProductRelationV1::Gates {
        dependency: id("task.a"),
        dependent: id("task.b"),
    }));
    assert!(
        valid
            .relations()
            .contains(&WorkProductRelationV1::MilestoneContainsTask {
                milestone_id: id("milestone.release"),
                task_id: id("task.d"),
            })
    );

    let cycle = WorkProductGraphV1::new(
        WorkGraphVersionV1::initial(),
        valid.initiatives().to_vec(),
        valid.plans().to_vec(),
        valid.milestones().to_vec(),
        vec![
            item("task.a", &["task.d"], 3),
            item("task.b", &["task.a"], 5),
            item("task.c", &["task.a"], 2),
            item("task.d", &["task.b", "task.c"], 4),
        ],
    )
    .unwrap_err();
    assert_eq!(cycle, WorkProductContractError::DependencyCycle);

    let missing_milestone = WorkProductGraphV1::new(
        WorkGraphVersionV1::initial(),
        valid.initiatives().to_vec(),
        valid.plans().to_vec(),
        Vec::new(),
        vec![item("task.a", &[], 3)],
    )
    .unwrap_err();
    assert_eq!(
        missing_milestone,
        WorkProductContractError::UnknownHierarchy
    );
}

#[test]
fn every_work_view_is_a_projection_of_the_same_versioned_selection() {
    let graph = graph(vec![
        item("task.a", &[], 3),
        item("task.b", &["task.a"], 5),
        item("task.c", &["task.a"], 2),
        item("task.d", &["task.b", "task.c"], 4),
    ]);
    let bundle = WorkProductProjectionBundleV1::from_graph(&graph).unwrap();

    assert_eq!(bundle.graph_version(), graph.version());
    assert_eq!(bundle.kanban().graph_version(), graph.version());
    assert_eq!(bundle.dag().graph_version(), graph.version());
    assert_eq!(bundle.timeline().graph_version(), graph.version());
    assert_eq!(bundle.causal().graph_version(), graph.version());
    assert_eq!(bundle.critical_path().graph_version(), graph.version());
    assert_eq!(bundle.workload().graph_version(), graph.version());
    assert_eq!(
        bundle
            .critical_path()
            .task_ids()
            .iter()
            .map(TaskId::as_str)
            .collect::<Vec<_>>(),
        vec!["task.a", "task.b", "task.d"]
    );
    assert_eq!(bundle.critical_path().total_effort(), 12);
    assert_eq!(bundle.workload().total_effort(), 14);
    assert_eq!(bundle.dag().gating_edges().len(), 4);
    assert_eq!(
        bundle.kanban().lane_for(&id::<TaskId>("task.a")),
        Some(WorkTimelineLaneV1::Todo)
    );
    assert_eq!(
        bundle.kanban().lane_for(&id::<TaskId>("task.d")),
        Some(WorkTimelineLaneV1::Blocked)
    );
}

#[test]
fn task_evidence_is_task_rooted_bounded_and_exactly_expandable() {
    let task_id = id::<TaskId>("task.evidence");
    let evidence = WorkTaskEvidenceV1::new(
        task_id.clone(),
        WorkGraphVersionV1::new(7).unwrap(),
        vec![
            tracedecay_domain::TaskEvidenceLinkV1::new(
                id::<TaskEvidenceLinkId>("evidence.task.review"),
                2,
                task_id.clone(),
                id::<RetrievalAnchorId>("anchor.task.review"),
                digest('e'),
                UtcMicros(50),
            )
            .unwrap(),
        ],
        WorkTaskEvidenceCoverageV1::Partial {
            returned: 1,
            available: 3,
            unknowns: BTreeSet::from(["delivery evidence unavailable".to_owned()]),
        },
    )
    .unwrap();

    assert_eq!(evidence.task_id(), &task_id);
    assert_eq!(evidence.links().len(), 1);
    assert_eq!(
        evidence.links()[0].anchor_id().as_str(),
        "anchor.task.review"
    );

    let wrong_root = WorkTaskEvidenceV1::new(
        id("task.other"),
        WorkGraphVersionV1::new(7).unwrap(),
        evidence.links().to_vec(),
        WorkTaskEvidenceCoverageV1::Complete {
            returned: 1,
            available: 1,
        },
    )
    .unwrap_err();
    assert_eq!(wrong_root, WorkProductContractError::EvidenceTaskMismatch);
}

#[test]
fn accepting_a_decomposition_proposal_fans_out_without_changing_parent_identity() {
    let parent = id::<TaskId>("task.parent");
    let graph = graph(vec![item(parent.as_str(), &[], 8)]);
    let proposal = WorkProposalV1::new(
        id::<ProposalId>("proposal.parent.split"),
        parent.clone(),
        graph.version(),
        WorkShapeAssessmentV1::new(WorkScoreKindV1::Ordinal, 4, 3, 5, 2).unwrap(),
        WorkSizingV1::new(WorkScoreKindV1::Heuristic, 5, 8, 13, "cold-start").unwrap(),
        vec![
            WorkProposedChildV1::new(id("task.child.a"), "Child A".to_owned(), 3, BTreeSet::new())
                .unwrap(),
            WorkProposedChildV1::new(
                id("task.child.b"),
                "Child B".to_owned(),
                5,
                BTreeSet::from([id("task.child.a")]),
            )
            .unwrap(),
        ],
        WorkRouteDecisionV1::abstain("No admitted provider route").unwrap(),
        "Split independent preparation from the gated delivery step".to_owned(),
        digest('f'),
    )
    .unwrap();

    let accepted = graph
        .apply(WorkGraphChangeV1::ProposalAccepted {
            proposal,
            accepted_at: UtcMicros(20),
        })
        .unwrap();

    assert_eq!(accepted.items().len(), 3);
    assert!(accepted.item(&parent).is_some());
    assert_eq!(
        accepted
            .item(&parent)
            .unwrap()
            .accepted_proposal()
            .unwrap()
            .as_str(),
        "proposal.parent.split"
    );
    assert_eq!(
        accepted.item(&id("task.child.b")).unwrap().dependencies(),
        &BTreeSet::from([id("task.child.a")])
    );
}
