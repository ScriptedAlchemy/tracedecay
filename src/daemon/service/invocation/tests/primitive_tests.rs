//! `primitive` module test coverage (split from the former monolithic
//! `invocation::tests` module).

use super::*;

#[tokio::test]
async fn context_scout_registry_remounts_same_project_database_after_daemon_restart() {
    let temporary = tempfile::tempdir().unwrap();
    let database_path = temporary.path().join("graph.db");
    crate::daemon::store_runtime::register_registered_schema_installer();
    let authority =
        crate::db::DatabaseAuthority::acquire_test(&database_path, "daemon Context Scout registry")
            .unwrap();
    let database = Database::publish_test_runtime(
        &database_path,
        &authority,
        crate::db::TestDatabaseRuntimeMode::Initialize,
    )
    .await
    .unwrap()
    .0;
    let project_id = ProjectId::new("project.scout.daemon-restart").unwrap();

    let first_service = DaemonInvocationService::default();
    let first_registrar = DaemonContextScoutRuntimeRegistrar::new(&first_service);
    let first = first_registrar
        .open_and_register(database.clone(), project_id.clone())
        .await
        .unwrap();
    assert!(Arc::ptr_eq(
        &first,
        &first_registrar.get(&project_id).await.unwrap()
    ));
    first_service.expire_all().await;
    assert!(first_registrar.get(&project_id).await.is_none());

    let restarted_service = DaemonInvocationService::default();
    let restarted_registrar = DaemonContextScoutRuntimeRegistrar::new(&restarted_service);
    let restarted = restarted_registrar
        .open_and_register(database.clone(), project_id.clone())
        .await
        .unwrap();
    assert!(!Arc::ptr_eq(&first, &restarted));
    assert!(Arc::ptr_eq(
        &restarted,
        &restarted_registrar.get(&project_id).await.unwrap()
    ));
    assert!(matches!(
        restarted_registrar
            .open_and_register(database, project_id)
            .await,
        Err(DaemonContextScoutRuntimeRegistrationError::AlreadyRegistered)
    ));
}

#[test]
fn callable_code_outcome_is_distinct_and_context_grant_is_exact() {
    let observed_at = current_micros();
    let completed_at = UtcMicros(
        observed_at
            .0
            .checked_add(1)
            .expect("fixture completion timestamp"),
    );
    let deadline = Deadline::new(UtcMicros(
        observed_at
            .0
            .checked_add(60_000_000)
            .expect("fixture deadline"),
    ))
    .expect("deadline");
    let operation = callable_code_operations()
        .expect("operations")
        .get(CallableCodeOperationKind::ExactOccurrence)
        .clone();
    let scope = ResolvedScope::new(
        ProjectId::new("project.callable-code").expect("project"),
        tracedecay_domain::RepositoryId::new("repository.callable-code").expect("repository"),
        tracedecay_domain::WorktreeId::new("worktree.callable-code").expect("worktree"),
        None,
    )
    .expect("scope");
    let access = ProjectSourceAccessSnapshot {
        scope: scope.clone(),
        requester: ActorId::new("actor.callable-code").expect("actor"),
        binding: tracedecay_domain::configuration::ScopeSourceBinding::new(
            tracedecay_domain::SourceBindingId::new("binding.callable-code").expect("binding"),
            tracedecay_domain::configuration::SourceKindV1::Cursor,
            tracedecay_domain::LocatorDigest::new(format!("sha256:{}", "a".repeat(64)))
                .expect("locator"),
            tracedecay_domain::configuration::AuthorityRef::Project(scope.project_id.clone()),
        )
        .expect("source binding"),
        configuration_revision: ConfigurationRevisionId::new("revision.callable-code")
            .expect("configuration revision"),
        configuration_digest: canonical_sha256(&"callable-code-configuration")
            .expect("configuration digest"),
        configuration_provenance_digest: canonical_sha256(
            &"callable-code-configuration-provenance",
        )
        .expect("configuration provenance"),
        effective_capabilities: [operation.capability_id().clone()].into_iter().collect(),
        grant_expires_at: deadline.expires_at,
    };
    let expired = callable_code_request_context(
        &scope,
        &access,
        "request.callable-code.expired",
        &operation,
        UtcMicros(1),
        Deadline::new(UtcMicros(observed_at.0.saturating_sub(1))).expect("expired deadline"),
        CancellationContext::active("cancel.callable-code.expired").expect("cancellation"),
    )
    .expect_err("wall-clock-expired deadline must fail despite a stale caller timestamp");
    assert_eq!(expired.kind(), ApplicationProblemKind::TimedOut);
    let context = callable_code_request_context(
        &scope,
        &access,
        "request.callable-code",
        &operation,
        observed_at,
        deadline.clone(),
        CancellationContext::active("cancel.callable-code").expect("cancellation"),
    )
    .expect("context");
    assert_eq!(context.scope(), &scope);
    assert_eq!(
        context.grant().allowed_capabilities,
        [operation.capability_id().clone()].into_iter().collect()
    );

    let authority = AuthorityReceipt::from_context(
        &context,
        PolicyDecisionRef::new(
            "policy.callable-code.fixture",
            1,
            canonical_sha256(&"callable-code-policy").expect("policy digest"),
            ComponentVersion::new("callable-code-policy.v1").expect("policy component"),
        )
        .expect("policy"),
        completed_at,
    )
    .expect("authority");
    let result = DaemonFeedbackResult::from_application(EvidencePacket {
        temporal: TemporalState::current(completed_at),
        authority,
        evidence_authorities: Vec::new(),
        coverage: EvidenceCoverage::complete(vec![EvidenceDomain::Symbol], 0, 0, 0)
            .expect("coverage"),
        omissions: Vec::new(),
        scores: Vec::new(),
        contributions: Vec::new(),
        page: PageState::first_page(
            SortContractId::new("sort.callable-code.fixture").expect("sort"),
            1,
            Some(0),
            0,
        )
        .expect("page"),
        execution: OperationReceipt::completed(
            observed_at,
            completed_at,
            deadline,
            OperationBudgetUsage::default(),
        )
        .expect("execution"),
        payload: Some(serde_json::json!({"generation": "generation.callable-code"})),
    });
    let outcome = DaemonInvocationOutcome::CallableCode { scope, result };
    let encoded = serde_json::to_value(&outcome).expect("encode outcome");
    assert_eq!(encoded["status"], "callable_code");
    assert!(matches!(
        serde_json::from_value(encoded).expect("decode outcome"),
        DaemonInvocationOutcome::CallableCode { .. }
    ));
}
