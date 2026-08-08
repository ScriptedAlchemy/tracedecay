use super::*;

#[test]
fn accepted_attempt_links_exact_evidence_without_copying_runtime_state() {
    let task_id = id::<TaskId>("task.attempt");
    let original = graph(vec![item(task_id.as_str(), &[], 8)]);
    let identity = attempt(&task_id, "accepted");
    let evidence = TaskEvidenceLinkV1::new(
        id("evidence.attempt"),
        1,
        task_id.clone(),
        id("anchor.attempt.receipt"),
        digest('d'),
        UtcMicros(70),
    )
    .unwrap();
    let graph = original
        .apply(WorkGraphChangeV1::AcceptedAttemptLinked {
            task_id: task_id.clone(),
            based_on_version: WorkGraphVersionV1::initial(),
            identity: identity.clone(),
            evidence,
            linked_at: UtcMicros(75),
        })
        .unwrap();

    assert_eq!(
        graph
            .item(&task_id)
            .unwrap()
            .accepted_attempts()
            .get(&identity),
        Some(&id("evidence.attempt"))
    );
    assert!(
        graph
            .relations()
            .contains(&WorkProductRelationV1::AcceptedAttempt {
                task_id: task_id.clone(),
                identity: identity.clone(),
                link_id: id("evidence.attempt"),
            })
    );
    let runtime_snapshot = runtime(
        &graph,
        UtcMicros(80),
        vec![WorkRuntimeAttemptProjectionV1 {
            identity: identity.clone(),
            state: WorkAttemptStateV1::Running,
        }],
    );
    let projection =
        WorkProductProjectionBundleV1::from_graph(&graph, &runtime_snapshot, UtcMicros(80))
            .unwrap();
    assert_eq!(
        projection.kanban().lane_for(&task_id),
        Some(WorkTimelineLaneV1::Running)
    );
    assert_eq!(projection.workload().actual_concurrency(), Some(1));
    assert_eq!(
        WorkProductProjectionBundleV1::from_graph(&graph, &runtime_snapshot, UtcMicros(81))
            .unwrap_err(),
        WorkProductContractError::IllegalTransition
    );
    let unavailable = WorkRuntimeProjectionV1::new(
        graph.version(),
        id("generation.runtime.unavailable"),
        WorkProjectionSequenceV1::new(2),
        UtcMicros(80),
        Vec::new(),
        WorkRuntimeProjectionCoverageV1::Unavailable,
    )
    .unwrap();
    let projection =
        WorkProductProjectionBundleV1::from_graph(&graph, &unavailable, UtcMicros(80)).unwrap();
    assert_eq!(
        projection.kanban().lane_for(&task_id),
        Some(WorkTimelineLaneV1::Unavailable)
    );
    assert_eq!(projection.workload().actual_concurrency(), None);
    for (state, lane) in [
        (WorkAttemptStateV1::Succeeded, WorkTimelineLaneV1::Review),
        (WorkAttemptStateV1::Cancelled, WorkTimelineLaneV1::Cancelled),
    ] {
        let observed = runtime(
            &graph,
            UtcMicros(80),
            vec![WorkRuntimeAttemptProjectionV1 {
                identity: identity.clone(),
                state,
            }],
        );
        assert_eq!(
            WorkProductProjectionBundleV1::from_graph(&graph, &observed, UtcMicros(80))
                .unwrap()
                .kanban()
                .lane_for(&task_id),
            Some(lane)
        );
    }

    let criterion_evidence =
        BTreeMap::from([(id("criterion.task.attempt"), id("evidence.attempt"))]);
    assert_eq!(
        graph
            .clone()
            .apply(WorkGraphChangeV1::TaskAccepted {
                task_id: task_id.clone(),
                evidence_by_criterion: criterion_evidence.clone(),
                accepted_at: UtcMicros(74),
            })
            .unwrap_err(),
        WorkProductContractError::InvalidTime
    );
    let stale_handoff = WorkHandoffV1::new(
        id::<WorkHandoffId>("handoff.attempt"),
        task_id.clone(),
        id::<ActorId>("actor.from"),
        id::<ActorId>("actor.to"),
        BTreeSet::from([id("evidence.attempt")]),
        BTreeSet::new(),
        UtcMicros(74),
    )
    .unwrap();
    assert_eq!(
        graph
            .clone()
            .apply(WorkGraphChangeV1::HandoffRecorded {
                handoff: stale_handoff,
            })
            .unwrap_err(),
        WorkProductContractError::InvalidTime
    );
    let accepted = graph
        .apply(WorkGraphChangeV1::TaskAccepted {
            task_id: task_id.clone(),
            evidence_by_criterion: criterion_evidence,
            accepted_at: UtcMicros(80),
        })
        .unwrap();
    assert!(accepted.item(&task_id).unwrap().is_accepted());
}

#[test]
fn accepted_attempt_wire_is_json_safe_deterministic_and_rejects_duplicate_or_malformed_entries() {
    let task_id = id::<TaskId>("task.attempt.wire");
    let evidence = |link_id: &str, suffix: &str| {
        TaskEvidenceLinkV1::new(
            id(link_id),
            1,
            task_id.clone(),
            id(&format!("anchor.attempt.wire.{suffix}")),
            digest('d'),
            UtcMicros(70),
        )
        .unwrap()
    };
    let graph = graph(vec![item(task_id.as_str(), &[], 8)])
        .apply(WorkGraphChangeV1::AcceptedAttemptLinked {
            task_id: task_id.clone(),
            based_on_version: WorkGraphVersionV1::initial(),
            identity: attempt(&task_id, "z"),
            evidence: evidence("evidence.attempt.wire.z", "z"),
            linked_at: UtcMicros(75),
        })
        .unwrap()
        .apply(WorkGraphChangeV1::AcceptedAttemptLinked {
            task_id: task_id.clone(),
            based_on_version: WorkGraphVersionV1::new(2).unwrap(),
            identity: attempt(&task_id, "a"),
            evidence: evidence("evidence.attempt.wire.a", "a"),
            linked_at: UtcMicros(76),
        })
        .unwrap();

    let wire = serde_json::to_value(&graph).expect("accepted attempts are JSON-safe");
    let attempts = wire["items"][0]["accepted_attempts"]
        .as_array()
        .expect("accepted attempts are a JSON array of identity-link entries");
    assert_eq!(attempts.len(), 2);
    assert_eq!(attempts[0]["identity"]["run_id"], "run.a");
    assert_eq!(attempts[1]["identity"]["run_id"], "run.z");

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
    malformed["items"][0]["accepted_attempts"][0]["identity"]["task_id"] =
        serde_json::json!("task.someone-else");
    assert!(serde_json::from_value::<WorkProductGraphV1>(malformed).is_err());
}

#[test]
fn accepted_attempt_links_reject_stale_mismatched_and_duplicate_evidence() {
    let task_id = id::<TaskId>("task.attempt");
    let original = graph(vec![item(task_id.as_str(), &[], 8)]);
    let identity = attempt(&task_id, "accepted");
    let evidence = || {
        TaskEvidenceLinkV1::new(
            id("evidence.attempt"),
            1,
            task_id.clone(),
            id("anchor.attempt.receipt"),
            digest('e'),
            UtcMicros(70),
        )
        .unwrap()
    };
    let mut malformed = serde_json::to_value(WorkGraphChangeV1::AcceptedAttemptLinked {
        task_id: task_id.clone(),
        based_on_version: WorkGraphVersionV1::initial(),
        identity: identity.clone(),
        evidence: evidence(),
        linked_at: UtcMicros(75),
    })
    .unwrap();
    malformed["evidence"]["revision"] = serde_json::json!(0);
    assert_eq!(
        original
            .clone()
            .apply(serde_json::from_value(malformed).unwrap())
            .unwrap_err(),
        WorkProductContractError::InvalidVersion
    );

    assert_eq!(
        original
            .clone()
            .apply(WorkGraphChangeV1::AcceptedAttemptLinked {
                task_id: task_id.clone(),
                based_on_version: WorkGraphVersionV1::new(2).unwrap(),
                identity: identity.clone(),
                evidence: evidence(),
                linked_at: UtcMicros(75),
            })
            .unwrap_err(),
        WorkProductContractError::IllegalTransition
    );
    assert_eq!(
        original
            .clone()
            .apply(WorkGraphChangeV1::AcceptedAttemptLinked {
                task_id: task_id.clone(),
                based_on_version: WorkGraphVersionV1::initial(),
                identity: identity.clone(),
                evidence: evidence(),
                linked_at: UtcMicros(9),
            })
            .unwrap_err(),
        WorkProductContractError::InvalidTime
    );
    let future_evidence = TaskEvidenceLinkV1::new(
        id("evidence.future"),
        1,
        task_id.clone(),
        id("anchor.attempt.future"),
        digest('f'),
        UtcMicros(76),
    )
    .unwrap();
    assert_eq!(
        original
            .clone()
            .apply(WorkGraphChangeV1::AcceptedAttemptLinked {
                task_id: task_id.clone(),
                based_on_version: WorkGraphVersionV1::initial(),
                identity: identity.clone(),
                evidence: future_evidence,
                linked_at: UtcMicros(75),
            })
            .unwrap_err(),
        WorkProductContractError::InvalidTime
    );
    assert_eq!(
        original
            .clone()
            .apply(WorkGraphChangeV1::AcceptedAttemptLinked {
                task_id: task_id.clone(),
                based_on_version: WorkGraphVersionV1::initial(),
                identity: attempt(&id("task.other"), "mismatched"),
                evidence: evidence(),
                linked_at: UtcMicros(75),
            })
            .unwrap_err(),
        WorkProductContractError::IllegalTransition
    );
    let wrong_evidence = TaskEvidenceLinkV1::new(
        id("evidence.other"),
        1,
        id("task.other"),
        id("anchor.attempt.other"),
        digest('f'),
        UtcMicros(70),
    )
    .unwrap();
    assert_eq!(
        original
            .clone()
            .apply(WorkGraphChangeV1::AcceptedAttemptLinked {
                task_id: task_id.clone(),
                based_on_version: WorkGraphVersionV1::initial(),
                identity: identity.clone(),
                evidence: wrong_evidence,
                linked_at: UtcMicros(75),
            })
            .unwrap_err(),
        WorkProductContractError::EvidenceTaskMismatch
    );
    let linked = original
        .apply(WorkGraphChangeV1::AcceptedAttemptLinked {
            task_id: task_id.clone(),
            based_on_version: WorkGraphVersionV1::initial(),
            identity: identity.clone(),
            evidence: evidence(),
            linked_at: UtcMicros(75),
        })
        .unwrap();
    assert_eq!(
        linked
            .apply(WorkGraphChangeV1::AcceptedAttemptLinked {
                task_id: task_id.clone(),
                based_on_version: WorkGraphVersionV1::new(2).unwrap(),
                identity,
                evidence: evidence(),
                linked_at: UtcMicros(76),
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
    let link = |link: &str, anchor: &str| {
        TaskEvidenceLinkV1::new(
            id(link),
            1,
            task_id.clone(),
            id(anchor),
            digest('a'),
            UtcMicros(20),
        )
        .unwrap()
    };
    let graph = graph(vec![item(task_id.as_str(), &[], 1)])
        .apply(WorkGraphChangeV1::AcceptedAttemptLinked {
            task_id: task_id.clone(),
            based_on_version: WorkGraphVersionV1::initial(),
            identity: first.clone(),
            evidence: link("evidence.partial.first", "anchor.partial.first"),
            linked_at: UtcMicros(20),
        })
        .unwrap()
        .apply(WorkGraphChangeV1::AcceptedAttemptLinked {
            task_id: task_id.clone(),
            based_on_version: WorkGraphVersionV1::new(2).unwrap(),
            identity: second.clone(),
            evidence: link("evidence.partial.second", "anchor.partial.second"),
            linked_at: UtcMicros(20),
        })
        .unwrap();
    let runtime = WorkRuntimeProjectionV1::new(
        graph.version(),
        id("generation.runtime.partial"),
        WorkProjectionSequenceV1::new(3),
        UtcMicros(30),
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
        WorkProductProjectionBundleV1::from_graph(&graph, &runtime, UtcMicros(30)).unwrap();

    assert_eq!(
        projection.kanban().lane_for(&task_id),
        Some(WorkTimelineLaneV1::Unavailable)
    );
    assert_eq!(projection.workload().actual_concurrency(), None);
}
