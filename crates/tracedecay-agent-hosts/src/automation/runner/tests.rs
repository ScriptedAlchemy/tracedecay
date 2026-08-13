use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use serde_json::json;
use tracedecay_application::retained_surfaces::MemoryAutomationFactEvidenceItemV1;
use tracedecay_application::{
    CancellationContext, CapabilityGrantId, CapabilityGrantSnapshot, Deadline, DisclosureClass,
    RequestContext, RequestId,
};
use tracedecay_domain::{
    ActorId, FactOwnerV1, ProjectId, RepositoryId, RetrievalGrainV1, SessionId,
    TemporalCoverageCountsV1, UtcMicros, WorktreeId,
};
use tracedecay_temporal_query::TemporalKernelResult;
use tracedecay_temporal_query::context::VersionedTokenEstimator;
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};
use tracedecay_usecases::context::{
    BranchId, CancellationToken, CapabilityDigest, ConfigurationDigest, PolicyDigest, ProfileId,
    RequestBudgets, ResolvedGitRoute, ResolvedSessionIdentity, SessionRootId, SessionStoreId,
    session_application_grant_digest,
};
use tracedecay_usecases::memory::MemoryApplication;
use tracedecay_usecases::session::{
    SessionAccess, SessionAuthorizationError, SessionAuthorizationGrant, SessionFreshnessPolicy,
    SessionRequestBinding, SessionRetrievalConfiguration, SessionRetrievalOutcome,
    SessionRetrievalService, SessionScopeAuthorizationRequest, SessionScopeAuthorizer,
    SessionTemporalExecutionPort, SessionTemporalQuery,
};

use super::super::automatic_facts::{AutomaticFactState, record_session_automatic_facts};
use super::evidence::{
    AutomationEvidenceFilters, SESSION_REPLAY_SNIPPET_CHARS,
    serialize_automation_temporal_evidence, validate_complete_evidence,
};
use super::retrieval::{
    AUTOMATION_SESSION_MAX_BYTES, AutomationWordEstimator, accept_automation_temporal_outcome,
    retrieve_automation_session_evidence,
};
use super::{
    AuthorizedAutomationSessionRetrieval, AutomationRunControl, AutomationSessionRetrieval,
    AutomationSessionRetrievalFuture, AutomationTemporalEvidence, AutomationTemporalEvidenceItem,
    AutomationTemporalRetrieval, CombinedReviewDispatch, canonical_evidence_hash,
    combined_asymmetric_failure, combined_reflector_failure_projection,
    combined_skill_failure_projection, split_skill_runtime_failure,
    validate_session_fact_candidates,
};
use crate::db::{Database, DatabaseAuthority, TestDatabaseRuntimeMode};
use crate::ports::session_evidence::{LcmGrepSort, LcmScope};
use crate::store::memory::DatabaseFactStore;

mod early_gate;

#[test]
fn combined_skill_runtime_failure_preserves_both_error_causes() {
    let runtime_error = crate::errors::TraceDecayError::Config {
        message: "skill runtime failed".to_owned(),
    };
    let record_error = crate::errors::TraceDecayError::Config {
        message: "skill failed-record publication failed".to_owned(),
    };

    let (record, error, record_error) =
        split_skill_runtime_failure(runtime_error, Err(record_error));

    assert!(record.is_none());
    assert!(error.to_string().contains("skill runtime failed"));
    assert!(
        record_error
            .expect("failed-record cause")
            .to_string()
            .contains("skill failed-record publication failed")
    );
}

#[test]
fn asymmetric_combined_failure_preserves_the_successful_sibling_record() {
    let record = serde_json::from_value(json!({
        "schema_version": 2,
        "run_id": "combined-asymmetric",
        "trigger": "scheduler",
        "task": "session_reflector",
        "task_key": "session_reflector",
        "backend": "codex_app_server",
        "status": "failed",
        "accepted_count": 0,
        "rejected_count": 0,
        "error": "original failure",
        "started_at": "1",
        "completed_at": "2"
    }))
    .expect("failed record");
    let append_error = crate::errors::TraceDecayError::Config {
        message: "skill terminal construction failed".to_owned(),
    };
    let original_error = crate::errors::TraceDecayError::Config {
        message: "combined output failed".to_owned(),
    };

    let dispatch =
        combined_asymmetric_failure(Ok(record.clone()), Err(append_error), original_error);
    let CombinedReviewDispatch::FailureTerminals(failure) = dispatch else {
        panic!("asymmetric construction must retain each leg independently");
    };
    assert_eq!(failure.reflector_record, Some(record));
    assert!(failure.reflector_error.is_none());
    assert!(failure.skill_writer_record.is_none());
    assert!(failure.skill_writer_error.is_some());
}

#[test]
fn combined_reflector_failure_projection_is_payload_free() {
    let secret = "sk-live-combined-reflector-failure";
    let output = json!({
        "facts": [{"content": secret}],
        "skills": [{"instructions": secret}],
    });

    let projection = combined_reflector_failure_projection(&output);

    assert_eq!(projection.pointer("/proposed/count"), Some(&json!(1)));
    assert!(projection.pointer("/proposed/sha256").is_some());
    assert!(!serde_json::to_string(&projection).unwrap().contains(secret));
}

#[test]
fn combined_skill_failure_projection_excludes_fact_payloads() {
    let fact_secret = "sk-live-combined-fact-do-not-persist-in-skill-ledger";
    let output = json!({
        "facts": [{"content": fact_secret}],
        "skills": [{"name": "safe-skill", "instructions": "safe instructions"}],
    });

    let projection = combined_skill_failure_projection(&output);
    let serialized = serde_json::to_string(&projection).unwrap();

    assert_eq!(
        projection.pointer("/skills/0/name"),
        Some(&json!("safe-skill"))
    );
    assert!(!serialized.contains(fact_secret));
    assert!(projection.get("facts").is_none());
}

struct RecordingDenyAutomationAuthorizer {
    requests: Arc<Mutex<Vec<SessionScopeAuthorizationRequest>>>,
}

impl SessionScopeAuthorizer for RecordingDenyAutomationAuthorizer {
    fn authorize(
        &self,
        _context: &RequestContext,
        _binding: &SessionRequestBinding,
        request: &SessionScopeAuthorizationRequest,
    ) -> std::result::Result<SessionAuthorizationGrant, SessionAuthorizationError> {
        self.requests.lock().unwrap().push(request.clone());
        Err(SessionAuthorizationError::Denied)
    }
}

struct NeverAutomationExecution;

impl SessionTemporalExecutionPort for NeverAutomationExecution {
    fn execute<'a, E>(
        &'a self,
        _request: tracedecay_usecases::session::AuthorizedTemporalExecutionRequest,
        _estimator: &'a E,
    ) -> tracedecay_usecases::session::TemporalExecutionFuture<'a>
    where
        E: VersionedTokenEstimator + Sync + 'a,
    {
        Box::pin(async { panic!("denied retrieval must not reach temporal execution") })
    }
}

fn authorized_retrieval_context() -> (RequestContext, SessionRequestBinding) {
    let actor = ActorId::new("automation.session-evidence").unwrap();
    let request_id = RequestId::new("request.automation.session-evidence.test").unwrap();
    let identity = ResolvedSessionIdentity::for_project(
        ProfileId::new("profile.test").unwrap(),
        ProjectId::new("project.test").unwrap(),
        SessionStoreId::new("store.project.test").unwrap(),
        SessionRootId::new("root.project.test").unwrap(),
        ResolvedGitRoute::new(
            RepositoryId::new("repository.test").unwrap(),
            WorktreeId::new("worktree.test").unwrap(),
            BranchId::new("main").unwrap(),
        ),
    );
    let scope = identity.application_scope().unwrap();
    let capability = CapabilityDigest::new([0x11; 32]);
    let policy = PolicyDigest::new([0x22; 32]);
    let configuration = ConfigurationDigest::new([0x33; 32]);
    let cancellation = CancellationToken::for_application_request(request_id.as_str());
    let budgets = RequestBudgets::new(128, AUTOMATION_SESSION_MAX_BYTES, 10_000).unwrap();
    let grant = CapabilityGrantSnapshot::new(
        CapabilityGrantId::new("grant.automation.session-evidence.test").unwrap(),
        1,
        session_application_grant_digest(capability, policy, configuration, &cancellation, budgets)
            .unwrap(),
        actor.clone(),
        UtcMicros(1),
        UtcMicros(i64::MAX - 1),
        scope.clone(),
        BTreeSet::from([CapabilityId::new("capability.session.temporal-retrieval").unwrap()]),
        BTreeSet::from([UseCaseId::new("use-case.automation.session-evidence").unwrap()]),
        DisclosureClass::Evidence,
    )
    .unwrap();
    let context = RequestContext::new(
        actor,
        scope,
        grant,
        request_id.clone(),
        Deadline::new(UtcMicros(i64::MAX - 1)).unwrap(),
        CancellationContext::active(cancellation.application_token_id().unwrap()).unwrap(),
    )
    .unwrap();
    let binding = SessionRequestBinding::new(
        identity,
        capability,
        policy,
        configuration,
        cancellation,
        budgets,
    );
    (context, binding)
}

#[tokio::test]
async fn real_authorized_service_path_denies_before_execution() {
    let authorization_requests = Arc::new(Mutex::new(Vec::new()));
    let service = SessionRetrievalService::new(
        RecordingDenyAutomationAuthorizer {
            requests: Arc::clone(&authorization_requests),
        },
        NeverAutomationExecution,
        AutomationWordEstimator,
        SessionRetrievalConfiguration::new(1, 1).unwrap(),
    );
    let (context, binding) = authorized_retrieval_context();
    let adapter = AuthorizedAutomationSessionRetrieval::new(
        &service,
        &context,
        &binding,
        SessionId::new("session.authorized.test").unwrap(),
    );
    let outcome = retrieve_automation_session_evidence(
        &adapter,
        "authorized test",
        LcmScope::All,
        AutomationEvidenceFilters {
            provider: "cursor",
            session_id: None,
            include_summaries: true,
            evidence_limit: 5,
            include_recent_sessions: false,
            recent_sessions_limit: 1,
            role: None,
            start_time: None,
            end_time: None,
            sort: LcmGrepSort::Relevance,
        },
    )
    .await
    .unwrap();

    assert!(matches!(
        outcome,
        AutomationTemporalRetrieval::Rejected("session_evidence_denied")
    ));
    let requests = authorization_requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].temporal_mode(),
        tracedecay_domain::TemporalModeV1::Forensic
    );
    assert_eq!(requests[0].grain(), RetrievalGrainV1::LogicalMessage);
    assert_eq!(requests[0].access(), SessionAccess::Hydrate);
}

#[test]
fn temporal_automation_evidence_fails_closed_for_non_complete_outcomes() {
    for (outcome, expected_reason) in [
        (
            SessionRetrievalOutcome::<TemporalKernelResult>::Stale {
                freshness: tracedecay_usecases::session::SessionDataFreshness::Stored {
                    generation_lag: 1,
                },
            },
            "session_evidence_stale",
        ),
        (
            SessionRetrievalOutcome::Partial {
                items: Vec::new(),
                freshness: tracedecay_usecases::session::SessionDataFreshness::Fresh,
                omitted: 1,
            },
            "session_evidence_partial",
        ),
        (SessionRetrievalOutcome::Denied, "session_evidence_denied"),
        (
            SessionRetrievalOutcome::BudgetExhausted,
            "session_evidence_budget_exhausted",
        ),
        (
            SessionRetrievalOutcome::Cancelled,
            "session_evidence_cancelled",
        ),
        (
            SessionRetrievalOutcome::CompleteZero {
                freshness: tracedecay_usecases::session::SessionDataFreshness::Stored {
                    generation_lag: 2,
                },
            },
            "session_evidence_stale",
        ),
    ] {
        assert!(matches!(
            accept_automation_temporal_outcome(outcome),
            AutomationTemporalRetrieval::Rejected(reason) if reason == expected_reason
        ));
    }
    assert!(matches!(
        accept_automation_temporal_outcome(
            SessionRetrievalOutcome::<TemporalKernelResult>::CompleteZero {
                freshness: tracedecay_usecases::session::SessionDataFreshness::Fresh,
            }
        ),
        AutomationTemporalRetrieval::CompleteZero
    ));
}

#[test]
fn complete_temporal_outcome_independently_requires_fresh_coverage() {
    let stale = SessionRetrievalOutcome::<TemporalKernelResult>::Complete {
        items: Vec::new(),
        freshness: tracedecay_usecases::session::SessionDataFreshness::Stored { generation_lag: 1 },
    };
    // Empty Complete is invalid at the type layer, but stale freshness must
    // still fail closed before any serialization or write path.
    assert!(matches!(
        accept_automation_temporal_outcome(stale),
        AutomationTemporalRetrieval::Rejected("session_evidence_stale")
    ));
}

struct RecordingRejectedAutomationRetrieval {
    anchor_session_id: SessionId,
    queries: Mutex<Vec<SessionTemporalQuery>>,
}

impl AutomationSessionRetrieval for RecordingRejectedAutomationRetrieval {
    fn anchor_session_id(&self) -> &SessionId {
        &self.anchor_session_id
    }

    fn retrieve(&self, query: SessionTemporalQuery) -> AutomationSessionRetrievalFuture<'_> {
        self.queries.lock().unwrap().push(query);
        Box::pin(async { AutomationTemporalRetrieval::Rejected("session_evidence_denied") })
    }
}

#[tokio::test]
async fn automation_retrieval_requests_fresh_forensic_evidence_and_preserves_rejection() {
    let retrieval = RecordingRejectedAutomationRetrieval {
        anchor_session_id: SessionId::new("session.automation.recording").unwrap(),
        queries: Mutex::new(Vec::new()),
    };
    let outcome = retrieve_automation_session_evidence(
        &retrieval,
        "record the canonical request",
        LcmScope::All,
        AutomationEvidenceFilters {
            provider: "cursor",
            session_id: None,
            include_summaries: true,
            evidence_limit: 5,
            include_recent_sessions: true,
            recent_sessions_limit: 3,
            role: None,
            start_time: None,
            end_time: None,
            sort: LcmGrepSort::Relevance,
        },
    )
    .await
    .unwrap();

    assert!(matches!(
        outcome,
        AutomationTemporalRetrieval::Rejected("session_evidence_denied")
    ));
    let queries = retrieval.queries.lock().unwrap();
    assert_eq!(queries.len(), 1);
    assert_eq!(
        queries[0].temporal_mode(),
        tracedecay_domain::TemporalModeV1::Forensic
    );
    assert_eq!(
        queries[0].freshness_policy(),
        SessionFreshnessPolicy::RequireFresh
    );
    assert_eq!(queries[0].grain(), RetrievalGrainV1::LogicalMessage);
}

#[test]
fn builders_reject_hidden_unknown_and_redacted_complete_evidence() {
    for coverage in [
        TemporalCoverageCountsV1 {
            visible: 1,
            hidden: 1,
            unknown: 0,
            redacted: 0,
        },
        TemporalCoverageCountsV1 {
            visible: 1,
            hidden: 0,
            unknown: 1,
            redacted: 0,
        },
        TemporalCoverageCountsV1 {
            visible: 1,
            hidden: 0,
            unknown: 0,
            redacted: 1,
        },
    ] {
        let evidence = AutomationTemporalEvidence {
            items: vec![AutomationTemporalEvidenceItem {
                anchor_id: "coverage-anchor".to_string(),
                stable_id: "coverage-stable".to_string(),
                provider: "cursor".to_string(),
                session_id: "coverage-session".to_string(),
                message_id: Some("coverage-message".to_string()),
                source_id: Some("coverage-source".to_string()),
                store_id: Some(1),
                role: Some("user".to_string()),
                ordinal: Some(1),
                session_total_messages: Some(1),
                knowledge_at_micros: 1,
                normalized_score_micros: 1,
                snippet: "coverage".to_string(),
            }],
            coverage,
        };
        assert_eq!(
            validate_complete_evidence(&evidence),
            Err("session_evidence_partial")
        );
    }
}

#[test]
fn temporal_automation_serializer_preserves_citations_bounds_and_hashes() {
    let oversized = "x".repeat(SESSION_REPLAY_SNIPPET_CHARS + 25);
    let filters = AutomationEvidenceFilters {
        provider: "cursor",
        session_id: None,
        include_summaries: true,
        evidence_limit: 5,
        include_recent_sessions: true,
        recent_sessions_limit: 3,
        role: None,
        start_time: None,
        end_time: None,
        sort: LcmGrepSort::Recency,
    };
    let serialized = serialize_automation_temporal_evidence(
        AutomationTemporalEvidence {
            items: vec![
                AutomationTemporalEvidenceItem {
                    anchor_id: "anchor-1".to_string(),
                    stable_id: "stable-1".to_string(),
                    provider: "cursor".to_string(),
                    session_id: "session-1".to_string(),
                    message_id: Some("message-1".to_string()),
                    source_id: Some("occurrence-1".to_string()),
                    store_id: None,
                    role: Some("user".to_string()),
                    ordinal: Some(1),
                    session_total_messages: Some(1),
                    knowledge_at_micros: 1_715_000_001_000_000,
                    normalized_score_micros: 1_000_000,
                    snippet: oversized,
                },
                AutomationTemporalEvidenceItem {
                    anchor_id: "anchor-2".to_string(),
                    stable_id: "stable-2".to_string(),
                    provider: "cursor".to_string(),
                    session_id: "session-1".to_string(),
                    message_id: None,
                    source_id: Some("occurrence-1".to_string()),
                    store_id: None,
                    role: None,
                    ordinal: None,
                    session_total_messages: Some(1),
                    knowledge_at_micros: 1_715_000_000_000_000,
                    normalized_score_micros: 900_000,
                    snippet: "summary payload".to_string(),
                },
            ],
            coverage: TemporalCoverageCountsV1 {
                visible: 2,
                hidden: 0,
                unknown: 0,
                redacted: 0,
            },
        },
        filters,
    );

    // Raw-message citations carry `message_id` only; `node_id` is the
    // summary-node citation arm and must stay empty here.
    assert_eq!(serialized.hits[0].kind, "raw_message");
    assert_eq!(serialized.hits[0].session_id, "session-1");
    assert_eq!(serialized.hits[0].message_id.as_deref(), Some("message-1"));
    assert!(serialized.hits[0].node_id.is_none());
    assert_eq!(serialized.hits[0].anchor_id, "anchor-1");
    assert_eq!(serialized.hits[0].stable_id, "stable-1");
    assert_eq!(
        serialized.hits[0].snippet.chars().count(),
        SESSION_REPLAY_SNIPPET_CHARS
    );

    // Summary items (no message_id) cite through `node_id`, sourced from
    // the occurrence id.
    let summary_hit = serialized
        .hits
        .iter()
        .find(|hit| hit.message_id.is_none())
        .expect("summary evidence item must survive serialization");
    assert_eq!(summary_hit.kind, "summary_node");
    assert_eq!(summary_hit.node_id.as_deref(), Some("occurrence-1"));
    let replay = serialized.recent_session_slices.unwrap();
    assert_eq!(
        replay["sessions"][0]["head"][0]["message_id"],
        json!("message-1")
    );
    assert_eq!(replay["sessions"][0]["provider"], json!("cursor"));
    assert_eq!(replay["sessions"][0]["total_messages"], json!(1));
    assert_eq!(replay["sessions"][0]["head"][0]["ordinal"], json!(1));
    assert_eq!(
        replay["sessions"][0]["head"][0]["anchor_id"],
        json!("anchor-1")
    );
    assert_eq!(
        replay["bounds"]["snippet_chars"],
        json!(SESSION_REPLAY_SNIPPET_CHARS)
    );
    let mut evidence = json!({
        "hits": serialized.hits,
        "recent_session_slices": replay,
        "temporal_coverage": serialized.coverage,
    });
    let first_hash = canonical_evidence_hash(&evidence);
    evidence["hits"][0]["message_id"] = json!("message-2");
    assert!(first_hash.starts_with("sha256:"));
    assert_ne!(first_hash, canonical_evidence_hash(&evidence));
}

#[test]
fn canonical_evidence_is_permutation_stable_and_request_bound() {
    let item =
        |provider: &str, anchor: &str, ordinal: i64, score: u64| AutomationTemporalEvidenceItem {
            anchor_id: anchor.to_string(),
            stable_id: format!("stable-{anchor}"),
            provider: provider.to_string(),
            session_id: "session-canonical".to_string(),
            message_id: Some(format!("message-{ordinal}")),
            source_id: Some(format!("occurrence-{ordinal}")),
            store_id: Some(ordinal),
            role: Some("user".to_string()),
            ordinal: Some(ordinal),
            session_total_messages: Some(2),
            knowledge_at_micros: 1_715_000_000_000_000 + ordinal,
            normalized_score_micros: score,
            snippet: format!("payload-{ordinal}"),
        };
    let filters = AutomationEvidenceFilters {
        provider: "all",
        session_id: None,
        include_summaries: true,
        evidence_limit: 1,
        include_recent_sessions: false,
        recent_sessions_limit: 3,
        role: None,
        start_time: None,
        end_time: None,
        sort: LcmGrepSort::Relevance,
    };
    let serialize = |items| {
        serialize_automation_temporal_evidence(
            AutomationTemporalEvidence {
                items,
                coverage: TemporalCoverageCountsV1 {
                    visible: 2,
                    hidden: 0,
                    unknown: 0,
                    redacted: 0,
                },
            },
            filters,
        )
    };
    let first = serialize(vec![
        item("cursor", "anchor-a", 1, 10),
        item("codex", "anchor-b", 2, 20),
    ]);
    let second = serialize(vec![
        item("codex", "anchor-b", 2, 20),
        item("cursor", "anchor-a", 1, 10),
    ]);
    let first_value = json!({
        "provider": "all",
        "query": "canonical request",
        "sort": "relevance",
        "hits": first.hits,
        "temporal_coverage": first.coverage,
    });
    let second_value = json!({
        "provider": "all",
        "query": "canonical request",
        "sort": "relevance",
        "hits": second.hits,
        "temporal_coverage": second.coverage,
    });
    let digest = canonical_evidence_hash(&first_value);

    assert_eq!(first_value, second_value);
    assert_eq!(first_value["hits"][0]["provider"], json!("codex"));
    assert_eq!(first_value["hits"][0]["anchor_id"], json!("anchor-b"));
    assert_eq!(first_value["temporal_coverage"]["visible"], json!(1));
    assert_eq!(
        digest,
        "sha256:20c37de4e2fdcca8c190087087c6ad4a0ae1ba2969bcb8cee018c6ec6a6edac3"
    );

    let mut provider_mutation = first_value.clone();
    provider_mutation["provider"] = json!("cursor");
    assert_ne!(digest, canonical_evidence_hash(&provider_mutation));
    let mut query_mutation = first_value;
    query_mutation["query"] = json!("different request");
    assert_ne!(digest, canonical_evidence_hash(&query_mutation));
}

#[tokio::test]
async fn proposal_validation_does_not_wait_for_the_writer_lane() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("memory.db");
    crate::register_test_schema_installer();
    let authority =
        crate::db::DatabaseAuthority::acquire_test(&path, "automation validation writer lane")
            .unwrap();
    let (db, _) = crate::db::Database::publish_test_runtime(
        &path,
        &authority,
        TestDatabaseRuntimeMode::Initialize,
    )
    .await
    .unwrap();
    let owner = FactOwnerV1::Profile;
    let memory = MemoryApplication::new(owner.clone(), DatabaseFactStore::new(&db)).unwrap();
    let interrupted = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let run_control = AutomationRunControl::from_interrupted({
        let interrupted = Arc::clone(&interrupted);
        Arc::new(move || interrupted.load(std::sync::atomic::Ordering::Acquire))
    });
    let preflight = memory
        .preflight_project_memory_fact_add(
            tracedecay_usecases::memory::ProjectMemoryFactAddRequest {
                content: "Committed memory baseline".to_string(),
                category: tracedecay_domain::FactCategoryV1::Project,
                source_label: None,
                tags: vec!["automation".to_string()],
                entities: vec!["TraceDecay".to_string()],
                trust: Some(tracedecay_domain::Confidence::new(0.8).unwrap()),
                metadata: json!({}),
            },
            None,
        )
        .unwrap();
    let write_control = run_control.write_control();
    let outcome = memory
        .add_preflighted_project_memory_fact(preflight, &write_control)
        .await
        .unwrap();
    let tracedecay_usecases::memory::ProjectMemoryFactAddRequestOutcome::Applied(outcome) = outcome
    else {
        panic!("seed request must apply");
    };
    let tracedecay_store::ProjectMemoryFactProjectionV1::Available(existing_fact) = outcome.fact()
    else {
        panic!("seeded fact payload must remain available");
    };
    let existing_fact_id = existing_fact.fact_id().clone();
    let transaction = db
        .begin_write_transaction("hold automation validation writer")
        .await
        .unwrap();
    let proposals = [json!({
        "content": "Validation stays read-only",
        "category": "project",
        "tags": ["automation"],
        "entities": ["TraceDecay"],
        "trust": 0.8,
        "source_span": {"session_id": "session", "message_id": "message"},
        "reason": "bounded test evidence"
    })];
    let evidence = json!({
        "hits": [{
            "kind": "raw_message",
            "session_id": "session",
            "message_id": "message"
        }]
    });

    let validated = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        validate_session_fact_candidates(&memory, &run_control, &proposals, &evidence),
    )
    .await
    .expect("read-only validation must not wait for writer authority")
    .unwrap();
    assert_eq!(validated.0.len(), 1);
    assert!(validated.1.is_empty());
    let fact = memory
        .get_project_memory_fact(
            tracedecay_store::ProjectMemoryFactIdV1::new(owner, existing_fact_id)
                .expect("canonical fact target"),
            run_control.read_control(),
        )
        .await
        .unwrap()
        .expect("seeded fact remains available");
    let tracedecay_store::ProjectMemoryFactProjectionV1::Available(fact) = fact else {
        panic!("seeded fact payload must remain available");
    };
    assert_eq!(fact.telemetry().access_count(), 0);
    transaction.rollback().await.unwrap();
}

#[tokio::test]
async fn proposal_validation_quarantines_fields_outside_the_public_receipt_contract() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("memory.db");
    crate::register_test_schema_installer();
    let authority =
        crate::db::DatabaseAuthority::acquire_test(&path, "automation evidence closed fields")
            .unwrap();
    let (db, _) = crate::db::Database::publish_test_runtime(
        &path,
        &authority,
        TestDatabaseRuntimeMode::Initialize,
    )
    .await
    .unwrap();
    let memory = MemoryApplication::new(FactOwnerV1::Profile, DatabaseFactStore::new(&db)).unwrap();
    let run_control = AutomationRunControl::from_interrupted(Arc::new(|| false));
    let evidence = json!({
        "hits": [{
            "kind": "raw_message",
            "session_id": "session",
            "message_id": "message"
        }]
    });
    for proposal in [
        json!({
            "content": "Unknown top-level evidence cannot commit",
            "category": "project",
            "tags": ["automation"],
            "entities": ["TraceDecay"],
            "trust": 0.8,
            "source_span": {"session_id": "session", "message_id": "message"},
            "reason": "bounded evidence",
            "unknown": true
        }),
        json!({
            "content": "Unknown source span evidence cannot commit",
            "category": "project",
            "tags": ["automation"],
            "entities": ["TraceDecay"],
            "trust": 0.8,
            "source_span": {
                "session_id": "session",
                "message_id": "message",
                "unknown": true
            },
            "reason": "bounded evidence"
        }),
        json!({
            "content": "Mixed source identities cannot commit",
            "category": "project",
            "tags": ["automation"],
            "entities": ["TraceDecay"],
            "trust": 0.8,
            "source_span": {
                "session_id": "session",
                "message_id": "message",
                "store_id": 7
            },
            "reason": "bounded evidence"
        }),
        json!({
            "content": "Incomplete raw-message identities cannot commit",
            "category": "project",
            "tags": ["automation"],
            "entities": ["TraceDecay"],
            "trust": 0.8,
            "source_span": {"session_id": "session"},
            "reason": "bounded evidence"
        }),
        json!({
            "content": "Null optional source identities cannot commit",
            "category": "project",
            "tags": ["automation"],
            "entities": ["TraceDecay"],
            "trust": 0.8,
            "source_span": {
                "session_id": "session",
                "message_id": "message",
                "node_id": null
            },
            "reason": "bounded evidence"
        }),
        json!({
            "content": "Oversized reason evidence cannot commit",
            "category": "project",
            "tags": ["automation"],
            "entities": ["TraceDecay"],
            "trust": 0.8,
            "source_span": {"session_id": "session", "message_id": "message"},
            "reason": "r".repeat(4_097)
        }),
        json!({
            "content": "Control characters in evidence cannot commit",
            "category": "project",
            "tags": ["automation"],
            "entities": ["TraceDecay"],
            "trust": 0.8,
            "source_span": {"session_id": "session", "message_id": "message"},
            "reason": "invalid\u{0000}reason"
        }),
    ] {
        let validated = validate_session_fact_candidates(
            &memory,
            &run_control,
            std::slice::from_ref(&proposal),
            &evidence,
        )
        .await
        .unwrap();
        assert!(validated.0.is_empty());
        assert_eq!(validated.1.len(), 1);
    }
}

#[tokio::test]
async fn proposal_validation_canonicalizes_public_evidence_before_commit() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("memory.db");
    crate::register_test_schema_installer();
    let authority =
        crate::db::DatabaseAuthority::acquire_test(&path, "canonical automation evidence").unwrap();
    let (db, _) = crate::db::Database::publish_test_runtime(
        &path,
        &authority,
        TestDatabaseRuntimeMode::Initialize,
    )
    .await
    .unwrap();
    let memory = MemoryApplication::new(FactOwnerV1::Profile, DatabaseFactStore::new(&db)).unwrap();
    let run_control = AutomationRunControl::from_interrupted(Arc::new(|| false));
    let evidence = json!({
        "hits": [{
            "kind": "raw_message",
            "session_id": "session",
            "message_id": "message"
        }]
    });

    for (raw_trust, canonical_trust) in [(json!(1), json!(1.0)), (json!(" HIGH "), json!("high"))] {
        let proposal = json!({
            "content": "  Canonical evidence reaches the store  ",
            "category": "project",
            "tags": [" automation "],
            "entities": [" TraceDecay "],
            "trust": raw_trust,
            "source_span": {"session_id": "session", "message_id": "message"},
            "reason": "  bounded evidence  "
        });
        let validated = validate_session_fact_candidates(
            &memory,
            &run_control,
            std::slice::from_ref(&proposal),
            &evidence,
        )
        .await
        .unwrap();
        assert_eq!(validated.0.len(), 1);
        assert!(validated.1.is_empty());
        let item = validated.0[0]["item"].clone();
        assert_eq!(item["content"], "Canonical evidence reaches the store");
        assert_eq!(item["category"], "project");
        assert_eq!(item["tags"], json!(["automation"]));
        assert_eq!(item["entities"], json!(["TraceDecay"]));
        assert_eq!(item["trust"], canonical_trust);
        assert_eq!(item["reason"], "bounded evidence");
        serde_json::from_value::<MemoryAutomationFactEvidenceItemV1>(item)
            .expect("accepted evidence must decode through the public terminal type");
    }
}

#[tokio::test]
async fn automatic_session_curation_records_one_terminal_effect() {
    let temp = tempfile::tempdir().unwrap();
    let database_path = temp.path().join("memory.db");
    crate::register_test_schema_installer();
    let authority =
        DatabaseAuthority::acquire_test(&database_path, "automatic session curation receipt test")
            .unwrap();
    let (database, _) = Database::publish_test_runtime(
        &database_path,
        &authority,
        TestDatabaseRuntimeMode::Initialize,
    )
    .await
    .unwrap();
    let memory =
        MemoryApplication::new(FactOwnerV1::Profile, DatabaseFactStore::new(&database)).unwrap();
    let interrupted = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let run_control = AutomationRunControl::from_interrupted({
        let interrupted = Arc::clone(&interrupted);
        Arc::new(move || interrupted.load(std::sync::atomic::Ordering::Acquire))
    });
    let accepted = json!({
        "add_fact_request": {
            "content": "Validated session curation records one terminal effect",
            "category": "project",
            "source_label": "automation-test",
            "tags": ["automation"],
            "entities": ["TraceDecay"],
            "trust": 0.9,
            "metadata": {}
        }
    });
    let applied = record_session_automatic_facts(
        &memory,
        &run_control,
        "run-automatic-session-curation",
        Some(&format!("sha256:{}", "a".repeat(64))),
        &[accepted.clone()],
    )
    .await
    .unwrap();
    assert!(applied.retry_error.is_none());
    assert_eq!(applied.receipts[0].state, AutomaticFactState::Applied);

    let replayed = record_session_automatic_facts(
        &memory,
        &run_control,
        "run-automatic-session-curation",
        Some(&format!("sha256:{}", "a".repeat(64))),
        &[accepted],
    )
    .await
    .unwrap();
    assert!(replayed.retry_error.is_none());
    assert_eq!(replayed.receipts, applied.receipts);
}
