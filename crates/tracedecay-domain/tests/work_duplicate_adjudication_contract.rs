use tracedecay_domain::{
    ActorId, AttemptId, CoverageStateV1, DuplicateEffectOutcomeV1, DuplicateEffortKindV1,
    ManifestDigest, ProjectId, ProjectionGenerationId, QuantityEvidenceClassV1, RepositoryId,
    RunId, TaskId, UtcMicros, WorkAttemptIdentityV1, WorkAuthority, WorkCommandId,
    WorkDuplicateAdjudicationCommandV1, WorkDuplicateAdjudicationEvidenceV1,
    WorkDuplicateAdjudicationQuantitiesV1, WorkDuplicateAdjudicationReceiptV1,
    WorkDuplicateAdjudicationRevisionV1, WorkTopologyGenerationRefV1, WorktreeId,
};

fn id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    T::Error: std::fmt::Debug,
{
    T::try_from(value.to_owned()).unwrap()
}

fn attempt(task: &str, run: &str, attempt: &str) -> WorkAttemptIdentityV1 {
    WorkAttemptIdentityV1::new(
        id::<TaskId>(task),
        id::<RunId>(run),
        id::<AttemptId>(attempt),
    )
    .unwrap()
}

fn topology_ref(byte: char) -> WorkTopologyGenerationRefV1 {
    id(&format!("sha256:{}", byte.to_string().repeat(64)))
}

fn command() -> WorkDuplicateAdjudicationCommandV1 {
    WorkDuplicateAdjudicationCommandV1 {
        expected_revision: None,
        first_attempt: attempt("task.first", "run.first", "attempt.first"),
        second_attempt: attempt("task.second", "run.second", "attempt.second"),
        evidence: WorkDuplicateAdjudicationEvidenceV1 {
            work_generation: id::<ProjectionGenerationId>("generation.work.7"),
            topology_generation: topology_ref('7'),
        },
        verdict: DuplicateEffortKindV1::SupersededOverlap,
        quantities: WorkDuplicateAdjudicationQuantitiesV1 {
            wall_micros: Some(42),
            token_count: Some(7),
            cost_micros: None,
            test_count: Some(2),
            effect_count: None,
            evidence: QuantityEvidenceClassV1::OwnerReceipt,
            effect_outcome: DuplicateEffectOutcomeV1::NotApplicable,
            coverage: CoverageStateV1::Known,
        },
        reason: "independent review proved the second attempt superseded the first".to_owned(),
        command_id: id::<WorkCommandId>("command.duplicate.1"),
        occurred_at: UtcMicros(50),
    }
}

#[test]
fn duplicate_adjudication_binds_distinct_attempts_and_exact_generations() {
    let command = command();
    command.validate().unwrap();

    let mut same_attempt = command.clone();
    same_attempt.second_attempt = same_attempt.first_attempt.clone();
    assert!(same_attempt.validate().is_err());
}

#[test]
fn duplicate_adjudication_accepts_only_generations_with_mounted_authorities() {
    let command = serde_json::to_value(command()).unwrap();
    assert!(
        command.get("adjudication_id").is_none(),
        "the authority-bound attempt pair is the identity; callers must not invent an alias"
    );
    let evidence = command["evidence"].clone();
    assert_eq!(
        evidence
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>(),
        ["topology_generation", "work_generation"],
        "callers cannot fabricate file, symbol, or local-anchor authority"
    );
}

#[test]
fn duplicate_adjudication_receipt_pins_actor_revision_and_input_digest() {
    let receipt_command = command();
    let input_digest = receipt_command.canonical_input_digest().unwrap();
    let authority = WorkAuthority::new(
        id::<ProjectId>("project.duplicate"),
        id::<RepositoryId>("repository.duplicate"),
        id::<WorktreeId>("worktree.duplicate"),
        id::<ActorId>("actor.adjudicator"),
        ManifestDigest::new(format!("sha256:{}", "a".repeat(64))).unwrap(),
    )
    .unwrap();
    let receipt = WorkDuplicateAdjudicationReceiptV1::new(
        &authority,
        receipt_command,
        WorkDuplicateAdjudicationRevisionV1::initial(),
        input_digest.clone(),
    )
    .unwrap();
    assert_eq!(receipt.revision().get(), 1);
    assert_eq!(receipt.actor_id().as_str(), "actor.adjudicator");
    assert_eq!(receipt.canonical_input_digest(), &input_digest);
    let payload = receipt.observability_payload();
    assert_eq!(payload.adjudication_revision, 1);
    assert_eq!(payload.kind, DuplicateEffortKindV1::SupersededOverlap);
    assert_eq!(payload.wall_micros, Some(42));
    assert_eq!(
        payload.local_anchor_refs,
        [payload.adjudication_ref.clone()]
    );
    payload.validate().unwrap();
    let wire = serde_json::to_value(&receipt).unwrap();
    assert_eq!(
        wire["adjudication_ref"],
        receipt.adjudication_ref().as_str(),
        "the public receipt must expose its authority-bound relation identity"
    );
    let other_authority = WorkAuthority::new(
        id::<ProjectId>("project.duplicate.other"),
        id::<RepositoryId>("repository.duplicate"),
        id::<WorktreeId>("worktree.duplicate"),
        id::<ActorId>("actor.adjudicator"),
        ManifestDigest::new(format!("sha256:{}", "a".repeat(64))).unwrap(),
    )
    .unwrap();
    let other_command = command();
    let other_input_digest = other_command.canonical_input_digest().unwrap();
    let other_receipt = WorkDuplicateAdjudicationReceiptV1::new(
        &other_authority,
        other_command,
        WorkDuplicateAdjudicationRevisionV1::initial(),
        other_input_digest,
    )
    .unwrap();
    assert_ne!(
        receipt.adjudication_ref(),
        other_receipt.adjudication_ref(),
        "identical relation text in another Work authority cannot coalesce"
    );

    let command = command();
    assert!(
        WorkDuplicateAdjudicationReceiptV1::new(
            &authority,
            command,
            WorkDuplicateAdjudicationRevisionV1::initial(),
            tracedecay_domain::ManifestDigest::new(format!("sha256:{}", "f".repeat(64))).unwrap(),
        )
        .is_err(),
        "a syntactically valid but non-canonical digest is not a durable receipt"
    );
}

#[test]
fn duplicate_adjudication_rejects_unknown_quantity_evidence() {
    let mut command = command();
    command.quantities.evidence = QuantityEvidenceClassV1::Unknown;
    assert!(command.validate().is_err());
}

#[test]
fn duplicate_adjudication_does_not_turn_unknown_or_censored_evidence_into_a_verdict() {
    let mut unknown = command();
    unknown.verdict = DuplicateEffortKindV1::Unknown;
    assert!(unknown.validate().is_err());
    unknown.quantities.coverage = CoverageStateV1::Unknown;
    assert!(unknown.validate().is_ok());

    let mut censored = command();
    censored.verdict = DuplicateEffortKindV1::Censored;
    assert!(censored.validate().is_err());
    censored.quantities.coverage = CoverageStateV1::Partial;
    assert!(censored.validate().is_ok());
}
