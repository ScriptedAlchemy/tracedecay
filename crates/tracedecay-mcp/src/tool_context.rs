//! The single validated admitted binding a moved MCP handler family reads.
//!
//! The composition root resolves project admission, the carried request
//! deadline, cancellation, and every code-index authority *before* handler
//! dispatch, then hands the whole admitted set across this one boundary.
//!
//! Construction is one validated step, not a builder chain, and the binding
//! carries exactly one scope: the checkout the daemon admitted for this
//! request. A scoped authority is admitted *under* that scope rather than
//! arriving with a scope label of its own, so there is no second label a
//! caller could set to make one project's store or executors look like
//! another's. Where an authority knows its own identity, [`McpToolContext::bind`]
//! checks that identity rather than the caller's word: a registered store
//! lease reports the logical shard it was opened for, and a lease that is not
//! this project's session shard is refused however it was presented — a
//! `Project` or `Code` shard for the same project included, since those are
//! different stores and not project-session authority.
//!
//! A session-store lease is admitted by presence: attached means the daemon
//! admitted it; absent is the typed unavailable/denied state. Bind derives
//! that typed state from the lease and does not invent a verdict the daemon
//! does not produce.
//!
//! Absence stays typed. An authority the daemon never admitted is `None` here
//! and each handler turns that into its own unavailable state. Every binding
//! carries an admitted project; a call that never published a checkout is a
//! typed root failure, not a second binding shape. Graph verification refuses
//! rather than waving a query through.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tracedecay_configuration::ProjectConfigurationRuntime;
use tracedecay_contracts::code_index_freshness::{
    CodeIndexFreshnessPayloadV1, CodeIndexFreshnessReader,
};
use tracedecay_contracts::doctor::SemanticOwnerStateV1;
use tracedecay_contracts::{CancellationSignal, Deadline, ResolvedScope};
use tracedecay_dashboard_api::AdmittedDoctorReportV1;
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_global_db::RegisteredGlobalDbLeaseV1;
use tracedecay_graph_query::VerifiedGraphQuery;
use tracedecay_query::code_search::{
    CodeIndexBranchDiffExecutor, CodeIndexSearchAuthorityV1, CodeIndexSearchExecutor,
};
use tracedecay_runtime_core::db::Database;
use tracedecay_runtime_core::runtime_telemetry::GenerationCensusSnapshot;
use tracedecay_runtime_core::storage::StoreLayout;
use tracedecay_store::StoreShardScopeV1;
use tracedecay_store_runtime::DaemonSessionRuntimeRegistryV1;
use tracedecay_temporal_query::resolution::ValidatedAuthorization;

/// Why a proposed binding is not one coherent admitted request scope.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum McpToolBindingError {
    #[error("admitted project root '{root}' is not absolute")]
    RelativeProjectRoot { root: String },
    #[error("admitted request scope is not self-consistent: {detail}")]
    ScopeInvalid { detail: String },
    #[error(
        "admitted project session store must be a project-sessions shard, but its lease was opened for {shard} while this request resolved {request}"
    )]
    ProjectStoreNotSessionScoped { request: String, shard: String },
    #[error(
        "project session store lease was opened for project {lease} but this request resolved {request}"
    )]
    ProjectStoreProjectMismatch { request: String, lease: String },
    #[error("code index admission carries no executor to authorize")]
    CodeIndexWithoutExecutor,
    #[error("store layout project '{layout}' does not match admitted scope {request}")]
    StoreLayoutProjectMismatch { request: String, layout: String },
}

impl McpToolBindingError {
    /// The stable reason code an operator sees for a refused binding.
    #[must_use]
    pub fn reason_code(&self) -> &'static str {
        match self {
            Self::RelativeProjectRoot { .. } => "mcp_tool_binding_root_not_absolute",
            Self::ScopeInvalid { .. } => "mcp_tool_binding_scope_invalid",
            Self::ProjectStoreNotSessionScoped { .. } => "mcp_tool_binding_store_not_session_shard",
            Self::ProjectStoreProjectMismatch { .. } => "mcp_tool_binding_store_project_mismatch",
            Self::CodeIndexWithoutExecutor => "mcp_tool_binding_code_index_without_executor",
            Self::StoreLayoutProjectMismatch { .. } => {
                "mcp_tool_binding_store_layout_project_mismatch"
            }
        }
    }
}

impl From<McpToolBindingError> for TraceDecayError {
    fn from(error: McpToolBindingError) -> Self {
        // A refused binding is a daemon wiring fault, not a transient
        // condition: retrying the same admission reproduces it exactly.
        Self::project_route(error.reason_code(), false, error.to_string())
    }
}

/// The caller's carried deadline and cancellation.
///
/// Handlers propagate both into bounded walks so a cancelled call stops at its
/// next checkpoint instead of running to completion.
#[derive(Clone, Copy, Default)]
pub struct RequestControls<'a> {
    pub deadline: Option<&'a Deadline>,
    pub cancellation: Option<&'a CancellationSignal>,
}

/// The registered project session store the daemon opened for this request.
///
/// Presence of the lease *is* admission. [`McpToolContext::bind`] derives
/// [`ValidatedAuthorization::Authorized`] from that presence; an absent lease
/// is the typed unavailable/denied state, not a second verdict.
#[derive(Clone, Copy)]
pub struct AdmittedProjectStore<'a> {
    lease: &'a RegisteredGlobalDbLeaseV1,
}

impl<'a> AdmittedProjectStore<'a> {
    /// Wraps the admitted lease. Presence of the lease *is* admission;
    /// absence never reaches this type.
    #[must_use]
    pub fn new(lease: &'a RegisteredGlobalDbLeaseV1) -> Self {
        Self { lease }
    }
}

/// Daemon-owned code-index executors with the authorization proved for them.
///
/// The authority is required, not optional: an executor admitted without the
/// admission envelope it authenticates is an empty capability claim that would
/// report the index as mounted while nothing can answer. The executors carry
/// no scope of their own — they are admitted under the request's one scope,
/// and each one re-authorizes its embedded route admission against the request
/// root when it runs.
#[derive(Clone, Copy)]
pub struct AdmittedCodeIndex<'a> {
    authority: &'a CodeIndexSearchAuthorityV1,
    search: Option<&'a CodeIndexSearchExecutor>,
    branch_diff: Option<&'a CodeIndexBranchDiffExecutor>,
}

impl<'a> AdmittedCodeIndex<'a> {
    /// Admits at least one executor together with the authority it presents.
    pub fn new(
        authority: &'a CodeIndexSearchAuthorityV1,
        search: Option<&'a CodeIndexSearchExecutor>,
        branch_diff: Option<&'a CodeIndexBranchDiffExecutor>,
    ) -> std::result::Result<Self, McpToolBindingError> {
        if search.is_none() && branch_diff.is_none() {
            return Err(McpToolBindingError::CodeIndexWithoutExecutor);
        }
        Ok(Self {
            authority,
            search,
            branch_diff,
        })
    }
}

/// The checkout identity a request-scoped admitted project snapshot carries.
#[derive(Clone, Debug)]
pub struct McpProjectIdentityV1 {
    pub project_root: PathBuf,
    pub scope: ResolvedScope,
    pub active_branch: Option<String>,
    pub serving_branch: Option<String>,
    /// Ancestor-fallback warning retained at project open, when serving from a
    /// branch DB that is not the live HEAD. Status/active-project diagnostics
    /// need it to build the same [`tracedecay_application::tracedecay::BranchDiagnostics`]
    /// the root used to compute on `TraceDecay`.
    pub fallback_warning: Option<String>,
}

/// Request-scoped handles a moved handler family may read.
///
/// Lives exactly as long as the request's `Arc<TraceDecay>` snapshot and must
/// never be cached. A `Database` retained across a branch reopen would be a
/// stale handle: project-open swaps the served instance, then reconciles
/// owners (`mcp/server/lifecycle.rs`). The composition root builds this
/// snapshot per call from the instance that request already holds.
///
/// In-process handles are not wire contracts. The graph database, store
/// runtime, and configuration runtime are the same required `TraceDecay`
/// handles the root already holds; a session-store lease stays `None` when
/// the daemon never admitted one.
///
/// Trimmed to what graph leftovers, status/active-project, and runtime health
/// actually read: profile-session and diagnostics-database handles are not
/// on this snapshot because those families do not touch them.
pub struct McpAdmittedProjectV1 {
    identity: McpProjectIdentityV1,
    store_layout: StoreLayout,
    graph_database: Database,
    graph_db_path: PathBuf,
    store_runtime: Arc<DaemonSessionRuntimeRegistryV1>,
    configuration_runtime: Arc<ProjectConfigurationRuntime>,
    project_session_store: Option<RegisteredGlobalDbLeaseV1>,
}

impl McpAdmittedProjectV1 {
    /// Validates one request-scoped snapshot and freezes it.
    ///
    /// The root must be absolute, the admitted scope self-consistent, the
    /// store layout must name that same project, and a session lease — when
    /// present — must be a `ProjectSessions` shard for that project. Nothing
    /// is defaulted or repaired.
    pub fn new(
        identity: McpProjectIdentityV1,
        store_layout: StoreLayout,
        graph_database: Database,
        graph_db_path: PathBuf,
        store_runtime: Arc<DaemonSessionRuntimeRegistryV1>,
        configuration_runtime: Arc<ProjectConfigurationRuntime>,
        project_session_store: Option<RegisteredGlobalDbLeaseV1>,
    ) -> std::result::Result<Self, McpToolBindingError> {
        if !identity.project_root.is_absolute() {
            return Err(McpToolBindingError::RelativeProjectRoot {
                root: identity.project_root.display().to_string(),
            });
        }
        if let Err(error) = identity.scope.validate() {
            return Err(McpToolBindingError::ScopeInvalid {
                detail: error.to_string(),
            });
        }
        match store_layout.identity.project_id.as_deref() {
            Some(layout_project) if layout_project == identity.scope.project_id.as_str() => {}
            other => {
                return Err(McpToolBindingError::StoreLayoutProjectMismatch {
                    request: checkout_label(&identity.scope),
                    layout: other.unwrap_or("<missing>").to_owned(),
                });
            }
        }
        if let Some(lease) = &project_session_store {
            verify_store_lease(&identity.scope, AdmittedProjectStore::new(lease))?;
        }
        Ok(Self {
            identity,
            store_layout,
            graph_database,
            graph_db_path,
            store_runtime,
            configuration_runtime,
            project_session_store,
        })
    }

    /// The same branch diagnostic shape status and active-project used to
    /// read off `TraceDecay`.
    #[must_use]
    pub fn branch_diagnostics(&self) -> tracedecay_application::tracedecay::BranchDiagnostics {
        self.branch_diagnostics_for_serving_source(None, None)
    }

    #[must_use]
    pub fn branch_diagnostics_for_serving_source(
        &self,
        serving_source_reference: Option<&str>,
        serving_source_revision: Option<&str>,
    ) -> tracedecay_application::tracedecay::BranchDiagnostics {
        tracedecay_application::tracedecay::build_branch_diagnostics(
            &self.identity.project_root,
            &self.store_layout.data_root,
            self.identity.active_branch.clone(),
            self.identity.serving_branch.clone(),
            self.identity.fallback_warning.clone(),
            self.graph_db_path.clone(),
            serving_source_reference.zip(serving_source_revision),
        )
    }

    #[must_use]
    pub fn identity(&self) -> &McpProjectIdentityV1 {
        &self.identity
    }

    #[must_use]
    pub fn store_layout(&self) -> &StoreLayout {
        &self.store_layout
    }

    #[must_use]
    pub fn graph_database(&self) -> &Database {
        &self.graph_database
    }

    #[must_use]
    pub fn graph_db_path(&self) -> &Path {
        self.graph_db_path.as_path()
    }

    #[must_use]
    pub fn store_runtime(&self) -> &DaemonSessionRuntimeRegistryV1 {
        self.store_runtime.as_ref()
    }

    #[must_use]
    pub fn configuration_runtime(&self) -> &ProjectConfigurationRuntime {
        self.configuration_runtime.as_ref()
    }

    #[must_use]
    pub fn project_session_store(&self) -> Option<&RegisteredGlobalDbLeaseV1> {
        self.project_session_store.as_ref()
    }
}

impl std::fmt::Debug for McpAdmittedProjectV1 {
    /// Names the admitted set without reaching into opaque handles.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpAdmittedProjectV1")
            .field("identity", &self.identity)
            .field(
                "store_layout_project_id",
                &self.store_layout.identity.project_id,
            )
            .field("graph_db_path", &self.graph_db_path)
            .field(
                "has_project_session_store",
                &self.project_session_store.is_some(),
            )
            .finish()
    }
}

/// Semantic-owner snapshot the root computed for this request.
///
/// One state, not an `Option` plus a flag: a caller cannot claim the daemon
/// service was both attached and unattached.
#[derive(Clone, Copy, Default)]
pub enum McpSemanticOwnerV1<'a> {
    /// No daemon invocation service was attached to this call.
    #[default]
    NotAttached,
    /// The service was attached and the owner task has no registered state.
    AttachedAbsent,
    Attached(&'a SemanticOwnerStateV1),
}

/// Doctor-report snapshot the root computed for this request.
///
/// One state, not an `Option` plus a flag: a caller cannot claim the reader
/// both failed and was never attached.
#[derive(Clone, Copy, Default)]
pub enum McpDoctorReportV1<'a> {
    /// No doctor reader was attached to this call.
    #[default]
    NotAttached,
    /// The reader ran and failed. Status/runtime report this as `unknown`.
    ReadFailed,
    Read(&'a AdmittedDoctorReportV1),
}

/// Per-request snapshots and admitted executors a moved handler family may read.
///
/// Snapshots, not readers: the root computes freshness, census, semantic-owner,
/// and doctor report once per call and passes the values. Absence is typed.
#[derive(Clone, Copy, Default)]
pub struct McpRequestAuthoritiesV1<'a> {
    pub controls: RequestControls<'a>,
    pub code_index: Option<AdmittedCodeIndex<'a>>,
    /// Scheduler-freshness reader invoked at serve time, not at bind.
    /// Search and context call it after the lanes settle.
    pub freshness: Option<&'a CodeIndexFreshnessReader>,
    pub generation_census: Option<&'a GenerationCensusSnapshot>,
    pub semantic_owner: McpSemanticOwnerV1<'a>,
    pub doctor_report: McpDoctorReportV1<'a>,
}

/// Everything the composition root admits for one MCP tool call.
///
/// One serving shape: the route published a project snapshot, and root,
/// scope, branch, and session store live only on that snapshot — there is
/// no second label a caller can set beside it. A call that never published
/// a checkout is a typed root failure, not a second binding shape.
#[derive(Clone, Copy)]
pub struct McpToolBinding<'a> {
    pub project: &'a McpAdmittedProjectV1,
    pub request: McpRequestAuthoritiesV1<'a>,
}

/// Admitted daemon authorities for one MCP tool call.
///
/// Borrowed for the duration of the call: the root owns every authority and
/// the handler family only reads them, so no handler can outlive the
/// admission that produced them.
pub struct McpToolContext<'a> {
    project: &'a McpAdmittedProjectV1,
    request: McpRequestAuthoritiesV1<'a>,
    project_root: &'a Path,
    active_branch: Option<&'a str>,
    deadline: Option<&'a Deadline>,
    cancellation: Option<&'a CancellationSignal>,
    /// The one checkout every admitted authority in this binding belongs to.
    admitted_scope: &'a ResolvedScope,
    project_session_db: Option<&'a RegisteredGlobalDbLeaseV1>,
    code_index_search_executor: Option<&'a CodeIndexSearchExecutor>,
    code_index_branch_diff_executor: Option<&'a CodeIndexBranchDiffExecutor>,
    code_index_search_authority: Option<&'a CodeIndexSearchAuthorityV1>,
}

impl<'a> McpToolContext<'a> {
    /// Validates one admitted binding and freezes it for the call.
    ///
    /// The root must be absolute, the admitted scope self-consistent, and every
    /// scoped authority must actually have that scope to be admitted under. A
    /// store lease is checked against its own logical shard identity, so a
    /// lease opened for another project is refused whatever scope accompanied
    /// it. Nothing is defaulted or repaired: a binding that does not prove one
    /// coherent request scope is refused whole.
    pub fn bind(binding: McpToolBinding<'a>) -> std::result::Result<Self, McpToolBindingError> {
        let McpToolBinding { project, request } = binding;
        let project_root = project.identity().project_root.as_path();
        let admitted_scope = &project.identity().scope;
        let project_session_store = project
            .project_session_store()
            .map(AdmittedProjectStore::new);
        if !project_root.is_absolute() {
            return Err(McpToolBindingError::RelativeProjectRoot {
                root: project_root.display().to_string(),
            });
        }
        if let Err(error) = admitted_scope.validate() {
            return Err(McpToolBindingError::ScopeInvalid {
                detail: error.to_string(),
            });
        }
        if let Some(store) = project_session_store {
            verify_store_lease(admitted_scope, store)?;
        }

        Ok(Self {
            project,
            request,
            project_root,
            active_branch: project.identity().active_branch.as_deref(),
            deadline: request.controls.deadline,
            cancellation: request.controls.cancellation,
            admitted_scope,
            project_session_db: project_session_store.map(|store| store.lease),
            code_index_search_executor: request.code_index.and_then(|code_index| code_index.search),
            code_index_branch_diff_executor: request
                .code_index
                .and_then(|code_index| code_index.branch_diff),
            code_index_search_authority: request.code_index.map(|code_index| code_index.authority),
        })
    }

    #[must_use]
    pub fn project(&self) -> &'a McpAdmittedProjectV1 {
        self.project
    }

    #[must_use]
    pub fn request(&self) -> McpRequestAuthoritiesV1<'a> {
        self.request
    }

    #[must_use]
    pub fn store_layout(&self) -> &'a StoreLayout {
        self.project.store_layout()
    }

    #[must_use]
    pub fn graph_database(&self) -> &'a Database {
        self.project.graph_database()
    }

    #[must_use]
    pub fn graph_db_path(&self) -> &'a Path {
        self.project.graph_db_path()
    }

    #[must_use]
    pub fn store_runtime(&self) -> &'a DaemonSessionRuntimeRegistryV1 {
        self.project.store_runtime()
    }

    #[must_use]
    pub fn configuration_runtime(&self) -> &'a ProjectConfigurationRuntime {
        self.project.configuration_runtime()
    }

    #[must_use]
    pub fn serving_branch(&self) -> Option<&'a str> {
        self.project.identity().serving_branch.as_deref()
    }

    /// Reads scheduler freshness now. Search and context must call this after
    /// the lanes settle so the verdict describes serve time, not bind time.
    pub async fn freshness(&self) -> Option<CodeIndexFreshnessPayloadV1> {
        let reader = self.request.freshness?;
        let worktree = reader(self.project_root().to_path_buf()).await;
        Some(CodeIndexFreshnessPayloadV1::from_scheduler_read(worktree))
    }

    #[must_use]
    pub fn generation_census(&self) -> Option<&'a GenerationCensusSnapshot> {
        self.request.generation_census
    }

    #[must_use]
    pub fn semantic_owner(&self) -> McpSemanticOwnerV1<'a> {
        self.request.semantic_owner
    }

    #[must_use]
    pub fn doctor_report(&self) -> McpDoctorReportV1<'a> {
        self.request.doctor_report
    }

    #[must_use]
    pub fn branch_diagnostics(&self) -> tracedecay_application::tracedecay::BranchDiagnostics {
        self.project.branch_diagnostics()
    }

    #[must_use]
    pub fn branch_diagnostics_for_serving_source(
        &self,
        serving_source_reference: Option<&str>,
        serving_source_revision: Option<&str>,
    ) -> tracedecay_application::tracedecay::BranchDiagnostics {
        self.project.branch_diagnostics_for_serving_source(
            serving_source_reference,
            serving_source_revision,
        )
    }

    #[must_use]
    pub fn project_root(&self) -> &'a Path {
        self.project_root
    }

    #[must_use]
    pub fn active_branch(&self) -> Option<&'a str> {
        self.active_branch
    }

    #[must_use]
    pub fn deadline(&self) -> Option<&'a Deadline> {
        self.deadline
    }

    #[must_use]
    pub fn cancellation(&self) -> Option<&'a CancellationSignal> {
        self.cancellation
    }

    /// The one checkout this call is admitted for.
    #[must_use]
    pub fn admitted_scope(&self) -> &'a ResolvedScope {
        self.admitted_scope
    }

    /// The admitted project store, or the typed unavailable/denied absence.
    ///
    /// Attached means admitted: bind derives [`ValidatedAuthorization::Authorized`]
    /// from the lease's presence. An absent lease is the typed denied state,
    /// not an inferred capability.
    #[must_use]
    pub fn authorized_project_session_db(
        &self,
    ) -> Option<(&'a RegisteredGlobalDbLeaseV1, ValidatedAuthorization)> {
        self.project_session_db
            .map(|lease| (lease, ValidatedAuthorization::Authorized))
    }

    #[must_use]
    pub fn code_index_search_executor(&self) -> Option<&'a CodeIndexSearchExecutor> {
        self.code_index_search_executor
    }

    #[must_use]
    pub fn code_index_branch_diff_executor(&self) -> Option<&'a CodeIndexBranchDiffExecutor> {
        self.code_index_branch_diff_executor
    }

    #[must_use]
    pub fn code_index_search_authority(&self) -> Option<&'a CodeIndexSearchAuthorityV1> {
        self.code_index_search_authority
    }

    /// Admits a resolved graph query into this call's scope.
    ///
    /// The graph authority only exists once its admission future resolves, so
    /// it cannot be cross-checked at bind time; every handler that awaits one
    /// passes it through here first. The graph carries its own resolved scope,
    /// which must name the checkout this call was admitted for. With no
    /// admitted scope there is nothing to isolate against and the query is
    /// refused rather than trusted.
    pub fn verify_graph_scope(&self, graph: &VerifiedGraphQuery) -> Result<()> {
        verify_scope_isolation(Some(self.admitted_scope), graph.request_context().scope())
    }
}

impl std::fmt::Debug for McpToolContext<'_> {
    /// Names the admitted set without reaching into any authority: the
    /// executors are opaque closures and a store lease has no printable form,
    /// so presence is the diagnostic.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpToolContext")
            .field("project_id", &self.project.identity().scope.project_id)
            .field("has_freshness_reader", &self.request.freshness.is_some())
            .field(
                "has_generation_census",
                &self.request.generation_census.is_some(),
            )
            .field(
                "semantic_owner",
                &matches!(self.request.semantic_owner, McpSemanticOwnerV1::Attached(_)),
            )
            .field(
                "doctor_report",
                &matches!(self.request.doctor_report, McpDoctorReportV1::Read(_)),
            )
            .field("project_root", &self.project_root)
            .field("active_branch", &self.active_branch)
            .field("admitted_scope", &self.admitted_scope)
            .field("has_deadline", &self.deadline.is_some())
            .field("has_cancellation", &self.cancellation.is_some())
            .field("has_project_session_db", &self.project_session_db.is_some())
            .field(
                "has_code_index_search_executor",
                &self.code_index_search_executor.is_some(),
            )
            .field(
                "has_code_index_branch_diff_executor",
                &self.code_index_branch_diff_executor.is_some(),
            )
            .field(
                "has_code_index_search_authority",
                &self.code_index_search_authority.is_some(),
            )
            .finish()
    }
}

/// The project a shard is the *session* store for, if it is one at all.
///
/// The family is matched exactly, and exhaustively so a shard family added
/// later must be classified here rather than silently inheriting an answer.
/// [`StoreShardScopeV1::project_id`] cannot stand in: it reports `Project`,
/// `ProjectSessions`, and `Code` shards alike, so a project-only comparison
/// would accept a lease on this project's *project* store or one of its code
/// stores as project-session authority. Those are separate stores with their
/// own tables and retention.
fn session_shard_project(shard: &StoreShardScopeV1) -> Option<&tracedecay_domain::ProjectId> {
    match shard {
        StoreShardScopeV1::ProjectSessions { project_id } => Some(project_id),
        StoreShardScopeV1::Profile
        | StoreShardScopeV1::ProfileMemory
        | StoreShardScopeV1::ProfileSessions
        | StoreShardScopeV1::RemoteNode { .. }
        | StoreShardScopeV1::Project { .. }
        | StoreShardScopeV1::Code { .. } => None,
    }
}

/// Refuses a store lease that is not this project's session store.
///
/// The lease reports the logical shard the registry opened it for, which is
/// the store's own identity rather than a label travelling beside it.
fn verify_store_lease(
    scope: &ResolvedScope,
    store: AdmittedProjectStore<'_>,
) -> std::result::Result<(), McpToolBindingError> {
    let shard = &store.lease.binding().shard_id;
    let Some(lease_project) = session_shard_project(&shard.scope) else {
        return Err(McpToolBindingError::ProjectStoreNotSessionScoped {
            request: checkout_label(scope),
            shard: format!("{:?}", shard.scope),
        });
    };
    if lease_project != &scope.project_id {
        return Err(McpToolBindingError::ProjectStoreProjectMismatch {
            request: checkout_label(scope),
            lease: lease_project.as_str().to_owned(),
        });
    }
    Ok(())
}

/// Refuses a graph admitted for a different checkout than this call.
fn verify_scope_isolation(admitted: Option<&ResolvedScope>, graph: &ResolvedScope) -> Result<()> {
    let Some(admitted) = admitted else {
        return Err(TraceDecayError::project_route(
            "mcp_tool_graph_scope_unresolved",
            false,
            format!(
                "verified graph answers for checkout {} but this request resolved no admitted scope",
                checkout_label(graph)
            ),
        ));
    };
    if admitted.identifies_same_checkout(graph) {
        return Ok(());
    }
    Err(TraceDecayError::project_route(
        "mcp_tool_graph_scope_mismatch",
        false,
        format!(
            "verified graph answers for checkout {} but this request is admitted for {}",
            checkout_label(graph),
            checkout_label(admitted)
        ),
    ))
}

/// Names the physical checkout a scope identifies, for operator-facing refusals.
fn checkout_label(scope: &ResolvedScope) -> String {
    format!(
        "{}/{}/{}",
        scope.project_id.as_str(),
        scope.repository_id.as_str(),
        scope.worktree_id.as_str()
    )
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
pub(crate) mod tests {
    use super::*;
    use tracedecay_configuration::{
        OpenedRuntimeConfiguration, PinnedRuntimeConfiguration, RuntimeConfigurationTarget,
    };
    use tracedecay_domain::configuration::ConfigurationRevisionId;
    use tracedecay_domain::{ProjectId, RepositoryId, WorktreeId};
    use tracedecay_global_db::tests::harness::RegisteredGlobalDbTestRuntime;

    /// A registered project session store opened by the production
    /// registration path, so a binding is checked against a lease's own
    /// logical shard rather than a stand-in that repeats whatever the caller
    /// claimed.
    async fn registered_project_store(
        home: &Path,
        project: &str,
    ) -> (RegisteredGlobalDbTestRuntime, RegisteredGlobalDbLeaseV1) {
        let project_id = ProjectId::new(format!("project.{project}")).expect("project id");
        let runtime = RegisteredGlobalDbTestRuntime::project(
            home.join(format!("profile-{project}")),
            home.join(format!("checkout-{project}")),
            project_id,
        )
        .await
        .expect("registered project store");
        let lease = runtime
            .project_database_arc()
            .expect("registered project lease");
        (runtime, lease)
    }

    /// A registered store opened for one exact shard family through the same
    /// publication, schema installation, and client issuance route production
    /// admission uses. The owner is returned so the caller keeps it alive.
    async fn registered_store_for_shard(
        home: &Path,
        label: &str,
        scope: tracedecay_runtime_core::db::TestDatabaseRuntimeScope,
    ) -> (
        RegisteredGlobalDbLeaseV1,
        tracedecay_global_db::RegisteredGlobalDbOwnerV1,
    ) {
        let path = home.join(label).join("store.sqlite3");
        tracedecay_global_db::tests::harness::open_registered_test_database_fixture(&path, scope)
            .await
            .expect("registered store fixture")
    }

    pub(crate) fn scope(project: &str) -> ResolvedScope {
        ResolvedScope::new(
            ProjectId::new(format!("project.{project}")).expect("project id"),
            RepositoryId::new(format!("repository.{project}")).expect("repository id"),
            WorktreeId::new(format!("worktree.{project}")).expect("worktree id"),
            None,
        )
        .expect("scope")
    }

    struct FixtureHandles {
        graph_database: Database,
        store_runtime: Arc<DaemonSessionRuntimeRegistryV1>,
        configuration_runtime: Arc<ProjectConfigurationRuntime>,
    }

    fn fixture_handles() -> &'static FixtureHandles {
        static HANDLES: std::sync::OnceLock<FixtureHandles> = std::sync::OnceLock::new();
        HANDLES.get_or_init(open_fixture_handles_blocking)
    }

    fn open_fixture_handles_blocking() -> FixtureHandles {
        std::thread::Builder::new()
            .name("mcp-admitted-handles".into())
            .spawn(|| {
                let runtime = tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .enable_all()
                    .build()
                    .expect("fixture runtime");
                let handles = runtime.block_on(open_fixture_handles());
                std::mem::forget(runtime);
                handles
            })
            .expect("spawn fixture thread")
            .join()
            .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
    }

    async fn open_fixture_handles() -> FixtureHandles {
        let temp = tempfile::tempdir().expect("fixture home");
        let profile_root = temp.path().join("profile");
        std::fs::create_dir_all(&profile_root).expect("profile root");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&profile_root, std::fs::Permissions::from_mode(0o700))
                .expect("private profile root");
        }
        let identity = tracedecay_daemon_identity::profile_identity::load_or_create(&profile_root)
            .expect("profile identity");
        let scope = tracedecay_runtime_core::db::enter_daemon_database_scope(
            &profile_root,
            1,
            "mcp-admitted-handles",
        )
        .expect("daemon database scope");
        let store_runtime = Arc::new(
            DaemonSessionRuntimeRegistryV1::open(identity)
                .await
                .expect("store runtime"),
        );
        let graph_path = temp.path().join("graph.db");
        let authority = tracedecay_runtime_core::db::DatabaseAuthority::acquire_test(
            &graph_path,
            "mcp admitted project fixture",
        )
        .expect("test database authority");
        let (graph_database, _) = Database::publish_test_runtime(
            &graph_path,
            &authority,
            tracedecay_runtime_core::db::TestDatabaseRuntimeMode::Initialize,
        )
        .await
        .expect("graph database");
        let project_id = ProjectId::new("project.admitted-handles".to_owned()).expect("project id");
        let sessions = RegisteredGlobalDbTestRuntime::project(
            profile_root.join("sessions-profile"),
            temp.path().join("checkout"),
            project_id.clone(),
        )
        .await
        .expect("configuration store");
        let lease = sessions
            .project_database_arc()
            .expect("configuration lease");
        let snapshot = tracedecay_configuration::config::resolver::resolve_configuration(
            &tracedecay_configuration::config::registry::ConfigurationRegistry::core()
                .expect("configuration registry"),
            &[],
        )
        .expect("default configuration")
        .snapshot;
        let pinned = PinnedRuntimeConfiguration::new(
            RuntimeConfigurationTarget {
                project_id,
                project_root: temp.path().to_path_buf(),
            },
            ConfigurationRevisionId::new("configuration.revision.mcp-admitted-handles")
                .expect("revision"),
            snapshot,
        )
        .expect("pinned configuration");
        let (configuration_runtime, _) =
            ProjectConfigurationRuntime::open(OpenedRuntimeConfiguration::new(pinned, lease))
                .expect("configuration runtime");
        std::mem::forget(temp);
        std::mem::forget(scope);
        std::mem::forget(sessions);
        FixtureHandles {
            graph_database,
            store_runtime,
            configuration_runtime: Arc::new(configuration_runtime),
        }
    }

    fn runtime_handles() -> (
        Database,
        Arc<DaemonSessionRuntimeRegistryV1>,
        Arc<ProjectConfigurationRuntime>,
    ) {
        let handles = fixture_handles();
        (
            handles.graph_database.clone(),
            Arc::clone(&handles.store_runtime),
            Arc::clone(&handles.configuration_runtime),
        )
    }

    fn test_store_layout(root: &Path, project_id: &str) -> StoreLayout {
        StoreLayout {
            identity: tracedecay_runtime_core::storage::ProjectIdentity {
                project_id: Some(project_id.to_owned()),
                display_root: root.to_path_buf(),
                primary_alias: root.to_path_buf(),
            },
            store_kind: tracedecay_runtime_core::storage::StoreKind::CodeProject,
            storage_mode: tracedecay_runtime_core::storage::StorageMode::ProjectLocal,
            project_root: root.to_path_buf(),
            data_root: root.join(".tracedecay"),
            graph_db_path: root.join("graph.db"),
            config_path: root.join("config.toml"),
            branch_meta_path: root.join("branch-meta.json"),
            sessions_db_path: root.join("sessions.db"),
            response_handle_root: root.join("handles"),
            lcm_payload_root: root.join("lcm"),
            dashboard_root: root.join("dashboard"),
            manifest_path: None,
            dirty_path: root.join("dirty"),
            sync_lock_path: root.join("sync.lock"),
            branch_add_lock_path: root.join("branch-add.lock"),
        }
    }

    fn project_identity(root: &Path, admitted: &ResolvedScope) -> McpProjectIdentityV1 {
        McpProjectIdentityV1 {
            project_root: root.to_path_buf(),
            scope: admitted.clone(),
            active_branch: None,
            serving_branch: None,
            fallback_warning: None,
        }
    }

    fn admit_project(
        identity: McpProjectIdentityV1,
        store_layout: StoreLayout,
        graph_db_path: PathBuf,
        lease: Option<RegisteredGlobalDbLeaseV1>,
    ) -> std::result::Result<McpAdmittedProjectV1, McpToolBindingError> {
        let (graph_database, store_runtime, configuration_runtime) = runtime_handles();
        McpAdmittedProjectV1::new(
            identity,
            store_layout,
            graph_database,
            graph_db_path,
            store_runtime,
            configuration_runtime,
            lease,
        )
    }

    pub(crate) fn project_bundle(
        root: &Path,
        admitted: &ResolvedScope,
        lease: Option<RegisteredGlobalDbLeaseV1>,
    ) -> McpAdmittedProjectV1 {
        admit_project(
            project_identity(root, admitted),
            test_store_layout(root, admitted.project_id.as_str()),
            root.join("graph.db"),
            lease,
        )
        .expect("coherent project bundle")
    }

    /// Test-only admitted snapshot for a worktree root. Git family tests use
    /// this instead of a production `Unprojected` binding — every served
    /// route is admitted.
    pub(crate) fn fixture_project(
        root: &Path,
        active_branch: Option<&str>,
    ) -> McpAdmittedProjectV1 {
        let admitted = scope("git-fixture");
        let mut identity = project_identity(root, &admitted);
        identity.active_branch = active_branch.map(str::to_owned);
        admit_project(
            identity,
            test_store_layout(root, admitted.project_id.as_str()),
            root.join("graph.db"),
            None,
        )
        .expect("fixture project")
    }

    pub(crate) fn fixture_context(project: &McpAdmittedProjectV1) -> McpToolContext<'_> {
        McpToolContext::bind(McpToolBinding {
            project,
            request: McpRequestAuthoritiesV1::default(),
        })
        .expect("admitted fixture binds")
    }

    fn authority() -> CodeIndexSearchAuthorityV1 {
        CodeIndexSearchAuthorityV1 {
            principal: tracedecay_domain::PrincipalId::new("principal.mcp-binding.fixture")
                .expect("principal"),
            authorization_revision: tracedecay_domain::AuthorizationRevision::new(
                "revision.mcp-binding.fixture",
            )
            .expect("revision"),
        }
    }

    /// A code-index admission with no executor is an empty capability claim:
    /// handlers would report the index as mounted while nothing can answer.
    #[test]
    fn a_code_index_admission_without_an_executor_is_refused() {
        let authority = authority();

        let Err(error) = AdmittedCodeIndex::new(&authority, None, None) else {
            panic!("an executorless code index admission must be refused");
        };

        assert_eq!(
            error.reason_code(),
            "mcp_tool_binding_code_index_without_executor"
        );
    }

    /// A graph admitted for another checkout is refused before a handler reads
    /// it, so cross-project graph evidence cannot reach a scoped response.
    #[test]
    fn a_graph_from_another_checkout_is_refused() {
        let admitted = scope("admitted");
        let foreign = scope("foreign");

        let error = verify_scope_isolation(Some(&admitted), &foreign)
            .expect_err("a foreign graph scope must be refused");
        assert_eq!(
            error.project_route_context().map(|(reason, _, _)| reason),
            Some("mcp_tool_graph_scope_mismatch")
        );

        verify_scope_isolation(Some(&admitted), &admitted)
            .expect("the admitted checkout's own graph must pass");
    }

    /// With no admitted scope the graph's own admission is the only identity
    /// in play, and trusting it would let any checkout's graph answer. The
    /// query is refused instead.
    #[test]
    fn a_graph_without_an_admitted_scope_is_refused() {
        let graph = scope("graph");

        let error = verify_scope_isolation(None, &graph)
            .expect_err("an unscoped request must not read a verified graph");
        assert_eq!(
            error.project_route_context().map(|(reason, _, _)| reason),
            Some("mcp_tool_graph_scope_unresolved")
        );
    }

    /// A store lease opened for one project must never be readable through a
    /// context admitted for another. The lease below is a real registered
    /// project store, and the refusal comes from its own logical shard rather
    /// than from any label presented alongside it.
    #[tokio::test]
    async fn a_real_lease_from_another_project_is_refused() {
        let home = tempfile::tempdir().expect("temp home");
        let (_admitted_runtime, admitted_lease) =
            registered_project_store(home.path(), "admitted").await;
        let (_foreign_runtime, foreign_lease) =
            registered_project_store(home.path(), "foreign").await;
        let admitted = scope("admitted");

        let error = admit_project(
            project_identity(home.path(), &admitted),
            test_store_layout(home.path(), admitted.project_id.as_str()),
            home.path().join("graph.db"),
            Some(foreign_lease),
        )
        .expect_err("another project's real lease must be refused");
        assert_eq!(
            error.reason_code(),
            "mcp_tool_binding_store_project_mismatch"
        );
        assert!(error.to_string().contains("project.foreign"), "got {error}");

        let project = project_bundle(home.path(), &admitted, Some(admitted_lease.clone()));
        let bound = fixture_context(&project);
        let (bound_lease, authorization) = bound
            .authorized_project_session_db()
            .expect("the bound store is reported");
        assert!(
            bound_lease.shares_client_with(&admitted_lease),
            "the bound context must report the very lease it was admitted with"
        );
        assert_eq!(authorization, ValidatedAuthorization::Authorized);
    }

    /// The family gate fires on a real registered lease, not just on a shard
    /// identity in isolation. The lease below is genuinely published through
    /// the production registration route and is a real store this daemon
    /// opens — it is simply the profile's session store rather than the
    /// admitted project's, so it carries no project-session authority here.
    #[tokio::test]
    async fn a_real_non_session_shard_lease_is_refused_at_the_binding() {
        let home = tempfile::tempdir().expect("temp home");
        let admitted = scope("admitted");
        let (lease, _owner) = registered_store_for_shard(
            home.path(),
            "profile-sessions",
            tracedecay_runtime_core::db::TestDatabaseRuntimeScope::ProfileSessions,
        )
        .await;

        let error = admit_project(
            project_identity(home.path(), &admitted),
            test_store_layout(home.path(), admitted.project_id.as_str()),
            home.path().join("graph.db"),
            Some(lease),
        )
        .expect_err("a non-session-family lease must be refused");
        assert_eq!(
            error.reason_code(),
            "mcp_tool_binding_store_not_session_shard"
        );
    }

    /// Only `ProjectSessions` carries project-session authority, and the two
    /// families a project-only comparison would have waved through are the
    /// dangerous ones: this project's own `Project` and `Code` shards name the
    /// admitted project exactly, so nothing but the family distinguishes them.
    ///
    /// Both are asserted against canonical production shard identities rather
    /// than through a registered lease because neither family can hold one:
    /// the registered global-db schema is the session store's, so the
    /// publication route refuses a `Project` shard outright and a `Code` shard
    /// is a graph store that never becomes a global-db lease. The lease route
    /// itself is covered by the tests above.
    #[test]
    fn only_the_project_sessions_family_carries_session_authority() {
        let project = ProjectId::new("project.admitted").expect("project id");
        let repository = RepositoryId::new("repository.admitted").expect("repository id");
        let worktree = WorktreeId::new("worktree.admitted").expect("worktree id");

        let code = tracedecay_store::StoreShardIdV1::code(
            tracedecay_domain::BrainId::new("brain.admitted").expect("brain id"),
            tracedecay_domain::UserProfileId::new("profile.admitted").expect("profile id"),
            project.clone(),
            repository,
            tracedecay_store::CodeShardScopeV1::Worktree {
                worktree_id: worktree,
            },
        );
        assert_eq!(
            code.scope.project_id(),
            Some(&project),
            "the code shard names the admitted project, so a project-only \
             comparison would have accepted it"
        );
        assert_eq!(
            session_shard_project(&code.scope),
            None,
            "a code shard is not project-session authority"
        );

        assert_eq!(
            session_shard_project(&StoreShardScopeV1::Project {
                project_id: project.clone()
            }),
            None,
            "a project shard is not project-session authority"
        );
        assert_eq!(
            session_shard_project(&StoreShardScopeV1::Profile),
            None,
            "a profile shard has no project at all"
        );
        assert_eq!(
            session_shard_project(&StoreShardScopeV1::ProjectSessions {
                project_id: project.clone()
            }),
            Some(&project),
            "the project's session shard is the one family that carries it"
        );
    }

    /// An absent lease is the typed denied state: bind must not invent an
    /// authorized store read from nothing.
    #[test]
    fn an_unauthorized_verdict_survives_binding() {
        let temp = tempfile::tempdir().expect("temp root");
        let admitted = scope("admitted");
        let project = project_bundle(temp.path(), &admitted, None);
        let bound = fixture_context(&project);

        assert!(
            bound.authorized_project_session_db().is_none(),
            "an absent lease is the typed unavailable/denied state"
        );
    }

    /// An admitted snapshot without a session-store lease must not become an
    /// authorized store read. Presence of the lease is admission; absence is
    /// the typed denied state the first authorized-only read refuses.
    #[test]
    fn an_admitted_denied_verdict_refuses_the_first_authorized_store_read() {
        let temp = tempfile::tempdir().expect("temp root");
        let admitted = scope("admitted");
        let project = project_bundle(temp.path(), &admitted, None);
        let bound = fixture_context(&project);

        assert!(
            bound.authorized_project_session_db().is_none(),
            "bind must not invent an authorized store from an absent lease"
        );
    }

    /// A checkout differs from another by project, repository, or worktree —
    /// never by the branch reference HEAD happens to carry. Two scopes for the
    /// same checkout on different branches must isolate identically.
    #[test]
    fn a_branch_switch_does_not_change_the_admitted_checkout() {
        let registered = scope("admitted");
        let switched = ResolvedScope::new(
            registered.project_id.clone(),
            registered.repository_id.clone(),
            registered.worktree_id.clone(),
            Some(tracedecay_domain::RefId::new("refs/heads/feature").expect("reference")),
        )
        .expect("scope on another branch");
        assert_ne!(
            registered.scope_digest, switched.scope_digest,
            "fixture must differ in the reference-sensitive digest"
        );

        verify_scope_isolation(Some(&registered), &switched)
            .expect("the same checkout on another branch must still bind");
    }

    /// A relative root cannot identify a project store or resolve handler
    /// paths, so [`McpAdmittedProjectV1::new`] refuses it.
    #[test]
    fn a_relative_project_root_is_refused() {
        let admitted = scope("admitted");
        let error = admit_project(
            McpProjectIdentityV1 {
                project_root: PathBuf::from("relative/root"),
                scope: admitted.clone(),
                active_branch: None,
                serving_branch: None,
                fallback_warning: None,
            },
            test_store_layout(Path::new("/tmp/admitted"), admitted.project_id.as_str()),
            PathBuf::from("relative/root/graph.db"),
            None,
        )
        .expect_err("a relative project root must be refused");
        assert_eq!(error.reason_code(), "mcp_tool_binding_root_not_absolute");
    }

    /// The store layout names a different project than the admitted scope.
    /// Handlers would otherwise read that project's graph and sessions under
    /// the wrong checkout label.
    #[test]
    fn a_store_layout_for_another_project_is_refused() {
        let temp = tempfile::tempdir().expect("temp root");
        let admitted = scope("admitted");
        let error = admit_project(
            project_identity(temp.path(), &admitted),
            test_store_layout(temp.path(), "project.foreign"),
            temp.path().join("graph.db"),
            None,
        )
        .expect_err("a foreign store-layout project id must be refused");
        assert_eq!(
            error.reason_code(),
            "mcp_tool_binding_store_layout_project_mismatch"
        );
        assert!(error.to_string().contains("project.foreign"), "got {error}");
    }

    /// A real registered lease that is not a `ProjectSessions` shard cannot
    /// become the project-session authority on the bundle.
    #[tokio::test]
    async fn a_non_session_shard_lease_is_refused_by_the_project_bundle() {
        let home = tempfile::tempdir().expect("temp home");
        let admitted = scope("admitted");
        let (lease, _owner) = registered_store_for_shard(
            home.path(),
            "profile-sessions",
            tracedecay_runtime_core::db::TestDatabaseRuntimeScope::ProfileSessions,
        )
        .await;

        let error = admit_project(
            project_identity(home.path(), &admitted),
            test_store_layout(home.path(), admitted.project_id.as_str()),
            home.path().join("graph.db"),
            Some(lease),
        )
        .expect_err("a non-session-family lease must be refused");
        assert_eq!(
            error.reason_code(),
            "mcp_tool_binding_store_not_session_shard"
        );
    }

    /// Missing optional request authorities stay typed absence: a handler
    /// reading them gets `None`, never a panic and never an invented empty
    /// success value.
    #[test]
    fn a_missing_optional_authority_is_typed_absence() {
        let temp = tempfile::tempdir().expect("temp root");
        let admitted = scope("admitted");
        let project = project_bundle(temp.path(), &admitted, None);

        let bound = McpToolContext::bind(McpToolBinding {
            project: &project,
            request: McpRequestAuthoritiesV1::default(),
        })
        .expect("a snapshot with no optional request authorities must still bind");

        assert!(
            bound.generation_census().is_none(),
            "census absence must stay None"
        );
        assert!(
            matches!(bound.semantic_owner(), McpSemanticOwnerV1::NotAttached),
            "semantic-owner absence must stay NotAttached"
        );
        assert!(
            bound.request().freshness.is_none(),
            "freshness-reader absence must stay None"
        );
        assert!(
            matches!(bound.doctor_report(), McpDoctorReportV1::NotAttached),
            "doctor-report absence must stay NotAttached"
        );
        assert!(
            bound.authorized_project_session_db().is_none(),
            "session-store absence must stay None"
        );
        assert_eq!(
            tracedecay_runtime_core::runtime_telemetry::GenerationCensusSnapshot::Unavailable {
                reason: tracedecay_runtime_core::runtime_telemetry::GenerationCensusUnavailableReason::AuthorityUnavailable,
            },
            bound.generation_census().cloned().unwrap_or(
                tracedecay_runtime_core::runtime_telemetry::GenerationCensusSnapshot::Unavailable {
                    reason: tracedecay_runtime_core::runtime_telemetry::GenerationCensusUnavailableReason::AuthorityUnavailable,
                }
            ),
            "the typed unavailable census is what a handler must emit, not an empty success"
        );
    }

    /// An admitted snapshot is the only source of root, scope, and store.
    /// [`McpToolBinding`] has no fields that could supply a second label;
    /// bind reports the snapshot's identity.
    #[test]
    fn an_admitted_binding_cannot_carry_a_second_root_or_scope() {
        let temp = tempfile::tempdir().expect("temp root");
        let admitted = scope("admitted");
        let project = project_bundle(temp.path(), &admitted, None);

        let bound = McpToolContext::bind(McpToolBinding {
            project: &project,
            request: McpRequestAuthoritiesV1::default(),
        })
        .expect("admitted snapshot binds");

        assert_eq!(
            bound.project_root(),
            project.identity().project_root.as_path()
        );
        assert_eq!(bound.admitted_scope(), &project.identity().scope);
        assert_eq!(
            bound.project().identity().project_root,
            project.identity().project_root
        );
    }
}
