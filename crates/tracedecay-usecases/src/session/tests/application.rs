use std::collections::BTreeSet;

use sha2::{Digest, Sha256};
use tracedecay_application::{
    CancellationContext, CapabilityGrantId, CapabilityGrantSnapshot, Deadline, DisclosureClass,
    RequestContext, RequestId,
};
use tracedecay_domain::{
    ActorId, ProjectId, RepositoryId, RetrievalGrainV1, SessionId, TemporalModeV1, UtcMicros,
    WorktreeId,
};
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

use super::harness::{EXTERNAL_PAYLOAD, INLINE_PAYLOAD, PROJECT_ID, RegisteredTemporalHarness};
use crate::context::{
    BranchId, CancellationToken, CapabilityDigest, ConfigurationDigest, PolicyDigest, ProfileId,
    RequestBudgets, ResolvedGitRoute, ResolvedSessionIdentity, SessionRootId, SessionStoreId,
    application_observed_at, session_application_grant_digest,
};
use crate::session::{
    AuthorizationGrantId, SessionAuthorizationError, SessionAuthorizationGrant,
    SessionRequestBinding, SessionRetrievalConfiguration, SessionRetrievalOutcome,
    SessionRetrievalScope, SessionRetrievalService, SessionScopeAuthorizationRequest,
    SessionScopeAuthorizer, SessionTemporalQuery,
};
use tracedecay_global_db::session_temporal::RegisteredGlobalDbSessionTemporalExecution;
use tracedecay_temporal_query::context::{ContextBudget, TokenPolicy, VersionedTokenEstimator};
use tracedecay_temporal_query::ranking::DiversityLimits;

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

#[derive(Clone, Copy)]
struct Words;

impl VersionedTokenEstimator for Words {
    fn version(&self) -> &'static str {
        "words-v1"
    }

    fn token_policy(&self) -> TokenPolicy {
        TokenPolicy::Whitespace
    }
}

#[tokio::test]
async fn registered_root_scope_isolated_and_provider_filtered() {
    let harness = RegisteredTemporalHarness::open("registered-root-filter").await;
    let policy_digest = harness.seed_root_fixture().await;
    let before = Sha256::digest(std::fs::read(harness.registered.db_path()).unwrap());
    let (context, binding) = request_context("root.one", "request.registered-root", policy_digest);
    let execution = RegisteredGlobalDbSessionTemporalExecution::new(harness.registered.as_ref());
    let service = SessionRetrievalService::new(
        AllowAuthorizer,
        &execution,
        Words,
        SessionRetrievalConfiguration::new(3, 5).unwrap(),
    );

    let all = service
        .retrieve(
            &context,
            &binding,
            root_query("session.root.a", None, None, 8),
        )
        .await;
    let SessionRetrievalOutcome::Complete { items, .. } = all else {
        panic!("root-wide retrieval was not complete: {all:?}");
    };
    assert_eq!(items[0].ranked.len(), 2);
    assert_eq!(items[0].coverage.visible, 2);
    assert_eq!(items[0].coverage.unknown, 0);
    assert!(items[0].lineage.is_empty());
    assert!(items[0].context.rendered.contains("session alpha"));
    assert!(items[0].context.rendered.contains("session beta"));
    let all_anchors = items[0]
        .ranked
        .iter()
        .map(|item| item.anchor_id.to_string())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        items[0]
            .ranked
            .iter()
            .filter_map(|item| item.session.as_deref())
            .collect::<BTreeSet<_>>(),
        ["session.root.a", "session.root.b"].into()
    );
    assert!(
        items[0]
            .ranked
            .iter()
            .all(|item| item.session.as_deref() != Some("session.root.foreign"))
    );
    assert_eq!(
        items[0]
            .hydrated
            .iter()
            .filter_map(|item| item.content())
            .map(|content| std::str::from_utf8(content).unwrap())
            .collect::<BTreeSet<_>>(),
        [
            "root-wide payload from session alpha",
            "root-wide payload from session beta",
        ]
        .into()
    );

    let filtered = service
        .retrieve(
            &context,
            &binding,
            root_query("session.root.a", Some("provider.application"), None, 8),
        )
        .await;
    let SessionRetrievalOutcome::Complete { items, .. } = filtered else {
        panic!("provider-filtered root retrieval was not complete: {filtered:?}");
    };
    assert_eq!(items[0].ranked.len(), 1);
    assert_eq!(
        items[0].ranked[0].session.as_deref(),
        Some("session.root.a")
    );
    let filtered_anchor = items[0].ranked[0].anchor_id.clone();
    let exact = service
        .retrieve(
            &context,
            &binding,
            SessionTemporalQuery::new(
                SessionId::new("session.root.a").unwrap(),
                Some("provider.application".to_owned()),
                "root-wide",
                None,
                TemporalModeV1::Current,
                RetrievalGrainV1::Occurrence,
                8,
                DiversityLimits::unbounded(),
                context_budget(),
            )
            .unwrap(),
        )
        .await;
    let SessionRetrievalOutcome::Complete { items, .. } = exact else {
        panic!("exact-session retrieval was not complete: {exact:?}");
    };
    assert_eq!(items[0].ranked[0].anchor_id, filtered_anchor);
    assert_eq!(all_anchors.len(), 2);
    assert_eq!(
        Sha256::digest(std::fs::read(harness.registered.db_path()).unwrap()),
        before
    );
}

#[tokio::test]
async fn registered_root_cursor_survives_service_restart_and_rejects_scope_drift() {
    let harness = RegisteredTemporalHarness::open("registered-root-restart").await;
    let policy_digest = harness.seed_root_fixture().await;
    let (context, binding) =
        request_context("root.one", "request.registered-restart", policy_digest);
    let execution = RegisteredGlobalDbSessionTemporalExecution::new(harness.registered.as_ref());
    let service = SessionRetrievalService::new(
        AllowAuthorizer,
        &execution,
        Words,
        SessionRetrievalConfiguration::new(3, 5).unwrap(),
    );
    let first = service
        .retrieve(
            &context,
            &binding,
            root_query("session.root.a", None, None, 1),
        )
        .await;
    let SessionRetrievalOutcome::Partial { items, omitted, .. } = first else {
        panic!("first root page was not partial: {first:?}");
    };
    assert_eq!(omitted, 1);
    let first_anchor = items[0].ranked[0].anchor_id.to_string();
    let cursor = items[0].next_cursor.clone().expect("root continuation");

    let restarted_execution =
        RegisteredGlobalDbSessionTemporalExecution::new(harness.registered.as_ref());
    let restarted = SessionRetrievalService::new(
        AllowAuthorizer,
        &restarted_execution,
        Words,
        SessionRetrievalConfiguration::new(3, 5).unwrap(),
    );
    let resumed = restarted
        .retrieve(
            &context,
            &binding,
            root_query("session.root.b", None, Some(cursor.clone()), 1),
        )
        .await;
    let SessionRetrievalOutcome::Complete { items, .. } = resumed else {
        panic!("resumed root page was not complete: {resumed:?}");
    };
    let resumed_anchor = items[0].ranked[0].anchor_id.to_string();
    assert_eq!(
        [first_anchor, resumed_anchor]
            .into_iter()
            .collect::<BTreeSet<_>>()
            .len(),
        2
    );
    assert!(matches!(
        restarted
            .retrieve(
                &context,
                &binding,
                SessionTemporalQuery::new(
                    SessionId::new("session.root.a").unwrap(),
                    None,
                    "duplicate",
                    Some(cursor.clone()),
                    TemporalModeV1::Current,
                    RetrievalGrainV1::Occurrence,
                    1,
                    DiversityLimits::unbounded(),
                    context_budget(),
                )
                .unwrap(),
            )
            .await,
        SessionRetrievalOutcome::WrongScope
    ));
    let (other_context, other_binding) =
        request_context("root.other", "request.scope-drift", policy_digest);
    assert!(matches!(
        restarted
            .retrieve(
                &other_context,
                &other_binding,
                root_query("session.root.b", None, Some(cursor), 1),
            )
            .await,
        SessionRetrievalOutcome::WrongScope
    ));
}

#[tokio::test]
async fn registered_occurrence_hydrates_inline_and_external_payloads_without_writes() {
    let harness = RegisteredTemporalHarness::open("registered-hydration").await;
    let policy_digest = harness.seed_application_fixture().await;
    let before_db = Sha256::digest(std::fs::read(harness.registered.db_path()).unwrap());
    let payload_path = harness.application_external_payload_path();
    let before_payload = Sha256::digest(std::fs::read(&payload_path).unwrap());
    let execution = RegisteredGlobalDbSessionTemporalExecution::new(harness.registered.as_ref());
    let service = SessionRetrievalService::new(
        AllowAuthorizer,
        &execution,
        Words,
        SessionRetrievalConfiguration::new(3, 5).unwrap(),
    );
    let (context, binding) =
        request_context("root.one", "request.registered-hydration", policy_digest);
    for temporal_mode in [
        TemporalModeV1::Current,
        TemporalModeV1::AsOf {
            cutoff: tracedecay_domain::UtcMicros(10),
        },
        TemporalModeV1::Evolution,
        TemporalModeV1::Forensic,
    ] {
        let outcome = service
            .retrieve(
                &context,
                &binding,
                occurrence_query("inline", temporal_mode),
            )
            .await;
        let SessionRetrievalOutcome::Complete { items, .. } = outcome else {
            panic!("registered hydration was not complete: {outcome:?}");
        };
        assert_eq!(items[0].ranked.len(), 1);
        assert!(items[0].context.rendered.contains(INLINE_PAYLOAD));
    }
    let (external_context, external_binding) = request_context(
        "root.one",
        "request.registered-external-hydration",
        policy_digest,
    );
    let external = service
        .retrieve(
            &external_context,
            &external_binding,
            occurrence_query("external", TemporalModeV1::Current),
        )
        .await;
    let SessionRetrievalOutcome::Complete { items, .. } = external else {
        panic!("registered external hydration was not complete: {external:?}");
    };
    assert_eq!(items[0].ranked.len(), 1);
    assert!(
        items[0].context.rendered.contains(EXTERNAL_PAYLOAD),
        "{}",
        items[0].context.rendered
    );
    assert_eq!(
        Sha256::digest(std::fs::read(harness.registered.db_path()).unwrap()),
        before_db
    );
    assert_eq!(
        Sha256::digest(std::fs::read(payload_path).unwrap()),
        before_payload
    );
}

#[tokio::test]
async fn registered_complete_zero_preserves_the_authoritative_store() {
    let harness = RegisteredTemporalHarness::open("registered-complete-zero").await;
    let policy_digest = harness.seed_empty_fixture().await;
    let before = Sha256::digest(std::fs::read(harness.registered.db_path()).unwrap());
    let execution = RegisteredGlobalDbSessionTemporalExecution::new(harness.registered.as_ref());
    let service = SessionRetrievalService::new(
        AllowAuthorizer,
        &execution,
        Words,
        SessionRetrievalConfiguration::new(3, 5).unwrap(),
    );
    let (context, binding) = request_context("root.one", "request.complete-zero", policy_digest);
    assert!(matches!(
        service
            .retrieve(
                &context,
                &binding,
                occurrence_query("absent", TemporalModeV1::Current),
            )
            .await,
        SessionRetrievalOutcome::CompleteZero { .. }
    ));
    assert_eq!(
        Sha256::digest(std::fs::read(harness.registered.db_path()).unwrap()),
        before
    );
}

fn request_context(
    root: &str,
    request: &str,
    policy_digest: [u8; 32],
) -> (RequestContext, SessionRequestBinding) {
    let actor = ActorId::new("actor.cursor").unwrap();
    let request_id = RequestId::new(request).unwrap();
    let identity = ResolvedSessionIdentity::for_project(
        ProfileId::new("profile.primary").unwrap(),
        ProjectId::new(PROJECT_ID).unwrap(),
        SessionStoreId::new("store.project.tracedecay").unwrap(),
        SessionRootId::new(root).unwrap(),
        ResolvedGitRoute::new(
            RepositoryId::new("repository.tracedecay").unwrap(),
            WorktreeId::new("worktree.main").unwrap(),
            BranchId::new("branch.temporal-application").unwrap(),
        ),
    );
    let scope = identity.application_scope().unwrap();
    let capability = CapabilityDigest::new(DIGEST);
    let policy = PolicyDigest::new(policy_digest);
    let configuration = ConfigurationDigest::new(DIGEST);
    let cancellation = CancellationToken::for_application_request(&request_id);
    let budgets = RequestBudgets::new(64, 64 * 1024 * 1024, 10_000).unwrap();
    let observed_at = application_observed_at();
    let expires_at = UtcMicros(observed_at.0.saturating_add(30_000_000));
    let grant = CapabilityGrantSnapshot::new(
        CapabilityGrantId::new("grant.temporal.application.test").unwrap(),
        1,
        session_application_grant_digest(capability, policy, configuration, &cancellation, budgets)
            .unwrap(),
        actor.clone(),
        observed_at,
        expires_at,
        scope.clone(),
        BTreeSet::from([CapabilityId::new("capability.session.temporal-retrieval").unwrap()]),
        BTreeSet::from([UseCaseId::new("use-case.session.temporal-application-test").unwrap()]),
        DisclosureClass::Evidence,
    )
    .unwrap();
    let context = RequestContext::new(
        actor,
        scope,
        grant,
        request_id,
        Deadline::new(expires_at).unwrap(),
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

fn root_query(
    anchor_session: &str,
    provider: Option<&str>,
    cursor: Option<String>,
    limit: usize,
) -> SessionTemporalQuery {
    SessionTemporalQuery::new(
        SessionId::new(anchor_session).unwrap(),
        provider.map(str::to_owned),
        "root-wide",
        cursor,
        TemporalModeV1::Current,
        RetrievalGrainV1::Occurrence,
        limit,
        DiversityLimits::unbounded(),
        context_budget(),
    )
    .unwrap()
    .with_retrieval_scope(SessionRetrievalScope::AllSessionsInAuthorizedRoot)
}

fn occurrence_query(text: &str, temporal_mode: TemporalModeV1) -> SessionTemporalQuery {
    SessionTemporalQuery::new(
        SessionId::new("session.temporal.application").unwrap(),
        Some("provider.application".to_owned()),
        text,
        None,
        temporal_mode,
        RetrievalGrainV1::Occurrence,
        8,
        DiversityLimits::default(),
        context_budget(),
    )
    .unwrap()
}

fn context_budget() -> ContextBudget {
    ContextBudget {
        max_bytes: 64_000,
        max_tokens: 16_000,
        estimator_version: "words-v1".to_owned(),
    }
}
