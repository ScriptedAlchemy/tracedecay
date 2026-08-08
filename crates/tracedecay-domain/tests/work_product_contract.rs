use std::collections::{BTreeMap, BTreeSet};

use tracedecay_domain::{
    AcceptanceCriterionId, ActorId, AttemptId, InitiativeId, MAX_WORK_PRODUCT_EVENT_EVIDENCE,
    MAX_WORK_PRODUCT_EVENT_RELATION_SCOPES, MAX_WORK_PRODUCT_EVENT_SOURCE_WATERMARKS,
    ManifestDigest, MilestoneId, ProjectionGenerationId, ProposalId, RetrievalAnchorId, RunId,
    SourceStoreId, TaskEvidenceLinkId, TaskEvidenceLinkV1, TaskId, UtcMicros,
    WorkAcceptanceCriterionV1, WorkAttemptIdentityV1, WorkAttemptStateV1, WorkGraphChangeV1,
    WorkGraphVersionV1, WorkHandoffId, WorkHandoffV1, WorkHierarchyV1, WorkInitiativeV1,
    WorkItemInputV1, WorkItemV1, WorkPlanId, WorkPlanV1, WorkProductAuthorizedRelationScopeV1,
    WorkProductContractError, WorkProductEventContractError, WorkProductEventEvidenceV1,
    WorkProductEventInputV1, WorkProductEventPayloadV1, WorkProductEventSequenceV1,
    WorkProductEventV1, WorkProductGraphV1, WorkProductProfileScopeV1,
    WorkProductProjectionBundleV1, WorkProductRelationV1, WorkProductSourceWatermarkV1,
    WorkProjectionSequenceV1, WorkProposalDispositionV1, WorkProposalV1, WorkProposedChildV1,
    WorkRelationReplanProposalV1, WorkRouteDecisionV1, WorkRuntimeAttemptProjectionV1,
    WorkRuntimeProjectionCoverageV1, WorkRuntimeProjectionV1, WorkScoreKindV1,
    WorkShapeAssessmentV1, WorkSizingV1, WorkTaskEvidenceCoverageV1, WorkTaskEvidenceV1,
    WorkTimelineLaneV1, canonical_json_bytes,
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
    item_scheduled_at(task, dependencies, effort, None)
}

fn item_scheduled_at(
    task: &str,
    dependencies: &[&str],
    effort: u32,
    scheduled_at: Option<UtcMicros>,
) -> WorkItemV1 {
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
        scheduled_at,
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

fn attempt(task_id: &TaskId, suffix: &str) -> WorkAttemptIdentityV1 {
    WorkAttemptIdentityV1::new(
        task_id.clone(),
        id::<RunId>(&format!("run.{suffix}")),
        id::<AttemptId>(&format!("attempt.{suffix}")),
    )
    .unwrap()
}

fn runtime(
    graph: &WorkProductGraphV1,
    observed_at: UtcMicros,
    attempts: Vec<WorkRuntimeAttemptProjectionV1>,
) -> WorkRuntimeProjectionV1 {
    WorkRuntimeProjectionV1::new(
        graph.version(),
        id::<ProjectionGenerationId>("generation.runtime.contract"),
        WorkProjectionSequenceV1::new(1),
        observed_at,
        attempts,
        WorkRuntimeProjectionCoverageV1::Complete,
    )
    .unwrap()
}

#[path = "work_product_contract/accepted_attempt.rs"]
mod accepted_attempt;

fn graph_with_accepted_relation_replan(
    items: Vec<WorkItemV1>,
    task_id: &str,
    proposal_id: &str,
    dependencies: &[&str],
    informational_relations: &[&str],
    causal_candidates: &[&str],
) -> WorkProductGraphV1 {
    let graph = graph(items);
    let proposal = WorkRelationReplanProposalV1::new(
        id(proposal_id),
        id(task_id),
        graph.version(),
        dependencies.iter().map(|value| id(value)).collect(),
        informational_relations
            .iter()
            .map(|value| id(value))
            .collect(),
        causal_candidates.iter().map(|value| id(value)).collect(),
    )
    .unwrap();
    graph
        .apply(WorkGraphChangeV1::RelationReplanDecided {
            proposal,
            disposition: WorkProposalDispositionV1::Accepted,
            decided_at: UtcMicros(20),
        })
        .unwrap()
}

fn relations_replanned(proposal_id: &str, applied_at: UtcMicros) -> WorkGraphChangeV1 {
    WorkGraphChangeV1::TaskRelationsReplanned {
        proposal_id: id(proposal_id),
        applied_at,
    }
}

fn work_product_event_input(event_id: &str, task_id: &str) -> WorkProductEventInputV1 {
    WorkProductEventInputV1 {
        event_id: id(event_id),
        sequence: WorkProductEventSequenceV1::new(1).unwrap(),
        actor_id: id("actor.contract"),
        owner_scope: WorkProductProfileScopeV1 {
            brain_id: id("brain.contract"),
            profile_id: id("profile.contract"),
        },
        authorized_relation_scopes: Vec::new(),
        expected_graph_version: None,
        result_graph_version: WorkGraphVersionV1::initial(),
        command_id: id("command.contract"),
        canonical_input_digest: digest('1'),
        causation_event_id: None,
        evidence: Vec::new(),
        source_watermark: WorkProductSourceWatermarkV1::new(BTreeMap::new()).unwrap(),
        occurred_at: UtcMicros(0),
        policy_revision_id: id("policy.contract"),
        configuration_revision_id: id("configuration.contract"),
        catalog_generation_id: id("catalog.contract"),
        payload: WorkProductEventPayloadV1::Created {
            graph: graph(vec![item(task_id, &[], 1)]),
        },
    }
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
fn relation_replanning_replaces_all_selected_task_relations_at_one_version() {
    let selected = id::<TaskId>("task.c");
    let original = graph_with_accepted_relation_replan(
        vec![
            item("task.a", &[], 3),
            item("task.b", &[], 5),
            item("task.c", &["task.b"], 2),
        ],
        "task.c",
        "proposal.c.replan",
        &["task.a"],
        &["task.b"],
        &["task.a"],
    );

    let replanned = original
        .apply(relations_replanned("proposal.c.replan", UtcMicros(30)))
        .unwrap();

    assert_eq!(replanned.version().get(), 3);
    let item = replanned.item(&selected).unwrap();
    assert_eq!(item.dependencies(), &BTreeSet::from([id("task.a")]));
    assert_eq!(
        item.informational_relations(),
        &BTreeSet::from([id("task.b")])
    );
    assert_eq!(item.causal_candidates(), &BTreeSet::from([id("task.a")]));
    assert!(item.accepted_proposal().is_none());
    assert!(item.accepted_attempts().is_empty());
    assert_eq!(item.updated_at(), UtcMicros(30));
}

#[test]
fn accepted_relation_replan_payload_cannot_be_substituted_at_apply_time() {
    let ordered = WorkRelationReplanProposalV1::new(
        id("proposal.order.a"),
        id("task.c"),
        WorkGraphVersionV1::initial(),
        vec![id("task.a"), id("task.b")],
        Vec::new(),
        Vec::new(),
    )
    .unwrap();
    let reversed = WorkRelationReplanProposalV1::new(
        id("proposal.order.b"),
        id("task.c"),
        WorkGraphVersionV1::initial(),
        vec![id("task.b"), id("task.a")],
        Vec::new(),
        Vec::new(),
    )
    .unwrap();
    assert_eq!(ordered.payload_digest, reversed.payload_digest);
    assert_eq!(ordered.dependencies(), reversed.dependencies());

    let original = graph_with_accepted_relation_replan(
        vec![
            item("task.a", &[], 3),
            item("task.b", &[], 5),
            item("task.c", &["task.b"], 2),
        ],
        "task.c",
        "proposal.c.replan",
        &["task.a"],
        &[],
        &[],
    );
    let mut encoded =
        serde_json::to_value(relations_replanned("proposal.c.replan", UtcMicros(30))).unwrap();
    encoded["dependencies"] = serde_json::json!(["task.b"]);

    assert!(serde_json::from_value::<WorkGraphChangeV1>(encoded).is_err());
    let replanned = original
        .apply(relations_replanned("proposal.c.replan", UtcMicros(30)))
        .unwrap();
    assert_eq!(
        replanned.item(&id("task.c")).unwrap().dependencies(),
        &BTreeSet::from([id("task.a")])
    );
}

#[test]
fn relation_replanning_rejects_stale_unknown_duplicate_self_and_cyclic_proposals() {
    let original = graph(vec![
        item("task.a", &[], 3),
        item("task.b", &["task.a"], 5),
        item("task.c", &[], 2),
    ]);
    let proposal = |task: &str,
                    proposal: &str,
                    dependencies: &[&str],
                    informational: &[&str],
                    causal: &[&str]| {
        WorkRelationReplanProposalV1::new(
            id(proposal),
            id(task),
            original.version(),
            dependencies.iter().map(|value| id(value)).collect(),
            informational.iter().map(|value| id(value)).collect(),
            causal.iter().map(|value| id(value)).collect(),
        )
    };
    assert_eq!(
        proposal(
            "task.c",
            "proposal.duplicate",
            &["task.a", "task.a"],
            &[],
            &[]
        )
        .unwrap_err(),
        WorkProductContractError::DuplicateIdentity
    );
    assert_eq!(
        proposal("task.c", "proposal.self-gating", &["task.c"], &[], &[]).unwrap_err(),
        WorkProductContractError::DependencyCycle
    );
    assert_eq!(
        proposal("task.c", "proposal.self-info", &[], &["task.c"], &[]).unwrap_err(),
        WorkProductContractError::IllegalTransition
    );
    assert_eq!(
        proposal("task.c", "proposal.self-causal", &[], &[], &["task.c"]).unwrap_err(),
        WorkProductContractError::IllegalTransition
    );

    let decide = |proposal| WorkGraphChangeV1::RelationReplanDecided {
        proposal,
        disposition: WorkProposalDispositionV1::Accepted,
        decided_at: UtcMicros(20),
    };
    assert_eq!(
        original
            .clone()
            .apply(WorkGraphChangeV1::RelationReplanDecided {
                proposal: proposal("task.c", "proposal.stale-time", &[], &[], &[]).unwrap(),
                disposition: WorkProposalDispositionV1::Accepted,
                decided_at: UtcMicros(9),
            })
            .unwrap_err(),
        WorkProductContractError::InvalidTime
    );
    let mut mismatched_digest = serde_json::to_value(
        proposal(
            "task.c",
            "proposal.mismatched-digest",
            &["task.a"],
            &[],
            &[],
        )
        .unwrap(),
    )
    .unwrap();
    mismatched_digest["payload_digest"] = serde_json::to_value(digest('0')).unwrap();
    assert_eq!(
        original
            .clone()
            .apply(decide(
                serde_json::from_value::<WorkRelationReplanProposalV1>(mismatched_digest).unwrap()
            ))
            .unwrap_err(),
        WorkProductContractError::ProposalMismatch
    );
    assert_eq!(
        original
            .clone()
            .apply(decide(
                proposal("task.unknown", "proposal.unknown-task", &[], &[], &[]).unwrap()
            ))
            .unwrap_err(),
        WorkProductContractError::UnknownTask
    );
    assert_eq!(
        original
            .clone()
            .apply(decide(
                proposal(
                    "task.c",
                    "proposal.unknown-relation",
                    &["task.unknown"],
                    &[],
                    &[]
                )
                .unwrap()
            ))
            .unwrap_err(),
        WorkProductContractError::UnknownTask
    );
    assert_eq!(
        original
            .clone()
            .apply(decide(
                proposal("task.a", "proposal.cycle", &["task.b"], &[], &[]).unwrap()
            ))
            .unwrap_err(),
        WorkProductContractError::DependencyCycle
    );
    assert_eq!(original.version(), WorkGraphVersionV1::initial());

    let accepted = graph_with_accepted_relation_replan(
        original.items().to_vec(),
        "task.c",
        "proposal.c.stale",
        &["task.a"],
        &[],
        &[],
    );
    let advanced = accepted
        .apply(WorkGraphChangeV1::TaskAdded {
            item: Box::new(item("task.d", &[], 1)),
        })
        .unwrap();
    assert_eq!(
        advanced
            .apply(relations_replanned("proposal.c.stale", UtcMicros(30)))
            .unwrap_err(),
        WorkProductContractError::ProposalMismatch
    );
}

#[test]
fn informational_and_causal_relations_may_form_multi_task_cycles() {
    let graph = graph_with_accepted_relation_replan(
        vec![item("task.a", &[], 3), item("task.b", &[], 5)],
        "task.a",
        "proposal.a.relations",
        &[],
        &["task.b"],
        &["task.b"],
    )
    .apply(relations_replanned("proposal.a.relations", UtcMicros(30)))
    .unwrap();
    let graph = graph_with_accepted_relation_replan(
        graph.items().to_vec(),
        "task.b",
        "proposal.b.relations",
        &[],
        &["task.a"],
        &["task.a"],
    )
    .apply(relations_replanned("proposal.b.relations", UtcMicros(30)))
    .unwrap();

    assert_eq!(
        graph.item(&id("task.a")).unwrap().informational_relations(),
        &BTreeSet::from([id("task.b")])
    );
    assert_eq!(
        graph.item(&id("task.b")).unwrap().causal_candidates(),
        &BTreeSet::from([id("task.a")])
    );
}

#[test]
fn work_product_event_envelopes_pin_profile_authority_versions_and_exact_evidence() {
    let mut input = work_product_event_input("event.work-product.1", "task.event");
    input.sequence = WorkProductEventSequenceV1::new(7).unwrap();
    input.authorized_relation_scopes = vec![
        WorkProductAuthorizedRelationScopeV1::Repository {
            project_id: id("project.contract"),
            repository_id: id("repository.contract"),
        },
        WorkProductAuthorizedRelationScopeV1::Project {
            project_id: id("project.contract"),
        },
    ];
    input.evidence = vec![WorkProductEventEvidenceV1 {
        source_store_id: id("source.contract"),
        anchor_id: id("anchor.contract"),
        evidence_digest: digest('2'),
    }];
    input.source_watermark =
        WorkProductSourceWatermarkV1::new(BTreeMap::from([(id("source.contract"), 11)])).unwrap();
    let event = WorkProductEventV1::new(input).unwrap();

    assert_eq!(event.expected_graph_version(), None);
    assert_eq!(event.result_graph_version(), WorkGraphVersionV1::initial());
    assert_eq!(event.occurred_at(), UtcMicros(0));
    assert_eq!(event.evidence()[0].anchor_id.as_str(), "anchor.contract");
    assert_eq!(event.authorized_relation_scopes().len(), 2);
    assert!(matches!(
        event.authorized_relation_scopes()[0],
        WorkProductAuthorizedRelationScopeV1::Project { .. }
    ));
    let encoded = serde_json::to_value(&event).unwrap();
    assert_eq!(
        serde_json::from_value::<WorkProductEventV1>(encoded).unwrap(),
        event
    );
}

#[test]
fn work_product_event_deserialization_rejects_invalid_creation_progression_and_self_causation() {
    let input = work_product_event_input("event.work-product.invalid", "task.event.invalid");
    let event = WorkProductEventV1::new(input).unwrap();
    let mut noncontiguous = serde_json::to_value(&event).unwrap();
    noncontiguous["result_graph_version"] = serde_json::json!(2);
    assert!(serde_json::from_value::<WorkProductEventV1>(noncontiguous).is_err());

    let mut self_caused = serde_json::to_value(event).unwrap();
    self_caused["causation_event_id"] = serde_json::json!("event.work-product.invalid");
    assert_eq!(
        serde_json::from_value::<WorkProductEventV1>(self_caused)
            .unwrap_err()
            .to_string(),
        WorkProductEventContractError::SelfCausation.to_string()
    );
}

#[test]
fn work_product_event_rejects_created_and_changed_payload_version_substitution() {
    let mut created =
        work_product_event_input("event.work-product.created-mismatch", "task.event.created");
    created.expected_graph_version = Some(WorkGraphVersionV1::initial());
    created.result_graph_version = WorkGraphVersionV1::new(2).unwrap();
    assert_eq!(
        WorkProductEventV1::new(created).unwrap_err(),
        WorkProductEventContractError::InvalidVersionProgression
    );

    let mut changed =
        work_product_event_input("event.work-product.changed-mismatch", "task.event.changed");
    changed.payload = WorkProductEventPayloadV1::Changed {
        change: Box::new(WorkGraphChangeV1::TaskAdded {
            item: Box::new(item("task.event.changed.next", &[], 1)),
        }),
    };
    assert_eq!(
        WorkProductEventV1::new(changed.clone()).unwrap_err(),
        WorkProductEventContractError::InvalidVersionProgression
    );
    changed.expected_graph_version = Some(WorkGraphVersionV1::initial());
    changed.result_graph_version = WorkGraphVersionV1::new(2).unwrap();
    assert!(WorkProductEventV1::new(changed).is_ok());

    let relation_proposal = WorkRelationReplanProposalV1::new(
        id("proposal.event.replan"),
        id("task.event.changed"),
        WorkGraphVersionV1::initial(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
    )
    .unwrap();
    let mut relation_event =
        work_product_event_input("event.work-product.replan", "task.event.changed");
    relation_event.expected_graph_version = Some(WorkGraphVersionV1::initial());
    relation_event.result_graph_version = WorkGraphVersionV1::new(2).unwrap();
    relation_event.payload = WorkProductEventPayloadV1::Changed {
        change: Box::new(WorkGraphChangeV1::RelationReplanDecided {
            proposal: relation_proposal,
            disposition: WorkProposalDispositionV1::Accepted,
            decided_at: UtcMicros(1),
        }),
    };
    let mut mismatched_digest =
        serde_json::to_value(WorkProductEventV1::new(relation_event).unwrap()).unwrap();
    mismatched_digest["payload"]["change"]["proposal"]["payload_digest"] =
        serde_json::to_value(digest('0')).unwrap();
    assert!(serde_json::from_value::<WorkProductEventV1>(mismatched_digest).is_err());

    let version_two_graph = graph(vec![item("task.event.graph", &[], 1)])
        .apply(WorkGraphChangeV1::TaskAdded {
            item: Box::new(item("task.event.graph.next", &[], 1)),
        })
        .unwrap();
    let mut wrong_created =
        work_product_event_input("event.work-product.graph-mismatch", "task.event.ignored");
    wrong_created.payload = WorkProductEventPayloadV1::Created {
        graph: version_two_graph,
    };
    assert_eq!(
        WorkProductEventV1::new(wrong_created).unwrap_err(),
        WorkProductEventContractError::InvalidVersionProgression
    );
}

#[test]
fn work_product_event_rejects_duplicate_authorized_scopes() {
    let relation_scope = WorkProductAuthorizedRelationScopeV1::Project {
        project_id: id("project.contract"),
    };
    let mut input = work_product_event_input(
        "event.work-product.duplicate-scope",
        "task.event.duplicate-scope",
    );
    input.authorized_relation_scopes = vec![relation_scope.clone(), relation_scope];
    let rejected = WorkProductEventV1::new(input).unwrap_err();

    assert_eq!(
        rejected,
        WorkProductEventContractError::DuplicateRelationScope
    );
}

#[test]
fn work_product_event_rejects_duplicate_evidence_and_missing_source_watermarks() {
    let evidence = WorkProductEventEvidenceV1 {
        source_store_id: id("source.contract"),
        anchor_id: id("anchor.contract"),
        evidence_digest: digest('4'),
    };
    let mut duplicate =
        work_product_event_input("event.work-product.duplicate", "task.event.duplicate");
    duplicate.evidence = vec![evidence.clone(), evidence.clone()];
    duplicate.source_watermark =
        WorkProductSourceWatermarkV1::new(BTreeMap::from([(id("source.contract"), 1)])).unwrap();

    assert_eq!(
        WorkProductEventV1::new(duplicate).unwrap_err(),
        WorkProductEventContractError::DuplicateEvidence
    );

    let mut missing_source =
        work_product_event_input("event.work-product.no-watermark", "task.event.no-watermark");
    missing_source.evidence = vec![evidence];
    assert_eq!(
        WorkProductEventV1::new(missing_source).unwrap_err(),
        WorkProductEventContractError::MissingEvidenceSourceWatermark
    );
}

#[test]
fn work_product_event_sequence_and_metadata_bounds_are_enforced() {
    assert_eq!(
        WorkProductEventSequenceV1::new(0).unwrap_err(),
        WorkProductEventContractError::InvalidSequence
    );
    assert_eq!(
        WorkProductSourceWatermarkV1::new(BTreeMap::from([(id("source.zero"), 0)])).unwrap_err(),
        WorkProductEventContractError::InvalidSourceWatermarkSequence
    );
    assert_eq!(
        serde_json::from_value::<WorkProductSourceWatermarkV1>(serde_json::json!({
            "source.zero": 0
        }))
        .unwrap_err()
        .to_string(),
        WorkProductEventContractError::InvalidSourceWatermarkSequence.to_string()
    );
    let components = (0..=MAX_WORK_PRODUCT_EVENT_SOURCE_WATERMARKS)
        .map(|ordinal| {
            (
                id::<SourceStoreId>(&format!("source.contract.{ordinal}")),
                ordinal as u64 + 1,
            )
        })
        .collect();
    assert_eq!(
        WorkProductSourceWatermarkV1::new(components).unwrap_err(),
        WorkProductEventContractError::TooManySourceWatermarks
    );

    let mut too_many_scopes =
        work_product_event_input("event.work-product.scopes-bound", "task.event.scopes-bound");
    too_many_scopes.authorized_relation_scopes = (0..=MAX_WORK_PRODUCT_EVENT_RELATION_SCOPES)
        .map(|ordinal| WorkProductAuthorizedRelationScopeV1::Project {
            project_id: id(&format!("project.contract.{ordinal}")),
        })
        .collect();
    assert_eq!(
        WorkProductEventV1::new(too_many_scopes).unwrap_err(),
        WorkProductEventContractError::TooManyRelationScopes
    );

    let mut too_much_evidence = work_product_event_input(
        "event.work-product.evidence-bound",
        "task.event.evidence-bound",
    );
    too_much_evidence.evidence = (0..=MAX_WORK_PRODUCT_EVENT_EVIDENCE)
        .map(|ordinal| WorkProductEventEvidenceV1 {
            source_store_id: id("source.contract"),
            anchor_id: id(&format!("anchor.contract.{ordinal}")),
            evidence_digest: digest('6'),
        })
        .collect();
    too_much_evidence.source_watermark =
        WorkProductSourceWatermarkV1::new(BTreeMap::from([(id("source.contract"), 1)])).unwrap();
    assert_eq!(
        WorkProductEventV1::new(too_much_evidence).unwrap_err(),
        WorkProductEventContractError::TooMuchEvidence
    );
}

#[test]
fn crafted_json_cannot_deserialize_a_cyclic_graph_snapshot() {
    let graph = graph(vec![item("task.a", &[], 3), item("task.b", &["task.a"], 5)]);
    let mut encoded = serde_json::to_value(graph).unwrap();
    encoded["items"][0]["input"]["dependencies"] = serde_json::json!(["task.b"]);

    assert!(serde_json::from_value::<WorkProductGraphV1>(encoded).is_err());
}

#[test]
fn every_work_view_is_a_projection_of_the_same_versioned_selection() {
    let graph = graph(vec![
        item("task.a", &[], 3),
        item("task.b", &["task.a"], 5),
        item("task.c", &["task.a"], 2),
        item("task.d", &["task.b", "task.c"], 4),
    ]);
    let bundle = WorkProductProjectionBundleV1::from_graph(
        &graph,
        &runtime(&graph, UtcMicros(100), Vec::new()),
        UtcMicros(100),
    )
    .unwrap();

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
fn an_initial_empty_graph_has_empty_zero_effort_projections() {
    let graph = graph(Vec::new());
    let bundle = WorkProductProjectionBundleV1::from_graph(
        &graph,
        &runtime(&graph, UtcMicros(0), Vec::new()),
        UtcMicros(0),
    )
    .unwrap();

    assert!(bundle.critical_path().task_ids().is_empty());
    assert_eq!(bundle.critical_path().total_effort(), 0);
    assert_eq!(bundle.workload().total_effort(), 0);
    assert!(bundle.dag().gating_edges().is_empty());
}

#[test]
fn projection_observation_time_controls_scheduled_lane_boundaries() {
    let task_id = id::<TaskId>("task.scheduled");
    let graph = graph(vec![item_scheduled_at(
        task_id.as_str(),
        &[],
        3,
        Some(UtcMicros(500)),
    )]);

    let before_schedule = WorkProductProjectionBundleV1::from_graph(
        &graph,
        &runtime(&graph, UtcMicros(0), Vec::new()),
        UtcMicros(0),
    )
    .unwrap();
    assert_eq!(
        before_schedule.kanban().lane_for(&task_id),
        Some(WorkTimelineLaneV1::Scheduled)
    );
    let at_schedule = WorkProductProjectionBundleV1::from_graph(
        &graph,
        &runtime(&graph, UtcMicros(500), Vec::new()),
        UtcMicros(500),
    )
    .unwrap();
    assert_eq!(
        at_schedule.kanban().lane_for(&task_id),
        Some(WorkTimelineLaneV1::Todo)
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

    let mut wrong_root = serde_json::to_value(&evidence).unwrap();
    wrong_root["task_id"] = serde_json::json!("task.other");
    assert_eq!(
        serde_json::from_value::<WorkTaskEvidenceV1>(wrong_root)
            .unwrap()
            .validate()
            .unwrap_err(),
        WorkProductContractError::EvidenceTaskMismatch
    );
    let mut invalid_link = serde_json::to_value(&evidence).unwrap();
    invalid_link["links"][0]["revision"] = serde_json::json!(0);
    assert_eq!(
        serde_json::from_value::<WorkTaskEvidenceV1>(invalid_link)
            .unwrap()
            .validate()
            .unwrap_err(),
        WorkProductContractError::InvalidVersion
    );
    let mut duplicate_link = serde_json::to_value(&evidence).unwrap();
    let repeated_link = duplicate_link["links"][0].clone();
    duplicate_link["links"]
        .as_array_mut()
        .unwrap()
        .push(repeated_link);
    duplicate_link["coverage"] = serde_json::json!({"state":"complete","returned":2,"available":2});
    assert_eq!(
        serde_json::from_value::<WorkTaskEvidenceV1>(duplicate_link)
            .unwrap()
            .validate()
            .unwrap_err(),
        WorkProductContractError::DuplicateIdentity
    );
    for coverage in [
        serde_json::json!({"state":"complete","returned":1,"available":2}),
        serde_json::json!({"state":"partial","returned":2,"available":1,"unknowns":["missing"]}),
        serde_json::json!({"state":"partial","returned":1,"available":2,"unknowns":[]}),
        serde_json::json!({"state":"partial","returned":1,"available":2,"unknowns":["bad\ntext"]}),
    ] {
        let mut malformed = serde_json::to_value(&evidence).unwrap();
        malformed["coverage"] = coverage;
        assert!(
            serde_json::from_value::<WorkTaskEvidenceV1>(malformed)
                .unwrap()
                .validate()
                .is_err()
        );
    }
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

    assert_eq!(
        graph
            .clone()
            .apply(WorkGraphChangeV1::ProposalAccepted {
                proposal: proposal.clone(),
                accepted_at: UtcMicros(9),
            })
            .unwrap_err(),
        WorkProductContractError::InvalidTime
    );
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
