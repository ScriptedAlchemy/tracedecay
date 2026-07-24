use std::time::{Duration, Instant};

use tracedecay_domain::{
    ActorId, ProjectId, RepositoryId, RetrievalGrainV1, SessionId, TemporalModeV1, WorktreeId,
};

use super::harness::{PRIVACY_CANARY, PROJECT_ID, RegisteredTemporalHarness, SAFE_PRIVACY_PAYLOAD};
use crate::application::context::{
    BranchId, CancellationToken, CapabilityDigest, ConfigurationDigest, MonotonicDeadline,
    PolicyDigest, ProfileId, RequestBudgets, RequestContext, RequestId, ResolvedGitRoute,
    ResolvedSessionIdentity, SessionRootId, SessionStoreId,
};
use crate::application::session::{
    AuthorizationGrantId, SessionAuthorizationError, SessionAuthorizationGrant,
    SessionDataFreshness, SessionRetrievalConfiguration, SessionRetrievalOutcome,
    SessionRetrievalService, SessionScopeAuthorizationRequest, SessionScopeAuthorizer,
    SessionTemporalQuery,
};
use crate::global_db::session_temporal::RegisteredGlobalDbSessionTemporalExecution;
use crate::query::temporal::context::{ContextBudget, TokenPolicy, VersionedTokenEstimator};
use crate::query::temporal::ranking::DiversityLimits;

const DIGEST: [u8; 32] = [0x5a; 32];

struct AllowAuthorizer;

impl SessionScopeAuthorizer for AllowAuthorizer {
    fn authorize(
        &self,
        context: &RequestContext,
        request: &SessionScopeAuthorizationRequest,
    ) -> Result<SessionAuthorizationGrant, SessionAuthorizationError> {
        SessionAuthorizationGrant::issue(
            AuthorizationGrantId::new("grant.temporal.privacy").unwrap(),
            1,
            context,
            request,
        )
    }
}

struct DenyAuthorizer;

impl SessionScopeAuthorizer for DenyAuthorizer {
    fn authorize(
        &self,
        _context: &RequestContext,
        _request: &SessionScopeAuthorizationRequest,
    ) -> Result<SessionAuthorizationGrant, SessionAuthorizationError> {
        Err(SessionAuthorizationError::Denied)
    }
}

#[derive(Clone, Copy)]
struct Words;

impl VersionedTokenEstimator for Words {
    fn version(&self) -> &str {
        "privacy-words-v1"
    }

    fn token_policy(&self) -> TokenPolicy {
        TokenPolicy::Whitespace
    }
}

#[tokio::test]
async fn registered_authorized_retrieval_returns_only_sanitized_context() {
    let harness = RegisteredTemporalHarness::open("registered-privacy-authorized").await;
    let policy_digest = harness.seed_privacy_fixture().await;
    let execution = RegisteredGlobalDbSessionTemporalExecution::new(harness.registered.as_ref());
    let service = SessionRetrievalService::new(
        AllowAuthorizer,
        &execution,
        Words,
        SessionRetrievalConfiguration::new(3, 5).unwrap(),
    );
    let outcome = service
        .retrieve(&request_context(policy_digest), privacy_query())
        .await;
    let SessionRetrievalOutcome::Complete { items, freshness } = &outcome else {
        panic!("authorized registered retrieval was not complete: {outcome:?}");
    };
    assert_eq!(*freshness, SessionDataFreshness::Fresh);
    assert_eq!(items[0].ranked.len(), 1);
    let ranked = &items[0].ranked[0];
    assert!(
        !ranked.contributions.is_empty()
            && ranked
                .contributions
                .iter()
                .all(|contribution| !contribution.retriever_record_id.is_empty())
    );
    let assembled = items[0]
        .context
        .bundle
        .records
        .iter()
        .find(|record| record.anchor_id == ranked.anchor_id)
        .expect("registered context retains the ranked occurrence");
    assert_eq!(assembled.grain, RetrievalGrainV1::Occurrence);
    assert!(items[0].context.rendered.contains(SAFE_PRIVACY_PAYLOAD));
    assert!(!format!("{outcome:?}").contains(PRIVACY_CANARY));
}

#[tokio::test]
async fn registered_denied_retrieval_never_exposes_private_context() {
    let harness = RegisteredTemporalHarness::open("registered-privacy-denied").await;
    let policy_digest = harness.seed_privacy_fixture().await;
    let execution = RegisteredGlobalDbSessionTemporalExecution::new(harness.registered.as_ref());
    let service = SessionRetrievalService::new(
        DenyAuthorizer,
        &execution,
        Words,
        SessionRetrievalConfiguration::new(3, 5).unwrap(),
    );
    let outcome = service
        .retrieve(&request_context(policy_digest), privacy_query())
        .await;
    assert!(matches!(outcome, SessionRetrievalOutcome::Denied));
    assert!(!format!("{outcome:?}").contains(PRIVACY_CANARY));
}

#[tokio::test]
async fn registered_quarantined_legacy_source_never_enters_temporal_sinks() {
    let harness = RegisteredTemporalHarness::open("registered-privacy-quarantine").await;
    harness.seed_quarantined_legacy_fixture().await;
    assert_eq!(
        harness
            .count(
                "SELECT COUNT(*) FROM session_occurrences_fts
                 WHERE session_occurrences_fts MATCH '\"sk-proj-private-canary\"'",
            )
            .await,
        0
    );
    assert_eq!(
        harness
            .count(
                "SELECT COUNT(*) FROM session_summary_nodes_fts
                 WHERE session_summary_nodes_fts MATCH '\"sk-proj-private-canary\"'",
            )
            .await,
        0
    );
}

#[tokio::test]
async fn registered_sanitized_temporal_state_is_stable_across_execution_replay() {
    let harness = RegisteredTemporalHarness::open("registered-privacy-replay").await;
    let policy_digest = harness.seed_privacy_fixture().await;
    let context = request_context(policy_digest);
    let first_execution =
        RegisteredGlobalDbSessionTemporalExecution::new(harness.registered.as_ref());
    let first_service = SessionRetrievalService::new(
        AllowAuthorizer,
        &first_execution,
        Words,
        SessionRetrievalConfiguration::new(3, 5).unwrap(),
    );
    let first = first_service.retrieve(&context, privacy_query()).await;
    drop(first_service);
    let replay_execution =
        RegisteredGlobalDbSessionTemporalExecution::new(harness.registered.as_ref());
    let replay_service = SessionRetrievalService::new(
        AllowAuthorizer,
        &replay_execution,
        Words,
        SessionRetrievalConfiguration::new(3, 5).unwrap(),
    );
    let replay = replay_service.retrieve(&context, privacy_query()).await;
    let (
        SessionRetrievalOutcome::Complete {
            items: first_items, ..
        },
        SessionRetrievalOutcome::Complete {
            items: replay_items,
            ..
        },
    ) = (&first, &replay)
    else {
        panic!("registered privacy replay was not complete: {first:?} / {replay:?}");
    };
    assert_eq!(
        first_items[0].ranked[0].anchor_id,
        replay_items[0].ranked[0].anchor_id
    );
    assert_eq!(
        first_items[0].context.rendered,
        replay_items[0].context.rendered
    );
    assert!(!format!("{first:?}{replay:?}").contains(PRIVACY_CANARY));
}

fn request_context(policy_digest: [u8; 32]) -> RequestContext {
    RequestContext::new(
        ActorId::new("actor.temporal.privacy").unwrap(),
        RequestId::new("request.temporal.privacy").unwrap(),
        ResolvedSessionIdentity::for_project(
            ProfileId::new("profile.primary").unwrap(),
            ProjectId::new(PROJECT_ID).unwrap(),
            SessionStoreId::new("store.project.tracedecay").unwrap(),
            SessionRootId::new("root.one").unwrap(),
            ResolvedGitRoute::new(
                RepositoryId::new("repository.tracedecay").unwrap(),
                WorktreeId::new("worktree.main").unwrap(),
                BranchId::new("branch.temporal-privacy").unwrap(),
            ),
        ),
        CapabilityDigest::new(DIGEST),
        PolicyDigest::new(policy_digest),
        ConfigurationDigest::new(DIGEST),
        MonotonicDeadline::at(Instant::now() + Duration::from_secs(30)),
        CancellationToken::new(),
        RequestBudgets::new(64, 64 * 1024 * 1024, 10_000).unwrap(),
    )
}

fn privacy_query() -> SessionTemporalQuery {
    SessionTemporalQuery::new(
        SessionId::new("session.temporal.privacy").unwrap(),
        Some("codex".to_owned()),
        "billing",
        None,
        TemporalModeV1::Current,
        RetrievalGrainV1::Occurrence,
        8,
        DiversityLimits::default(),
        ContextBudget {
            max_bytes: 64_000,
            max_tokens: 16_000,
            estimator_version: "privacy-words-v1".to_owned(),
        },
    )
    .unwrap()
}
