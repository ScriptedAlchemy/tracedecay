use super::evidence::{
    AutomationEvidenceFilters, AutomationTemporalEvidence, AutomationTemporalEvidenceItem,
    SESSION_REPLAY_HEAD_TURNS, SESSION_REPLAY_SUMMARY_NODES, SESSION_REPLAY_TAIL_TURNS,
    find_i64_field_in_json, find_string_field_in_json,
};

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use serde_json::Value;
use sha2::{Digest, Sha256};
use tracedecay_application::{
    CancellationContext, CapabilityGrantId, CapabilityGrantSnapshot, Deadline, DisclosureClass,
    RequestContext, RequestId,
};
use tracedecay_automation::evidence_budget::SESSION_EVIDENCE_BUDGET_EXHAUSTED;
use tracedecay_domain::{
    ActorId, ProjectId, RepositoryId, RetrievalGrainV1, SessionId, TemporalCoverageCountsV1,
    TemporalModeV1, UtcMicros, WorktreeId,
};
use tracedecay_store::{StoreShardIdV1, StoreShardScopeV1};
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

use crate::application::context::{
    BranchId, CancellationToken, CapabilityDigest, ConfigurationDigest, PolicyDigest, ProfileId,
    RequestBudgets, ResolvedGitRoute, ResolvedSessionIdentity, SessionRootId, SessionStoreId,
    application_observed_at, session_application_grant_digest,
};
use crate::application::session::{
    AuthorizationGrantId, SessionAccess, SessionAuthorizationError, SessionAuthorizationGrant,
    SessionFreshnessPolicy, SessionRequestBinding, SessionRetrievalConfiguration,
    SessionRetrievalOutcome, SessionRetrievalScope, SessionRetrievalService,
    SessionScopeAuthorizationRequest, SessionScopeAuthorizer, SessionTemporalExecutionPort,
    SessionTemporalQuery,
};
use crate::errors::{Result, TraceDecayError};
use crate::ports::project_runtime::ProfileIdentity;
use crate::ports::session_evidence::LcmScope;
use crate::request_identity::{GlobalRequestSurface, mint_global_request_id};
use crate::tracedecay::TraceDecay;
use tracedecay_global_db::session_temporal::RegisteredGlobalDbSessionTemporalExecution;
use tracedecay_global_db::{RegisteredGlobalDb, RegisteredGlobalDbLeaseV1};
use tracedecay_temporal_query::TemporalKernelResult;
use tracedecay_temporal_query::context::{ContextBudget, TokenPolicy, VersionedTokenEstimator};
use tracedecay_temporal_query::ranking::DiversityLimits;

pub(super) const AUTOMATION_SESSION_MAX_BYTES: u64 = 2 * 1024 * 1024;
const AUTOMATION_SESSION_MAX_RESULTS: u64 = 128;
const AUTOMATION_SESSION_MAX_WORK_UNITS: u64 = 100_000;
const AUTOMATION_SESSION_TIMEOUT: Duration = Duration::from_secs(30);
const AUTOMATION_SESSION_ESTIMATOR_VERSION: &str = "automation-words-v1";
const AUTOMATION_SESSION_ACTOR_ID: &str = "automation.session-evidence";
const AUTOMATION_SESSION_SCHEMA_VERSION: u32 = 1;
const AUTOMATION_SESSION_RANKING_VERSION: u32 = 1;
#[doc(hidden)]
pub enum AutomationTemporalRetrieval {
    Complete(AutomationTemporalEvidence),
    CompleteZero,
    Rejected(&'static str),
}

pub type AutomationSessionRetrievalFuture<'a> =
    Pin<Box<dyn Future<Output = AutomationTemporalRetrieval> + Send + 'a>>;

/// Authorized retrieval dependency supplied by the automation composition root.
///
/// Implementations own the request context and grant authority. The runner only
/// supplies a bounded forensic query and serializes complete results.
pub trait AutomationSessionRetrieval: Send + Sync {
    fn anchor_session_id(&self) -> &SessionId;

    fn retrieve(&self, query: SessionTemporalQuery) -> AutomationSessionRetrievalFuture<'_>;
}

impl<'a, A, P, E> AuthorizedAutomationSessionRetrieval<'a, A, P, E> {
    pub fn new(
        service: &'a SessionRetrievalService<A, P, E>,
        context: &'a RequestContext,
        binding: &'a SessionRequestBinding,
        anchor_session_id: SessionId,
    ) -> Self {
        Self {
            service,
            context,
            binding,
            anchor_session_id,
        }
    }
}

impl<A, P, E> AutomationSessionRetrieval for AuthorizedAutomationSessionRetrieval<'_, A, P, E>
where
    A: SessionScopeAuthorizer + Send + Sync,
    P: SessionTemporalExecutionPort + Send + Sync,
    E: VersionedTokenEstimator + Send + Sync,
{
    fn anchor_session_id(&self) -> &SessionId {
        &self.anchor_session_id
    }

    fn retrieve(&self, query: SessionTemporalQuery) -> AutomationSessionRetrievalFuture<'_> {
        Box::pin(async move {
            accept_automation_temporal_outcome(
                self.service
                    .retrieve(self.context, self.binding, query)
                    .await,
            )
        })
    }
}

/// Adapter for an already-authorized application retrieval service.
pub struct AuthorizedAutomationSessionRetrieval<'a, A, P, E> {
    service: &'a SessionRetrievalService<A, P, E>,
    context: &'a RequestContext,
    binding: &'a SessionRequestBinding,
    anchor_session_id: SessionId,
}

struct ProductionAutomationSessionRetrieval {
    database: RegisteredGlobalDbLeaseV1,
    identity: ResolvedSessionIdentity,
    anchor_session_id: SessionId,
}

struct UnavailableAutomationSessionRetrieval {
    anchor_session_id: SessionId,
    reason: &'static str,
}

#[derive(Clone, Copy)]
pub(super) struct AutomationWordEstimator;

impl VersionedTokenEstimator for AutomationWordEstimator {
    fn version(&self) -> &str {
        AUTOMATION_SESSION_ESTIMATOR_VERSION
    }

    fn token_policy(&self) -> TokenPolicy {
        TokenPolicy::Whitespace
    }
}

struct ProductionAutomationSessionAuthorizer {
    identity: ResolvedSessionIdentity,
    anchor_session_id: SessionId,
    retrieval_scope: SessionRetrievalScope,
    provider: Option<String>,
    grant_id: &'static str,
}

impl SessionScopeAuthorizer for ProductionAutomationSessionAuthorizer {
    fn authorize(
        &self,
        context: &RequestContext,
        binding: &SessionRequestBinding,
        request: &SessionScopeAuthorizationRequest,
    ) -> std::result::Result<SessionAuthorizationGrant, SessionAuthorizationError> {
        if context.actor().as_str() != AUTOMATION_SESSION_ACTOR_ID
            || request.actor_id() != context.actor()
            || binding.identity() != &self.identity
            || request.identity() != &self.identity
        {
            return Err(SessionAuthorizationError::WrongContext);
        }
        if request.session_id() != &self.anchor_session_id
            || request.retrieval_scope() != &self.retrieval_scope
            || request.provider_scope() != self.provider.as_deref()
            || request.temporal_mode() != TemporalModeV1::Forensic
            || request.grain() != RetrievalGrainV1::LogicalMessage
            || request.access() != SessionAccess::Hydrate
        {
            return Err(SessionAuthorizationError::WrongScope);
        }
        SessionAuthorizationGrant::issue(
            AuthorizationGrantId::new(self.grant_id)?,
            1,
            context,
            binding,
            request,
        )
    }
}

impl ProductionAutomationSessionRetrieval {
    fn request_context(
        &self,
        provider: Option<&str>,
    ) -> Option<(RequestContext, SessionRequestBinding)> {
        let request_id =
            mint_global_request_id(GlobalRequestSurface::AutomationSessionRetrieval).ok()?;
        let actor = ActorId::new(AUTOMATION_SESSION_ACTOR_ID).ok()?;
        let request_id = RequestId::new(request_id.as_str()).ok()?;
        let scope = self.identity.application_scope().ok()?;
        let capability = CapabilityDigest::new(automation_session_digest(
            b"tracedecay.automation.session.capability.v1\0",
            &self.identity,
            provider,
        ));
        let policy = automation_session_policy_digest()?;
        let configuration = ConfigurationDigest::new(automation_session_digest(
            b"tracedecay.automation.session.configuration.v1\0",
            &self.identity,
            None,
        ));
        let cancellation = CancellationToken::for_application_request(request_id.as_str());
        let budgets = RequestBudgets::new(
            AUTOMATION_SESSION_MAX_RESULTS,
            AUTOMATION_SESSION_MAX_BYTES,
            AUTOMATION_SESSION_MAX_WORK_UNITS,
        )
        .ok()?;
        let grant_digest = session_application_grant_digest(
            capability,
            policy,
            configuration,
            &cancellation,
            budgets,
        )
        .ok()?;
        let observed_at = application_observed_at();
        let timeout_micros =
            i64::try_from(AUTOMATION_SESSION_TIMEOUT.as_micros()).unwrap_or(i64::MAX);
        let expires_at = UtcMicros(observed_at.0.saturating_add(timeout_micros));
        let grant = CapabilityGrantSnapshot::new(
            CapabilityGrantId::new("grant.automation.session-evidence.application").ok()?,
            1,
            grant_digest,
            actor.clone(),
            observed_at,
            expires_at,
            scope.clone(),
            BTreeSet::from([CapabilityId::new("capability.session.temporal-retrieval").ok()?]),
            BTreeSet::from([UseCaseId::new("use-case.automation.session-evidence").ok()?]),
            DisclosureClass::Evidence,
        )
        .ok()?;
        let context = RequestContext::new(
            actor,
            scope,
            grant,
            request_id.clone(),
            Deadline::new(expires_at).ok()?,
            CancellationContext::active(cancellation.application_token_id()?).ok()?,
        )
        .ok()?;
        let binding = SessionRequestBinding::new(
            self.identity.clone(),
            capability,
            policy,
            configuration,
            cancellation,
            budgets,
        );
        Some((context, binding))
    }
}

impl AutomationSessionRetrieval for ProductionAutomationSessionRetrieval {
    fn anchor_session_id(&self) -> &SessionId {
        &self.anchor_session_id
    }

    fn retrieve(&self, query: SessionTemporalQuery) -> AutomationSessionRetrievalFuture<'_> {
        Box::pin(async move {
            let Some((context, binding)) = self.request_context(query.provider()) else {
                return AutomationTemporalRetrieval::Rejected("session_evidence_unavailable");
            };
            let Ok(configuration) = SessionRetrievalConfiguration::new(
                AUTOMATION_SESSION_SCHEMA_VERSION,
                AUTOMATION_SESSION_RANKING_VERSION,
            ) else {
                return AutomationTemporalRetrieval::Rejected("session_evidence_unavailable");
            };
            let service = SessionRetrievalService::new(
                ProductionAutomationSessionAuthorizer {
                    identity: self.identity.clone(),
                    anchor_session_id: query.session_id().clone(),
                    retrieval_scope: query.retrieval_scope().clone(),
                    provider: query.provider().map(str::to_owned),
                    grant_id: "grant.automation.session-evidence.project",
                },
                RegisteredGlobalDbSessionTemporalExecution::new(self.database.as_ref()),
                AutomationWordEstimator,
                configuration,
            );
            accept_automation_temporal_outcome(service.retrieve(&context, &binding, query).await)
        })
    }
}

impl AutomationSessionRetrieval for UnavailableAutomationSessionRetrieval {
    fn anchor_session_id(&self) -> &SessionId {
        &self.anchor_session_id
    }

    fn retrieve(&self, _query: SessionTemporalQuery) -> AutomationSessionRetrievalFuture<'_> {
        Box::pin(async move { AutomationTemporalRetrieval::Rejected(self.reason) })
    }
}

fn automation_session_digest(
    domain: &[u8],
    identity: &ResolvedSessionIdentity,
    provider: Option<&str>,
) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(domain);
    digest.update(identity.profile_id().as_str().as_bytes());
    if let Some(project_id) = identity.project_id() {
        digest.update([0]);
        digest.update(project_id.as_str().as_bytes());
    }
    digest.update([0]);
    digest.update(identity.store_id().as_str().as_bytes());
    digest.update([0]);
    digest.update(identity.root_id().as_str().as_bytes());
    if let Some(route) = identity.git_route() {
        digest.update([0]);
        digest.update(route.repository_id().as_str().as_bytes());
        digest.update([0]);
        digest.update(route.worktree_id().as_str().as_bytes());
        digest.update([0]);
        digest.update(route.branch_id().as_str().as_bytes());
    }
    if let Some(provider) = provider {
        digest.update([0]);
        digest.update(provider.as_bytes());
    }
    digest.finalize().into()
}

fn automation_session_policy_digest() -> Option<PolicyDigest> {
    let digest = tracedecay_store::observation_capture_access_policy_digest_v1().ok()?;
    PolicyDigest::from_access_policy_digest(&digest).ok()
}

pub(super) async fn retrieve_automation_session_evidence(
    retrieval: &dyn AutomationSessionRetrieval,
    query_text: &str,
    scope: LcmScope,
    filters: AutomationEvidenceFilters<'_>,
) -> Result<AutomationTemporalRetrieval> {
    let anchor_session_id = match filters.session_id {
        Some(session_id) => {
            SessionId::new(session_id.to_string()).map_err(|error| TraceDecayError::Config {
                message: format!("invalid automation session anchor: {error}"),
            })?
        }
        None => retrieval.anchor_session_id().clone(),
    };
    let retrieval_scope = if matches!(scope, LcmScope::Session) {
        SessionRetrievalScope::Session(anchor_session_id.clone())
    } else {
        SessionRetrievalScope::AllSessionsInAuthorizedRoot
    };
    let provider = (filters.provider != "all").then(|| filters.provider.to_string());
    let requested_limit = filters
        .evidence_limit
        .max(filters.recent_sessions_limit.clamp(1, 10).saturating_mul(
            SESSION_REPLAY_HEAD_TURNS + SESSION_REPLAY_TAIL_TURNS + SESSION_REPLAY_SUMMARY_NODES,
        ))
        .clamp(1, 128);
    let temporal_query = SessionTemporalQuery::new(
        anchor_session_id,
        provider,
        query_text,
        None,
        TemporalModeV1::Forensic,
        RetrievalGrainV1::LogicalMessage,
        requested_limit,
        DiversityLimits {
            per_logical_message: 1,
            per_turn: SESSION_REPLAY_HEAD_TURNS + SESSION_REPLAY_TAIL_TURNS,
            per_session: SESSION_REPLAY_HEAD_TURNS
                + SESSION_REPLAY_TAIL_TURNS
                + SESSION_REPLAY_SUMMARY_NODES,
            per_source: requested_limit,
            per_evidence_role: requested_limit,
        },
        ContextBudget {
            max_bytes: AUTOMATION_SESSION_MAX_BYTES,
            max_tokens: AUTOMATION_SESSION_MAX_BYTES / 4,
            estimator_version: AUTOMATION_SESSION_ESTIMATOR_VERSION.to_string(),
        },
    )
    .map_err(|error| TraceDecayError::Config {
        message: format!("invalid automation session forensic query: {error}"),
    })?
    .with_retrieval_scope(retrieval_scope)
    .with_freshness_policy(SessionFreshnessPolicy::RequireFresh);
    Ok(retrieval.retrieve(temporal_query).await)
}

pub(super) fn accept_automation_temporal_outcome(
    outcome: SessionRetrievalOutcome<TemporalKernelResult>,
) -> AutomationTemporalRetrieval {
    match outcome {
        SessionRetrievalOutcome::Complete { items, freshness } => {
            if !SessionFreshnessPolicy::RequireFresh.accepts(freshness) {
                return AutomationTemporalRetrieval::Rejected("session_evidence_stale");
            }
            let mut evidence_items = Vec::new();
            let mut coverage = TemporalCoverageCountsV1::default();
            for item in items {
                if !item.context.bundle.continuation_anchors.is_empty()
                    || !item.context.bundle.omissions.is_empty()
                    || !item.summary_omissions.is_empty()
                    || item.next_cursor.is_some()
                    || item.coverage.hidden != 0
                    || item.coverage.unknown != 0
                    || item.coverage.redacted != 0
                    || item.context.bundle.coverage != item.coverage
                {
                    return AutomationTemporalRetrieval::Rejected("session_evidence_partial");
                }
                let payloads = authorized_temporal_payloads(&item.context.rendered);
                if payloads.len() != item.ranked.len()
                    || item.context.bundle.records.len() != item.ranked.len()
                {
                    return AutomationTemporalRetrieval::Rejected("session_evidence_partial");
                }
                coverage.visible = coverage.visible.saturating_add(item.coverage.visible);
                for ranked in item.ranked {
                    let snippet = payloads
                        .get(ranked.anchor_id.as_str())
                        .cloned()
                        .unwrap_or_default();
                    if snippet.is_empty() {
                        return AutomationTemporalRetrieval::Rejected(
                            "session_evidence_unavailable",
                        );
                    }
                    let provider =
                        find_string_field_in_json(&snippet, "provider").unwrap_or_default();
                    let session_id = ranked.session.unwrap_or_default();
                    if provider.is_empty() || session_id.is_empty() {
                        return AutomationTemporalRetrieval::Rejected(
                            "session_evidence_unavailable",
                        );
                    }
                    evidence_items.push(AutomationTemporalEvidenceItem {
                        anchor_id: ranked.anchor_id.to_string(),
                        stable_id: ranked.stable_id,
                        provider,
                        session_id,
                        message_id: ranked.logical_message,
                        source_id: ranked.source,
                        store_id: find_i64_field_in_json(&snippet, "store_id"),
                        role: ranked.evidence_role,
                        ordinal: find_i64_field_in_json(&snippet, "ordinal"),
                        session_total_messages: find_i64_field_in_json(
                            &snippet,
                            "session_total_messages",
                        )
                        .and_then(|value| u64::try_from(value).ok()),
                        knowledge_at_micros: ranked.knowledge_at_micros,
                        normalized_score_micros: ranked.normalized_score_micros,
                        snippet,
                    });
                }
            }
            let unique_visible = evidence_items
                .iter()
                .map(|item| item.anchor_id.as_str())
                .collect::<BTreeSet<_>>()
                .len() as u64;
            if unique_visible != coverage.visible {
                return AutomationTemporalRetrieval::Rejected("session_evidence_partial");
            }
            if evidence_items.is_empty() {
                AutomationTemporalRetrieval::CompleteZero
            } else {
                AutomationTemporalRetrieval::Complete(AutomationTemporalEvidence {
                    items: evidence_items,
                    coverage,
                })
            }
        }
        SessionRetrievalOutcome::CompleteZero { freshness } => {
            if SessionFreshnessPolicy::RequireFresh.accepts(freshness) {
                AutomationTemporalRetrieval::CompleteZero
            } else {
                AutomationTemporalRetrieval::Rejected("session_evidence_stale")
            }
        }
        SessionRetrievalOutcome::Stale { .. } => {
            AutomationTemporalRetrieval::Rejected("session_evidence_stale")
        }
        SessionRetrievalOutcome::Partial { .. } => {
            AutomationTemporalRetrieval::Rejected("session_evidence_partial")
        }
        SessionRetrievalOutcome::Denied | SessionRetrievalOutcome::WrongScope => {
            AutomationTemporalRetrieval::Rejected("session_evidence_denied")
        }
        SessionRetrievalOutcome::Locked
        | SessionRetrievalOutcome::Redacted
        | SessionRetrievalOutcome::Deleted => {
            AutomationTemporalRetrieval::Rejected("session_evidence_locked")
        }
        SessionRetrievalOutcome::Unavailable => {
            AutomationTemporalRetrieval::Rejected("session_evidence_unavailable")
        }
        SessionRetrievalOutcome::ResetRequired => {
            AutomationTemporalRetrieval::Rejected("session_evidence_reset_required")
        }
        SessionRetrievalOutcome::CursorManifestLimitExceeded { .. } => {
            AutomationTemporalRetrieval::Rejected("session_cursor_manifest_limit_exceeded")
        }
        SessionRetrievalOutcome::BudgetExhausted => {
            AutomationTemporalRetrieval::Rejected(SESSION_EVIDENCE_BUDGET_EXHAUSTED)
        }
        SessionRetrievalOutcome::Cancelled => {
            AutomationTemporalRetrieval::Rejected("session_evidence_cancelled")
        }
    }
}

fn authorized_temporal_payloads(rendered: &str) -> BTreeMap<String, String> {
    serde_json::from_str::<Value>(rendered)
        .ok()
        .and_then(|value| value.get("payloads").and_then(Value::as_array).cloned())
        .unwrap_or_default()
        .into_iter()
        .filter_map(|payload| {
            let anchor = payload.get("anchor_id")?.as_str()?.to_string();
            let data = payload.get("data")?.as_str()?.to_string();
            Some((anchor, data))
        })
        .collect()
}

fn registered_scope_matches(
    shard: &StoreShardIdV1,
    brain_id: &tracedecay_domain::BrainId,
    profile_id: &tracedecay_domain::UserProfileId,
    project_id: Option<&ProjectId>,
) -> bool {
    if &shard.brain_id != brain_id || &shard.profile_id != profile_id {
        return false;
    }
    match (&shard.scope, project_id) {
        (StoreShardScopeV1::ProfileSessions, None) => true,
        (StoreShardScopeV1::ProjectSessions { project_id: actual }, Some(expected)) => {
            actual == expected
        }
        _ => false,
    }
}

async fn active_registered_automation_anchor(database: &RegisteredGlobalDb) -> Option<SessionId> {
    let snapshot = database.read_snapshot().await.ok()?;
    let mut rows = snapshot
        .query(
            "SELECT session_id
             FROM session_temporal_generations
             WHERE state = 'active'
             ORDER BY COALESCE(activated_at, created_at) DESC, session_id
             LIMIT 1",
            (),
        )
        .await
        .ok()?;
    let session_id = rows.next().await.ok()??.get::<String>(0).ok()?;
    SessionId::new(session_id).ok()
}

fn profile_id(profile_identity: &dyn ProfileIdentity) -> Option<ProfileId> {
    ProfileId::new(profile_identity.profile_id().as_str().to_string()).ok()
}

fn session_store_id(shard: &StoreShardIdV1) -> Option<SessionStoreId> {
    let encoded = serde_json::to_vec(shard).ok()?;
    let digest = Sha256::digest(encoded);
    SessionStoreId::new(format!("store.sessions.{}", hex::encode(&digest[..16]))).ok()
}

fn project_automation_identity(
    shard: &StoreShardIdV1,
    profile_identity: &dyn ProfileIdentity,
    project_id: &ProjectId,
) -> Option<ResolvedSessionIdentity> {
    Some(ResolvedSessionIdentity::for_project(
        profile_id(profile_identity)?,
        project_id.clone(),
        session_store_id(shard)?,
        SessionRootId::new(format!("root.sessions.project.{}", project_id.as_str())).ok()?,
        ResolvedGitRoute::new(
            RepositoryId::new(format!("repository.project.{}", project_id.as_str())).ok()?,
            WorktreeId::new(format!("worktree.project.{}", project_id.as_str())).ok()?,
            BranchId::new(format!("branch.project.{}", project_id.as_str())).ok()?,
        ),
    ))
}

async fn registered_automation_retrieval_for_identity(
    database: RegisteredGlobalDbLeaseV1,
    identity: ResolvedSessionIdentity,
) -> Box<dyn AutomationSessionRetrieval> {
    let Some(anchor_session_id) = active_registered_automation_anchor(&database).await else {
        return unavailable_automation_retrieval("session_evidence_retrieval_unavailable");
    };
    Box::new(ProductionAutomationSessionRetrieval {
        database,
        identity,
        anchor_session_id,
    })
}

#[cfg(test)]
fn profile_automation_identity(
    shard: &StoreShardIdV1,
    profile_identity: &dyn ProfileIdentity,
) -> Option<ResolvedSessionIdentity> {
    Some(ResolvedSessionIdentity::for_profile(
        profile_id(profile_identity)?,
        session_store_id(shard)?,
        SessionRootId::new(format!(
            "root.sessions.profile.{}",
            profile_identity.profile_id().as_str()
        ))
        .ok()?,
    ))
}

pub async fn registered_project_automation_retrieval(
    database: RegisteredGlobalDbLeaseV1,
    profile_identity: &dyn ProfileIdentity,
    project_id: &ProjectId,
) -> Result<Box<dyn AutomationSessionRetrieval>> {
    let shard = &database.binding().shard_id;
    if !registered_scope_matches(
        shard,
        profile_identity.brain_id(),
        profile_identity.profile_id(),
        Some(project_id),
    ) {
        return Err(TraceDecayError::Config {
            message: "registered project session runtime authority mismatch".to_string(),
        });
    }
    let Some(identity) = project_automation_identity(shard, profile_identity, project_id) else {
        return Err(TraceDecayError::Config {
            message: "invalid registered project session retrieval identity".to_string(),
        });
    };
    Ok(registered_automation_retrieval_for_identity(database, identity).await)
}

pub(super) async fn production_project_automation_retrieval(
    _cg: &TraceDecay,
) -> Box<dyn AutomationSessionRetrieval> {
    unavailable_automation_retrieval("session_evidence_retrieval_unavailable")
}

fn unavailable_automation_retrieval(reason: &'static str) -> Box<dyn AutomationSessionRetrieval> {
    // The static fallback session id is a fixed, valid identifier.
    #[allow(clippy::expect_used)]
    Box::new(UnavailableAutomationSessionRetrieval {
        anchor_session_id: SessionId::new("session.automation.unavailable")
            .expect("static automation session id is valid"),
        reason,
    })
}

pub(super) async fn production_user_automation_retrieval(
    _profile_root: &std::path::Path,
) -> Box<dyn AutomationSessionRetrieval> {
    unavailable_automation_retrieval("session_evidence_retrieval_unavailable")
}

#[cfg(test)]
mod authority_tests {
    use tempfile::tempdir;
    use tracedecay_domain::{BrainId, ProjectId, UserProfileId};
    use tracedecay_global_db::tests::harness::RegisteredGlobalDbTestRuntime;
    use tracedecay_store::StoreShardIdV1;

    use super::*;

    struct FixtureProfileIdentity {
        brain_id: BrainId,
        profile_id: UserProfileId,
    }

    impl FixtureProfileIdentity {
        fn new(brain_id: BrainId, profile_id: UserProfileId) -> Self {
            Self {
                brain_id,
                profile_id,
            }
        }
    }

    impl ProfileIdentity for FixtureProfileIdentity {
        fn brain_id(&self) -> &BrainId {
            &self.brain_id
        }

        fn profile_id(&self) -> &UserProfileId {
            &self.profile_id
        }
    }

    #[test]
    fn registered_automation_scope_rejects_profile_and_project_mismatches() {
        let brain = BrainId::new("brain.automation").expect("brain id");
        let profile = UserProfileId::new("profile.automation").expect("profile id");
        let other_profile =
            UserProfileId::new("profile.automation.other").expect("other profile id");
        let project = ProjectId::new("project.automation").expect("project id");
        let other_project = ProjectId::new("project.automation.other").expect("other project id");
        let profile_sessions = StoreShardIdV1::profile_sessions(brain.clone(), profile.clone());
        let project_sessions =
            StoreShardIdV1::project_sessions(brain.clone(), profile.clone(), project.clone());

        assert!(registered_scope_matches(
            &profile_sessions,
            &brain,
            &profile,
            None,
        ));
        assert!(registered_scope_matches(
            &project_sessions,
            &brain,
            &profile,
            Some(&project),
        ));
        assert!(!registered_scope_matches(
            &profile_sessions,
            &brain,
            &profile,
            Some(&project),
        ));
        assert!(!registered_scope_matches(
            &project_sessions,
            &brain,
            &profile,
            None,
        ));
        assert!(!registered_scope_matches(
            &project_sessions,
            &brain,
            &profile,
            Some(&other_project),
        ));
        assert!(!registered_scope_matches(
            &project_sessions,
            &brain,
            &other_profile,
            Some(&project),
        ));
    }

    #[tokio::test]
    async fn convenience_retrieval_does_not_create_a_profile_session_database() {
        let directory = tempdir().expect("temporary profile");
        let database_path = directory.path().join("user-sessions.db");
        assert!(!database_path.exists());

        let retrieval = production_user_automation_retrieval(directory.path()).await;

        assert!(!database_path.exists());
        assert!(matches!(
            retrieval
                .retrieve(
                    SessionTemporalQuery::new(
                        SessionId::new("session.automation.test").expect("session id"),
                        None,
                        "test",
                        None,
                        TemporalModeV1::Forensic,
                        RetrievalGrainV1::LogicalMessage,
                        1,
                        DiversityLimits {
                            per_logical_message: 1,
                            per_turn: 1,
                            per_session: 1,
                            per_source: 1,
                            per_evidence_role: 1,
                        },
                        ContextBudget {
                            max_bytes: 1024,
                            max_tokens: 256,
                            estimator_version: AUTOMATION_SESSION_ESTIMATOR_VERSION.to_string(),
                        },
                    )
                    .expect("bounded query"),
                )
                .await,
            AutomationTemporalRetrieval::Rejected("session_evidence_retrieval_unavailable")
        ));
    }

    #[tokio::test]
    async fn project_retrieval_rejects_non_project_scope_without_fallback() {
        let directory = tempdir().expect("temporary profile");
        let runtime = RegisteredGlobalDbTestRuntime::profile(directory.path())
            .await
            .expect("registered test runtime");
        let database = runtime.profile_database_arc();
        let shard = &database.binding().shard_id;
        let identity =
            FixtureProfileIdentity::new(shard.brain_id.clone(), shard.profile_id.clone());
        let project_id = ProjectId::new("project.automation.wrong-scope").expect("project id");

        let error = registered_project_automation_retrieval(database, &identity, &project_id)
            .await
            .err()
            .expect("non-project authority must not become project retrieval");

        assert!(
            error
                .to_string()
                .contains("registered project session runtime authority mismatch")
        );
    }

    async fn typed_reject_reason(retrieval: &dyn AutomationSessionRetrieval) -> &'static str {
        let query = SessionTemporalQuery::new(
            SessionId::new("session.automation.parity").expect("session id"),
            None,
            "parity",
            None,
            TemporalModeV1::Forensic,
            RetrievalGrainV1::LogicalMessage,
            1,
            DiversityLimits {
                per_logical_message: 1,
                per_turn: 1,
                per_session: 1,
                per_source: 1,
                per_evidence_role: 1,
            },
            ContextBudget {
                max_bytes: 1024,
                max_tokens: 256,
                estimator_version: AUTOMATION_SESSION_ESTIMATOR_VERSION.to_string(),
            },
        )
        .expect("bounded query");
        match retrieval.retrieve(query).await {
            AutomationTemporalRetrieval::Rejected(reason) => reason,
            AutomationTemporalRetrieval::Complete(_) => {
                panic!("expected typed rejection, got Complete")
            }
            AutomationTemporalRetrieval::CompleteZero => {
                panic!("expected typed rejection, got CompleteZero")
            }
        }
    }

    #[tokio::test]
    async fn registered_identities_keep_typed_unavailable_without_active_anchor() {
        let directory = tempdir().expect("temporary profile");
        let runtime = RegisteredGlobalDbTestRuntime::profile(directory.path())
            .await
            .expect("registered test runtime");
        let database = runtime.profile_database_arc();
        let brain_id = BrainId::new("brain.automation.parity").expect("brain id");
        let profile_id = UserProfileId::new("profile.automation.parity").expect("profile id");
        let identity = FixtureProfileIdentity::new(brain_id.clone(), profile_id.clone());
        let profile_shard = StoreShardIdV1::profile_sessions(brain_id.clone(), profile_id.clone());
        let profile_identity =
            profile_automation_identity(&profile_shard, &identity).expect("profile identity");
        let profile_retrieval =
            registered_automation_retrieval_for_identity(database.clone(), profile_identity).await;
        assert_eq!(
            typed_reject_reason(profile_retrieval.as_ref()).await,
            "session_evidence_retrieval_unavailable"
        );

        let project_id = ProjectId::new("project.automation.parity").expect("project id");
        let project_shard =
            StoreShardIdV1::project_sessions(brain_id, profile_id, project_id.clone());
        let project_identity = project_automation_identity(&project_shard, &identity, &project_id)
            .expect("project identity");
        let project_retrieval =
            registered_automation_retrieval_for_identity(database, project_identity).await;
        assert_eq!(
            typed_reject_reason(project_retrieval.as_ref()).await,
            "session_evidence_retrieval_unavailable"
        );
    }

    #[tokio::test]
    async fn path_only_user_convenience_never_fabricates_empty_hits() {
        let directory = tempdir().expect("temporary profile");
        let retrieval = production_user_automation_retrieval(directory.path()).await;
        assert_eq!(
            typed_reject_reason(retrieval.as_ref()).await,
            "session_evidence_retrieval_unavailable"
        );
        assert!(
            !directory.path().join("user-sessions.db").exists(),
            "path-only convenience must not invent a session database"
        );
    }
}
