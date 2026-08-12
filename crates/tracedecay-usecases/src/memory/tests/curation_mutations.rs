use serde_json::json;
use tracedecay_domain::{
    ActorId, Confidence, FactCategoryV1, FactEventId, FactOwnerV1, PayloadAccessState, RunId,
    UtcMicros,
};
use tracedecay_store::{
    ProjectMemoryFactAddOutcomeV1, ProjectMemoryFactCurationBatchV1,
    ProjectMemoryFactCurationEvidenceV1, ProjectMemoryFactCurationMutationKindV1,
    ProjectMemoryFactCurationOperationEffectV1, ProjectMemoryFactCurationOperationV1,
    ProjectMemoryFactCurationReceiptV1, ProjectMemoryFactCurationRemoveV1, ProjectMemoryFactIdV1,
    ProjectMemoryFactProjectionV1, ProjectMemoryFactRemoveOutcomeV1, ProjectMemoryFactStatusV1,
    ProjectMemoryFactUnavailableV1, ProjectMemoryFactUpdatePatchV1,
    derive_project_memory_fact_curation_child_operation_id,
};

use super::*;

fn mutation_target(
    owner: FactOwnerV1,
    operation: &str,
    event: &str,
) -> ProjectMemoryFactMutationTarget {
    ProjectMemoryFactMutationTarget::exact(
        fact_id(owner, operation),
        FactEventId::new(event.to_owned()).unwrap(),
    )
}

fn context(owner: &FactOwnerV1, action: &str) -> MemoryOperationContext {
    MemoryOperationContext::from_request_id(
        owner,
        action,
        "request.curation-mutation",
        Some(ActorId::new("actor.curation-mutation").unwrap()),
    )
    .unwrap()
}

fn add_request(content: &str) -> ProjectMemoryFactAddRequest {
    ProjectMemoryFactAddRequest {
        content: content.to_owned(),
        category: FactCategoryV1::General,
        source_label: Some("memory-curator".to_owned()),
        tags: vec!["durable".to_owned(), "reviewed".to_owned()],
        entities: vec!["TraceDecay".to_owned()],
        trust: Some(Confidence::new(0.92).unwrap()),
        metadata: json!({"reviewed": true}),
    }
}

fn add_operation(content: &str, owner: &FactOwnerV1) -> ProjectMemoryCurationOperation {
    ProjectMemoryCurationOperation::Add {
        request: add_request(content),
        evidence_facts: vec![ProjectMemoryCurationMutationTarget::new(
            fact_id(owner.clone(), "operation.curation-add-evidence"),
            FactEventId::new("event.curation-add-evidence".to_owned()).unwrap(),
        )],
        confidence: Confidence::new(0.96).unwrap(),
        reason: "reviewed canonical add evidence".to_owned(),
    }
}

#[tokio::test]
async fn curation_add_reuses_privacy_preflight_and_binds_canonical_child_identity() {
    let owner = owner();
    let application = MemoryApplication::new(owner.clone(), FakeAuthority::default()).unwrap();
    let outer_context = context(&owner, "curation-add");
    let run_id = RunId::new("run.curation-add").unwrap();

    let result = application
        .apply_project_memory_curation(
            vec![add_operation("canonical reviewed memory", &owner)],
            Confidence::new(0.9).unwrap(),
            outer_context.clone(),
            Some(run_id.clone()),
            &write_control(),
        )
        .await;

    assert!(matches!(
        result,
        Err(MemoryMutationError::Application(
            MemoryApplicationError::Store(_)
        ))
    ));
    let requests = application.authority.curation_requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    let [ProjectMemoryFactCurationOperationV1::Add(add)] = request.operations() else {
        panic!("curation add must remain an add command");
    };
    let command = add.command();
    assert_eq!(command.owner(), &owner);
    assert_eq!(command.content(), "canonical reviewed memory");
    assert_eq!(command.actor(), outer_context.actor());
    assert_eq!(command.automation_run_id(), Some(run_id.as_str()));
    assert_eq!(
        command.operation_id(),
        &derive_project_memory_fact_curation_child_operation_id(
            request.operation_id(),
            0,
            ProjectMemoryFactCurationMutationKindV1::Add,
        )
        .unwrap()
    );
    assert_eq!(
        add.evidence().facts()[0].fact_id(),
        &fact_id(owner, "operation.curation-add-evidence")
    );
    assert!(request.input_digest().is_ok());
    assert_eq!(
        application
            .authority
            .authority_calls
            .lock()
            .unwrap()
            .as_slice(),
        ["curation"]
    );
}

#[tokio::test]
async fn curation_add_rejects_secret_like_content_before_authority() {
    let owner = owner();
    let application = MemoryApplication::new(owner.clone(), FakeAuthority::default()).unwrap();

    let result = application
        .apply_project_memory_curation(
            vec![add_operation(
                "api_key=sk-canonical-fixture-secret-1234567890abcdef",
                &owner,
            )],
            Confidence::new(0.9).unwrap(),
            context(&owner, "curation-add-secret"),
            Some(RunId::new("run.curation-add-secret").unwrap()),
            &write_control(),
        )
        .await;

    assert!(matches!(
        result,
        Err(MemoryMutationError::Application(
            MemoryApplicationError::InvalidInput {
                invariant: "curation add declined by memory privacy sanitizer",
            }
        ))
    ));
    assert!(
        application
            .authority
            .authority_calls
            .lock()
            .unwrap()
            .is_empty()
    );
    assert!(
        application
            .authority
            .curation_requests
            .lock()
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn curation_add_retry_preserves_digest_and_accepts_normalized_duplicate_replay() {
    let owner = owner();
    let application = MemoryApplication::new(owner.clone(), FakeAuthority::default()).unwrap();
    let outer_context = context(&owner, "curation-add-replay");
    let run_id = RunId::new("run.curation-add-replay").unwrap();
    let operations = vec![add_operation("canonical replayed memory", &owner)];

    let first = application
        .apply_project_memory_curation(
            operations.clone(),
            Confidence::new(0.9).unwrap(),
            outer_context.clone(),
            Some(run_id.clone()),
            &write_control(),
        )
        .await;
    assert!(first.is_err());

    let first_request = application.authority.curation_requests.lock().unwrap()[0].clone();
    let duplicate_fact_id = fact_id(owner.clone(), "operation.curation-add-normalized-duplicate");
    let projection = ProjectMemoryFactProjectionV1::Unavailable(
        ProjectMemoryFactUnavailableV1::new(
            ProjectMemoryFactStatusV1::new(
                owner.clone(),
                duplicate_fact_id.clone(),
                PayloadAccessState::Deleted,
                UtcMicros(1),
            )
            .unwrap(),
        )
        .unwrap(),
    );
    let duplicate = ProjectMemoryFactAddOutcomeV1::normalized_duplicate(
        projection,
        ProjectMemoryFactIdV1::new(owner.clone(), duplicate_fact_id).unwrap(),
    )
    .unwrap();
    let receipt = ProjectMemoryFactCurationReceiptV1::new(
        owner,
        first_request.operation_id().clone(),
        first_request.input_digest().unwrap(),
        Some(run_id.clone()),
        vec![ProjectMemoryFactCurationOperationEffectV1::add(&duplicate).unwrap()],
        vec![],
    )
    .unwrap()
    .into_replayed();
    *application.authority.curation_receipt.lock().unwrap() = Some(receipt.clone());

    let replayed = application
        .apply_project_memory_curation(
            operations,
            Confidence::new(0.9).unwrap(),
            outer_context,
            Some(run_id),
            &write_control(),
        )
        .await
        .unwrap();

    assert_eq!(replayed, receipt);
    assert!(replayed.replayed());
    assert_eq!(replayed.accepted_operations(), 1);
    assert_eq!(replayed.facts_added(), 0);
    let requests = application.authority.curation_requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[0].input_digest().unwrap(),
        requests[1].input_digest().unwrap()
    );
    assert_eq!(
        requests[0].operations()[0].child_operation_id(),
        requests[1].operations()[0].child_operation_id()
    );
}

#[tokio::test]
async fn curation_add_rejects_a_committed_same_owner_fact_from_another_child_identity() {
    let owner = owner();
    let application = MemoryApplication::new(owner.clone(), FakeAuthority::default()).unwrap();
    let outer_context = context(&owner, "curation-add-wrong-commit");
    let first = application
        .apply_project_memory_curation(
            vec![add_operation("canonical committed memory", &owner)],
            Confidence::new(0.9).unwrap(),
            outer_context,
            None,
            &write_control(),
        )
        .await;
    assert!(first.is_err());
    let request = application.authority.curation_requests.lock().unwrap()[0].clone();
    let wrong_fact_id = fact_id(owner.clone(), "operation.curation-add-wrong-commit");
    let projection = ProjectMemoryFactProjectionV1::Unavailable(
        ProjectMemoryFactUnavailableV1::new(
            ProjectMemoryFactStatusV1::new(
                owner.clone(),
                wrong_fact_id.clone(),
                PayloadAccessState::Deleted,
                UtcMicros(2),
            )
            .unwrap(),
        )
        .unwrap(),
    );
    let event_id = FactEventId::new("event.curation-add-wrong-commit".to_owned()).unwrap();
    let commit = tracedecay_store::FactCommitReceipt::new(
        wrong_fact_id,
        owner.clone(),
        vec![event_id.clone()],
        event_id,
        None,
    )
    .unwrap();
    let outcome = ProjectMemoryFactAddOutcomeV1::added(projection, commit, false).unwrap();
    let receipt = ProjectMemoryFactCurationReceiptV1::new(
        owner,
        request.operation_id().clone(),
        request.input_digest().unwrap(),
        None,
        vec![ProjectMemoryFactCurationOperationEffectV1::add(&outcome).unwrap()],
        vec![
            ProjectMemoryFactIdV1::new(
                outcome.fact().owner().clone(),
                outcome.fact().fact_id().clone(),
            )
            .unwrap(),
        ],
    )
    .unwrap();
    *application.authority.curation_receipt.lock().unwrap() = Some(receipt.clone());

    let error = application
        .dashboard_curation(request, &write_control())
        .await
        .unwrap_err();

    let MemoryMutationError::InvalidAuthorityResult {
        authority_result, ..
    } = error
    else {
        panic!("wrong committed add identity must retain its authority receipt");
    };
    assert_eq!(authority_result, receipt);
}

#[test]
fn update_command_binds_the_canonical_patch_to_owner_snapshot_and_context() {
    let owner = owner();
    let application = MemoryApplication::new(owner.clone(), FakeAuthority::default()).unwrap();
    let target = mutation_target(
        owner.clone(),
        "operation.curation-update-target",
        "event.curation-update-target",
    );
    let expected_fact_id = target.fact_id().clone();
    let expected_event_id = target.expected_last_event_id().unwrap().clone();
    let context = context(&owner, "update");
    let patch = ProjectMemoryFactUpdatePatchV1::new(
        Some("canonical updated memory".to_owned()),
        None,
        Some(Some("curator".to_owned())),
        Some(vec!["canonical".to_owned()]),
        None,
        Some(json!({"reviewed": true})),
        Some(Confidence::new(0.91).unwrap()),
    )
    .unwrap();

    let command = application
        .canonical_fact_update_command(target, patch.clone(), &context)
        .unwrap();

    assert_eq!(command.target().owner(), &owner);
    assert_eq!(command.target().fact_id(), &expected_fact_id);
    assert_eq!(command.expected_last_event_id(), Some(&expected_event_id));
    assert_eq!(command.patch(), &patch);
    assert_eq!(command.operation_id(), context.operation_id());
    assert_eq!(command.actor(), context.actor());
    assert!(
        application
            .authority
            .authority_calls
            .lock()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn mutation_construction_rejects_a_fact_from_another_owner() {
    let owner = owner();
    let application = MemoryApplication::new(owner.clone(), FakeAuthority::default()).unwrap();
    let foreign = mutation_target(
        FactOwnerV1::Profile,
        "operation.foreign-curation-target",
        "event.foreign-curation-target",
    );

    let error = application
        .canonical_fact_remove_command(foreign, &context(&owner, "remove"))
        .unwrap_err();

    assert!(matches!(error, MemoryApplicationError::Store(_)));
    assert!(
        application
            .authority
            .authority_calls
            .lock()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn merge_command_preserves_each_snapshot_and_sanitizes_content_before_authority() {
    let owner = owner();
    let application = MemoryApplication::new(owner.clone(), FakeAuthority::default()).unwrap();
    let winner = mutation_target(
        owner.clone(),
        "operation.curation-merge-winner",
        "event.curation-merge-winner",
    );
    let loser = mutation_target(
        owner.clone(),
        "operation.curation-merge-loser",
        "event.curation-merge-loser",
    );
    let winner_event = winner.expected_last_event_id().unwrap().clone();
    let loser_event = loser.expected_last_event_id().unwrap().clone();
    let context = context(&owner, "merge");

    let command = application
        .canonical_fact_merge_command(
            winner,
            vec![loser],
            Some("canonical merged memory".to_owned()),
            &context,
        )
        .unwrap();

    assert_eq!(
        command.winner_target().expected_last_event_id(),
        &winner_event
    );
    assert_eq!(command.loser_targets().len(), 1);
    assert_eq!(
        command.loser_targets()[0].expected_last_event_id(),
        &loser_event
    );
    assert_eq!(command.merged_content(), Some("canonical merged memory"));
    assert_eq!(command.operation_id(), context.operation_id());
    assert_eq!(command.actor(), context.actor());
    assert!(
        application
            .authority
            .authority_calls
            .lock()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn merge_command_rejects_secret_like_content_without_constructing_a_store_command() {
    let owner = owner();
    let application = MemoryApplication::new(owner.clone(), FakeAuthority::default()).unwrap();

    let error = application
        .canonical_fact_merge_command(
            mutation_target(
                owner.clone(),
                "operation.secret-merge-winner",
                "event.secret-merge-winner",
            ),
            vec![mutation_target(
                owner.clone(),
                "operation.secret-merge-loser",
                "event.secret-merge-loser",
            )],
            Some("api_key=sk-canonical-fixture-secret-1234567890abcdef".to_owned()),
            &context(&owner, "merge"),
        )
        .unwrap_err();

    assert!(matches!(
        error,
        MemoryApplicationError::InvalidInput {
            invariant: "canonical merge content rejected by privacy sanitizer",
        }
    ));
    assert!(
        application
            .authority
            .authority_calls
            .lock()
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn mixed_curation_builds_one_owner_bound_cas_batch_without_partial_writes() {
    let owner = owner();
    let application = MemoryApplication::new(owner.clone(), FakeAuthority::default()).unwrap();
    let update_fact = fact_id(owner.clone(), "operation.curation-batch-update");
    let merge_winner = fact_id(owner.clone(), "operation.curation-batch-merge-winner");
    let merge_loser = fact_id(owner.clone(), "operation.curation-batch-merge-loser");
    let remove_fact = fact_id(owner.clone(), "operation.curation-batch-remove");
    let evidence_a = fact_id(owner.clone(), "operation.curation-batch-evidence-a");
    let evidence_b = fact_id(owner.clone(), "operation.curation-batch-evidence-b");
    let outer_context = context(&owner, "curation");
    let operations = vec![
        ProjectMemoryCurationOperation::Add {
            request: add_request("canonical memory admitted with destructive review"),
            evidence_facts: vec![ProjectMemoryCurationMutationTarget::new(
                evidence_a.clone(),
                FactEventId::new("event.curation-batch-evidence-a".to_owned()).unwrap(),
            )],
            confidence: Confidence::new(0.94).unwrap(),
            reason: "reviewed add evidence".to_owned(),
        },
        ProjectMemoryCurationOperation::Update {
            target: ProjectMemoryCurationMutationTarget::new(
                update_fact.clone(),
                FactEventId::new("event.curation-batch-update".to_owned()).unwrap(),
            ),
            patch: ProjectMemoryFactUpdatePatchV1::new(
                None,
                None,
                None,
                Some(vec!["reviewed".to_owned()]),
                None,
                None,
                None,
            )
            .unwrap(),
            evidence_facts: vec![
                ProjectMemoryCurationMutationTarget::new(
                    evidence_b.clone(),
                    FactEventId::new("event.curation-batch-evidence-b".to_owned()).unwrap(),
                ),
                ProjectMemoryCurationMutationTarget::new(
                    evidence_a.clone(),
                    FactEventId::new("event.curation-batch-evidence-a".to_owned()).unwrap(),
                ),
            ],
            confidence: Confidence::new(0.93).unwrap(),
            reason: "reviewed update evidence".to_owned(),
        },
        ProjectMemoryCurationOperation::Merge {
            winner: ProjectMemoryCurationMutationTarget::new(
                merge_winner.clone(),
                FactEventId::new("event.curation-batch-merge-winner".to_owned()).unwrap(),
            ),
            losers: vec![ProjectMemoryCurationMutationTarget::new(
                merge_loser.clone(),
                FactEventId::new("event.curation-batch-merge-loser".to_owned()).unwrap(),
            )],
            merged_content: Some("merged after evidence review".to_owned()),
            evidence_facts: vec![ProjectMemoryCurationMutationTarget::new(
                evidence_a.clone(),
                FactEventId::new("event.curation-batch-evidence-a".to_owned()).unwrap(),
            )],
            confidence: Confidence::new(0.96).unwrap(),
            reason: "reviewed merge evidence".to_owned(),
        },
        ProjectMemoryCurationOperation::Remove {
            target: ProjectMemoryCurationMutationTarget::new(
                remove_fact.clone(),
                FactEventId::new("event.curation-batch-remove".to_owned()).unwrap(),
            ),
            evidence_facts: vec![ProjectMemoryCurationMutationTarget::new(
                evidence_b.clone(),
                FactEventId::new("event.curation-batch-evidence-b".to_owned()).unwrap(),
            )],
            confidence: Confidence::new(0.99).unwrap(),
            reason: "reviewed removal evidence".to_owned(),
        },
    ];

    let result = application
        .apply_project_memory_curation(
            operations,
            Confidence::new(0.9).unwrap(),
            outer_context.clone(),
            Some(RunId::new("run.curation-mutations").unwrap()),
            &write_control(),
        )
        .await;

    assert!(matches!(
        result,
        Err(MemoryMutationError::Application(
            MemoryApplicationError::Store(_)
        ))
    ));
    let requests = application.authority.curation_requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(request.owner(), &owner);
    assert_eq!(request.actor(), outer_context.actor());
    assert_eq!(
        request
            .automation_run_id()
            .map(tracedecay_domain::RunId::as_str),
        Some("run.curation-mutations")
    );
    let [
        ProjectMemoryFactCurationOperationV1::Add(add),
        ProjectMemoryFactCurationOperationV1::Update(update),
        ProjectMemoryFactCurationOperationV1::Merge(merge),
        ProjectMemoryFactCurationOperationV1::Remove(remove),
    ] = request.operations()
    else {
        panic!("curation mutations must preserve request order and types");
    };
    assert_eq!(
        add.command().content(),
        "canonical memory admitted with destructive review"
    );
    assert_eq!(update.command().target().fact_id(), &update_fact);
    assert_eq!(
        update
            .command()
            .expected_last_event_id()
            .map(FactEventId::as_str),
        Some("event.curation-batch-update")
    );
    assert_eq!(merge.command().winner().fact_id(), &merge_winner);
    assert_eq!(
        merge
            .command()
            .winner_target()
            .expected_last_event_id()
            .as_str(),
        "event.curation-batch-merge-winner"
    );
    assert_eq!(
        merge
            .command()
            .loser_facts()
            .map(ProjectMemoryFactIdV1::fact_id)
            .collect::<Vec<_>>(),
        vec![&merge_loser]
    );
    assert_eq!(remove.command().target().fact_id(), &remove_fact);
    assert_eq!(
        remove
            .command()
            .expected_last_event_id()
            .map(FactEventId::as_str),
        Some("event.curation-batch-remove")
    );
    let child_ids = [
        add.command().operation_id(),
        update.command().operation_id(),
        merge.command().operation_id(),
        remove.command().operation_id(),
    ];
    let expected_child_ids = [
        ProjectMemoryFactCurationMutationKindV1::Add,
        ProjectMemoryFactCurationMutationKindV1::Update,
        ProjectMemoryFactCurationMutationKindV1::Merge,
        ProjectMemoryFactCurationMutationKindV1::Remove,
    ]
    .into_iter()
    .enumerate()
    .map(|(index, kind)| {
        derive_project_memory_fact_curation_child_operation_id(request.operation_id(), index, kind)
            .unwrap()
    })
    .collect::<Vec<_>>();
    for (actual, expected) in child_ids.iter().zip(&expected_child_ids) {
        assert_eq!(*actual, expected);
    }
    assert!(child_ids.iter().all(|id| *id != request.operation_id()));
    assert_eq!(
        application
            .authority
            .authority_calls
            .lock()
            .unwrap()
            .as_slice(),
        ["curation"]
    );
    assert!(application.authority.committed.lock().unwrap().is_empty());
    assert_eq!(
        update
            .evidence()
            .facts()
            .iter()
            .map(ProjectMemoryFactIdV1::fact_id)
            .collect::<Vec<_>>(),
        vec![&evidence_a, &evidence_b]
    );
    assert_eq!(
        merge.command().merged_content(),
        Some("merged after evidence review")
    );
    assert_eq!(remove.evidence().reason(), "reviewed removal evidence");
}

#[tokio::test]
async fn destructive_curation_rejects_a_foreign_snapshot_before_authority() {
    let owner = owner();
    let application = MemoryApplication::new(owner.clone(), FakeAuthority::default()).unwrap();
    let target = ProjectMemoryCurationMutationTarget::new(
        fact_id(FactOwnerV1::Profile, "operation.curation-foreign-snapshot"),
        FactEventId::new("event.curation-foreign-snapshot".to_owned()).unwrap(),
    );

    let result = application
        .apply_project_memory_curation(
            vec![ProjectMemoryCurationOperation::Remove {
                target,
                evidence_facts: vec![ProjectMemoryCurationMutationTarget::new(
                    fact_id(
                        owner.clone(),
                        "operation.curation-foreign-snapshot-evidence",
                    ),
                    FactEventId::new("event.curation-foreign-snapshot-evidence".to_owned())
                        .unwrap(),
                )],
                confidence: Confidence::new(1.0).unwrap(),
                reason: "reviewed exact removal evidence".to_owned(),
            }],
            Confidence::new(0.9).unwrap(),
            context(&owner, "curation"),
            Some(tracedecay_domain::RunId::new("run.curation-foreign-snapshot").unwrap()),
            &write_control(),
        )
        .await;

    assert!(matches!(
        result,
        Err(MemoryMutationError::Application(
            MemoryApplicationError::Store(_)
        ))
    ));
    assert!(
        application
            .authority
            .authority_calls
            .lock()
            .unwrap()
            .is_empty()
    );
    assert!(
        application
            .authority
            .curation_requests
            .lock()
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn remove_receipt_target_mismatch_retains_the_settled_authority_receipt() {
    let owner = owner();
    let application = MemoryApplication::new(owner.clone(), FakeAuthority::default()).unwrap();
    let outer_context = context(&owner, "curation-receipt");
    let target = mutation_target(
        owner.clone(),
        "operation.curation-receipt-requested",
        "event.curation-receipt-requested",
    );
    let command = tracedecay_store::ProjectMemoryFactRemoveCommandV1::new(
        ProjectMemoryFactIdV1::new(owner.clone(), target.fact_id().clone()).unwrap(),
        derive_project_memory_fact_curation_child_operation_id(
            outer_context.operation_id(),
            0,
            ProjectMemoryFactCurationMutationKindV1::Remove,
        )
        .unwrap(),
        target.expected_last_event_id().cloned(),
        outer_context.actor().cloned(),
    )
    .unwrap();
    let evidence = ProjectMemoryFactCurationEvidenceV1::new(
        &owner,
        vec![
            ProjectMemoryFactIdV1::new(
                owner.clone(),
                fact_id(owner.clone(), "operation.curation-receipt-evidence"),
            )
            .unwrap(),
        ],
        Confidence::new(1.0).unwrap(),
        "reviewed removal evidence".to_owned(),
    )
    .unwrap();
    let request = ProjectMemoryFactCurationBatchV1::new(
        owner.clone(),
        outer_context.operation_id().clone(),
        outer_context.actor().cloned(),
        Confidence::new(0.9).unwrap(),
        vec![ProjectMemoryFactCurationOperationV1::Remove(
            ProjectMemoryFactCurationRemoveV1::new(command, evidence).unwrap(),
        )],
    )
    .unwrap();
    let wrong_target = ProjectMemoryFactIdV1::new(
        owner.clone(),
        fact_id(owner.clone(), "operation.curation-receipt-wrong"),
    )
    .unwrap();
    let receipt = ProjectMemoryFactCurationReceiptV1::new(
        owner,
        request.operation_id().clone(),
        request.input_digest().unwrap(),
        None,
        vec![
            ProjectMemoryFactCurationOperationEffectV1::remove(
                wrong_target,
                &ProjectMemoryFactRemoveOutcomeV1::not_found(7),
            )
            .unwrap(),
        ],
        vec![],
    )
    .unwrap();
    *application.authority.curation_receipt.lock().unwrap() = Some(receipt.clone());

    let error = application
        .dashboard_curation(request, &write_control())
        .await
        .unwrap_err();

    let MemoryMutationError::InvalidAuthorityResult {
        authority_result, ..
    } = error
    else {
        panic!("mismatched destructive receipt must retain its authority result");
    };
    assert_eq!(authority_result, receipt);
}
