use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tracedecay_domain::{
    ActorId, FactOwnerV1, PayloadReferenceV1, ProjectId, RepositoryId, RetrievalGrainV1, SessionId,
    TemporalCoverageCountsV1, TemporalModeV1, WorktreeId,
};
use tracedecay_store::FactCompatibilityStore;

use super::apply_policy::MemoryApplyPolicy;
use super::artifacts::sha256_json;
use super::backend::{
    AgentTaskBackend, AgentTaskKind, AgentTaskRequest, AgentTaskResponse, BackendRetryPolicy,
    agent_task_contract, extract_json_object_prefix, prompt_version, run_agent_task_with_retry,
    task_key,
};
use super::config::AutomationConfig;
use super::fact_proposals::{
    FactProposalRecord, FactProposalState, apply_fact_proposal_with_result,
    record_session_fact_proposals,
};
use super::lifecycle::{
    AgentRunFinalizer, AgentTaskRunContext, BackendTaskRun, SchedulerGate,
    failed_backend_fallback_report, generated_run_id, task_run_gate, task_skip_reason,
};
use super::managed_skills::list_managed_skills;
use super::run_ledger::{AutomationRunLedgerRecord, AutomationRunStatus, AutomationTrigger};
use super::session_reflector::validate_fact_proposals;
use super::skill_usage::{
    DEFAULT_SKILL_OVERLAP_LIMIT, ingest_project_analytics_events, skill_overlap_candidates,
    stale_skill_recommendations, summarize_skill_usage,
};
use super::skill_writer::{
    activation_policy as skill_writer_activation_policy, skill_improvement_recommendations,
    support_file_evidence as skill_writer_support_file_evidence,
    validate_and_apply_skill_proposals,
};
use super::text::truncate_chars_for_prompt;
use crate::analytics::{ToolUsageObservation, underused_tool_family_signals};
use crate::application::context::{
    BranchId, CancellationToken, CapabilityDigest, ConfigurationDigest, MonotonicDeadline,
    PolicyDigest, ProfileId, RequestBudgets, RequestContext, RequestId, ResolvedGitRoute,
    ResolvedSessionIdentity, SessionRootId, SessionStoreId,
};
use crate::application::memory::MemoryApplication;
use crate::application::session::{
    AuthorizationGrantId, SessionAccess, SessionAuthorizationError, SessionAuthorizationGrant,
    SessionFreshnessPolicy, SessionRetrievalConfiguration, SessionRetrievalOutcome,
    SessionRetrievalScope, SessionRetrievalService, SessionScopeAuthorizationRequest,
    SessionScopeAuthorizer, SessionTemporalExecutionPort, SessionTemporalQuery,
};
use crate::errors::{Result, TraceDecayError};
use crate::global_db::{GlobalDb, GlobalDbSessionTemporalExecution, ProjectRegistryContext};
use crate::memory::user::open_user_memory_db;
use crate::query::temporal::TemporalKernelResult;
use crate::query::temporal::context::{ContextBudget, TokenPolicy, VersionedTokenEstimator};
use crate::query::temporal::ranking::DiversityLimits;
use crate::sessions::lcm::{LcmGrepHit, LcmGrepSort, LcmScope};
use crate::sessions::user_sessions_db_path;
use crate::store::memory::DatabaseFactStore;
use crate::tracedecay::{TraceDecay, current_timestamp};

pub use super::memory_curator::{
    MemoryCuratorAutomationOptions, MemoryCuratorAutomationRun, run_memory_curator_with_backend,
    run_user_memory_curator_with_backend,
};

const SKILL_ANALYTICS_IMPORT_LIMIT: usize = 2_000;
const USER_AUTOMATION_DIR: &str = "user-automation";

/// Profile-level artifact, ledger, and lock root for projectless automation.
pub fn user_automation_root(profile_root: &std::path::Path) -> PathBuf {
    profile_root.join(USER_AUTOMATION_DIR)
}

/// Bounds for the session-replay evidence channel. Worst case per session is
/// `(4 + 4) * 500 + 3 * 700 = 6_100` snippet chars, so the default three
/// sessions stay under ~5k tokens alongside the grep hits.
const SESSION_REPLAY_HEAD_TURNS: usize = 4;
const SESSION_REPLAY_TAIL_TURNS: usize = 4;
const SESSION_REPLAY_SNIPPET_CHARS: usize = 500;
const SESSION_REPLAY_SUMMARY_NODES: usize = 3;
const SESSION_REPLAY_SUMMARY_CHARS: usize = 700;
const AUTOMATION_SESSION_MAX_BYTES: u64 = 2 * 1024 * 1024;
const AUTOMATION_SESSION_MAX_RESULTS: u64 = 128;
const AUTOMATION_SESSION_MAX_WORK_UNITS: u64 = 100_000;
const AUTOMATION_SESSION_TIMEOUT: Duration = Duration::from_secs(30);
const AUTOMATION_SESSION_ESTIMATOR_VERSION: &str = "automation-words-v1";
const AUTOMATION_SESSION_ACTOR_ID: &str = "automation.session-evidence";
const AUTOMATION_SESSION_SCHEMA_VERSION: u32 = 1;
const AUTOMATION_SESSION_RANKING_VERSION: u32 = 1;

#[derive(Clone)]
#[doc(hidden)]
pub struct AutomationTemporalEvidenceItem {
    pub anchor_id: String,
    pub stable_id: String,
    pub provider: String,
    pub session_id: String,
    pub message_id: Option<String>,
    pub source_id: Option<String>,
    pub store_id: Option<i64>,
    pub role: Option<String>,
    pub ordinal: Option<i64>,
    pub session_total_messages: Option<u64>,
    pub knowledge_at_micros: i64,
    pub normalized_score_micros: u64,
    pub snippet: String,
}

#[doc(hidden)]
pub struct AutomationTemporalEvidence {
    pub items: Vec<AutomationTemporalEvidenceItem>,
    pub coverage: TemporalCoverageCountsV1,
}

struct SerializedAutomationEvidence {
    hits: Vec<CanonicalEvidenceHit>,
    recent_session_slices: Option<Value>,
    tool_usage: Vec<OwnedToolUsageObservation>,
    coverage: TemporalCoverageCountsV1,
}

#[derive(Clone, Debug, Serialize)]
struct CanonicalEvidenceHit {
    kind: String,
    provider: String,
    session_id: String,
    message_id: Option<String>,
    node_id: Option<String>,
    store_id: Option<i64>,
    role: Option<String>,
    snippet: String,
    anchor_id: String,
    stable_id: String,
    knowledge_at_micros: i64,
    normalized_score_micros: u64,
    ordinal: Option<i64>,
}

impl CanonicalEvidenceHit {
    fn compatibility_hit(&self) -> LcmGrepHit {
        LcmGrepHit {
            kind: self.kind.clone(),
            provider: self.provider.clone(),
            session_id: self.session_id.clone(),
            message_id: self.message_id.clone(),
            node_id: self.node_id.clone(),
            store_id: self.store_id,
            role: self.role.clone(),
            snippet: self.snippet.clone(),
        }
    }
}

struct OwnedToolUsageObservation {
    tool_names: Option<String>,
    metadata_json: Option<String>,
    text: Option<String>,
}

#[derive(Clone, Copy)]
struct AutomationEvidenceFilters<'a> {
    provider: &'a str,
    session_id: Option<&'a str>,
    include_summaries: bool,
    evidence_limit: usize,
    include_recent_sessions: bool,
    recent_sessions_limit: usize,
    role: Option<&'a str>,
    start_time: Option<i64>,
    end_time: Option<i64>,
    sort: LcmGrepSort,
}

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

    fn retrieve<'a>(&'a self, query: SessionTemporalQuery) -> AutomationSessionRetrievalFuture<'a>;
}

/// Adapter for an already-authorized application retrieval service.
pub struct AuthorizedAutomationSessionRetrieval<'a, A, P, E> {
    service: &'a SessionRetrievalService<A, P, E>,
    context: &'a RequestContext,
    anchor_session_id: SessionId,
}

impl<'a, A, P, E> AuthorizedAutomationSessionRetrieval<'a, A, P, E> {
    pub fn new(
        service: &'a SessionRetrievalService<A, P, E>,
        context: &'a RequestContext,
        anchor_session_id: SessionId,
    ) -> Self {
        Self {
            service,
            context,
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

    fn retrieve<'a>(&'a self, query: SessionTemporalQuery) -> AutomationSessionRetrievalFuture<'a> {
        Box::pin(async move {
            accept_automation_temporal_outcome(self.service.retrieve(self.context, query).await)
        })
    }
}

#[derive(Clone, Copy)]
enum AutomationRetrievalStoreScope {
    Project,
    Profile,
}

struct ProductionAutomationSessionRetrieval {
    database: Arc<GlobalDb>,
    identity: ResolvedSessionIdentity,
    anchor_session_id: SessionId,
    store_scope: AutomationRetrievalStoreScope,
}

struct UnavailableAutomationSessionRetrieval {
    anchor_session_id: SessionId,
    reason: &'static str,
}

#[derive(Clone, Copy)]
struct AutomationWordEstimator;

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
        request: &SessionScopeAuthorizationRequest,
    ) -> std::result::Result<SessionAuthorizationGrant, SessionAuthorizationError> {
        if context.actor_id().as_str() != AUTOMATION_SESSION_ACTOR_ID
            || request.actor_id() != context.actor_id()
            || context.identity() != &self.identity
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
            request,
        )
    }
}

impl ProductionAutomationSessionRetrieval {
    fn request_context(&self, provider: Option<&str>) -> Option<RequestContext> {
        Some(RequestContext::new(
            ActorId::new(AUTOMATION_SESSION_ACTOR_ID).ok()?,
            RequestId::new(AUTOMATION_SESSION_ACTOR_ID).ok()?,
            self.identity.clone(),
            CapabilityDigest::new(automation_session_digest(
                b"tracedecay.automation.session.capability.v1\0",
                &self.identity,
                provider,
            )),
            PolicyDigest::new(automation_session_policy_digest()?),
            ConfigurationDigest::new(automation_session_digest(
                b"tracedecay.automation.session.configuration.v1\0",
                &self.identity,
                None,
            )),
            MonotonicDeadline::at(Instant::now() + AUTOMATION_SESSION_TIMEOUT),
            CancellationToken::new(),
            RequestBudgets::new(
                AUTOMATION_SESSION_MAX_RESULTS,
                AUTOMATION_SESSION_MAX_BYTES,
                AUTOMATION_SESSION_MAX_WORK_UNITS,
            )
            .ok()?,
        ))
    }
}

impl AutomationSessionRetrieval for ProductionAutomationSessionRetrieval {
    fn anchor_session_id(&self) -> &SessionId {
        &self.anchor_session_id
    }

    fn retrieve<'a>(&'a self, query: SessionTemporalQuery) -> AutomationSessionRetrievalFuture<'a> {
        Box::pin(async move {
            let Some(context) = self.request_context(query.provider()) else {
                return AutomationTemporalRetrieval::Rejected("session_evidence_unavailable");
            };
            let configuration = match SessionRetrievalConfiguration::new(
                AUTOMATION_SESSION_SCHEMA_VERSION,
                AUTOMATION_SESSION_RANKING_VERSION,
            ) {
                Ok(configuration) => configuration,
                Err(_) => {
                    return AutomationTemporalRetrieval::Rejected("session_evidence_unavailable");
                }
            };
            let grant_id = match self.store_scope {
                AutomationRetrievalStoreScope::Project => {
                    "grant.automation.session-evidence.project"
                }
                AutomationRetrievalStoreScope::Profile => {
                    "grant.automation.session-evidence.profile"
                }
            };
            let service = SessionRetrievalService::new(
                ProductionAutomationSessionAuthorizer {
                    identity: self.identity.clone(),
                    anchor_session_id: query.session_id().clone(),
                    retrieval_scope: query.retrieval_scope().clone(),
                    provider: query.provider().map(str::to_owned),
                    grant_id,
                },
                GlobalDbSessionTemporalExecution::new(self.database.as_ref()),
                AutomationWordEstimator,
                configuration,
            );
            accept_automation_temporal_outcome(service.retrieve(&context, query).await)
        })
    }
}

impl AutomationSessionRetrieval for UnavailableAutomationSessionRetrieval {
    fn anchor_session_id(&self) -> &SessionId {
        &self.anchor_session_id
    }

    fn retrieve<'a>(
        &'a self,
        _query: SessionTemporalQuery,
    ) -> AutomationSessionRetrievalFuture<'a> {
        Box::pin(async move { AutomationTemporalRetrieval::Rejected(self.reason) })
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionReflectorAutomationOptions {
    #[serde(default)]
    pub trigger: AutomationTrigger,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default = "default_session_provider")]
    pub provider: String,
    #[serde(default = "default_session_reflection_query")]
    pub query: String,
    #[serde(default = "default_lcm_grep_scope")]
    pub scope: LcmScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default = "default_include_summaries")]
    pub include_summaries: bool,
    #[serde(default = "default_session_evidence_limit")]
    pub evidence_limit: usize,
    /// When true, include bounded turn-ordered slices of recently active
    /// sessions as a primary evidence channel alongside the keyword grep.
    #[serde(default = "default_include_recent_sessions")]
    pub include_recent_sessions: bool,
    /// How many recently active sessions to replay when `session_id` is not
    /// explicitly set.
    #[serde(default = "default_recent_sessions_limit")]
    pub recent_sessions_limit: usize,
    #[serde(default = "default_lcm_grep_sort")]
    pub sort: LcmGrepSort,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_time: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_time: Option<i64>,
}

impl Default for SessionReflectorAutomationOptions {
    fn default() -> Self {
        Self {
            trigger: AutomationTrigger::ManualCli,
            run_id: None,
            provider: default_session_provider(),
            query: default_session_reflection_query(),
            scope: default_lcm_grep_scope(),
            session_id: None,
            include_summaries: default_include_summaries(),
            evidence_limit: default_session_evidence_limit(),
            include_recent_sessions: default_include_recent_sessions(),
            recent_sessions_limit: default_recent_sessions_limit(),
            sort: default_lcm_grep_sort(),
            source: None,
            role: None,
            start_time: None,
            end_time: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionReflectorAutomationRun {
    pub run_id: String,
    pub report: Value,
    pub ledger_record: AutomationRunLedgerRecord,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend_response: Option<AgentTaskResponse>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillWriterAutomationOptions {
    #[serde(default)]
    pub trigger: AutomationTrigger,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default = "default_skill_writer_provider")]
    pub provider: String,
    #[serde(default = "default_skill_writer_query")]
    pub query: String,
    #[serde(default = "default_skill_writer_evidence_limit")]
    pub evidence_limit: usize,
    /// When true, include bounded turn-ordered slices of recently active
    /// sessions as a primary evidence channel alongside the keyword grep.
    #[serde(default = "default_include_recent_sessions")]
    pub include_recent_sessions: bool,
    /// How many recently active sessions to replay.
    #[serde(default = "default_recent_sessions_limit")]
    pub recent_sessions_limit: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_root: Option<PathBuf>,
}

impl Default for SkillWriterAutomationOptions {
    fn default() -> Self {
        Self {
            trigger: AutomationTrigger::ManualCli,
            run_id: None,
            provider: default_skill_writer_provider(),
            query: default_skill_writer_query(),
            evidence_limit: default_skill_writer_evidence_limit(),
            include_recent_sessions: default_include_recent_sessions(),
            recent_sessions_limit: default_recent_sessions_limit(),
            profile_root: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SkillWriterAutomationRun {
    pub run_id: String,
    pub report: Value,
    pub ledger_record: AutomationRunLedgerRecord,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend_response: Option<AgentTaskResponse>,
}

/// One callable projectless post-session review suitable for host hooks.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct UserSessionAutomationOptions {
    #[serde(default)]
    pub session_reflector: SessionReflectorAutomationOptions,
    #[serde(default)]
    pub memory_curator: MemoryCuratorAutomationOptions,
    #[serde(default)]
    pub skill_writer: SkillWriterAutomationOptions,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UserSessionAutomationRun {
    pub session_reflector: SessionReflectorAutomationRun,
    pub memory_curator: MemoryCuratorAutomationRun,
    pub skill_writer: SkillWriterAutomationRun,
}

pub async fn run_user_session_automation_with_backend(
    profile_root: &std::path::Path,
    config: &AutomationConfig,
    backend: &dyn AgentTaskBackend,
    options: UserSessionAutomationOptions,
) -> Result<UserSessionAutomationRun> {
    let retrieval = production_user_automation_retrieval(profile_root).await;
    run_user_session_automation_with_backend_and_retrieval(
        profile_root,
        config,
        backend,
        retrieval.as_ref(),
        options,
    )
    .await
}

pub async fn run_user_session_automation_with_backend_and_retrieval(
    profile_root: &std::path::Path,
    config: &AutomationConfig,
    backend: &dyn AgentTaskBackend,
    retrieval: &dyn AutomationSessionRetrieval,
    options: UserSessionAutomationOptions,
) -> Result<UserSessionAutomationRun> {
    let session_reflector = run_user_session_reflector_with_backend_and_retrieval(
        profile_root,
        config,
        backend,
        retrieval,
        options.session_reflector,
    )
    .await?;
    let memory_curator =
        run_user_memory_curator_with_backend(profile_root, config, backend, options.memory_curator)
            .await?;
    let skill_writer = run_user_skill_writer_with_backend_and_retrieval(
        profile_root,
        config,
        backend,
        retrieval,
        options.skill_writer,
    )
    .await?;
    Ok(UserSessionAutomationRun {
        session_reflector,
        memory_curator,
        skill_writer,
    })
}

struct SkillWriterEvidenceBundle {
    profile_root: PathBuf,
    evidence: Value,
    evidence_hash: Option<String>,
}

enum SkillWriterEvidenceOutcome {
    Ready(SkillWriterEvidenceBundle),
    Skipped {
        reason: &'static str,
        evidence_hash: Option<String>,
    },
}

struct SessionReflectorEvidenceBundle {
    evidence: Value,
    evidence_hash: Option<String>,
}

enum SessionReflectorEvidenceOutcome {
    Ready(SessionReflectorEvidenceBundle),
    Skipped {
        reason: &'static str,
        evidence_hash: Option<String>,
    },
}

pub async fn run_session_reflector_with_backend(
    cg: &TraceDecay,
    config: &AutomationConfig,
    backend: &dyn AgentTaskBackend,
    options: SessionReflectorAutomationOptions,
) -> Result<SessionReflectorAutomationRun> {
    let retrieval = production_project_automation_retrieval(cg).await;
    run_session_reflector_with_backend_and_retrieval(
        cg,
        config,
        backend,
        retrieval.as_ref(),
        options,
    )
    .await
}

pub async fn run_session_reflector_with_backend_and_retrieval(
    cg: &TraceDecay,
    config: &AutomationConfig,
    backend: &dyn AgentTaskBackend,
    retrieval: &dyn AutomationSessionRetrieval,
    options: SessionReflectorAutomationOptions,
) -> Result<SessionReflectorAutomationRun> {
    let memory =
        MemoryApplication::new(cg.project_memory_owner()?, DatabaseFactStore::new(cg.db()))
            .map_err(|error| TraceDecayError::Config {
                message: format!(
                    "could not initialize project session reflector memory authority: {error}"
                ),
            })?;
    run_session_reflector_for_store(
        cg.store_layout().dashboard_root.clone(),
        cg.store_layout().sessions_db_path.clone(),
        retrieval,
        &memory,
        Some(cg.store_layout().project_root.as_path()),
        config,
        backend,
        options,
    )
    .await
}

/// Runs session reflection for projectless evidence and profile-level memory.
pub async fn run_user_session_reflector_with_backend(
    profile_root: &std::path::Path,
    config: &AutomationConfig,
    backend: &dyn AgentTaskBackend,
    options: SessionReflectorAutomationOptions,
) -> Result<SessionReflectorAutomationRun> {
    let retrieval = production_user_automation_retrieval(profile_root).await;
    run_user_session_reflector_with_backend_and_retrieval(
        profile_root,
        config,
        backend,
        retrieval.as_ref(),
        options,
    )
    .await
}

pub async fn run_user_session_reflector_with_backend_and_retrieval(
    profile_root: &std::path::Path,
    config: &AutomationConfig,
    backend: &dyn AgentTaskBackend,
    retrieval: &dyn AutomationSessionRetrieval,
    options: SessionReflectorAutomationOptions,
) -> Result<SessionReflectorAutomationRun> {
    if let SessionReflectorEvidenceOutcome::Skipped {
        reason,
        evidence_hash,
    } = build_session_reflector_evidence(retrieval, &options).await?
    {
        let run = AgentTaskRunContext::new(
            user_automation_root(profile_root),
            user_sessions_db_path(profile_root),
            options.run_id.clone(),
            "session_reflector",
            options.trigger,
            config,
            AgentTaskKind::SessionReflector,
        );
        return Ok(rejected_session_reflector_run(
            &run,
            config,
            reason,
            evidence_hash,
        ));
    }
    let memory_db = open_user_memory_db(profile_root).await?;
    let memory = MemoryApplication::new(FactOwnerV1::Profile, DatabaseFactStore::new(&memory_db))
        .map_err(|error| TraceDecayError::Config {
        message: format!(
            "could not initialize profile session reflector memory authority: {error}"
        ),
    })?;
    run_session_reflector_for_store(
        user_automation_root(profile_root),
        user_sessions_db_path(profile_root),
        retrieval,
        &memory,
        None,
        config,
        backend,
        options,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn run_session_reflector_for_store<A: FactCompatibilityStore>(
    dashboard_root: PathBuf,
    sessions_db_path: PathBuf,
    retrieval: &dyn AutomationSessionRetrieval,
    memory: &MemoryApplication<A>,
    digest_root: Option<&std::path::Path>,
    config: &AutomationConfig,
    backend: &dyn AgentTaskBackend,
    options: SessionReflectorAutomationOptions,
) -> Result<SessionReflectorAutomationRun> {
    let mut run = AgentTaskRunContext::new(
        dashboard_root,
        sessions_db_path.clone(),
        options.run_id.clone(),
        "session_reflector",
        options.trigger,
        config,
        AgentTaskKind::SessionReflector,
    );
    if let Some(reason @ ("automation_disabled" | "session_reflector_disabled")) =
        task_skip_reason(config, AgentTaskKind::SessionReflector)
    {
        return Ok(rejected_session_reflector_run(&run, config, reason, None));
    }
    let SessionReflectorEvidenceBundle {
        evidence,
        evidence_hash,
    } = match build_session_reflector_evidence(retrieval, &options).await? {
        SessionReflectorEvidenceOutcome::Ready(bundle) => bundle,
        SessionReflectorEvidenceOutcome::Skipped {
            reason,
            evidence_hash,
        } => {
            return Ok(rejected_session_reflector_run(
                &run,
                config,
                reason,
                evidence_hash,
            ));
        }
    };
    let _run_lock = match run.gate().await? {
        SchedulerGate::Proceed(lock) => lock,
        SchedulerGate::Skip(reason) => {
            return skipped_session_reflector_run(&run, reason, evidence_hash.clone()).await;
        }
    };
    if let Err(err) =
        super::outcomes::refresh_fact_outcomes(&run.dashboard_root, memory, current_timestamp())
            .await
    {
        eprintln!("[tracedecay] warning: failed to refresh fact outcomes: {err}");
    }

    let request = AgentTaskRequest::new(
        run.run_id.clone(),
        AgentTaskKind::SessionReflector,
        build_session_reflector_prompt(&evidence),
        evidence_hash.clone(),
        json!({
            "session_reflection_evidence": evidence,
            "apply": false,
        }),
    );
    let input_hash = Some(request.input_hash.clone());
    let finalizer = run.finalizer(input_hash.clone());
    let response = match finalizer
        .run_backend_or_fallback(backend, &request, evidence_hash.clone())
        .await?
    {
        BackendTaskRun::Response(response) => response,
        BackendTaskRun::Fallback(record) => {
            let record = *record;
            return Ok(SessionReflectorAutomationRun {
                run_id: record.run_id.clone(),
                report: failed_backend_fallback_report(&record),
                ledger_record: record,
                backend_response: None,
            });
        }
    };
    let (proposed_ops, proposals) = finalizer
        .response_output_array(
            &response,
            evidence_hash.clone(),
            "facts",
            "session reflector output must include a facts array",
        )
        .await?;
    let (report, record) = finalize_session_reflector_success(
        memory,
        digest_root,
        &finalizer,
        ProposedAgentOutput {
            response: &response,
            evidence: &evidence,
            evidence_hash,
            proposed_ops: &proposed_ops,
            proposals: &proposals,
        },
    )
    .await?;
    let record = finalizer
        .append_success_record(&request, &response, record)
        .await?;

    Ok(SessionReflectorAutomationRun {
        run_id: run.run_id,
        report,
        ledger_record: record,
        backend_response: Some(response),
    })
}

/// Validates and stages the `facts` half of a reflector (or combined) run,
/// returning the report plus the not-yet-appended success ledger record.
struct ProposedAgentOutput<'a> {
    response: &'a AgentTaskResponse,
    evidence: &'a Value,
    evidence_hash: Option<String>,
    proposed_ops: &'a Value,
    proposals: &'a [Value],
}

async fn finalize_session_reflector_success<A: FactCompatibilityStore>(
    memory: &MemoryApplication<A>,
    digest_root: Option<&std::path::Path>,
    finalizer: &AgentRunFinalizer<'_>,
    output: ProposedAgentOutput<'_>,
) -> Result<(Value, AutomationRunLedgerRecord)> {
    let ProposedAgentOutput {
        response,
        evidence,
        evidence_hash,
        proposed_ops,
        proposals,
    } = output;
    let dashboard_root = finalizer.dashboard_root();
    let run_id = finalizer.run_id();
    let (accepted_facts, rejected_facts) =
        validate_session_fact_proposals(memory, proposals, evidence).await?;
    let accepted_count = accepted_facts.len();
    let rejected_count = rejected_facts.len();
    let mut proposal_records = record_session_fact_proposals(
        memory,
        dashboard_root,
        run_id,
        evidence_hash.as_deref(),
        &accepted_facts,
        &rejected_facts,
    )
    .await?;
    let auto_apply_facts = MemoryApplyPolicy::should_apply(accepted_count);
    let applied_fact_proposals = if auto_apply_facts {
        let (records, _) = auto_apply_session_fact_proposals(
            memory,
            digest_root,
            dashboard_root,
            std::mem::take(&mut proposal_records),
        )
        .await?;
        records
    } else {
        Vec::new()
    };
    if auto_apply_facts {
        proposal_records.clone_from(&applied_fact_proposals);
    }
    let proposal_ids: Vec<String> = proposal_records
        .iter()
        .map(|record| record.proposal_id.clone())
        .collect();
    let applied_proposal_ids: Vec<String> = applied_fact_proposals
        .iter()
        .filter(|record| record.state == FactProposalState::Applied)
        .map(|record| record.proposal_id.clone())
        .collect();
    let applied_canonical_fact_ids: Vec<String> = applied_fact_proposals
        .iter()
        .filter(|record| record.state == FactProposalState::Applied)
        .filter_map(|record| record.applied_canonical_fact_id.clone())
        .collect();
    let applied_legacy_fact_ids: Vec<i64> = applied_fact_proposals
        .iter()
        .filter(|record| record.state == FactProposalState::Applied)
        .filter_map(|record| record.applied_fact_id)
        .collect();
    let applied_count = applied_proposal_ids.len();
    let fully_applied = accepted_count > 0 && applied_count == accepted_count;
    let mut session_fact_apply_policy =
        MemoryApplyPolicy::session_facts(accepted_count, applied_count, auto_apply_facts).to_json();
    if let Some(object) = session_fact_apply_policy.as_object_mut() {
        object.insert(
            "applied_proposal_ids".to_string(),
            json!(applied_proposal_ids),
        );
        object.insert(
            "applied_canonical_fact_ids".to_string(),
            json!(applied_canonical_fact_ids),
        );
        // Compatibility-only numeric mappings. Canonical IDs above are the
        // primary fact identities reported by session reflection.
        object.insert(
            "applied_legacy_fact_ids".to_string(),
            json!(applied_legacy_fact_ids),
        );
        object.insert(
            "applied_fact_ids".to_string(),
            json!(applied_legacy_fact_ids),
        );
        object.insert("applied_count".to_string(), json!(applied_count));
        object.insert("fully_applied".to_string(), json!(fully_applied));
    }
    let report = json!({
        "status": if auto_apply_facts { "auto_applied" } else { "needs_approval" },
        "dry_run": !auto_apply_facts,
        "task": "session_reflector",
        "evidence_hash": evidence_hash,
        "accepted_facts": accepted_facts,
        "rejected_facts": rejected_facts,
        "proposal_ids": proposal_ids,
        "proposal_records": proposal_records,
        "session_fact_apply_policy": session_fact_apply_policy,
    });
    let mut record = finalizer.success_record(
        response,
        report
            .get("evidence_hash")
            .and_then(Value::as_str)
            .map(str::to_string),
        Some(json!({
            "facts": proposed_ops.get("facts").cloned().unwrap_or_else(|| json!([])),
            "accepted_facts": report.get("accepted_facts").cloned().unwrap_or_else(|| json!([])),
            "rejected_facts": report.get("rejected_facts").cloned().unwrap_or_else(|| json!([])),
            "proposal_ids": report.get("proposal_ids").cloned().unwrap_or_else(|| json!([])),
        })),
        accepted_count,
        rejected_count,
    );
    record.applied_ops = report
        .pointer("/session_fact_apply_policy/applied_proposal_ids")
        .filter(|value| value.as_array().is_some_and(|items| !items.is_empty()))
        .cloned();
    record.rejected_ops = report.get("rejected_facts").cloned();
    let proposal_review_key = if auto_apply_facts {
        "applied_proposals"
    } else {
        "pending_proposals"
    };
    let proposal_review_ids = if auto_apply_facts {
        report
            .pointer("/session_fact_apply_policy/applied_proposal_ids")
            .cloned()
    } else {
        report.get("proposal_ids").cloned()
    }
    .unwrap_or_else(|| json!([]));
    let mut validation_report = json!({
        "status": report.get("status").cloned().unwrap_or_else(|| json!("needs_approval")),
        "dry_run": report.get("dry_run").cloned().unwrap_or(json!(true)),
        "accepted_count": accepted_count,
        "rejected_count": rejected_count,
        "session_fact_apply_policy": report.get("session_fact_apply_policy").cloned().unwrap_or_else(|| json!({})),
    });
    if let Some(object) = validation_report.as_object_mut() {
        object.insert(
            proposal_review_key.to_string(),
            json!({
            "proposal_ids": proposal_review_ids,
            "accepted_facts": report.get("accepted_facts").cloned().unwrap_or_else(|| json!([])),
            }),
        );
    }
    record.validation_report = Some(validation_report);
    Ok((report, record))
}

async fn validate_session_fact_proposals<A: FactCompatibilityStore>(
    memory: &MemoryApplication<A>,
    proposals: &[Value],
    evidence: &Value,
) -> Result<(Vec<Value>, Vec<Value>)> {
    validate_fact_proposals(memory, proposals, evidence).await
}

async fn auto_apply_session_fact_proposals<A: FactCompatibilityStore>(
    memory: &MemoryApplication<A>,
    digest_root: Option<&std::path::Path>,
    dashboard_root: &std::path::Path,
    proposal_records: Vec<FactProposalRecord>,
) -> Result<(Vec<FactProposalRecord>, bool)> {
    let mut applied = Vec::with_capacity(proposal_records.len());
    let mut newly_promoted = false;
    for record in proposal_records {
        if record.state != FactProposalState::PendingApproval {
            applied.push(record);
            continue;
        }
        let result = match apply_fact_proposal_with_result(
            memory,
            dashboard_root,
            &record.proposal_id,
            Some("session_reflector:auto_apply".to_string()),
        )
        .await
        {
            Ok(result) => result,
            Err(error) => {
                refresh_auto_apply_digest_for_new_promotions(memory, digest_root, newly_promoted)
                    .await;
                return Err(error);
            }
        };
        newly_promoted |= result.newly_promoted;
        applied.push(result.record);
    }
    refresh_auto_apply_digest_for_new_promotions(memory, digest_root, newly_promoted).await;
    Ok((applied, newly_promoted))
}

async fn refresh_auto_apply_digest_for_new_promotions<A: FactCompatibilityStore>(
    memory: &MemoryApplication<A>,
    digest_root: Option<&std::path::Path>,
    newly_promoted: bool,
) {
    if !newly_promoted {
        return;
    }
    if let Some(digest_root) = digest_root {
        crate::automation::memory_digest::refresh_memory_digest_after_memory_change(
            memory,
            digest_root,
        )
        .await;
    }
}

pub async fn run_skill_writer_with_backend(
    cg: &TraceDecay,
    config: &AutomationConfig,
    backend: &dyn AgentTaskBackend,
    options: SkillWriterAutomationOptions,
) -> Result<SkillWriterAutomationRun> {
    let retrieval = production_project_automation_retrieval(cg).await;
    run_skill_writer_with_backend_and_retrieval(cg, config, backend, retrieval.as_ref(), options)
        .await
}

pub async fn run_skill_writer_with_backend_and_retrieval(
    cg: &TraceDecay,
    config: &AutomationConfig,
    backend: &dyn AgentTaskBackend,
    retrieval: &dyn AutomationSessionRetrieval,
    options: SkillWriterAutomationOptions,
) -> Result<SkillWriterAutomationRun> {
    run_skill_writer_for_store(
        cg.store_layout().dashboard_root.clone(),
        cg.store_layout().sessions_db_path.clone(),
        retrieval,
        Some(cg.project_root()),
        config,
        backend,
        options,
    )
    .await
}

/// Runs skill writing from profile-level projectless session evidence.
pub async fn run_user_skill_writer_with_backend(
    profile_root: &std::path::Path,
    config: &AutomationConfig,
    backend: &dyn AgentTaskBackend,
    options: SkillWriterAutomationOptions,
) -> Result<SkillWriterAutomationRun> {
    let retrieval = production_user_automation_retrieval(profile_root).await;
    run_user_skill_writer_with_backend_and_retrieval(
        profile_root,
        config,
        backend,
        retrieval.as_ref(),
        options,
    )
    .await
}

pub async fn run_user_skill_writer_with_backend_and_retrieval(
    profile_root: &std::path::Path,
    config: &AutomationConfig,
    backend: &dyn AgentTaskBackend,
    retrieval: &dyn AutomationSessionRetrieval,
    mut options: SkillWriterAutomationOptions,
) -> Result<SkillWriterAutomationRun> {
    options.profile_root = Some(profile_root.to_path_buf());
    run_skill_writer_for_store(
        user_automation_root(profile_root),
        user_sessions_db_path(profile_root),
        retrieval,
        None,
        config,
        backend,
        options,
    )
    .await
}

async fn run_skill_writer_for_store(
    dashboard_root: PathBuf,
    sessions_db_path: PathBuf,
    retrieval: &dyn AutomationSessionRetrieval,
    analytics_project_root: Option<&std::path::Path>,
    config: &AutomationConfig,
    backend: &dyn AgentTaskBackend,
    options: SkillWriterAutomationOptions,
) -> Result<SkillWriterAutomationRun> {
    let mut run = AgentTaskRunContext::new(
        dashboard_root,
        sessions_db_path.clone(),
        options.run_id.clone(),
        "skill_writer",
        options.trigger,
        config,
        AgentTaskKind::SkillWriter,
    );
    if let Some(reason @ ("automation_disabled" | "skill_writer_disabled")) =
        task_skip_reason(config, AgentTaskKind::SkillWriter)
    {
        return Ok(rejected_skill_writer_run(&run, config, reason, None));
    }
    let evidence_bundle =
        match build_skill_writer_evidence(retrieval, analytics_project_root, options).await? {
            SkillWriterEvidenceOutcome::Ready(bundle) => bundle,
            SkillWriterEvidenceOutcome::Skipped {
                reason,
                evidence_hash,
            } => {
                return Ok(rejected_skill_writer_run(
                    &run,
                    config,
                    reason,
                    evidence_hash,
                ));
            }
        };
    let SkillWriterEvidenceBundle {
        profile_root,
        evidence,
        evidence_hash,
    } = evidence_bundle;
    let _run_lock = match run.gate().await? {
        SchedulerGate::Proceed(lock) => lock,
        SchedulerGate::Skip(reason) => {
            return skipped_skill_writer_run(&run, reason, evidence_hash.clone()).await;
        }
    };

    // Refresh adoption outcomes of previously approved skills so this run's
    // feedback artifact reports real post-approval quality. Best effort: a
    // stale snapshot must not block skill writing.
    if let Err(err) = super::outcomes::refresh_skill_outcomes(
        &profile_root,
        &run.dashboard_root,
        current_timestamp(),
    )
    .await
    {
        eprintln!("[tracedecay] warning: failed to refresh skill outcomes: {err}");
    }

    let activation_policy = skill_writer_activation_policy(config);
    let request = AgentTaskRequest::new(
        run.run_id.clone(),
        AgentTaskKind::SkillWriter,
        build_skill_writer_prompt(&evidence),
        evidence_hash.clone(),
        json!({
            "skill_writer_evidence": evidence,
            "apply": false,
            "activation_policy": activation_policy,
        }),
    );
    let input_hash = Some(request.input_hash.clone());
    let finalizer = run.finalizer(input_hash.clone());
    let response = match finalizer
        .run_backend_or_fallback(backend, &request, evidence_hash.clone())
        .await?
    {
        BackendTaskRun::Response(response) => response,
        BackendTaskRun::Fallback(record) => {
            let record = *record;
            return Ok(SkillWriterAutomationRun {
                run_id: record.run_id.clone(),
                report: failed_backend_fallback_report(&record),
                ledger_record: record,
                backend_response: None,
            });
        }
    };
    let (proposed_ops, proposals) = finalizer
        .response_output_array(
            &response,
            evidence_hash.clone(),
            "skills",
            "skill writer output must include a skills array",
        )
        .await?;
    let (report, record) = finalize_skill_writer_success(
        &finalizer,
        &profile_root,
        activation_policy,
        ProposedAgentOutput {
            response: &response,
            evidence: &evidence,
            evidence_hash,
            proposed_ops: &proposed_ops,
            proposals: &proposals,
        },
    )
    .await?;
    let record = finalizer
        .append_success_record(&request, &response, record)
        .await?;

    Ok(SkillWriterAutomationRun {
        run_id: run.run_id,
        report,
        ledger_record: record,
        backend_response: Some(response),
    })
}

/// Validates and stages the `skills` half of a skill-writer (or combined)
/// run, returning the report plus the not-yet-appended success ledger record.
/// A skill-proposal validation failure appends a failed record before
/// bubbling the error.
async fn finalize_skill_writer_success(
    finalizer: &AgentRunFinalizer<'_>,
    profile_root: &std::path::Path,
    activation_policy: &'static str,
    output: ProposedAgentOutput<'_>,
) -> Result<(Value, AutomationRunLedgerRecord)> {
    let ProposedAgentOutput {
        response,
        evidence,
        evidence_hash,
        proposed_ops,
        proposals,
    } = output;
    let config = finalizer.config();
    let run_id = finalizer.run_id();
    let proposal_outcome = match validate_and_apply_skill_proposals(
        profile_root,
        run_id,
        proposals,
        config.auto_enable_skills,
    )
    .await
    {
        Ok(result) => result,
        Err(err) => {
            finalizer
                .append_failed_record(
                    response.model.clone(),
                    evidence_hash,
                    Some(proposed_ops.clone()),
                    err.to_string(),
                )
                .await?;
            return Err(err);
        }
    };
    let accepted_count = proposal_outcome.created.len()
        + proposal_outcome.updated.len()
        + proposal_outcome.consolidations.len();
    let rejected_count = proposal_outcome.rejected.len();
    let report = json!({
        "status": if config.auto_enable_skills { "auto_enabled" } else { "needs_approval" },
        "dry_run": !config.auto_enable_skills,
        "task": "skill_writer",
        "evidence_hash": evidence_hash,
        "activation_policy": activation_policy,
        "created_skills": proposal_outcome.created,
        "updated_skills": proposal_outcome.updated,
        "staged_consolidations": proposal_outcome.consolidations,
        "rejected_skills": proposal_outcome.rejected,
        "skill_improvement_recommendations": evidence
            .get("skill_improvement_recommendations")
            .cloned()
            .unwrap_or_else(|| json!([])),
    });
    let mut record = finalizer.success_record(
        response,
        report
            .get("evidence_hash")
            .and_then(Value::as_str)
            .map(str::to_string),
        Some(json!({
            "skills": proposed_ops.get("skills").cloned().unwrap_or_else(|| json!([])),
            "created_skills": report.get("created_skills").cloned().unwrap_or_else(|| json!([])),
            "updated_skills": report.get("updated_skills").cloned().unwrap_or_else(|| json!([])),
            "staged_consolidations": report.get("staged_consolidations").cloned().unwrap_or_else(|| json!([])),
            "rejected_skills": report.get("rejected_skills").cloned().unwrap_or_else(|| json!([])),
        })),
        accepted_count,
        rejected_count,
    );
    record.applied_ops = Some(json!({
        "created_skills": report.get("created_skills").cloned().unwrap_or_else(|| json!([])),
        "updated_skills": report.get("updated_skills").cloned().unwrap_or_else(|| json!([])),
        "staged_consolidations": report.get("staged_consolidations").cloned().unwrap_or_else(|| json!([])),
    }));
    record.rejected_ops = report.get("rejected_skills").cloned();
    record.validation_report = Some(json!({
        "status": report.get("status").cloned().unwrap_or_else(|| json!("needs_approval")),
        "dry_run": !config.auto_enable_skills,
        "activation_policy": activation_policy,
        "accepted_count": accepted_count,
        "rejected_count": rejected_count,
    }));
    Ok((report, record))
}

async fn build_session_reflector_evidence(
    retrieval: &dyn AutomationSessionRetrieval,
    options: &SessionReflectorAutomationOptions,
) -> Result<SessionReflectorEvidenceOutcome> {
    let provider = normalized_non_empty(&options.provider).unwrap_or_else(default_session_provider);
    let query =
        normalized_non_empty(&options.query).unwrap_or_else(default_session_reflection_query);
    let evidence_limit = options.evidence_limit.clamp(1, 50);
    let session_id = options.session_id.as_deref().and_then(normalized_non_empty);
    let source = options.source.as_deref().and_then(normalized_non_empty);
    let role = options.role.as_deref().and_then(normalized_non_empty);

    if source.is_some()
        || role.is_some()
        || options.start_time.is_some()
        || options.end_time.is_some()
    {
        return Ok(SessionReflectorEvidenceOutcome::Skipped {
            reason: "session_evidence_filter_unavailable",
            evidence_hash: None,
        });
    }
    let include_recent_sessions = options.include_recent_sessions
        && session_reflector_replay_allowed(
            options.scope,
            session_id.as_deref(),
            source.as_deref(),
            role.as_deref(),
            options.start_time,
            options.end_time,
        );
    let filters = AutomationEvidenceFilters {
        provider: &provider,
        session_id: session_id.as_deref(),
        include_summaries: options.include_summaries,
        evidence_limit,
        include_recent_sessions,
        recent_sessions_limit: options.recent_sessions_limit,
        role: role.as_deref(),
        start_time: options.start_time,
        end_time: options.end_time,
        sort: options.sort,
    };
    let retrieval =
        retrieve_automation_session_evidence(retrieval, &query, options.scope, filters).await?;
    let serialized = match retrieval {
        AutomationTemporalRetrieval::Complete(evidence) => {
            match validate_complete_evidence(&evidence) {
                Ok(()) => serialize_automation_temporal_evidence(evidence, filters),
                Err(reason) => {
                    return Ok(SessionReflectorEvidenceOutcome::Skipped {
                        reason,
                        evidence_hash: None,
                    });
                }
            }
        }
        AutomationTemporalRetrieval::CompleteZero => serialize_automation_temporal_evidence(
            AutomationTemporalEvidence {
                items: Vec::new(),
                coverage: TemporalCoverageCountsV1::default(),
            },
            filters,
        ),
        AutomationTemporalRetrieval::Rejected(reason) => {
            return Ok(SessionReflectorEvidenceOutcome::Skipped {
                reason,
                evidence_hash: None,
            });
        }
    };
    let SerializedAutomationEvidence {
        hits,
        recent_session_slices,
        coverage,
        ..
    } = serialized;
    let evidence = json!({
        "evidence_mode": evidence_mode_label(recent_session_slices.is_some()),
        "temporal_mode": "forensic",
        "temporal_coverage": coverage,
        "provider": provider,
        "query": query,
        "scope": options.scope,
        "session_id": session_id,
        "include_summaries": options.include_summaries,
        "sort": options.sort,
        "source": source,
        "role": role,
        "start_time": options.start_time,
        "end_time": options.end_time,
        "recent_session_slices": recent_session_slices,
        "hits": hits,
    });
    let evidence_hash = Some(canonical_evidence_hash(&evidence));
    let has_grep_hits = evidence
        .get("hits")
        .and_then(Value::as_array)
        .is_some_and(|hits| !hits.is_empty());
    let has_replay_sessions = evidence
        .pointer("/recent_session_slices/sessions")
        .and_then(Value::as_array)
        .is_some_and(|sessions| !sessions.is_empty());
    if !has_grep_hits && !has_replay_sessions {
        return Ok(SessionReflectorEvidenceOutcome::Skipped {
            reason: "no_session_evidence",
            evidence_hash,
        });
    }

    Ok(SessionReflectorEvidenceOutcome::Ready(
        SessionReflectorEvidenceBundle {
            evidence,
            evidence_hash,
        },
    ))
}

async fn build_skill_writer_evidence(
    retrieval: &dyn AutomationSessionRetrieval,
    analytics_project_root: Option<&std::path::Path>,
    options: SkillWriterAutomationOptions,
) -> Result<SkillWriterEvidenceOutcome> {
    let profile_root = match options.profile_root {
        Some(path) => path,
        None => crate::storage::default_profile_root()?,
    };
    let provider =
        normalized_non_empty(&options.provider).unwrap_or_else(default_skill_writer_provider);
    let query = normalized_non_empty(&options.query).unwrap_or_else(default_skill_writer_query);
    let evidence_limit = options.evidence_limit.clamp(1, 50);

    let filters = AutomationEvidenceFilters {
        provider: &provider,
        session_id: None,
        include_summaries: true,
        evidence_limit,
        include_recent_sessions: options.include_recent_sessions,
        recent_sessions_limit: options.recent_sessions_limit,
        role: None,
        start_time: None,
        end_time: None,
        sort: LcmGrepSort::Recency,
    };
    let retrieval =
        retrieve_automation_session_evidence(retrieval, &query, LcmScope::All, filters).await?;
    let serialized = match retrieval {
        AutomationTemporalRetrieval::Complete(evidence) => {
            match validate_complete_evidence(&evidence) {
                Ok(()) => serialize_automation_temporal_evidence(evidence, filters),
                Err(reason) => {
                    return Ok(SkillWriterEvidenceOutcome::Skipped {
                        reason,
                        evidence_hash: None,
                    });
                }
            }
        }
        AutomationTemporalRetrieval::CompleteZero => serialize_automation_temporal_evidence(
            AutomationTemporalEvidence {
                items: Vec::new(),
                coverage: TemporalCoverageCountsV1::default(),
            },
            filters,
        ),
        AutomationTemporalRetrieval::Rejected(reason) => {
            return Ok(SkillWriterEvidenceOutcome::Skipped {
                reason,
                evidence_hash: None,
            });
        }
    };
    let SerializedAutomationEvidence {
        hits,
        recent_session_slices,
        tool_usage,
        coverage,
    } = serialized;
    let existing_skills = list_managed_skills(&profile_root).await?;
    if let Some(project_root) = analytics_project_root {
        let global_db_path = crate::global_db::global_db_path();
        let global_db = match global_db_path.as_deref() {
            Some(path) => GlobalDb::open_read_only_at(path).await,
            None => None,
        };
        ingest_project_analytics_events(
            &profile_root,
            project_root,
            global_db.as_ref(),
            SKILL_ANALYTICS_IMPORT_LIMIT,
        )
        .await?;
    }
    let skill_usage_summaries = summarize_skill_usage(&profile_root, &existing_skills).await?;
    let stale_recommendations = stale_skill_recommendations(
        &skill_usage_summaries,
        current_timestamp(),
        60 * 60 * 24 * 90,
    );
    let underused_tool_families =
        underused_tool_family_signals(tool_usage.iter().map(|row| ToolUsageObservation {
            tool_names: row.tool_names.as_deref(),
            metadata_json: row.metadata_json.as_deref(),
            text: row.text.as_deref(),
        }));
    let overlap_candidates =
        skill_overlap_candidates(&existing_skills, DEFAULT_SKILL_OVERLAP_LIMIT);
    let compatibility_hits = hits
        .iter()
        .map(CanonicalEvidenceHit::compatibility_hit)
        .collect::<Vec<_>>();
    let skill_improvement_recommendations = skill_improvement_recommendations(
        &compatibility_hits,
        &skill_usage_summaries,
        &stale_recommendations,
        &underused_tool_families,
        &overlap_candidates,
    );
    let evidence = json!({
        "evidence_mode": evidence_mode_label(recent_session_slices.is_some()),
        "temporal_mode": "forensic",
        "temporal_coverage": coverage,
        "provider": provider,
        "query": query,
        "recent_session_slices": recent_session_slices,
        "hits": hits,
        "skill_usage_summaries": skill_usage_summaries,
        "stale_recommendations": stale_recommendations,
        "underused_tool_families": underused_tool_families,
        "skill_overlap_candidates": overlap_candidates,
        "skill_improvement_recommendations": skill_improvement_recommendations,
        "existing_managed_skills": existing_skills
            .iter()
            .map(|skill| json!({
                "id": skill.metadata.id,
                "title": skill.metadata.title,
                "summary": skill.metadata.summary,
                "category": skill.metadata.category,
                "state": skill.metadata.state,
                "pinned": skill.metadata.pinned,
                "checksum": skill.metadata.checksum,
                "updated_at": skill.metadata.updated_at,
                "body_markdown": truncate_chars_for_prompt(&skill.body_markdown, 4000),
                "support_files": skill.support_files
                    .iter()
                    .map(skill_writer_support_file_evidence)
                    .collect::<Vec<_>>(),
            }))
            .collect::<Vec<_>>(),
    });
    let evidence_hash = Some(canonical_evidence_hash(&evidence));
    let has_grep_hits = evidence
        .get("hits")
        .and_then(Value::as_array)
        .is_some_and(|hits| !hits.is_empty());
    let has_replay_sessions = evidence
        .pointer("/recent_session_slices/sessions")
        .and_then(Value::as_array)
        .is_some_and(|sessions| !sessions.is_empty());
    if !has_grep_hits && !has_replay_sessions {
        return Ok(SkillWriterEvidenceOutcome::Skipped {
            reason: "no_skill_writer_evidence",
            evidence_hash,
        });
    }

    Ok(SkillWriterEvidenceOutcome::Ready(
        SkillWriterEvidenceBundle {
            profile_root,
            evidence,
            evidence_hash,
        },
    ))
}

async fn skipped_session_reflector_run(
    run: &AgentTaskRunContext<'_>,
    reason: &str,
    evidence_hash: Option<String>,
) -> Result<SessionReflectorAutomationRun> {
    let (report, record) = run
        .skipped_parts(evidence_hash, reason, Some("session_reflector"))
        .await?;
    Ok(SessionReflectorAutomationRun {
        run_id: run.run_id.clone(),
        report,
        ledger_record: record,
        backend_response: None,
    })
}

async fn skipped_skill_writer_run(
    run: &AgentTaskRunContext<'_>,
    reason: &str,
    evidence_hash: Option<String>,
) -> Result<SkillWriterAutomationRun> {
    let (report, record) = run
        .skipped_parts(evidence_hash, reason, Some("skill_writer"))
        .await?;
    Ok(SkillWriterAutomationRun {
        run_id: run.run_id.clone(),
        report,
        ledger_record: record,
        backend_response: None,
    })
}

fn rejected_session_reflector_run(
    run: &AgentTaskRunContext<'_>,
    config: &AutomationConfig,
    reason: &str,
    evidence_hash: Option<String>,
) -> SessionReflectorAutomationRun {
    let (report, record) = unpersisted_rejected_parts(
        run,
        config,
        AgentTaskKind::SessionReflector,
        reason,
        evidence_hash,
        "session_reflector",
    );
    SessionReflectorAutomationRun {
        run_id: run.run_id.clone(),
        report,
        ledger_record: record,
        backend_response: None,
    }
}

fn rejected_skill_writer_run(
    run: &AgentTaskRunContext<'_>,
    config: &AutomationConfig,
    reason: &str,
    evidence_hash: Option<String>,
) -> SkillWriterAutomationRun {
    let (report, record) = unpersisted_rejected_parts(
        run,
        config,
        AgentTaskKind::SkillWriter,
        reason,
        evidence_hash,
        "skill_writer",
    );
    SkillWriterAutomationRun {
        run_id: run.run_id.clone(),
        report,
        ledger_record: record,
        backend_response: None,
    }
}

fn unpersisted_rejected_parts(
    run: &AgentTaskRunContext<'_>,
    config: &AutomationConfig,
    task: AgentTaskKind,
    reason: &str,
    evidence_hash: Option<String>,
    report_task: &'static str,
) -> (Value, AutomationRunLedgerRecord) {
    let completed_at = current_timestamp().to_string();
    let contract = agent_task_contract(task);
    let report = json!({
        "status": "skipped",
        "reason": reason,
        "dry_run": true,
        "task": report_task,
    });
    let record = AutomationRunLedgerRecord {
        schema_version: 2,
        run_id: run.run_id.clone(),
        trigger: run.trigger,
        task,
        task_key: Some(task_key(task).to_string()),
        backend: config.backend.as_str().to_string(),
        host_mode: Some(config.host_mode.as_str().to_string()),
        prompt_version: Some(prompt_version(task).to_string()),
        response_schema: Some(contract.response_schema),
        strict_json: Some(contract.strict_json),
        model: None,
        status: AutomationRunStatus::Skipped,
        evidence_hash,
        input_hash: None,
        output_hash: None,
        proposed_ops: None,
        applied_ops: None,
        rejected_ops: None,
        validation_report: None,
        reviewed_count: 0,
        accepted_count: 0,
        rejected_count: 0,
        skipped_count: 1,
        error: Some(reason.to_string()),
        error_classification: None,
        error_retryable: None,
        fallback_status: Some(reason.to_string()),
        report_ref: Some(json!({
            "dashboard_runs": "/api/plugins/holographic/curation/runs",
            "run_id": run.run_id,
        })),
        artifacts: Vec::new(),
        started_at: run.started_at().to_string(),
        completed_at,
    };
    (report, record)
}

/// Options for the scheduler-only combined reflector+skill pass. Manual
/// (CLI/dashboard) runs stay per-task; this path exists so one backend call
/// can serve both tasks when they are due in the same scheduler tick.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CombinedReviewAutomationOptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default)]
    pub session_reflector: SessionReflectorAutomationOptions,
    #[serde(default)]
    pub skill_writer: SkillWriterAutomationOptions,
    #[serde(default = "scheduler_trigger")]
    pub trigger: AutomationTrigger,
}

fn scheduler_trigger() -> AutomationTrigger {
    AutomationTrigger::Scheduler
}

impl Default for CombinedReviewAutomationOptions {
    fn default() -> Self {
        Self {
            run_id: None,
            session_reflector: SessionReflectorAutomationOptions::default(),
            skill_writer: SkillWriterAutomationOptions::default(),
            trigger: AutomationTrigger::Scheduler,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CombinedReviewAutomationRun {
    pub run_id: String,
    pub session_reflector: SessionReflectorAutomationRun,
    pub skill_writer: SkillWriterAutomationRun,
}

/// Outcome of attempting the combined dispatch. `NotCombined` means the
/// caller should fall back to the normal sequential per-task runs; nothing
/// was recorded and no locks are held.
#[derive(Debug)]
pub enum CombinedReviewDispatch {
    Ran(Box<CombinedReviewAutomationRun>),
    RecordedFailure {
        run: Box<CombinedReviewAutomationRun>,
        error: TraceDecayError,
    },
    NotCombined {
        reason: &'static str,
    },
}

/// Runs the session reflector and the skill writer as one combined backend
/// call when both tasks are due in the same scheduler tick.
///
/// Both per-task scheduler gates must proceed (their locks are held for the
/// whole combined run) and both evidence bundles must be available;
/// otherwise the dispatch reports `NotCombined` and the caller runs the
/// tasks sequentially as before. On a combined run, two ledger records are
/// appended — one per task, so per-task last-run bookkeeping and the
/// dashboard scheduler status stay coherent — sharing the combined request's
/// `input_hash` and a `combined_run_id` correlation in `report_ref`, with
/// `prompt_version` set to the combined contract's version.
pub async fn run_combined_review_with_backend(
    cg: &TraceDecay,
    config: &AutomationConfig,
    backend: &dyn AgentTaskBackend,
    options: CombinedReviewAutomationOptions,
) -> Result<CombinedReviewDispatch> {
    let retrieval = production_project_automation_retrieval(cg).await;
    run_combined_review_for_retrieval(cg, config, backend, retrieval.as_ref(), options).await
}

pub async fn run_combined_review_with_backend_and_retrieval(
    cg: &TraceDecay,
    config: &AutomationConfig,
    backend: &dyn AgentTaskBackend,
    retrieval: &dyn AutomationSessionRetrieval,
    options: CombinedReviewAutomationOptions,
) -> Result<CombinedReviewDispatch> {
    run_combined_review_for_retrieval(cg, config, backend, retrieval, options).await
}

async fn run_combined_review_for_retrieval(
    cg: &TraceDecay,
    config: &AutomationConfig,
    backend: &dyn AgentTaskBackend,
    retrieval: &dyn AutomationSessionRetrieval,
    options: CombinedReviewAutomationOptions,
) -> Result<CombinedReviewDispatch> {
    if !config.combine_due_tasks {
        return Ok(CombinedReviewDispatch::NotCombined {
            reason: "combined_mode_disabled",
        });
    }
    let dashboard_root = cg.store_layout().dashboard_root.clone();
    let sessions_db_path = cg.store_layout().sessions_db_path.clone();
    let memory =
        MemoryApplication::new(cg.project_memory_owner()?, DatabaseFactStore::new(cg.db()))
            .map_err(|error| TraceDecayError::Config {
                message: format!("could not initialize combined review memory authority: {error}"),
            })?;
    let started_at = current_timestamp().to_string();

    let reflector_bundle =
        match build_session_reflector_evidence(retrieval, &options.session_reflector).await? {
            SessionReflectorEvidenceOutcome::Ready(bundle) => bundle,
            SessionReflectorEvidenceOutcome::Skipped { .. } => {
                return Ok(CombinedReviewDispatch::NotCombined {
                    reason: "session_reflector_evidence_unavailable",
                });
            }
        };
    let skill_bundle =
        match build_skill_writer_evidence(retrieval, Some(cg.project_root()), options.skill_writer)
            .await?
        {
            SkillWriterEvidenceOutcome::Ready(bundle) => bundle,
            SkillWriterEvidenceOutcome::Skipped { .. } => {
                return Ok(CombinedReviewDispatch::NotCombined {
                    reason: "skill_writer_evidence_unavailable",
                });
            }
        };

    let (reflector_gate, _) = task_run_gate(
        config,
        &dashboard_root,
        &sessions_db_path,
        AgentTaskKind::SessionReflector,
        options.trigger,
    )
    .await?;
    let _reflector_lock = match reflector_gate {
        SchedulerGate::Proceed(lock) => lock,
        SchedulerGate::Skip(_) => {
            return Ok(CombinedReviewDispatch::NotCombined {
                reason: "session_reflector_not_due",
            });
        }
    };
    let (skill_gate, _) = task_run_gate(
        config,
        &dashboard_root,
        &sessions_db_path,
        AgentTaskKind::SkillWriter,
        options.trigger,
    )
    .await?;
    let _skill_lock = match skill_gate {
        SchedulerGate::Proceed(lock) => lock,
        SchedulerGate::Skip(_) => {
            return Ok(CombinedReviewDispatch::NotCombined {
                reason: "skill_writer_not_due",
            });
        }
    };
    if let Err(err) =
        super::outcomes::refresh_fact_outcomes(&dashboard_root, &memory, current_timestamp()).await
    {
        eprintln!("[tracedecay] warning: failed to refresh fact outcomes: {err}");
    }

    let run_id = options
        .run_id
        .unwrap_or_else(|| generated_run_id("combined_review"));
    let reflector_run_id = format!("{run_id}_facts");
    let skill_run_id = format!("{run_id}_skills");
    let activation_policy = skill_writer_activation_policy(config);
    let combined_evidence_hash = Some(canonical_evidence_hash(&json!({
        "session_reflection_evidence": reflector_bundle.evidence,
        "skill_writer_evidence": skill_bundle.evidence,
    })));
    let request = AgentTaskRequest::new(
        run_id.clone(),
        AgentTaskKind::CombinedReview,
        build_combined_review_prompt(&reflector_bundle.evidence, &skill_bundle.evidence),
        combined_evidence_hash,
        json!({
            "session_reflection_evidence": reflector_bundle.evidence,
            "skill_writer_evidence": skill_bundle.evidence,
            "apply": false,
            "activation_policy": activation_policy,
        }),
    );
    let input_hash = Some(request.input_hash.clone());
    let reflector_finalizer = AgentRunFinalizer::new(
        &dashboard_root,
        &reflector_run_id,
        options.trigger,
        config,
        AgentTaskKind::SessionReflector,
        &started_at,
        input_hash.clone(),
    )
    .for_combined_run(run_id.clone());
    let skill_finalizer = AgentRunFinalizer::new(
        &dashboard_root,
        &skill_run_id,
        options.trigger,
        config,
        AgentTaskKind::SkillWriter,
        &started_at,
        input_hash,
    )
    .for_combined_run(run_id.clone());

    let retry_policy = BackendRetryPolicy::from_timeout_secs(config.timeout_secs);
    let response = match run_agent_task_with_retry(backend, &request, &retry_policy).await {
        Ok(response) => response,
        Err(err) => {
            let reflector_record = reflector_finalizer
                .append_backend_fallback_record(
                    reflector_bundle.evidence_hash.clone(),
                    err.to_string(),
                )
                .await?;
            let skill_record = skill_finalizer
                .append_backend_fallback_record(skill_bundle.evidence_hash.clone(), err.to_string())
                .await?;
            return Ok(CombinedReviewDispatch::Ran(Box::new(
                CombinedReviewAutomationRun {
                    run_id,
                    session_reflector: SessionReflectorAutomationRun {
                        run_id: reflector_record.run_id.clone(),
                        report: failed_backend_fallback_report(&reflector_record),
                        ledger_record: reflector_record,
                        backend_response: None,
                    },
                    skill_writer: SkillWriterAutomationRun {
                        run_id: skill_record.run_id.clone(),
                        report: failed_backend_fallback_report(&skill_record),
                        ledger_record: skill_record,
                        backend_response: None,
                    },
                },
            )));
        }
    };

    let output = match response
        .output_json
        .clone()
        .map_or_else(|| extract_json_object_prefix(&response.output_text), Ok)
    {
        Ok(output) => output,
        Err(err) => {
            let (reflector_record, skill_record) = append_combined_failed_records(
                &reflector_finalizer,
                &skill_finalizer,
                &response,
                &reflector_bundle,
                &skill_bundle,
                None,
                &err,
            )
            .await?;
            return Ok(CombinedReviewDispatch::RecordedFailure {
                run: combined_failed_run(run_id, reflector_record, skill_record, &response),
                error: err,
            });
        }
    };
    let facts = output.get("facts").and_then(Value::as_array).cloned();
    let skills = output.get("skills").and_then(Value::as_array).cloned();
    let (Some(facts), Some(skills)) = (facts, skills) else {
        let err = TraceDecayError::Config {
            message: "combined review output must include facts and skills arrays".to_string(),
        };
        let (reflector_record, skill_record) = append_combined_failed_records(
            &reflector_finalizer,
            &skill_finalizer,
            &response,
            &reflector_bundle,
            &skill_bundle,
            Some(&output),
            &err,
        )
        .await?;
        return Ok(CombinedReviewDispatch::RecordedFailure {
            run: combined_failed_run(run_id, reflector_record, skill_record, &response),
            error: err,
        });
    };
    if !facts.is_empty() || !skills.is_empty() {
        let err = TraceDecayError::Config {
            message: "combined review proposals require an atomic apply authority".to_string(),
        };
        let (reflector_record, skill_record) = append_combined_failed_records(
            &reflector_finalizer,
            &skill_finalizer,
            &response,
            &reflector_bundle,
            &skill_bundle,
            Some(&output),
            &err,
        )
        .await?;
        return Ok(CombinedReviewDispatch::RecordedFailure {
            run: combined_failed_run(run_id, reflector_record, skill_record, &response),
            error: err,
        });
    }

    let (reflector_report, reflector_record) = finalize_session_reflector_success(
        &memory,
        Some(cg.store_layout().project_root.as_path()),
        &reflector_finalizer,
        ProposedAgentOutput {
            response: &response,
            evidence: &reflector_bundle.evidence,
            evidence_hash: reflector_bundle.evidence_hash.clone(),
            proposed_ops: &output,
            proposals: &facts,
        },
    )
    .await?;

    let (skill_report, skill_record) = finalize_skill_writer_success(
        &skill_finalizer,
        &skill_bundle.profile_root,
        activation_policy,
        ProposedAgentOutput {
            response: &response,
            evidence: &skill_bundle.evidence,
            evidence_hash: skill_bundle.evidence_hash.clone(),
            proposed_ops: &output,
            proposals: &skills,
        },
    )
    .await?;
    let skill_record = skill_finalizer
        .append_success_record(&request, &response, skill_record)
        .await?;
    let reflector_record = match reflector_finalizer
        .append_success_record(&request, &response, reflector_record)
        .await
    {
        Ok(record) => record,
        Err(error) => {
            let _ = skill_finalizer
                .append_failed_record(
                    response.model.clone(),
                    skill_bundle.evidence_hash.clone(),
                    Some(output.clone()),
                    format!("combined reflector ledger append failed: {error}"),
                )
                .await;
            return Err(error);
        }
    };

    Ok(CombinedReviewDispatch::Ran(Box::new(
        CombinedReviewAutomationRun {
            run_id,
            session_reflector: SessionReflectorAutomationRun {
                run_id: reflector_run_id,
                report: reflector_report,
                ledger_record: reflector_record,
                backend_response: Some(response.clone()),
            },
            skill_writer: SkillWriterAutomationRun {
                run_id: skill_run_id,
                report: skill_report,
                ledger_record: skill_record,
                backend_response: Some(response),
            },
        },
    )))
}

fn combined_failed_run(
    run_id: String,
    reflector_record: AutomationRunLedgerRecord,
    skill_record: AutomationRunLedgerRecord,
    response: &AgentTaskResponse,
) -> Box<CombinedReviewAutomationRun> {
    Box::new(CombinedReviewAutomationRun {
        run_id,
        session_reflector: SessionReflectorAutomationRun {
            run_id: reflector_record.run_id.clone(),
            report: failed_backend_fallback_report(&reflector_record),
            ledger_record: reflector_record,
            backend_response: Some(response.clone()),
        },
        skill_writer: SkillWriterAutomationRun {
            run_id: skill_record.run_id.clone(),
            report: failed_backend_fallback_report(&skill_record),
            ledger_record: skill_record,
            backend_response: Some(response.clone()),
        },
    })
}

/// Records the same failure for both halves of a combined run so each task's
/// cooldown/retry bookkeeping sees it.
async fn append_combined_failed_records(
    reflector_finalizer: &AgentRunFinalizer<'_>,
    skill_finalizer: &AgentRunFinalizer<'_>,
    response: &AgentTaskResponse,
    reflector_bundle: &SessionReflectorEvidenceBundle,
    skill_bundle: &SkillWriterEvidenceBundle,
    proposed_ops: Option<&Value>,
    err: &TraceDecayError,
) -> Result<(AutomationRunLedgerRecord, AutomationRunLedgerRecord)> {
    let reflector_record = reflector_finalizer
        .append_failed_record(
            response.model.clone(),
            reflector_bundle.evidence_hash.clone(),
            proposed_ops.cloned(),
            err.to_string(),
        )
        .await?;
    let skill_record = skill_finalizer
        .append_failed_record(
            response.model.clone(),
            skill_bundle.evidence_hash.clone(),
            proposed_ops.cloned(),
            err.to_string(),
        )
        .await?;
    Ok((reflector_record, skill_record))
}

fn build_combined_review_prompt(reflector_evidence: &Value, skill_evidence: &Value) -> String {
    format!(
        "This is a combined TraceDecay self-improvement review covering both session reflection and skill writing in one pass. Return only one JSON object containing both a facts array and a skills array; use an empty array for a part with nothing to propose. Follow each part's instructions exactly.\n\n## Part 1: session reflection\n{}\n\n## Part 2: skill review\n{}",
        build_session_reflector_prompt(reflector_evidence),
        build_skill_writer_prompt(skill_evidence)
    )
}

fn build_session_reflector_prompt(evidence: &Value) -> String {
    const POLICY: &str = concat!(
        "Review these bounded TraceDecay session snippets and propose only durable memory facts.\n",
        "Evidence has two channels: recent_session_slices holds turn-ordered head/tail turns and summary nodes replayed from recently active sessions, and hits holds keyword search matches; both are citable.\n",
        "\n",
        "Signals worth capturing (any one is enough):\n",
        "- The user revealed durable preferences, persona, expectations, or ways they want the agent to operate.\n",
        "- The user corrected the agent's style, tone, format, verbosity, workflow, or approach. Frustration signals like 'stop doing X', 'this is too verbose', 'don't format like this', or an explicit 'remember this' are FIRST-CLASS signals: capture the correction as a durable user_pref or decision fact so the next session starts already knowing. These corrections should also end up embedded in the skill that governs that class of task, not only in memory; the skill writer handles the skill side, but the fact must still be recorded here.\n",
        "- A durable project, tool, decision, or code-area fact emerged that a future session would need.\n",
        "\n",
        "Do NOT capture (these harden into stale or self-defeating rules):\n",
        "- Environment-dependent failures: missing binaries, 'command not found', unconfigured credentials, uninstalled packages, post-migration path mismatches. The user can fix these; they are not durable facts.\n",
        "- Negative claims about tools or features ('X is broken', 'Y does not work'). These harden into self-imposed refusals cited long after the actual problem was fixed. If a tool failed because of setup state, the durable fact is the FIX (install command, config step, env var), never 'this tool does not work'.\n",
        "- Session-specific transient errors that resolved before the session ended. If retrying worked, the lesson is the retry pattern, not the original failure.\n",
        "- One-off task narratives. A single 'summarize this' or 'analyze this PR' request is not a durable fact about the user or project.\n",
        "- Secrets, credentials, tokens, or ephemeral status.\n",
        "\n",
        "Proposing nothing is a real option when the session ran smoothly and revealed nothing durable, but do not reach for it as a default.\n",
        "\n",
        "Response contract: Return only JSON with a facts array. Each fact must include content, category, optional tags, optional entities, trust, source_span, and reason. Category must be one of general, user_pref, project, tool, decision, or code_area. Use trust, not confidence; trust must be a JSON number from 0.0 to 1.0. Do not use string labels like high, medium, or low. source_span must cite one bounded evidence hit by session_id plus message_id for raw messages, by store_id for raw messages, or by node_id for summaries. Do not include secrets or ephemeral status.\n",
    );
    format!(
        "{POLICY}{}",
        serde_json::to_string_pretty(evidence).unwrap_or_else(|_| "{}".to_string())
    )
}

fn build_skill_writer_prompt(evidence: &Value) -> String {
    const POLICY: &str = concat!(
        "Review these bounded TraceDecay session snippets and propose only reusable managed skills for repeated workflows, corrections, or tool-use patterns.\n",
        "Evidence has two channels: recent_session_slices holds turn-ordered head/tail turns and summary nodes replayed from recently active sessions, and hits holds keyword search matches.\n",
        "\n",
        "Target shape of the skill library: CLASS-LEVEL umbrella skills, each with a rich body and support files for session-specific detail — not a long flat list of narrow one-session-one-skill entries. This shapes HOW you update, not WHETHER you update.\n",
        "\n",
        "Signals that warrant a skill proposal (any one is enough):\n",
        "- The user corrected the agent's style, tone, format, verbosity, workflow, or approach. Frustration signals like 'stop doing X', 'this is too verbose', 'don't format like this', 'you always do Y and I hate it', or an explicit 'remember this' are FIRST-CLASS skill signals, not just memory signals. Embed the correction in the body of the skill that governs that class of task so the next session starts already knowing; a memory fact alone is not enough.\n",
        "- A non-trivial technique, fix, workaround, debugging path, or tool-usage pattern emerged that a future session would benefit from.\n",
        "- A skill that evidence shows was used or loaded this session turned out to be wrong, missing a step, or outdated. Patch it now.\n",
        "\n",
        "Preference order — pick the EARLIEST action that fits:\n",
        "1. UPDATE a skill that the evidence (skill_usage_summaries, skill_improvement_recommendations, existing_managed_skills) shows was used or loaded recently. It was in play, so it is the right one to extend.\n",
        "2. PATCH an existing umbrella skill from existing_managed_skills whose class covers the new learning. Add a subsection, a pitfall, or broaden a trigger.\n",
        "3. ADD to an existing skill's scope via its support_files (reference notes, templates, or re-runnable snippets), with a one-line pointer in the skill body so future sessions find it.\n",
        "4. CREATE a new skill only when nothing existing fits. The name MUST be at the class level and MUST survive the test: 'does this name only make sense for today's task?' If yes, it is wrong — no PR numbers, error strings, feature codenames, or fix-X/debug-Y session artifacts. Fall back to option 1, 2, or 3 instead.\n",
        "\n",
        "Do NOT capture (these become persistent self-imposed constraints that bite later when the environment changes):\n",
        "- Environment-dependent failures: missing binaries, 'command not found', unconfigured credentials, uninstalled packages, post-migration path mismatches. The user can fix these; they are not durable rules.\n",
        "- Negative claims about tools or features ('X is broken', 'browser tools do not work'). These harden into refusals the agent cites against itself long after the actual problem was fixed. If a tool failed because of setup state, capture the FIX (install command, config step, env var) under an existing setup or troubleshooting skill — never 'this tool does not work' as a standalone constraint.\n",
        "- Session-specific transient errors that resolved before the session ended. If retrying worked, the lesson is the retry pattern, not the original failure.\n",
        "- One-off task narratives. A single 'summarize this' or 'analyze this PR' request is not a class of work that warrants a skill.\n",
        "- Secrets, credentials, or tokens in any skill body or support file.\n",
        "\n",
        "An empty skills array is a real option when the session ran smoothly with no corrections and produced no new technique, but do not reach for it as a default.\n",
        "\n",
        "Response contract: Return only JSON with a skills array of managed skill creates or updates. New skills may omit action or use action=create and must include id, title, summary, category, body_markdown, optional targets, optional support_files with text content, and reason. Targets, when present, must be an array using cursor, codex, claude, agents, opencode, kimi, kiro, or hermes; Hermes exports are generated read-only under the TraceDecay plugin package and never overwrite host-owned user skills. Updates must use action=update or action=patch, include id and base_checksum, and include at least one changed field among title, summary, category, targets, body_markdown/body, support_files, or pinned. For updates, support_files is a complete replacement list, not a partial file patch. Consolidations: when skill_overlap_candidates shows overlapping managed skills, you may propose action=merge (include id for the surviving skill, base_checksum, source_skill_id, source_base_checksum, reason, and optional merged title/summary/category/targets/body_markdown/support_files) or action=archive (include id, base_checksum, reason). Consolidations preserve archived source content. The runner stages them by default and may auto-apply only when auto_enable_skills is explicitly enabled and every ownership, checksum, pin, pending-update, and scheduled-job guard passes. Never propose merge or archive for pinned or user-authored skills. Activation is controlled only by the runner policy; do not assume activation from your response.\n",
    );
    format!(
        "{POLICY}{}",
        serde_json::to_string_pretty(evidence).unwrap_or_else(|_| "{}".to_string())
    )
}

fn normalized_non_empty(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

async fn production_project_automation_retrieval(
    cg: &TraceDecay,
) -> Box<dyn AutomationSessionRetrieval> {
    let fallback = || unavailable_automation_retrieval("session_evidence_retrieval_unavailable");
    let Some(database) = GlobalDb::open_read_only_at(&cg.store_layout().sessions_db_path).await
    else {
        return fallback();
    };
    let Some(registry_path) = crate::global_db::global_db_path() else {
        return fallback();
    };
    let Some(registry) = GlobalDb::open_read_only_at(&registry_path).await else {
        return fallback();
    };
    let Some(project_id) = cg.store_layout().identity.project_id.as_deref() else {
        return fallback();
    };
    let Some(context) = registry.project_registry_context_by_id(project_id).await else {
        return fallback();
    };
    let Some(identity) = project_automation_identity(cg, &registry, &context) else {
        return fallback();
    };
    let database = Arc::new(database);
    let Some(anchor_session_id) = active_automation_anchor(database.as_ref()).await else {
        return fallback();
    };
    Box::new(ProductionAutomationSessionRetrieval {
        database,
        identity,
        anchor_session_id,
        store_scope: AutomationRetrievalStoreScope::Project,
    })
}

async fn production_user_automation_retrieval(
    profile_root: &std::path::Path,
) -> Box<dyn AutomationSessionRetrieval> {
    let fallback = || unavailable_automation_retrieval("session_evidence_retrieval_unavailable");
    let sessions_db_path = user_sessions_db_path(profile_root);
    let Some(database) = GlobalDb::open_read_only_at(&sessions_db_path).await else {
        return fallback();
    };
    let Some(identity) = profile_automation_identity() else {
        return fallback();
    };
    let database = Arc::new(database);
    let Some(anchor_session_id) = active_automation_anchor(database.as_ref()).await else {
        return fallback();
    };
    Box::new(ProductionAutomationSessionRetrieval {
        database,
        identity,
        anchor_session_id,
        store_scope: AutomationRetrievalStoreScope::Profile,
    })
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

fn project_automation_identity(
    cg: &TraceDecay,
    registry: &GlobalDb,
    context: &ProjectRegistryContext,
) -> Option<ResolvedSessionIdentity> {
    let profile_root = registry.db_path().parent()?;
    let serving_db = cg.db_path();
    let mut selected = None;
    for store in &context.stores {
        for scope in &store.graph_scopes {
            if scope.writable
                && scope.project_id == context.project.project_id
                && scope.store_id == store.store.store_id
                && profile_root.join(&scope.db_relpath) == serving_db
            {
                if selected.is_some() {
                    return None;
                }
                selected = Some((store.store.store_id.clone(), scope.graph_scope_id.clone()));
            }
        }
    }
    let (store_id, graph_scope_id) = selected?;
    Some(ResolvedSessionIdentity::for_project(
        ProfileId::new("profile.primary").ok()?,
        ProjectId::new(context.project.project_id.clone()).ok()?,
        SessionStoreId::new(store_id).ok()?,
        SessionRootId::new(graph_scope_id.clone()).ok()?,
        ResolvedGitRoute::new(
            RepositoryId::new(context.project.git_common_dir.clone()?).ok()?,
            WorktreeId::new(context.project.canonical_root.clone()).ok()?,
            BranchId::new(graph_scope_id).ok()?,
        ),
    ))
}

fn profile_automation_identity() -> Option<ResolvedSessionIdentity> {
    Some(ResolvedSessionIdentity::for_profile(
        ProfileId::new("profile.primary").ok()?,
        SessionStoreId::new("store.profile.primary").ok()?,
        SessionRootId::new("root.profile.primary").ok()?,
    ))
}

async fn active_automation_anchor(database: &GlobalDb) -> Option<SessionId> {
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

fn automation_session_policy_digest() -> Option<[u8; 32]> {
    let encoded = PayloadReferenceV1::for_payload(&json!({
        "domain": "tracedecay.observation-anchor.authorization.v1",
        "authority": "observation-capture.v1",
    }))
    .ok()?;
    let digest = encoded.digest().as_str().strip_prefix("sha256:")?;
    hex::decode(digest).ok()?.try_into().ok()
}

async fn retrieve_automation_session_evidence(
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

fn accept_automation_temporal_outcome(
    outcome: SessionRetrievalOutcome<TemporalKernelResult>,
) -> AutomationTemporalRetrieval {
    match outcome {
        SessionRetrievalOutcome::Complete { items, .. } => {
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
        SessionRetrievalOutcome::CompleteZero { .. } => AutomationTemporalRetrieval::CompleteZero,
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
        SessionRetrievalOutcome::BudgetExhausted => {
            AutomationTemporalRetrieval::Rejected("session_evidence_budget_exhausted")
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

fn validate_complete_evidence(
    evidence: &AutomationTemporalEvidence,
) -> std::result::Result<(), &'static str> {
    if evidence.coverage.hidden != 0
        || evidence.coverage.unknown != 0
        || evidence.coverage.redacted != 0
    {
        return Err("session_evidence_partial");
    }
    let anchors = evidence
        .items
        .iter()
        .map(|item| item.anchor_id.as_str())
        .collect::<BTreeSet<_>>();
    if anchors.len() != evidence.items.len()
        || u64::try_from(anchors.len()).ok() != Some(evidence.coverage.visible)
    {
        return Err("session_evidence_partial");
    }
    if evidence.items.iter().any(|item| {
        item.anchor_id.is_empty()
            || item.stable_id.is_empty()
            || item.provider.is_empty()
            || item.session_id.is_empty()
            || item.snippet.is_empty()
    }) {
        return Err("session_evidence_unavailable");
    }
    Ok(())
}

fn serialize_automation_temporal_evidence(
    evidence: AutomationTemporalEvidence,
    filters: AutomationEvidenceFilters<'_>,
) -> SerializedAutomationEvidence {
    let mut filtered = evidence
        .items
        .into_iter()
        .filter(|item| {
            filters
                .session_id
                .is_none_or(|session_id| item.session_id == session_id)
                && filters
                    .role
                    .is_none_or(|role| item.role.as_deref() == Some(role))
                && filters
                    .start_time
                    .is_none_or(|start| temporal_seconds(item.knowledge_at_micros) >= start)
                && filters
                    .end_time
                    .is_none_or(|end| temporal_seconds(item.knowledge_at_micros) <= end)
                && (filters.include_summaries || item.message_id.is_some())
        })
        .collect::<Vec<_>>();
    filtered.sort_by(|left, right| compare_evidence_items(left, right, filters.sort));
    let hits = filtered
        .iter()
        .take(filters.evidence_limit)
        .map(|item| {
            let summary = item.message_id.is_none();
            CanonicalEvidenceHit {
                kind: if summary {
                    "summary_node".to_string()
                } else {
                    "raw_message".to_string()
                },
                provider: item.provider.clone(),
                session_id: item.session_id.clone(),
                message_id: item.message_id.clone(),
                node_id: summary.then(|| {
                    item.source_id
                        .clone()
                        .unwrap_or_else(|| item.stable_id.clone())
                }),
                store_id: item.store_id,
                role: item.role.clone(),
                snippet: truncate_chars_for_prompt(
                    &temporal_payload_text(&item.snippet),
                    if summary {
                        SESSION_REPLAY_SUMMARY_CHARS
                    } else {
                        SESSION_REPLAY_SNIPPET_CHARS
                    },
                ),
                anchor_id: item.anchor_id.clone(),
                stable_id: item.stable_id.clone(),
                knowledge_at_micros: item.knowledge_at_micros,
                normalized_score_micros: item.normalized_score_micros,
                ordinal: item.ordinal,
            }
        })
        .collect::<Vec<_>>();
    let mut selected_anchors = hits
        .iter()
        .map(|hit| hit.anchor_id.clone())
        .collect::<BTreeSet<_>>();
    let recent_session_slices = if filters.include_recent_sessions {
        recent_session_slices_from_temporal(
            &filtered,
            filters.session_id,
            filters.include_summaries,
            filters.recent_sessions_limit,
        )
        .map(|(slices, replay_anchors)| {
            selected_anchors.extend(replay_anchors);
            slices
        })
    } else {
        None
    };
    let tool_usage = filtered
        .iter()
        .filter(|item| selected_anchors.contains(&item.anchor_id))
        .map(tool_usage_observation)
        .collect::<Vec<_>>();
    SerializedAutomationEvidence {
        hits,
        recent_session_slices,
        tool_usage,
        coverage: TemporalCoverageCountsV1 {
            visible: selected_anchors.len() as u64,
            hidden: 0,
            unknown: 0,
            redacted: 0,
        },
    }
}

fn compare_evidence_items(
    left: &AutomationTemporalEvidenceItem,
    right: &AutomationTemporalEvidenceItem,
    sort: LcmGrepSort,
) -> std::cmp::Ordering {
    let primary = match sort {
        LcmGrepSort::Recency => right.knowledge_at_micros.cmp(&left.knowledge_at_micros),
        LcmGrepSort::Relevance => right
            .normalized_score_micros
            .cmp(&left.normalized_score_micros),
        LcmGrepSort::Hybrid => right
            .normalized_score_micros
            .cmp(&left.normalized_score_micros)
            .then_with(|| right.knowledge_at_micros.cmp(&left.knowledge_at_micros)),
    };
    primary
        .then_with(|| left.provider.cmp(&right.provider))
        .then_with(|| left.session_id.cmp(&right.session_id))
        .then_with(|| left.ordinal.cmp(&right.ordinal))
        .then_with(|| left.stable_id.cmp(&right.stable_id))
        .then_with(|| left.anchor_id.cmp(&right.anchor_id))
}

fn recent_session_slices_from_temporal(
    items: &[AutomationTemporalEvidenceItem],
    explicit_session_id: Option<&str>,
    include_summaries: bool,
    sessions_limit: usize,
) -> Option<(Value, BTreeSet<String>)> {
    let mut grouped: BTreeMap<(String, String), Vec<&AutomationTemporalEvidenceItem>> =
        BTreeMap::new();
    for item in items {
        grouped
            .entry((item.provider.clone(), item.session_id.clone()))
            .or_default()
            .push(item);
    }
    let mut session_order = grouped
        .iter()
        .map(|((provider, session_id), items)| {
            (
                items
                    .iter()
                    .map(|item| item.knowledge_at_micros)
                    .max()
                    .unwrap_or(i64::MIN),
                provider.clone(),
                session_id.clone(),
            )
        })
        .collect::<Vec<_>>();
    session_order.sort_by(|left, right| {
        right
            .0
            .cmp(&left.0)
            .then_with(|| left.1.cmp(&right.1))
            .then_with(|| left.2.cmp(&right.2))
    });
    let sessions_limit = sessions_limit.clamp(1, 10);
    let mut selected_anchors = BTreeSet::new();
    let sessions = session_order
        .into_iter()
        .take(sessions_limit)
        .filter_map(|(_, provider, session_id)| {
            let mut session_items = grouped.remove(&(provider.clone(), session_id.clone()))?;
            session_items.sort_by(|left, right| {
                left.ordinal
                    .cmp(&right.ordinal)
                    .then_with(|| left.knowledge_at_micros.cmp(&right.knowledge_at_micros))
                    .then_with(|| left.anchor_id.cmp(&right.anchor_id))
            });
            let messages = session_items
                .iter()
                .filter(|item| item.message_id.is_some())
                .copied()
                .collect::<Vec<_>>();
            let ordinals = messages
                .iter()
                .map(|item| item.ordinal)
                .collect::<Option<Vec<_>>>()?;
            let total_messages = messages
                .iter()
                .filter_map(|item| item.session_total_messages)
                .next()
                .or_else(|| {
                    let max = ordinals.iter().copied().max()?;
                    u64::try_from(max).ok()
                })?;
            let expected_ordinals = (1..=messages.len())
                .map(|ordinal| i64::try_from(ordinal).ok())
                .collect::<Option<Vec<_>>>()?;
            if ordinals != expected_ordinals
                || total_messages != u64::try_from(messages.len()).ok()?
                || messages
                    .iter()
                    .filter_map(|item| item.session_total_messages)
                    .any(|total| total != total_messages)
            {
                return None;
            }
            let head_count = messages.len().min(SESSION_REPLAY_HEAD_TURNS);
            let tail_start = messages
                .len()
                .saturating_sub(SESSION_REPLAY_TAIL_TURNS)
                .max(head_count);
            let replay_message = |item: &&AutomationTemporalEvidenceItem| {
                let text = temporal_payload_text(&item.snippet);
                let snippet = truncate_chars_for_prompt(&text, SESSION_REPLAY_SNIPPET_CHARS);
                json!({
                    "message_id": item.message_id,
                    "store_id": item.store_id,
                    "role": item.role,
                    "ordinal": item.ordinal,
                    "timestamp": temporal_seconds(item.knowledge_at_micros),
                    "snippet": snippet,
                    "truncated": snippet.chars().count() < text.chars().count(),
                    "provider": item.provider,
                    "anchor_id": item.anchor_id,
                    "stable_id": item.stable_id,
                    "knowledge_at_micros": item.knowledge_at_micros,
                })
            };
            let head = messages
                .iter()
                .take(head_count)
                .map(&replay_message)
                .collect::<Vec<_>>();
            let tail = messages
                .iter()
                .skip(tail_start)
                .map(replay_message)
                .collect::<Vec<_>>();
            for item in messages.iter().take(head_count) {
                selected_anchors.insert(item.anchor_id.clone());
            }
            for item in messages.iter().skip(tail_start) {
                selected_anchors.insert(item.anchor_id.clone());
            }
            let summary_nodes = if include_summaries {
                let nodes = session_items
                    .iter()
                    .filter(|item| item.message_id.is_none())
                    .take(SESSION_REPLAY_SUMMARY_NODES)
                    .map(|item| {
                        let text = temporal_payload_text(&item.snippet);
                        let snippet =
                            truncate_chars_for_prompt(&text, SESSION_REPLAY_SUMMARY_CHARS);
                        json!({
                            "node_id": item.source_id.clone().unwrap_or_else(|| item.stable_id.clone()),
                            "depth": 0,
                            "created_at": temporal_seconds(item.knowledge_at_micros),
                            "snippet": snippet,
                            "truncated": snippet.chars().count() < text.chars().count(),
                            "provider": item.provider,
                            "anchor_id": item.anchor_id,
                            "stable_id": item.stable_id,
                            "knowledge_at_micros": item.knowledge_at_micros,
                        })
                    })
                    .collect::<Vec<_>>();
                for item in session_items
                    .iter()
                    .filter(|item| item.message_id.is_none())
                    .take(SESSION_REPLAY_SUMMARY_NODES)
                {
                    selected_anchors.insert(item.anchor_id.clone());
                }
                nodes
            } else {
                Vec::new()
            };
            Some(json!({
                "provider": provider,
                "session_id": session_id,
                "total_messages": total_messages,
                "omitted_messages": total_messages.saturating_sub((head.len() + tail.len()) as u64),
                "head": head,
                "tail": tail,
                "summary_nodes": summary_nodes,
            }))
        })
        .collect::<Vec<_>>();
    if sessions.is_empty() {
        return None;
    }
    Some((
        json!({
            "mode": "recent_sessions",
            "session_selection": if explicit_session_id.is_some() {
                "explicit_session_id"
            } else {
                "recent_activity"
            },
            "sessions_limit": sessions_limit,
            "bounds": {
                "head_turns": SESSION_REPLAY_HEAD_TURNS,
                "tail_turns": SESSION_REPLAY_TAIL_TURNS,
                "snippet_chars": SESSION_REPLAY_SNIPPET_CHARS,
                "summary_nodes": if include_summaries {
                    SESSION_REPLAY_SUMMARY_NODES
                } else {
                    0
                },
                "summary_chars": SESSION_REPLAY_SUMMARY_CHARS,
            },
            "sessions": sessions,
        }),
        selected_anchors,
    ))
}

fn temporal_seconds(timestamp_micros: i64) -> i64 {
    if timestamp_micros.unsigned_abs() >= 100_000_000_000 {
        timestamp_micros / 1_000_000
    } else {
        timestamp_micros
    }
}

fn temporal_payload_text(payload: &str) -> String {
    let Ok(value) = serde_json::from_str::<Value>(payload) else {
        return payload.to_string();
    };
    [
        "/payload/text",
        "/payload/content",
        "/payload/summary_text",
        "/text",
        "/content",
    ]
    .into_iter()
    .find_map(|pointer| value.pointer(pointer).and_then(Value::as_str))
    .unwrap_or(payload)
    .to_string()
}

fn tool_usage_observation(item: &AutomationTemporalEvidenceItem) -> OwnedToolUsageObservation {
    let value = serde_json::from_str::<Value>(&item.snippet).ok();
    OwnedToolUsageObservation {
        tool_names: value
            .as_ref()
            .and_then(|value| find_string_field(value, "tool_names")),
        metadata_json: value
            .as_ref()
            .and_then(|value| find_string_field(value, "metadata_json")),
        text: Some(value.as_ref().map_or_else(
            || item.snippet.clone(),
            |value| {
                find_string_field(value, "text")
                    .unwrap_or_else(|| temporal_payload_text(&item.snippet))
            },
        )),
    }
}

fn find_string_field(value: &Value, field: &str) -> Option<String> {
    match value {
        Value::Object(object) => object
            .get(field)
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| {
                object
                    .values()
                    .find_map(|value| find_string_field(value, field))
            }),
        Value::Array(values) => values
            .iter()
            .find_map(|value| find_string_field(value, field)),
        _ => None,
    }
}

fn find_i64_field_in_json(encoded: &str, field: &str) -> Option<i64> {
    fn visit(value: &Value, field: &str) -> Option<i64> {
        match value {
            Value::Object(object) => object
                .get(field)
                .and_then(Value::as_i64)
                .or_else(|| object.values().find_map(|value| visit(value, field))),
            Value::Array(values) => values.iter().find_map(|value| visit(value, field)),
            _ => None,
        }
    }

    serde_json::from_str(encoded)
        .ok()
        .and_then(|value| visit(&value, field))
}

fn find_string_field_in_json(encoded: &str, field: &str) -> Option<String> {
    serde_json::from_str(encoded)
        .ok()
        .and_then(|value| find_string_field(&value, field))
}

fn canonical_evidence_hash(value: &Value) -> String {
    fn canonicalize(value: &Value) -> Value {
        match value {
            Value::Array(values) => Value::Array(values.iter().map(canonicalize).collect()),
            Value::Object(object) => {
                let mut entries = object.iter().collect::<Vec<_>>();
                entries.sort_by(|left, right| left.0.cmp(right.0));
                let mut canonical = serde_json::Map::new();
                for (key, value) in entries {
                    canonical.insert(key.clone(), canonicalize(value));
                }
                Value::Object(canonical)
            }
            scalar => scalar.clone(),
        }
    }

    sha256_json(&canonicalize(value))
}

/// Names the evidence channels actually present so run artifacts can
/// distinguish replay-backed runs from grep-only runs.
fn evidence_mode_label(has_replay: bool) -> &'static str {
    if has_replay {
        "session_replay_with_grep"
    } else {
        "grep_only"
    }
}

fn session_reflector_replay_allowed(
    scope: LcmScope,
    session_id: Option<&str>,
    source: Option<&str>,
    role: Option<&str>,
    start_time: Option<i64>,
    end_time: Option<i64>,
) -> bool {
    if source.is_some() || role.is_some() || start_time.is_some() || end_time.is_some() {
        return false;
    }

    matches!(scope, LcmScope::All) || session_id.is_some()
}

fn default_session_provider() -> String {
    "cursor".to_string()
}

fn default_skill_writer_provider() -> String {
    "all".to_string()
}

fn default_lcm_grep_scope() -> LcmScope {
    LcmScope::All
}

fn default_include_summaries() -> bool {
    true
}

fn default_lcm_grep_sort() -> LcmGrepSort {
    LcmGrepSort::Recency
}

fn default_session_reflection_query() -> String {
    "remember prefer decision requirement workflow".to_string()
}

fn default_session_evidence_limit() -> usize {
    20
}

fn default_include_recent_sessions() -> bool {
    true
}

fn default_recent_sessions_limit() -> usize {
    3
}

fn default_skill_writer_query() -> String {
    "workflow correction repeated skill tool pattern".to_string()
}

fn default_skill_writer_evidence_limit() -> usize {
    20
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{Database, DatabaseAuthority};
    use std::path::PathBuf;

    struct DenyAutomationAuthorizer;

    impl SessionScopeAuthorizer for DenyAutomationAuthorizer {
        fn authorize(
            &self,
            _context: &RequestContext,
            _request: &SessionScopeAuthorizationRequest,
        ) -> std::result::Result<SessionAuthorizationGrant, SessionAuthorizationError> {
            Err(SessionAuthorizationError::Denied)
        }
    }

    struct NeverAutomationExecution;

    impl SessionTemporalExecutionPort for NeverAutomationExecution {
        fn execute<'a, E>(
            &'a self,
            _request: crate::application::session::AuthorizedTemporalExecutionRequest,
            _estimator: &'a E,
        ) -> crate::application::session::TemporalExecutionFuture<'a>
        where
            E: VersionedTokenEstimator + Sync + 'a,
        {
            Box::pin(async { panic!("denied retrieval must not reach temporal execution") })
        }
    }

    fn authorized_retrieval_context() -> RequestContext {
        RequestContext::new(
            ActorId::new("automation.session-evidence").unwrap(),
            RequestId::new("request.automation.session-evidence.test").unwrap(),
            ResolvedSessionIdentity::for_profile(
                ProfileId::new("profile.test").unwrap(),
                SessionStoreId::new("store.profile.test").unwrap(),
                SessionRootId::new("root.profile.test").unwrap(),
            ),
            CapabilityDigest::new([0x11; 32]),
            PolicyDigest::new([0x22; 32]),
            ConfigurationDigest::new([0x33; 32]),
            MonotonicDeadline::at(Instant::now() + Duration::from_secs(5)),
            CancellationToken::new(),
            RequestBudgets::new(128, AUTOMATION_SESSION_MAX_BYTES, 10_000).unwrap(),
        )
    }

    #[tokio::test]
    async fn real_authorized_service_path_denies_before_execution() {
        let service = SessionRetrievalService::new(
            DenyAutomationAuthorizer,
            NeverAutomationExecution,
            AutomationWordEstimator,
            SessionRetrievalConfiguration::new(1, 1).unwrap(),
        );
        let context = authorized_retrieval_context();
        let adapter = AuthorizedAutomationSessionRetrieval::new(
            &service,
            &context,
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
    }

    #[test]
    fn temporal_automation_evidence_fails_closed_for_non_complete_outcomes() {
        for (outcome, expected_reason) in [
            (
                SessionRetrievalOutcome::<TemporalKernelResult>::Stale {
                    freshness: crate::application::session::SessionDataFreshness::Stored {
                        generation_lag: 1,
                    },
                },
                "session_evidence_stale",
            ),
            (
                SessionRetrievalOutcome::Partial {
                    items: Vec::new(),
                    freshness: crate::application::session::SessionDataFreshness::Fresh,
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
        ] {
            assert!(matches!(
                accept_automation_temporal_outcome(outcome),
                AutomationTemporalRetrieval::Rejected(reason) if reason == expected_reason
            ));
        }
        assert!(matches!(
            accept_automation_temporal_outcome(
                SessionRetrievalOutcome::<TemporalKernelResult>::CompleteZero {
                    freshness: crate::application::session::SessionDataFreshness::Fresh,
                }
            ),
            AutomationTemporalRetrieval::CompleteZero
        ));
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
                items: vec![AutomationTemporalEvidenceItem {
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
                }],
                coverage: TemporalCoverageCountsV1 {
                    visible: 1,
                    hidden: 0,
                    unknown: 0,
                    redacted: 0,
                },
            },
            filters,
        );

        assert_eq!(serialized.hits[0].kind, "raw_message");
        assert_eq!(serialized.hits[0].session_id, "session-1");
        assert_eq!(serialized.hits[0].message_id.as_deref(), Some("message-1"));
        assert_eq!(
            serialized.hits[0].snippet.chars().count(),
            SESSION_REPLAY_SNIPPET_CHARS
        );
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
        let item = |provider: &str, anchor: &str, ordinal: i64, score: u64| {
            AutomationTemporalEvidenceItem {
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
            }
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
        let authority =
            crate::db::DatabaseAuthority::acquire_test(&path, "automation validation writer lane")
                .unwrap();
        let (db, _) = crate::db::Database::initialize(&path, &authority)
            .await
            .unwrap();
        let owner = FactOwnerV1::Profile;
        let memory = MemoryApplication::new(owner.clone(), DatabaseFactStore::new(&db)).unwrap();
        let existing_fact_id = memory
            .add_fact_v1(
                crate::memory::types::AddFactRequest {
                    content: "Committed memory baseline".to_string(),
                    category: crate::memory::types::MemoryCategory::Project,
                    source: None,
                    tags: vec!["automation".to_string()],
                    entities: vec!["TraceDecay".to_string()],
                    trust: Some(0.8),
                    metadata: json!({}),
                },
                crate::application::memory::MemoryOperationContext::generated(
                    &owner,
                    "seed automation validation",
                    None,
                )
                .unwrap(),
            )
            .await
            .unwrap()
            .fact
            .unwrap()
            .fact_id;
        let transaction = db
            .begin_write_transaction("hold automation validation writer")
            .await
            .unwrap();
        transaction
            .execute(
                "UPDATE memory_facts SET updated_at = updated_at WHERE fact_id = ?1",
                [existing_fact_id],
            )
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
            validate_session_fact_proposals(&memory, &proposals, &evidence),
        )
        .await
        .expect("read-only validation must not wait for writer authority")
        .unwrap();
        assert_eq!(validated.0.len(), 1);
        assert!(validated.1.is_empty());
        assert_eq!(
            memory
                .get_fact_v1(existing_fact_id)
                .await
                .unwrap()
                .unwrap()
                .access_count,
            0
        );
        transaction.rollback().await.unwrap();
    }

    #[tokio::test]
    async fn auto_apply_refreshes_digest_only_for_a_new_authority_promotion() {
        let _profile = crate::config::PinnedUserDataDir::new();
        let profile_root = std::env::var_os(crate::config::USER_DATA_DIR_ENV)
            .map(PathBuf::from)
            .expect("pinned profile root");
        let temp = tempfile::tempdir().unwrap();
        let project_root = temp.path().join("project");
        std::fs::create_dir_all(&project_root).unwrap();
        let database_path = temp.path().join("memory.db");
        let authority = DatabaseAuthority::acquire_test(
            &database_path,
            "automation proposal digest disposition test",
        )
        .unwrap();
        let (database, _) = Database::initialize(&database_path, &authority)
            .await
            .unwrap();
        let memory =
            MemoryApplication::new(FactOwnerV1::Profile, DatabaseFactStore::new(&database))
                .unwrap();
        let dashboard_root = temp.path().join("dashboard");
        let records = record_session_fact_proposals(
            &memory,
            &dashboard_root,
            "run-digest-disposition",
            None,
            &[json!({
                "add_fact_request": {
                    "content": "Refresh the digest only after a new authority promotion",
                    "category": "project",
                    "source": "automation-test",
                    "tags": ["automation"],
                    "entities": ["TraceDecay"],
                    "trust": 0.9,
                    "metadata": {}
                }
            })],
            &[],
        )
        .await
        .unwrap();

        let (applied, newly_promoted) = auto_apply_session_fact_proposals(
            &memory,
            Some(&project_root),
            &dashboard_root,
            records.clone(),
        )
        .await
        .unwrap();
        assert!(newly_promoted);
        assert_eq!(applied[0].state, FactProposalState::Applied);

        let snapshot = crate::automation::memory_digest::memory_digest_snapshot_path(&profile_root);
        assert!(snapshot.exists(), "new promotion must refresh the digest");
        std::fs::remove_file(&snapshot).unwrap();

        let (replayed, newly_promoted) = auto_apply_session_fact_proposals(
            &memory,
            Some(&project_root),
            &dashboard_root,
            records,
        )
        .await
        .unwrap();
        assert!(
            !newly_promoted,
            "an applied proposal replay is not a promotion"
        );
        assert_eq!(replayed[0].state, FactProposalState::Applied);
        assert!(
            !snapshot.exists(),
            "an idempotent applied replay must not refresh the digest"
        );
    }

    #[tokio::test]
    async fn auto_apply_flushes_a_new_promotion_before_later_conflict_returns() {
        let _profile = crate::config::PinnedUserDataDir::new();
        let profile_root = std::env::var_os(crate::config::USER_DATA_DIR_ENV)
            .map(PathBuf::from)
            .expect("pinned profile root");
        let temp = tempfile::tempdir().unwrap();
        let project_root = temp.path().join("project");
        std::fs::create_dir_all(&project_root).unwrap();
        let database_path = temp.path().join("memory.db");
        let authority = DatabaseAuthority::acquire_test(
            &database_path,
            "automation proposal partial digest refresh test",
        )
        .unwrap();
        let (database, _) = Database::initialize(&database_path, &authority)
            .await
            .unwrap();
        let owner = FactOwnerV1::Profile;
        let memory =
            MemoryApplication::new(owner.clone(), DatabaseFactStore::new(&database)).unwrap();
        let dashboard_root = temp.path().join("dashboard");
        let records = record_session_fact_proposals(
            &memory,
            &dashboard_root,
            "run-digest-partial",
            None,
            &[
                json!({
                    "add_fact_request": {
                        "content": "A successful promotion must refresh before a later conflict",
                        "category": "project",
                        "source": "automation-test",
                        "tags": ["automation"],
                        "entities": ["TraceDecay"],
                        "trust": 0.9,
                        "metadata": {}
                    }
                }),
                json!({
                    "add_fact_request": {
                        "content": "This proposal is rejected to force the later conflict",
                        "category": "project",
                        "source": "automation-test",
                        "tags": ["automation"],
                        "entities": ["TraceDecay"],
                        "trust": 0.9,
                        "metadata": {}
                    }
                }),
            ],
            &[],
        )
        .await
        .unwrap();
        let rejected_id =
            tracedecay_domain::ProvenanceId::new(records[1].proposal_id.clone()).unwrap();
        let rejected = memory
            .get_compatibility_fact_proposal(rejected_id.clone())
            .await
            .unwrap()
            .unwrap();
        memory
            .reject_compatibility_fact_proposal(
                rejected_id,
                rejected.revision(),
                tracedecay_domain::ActorId::new("test:reviewer".to_string()).unwrap(),
                "fixture conflict".to_string(),
            )
            .await
            .unwrap();

        let error = auto_apply_session_fact_proposals(
            &memory,
            Some(&project_root),
            &dashboard_root,
            records,
        )
        .await
        .expect_err("the rejected second proposal must keep its original error path");
        assert!(error.to_string().contains("not pending approval"));
        assert!(
            crate::automation::memory_digest::memory_digest_snapshot_path(&profile_root).exists(),
            "the first new promotion must still refresh before returning the later conflict"
        );
    }
}
