use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use sha2::{Digest, Sha256};
use tracedecay_application::{
    CancellationContext, CapabilityGrantId, CapabilityGrantSnapshot, Deadline, DisclosureClass,
    RequestContext, RequestId,
};
use tracedecay_domain::{
    ActorId, HydrationStateV1, ProjectId, RepositoryId, RetrievalAnchorId, RetrievalGrainV1,
    SessionId, TemporalModeV1, UtcMicros, WorktreeId,
};
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

use super::harness::{
    EXTERNAL_PAYLOAD, INLINE_PAYLOAD, PRIVACY_CANARY, PROJECT_ID, RegisteredTemporalHarness,
    SAFE_PRIVACY_PAYLOAD,
};
use crate::context::{
    BranchId, CancellationToken, CapabilityDigest, ConfigurationDigest, PolicyDigest, ProfileId,
    RequestBudgets, ResolvedGitRoute, ResolvedSessionIdentity, SessionRootId, SessionStoreId,
    application_observed_at, session_application_grant_digest,
};
use crate::session::{
    AuthorizationGrantId, SessionAuthorizationError, SessionAuthorizationGrant,
    SessionDataFreshness, SessionRequestBinding, SessionRetrievalConfiguration,
    SessionRetrievalOutcome, SessionRetrievalService, SessionScopeAuthorizationRequest,
    SessionScopeAuthorizer, SessionTemporalQuery,
};
use tracedecay_global_db::session_temporal::RegisteredGlobalDbSessionTemporalExecution;
use tracedecay_sessions::runtime::lcm::{
    LcmContentSlice, LcmDescribeRequest, LcmDescribeTarget, LcmExpandRequest, LcmExpandTarget,
};
use tracedecay_temporal_query::TemporalKernelResult;
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
            AuthorizationGrantId::new("grant.temporal.privacy").unwrap(),
            1,
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

#[derive(Clone)]
struct RevocableAuthorizer {
    authorized: Arc<AtomicBool>,
}

impl RevocableAuthorizer {
    fn new() -> Self {
        Self {
            authorized: Arc::new(AtomicBool::new(true)),
        }
    }

    fn allow(&self) {
        self.authorized.store(true, Ordering::SeqCst);
    }

    fn revoke(&self) {
        self.authorized.store(false, Ordering::SeqCst);
    }
}

impl SessionScopeAuthorizer for RevocableAuthorizer {
    fn authorize(
        &self,
        context: &RequestContext,
        binding: &SessionRequestBinding,
        request: &SessionScopeAuthorizationRequest,
    ) -> Result<SessionAuthorizationGrant, SessionAuthorizationError> {
        if !self.authorized.load(Ordering::SeqCst) {
            return Err(SessionAuthorizationError::Denied);
        }
        SessionAuthorizationGrant::issue(
            AuthorizationGrantId::new("grant.temporal.revocable").unwrap(),
            2,
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
    let (context, binding) = request_context(policy_digest);
    let outcome = service.retrieve(&context, &binding, privacy_query()).await;
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
    let (context, binding) = request_context(policy_digest);
    let outcome = service.retrieve(&context, &binding, privacy_query()).await;
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

/// Naming two sinks only proves those two are clean. A sink added later — a
/// new occurrence, summary, or fact index — would carry the quarantined text
/// with nothing complaining, so sweep every full-text sink the schema defines.
#[tokio::test]
async fn registered_quarantined_legacy_source_reaches_no_full_text_sink() {
    let harness = RegisteredTemporalHarness::open("registered-privacy-sink-sweep").await;
    harness.seed_quarantined_legacy_fixture().await;

    let sinks = harness.full_text_sinks().await;
    assert!(
        !sinks.is_empty(),
        "the schema must define full-text sinks for this sweep to mean anything"
    );
    for sink in sinks {
        assert_eq!(
            harness
                .count(&format!(
                    "SELECT COUNT(*) FROM {sink} WHERE {sink} MATCH '\"{PRIVACY_CANARY}\"'"
                ))
                .await,
            0,
            "quarantined legacy text reached the {sink} full-text sink"
        );
    }
}

/// Replaying through a fresh execution object reuses the open store. Privacy
/// also has to survive losing the handle entirely and remounting, which is
/// what every daemon restart does to a live user store.
#[tokio::test]
async fn registered_sanitized_temporal_state_stays_private_across_reopen() {
    let harness = RegisteredTemporalHarness::open("registered-privacy-reopen").await;
    let policy_digest = harness.seed_privacy_fixture().await;
    let (context, binding) = request_context(policy_digest);

    let execution = RegisteredGlobalDbSessionTemporalExecution::new(harness.registered.as_ref());
    let service = SessionRetrievalService::new(
        AllowAuthorizer,
        &execution,
        Words,
        SessionRetrievalConfiguration::new(3, 5).unwrap(),
    );
    let before = service.retrieve(&context, &binding, privacy_query()).await;

    let remounted = harness.remount().await;
    let reopened_execution = RegisteredGlobalDbSessionTemporalExecution::new(remounted.as_ref());
    let reopened_service = SessionRetrievalService::new(
        AllowAuthorizer,
        &reopened_execution,
        Words,
        SessionRetrievalConfiguration::new(3, 5).unwrap(),
    );
    let after = reopened_service
        .retrieve(&context, &binding, privacy_query())
        .await;

    let (
        SessionRetrievalOutcome::Complete {
            items: before_items,
            ..
        },
        SessionRetrievalOutcome::Complete {
            items: after_items, ..
        },
    ) = (&before, &after)
    else {
        panic!("privacy retrieval across reopen was not complete: {before:?} / {after:?}");
    };
    assert_eq!(
        before_items[0].context.rendered, after_items[0].context.rendered,
        "a reopen must not change what the sanitized context renders"
    );
    assert!(
        after_items[0]
            .context
            .rendered
            .contains(SAFE_PRIVACY_PAYLOAD)
    );
    assert!(
        !format!("{after:?}").contains(PRIVACY_CANARY),
        "the canary must not surface after a reopen"
    );
}

#[tokio::test]
async fn registered_sanitized_temporal_state_is_stable_across_execution_replay() {
    let harness = RegisteredTemporalHarness::open("registered-privacy-replay").await;
    let policy_digest = harness.seed_privacy_fixture().await;
    let (context, binding) = request_context(policy_digest);
    let first_execution =
        RegisteredGlobalDbSessionTemporalExecution::new(harness.registered.as_ref());
    let first_service = SessionRetrievalService::new(
        AllowAuthorizer,
        &first_execution,
        Words,
        SessionRetrievalConfiguration::new(3, 5).unwrap(),
    );
    let first = first_service
        .retrieve(&context, &binding, privacy_query())
        .await;
    let replay_execution =
        RegisteredGlobalDbSessionTemporalExecution::new(harness.registered.as_ref());
    let replay_service = SessionRetrievalService::new(
        AllowAuthorizer,
        &replay_execution,
        Words,
        SessionRetrievalConfiguration::new(3, 5).unwrap(),
    );
    let replay = replay_service
        .retrieve(&context, &binding, privacy_query())
        .await;
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

#[tokio::test]
async fn registered_lcm_describe_expand_and_expand_query_reauthorize_without_storage_mutation() {
    let harness = RegisteredTemporalHarness::open("registered-lcm-read-authorization").await;
    let policy_digest = harness.seed_application_fixture().await;
    let raw_store_id = harness.raw_store_id("message-1").await;
    let baseline = storage_fingerprint(&harness);
    let (context, binding) = request_context(policy_digest);
    let authorizer = RevocableAuthorizer::new();
    let execution = RegisteredGlobalDbSessionTemporalExecution::new(harness.registered.as_ref());
    let service = SessionRetrievalService::new(
        authorizer.clone(),
        &execution,
        Words,
        SessionRetrievalConfiguration::new(3, 5).unwrap(),
    );

    let describe_query = application_query(
        "",
        None,
        TemporalModeV1::Forensic,
        RetrievalGrainV1::Session,
        1,
    )
    .with_compatibility_filter_digest("surface.describe.v1".to_owned());
    assert_surface_completed(
        &service
            .retrieve(&context, &binding, describe_query.clone())
            .await,
        "describe",
    );
    let description = execution
        .render_lcm_describe(LcmDescribeRequest {
            provider: "provider.application".to_owned(),
            session_id: "session.temporal.application".to_owned(),
            target: LcmDescribeTarget::Session,
        })
        .await
        .expect("authorized describe render");
    let description = serde_json::to_value(description).expect("describe response JSON");
    assert_eq!(description["raw_message_count"], 2);
    assert_storage_unchanged(&harness, &baseline, "describe");
    authorizer.revoke();
    assert!(matches!(
        service.retrieve(&context, &binding, describe_query).await,
        SessionRetrievalOutcome::Denied
    ));
    assert_storage_unchanged(&harness, &baseline, "revoked describe");

    authorizer.allow();
    let expand_target = LcmExpandTarget::RawMessage {
        store_id: raw_store_id,
    };
    let direct = execution
        .resolve_lcm_expand_target(
            "provider.application",
            &SessionId::new("session.temporal.application").unwrap(),
            &expand_target,
        )
        .await
        .expect("resolve authorized expand target");
    assert_storage_unchanged(&harness, &baseline, "expand target resolution");
    let expand_query = application_query(
        "",
        None,
        TemporalModeV1::Current,
        RetrievalGrainV1::Occurrence,
        1,
    )
    .with_compatibility_filter_digest("surface.expand.v1".to_owned())
    .with_direct_anchor(direct.anchor_id.clone());
    let expanded = only_result(
        service
            .retrieve(&context, &binding, expand_query.clone())
            .await,
        "expand",
    );
    let canonical_content = available_content(&expanded, &direct.anchor_id);
    let expansion = execution
        .render_lcm_expand(
            LcmExpandRequest {
                provider: "provider.application".to_owned(),
                session_id: "session.temporal.application".to_owned(),
                target: expand_target,
                content_slice: Some(LcmContentSlice {
                    offset: 0,
                    limit: 256,
                }),
                source_offset: 0,
                source_limit: None,
            },
            &canonical_content,
        )
        .await
        .expect("authorized expand render");
    let expansion = serde_json::to_value(expansion).expect("expand response JSON");
    assert_eq!(expansion["content"], INLINE_PAYLOAD);
    assert_storage_unchanged(&harness, &baseline, "expand");
    authorizer.revoke();
    assert!(matches!(
        service.retrieve(&context, &binding, expand_query).await,
        SessionRetrievalOutcome::Denied
    ));
    assert_storage_unchanged(&harness, &baseline, "revoked expand");

    authorizer.allow();
    let expand_query = application_query(
        "occurrence payload",
        None,
        TemporalModeV1::Current,
        RetrievalGrainV1::Occurrence,
        8,
    )
    .with_compatibility_filter_digest("surface.expand-query.v1".to_owned());
    let expanded_query = only_result(
        service
            .retrieve(&context, &binding, expand_query.clone())
            .await,
        "expand-query",
    );
    assert!(expanded_query.context.rendered.contains(INLINE_PAYLOAD));
    assert!(expanded_query.context.rendered.contains(EXTERNAL_PAYLOAD));
    assert_storage_unchanged(&harness, &baseline, "expand-query");
    authorizer.revoke();
    assert!(matches!(
        service.retrieve(&context, &binding, expand_query).await,
        SessionRetrievalOutcome::Denied
    ));
    assert_storage_unchanged(&harness, &baseline, "revoked expand-query");
}

#[tokio::test]
async fn registered_direct_anchor_replay_and_continuation_reauthorize_without_storage_mutation() {
    let harness = RegisteredTemporalHarness::open("registered-lcm-read-replay").await;
    let policy_digest = harness.seed_application_fixture().await;
    let baseline = storage_fingerprint(&harness);
    let (context, binding) = request_context(policy_digest);
    let authorizer = RevocableAuthorizer::new();
    let execution = RegisteredGlobalDbSessionTemporalExecution::new(harness.registered.as_ref());
    let service = SessionRetrievalService::new(
        authorizer.clone(),
        &execution,
        Words,
        SessionRetrievalConfiguration::new(3, 5).unwrap(),
    );

    let discovery = only_result(
        service
            .retrieve(
                &context,
                &binding,
                application_query(
                    "inline occurrence",
                    None,
                    TemporalModeV1::Current,
                    RetrievalGrainV1::Occurrence,
                    8,
                ),
            )
            .await,
        "direct-anchor discovery",
    );
    let anchor_id = discovery.ranked[0].anchor_id.clone();
    assert_storage_unchanged(&harness, &baseline, "direct-anchor discovery");

    let direct_query = application_query(
        "",
        None,
        TemporalModeV1::Current,
        RetrievalGrainV1::Occurrence,
        1,
    )
    .with_direct_anchor(anchor_id.clone());
    let direct = only_result(
        service
            .retrieve(&context, &binding, direct_query.clone())
            .await,
        "direct-anchor",
    );
    assert_eq!(direct.ranked[0].anchor_id, anchor_id);
    assert_storage_unchanged(&harness, &baseline, "direct-anchor");
    authorizer.revoke();
    assert!(matches!(
        service.retrieve(&context, &binding, direct_query).await,
        SessionRetrievalOutcome::Denied
    ));
    assert_storage_unchanged(&harness, &baseline, "revoked direct-anchor");

    authorizer.allow();
    let replay_query = application_query(
        "occurrence payload",
        None,
        TemporalModeV1::Current,
        RetrievalGrainV1::Occurrence,
        8,
    );
    let first = only_result(
        service
            .retrieve(&context, &binding, replay_query.clone())
            .await,
        "replay first execution",
    );
    let replay_execution =
        RegisteredGlobalDbSessionTemporalExecution::new(harness.registered.as_ref());
    let replay_service = SessionRetrievalService::new(
        authorizer.clone(),
        &replay_execution,
        Words,
        SessionRetrievalConfiguration::new(3, 5).unwrap(),
    );
    let replay = only_result(
        replay_service
            .retrieve(&context, &binding, replay_query.clone())
            .await,
        "replay reconstructed execution",
    );
    assert_eq!(
        first
            .ranked
            .iter()
            .map(|item| &item.anchor_id)
            .collect::<Vec<_>>(),
        replay
            .ranked
            .iter()
            .map(|item| &item.anchor_id)
            .collect::<Vec<_>>()
    );
    assert_eq!(first.context.rendered, replay.context.rendered);
    assert_storage_unchanged(&harness, &baseline, "replay");
    authorizer.revoke();
    assert!(matches!(
        replay_service
            .retrieve(&context, &binding, replay_query)
            .await,
        SessionRetrievalOutcome::Denied
    ));
    assert_storage_unchanged(&harness, &baseline, "revoked replay");

    authorizer.allow();
    let first_page = only_result(
        service
            .retrieve(
                &context,
                &binding,
                application_query(
                    "occurrence payload",
                    None,
                    TemporalModeV1::Current,
                    RetrievalGrainV1::Occurrence,
                    1,
                ),
            )
            .await,
        "continuation first page",
    );
    let first_anchor = first_page.ranked[0].anchor_id.clone();
    let cursor = first_page
        .next_cursor
        .clone()
        .expect("first page must carry a canonical continuation");
    assert_storage_unchanged(&harness, &baseline, "continuation first page");
    let continuation_query = application_query(
        "occurrence payload",
        Some(cursor),
        TemporalModeV1::Current,
        RetrievalGrainV1::Occurrence,
        1,
    );
    let continued = only_result(
        service
            .retrieve(&context, &binding, continuation_query.clone())
            .await,
        "continuation",
    );
    assert_ne!(continued.ranked[0].anchor_id, first_anchor);
    assert_storage_unchanged(&harness, &baseline, "continuation");
    authorizer.revoke();
    assert!(matches!(
        service
            .retrieve(&context, &binding, continuation_query)
            .await,
        SessionRetrievalOutcome::Denied
    ));
    assert_storage_unchanged(&harness, &baseline, "revoked continuation");
}

fn assert_surface_completed(
    outcome: &SessionRetrievalOutcome<TemporalKernelResult>,
    surface: &str,
) {
    assert!(
        matches!(
            outcome,
            SessionRetrievalOutcome::Complete { .. }
                | SessionRetrievalOutcome::CompleteZero { .. }
                | SessionRetrievalOutcome::Partial { .. }
        ),
        "{surface} must complete through the canonical service: {outcome:?}"
    );
}

fn only_result(
    outcome: SessionRetrievalOutcome<TemporalKernelResult>,
    surface: &str,
) -> TemporalKernelResult {
    match outcome {
        SessionRetrievalOutcome::Complete { mut items, .. }
        | SessionRetrievalOutcome::Partial { mut items, .. } => {
            assert_eq!(items.len(), 1, "{surface} must return one kernel result");
            items.pop().unwrap()
        }
        outcome => panic!("{surface} must return a canonical kernel result: {outcome:?}"),
    }
}

fn available_content(result: &TemporalKernelResult, anchor_id: &RetrievalAnchorId) -> String {
    let content = result
        .hydrated
        .iter()
        .find(|hydrated| {
            hydrated.anchor_id() == anchor_id && hydrated.state() == HydrationStateV1::Available
        })
        .and_then(|hydrated| hydrated.content())
        .expect("authorized canonical payload");
    std::str::from_utf8(content)
        .expect("canonical payload UTF-8")
        .to_owned()
}

fn storage_fingerprint(harness: &RegisteredTemporalHarness) -> ([u8; 32], [u8; 32], [u8; 32]) {
    let database_path = harness.registered.db_path();
    let database = file_fingerprint(database_path);
    let wal = file_fingerprint(&database_path.with_file_name(format!(
        "{}-wal",
        database_path.file_name().unwrap().to_string_lossy()
    )));
    let payload = file_fingerprint(&harness.application_external_payload_path());
    (database, wal, payload)
}

fn file_fingerprint(path: &std::path::Path) -> [u8; 32] {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => panic!("read storage fingerprint {}: {error}", path.display()),
    };
    Sha256::digest(bytes).into()
}

fn assert_storage_unchanged(
    harness: &RegisteredTemporalHarness,
    expected: &([u8; 32], [u8; 32], [u8; 32]),
    surface: &str,
) {
    assert_eq!(
        &storage_fingerprint(harness),
        expected,
        "{surface} must not mutate the registered database, durable WAL, or external payload"
    );
}

fn request_context(policy_digest: [u8; 32]) -> (RequestContext, SessionRequestBinding) {
    let actor = ActorId::new("actor.temporal.privacy").unwrap();
    let request_id = RequestId::new("request.temporal.privacy").unwrap();
    let identity = ResolvedSessionIdentity::for_project(
        ProfileId::new("profile.primary").unwrap(),
        ProjectId::new(PROJECT_ID).unwrap(),
        SessionStoreId::new("store.project.tracedecay").unwrap(),
        SessionRootId::new("root.one").unwrap(),
        ResolvedGitRoute::new(
            RepositoryId::new("repository.tracedecay").unwrap(),
            WorktreeId::new("worktree.main").unwrap(),
            BranchId::new("branch.temporal-privacy").unwrap(),
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
        CapabilityGrantId::new("grant.temporal.privacy.application").unwrap(),
        1,
        session_application_grant_digest(capability, policy, configuration, &cancellation, budgets)
            .unwrap(),
        actor.clone(),
        observed_at,
        expires_at,
        scope.clone(),
        BTreeSet::from([CapabilityId::new("capability.session.temporal-retrieval").unwrap()]),
        BTreeSet::from([UseCaseId::new("use-case.session.temporal-privacy-test").unwrap()]),
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

fn application_query(
    text: &str,
    cursor: Option<String>,
    temporal_mode: TemporalModeV1,
    grain: RetrievalGrainV1,
    limit: usize,
) -> SessionTemporalQuery {
    SessionTemporalQuery::new(
        SessionId::new("session.temporal.application").unwrap(),
        Some("provider.application".to_owned()),
        text,
        cursor,
        temporal_mode,
        grain,
        limit,
        DiversityLimits::unbounded(),
        ContextBudget {
            max_bytes: 64_000,
            max_tokens: 16_000,
            estimator_version: "privacy-words-v1".to_owned(),
        },
    )
    .unwrap()
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
