//! Strict PR13 runtime acceptance over authentic provider response captures.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use tracedecay::application::advisory::ci_runtime::GitHubCiOfficialResponseDecoderV1;
use tracedecay::application::advisory::github_runtime::{
    GitHubReviewAtomicRefreshStoreV1, GitHubReviewRefreshCoordinatorV1,
    GitHubReviewRefreshOutcomeV1, GitHubReviewRefreshStateV1,
    GitHubReviewRefreshStoreCommitOutcomeV1, GitHubReviewRefreshStoreReadOutcomeV1,
};
use tracedecay::application::advisory::{
    CanonicalProximityEvidenceAuthorityV1, CiFailureLocalizationAdapter, CiReadOnlyEvidenceSource,
    GitHubCanonicalReviewAnchorAuthorityV1, GitHubCanonicalReviewAnchorsV1, GitHubHttpReadConfigV1,
    GitHubOfficialResponseDecoderV1, GitHubReadNetworkMetadataV1, GitHubReadNetworkStatusV1,
    GitHubReadOnlyCredentialV1, GitHubReadResponseDecoderV1, GitHubRepositoryTargetV1,
    GitHubReviewAnchorSeedV1, GitHubReviewProviderIdentityV1, Pr13ProximityRuntimeOutcomeV1,
    Pr13ProximityRuntimeOwnerV1, production_proximity_evidence_authority_v1,
};
use tracedecay::application::configuration::{
    AuthorizedActor, ComponentConfigurationState, ConfigurationAuditPage, ConfigurationAuditQuery,
    ConfigurationControlStore, ConfigurationCurrentStateV1, ConfigurationError,
    ConfigurationMutationAuthority, ConfigurationMutationReceipt, ConfigurationOperationFuture,
    ConfigurationRollbackRequest, DirectConfigurationMutation, ScopeRevalidationEvidenceV1,
};
use tracedecay::application::host_admission::{
    HostAdmissionAuthorities, HostAdmissionFacade, HostAdmissionStatus,
};
use tracedecay::application::observation::{CaptureObservationRequest, ObservationCancellation};
use tracedecay::privacy::{ClaudeRecordParseErrorV1, parse_normalized_observation_record_v1};
use tracedecay::sessions::git_correlation::{SpanObservation, SpanSource};
use tracedecay::sessions::{SessionMessageRecord, SessionRecord};
use tracedecay::tracedecay::TraceDecay;
use tracedecay_application::feedback::{
    CiFailureLocalizationPort, CiFailureLocalizationPortOutcomeV1, CiFailureLocalizationRequestV1,
    FeedbackPortFuture, GitHubReviewReadPort, GitHubReviewReadPortOutcomeV1,
    GitHubReviewReadRequestV1, ProximityEvaluationRequestV1,
};
use tracedecay_application::{
    CancellationContext, CapabilityGrantId, CapabilityGrantSnapshot, Deadline, DisclosureClass,
    RequestContext, RequestId, ResolvedScope,
};
use tracedecay_domain::configuration::{
    CandidateDispositionV1, ChangePlanId, ConfigurationCandidateV1, ConfigurationLayerIdV1,
    ConfigurationRevisionId, ConfigurationSnapshotV1, ConfigurationValueV1, ProtectedApplyRequest,
    ProtectedChange, ProtectedChangePlan, SettingKey,
};
use tracedecay_domain::feedback::{
    FeedbackScopeV1, GitHubPullRequestIdV1, GitHubReviewReadOperationV1,
    PROXIMITY_RISK_THRESHOLD_SETTING_KEY_V1, ProximityInclusionV1, ProximityTierV1,
    ProximityWarningClassV1,
};
use tracedecay_domain::{
    ActorId, CanonicalMessageRoleV1, CanonicalObservationEnvelopeV1,
    CanonicalObservationEvidenceV1, CanonicalObservationFactV1, CanonicalObservationRelationsV1,
    CommitId, ManifestDigest, ObservationId, ObservationIdentityMaterialV1,
    ObservationOrderingDomainV1, ObservationScopeV1, ObservationSourceGenerationV1,
    ObservationSourceIdentityV1, ObservationSourceRangeV1, ProjectId, ProviderId, RefId,
    RepositoryId, RetentionClass, SessionId, UtcMicros, WorktreeId,
};
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

mod common;

struct NoAnchors;

impl GitHubCanonicalReviewAnchorAuthorityV1 for NoAnchors {
    fn resolve<'a>(
        &'a self,
        _request: &'a GitHubReviewReadRequestV1,
        _seed: &'a GitHubReviewAnchorSeedV1,
    ) -> FeedbackPortFuture<'a, Option<GitHubCanonicalReviewAnchorsV1>> {
        Box::pin(async { None })
    }
}

struct PanicCiSource;

impl CiReadOnlyEvidenceSource for PanicCiSource {
    fn read_localization<'a>(
        &'a self,
        _context: &'a RequestContext,
        _request: &'a CiFailureLocalizationRequestV1,
    ) -> FeedbackPortFuture<'a, CiFailureLocalizationPortOutcomeV1> {
        Box::pin(async { panic!("denied CI request reached provider source") })
    }
}

struct PanicGitHubPort;

impl GitHubReviewReadPort for PanicGitHubPort {
    fn read<'a>(
        &'a self,
        _context: &'a RequestContext,
        _request: &'a GitHubReviewReadRequestV1,
    ) -> FeedbackPortFuture<'a, GitHubReviewReadPortOutcomeV1> {
        Box::pin(async { panic!("denied GitHub request reached provider port") })
    }
}

struct PanicGitHubStore;

impl GitHubReviewAtomicRefreshStoreV1 for PanicGitHubStore {
    fn load<'a>(
        &'a self,
        _context: &'a RequestContext,
        _request: &'a GitHubReviewReadRequestV1,
    ) -> FeedbackPortFuture<'a, GitHubReviewRefreshStoreReadOutcomeV1> {
        Box::pin(async { panic!("denied GitHub request reached refresh store") })
    }

    fn compare_and_record<'a>(
        &'a self,
        _context: &'a RequestContext,
        _request: &'a GitHubReviewReadRequestV1,
        _expected_revision: Option<&'a ManifestDigest>,
        _next: &'a GitHubReviewRefreshStateV1,
    ) -> FeedbackPortFuture<'a, GitHubReviewRefreshStoreCommitOutcomeV1> {
        Box::pin(async { panic!("denied GitHub request reached refresh store") })
    }
}

fn captured_response(source: &str) -> Value {
    serde_json::from_str::<Value>(source).expect("capture parses")["response"].clone()
}

fn scope() -> FeedbackScopeV1 {
    FeedbackScopeV1 {
        project_id: ProjectId::new("project.pr13.runtime.capture").unwrap(),
        repository_id: RepositoryId::new("repository.pr13.runtime.capture").unwrap(),
        worktree_id: WorktreeId::new("worktree.pr13.runtime.capture").unwrap(),
        branch_ref: "refs/heads/codex/tracedecay-total-redesign-plan".to_owned(),
        head_commit_id: CommitId::new("e29900448db98ae58e90d08770a3bb8bfa710846").unwrap(),
    }
}

#[test]
fn github_source_access_uses_owner_bound_ureq_dtos() {
    let credential = GitHubReadOnlyCredentialV1::from_declared_scopes(
        "fixture-access".to_owned(),
        ["pull_requests:read".to_owned()],
    )
    .expect("read-only source credential");
    let target = GitHubRepositoryTargetV1 {
        owner: "ScriptedAlchemy".to_owned(),
        repository: "tracedecay".to_owned(),
        pull_request_number: 421,
        pull_request_id: GitHubPullRequestIdV1::new("4026204542").unwrap(),
    };
    assert!(target.validate());
    let _owner_inputs = (credential, target, GitHubHttpReadConfigV1::default());
}

#[tokio::test]
async fn authentic_github_and_ci_responses_use_production_decoders() {
    let pull_request = captured_response(include_str!(
        "../src/application/advisory/fixtures/pr13_branch_pr/pull_request.json"
    ));
    let request = GitHubReviewReadRequestV1 {
        operation: GitHubReviewReadOperationV1::RestGetPullRequest,
        scope: scope(),
        pull_request_id: GitHubPullRequestIdV1::new("4026204542").unwrap(),
    };
    let decoder = GitHubOfficialResponseDecoderV1::new(
        GitHubReviewProviderIdentityV1 {
            provider: ProviderId::new("provider.github").unwrap(),
            base_commit_id: CommitId::new("8339371d01311289e2b7cd7b0669ea3549308b8c").unwrap(),
            head_commit_id: request.scope.head_commit_id.clone(),
            merge_base_commit_id: CommitId::new("8339371d01311289e2b7cd7b0669ea3549308b8c")
                .unwrap(),
        },
        NoAnchors,
    )
    .unwrap();
    let metadata = GitHubReadNetworkMetadataV1 {
        status: GitHubReadNetworkStatusV1::Ok,
        etag: None,
        next_cursor: None,
        rate_limit: None,
    };
    assert!(
        decoder
            .decode(
                &request,
                &metadata,
                serde_json::to_vec(&pull_request).unwrap().as_slice(),
            )
            .await
            .is_some()
    );
    let review_request = GitHubReviewReadRequestV1 {
        operation: GitHubReviewReadOperationV1::RestListPullRequestReviews,
        ..request.clone()
    };
    let review = captured_response(include_str!(
        "../src/application/advisory/fixtures/pr13_branch_pr/review.json"
    ));
    assert!(
        decoder
            .decode(
                &review_request,
                &metadata,
                serde_json::to_vec(&vec![review]).unwrap().as_slice(),
            )
            .await
            .is_some()
    );

    let ci = GitHubCiOfficialResponseDecoderV1::decode(
        include_str!("../src/application/advisory/fixtures/pr13_branch_pr/workflow_run.json"),
        include_str!("../src/application/advisory/fixtures/pr13_branch_pr/workflow_job.json"),
        include_str!("../src/application/advisory/fixtures/pr13_branch_pr/check_run.json"),
        include_str!("../src/application/advisory/fixtures/pr13_branch_pr/check_annotations.json"),
    )
    .expect("authentic CI responses decode");
    assert!(ci.failed_step().is_some());
    assert!(ci.failed_annotation().is_some());
}

#[tokio::test]
async fn corrupt_provider_identity_fails_production_decoder() {
    let mut pull_request = captured_response(include_str!(
        "../src/application/advisory/fixtures/pr13_branch_pr/pull_request.json"
    ));
    pull_request["id"] = json!(0);
    let request = GitHubReviewReadRequestV1 {
        operation: GitHubReviewReadOperationV1::RestGetPullRequest,
        scope: scope(),
        pull_request_id: GitHubPullRequestIdV1::new("4026204542").unwrap(),
    };
    let decoder = GitHubOfficialResponseDecoderV1::new(
        GitHubReviewProviderIdentityV1 {
            provider: ProviderId::new("provider.github").unwrap(),
            base_commit_id: CommitId::new("8339371d01311289e2b7cd7b0669ea3549308b8c").unwrap(),
            head_commit_id: request.scope.head_commit_id.clone(),
            merge_base_commit_id: CommitId::new("8339371d01311289e2b7cd7b0669ea3549308b8c")
                .unwrap(),
        },
        NoAnchors,
    )
    .unwrap();
    assert!(
        decoder
            .decode(
                &request,
                &GitHubReadNetworkMetadataV1 {
                    status: GitHubReadNetworkStatusV1::Ok,
                    etag: None,
                    next_cursor: None,
                    rate_limit: None,
                },
                serde_json::to_vec(&pull_request).unwrap().as_slice(),
            )
            .await
            .is_none()
    );
}

struct ProximityConfiguration {
    current: ConfigurationCurrentStateV1,
}

impl ConfigurationControlStore for ProximityConfiguration {
    fn current(&self) -> ConfigurationOperationFuture<'_, ConfigurationCurrentStateV1> {
        let current = self.current.clone();
        Box::pin(async move { Ok(current) })
    }

    fn save_plan(
        &self,
        _plan: &ProtectedChangePlan,
        _operation: &ProtectedChange,
    ) -> ConfigurationOperationFuture<'_, ()> {
        Box::pin(async { Err(ConfigurationError::Unavailable) })
    }

    fn load_plan(
        &self,
        _plan_id: &ChangePlanId,
    ) -> ConfigurationOperationFuture<'_, Option<ProtectedChangePlan>> {
        Box::pin(async { Err(ConfigurationError::Unavailable) })
    }

    fn commit_direct(
        &self,
        _authority: &ConfigurationMutationAuthority,
        _mutation: &DirectConfigurationMutation,
        _expected_revision: &ConfigurationRevisionId,
    ) -> ConfigurationOperationFuture<'_, ConfigurationMutationReceipt> {
        Box::pin(async { Err(ConfigurationError::Unavailable) })
    }

    fn commit_protected(
        &self,
        _authority: &ConfigurationMutationAuthority,
        _request: &ProtectedApplyRequest,
        _plan: &ProtectedChangePlan,
        _evidence: &ScopeRevalidationEvidenceV1,
    ) -> ConfigurationOperationFuture<'_, ConfigurationMutationReceipt> {
        Box::pin(async { Err(ConfigurationError::Unavailable) })
    }

    fn dry_run_rollback(
        &self,
        _authority: &ConfigurationMutationAuthority,
        _rollback: &ConfigurationRollbackRequest,
        _now: UtcMicros,
    ) -> ConfigurationOperationFuture<'_, ProtectedChangePlan> {
        Box::pin(async { Err(ConfigurationError::Unavailable) })
    }

    fn apply_rollback(
        &self,
        _authority: &ConfigurationMutationAuthority,
        _request: &ProtectedApplyRequest,
        _plan: &ProtectedChangePlan,
        _evidence: &ScopeRevalidationEvidenceV1,
    ) -> ConfigurationOperationFuture<'_, ConfigurationMutationReceipt> {
        Box::pin(async { Err(ConfigurationError::Unavailable) })
    }

    fn audit(
        &self,
        _actor: &AuthorizedActor,
        _query: &ConfigurationAuditQuery,
    ) -> ConfigurationOperationFuture<'_, ConfigurationAuditPage> {
        Box::pin(async { Err(ConfigurationError::Unavailable) })
    }

    fn observed_state(
        &self,
        _actor: &AuthorizedActor,
    ) -> ConfigurationOperationFuture<'_, Vec<ComponentConfigurationState>> {
        Box::pin(async { Ok(Vec::new()) })
    }
}

fn now_micros() -> UtcMicros {
    UtcMicros(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_micros()
            .try_into()
            .unwrap(),
    )
}

fn proximity_context(scope: &FeedbackScopeV1, now: UtcMicros) -> RequestContext {
    let resolved = ResolvedScope::new(
        scope.project_id.clone(),
        scope.repository_id.clone(),
        scope.worktree_id.clone(),
        Some(RefId::new(scope.branch_ref.clone()).unwrap()),
    )
    .unwrap();
    let grant = CapabilityGrantSnapshot::new(
        CapabilityGrantId::new("grant.pr13.proximity").unwrap(),
        1,
        ManifestDigest::new(format!("sha256:{}", "a".repeat(64))).unwrap(),
        ActorId::new("actor.pr13.proximity.issuer").unwrap(),
        UtcMicros(now.0.saturating_sub(1_000_000)),
        UtcMicros(now.0.saturating_add(60_000_000)),
        resolved.clone(),
        BTreeSet::from([CapabilityId::new("capability.application.feedback.proximity").unwrap()]),
        BTreeSet::from([UseCaseId::new("use-case.application.feedback.proximity").unwrap()]),
        DisclosureClass::Evidence,
    )
    .unwrap();
    RequestContext::new(
        ActorId::new("actor.pr13.proximity").unwrap(),
        resolved,
        grant,
        RequestId::new("request.pr13.proximity").unwrap(),
        Deadline::new(UtcMicros(now.0.saturating_add(30_000_000))).unwrap(),
        CancellationContext::active("cancel.pr13.proximity").unwrap(),
    )
    .unwrap()
}

#[tokio::test]
async fn unauthorized_ci_request_is_denied_before_provider_read() {
    let fixture =
        tracedecay::application::advisory::fixtures::load_pr13_source_backed_composite_fixture_v1()
            .unwrap();
    let scope = scope();
    let request = CiFailureLocalizationRequestV1 {
        scope: scope.clone(),
        run: fixture.ci.run,
    };
    let context = proximity_context(&scope, now_micros());
    let adapter = CiFailureLocalizationAdapter::new(PanicCiSource);

    assert!(matches!(
        adapter.localize(&context, &request).await,
        CiFailureLocalizationPortOutcomeV1::Denied
    ));
}

#[tokio::test]
async fn unauthorized_github_refresh_is_denied_before_port_or_store_access() {
    let fixture =
        tracedecay::application::advisory::fixtures::load_pr13_source_backed_composite_fixture_v1()
            .unwrap();
    let scope = scope();
    let request = GitHubReviewReadRequestV1 {
        operation: GitHubReviewReadOperationV1::GraphQlQueryPullRequestReviewThreads,
        scope: scope.clone(),
        pull_request_id: fixture.github.pull_request_id,
    };
    let context = proximity_context(&scope, now_micros());
    let coordinator = GitHubReviewRefreshCoordinatorV1::new(PanicGitHubPort, PanicGitHubStore);

    assert_eq!(
        coordinator.refresh(&context, &request).await,
        GitHubReviewRefreshOutcomeV1::Denied
    );
}

fn agent_observation(
    provider: &str,
    session_id: &str,
    agent_id: &str,
    project_id: &ProjectId,
    project_path: &str,
    sequence: u64,
) -> CaptureObservationRequest {
    let provider_id = ProviderId::new(provider).unwrap();
    let session_id = SessionId::new(session_id).unwrap();
    let record_id = ObservationId::new(format!("observation.pr13.proximity.{sequence}")).unwrap();
    let range = ObservationSourceRangeV1::new(sequence, sequence + 1).unwrap();
    let ordering = ObservationOrderingDomainV1::DaemonSequence;
    let relations = CanonicalObservationRelationsV1::new(session_id.clone())
        .with_message_id(record_id.clone())
        .with_agent_id(ObservationId::new(agent_id).unwrap());
    let envelope_provider = provider_id.clone();
    let envelope_record_id = record_id.clone();
    let project_path = project_path.to_owned();
    let parsed = parse_normalized_observation_record_v1(
        br#"{"text":"saved edit accepted"}"#,
        range,
        ordering,
        move |native| {
            CanonicalObservationEnvelopeV1::new(
                envelope_provider,
                "pr13_proximity",
                envelope_record_id,
                relations,
                vec![
                    CanonicalObservationFactV1::Session {
                        project_path: Some(project_path.clone()),
                        location_path: Some(project_path),
                        transcript_path: None,
                        title: Some("PR13 proximity fixture".to_owned()),
                        started_at: None,
                        ended_at: None,
                        source: Some("authentic-host-callback".to_owned()),
                        native_source: Some(provider.to_owned()),
                        profile: None,
                        location_provenance: Some("project-open".to_owned()),
                    },
                    CanonicalObservationFactV1::Message {
                        role: CanonicalMessageRoleV1::Assistant,
                        content: native,
                        model: None,
                        timestamp: None,
                    },
                ],
                CanonicalObservationEvidenceV1::new(ordering, range).with_native_sequence(sequence),
            )
            .map_err(|_| ClaudeRecordParseErrorV1::NormalizationFailed)
        },
    )
    .unwrap();
    CaptureObservationRequest::new(
        parsed,
        ObservationIdentityMaterialV1::for_native_record(
            ObservationSourceIdentityV1::for_provider(provider_id, session_id).unwrap(),
            ObservationScopeV1::Project {
                project_id: project_id.clone(),
            },
            ObservationSourceGenerationV1::new(1).unwrap(),
            range,
            ordering,
            record_id,
        )
        .unwrap(),
        None,
        RetentionClass::new("retention.pr13.proximity").unwrap(),
        ObservationCancellation::default(),
    )
    .unwrap()
}

#[tokio::test]
async fn proximity_file_overlap_and_tiering() {
    let (_environment, project) = common::IsolatedEnv::acquire().await;
    std::fs::create_dir_all(project.join("src")).unwrap();
    std::fs::write(project.join("src/lib.rs"), "pub fn shared_edit() {}\n").unwrap();
    let graph = Arc::new(TraceDecay::init(&project).await.unwrap());
    let session_store_dir = tempfile::tempdir().unwrap();
    let sessions = common::open_lcm_db(&session_store_dir).await;
    let fixture =
        tracedecay::application::advisory::fixtures::load_pr13_source_backed_composite_fixture_v1()
            .unwrap();
    let scope = scope();
    let now = now_micros();
    let now_seconds = now.0.div_euclid(1_000_000);
    let project_path = project.to_string_lossy().into_owned();
    let branch = fixture.proximity.branch.clone();
    let overlap_path = "src/lib.rs";
    let observation_scope = ObservationScopeV1::Project {
        project_id: scope.project_id.clone(),
    };
    let admission = HostAdmissionFacade::new(HostAdmissionAuthorities::for_project(
        &sessions,
        scope.project_id.clone(),
    ));

    for (index, (provider, session_id, agent_id)) in [
        (
            "claude",
            fixture.proximity.source_sessions[0].as_str(),
            "agent.pr13.proximity.claude",
        ),
        (
            "codex",
            fixture.proximity.source_sessions[1].as_str(),
            "agent.pr13.proximity.codex",
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let mut session = SessionRecord {
            provider: provider.to_owned(),
            session_id: session_id.to_owned(),
            project_key: scope.project_id.as_str().to_owned(),
            project_path: project_path.clone(),
            title: Some("PR13 concurrent edit".to_owned()),
            started_at: Some(now_seconds.saturating_sub(30)),
            ended_at: None,
            transcript_path: None,
            metadata_json: None,
            parent_session_id: None,
            is_subagent: false,
            agent_id: Some(agent_id.to_owned()),
            parent_tool_use_id: None,
        };
        assert!(sessions.upsert_session(&session).await);
        session.title = Some(format!("PR13 concurrent edit {index}"));
        let message = SessionMessageRecord {
            provider: provider.to_owned(),
            message_id: format!("message.pr13.proximity.{index}"),
            session_id: session_id.to_owned(),
            role: "assistant".to_owned(),
            timestamp: Some(now_seconds.saturating_sub(index as i64)),
            ordinal: index as i64 + 1,
            text: "saved edit".to_owned(),
            kind: Some("tool_result".to_owned()),
            model: None,
            tool_names: Some("write".to_owned()),
            source_path: None,
            source_offset: None,
            metadata_json: Some(json!({"files": [{"path": overlap_path}]}).to_string()),
        };
        assert!(sessions.upsert_session_message(&message).await);
        sessions
            .git_record_span_observation(
                &SpanObservation {
                    provider: provider.to_owned(),
                    session_id: session_id.to_owned(),
                    thread_id: None,
                    branch: Some(branch.clone()),
                    worktree: project_path.clone(),
                    ts: now_seconds.saturating_sub(index as i64),
                    source: SpanSource::HookRoute,
                },
                60,
            )
            .await
            .unwrap();
        assert_eq!(
            admission
                .capture(agent_observation(
                    provider,
                    session_id,
                    agent_id,
                    &scope.project_id,
                    &project_path,
                    index as u64 + 1,
                ))
                .await
                .status,
            HostAdmissionStatus::Committed
        );
    }
    admission
        .drain_projection_queue(
            "claude",
            &observation_scope,
            &ObservationCancellation::default(),
            8,
        )
        .await
        .unwrap();
    drop(admission);

    let sessions = Arc::new(sessions);
    let evidence = production_proximity_evidence_authority_v1(
        Arc::clone(&sessions),
        graph,
        scope.clone(),
        project,
    )
    .unwrap();
    let request = ProximityEvaluationRequestV1 {
        scope: scope.clone(),
        observed_at: now,
    };
    let context = proximity_context(&scope, now);
    let batch = evidence
        .current_evidence(&context, &request)
        .await
        .expect("production evidence authority");
    assert_eq!(batch.evidence.len(), 1);
    let overlap = &batch.evidence[0];
    assert_eq!(overlap.address.file.as_str(), overlap_path);
    assert_eq!(overlap.warning_class, ProximityWarningClassV1::SameFile);
    assert_eq!(overlap.risk_inputs.overlap_size, 2);
    assert_eq!(overlap.observations.len(), 2);

    let threshold_key = SettingKey::new(PROXIMITY_RISK_THRESHOLD_SETTING_KEY_V1).unwrap();
    let configuration_revision =
        ConfigurationRevisionId::new("configuration.pr13.proximity").unwrap();
    let configuration = ProximityConfiguration {
        current: ConfigurationCurrentStateV1 {
            revision_id: configuration_revision.clone(),
            snapshot: ConfigurationSnapshotV1::new(
                BTreeMap::from([(threshold_key.clone(), ConfigurationValueV1::Unsigned(9_999))]),
                BTreeMap::from([(
                    threshold_key,
                    vec![ConfigurationCandidateV1 {
                        layer: ConfigurationLayerIdV1::Project {
                            project_id: scope.project_id.clone(),
                        },
                        revision_id: configuration_revision,
                        disposition: CandidateDispositionV1::Winning,
                        safe_reason: None,
                    }],
                )]),
            )
            .unwrap(),
        },
    };
    let runtime = Pr13ProximityRuntimeOwnerV1::new(scope, evidence, configuration).unwrap();
    let Pr13ProximityRuntimeOutcomeV1::Completed(contributor) =
        runtime.evaluate(&context, &request).await
    else {
        panic!("production overlap must complete proximity evaluation");
    };
    assert_eq!(contributor.contributions().len(), 1);
    let contribution = &contributor.contributions()[0];
    assert_eq!(contribution.tier, ProximityTierV1::Immediate);
    assert_eq!(contribution.inclusion, ProximityInclusionV1::Included);
    assert_eq!(contribution.threshold_value_basis_points, None);
    assert_eq!(contribution.threshold_revision, None);
}
