use super::*;

fn accept_proposal(
    graph: WorkProductGraphV1,
    task_id: &TaskId,
    accepted_at: UtcMicros,
) -> WorkProductGraphV1 {
    let proposal = WorkProposalV1::new(
        id("proposal.execution.admission"),
        task_id.clone(),
        graph.version(),
        WorkShapeAssessmentV1::new(WorkScoreKindV1::Ordinal, 2, 1, 1, 1).unwrap(),
        WorkSizingV1::new(WorkScoreKindV1::Heuristic, 1, 1, 1, "bounded").unwrap(),
        Vec::new(),
        WorkRouteDecisionV1::abstain("route selected by execution admission").unwrap(),
        "Admit the selected execution after proposal acceptance".to_owned(),
        digest('d'),
    )
    .unwrap();
    graph
        .apply(WorkGraphChangeV1::ProposalAccepted {
            proposal,
            accepted_at,
        })
        .unwrap()
}

fn admit_execution(
    graph: WorkProductGraphV1,
    task_id: &TaskId,
    admitted_at: UtcMicros,
) -> WorkProductGraphV1 {
    let based_on_version = graph.version();
    graph
        .apply(WorkGraphChangeV1::ExecutionAdmitted {
            task_id: task_id.clone(),
            based_on_version,
            admitted_at,
        })
        .unwrap()
}

fn link_accepted_attempt(
    graph: WorkProductGraphV1,
    task_id: &TaskId,
    identity: WorkAttemptIdentityV1,
    linked_at: UtcMicros,
) -> WorkProductGraphV1 {
    let based_on_version = graph.version();
    graph
        .apply(WorkGraphChangeV1::AcceptedAttemptLinked {
            task_id: task_id.clone(),
            based_on_version,
            identity,
            linked_at,
        })
        .unwrap()
}

#[test]
fn execution_admission_precedes_identity_linking_and_task_evidence_stays_separate() {
    let task_id = id::<TaskId>("task.attempt");
    let evidence = TaskEvidenceLinkV1::new(
        id("evidence.task.review"),
        1,
        task_id.clone(),
        id("anchor.task.review"),
        digest('e'),
        UtcMicros(15),
    )
    .unwrap();
    let original = graph(vec![item(task_id.as_str(), &[], 8)])
        .apply(WorkGraphChangeV1::EvidenceLinked {
            task_id: task_id.clone(),
            evidence,
        })
        .unwrap();
    let identity = attempt(&task_id, "accepted");

    assert_eq!(
        original
            .clone()
            .apply(WorkGraphChangeV1::AcceptedAttemptLinked {
                task_id: task_id.clone(),
                based_on_version: original.version(),
                identity: identity.clone(),
                linked_at: UtcMicros(20),
            })
            .unwrap_err(),
        WorkProductContractError::IllegalTransition
    );

    let accepted = accept_proposal(original, &task_id, UtcMicros(20));
    let before_admission = runtime(&accepted, UtcMicros(20), Vec::new());
    let before_actions =
        WorkProductProjectionBundleV1::from_graph(&accepted, &before_admission, UtcMicros(20))
            .unwrap()
            .kanban()
            .legal_actions_for(&task_id)
            .unwrap()
            .clone();
    assert!(before_actions.contains(&tracedecay_domain::WorkLegalActionV1::AdmitExecution));
    assert!(!before_actions.contains(&tracedecay_domain::WorkLegalActionV1::LinkAcceptedAttempt));

    let admitted = admit_execution(accepted, &task_id, UtcMicros(25));
    let item = admitted.item(&task_id).unwrap();
    assert_eq!(item.execution_admitted_at(), Some(UtcMicros(25)));
    assert!(item.is_execution_admitted());
    let admitted_runtime = runtime(&admitted, UtcMicros(25), Vec::new());
    let admitted_actions =
        WorkProductProjectionBundleV1::from_graph(&admitted, &admitted_runtime, UtcMicros(25))
            .unwrap()
            .kanban()
            .legal_actions_for(&task_id)
            .unwrap()
            .clone();
    assert!(!admitted_actions.contains(&tracedecay_domain::WorkLegalActionV1::AdmitExecution));
    assert!(admitted_actions.contains(&tracedecay_domain::WorkLegalActionV1::LinkAcceptedAttempt));

    let graph = link_accepted_attempt(admitted, &task_id, identity.clone(), UtcMicros(30));
    assert!(
        graph
            .item(&task_id)
            .unwrap()
            .accepted_attempts()
            .contains(&identity)
    );
    assert!(
        graph
            .relations()
            .contains(&WorkProductRelationV1::AcceptedAttempt {
                task_id: task_id.clone(),
                identity: identity.clone(),
            })
    );
    assert!(
        graph
            .item(&task_id)
            .unwrap()
            .evidence_links()
            .contains(&id("evidence.task.review"))
    );

    let runtime_snapshot = runtime(
        &graph,
        UtcMicros(40),
        vec![WorkRuntimeAttemptProjectionV1 {
            identity: identity.clone(),
            state: WorkAttemptStateV1::Running,
        }],
    );
    let projection =
        WorkProductProjectionBundleV1::from_graph(&graph, &runtime_snapshot, UtcMicros(40))
            .unwrap();
    assert_eq!(
        projection.kanban().lane_for(&task_id),
        Some(WorkTimelineLaneV1::Running)
    );
    assert_eq!(projection.workload().actual_concurrency(), Some(1));

    let accepted = graph
        .apply(WorkGraphChangeV1::TaskAccepted {
            task_id: task_id.clone(),
            evidence_by_criterion: BTreeMap::from([(
                id("criterion.task.attempt"),
                id("evidence.task.review"),
            )]),
            accepted_at: UtcMicros(40),
        })
        .unwrap();
    assert!(accepted.item(&task_id).unwrap().is_accepted());
}

#[test]
fn accepted_attempt_wire_is_json_safe_deterministic_and_rejects_duplicate_or_malformed_entries() {
    let task_id = id::<TaskId>("task.attempt.wire");
    let graph = admit_execution(
        accept_proposal(
            graph(vec![item(task_id.as_str(), &[], 8)]),
            &task_id,
            UtcMicros(20),
        ),
        &task_id,
        UtcMicros(25),
    );
    let graph = link_accepted_attempt(graph, &task_id, attempt(&task_id, "z"), UtcMicros(30));
    let graph = link_accepted_attempt(graph, &task_id, attempt(&task_id, "a"), UtcMicros(35));

    let wire = serde_json::to_value(&graph).expect("accepted attempts are JSON-safe");
    let attempts = wire["items"][0]["accepted_attempts"]
        .as_array()
        .expect("accepted attempts are a JSON array of identities");
    assert_eq!(attempts.len(), 2);
    assert_eq!(attempts[0]["run_id"], "run.a");
    assert_eq!(attempts[1]["run_id"], "run.z");

    let encoded = canonical_json_bytes(&graph).expect("accepted attempt graph is canonicalizable");
    let recovered: WorkProductGraphV1 =
        serde_json::from_value(wire.clone()).expect("canonical accepted-attempt wire recovers");
    assert_eq!(recovered, graph);
    assert_eq!(canonical_json_bytes(&recovered).unwrap(), encoded);

    let mut duplicate = wire.clone();
    let duplicate_entry = duplicate["items"][0]["accepted_attempts"][0].clone();
    duplicate["items"][0]["accepted_attempts"]
        .as_array_mut()
        .unwrap()
        .push(duplicate_entry);
    assert!(serde_json::from_value::<WorkProductGraphV1>(duplicate).is_err());

    let mut malformed = wire;
    malformed["items"][0]["accepted_attempts"][0]["task_id"] =
        serde_json::json!("task.someone-else");
    assert!(serde_json::from_value::<WorkProductGraphV1>(malformed).is_err());
}

#[test]
fn admission_and_identity_linking_reject_illegal_version_time_and_identity() {
    let task_id = id::<TaskId>("task.attempt.rejection");
    let accepted = accept_proposal(
        graph(vec![item(task_id.as_str(), &[], 8)]),
        &task_id,
        UtcMicros(20),
    );
    let identity = attempt(&task_id, "accepted");

    assert_eq!(
        accepted
            .clone()
            .apply(WorkGraphChangeV1::ExecutionAdmitted {
                task_id: task_id.clone(),
                based_on_version: WorkGraphVersionV1::initial(),
                admitted_at: UtcMicros(25),
            })
            .unwrap_err(),
        WorkProductContractError::IllegalTransition
    );
    assert_eq!(
        accepted
            .clone()
            .apply(WorkGraphChangeV1::ExecutionAdmitted {
                task_id: task_id.clone(),
                based_on_version: accepted.version(),
                admitted_at: UtcMicros(19),
            })
            .unwrap_err(),
        WorkProductContractError::InvalidTime
    );

    let admitted = admit_execution(accepted, &task_id, UtcMicros(25));
    assert_eq!(
        admitted
            .clone()
            .apply(WorkGraphChangeV1::ExecutionAdmitted {
                task_id: task_id.clone(),
                based_on_version: admitted.version(),
                admitted_at: UtcMicros(30),
            })
            .unwrap_err(),
        WorkProductContractError::IllegalTransition
    );
    assert_eq!(
        admitted
            .clone()
            .apply(WorkGraphChangeV1::AcceptedAttemptLinked {
                task_id: task_id.clone(),
                based_on_version: WorkGraphVersionV1::new(2).unwrap(),
                identity: identity.clone(),
                linked_at: UtcMicros(30),
            })
            .unwrap_err(),
        WorkProductContractError::IllegalTransition
    );
    assert_eq!(
        admitted
            .clone()
            .apply(WorkGraphChangeV1::AcceptedAttemptLinked {
                task_id: task_id.clone(),
                based_on_version: admitted.version(),
                identity: identity.clone(),
                linked_at: UtcMicros(24),
            })
            .unwrap_err(),
        WorkProductContractError::InvalidTime
    );
    assert_eq!(
        admitted
            .clone()
            .apply(WorkGraphChangeV1::AcceptedAttemptLinked {
                task_id: task_id.clone(),
                based_on_version: admitted.version(),
                identity: attempt(&id("task.other"), "mismatched"),
                linked_at: UtcMicros(30),
            })
            .unwrap_err(),
        WorkProductContractError::IllegalTransition
    );
    let linked = link_accepted_attempt(admitted, &task_id, identity.clone(), UtcMicros(30));
    assert_eq!(
        linked
            .apply(WorkGraphChangeV1::AcceptedAttemptLinked {
                task_id: task_id.clone(),
                based_on_version: WorkGraphVersionV1::new(4).unwrap(),
                identity,
                linked_at: UtcMicros(31),
            })
            .unwrap_err(),
        WorkProductContractError::DuplicateIdentity
    );
}

#[test]
fn partial_runtime_coverage_keeps_unknown_attempts_unavailable() {
    let task_id = id::<TaskId>("task.partial");
    let first = attempt(&task_id, "partial.first");
    let second = attempt(&task_id, "partial.second");
    let graph = admit_execution(
        accept_proposal(
            graph(vec![item(task_id.as_str(), &[], 1)]),
            &task_id,
            UtcMicros(20),
        ),
        &task_id,
        UtcMicros(25),
    );
    let graph = link_accepted_attempt(graph, &task_id, first.clone(), UtcMicros(30));
    let graph = link_accepted_attempt(graph, &task_id, second.clone(), UtcMicros(35));
    let runtime = WorkRuntimeProjectionV1::new(
        graph.version(),
        id("generation.runtime.partial"),
        WorkProjectionSequenceV1::new(3),
        UtcMicros(40),
        vec![WorkRuntimeAttemptProjectionV1 {
            identity: first,
            state: WorkAttemptStateV1::Running,
        }],
        WorkRuntimeProjectionCoverageV1::Partial {
            unavailable_attempts: BTreeSet::from([second]),
        },
    )
    .unwrap();
    let projection =
        WorkProductProjectionBundleV1::from_graph(&graph, &runtime, UtcMicros(40)).unwrap();

    assert_eq!(
        projection.kanban().lane_for(&task_id),
        Some(WorkTimelineLaneV1::Unavailable)
    );
    assert_eq!(projection.workload().actual_concurrency(), None);
}
