//! Application admission and outcome ownership over the registered session
//! store. Every retrieval runs the production execution against a real
//! registered shard; request digests are read from the production admission
//! step both `retrieve` and `execute_task_session` run before execution.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sha2::{Digest, Sha256};
use tracedecay_contracts::{
    CancellationContext, CapabilityGrantId, CapabilityGrantSnapshot, Deadline, DisclosureClass,
    RequestContext, RequestId,
};
use tracedecay_domain::{
    ActorId, ProjectId, RepositoryId, RetrievalGrainV1, SessionId, TemporalModeV1, UtcMicros,
    WorktreeId,
};
use tracedecay_global_db::RegisteredGlobalDb;
use tracedecay_runtime_core::cancellation::CancellationToken;
use tracedecay_session_temporal_store::RegisteredGlobalDbSessionTemporalExecution;
use tracedecay_store::runtime::DEFAULT_MAX_READERS_PER_HOT_SHARD;
use tracedecay_temporal_query::TemporalKernelResult;
use tracedecay_temporal_query::context::{ContextBudget, TokenPolicy, VersionedTokenEstimator};
use tracedecay_temporal_query::execution::ExecutionLimits;
use tracedecay_temporal_query::ranking::DiversityLimits;
use tracedecay_temporal_query::snapshot::TemporalRetrievalScope;
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

use super::harness::{EXTERNAL_PAYLOAD, INLINE_PAYLOAD, PROJECT_ID, RegisteredTemporalHarness};
use crate::context::{
    BranchId, CapabilityDigest, ConfigurationDigest, PolicyDigest, ProfileId, RequestBudgets,
    ResolvedGitRoute, ResolvedSessionIdentity, SessionRootId, SessionStoreId,
    application_observed_at, session_application_grant_digest,
};
use crate::session::{
    AuthorizationGrantId, SessionAccess, SessionAuthorizationError, SessionAuthorizationGrant,
    SessionFreshnessPolicy, SessionRequestBinding, SessionRetrievalBudgetStageV1,
    SessionRetrievalConfiguration, SessionRetrievalOutcome, SessionRetrievalScope,
    SessionRetrievalService, SessionScopeAuthorizationRequest, SessionScopeAuthorizer,
    SessionTemporalQuery,
};

const DIGEST: [u8; 32] = [0x5a; 32];

struct AllowAuthorizer;

impl SessionScopeAuthorizer for AllowAuthorizer {
    fn authorize(
        &self,
        context: &RequestContext,
        binding: &SessionRequestBinding,
        request: &SessionScopeAuthorizationRequest,
    ) -> Result<SessionAuthorizationGrant, SessionAuthorizationError> {
        SessionAuthorizationGrant::issue(
            AuthorizationGrantId::new("grant.temporal.application").unwrap(),
            7,
            context,
            binding,
            request,
        )
    }
}

struct DenyAuthorizer;

impl SessionScopeAuthorizer for DenyAuthorizer {
    fn authorize(
        &self,
        _context: &RequestContext,
        _binding: &SessionRequestBinding,
        _request: &SessionScopeAuthorizationRequest,
    ) -> Result<SessionAuthorizationGrant, SessionAuthorizationError> {
        Err(SessionAuthorizationError::Denied)
    }
}

#[derive(Clone, Copy)]
struct GrantAuthorizer {
    id: &'static str,
    revision: u64,
}

impl SessionScopeAuthorizer for GrantAuthorizer {
    fn authorize(
        &self,
        context: &RequestContext,
        binding: &SessionRequestBinding,
        request: &SessionScopeAuthorizationRequest,
    ) -> Result<SessionAuthorizationGrant, SessionAuthorizationError> {
        SessionAuthorizationGrant::issue(
            AuthorizationGrantId::new(self.id).unwrap(),
            self.revision,
            context,
            binding,
            request,
        )
    }
}

struct MismatchedGrantAuthorizer;

impl SessionScopeAuthorizer for MismatchedGrantAuthorizer {
    fn authorize(
        &self,
        context: &RequestContext,
        binding: &SessionRequestBinding,
        request: &SessionScopeAuthorizationRequest,
    ) -> Result<SessionAuthorizationGrant, SessionAuthorizationError> {
        let mismatched = SessionScopeAuthorizationRequest::new(
            request.actor_id().clone(),
            request.identity().clone(),
            SessionId::new("session.other").unwrap(),
            request.provider_scope().map(str::to_owned),
            request.temporal_mode(),
            request.grain(),
            request.access(),
        )?;
        SessionAuthorizationGrant::issue(
            AuthorizationGrantId::new("grant.mismatched").unwrap(),
            1,
            context,
            binding,
            &mismatched,
        )
    }
}

#[derive(Clone)]
struct ReplayedGrantAuthorizer(SessionAuthorizationGrant);

impl SessionScopeAuthorizer for ReplayedGrantAuthorizer {
    fn authorize(
        &self,
        _context: &RequestContext,
        _binding: &SessionRequestBinding,
        _request: &SessionScopeAuthorizationRequest,
    ) -> Result<SessionAuthorizationGrant, SessionAuthorizationError> {
        Ok(self.0.clone())
    }
}

struct CancellingAuthorizer;

impl SessionScopeAuthorizer for CancellingAuthorizer {
    fn authorize(
        &self,
        context: &RequestContext,
        binding: &SessionRequestBinding,
        request: &SessionScopeAuthorizationRequest,
    ) -> Result<SessionAuthorizationGrant, SessionAuthorizationError> {
        let grant = SessionAuthorizationGrant::issue(
            AuthorizationGrantId::new("grant.cancel-during-authorization").unwrap(),
            1,
            context,
            binding,
            request,
        )?;
        binding.cancellation().cancel();
        Ok(grant)
    }
}

struct DelayingAuthorizer(Duration);

impl SessionScopeAuthorizer for DelayingAuthorizer {
    fn authorize(
        &self,
        context: &RequestContext,
        binding: &SessionRequestBinding,
        request: &SessionScopeAuthorizationRequest,
    ) -> Result<SessionAuthorizationGrant, SessionAuthorizationError> {
        std::thread::sleep(self.0);
        SessionAuthorizationGrant::issue(
            AuthorizationGrantId::new("grant.deadline-during-authorization").unwrap(),
            1,
            context,
            binding,
            request,
        )
    }
}

type CapturedTarget = (
    SessionRetrievalScope,
    Option<String>,
    TemporalModeV1,
    RetrievalGrainV1,
    SessionAccess,
);

struct CapturingAuthorizer {
    target: Arc<Mutex<Option<CapturedTarget>>>,
}

impl SessionScopeAuthorizer for CapturingAuthorizer {
    fn authorize(
        &self,
        context: &RequestContext,
        binding: &SessionRequestBinding,
        request: &SessionScopeAuthorizationRequest,
    ) -> Result<SessionAuthorizationGrant, SessionAuthorizationError> {
        *self.target.lock().unwrap() = Some((
            request.retrieval_scope().clone(),
            request.provider_scope().map(str::to_owned),
            request.temporal_mode(),
            request.grain(),
            request.access(),
        ));
        SessionAuthorizationGrant::issue(
            AuthorizationGrantId::new("grant.captured").unwrap(),
            1,
            context,
            binding,
            request,
        )
    }
}

#[derive(Clone, Copy)]
struct Words(&'static str);

impl VersionedTokenEstimator for Words {
    fn version(&self) -> &str {
        self.0
    }

    fn token_policy(&self) -> TokenPolicy {
        TokenPolicy::Whitespace
    }
}

#[derive(Clone)]
struct TestRequestContext {
    request: RequestContext,
    binding: SessionRequestBinding,
}

impl TestRequestContext {
    fn binding(&self) -> &SessionRequestBinding {
        &self.binding
    }

    fn cancellation(&self) -> &CancellationToken {
        self.binding.cancellation()
    }

    fn deadline(&self) -> &Deadline {
        self.request.deadline()
    }
}

fn project_identity(root: &str) -> ResolvedSessionIdentity {
    ResolvedSessionIdentity::for_project(
        ProfileId::new("profile.primary").unwrap(),
        ProjectId::new(PROJECT_ID).unwrap(),
        SessionStoreId::new("store.project.tracedecay").unwrap(),
        SessionRootId::new(root).unwrap(),
        ResolvedGitRoute::new(
            RepositoryId::new("repository.tracedecay").unwrap(),
            WorktreeId::new("worktree.main").unwrap(),
            BranchId::new("branch.temporal-application").unwrap(),
        ),
    )
}

fn default_budgets() -> RequestBudgets {
    RequestBudgets::new(64, 64 * 1024 * 1024, 10_000).unwrap()
}

/// Every input a request context binds; tests vary one field at a time.
#[derive(Clone)]
struct ContextSpec {
    actor: &'static str,
    request_id: &'static str,
    identity: ResolvedSessionIdentity,
    capability: [u8; 32],
    policy: [u8; 32],
    configuration: [u8; 32],
    budgets: RequestBudgets,
    cancellation: Option<CancellationToken>,
    expires_at: Option<UtcMicros>,
}

impl ContextSpec {
    fn new(root: &str, policy: [u8; 32]) -> Self {
        Self {
            actor: "actor.cursor",
            request_id: "request.temporal.application",
            identity: project_identity(root),
            capability: DIGEST,
            policy,
            configuration: DIGEST,
            budgets: default_budgets(),
            cancellation: None,
            expires_at: None,
        }
    }

    fn build(self) -> TestRequestContext {
        let actor = ActorId::new(self.actor).unwrap();
        let request_id = RequestId::new(self.request_id).unwrap();
        let capability = CapabilityDigest::new(self.capability);
        let policy = PolicyDigest::new(self.policy);
        let configuration = ConfigurationDigest::new(self.configuration);
        let cancellation = self
            .cancellation
            .unwrap_or_else(|| CancellationToken::for_application_request(request_id.as_str()));
        let expires_at = self.expires_at.unwrap_or(UtcMicros(
            application_observed_at().0.saturating_add(30_000_000),
        ));
        // Session requests are admitted under `session_request_scope`, exactly
        // as the daemon resolves them: it is total across both owners, whereas
        // `application_scope` fails closed for a profile-owned identity.
        let scope = self.identity.session_request_scope().unwrap();
        let grant = CapabilityGrantSnapshot::new(
            CapabilityGrantId::new("grant.temporal.application.context").unwrap(),
            1,
            session_application_grant_digest(
                capability,
                policy,
                configuration,
                &cancellation,
                self.budgets,
            )
            .unwrap(),
            actor.clone(),
            application_observed_at(),
            UtcMicros(i64::MAX - 1),
            scope.clone(),
            BTreeSet::from([CapabilityId::new("capability.session.temporal-retrieval").unwrap()]),
            BTreeSet::from([UseCaseId::new("use-case.session.temporal-retrieval").unwrap()]),
            DisclosureClass::Evidence,
        )
        .unwrap();
        let request = RequestContext::new(
            actor,
            scope,
            grant,
            request_id,
            Deadline::new(expires_at).unwrap(),
            CancellationContext::active(cancellation.application_token_id().unwrap()).unwrap(),
        )
        .unwrap();
        let binding = SessionRequestBinding::new(
            self.identity,
            capability,
            policy,
            configuration,
            cancellation,
            self.budgets,
        );
        TestRequestContext { request, binding }
    }
}

fn context(root: &str, policy: [u8; 32]) -> TestRequestContext {
    ContextSpec::new(root, policy).build()
}

fn configuration() -> SessionRetrievalConfiguration {
    SessionRetrievalConfiguration::new(3, 5).unwrap()
}

fn service<A: SessionScopeAuthorizer>(
    harness: &RegisteredTemporalHarness,
    authorizer: A,
    estimator: Words,
    configuration: SessionRetrievalConfiguration,
) -> SessionRetrievalService<'_, A, RegisteredGlobalDb, Words> {
    SessionRetrievalService::new(
        authorizer,
        RegisteredGlobalDbSessionTemporalExecution::new(harness.registered.as_ref()),
        estimator,
        configuration,
    )
}

fn allow(
    harness: &RegisteredTemporalHarness,
) -> SessionRetrievalService<'_, AllowAuthorizer, RegisteredGlobalDb, Words> {
    service(harness, AllowAuthorizer, Words("words-v1"), configuration())
}

async fn retrieve<A: SessionScopeAuthorizer>(
    service: &SessionRetrievalService<'_, A, RegisteredGlobalDb, Words>,
    context: &TestRequestContext,
    query: SessionTemporalQuery,
) -> SessionRetrievalOutcome<TemporalKernelResult> {
    service
        .retrieve(&context.request, context.binding(), query)
        .await
}

fn context_budget() -> ContextBudget {
    ContextBudget {
        max_bytes: 64_000,
        max_tokens: 16_000,
        estimator_version: "words-v1".to_owned(),
    }
}

#[derive(Clone)]
struct QuerySpec {
    session_id: &'static str,
    provider: Option<&'static str>,
    text: &'static str,
    cursor: Option<String>,
    temporal_mode: TemporalModeV1,
    grain: RetrievalGrainV1,
    limit: usize,
    diversity: DiversityLimits,
    context_budget: ContextBudget,
    execution_limits: ExecutionLimits,
    freshness_policy: SessionFreshnessPolicy,
    retrieval_scope: Option<SessionRetrievalScope>,
}

impl Default for QuerySpec {
    fn default() -> Self {
        Self {
            session_id: "session.temporal.application",
            provider: None,
            text: "alpha",
            cursor: None,
            temporal_mode: TemporalModeV1::Current,
            grain: RetrievalGrainV1::LogicalMessage,
            limit: 8,
            diversity: DiversityLimits::default(),
            context_budget: context_budget(),
            execution_limits: ExecutionLimits::default(),
            freshness_policy: SessionFreshnessPolicy::AllowStored,
            retrieval_scope: None,
        }
    }
}

impl QuerySpec {
    fn build(self) -> SessionTemporalQuery {
        let query = SessionTemporalQuery::new(
            SessionId::new(self.session_id).unwrap(),
            self.provider.map(str::to_owned),
            self.text,
            self.cursor,
            self.temporal_mode,
            self.grain,
            self.limit,
            self.diversity,
            self.context_budget,
        )
        .unwrap()
        .with_execution_limits(self.execution_limits)
        .with_freshness_policy(self.freshness_policy);
        match self.retrieval_scope {
            Some(scope) => query.with_retrieval_scope(scope),
            None => query,
        }
    }
}

fn query(text: &'static str) -> SessionTemporalQuery {
    QuerySpec {
        text,
        ..QuerySpec::default()
    }
    .build()
}

/// A query the application fixture answers with its inline occurrence.
fn inline_query() -> QuerySpec {
    QuerySpec {
        provider: Some("provider.application"),
        text: "inline",
        grain: RetrievalGrainV1::Occurrence,
        ..QuerySpec::default()
    }
}

fn root_page(limit: usize, cursor: Option<String>) -> SessionTemporalQuery {
    QuerySpec {
        session_id: "session.root.a",
        text: "root-wide",
        cursor,
        grain: RetrievalGrainV1::Occurrence,
        limit,
        diversity: DiversityLimits::unbounded(),
        retrieval_scope: Some(SessionRetrievalScope::AllSessionsInAuthorizedRoot),
        ..QuerySpec::default()
    }
    .build()
}

fn rendered(outcome: &SessionRetrievalOutcome<TemporalKernelResult>) -> &str {
    match outcome {
        SessionRetrievalOutcome::Complete { items, .. } => &items[0].context.rendered,
        other => panic!("retrieval was not complete: {other:?}"),
    }
}

/// `(request digest, access digest)` the production admission step binds for
/// this request, read before any execution is constructed.
async fn admitted_digests<A: SessionScopeAuthorizer>(
    authorizer: A,
    context: TestRequestContext,
    query: SessionTemporalQuery,
    estimator: Words,
    configuration: SessionRetrievalConfiguration,
) -> (String, String) {
    let harness = RegisteredTemporalHarness::open("temporal-admission-digest").await;
    let service = service(&harness, authorizer, estimator, configuration);
    let admitted = service
        .admit_execution(&context.request, context.binding(), &query)
        .unwrap();
    let request = admitted.execution.snapshot_request();
    (
        request.request_digest().as_str().to_owned(),
        request.access_digest().as_str().to_owned(),
    )
}

async fn admitted_digest<A: SessionScopeAuthorizer>(
    authorizer: A,
    context: TestRequestContext,
    query: SessionTemporalQuery,
    estimator: Words,
    configuration: SessionRetrievalConfiguration,
) -> String {
    admitted_digests(authorizer, context, query, estimator, configuration)
        .await
        .0
}

const BASELINE_GRANT: GrantAuthorizer = GrantAuthorizer {
    id: "grant.baseline",
    revision: 1,
};

#[tokio::test]
async fn canonical_request_digest_drifts_for_query_and_root_changes() {
    let harness = RegisteredTemporalHarness::open("temporal-digest-drift").await;
    let policy = harness.seed_empty_fixture().await;
    let service = allow(&harness);

    for (root, text) in [
        ("root.one", "alpha"),
        ("root.one", "beta"),
        ("root.two", "alpha"),
    ] {
        assert!(matches!(
            retrieve(&service, &context(root, policy), query(text)).await,
            SessionRetrievalOutcome::CompleteZero { .. }
        ));
    }
    let first = admitted_digest(
        AllowAuthorizer,
        context("root.one", policy),
        query("alpha"),
        Words("words-v1"),
        configuration(),
    )
    .await;
    let second = admitted_digest(
        AllowAuthorizer,
        context("root.one", policy),
        query("beta"),
        Words("words-v1"),
        configuration(),
    )
    .await;
    let third = admitted_digest(
        AllowAuthorizer,
        context("root.two", policy),
        query("alpha"),
        Words("words-v1"),
        configuration(),
    )
    .await;
    assert_ne!(first, second);
    assert_ne!(first, third);
}

#[tokio::test]
async fn application_authorizes_and_validates_the_exact_retrieval_target() {
    let harness = RegisteredTemporalHarness::open("temporal-exact-target").await;
    let policy = harness.seed_empty_fixture().await;
    let captured = Arc::new(Mutex::new(None));
    let capturing = service(
        &harness,
        CapturingAuthorizer {
            target: Arc::clone(&captured),
        },
        Words("words-v1"),
        configuration(),
    );
    let spec = QuerySpec {
        provider: Some("cursor"),
        temporal_mode: TemporalModeV1::AsOf {
            cutoff: UtcMicros(77),
        },
        grain: RetrievalGrainV1::Summary,
        ..QuerySpec::default()
    };

    assert!(matches!(
        retrieve(&capturing, &context("root.one", policy), spec.build()).await,
        SessionRetrievalOutcome::CompleteZero { .. }
    ));
    assert_eq!(
        captured.lock().unwrap().clone().unwrap(),
        (
            SessionRetrievalScope::Session(SessionId::new("session.temporal.application").unwrap()),
            Some("cursor".to_owned()),
            TemporalModeV1::AsOf {
                cutoff: UtcMicros(77)
            },
            RetrievalGrainV1::Summary,
            SessionAccess::Hydrate,
        )
    );

    let rejected = service(
        &harness,
        MismatchedGrantAuthorizer,
        Words("words-v1"),
        configuration(),
    );
    assert!(matches!(
        retrieve(&rejected, &context("root.one", policy), query("alpha")).await,
        SessionRetrievalOutcome::WrongScope
    ));
}

#[tokio::test]
async fn application_binds_and_freezes_root_wide_retrieval_scope() {
    let harness = RegisteredTemporalHarness::open("temporal-root-scope").await;
    let policy = harness.seed_root_fixture().await;
    let captured = Arc::new(Mutex::new(None));
    let capturing = service(
        &harness,
        CapturingAuthorizer {
            target: Arc::clone(&captured),
        },
        Words("words-v1"),
        configuration(),
    );

    let outcome = retrieve(&capturing, &context("root.one", policy), root_page(8, None)).await;
    let SessionRetrievalOutcome::Complete { items, .. } = outcome else {
        panic!("root-wide retrieval was not complete: {outcome:?}");
    };
    assert_eq!(
        captured.lock().unwrap().as_ref().unwrap().0,
        SessionRetrievalScope::AllSessionsInAuthorizedRoot
    );
    assert_eq!(
        items[0].snapshot.request().retrieval_scope(),
        &TemporalRetrievalScope::AllSessionsInAuthorizedRoot
    );
}

#[tokio::test]
async fn canonical_digest_binds_every_semantic_input_and_excludes_resume_ephemera() {
    let baseline_context = || ContextSpec::new("root.one", DIGEST);
    let (baseline, baseline_access) = admitted_digests(
        BASELINE_GRANT,
        baseline_context().build(),
        QuerySpec::default().build(),
        Words("words-v1"),
        configuration(),
    )
    .await;
    let digest_for = |context: ContextSpec, spec: QuerySpec| async move {
        let estimator = Words(if spec.context_budget.estimator_version == "words-v2" {
            "words-v2"
        } else {
            "words-v1"
        });
        admitted_digests(
            BASELINE_GRANT,
            context.build(),
            spec.build(),
            estimator,
            configuration(),
        )
        .await
    };

    let root_wide = QuerySpec {
        retrieval_scope: Some(SessionRetrievalScope::AllSessionsInAuthorizedRoot),
        ..QuerySpec::default()
    };
    let (root_wide_digest, _) = digest_for(baseline_context(), root_wide.clone()).await;
    assert_ne!(baseline, root_wide_digest);
    assert_eq!(
        root_wide_digest,
        digest_for(
            baseline_context(),
            QuerySpec {
                session_id: "session.compatibility-anchor.changed",
                ..root_wide
            },
        )
        .await
        .0
    );

    for authorizer in [
        GrantAuthorizer {
            id: "grant.changed",
            revision: 1,
        },
        GrantAuthorizer {
            id: "grant.baseline",
            revision: 2,
        },
    ] {
        let (request_digest, access_digest) = admitted_digests(
            authorizer,
            baseline_context().build(),
            QuerySpec::default().build(),
            Words("words-v1"),
            configuration(),
        )
        .await;
        assert_ne!(baseline, request_digest);
        assert_eq!(baseline_access, access_digest);
    }

    let mut semantic_variants = vec![
        QuerySpec {
            session_id: "session.changed",
            ..QuerySpec::default()
        },
        QuerySpec {
            provider: Some("cursor"),
            ..QuerySpec::default()
        },
        QuerySpec {
            text: "beta",
            ..QuerySpec::default()
        },
    ];
    for temporal_mode in [
        TemporalModeV1::Evolution,
        TemporalModeV1::Forensic,
        TemporalModeV1::AsOf {
            cutoff: UtcMicros(17),
        },
        TemporalModeV1::AsOf {
            cutoff: UtcMicros(18),
        },
    ] {
        semantic_variants.push(QuerySpec {
            temporal_mode,
            ..QuerySpec::default()
        });
    }
    semantic_variants.push(QuerySpec {
        grain: RetrievalGrainV1::Occurrence,
        ..QuerySpec::default()
    });
    semantic_variants.push(QuerySpec {
        limit: 9,
        ..QuerySpec::default()
    });
    semantic_variants.push(QuerySpec {
        freshness_policy: SessionFreshnessPolicy::RequireFresh,
        ..QuerySpec::default()
    });
    for index in 0..5 {
        let mut diversity = DiversityLimits::default();
        match index {
            0 => diversity.per_logical_message += 1,
            1 => diversity.per_turn += 1,
            2 => diversity.per_session += 1,
            3 => diversity.per_source += 1,
            _ => diversity.per_evidence_role += 1,
        }
        semantic_variants.push(QuerySpec {
            diversity,
            ..QuerySpec::default()
        });
    }
    for index in 0..15 {
        let mut limits = ExecutionLimits::default();
        match index {
            0 => limits.candidate_limit += 1,
            1 => limits.candidate_total_bytes -= 1,
            2 => limits.candidate_item_bytes += 1,
            3 => limits.candidate_key_bytes += 1,
            4 => limits.candidate_stable_id_bytes += 1,
            5 => limits.candidate_anchor_id_bytes += 1,
            6 => limits.candidate_metadata_field_bytes += 1,
            7 => limits.record_limit += 1,
            8 => limits.record_total_bytes -= 1,
            9 => limits.record_item_bytes += 1,
            10 => limits.record_key_bytes += 1,
            11 => limits.hydration_limit += 1,
            12 => limits.hydration_total_bytes += 1,
            13 => limits.hydration_payload_bytes += 1,
            _ => limits.hydration_chunk_bytes += 1,
        }
        semantic_variants.push(QuerySpec {
            execution_limits: limits,
            ..QuerySpec::default()
        });
    }
    for context_budget in [
        ContextBudget {
            max_bytes: 64_001,
            ..context_budget()
        },
        ContextBudget {
            max_tokens: 16_001,
            ..context_budget()
        },
        ContextBudget {
            estimator_version: "words-v2".to_owned(),
            ..context_budget()
        },
    ] {
        semantic_variants.push(QuerySpec {
            context_budget,
            ..QuerySpec::default()
        });
    }
    for spec in semantic_variants {
        assert_ne!(baseline, digest_for(baseline_context(), spec).await.0);
    }

    for (budgets, configuration_digest) in [
        (
            RequestBudgets::new(65, 64 * 1024 * 1024, 10_000).unwrap(),
            DIGEST,
        ),
        (
            RequestBudgets::new(64, 64 * 1024 * 1024 + 1, 10_000).unwrap(),
            DIGEST,
        ),
        (
            RequestBudgets::new(64, 64 * 1024 * 1024, 10_001).unwrap(),
            DIGEST,
        ),
        (default_budgets(), [0x5b; 32]),
    ] {
        let context = ContextSpec {
            request_id: "request.semantic",
            budgets,
            configuration: configuration_digest,
            ..baseline_context()
        };
        assert_ne!(baseline, digest_for(context, QuerySpec::default()).await.0);
    }
    for (capability, policy) in [([0x5b; 32], DIGEST), (DIGEST, [0x5b; 32])] {
        let context = ContextSpec {
            request_id: "request.semantic",
            capability,
            policy,
            ..baseline_context()
        };
        let (request_digest, access_digest) = digest_for(context, QuerySpec::default()).await;
        assert_ne!(baseline, request_digest);
        if policy == DIGEST {
            assert_eq!(baseline_access, access_digest);
        } else {
            assert_ne!(baseline_access, access_digest);
        }
    }
    for configuration in [
        SessionRetrievalConfiguration::new(4, 5).unwrap(),
        SessionRetrievalConfiguration::new(3, 6).unwrap(),
    ] {
        assert_ne!(
            baseline,
            admitted_digest(
                BASELINE_GRANT,
                baseline_context().build(),
                QuerySpec::default().build(),
                Words("words-v1"),
                configuration,
            )
            .await
        );
    }
    assert_ne!(
        baseline,
        digest_for(ContextSpec::new("root.two", DIGEST), QuerySpec::default())
            .await
            .0
    );

    let route = |repository: &str, worktree: &str, branch: &str| {
        ResolvedGitRoute::new(
            RepositoryId::new(repository).unwrap(),
            WorktreeId::new(worktree).unwrap(),
            BranchId::new(branch).unwrap(),
        )
    };
    let project = |profile: &str, project: &str, store: &str, git: ResolvedGitRoute| {
        ResolvedSessionIdentity::for_project(
            ProfileId::new(profile).unwrap(),
            ProjectId::new(project).unwrap(),
            SessionStoreId::new(store).unwrap(),
            SessionRootId::new("root.one").unwrap(),
            git,
        )
    };
    let main_route = || {
        route(
            "repository.tracedecay",
            "worktree.main",
            "branch.temporal-application",
        )
    };
    let identity_variants = [
        project(
            "profile.other",
            PROJECT_ID,
            "store.project.tracedecay",
            main_route(),
        ),
        project(
            "profile.primary",
            "project.other",
            "store.project.tracedecay",
            main_route(),
        ),
        project(
            "profile.primary",
            PROJECT_ID,
            "store.project.other",
            main_route(),
        ),
        project(
            "profile.primary",
            PROJECT_ID,
            "store.project.tracedecay",
            route(
                "repository.other",
                "worktree.main",
                "branch.temporal-application",
            ),
        ),
        project(
            "profile.primary",
            PROJECT_ID,
            "store.project.tracedecay",
            route(
                "repository.tracedecay",
                "worktree.other",
                "branch.temporal-application",
            ),
        ),
        project(
            "profile.primary",
            PROJECT_ID,
            "store.project.tracedecay",
            route("repository.tracedecay", "worktree.main", "branch.other"),
        ),
        ResolvedSessionIdentity::for_profile(
            ProfileId::new("profile.primary").unwrap(),
            SessionStoreId::new("store.project.tracedecay").unwrap(),
            SessionRootId::new("root.one").unwrap(),
        ),
    ];
    for identity in identity_variants {
        let context = ContextSpec {
            request_id: "request.identity-semantic",
            identity,
            ..baseline_context()
        };
        assert_ne!(baseline, digest_for(context, QuerySpec::default()).await.0);
    }

    let (other_actor_request, other_actor_access) = digest_for(
        ContextSpec {
            actor: "actor.other",
            request_id: "request.semantic",
            ..baseline_context()
        },
        QuerySpec::default(),
    )
    .await;
    assert_ne!(baseline, other_actor_request);
    assert_eq!(baseline_access, other_actor_access);

    assert_eq!(
        baseline,
        digest_for(
            ContextSpec {
                request_id: "request.ephemeral",
                ..baseline_context()
            },
            QuerySpec {
                cursor: Some("opaque-resume-cursor".to_owned()),
                ..QuerySpec::default()
            },
        )
        .await
        .0
    );
}

#[tokio::test]
async fn denial_never_reaches_temporal_execution_or_payload_hydration() {
    let harness = RegisteredTemporalHarness::open("temporal-denial").await;
    let policy = harness.seed_application_fixture().await;
    let root_wide = || QuerySpec {
        retrieval_scope: Some(SessionRetrievalScope::AllSessionsInAuthorizedRoot),
        ..inline_query()
    };
    let allowed = allow(&harness);
    assert!(
        rendered(
            &retrieve(
                &allowed,
                &context("root.one", policy),
                inline_query().build()
            )
            .await
        )
        .contains(INLINE_PAYLOAD)
    );

    let denied = service(&harness, DenyAuthorizer, Words("words-v1"), configuration());
    for spec in [inline_query(), root_wide()] {
        assert_eq!(
            retrieve(&denied, &context("root.one", policy), spec.build()).await,
            SessionRetrievalOutcome::Denied
        );
    }
}

#[tokio::test]
async fn replayed_grant_cannot_escape_its_deadline_cancellation_or_budgets() {
    let harness = RegisteredTemporalHarness::open("temporal-replayed-grant").await;
    let policy = harness.seed_application_fixture().await;
    let issued = ContextSpec::new("root.one", policy);
    let issued_context = issued.clone().build();
    let authorization = SessionScopeAuthorizationRequest::new(
        issued_context.request.actor().clone(),
        issued_context.binding.identity().clone(),
        SessionId::new("session.temporal.application").unwrap(),
        Some("provider.application".to_owned()),
        TemporalModeV1::Current,
        RetrievalGrainV1::Occurrence,
        SessionAccess::Hydrate,
    )
    .unwrap();
    let grant = AllowAuthorizer
        .authorize(
            &issued_context.request,
            issued_context.binding(),
            &authorization,
        )
        .unwrap();
    let replaying = service(
        &harness,
        ReplayedGrantAuthorizer(grant),
        Words("words-v1"),
        configuration(),
    );
    assert!(
        rendered(&retrieve(&replaying, &issued_context, inline_query().build()).await)
            .contains(INLINE_PAYLOAD)
    );

    let issued_deadline = issued_context.deadline().expires_at;
    let replay_contexts = [
        ContextSpec {
            request_id: "request.replay-deadline",
            cancellation: Some(issued_context.cancellation().clone()),
            expires_at: Some(UtcMicros(issued_deadline.0.saturating_add(1))),
            ..issued.clone()
        },
        ContextSpec {
            request_id: "request.replay-cancellation",
            expires_at: Some(issued_deadline),
            ..issued.clone()
        },
        ContextSpec {
            request_id: "request.replay-budgets",
            cancellation: Some(issued_context.cancellation().clone()),
            expires_at: Some(issued_deadline),
            budgets: RequestBudgets::new(65, 64 * 1024 * 1024, 10_000).unwrap(),
            ..issued
        },
    ];
    for replay in replay_contexts {
        assert_eq!(
            retrieve(&replaying, &replay.build(), inline_query().build()).await,
            SessionRetrievalOutcome::Denied
        );
    }
}

#[tokio::test]
async fn cancellation_or_deadline_during_authorization_prevents_execution_construction() {
    let harness = RegisteredTemporalHarness::open("temporal-authorization-interrupt").await;
    let policy = harness.seed_application_fixture().await;

    let cancelling = service(
        &harness,
        CancellingAuthorizer,
        Words("words-v1"),
        configuration(),
    );
    assert_eq!(
        retrieve(
            &cancelling,
            &context("root.one", policy),
            inline_query().build()
        )
        .await,
        SessionRetrievalOutcome::Cancelled
    );

    let deadline_context = ContextSpec {
        request_id: "request.deadline-during-authorization",
        expires_at: Some(UtcMicros(application_observed_at().0.saturating_add(1_000))),
        ..ContextSpec::new("root.one", policy)
    }
    .build();
    let delaying = service(
        &harness,
        DelayingAuthorizer(Duration::from_millis(10)),
        Words("words-v1"),
        configuration(),
    );
    assert_eq!(
        retrieve(&delaying, &deadline_context, inline_query().build()).await,
        SessionRetrievalOutcome::TimedOut
    );
    assert!(application_observed_at() >= deadline_context.deadline().expires_at);
}

#[tokio::test]
async fn request_budget_preflight_rejects_before_execution() {
    let harness = RegisteredTemporalHarness::open("temporal-budget-preflight").await;
    let policy = harness.seed_application_fixture().await;
    let service = allow(&harness);
    let constrained = ContextSpec {
        request_id: "request.constrained-budget",
        budgets: RequestBudgets::new(1, 64 * 1024 * 1024, 10_000).unwrap(),
        ..ContextSpec::new("root.one", policy)
    }
    .build();

    assert_eq!(
        retrieve(&service, &constrained, inline_query().build()).await,
        SessionRetrievalOutcome::BudgetExhausted {
            stage: SessionRetrievalBudgetStageV1::RequestResultLimit,
            accounting: None,
        }
    );
}

#[tokio::test]
async fn mode_cutoff_is_bound_and_a_forged_cursor_is_denied_by_the_cursor_authority() {
    let as_of = admitted_digest(
        AllowAuthorizer,
        context("root.one", DIGEST),
        QuerySpec {
            temporal_mode: TemporalModeV1::AsOf {
                cutoff: UtcMicros(17),
            },
            ..QuerySpec::default()
        }
        .build(),
        Words("words-v1"),
        configuration(),
    )
    .await;
    let evolution = admitted_digest(
        AllowAuthorizer,
        context("root.one", DIGEST),
        QuerySpec {
            temporal_mode: TemporalModeV1::Evolution,
            ..QuerySpec::default()
        }
        .build(),
        Words("words-v1"),
        configuration(),
    )
    .await;
    assert_ne!(as_of, evolution);

    let harness = RegisteredTemporalHarness::open("temporal-forged-cursor").await;
    let policy = harness.seed_application_fixture().await;
    let service = allow(&harness);
    assert!(
        rendered(
            &retrieve(
                &service,
                &context("root.one", policy),
                inline_query().build()
            )
            .await
        )
        .contains(INLINE_PAYLOAD)
    );
    let forged = QuerySpec {
        cursor: Some("forged".to_owned()),
        ..inline_query()
    };
    assert_eq!(
        admitted_digest(
            AllowAuthorizer,
            context("root.one", policy),
            forged.clone().build(),
            Words("words-v1"),
            configuration(),
        )
        .await,
        admitted_digest(
            AllowAuthorizer,
            context("root.one", policy),
            inline_query().build(),
            Words("words-v1"),
            configuration(),
        )
        .await,
        "the cursor is resume ephemera, not a request input"
    );
    assert_eq!(
        retrieve(&service, &context("root.one", policy), forged.build()).await,
        SessionRetrievalOutcome::Denied
    );
}

#[tokio::test]
async fn cancelled_request_never_executes_in_either_scope() {
    let harness = RegisteredTemporalHarness::open("temporal-precancelled").await;
    let policy = harness.seed_application_fixture().await;
    let service = allow(&harness);
    let cancelled = context("root.one", policy);
    cancelled.cancellation().cancel();
    for spec in [
        inline_query(),
        QuerySpec {
            retrieval_scope: Some(SessionRetrievalScope::AllSessionsInAuthorizedRoot),
            ..inline_query()
        },
    ] {
        assert_eq!(
            retrieve(&service, &cancelled, spec.build()).await,
            SessionRetrievalOutcome::Cancelled
        );
    }
    assert!(
        rendered(
            &retrieve(
                &service,
                &context("root.one", policy),
                inline_query().build()
            )
            .await
        )
        .contains(INLINE_PAYLOAD)
    );
}

/// A paged root read cancelled after its first page is observed settles as
/// `Cancelled` and leaves the continuation it already issued intact: a fresh
/// request resumes from that cursor to the remaining page.
#[tokio::test]
async fn cancellation_after_the_first_page_keeps_the_issued_continuation() {
    let harness = RegisteredTemporalHarness::open("temporal-mid-stream-cancel").await;
    let policy = harness.seed_root_fixture().await;
    let service = allow(&harness);

    let first = retrieve(&service, &context("root.one", policy), root_page(1, None)).await;
    let SessionRetrievalOutcome::Partial { items, omitted, .. } = first else {
        panic!("first root page was not partial: {first:?}");
    };
    assert_eq!(omitted, 0);
    let first_anchor = items[0].ranked[0].anchor_id.clone();
    let cursor = items[0].next_cursor.clone().expect("root continuation");

    let cancelled = ContextSpec {
        request_id: "request.cancelled-continuation",
        ..ContextSpec::new("root.one", policy)
    }
    .build();
    cancelled.cancellation().cancel();
    assert_eq!(
        retrieve(&service, &cancelled, root_page(1, Some(cursor.clone()))).await,
        SessionRetrievalOutcome::Cancelled
    );

    let resumed_context = ContextSpec {
        request_id: "request.resumed-continuation",
        ..ContextSpec::new("root.one", policy)
    }
    .build();
    let resumed = retrieve(&service, &resumed_context, root_page(1, Some(cursor))).await;
    let SessionRetrievalOutcome::Complete { items, .. } = resumed else {
        panic!("resumed root page was not complete: {resumed:?}");
    };
    assert_ne!(items[0].ranked[0].anchor_id, first_anchor);
}

/// The execution future is left pending on the registered reader pool the
/// test holds, then dropped by cancellation. Nothing it started survives: no
/// reader lease or waiter remains once the pool is released, the store bytes
/// are unchanged, and the same service answers the next request.
#[tokio::test]
async fn pending_execution_dropped_on_cancellation_leaves_no_partial_state() {
    let harness = RegisteredTemporalHarness::open("temporal-pending-drop").await;
    let policy = harness.seed_application_fixture().await;
    let before = Sha256::digest(std::fs::read(harness.registered.db_path()).unwrap());
    let occupancy = || {
        harness
            .registered
            .read_connection()
            .reader_pool_occupancy()
            .expect("registered reader pool")
    };
    let mut held = Vec::new();
    for _ in 0..DEFAULT_MAX_READERS_PER_HOT_SHARD {
        held.push(
            harness
                .registered
                .read_snapshot()
                .await
                .expect("hold a registered reader"),
        );
    }
    assert_eq!(
        occupancy().leased_general,
        DEFAULT_MAX_READERS_PER_HOT_SHARD
    );

    let service = allow(&harness);
    let pending = context("root.one", policy);
    let cancellation = pending.cancellation().clone();
    let cancel_once_queued = async {
        tokio::time::timeout(Duration::from_secs(30), async {
            while occupancy().waiting_general == 0 {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .expect("execution queues behind the held readers");
        cancellation.cancel();
    };
    let (outcome, ()) = tokio::join!(
        retrieve(&service, &pending, inline_query().build()),
        cancel_once_queued
    );
    assert_eq!(outcome, SessionRetrievalOutcome::Cancelled);

    drop(held);
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let pool = occupancy();
            if pool.leased_general == 0 && pool.waiting_general == 0 && pool.limbo_general == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("the dropped execution releases every reader it queued for");
    assert_eq!(
        Sha256::digest(std::fs::read(harness.registered.db_path()).unwrap()),
        before
    );
    let fresh = ContextSpec {
        request_id: "request.after-drop",
        ..ContextSpec::new("root.one", policy)
    }
    .build();
    assert!(
        rendered(&retrieve(&service, &fresh, inline_query().build()).await)
            .contains(INLINE_PAYLOAD)
    );
}

/// The external payload file is rewritten under the store between two reads
/// of the same occurrence: the first read proves and renders the original
/// bytes, the second refuses the drifted file as a typed outcome and renders
/// none of it.
#[tokio::test]
async fn external_payload_rewritten_between_reads_is_a_typed_refusal() {
    let harness = RegisteredTemporalHarness::open("temporal-payload-drift").await;
    let policy = harness.seed_application_fixture().await;
    let service = allow(&harness);
    let external = || {
        QuerySpec {
            text: "external",
            ..inline_query()
        }
        .build()
    };
    assert!(
        rendered(&retrieve(&service, &context("root.one", policy), external()).await)
            .contains(EXTERNAL_PAYLOAD)
    );

    let payload_path = harness.application_external_payload_path();
    let original = std::fs::read(&payload_path).unwrap();
    let drifted = original.iter().rev().copied().collect::<Vec<_>>();
    assert_eq!(drifted.len(), original.len());
    std::fs::write(&payload_path, &drifted).unwrap();

    let second_read = ContextSpec {
        request_id: "request.drifted-payload",
        ..ContextSpec::new("root.one", policy)
    }
    .build();
    let outcome = retrieve(&service, &second_read, external()).await;
    assert_eq!(outcome, SessionRetrievalOutcome::Unavailable, "{outcome:?}");
}
