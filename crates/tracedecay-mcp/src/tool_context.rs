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
//! lease reports the logical shard it was opened for, and a lease whose shard
//! names a different project is refused however it was presented.
//!
//! Authorization is carried, never inferred. The root validated whether this
//! request may read the admitted project store and hands that verdict over
//! with the lease; the context reports it verbatim and cannot upgrade a
//! missing or unauthorized verdict into an apparent capability.
//!
//! Absence stays typed. An authority the daemon never admitted is `None` here
//! and each handler turns that into its own unavailable state. With no
//! admitted scope no scoped authority may be admitted at all, and graph
//! verification refuses rather than waving a query through.

use std::path::Path;

use tracedecay_contracts::{CancellationSignal, Deadline, ResolvedScope};
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_global_db::RegisteredGlobalDbLeaseV1;
use tracedecay_graph_query::VerifiedGraphQuery;
use tracedecay_query::code_search::{
    CodeIndexBranchDiffExecutor, CodeIndexSearchAuthorityV1, CodeIndexSearchExecutor,
};
use tracedecay_temporal_query::resolution::ValidatedAuthorization;

/// Why a proposed binding is not one coherent admitted request scope.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum McpToolBindingError {
    #[error("admitted project root '{root}' is not absolute")]
    RelativeProjectRoot { root: String },
    #[error("admitted request scope is not self-consistent: {detail}")]
    ScopeInvalid { detail: String },
    #[error("{authority} cannot be admitted without a resolved request scope")]
    UnscopedAuthority { authority: &'static str },
    #[error(
        "project session store lease is not scoped to a project; its shard is {shard} while this request resolved {request}"
    )]
    ProjectStoreNotProjectScoped { request: String, shard: String },
    #[error(
        "project session store lease was opened for project {lease} but this request resolved {request}"
    )]
    ProjectStoreProjectMismatch { request: String, lease: String },
    #[error("code index admission carries no executor to authorize")]
    CodeIndexWithoutExecutor,
}

impl McpToolBindingError {
    /// The stable reason code an operator sees for a refused binding.
    #[must_use]
    pub fn reason_code(&self) -> &'static str {
        match self {
            Self::RelativeProjectRoot { .. } => "mcp_tool_binding_root_not_absolute",
            Self::ScopeInvalid { .. } => "mcp_tool_binding_scope_invalid",
            Self::UnscopedAuthority { .. } => "mcp_tool_binding_scope_unresolved",
            Self::ProjectStoreNotProjectScoped { .. } => "mcp_tool_binding_store_not_project_shard",
            Self::ProjectStoreProjectMismatch { .. } => "mcp_tool_binding_store_project_mismatch",
            Self::CodeIndexWithoutExecutor => "mcp_tool_binding_code_index_without_executor",
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

/// The registered project session store the daemon opened for this request,
/// with the authorization the daemon validated for reading it.
///
/// Both halves come from the root. The lease knows the logical shard it was
/// opened for, so [`McpToolContext::bind`] can check it against the admitted
/// checkout instead of trusting how it was presented; the authorization is the
/// root's own verdict and is carried through untouched.
#[derive(Clone, Copy)]
pub struct AdmittedProjectStore<'a> {
    lease: &'a RegisteredGlobalDbLeaseV1,
    authorization: ValidatedAuthorization,
}

impl<'a> AdmittedProjectStore<'a> {
    /// Pairs the lease the root opened with the verdict the root reached.
    ///
    /// `authorization` must be the authorization the daemon validated for this
    /// request. Passing [`ValidatedAuthorization::Unauthorized`] keeps every
    /// store-backed handler denied; there is no value that means "decide later".
    #[must_use]
    pub fn new(
        lease: &'a RegisteredGlobalDbLeaseV1,
        authorization: ValidatedAuthorization,
    ) -> Self {
        Self {
            lease,
            authorization,
        }
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

/// Everything the composition root admits for one MCP tool call.
#[derive(Clone, Copy)]
pub struct McpToolBinding<'a> {
    /// The admitted worktree root every handler resolves paths against.
    pub project_root: &'a Path,
    /// The branch git resolved for that worktree, when it has one.
    pub active_branch: Option<&'a str>,
    pub controls: RequestControls<'a>,
    /// The one checkout the daemon admitted for this request. Absent on a
    /// standalone server and on the core server that answers before
    /// project-open publication resolves a route.
    pub scope: Option<&'a ResolvedScope>,
    pub project_session_store: Option<AdmittedProjectStore<'a>>,
    pub code_index: Option<AdmittedCodeIndex<'a>>,
}

/// Admitted daemon authorities for one MCP tool call.
///
/// Borrowed for the duration of the call: the root owns every authority and
/// the handler family only reads them, so no handler can outlive the
/// admission that produced them.
pub struct McpToolContext<'a> {
    project_root: &'a Path,
    active_branch: Option<&'a str>,
    deadline: Option<&'a Deadline>,
    cancellation: Option<&'a CancellationSignal>,
    /// The one checkout every admitted authority in this binding belongs to.
    admitted_scope: Option<&'a ResolvedScope>,
    project_session_db: Option<&'a RegisteredGlobalDbLeaseV1>,
    /// The root's verdict for reading `project_session_db`, carried verbatim.
    project_session_authorization: Option<ValidatedAuthorization>,
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
        if !binding.project_root.is_absolute() {
            return Err(McpToolBindingError::RelativeProjectRoot {
                root: binding.project_root.display().to_string(),
            });
        }
        if let Some(scope) = binding.scope
            && let Err(error) = scope.validate()
        {
            return Err(McpToolBindingError::ScopeInvalid {
                detail: error.to_string(),
            });
        }
        if let Some(store) = binding.project_session_store {
            verify_store_lease(
                require_scope(binding.scope, "project session store")?,
                store,
            )?;
        }
        if binding.code_index.is_some() {
            require_scope(binding.scope, "code index")?;
        }

        Ok(Self {
            project_root: binding.project_root,
            active_branch: binding.active_branch,
            deadline: binding.controls.deadline,
            cancellation: binding.controls.cancellation,
            admitted_scope: binding.scope,
            project_session_db: binding.project_session_store.map(|store| store.lease),
            project_session_authorization: binding
                .project_session_store
                .map(|store| store.authorization),
            code_index_search_executor: binding.code_index.and_then(|code_index| code_index.search),
            code_index_branch_diff_executor: binding
                .code_index
                .and_then(|code_index| code_index.branch_diff),
            code_index_search_authority: binding.code_index.map(|code_index| code_index.authority),
        })
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

    /// The one checkout this call is admitted for, when the daemon resolved one.
    #[must_use]
    pub fn admitted_scope(&self) -> Option<&'a ResolvedScope> {
        self.admitted_scope
    }

    /// The admitted project store together with the root's authorization.
    ///
    /// The verdict is the root's, carried through [`Self::bind`] unchanged: a
    /// handler cannot decide for itself that a store read is authorized, and
    /// this accessor never supplies a verdict of its own. With no admitted
    /// store the caller receives the typed absence instead.
    #[must_use]
    pub fn authorized_project_session_db(
        &self,
    ) -> Option<(&'a RegisteredGlobalDbLeaseV1, ValidatedAuthorization)> {
        self.project_session_db
            .zip(self.project_session_authorization)
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
        verify_scope_isolation(self.admitted_scope, graph.request_context().scope())
    }
}

impl std::fmt::Debug for McpToolContext<'_> {
    /// Names the admitted set without reaching into any authority: the
    /// executors are opaque closures and a store lease has no printable form,
    /// so presence is the diagnostic.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpToolContext")
            .field("project_root", &self.project_root)
            .field("active_branch", &self.active_branch)
            .field("admitted_scope", &self.admitted_scope)
            .field("has_deadline", &self.deadline.is_some())
            .field("has_cancellation", &self.cancellation.is_some())
            .field("has_project_session_db", &self.project_session_db.is_some())
            .field(
                "project_session_authorization",
                &self.project_session_authorization,
            )
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

/// The admitted scope a scoped authority needs, or a typed refusal.
///
/// An authority the daemon scoped cannot be admitted into a request that never
/// resolved a checkout: there would be nothing to isolate it against, and a
/// handler reading it would answer from whatever project the authority happens
/// to hold.
fn require_scope<'a>(
    scope: Option<&'a ResolvedScope>,
    authority: &'static str,
) -> std::result::Result<&'a ResolvedScope, McpToolBindingError> {
    scope.ok_or(McpToolBindingError::UnscopedAuthority { authority })
}

/// Refuses a store lease whose own logical shard names another project.
///
/// The lease reports the shard the registry opened it for, which is the
/// store's own identity rather than a label travelling beside it. A profile or
/// remote-node shard has no project at all and cannot serve a project-scoped
/// read.
fn verify_store_lease(
    scope: &ResolvedScope,
    store: AdmittedProjectStore<'_>,
) -> std::result::Result<(), McpToolBindingError> {
    let shard = &store.lease.binding().shard_id;
    let Some(lease_project) = shard.scope.project_id() else {
        return Err(McpToolBindingError::ProjectStoreNotProjectScoped {
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
mod tests {
    use super::*;
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

    fn scope(project: &str) -> ResolvedScope {
        ResolvedScope::new(
            ProjectId::new(format!("project.{project}")).expect("project id"),
            RepositoryId::new(format!("repository.{project}")).expect("repository id"),
            WorktreeId::new(format!("worktree.{project}")).expect("worktree id"),
            None,
        )
        .expect("scope")
    }

    fn binding<'a>(root: &'a Path, scope: Option<&'a ResolvedScope>) -> McpToolBinding<'a> {
        McpToolBinding {
            project_root: root,
            active_branch: None,
            controls: RequestControls::default(),
            scope,
            project_session_store: None,
            code_index: None,
        }
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

    /// A relative root cannot anchor path resolution or identity, so it is
    /// refused instead of silently joined against the process directory.
    #[test]
    fn a_relative_project_root_is_refused() {
        let error = McpToolContext::bind(binding(Path::new("relative/root"), None))
            .expect_err("relative root must be refused");
        assert_eq!(error.reason_code(), "mcp_tool_binding_root_not_absolute");
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

    /// A request that resolved no checkout has nothing to isolate a scoped
    /// authority against, so admitting one fails closed rather than reading
    /// whatever project the authority happens to hold.
    #[test]
    fn a_code_index_cannot_be_admitted_without_a_resolved_scope() {
        let temp = tempfile::tempdir().expect("temp root");
        let authority = authority();
        let search: CodeIndexSearchExecutor =
            std::sync::Arc::new(|_| unreachable!("binding must be refused before any search runs"));

        let error = McpToolContext::bind(McpToolBinding {
            code_index: Some(
                AdmittedCodeIndex::new(&authority, Some(&search), None).expect("admission"),
            ),
            ..binding(temp.path(), None)
        })
        .expect_err("an unscoped code index admission must be refused");

        assert_eq!(error.reason_code(), "mcp_tool_binding_scope_unresolved");
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

        let error = McpToolContext::bind(McpToolBinding {
            project_session_store: Some(AdmittedProjectStore::new(
                &foreign_lease,
                ValidatedAuthorization::Authorized,
            )),
            ..binding(home.path(), Some(&admitted))
        })
        .map(|_| ())
        .expect_err("another project's real lease must be refused");
        assert_eq!(
            error.reason_code(),
            "mcp_tool_binding_store_project_mismatch"
        );
        assert!(error.to_string().contains("project.foreign"), "got {error}");

        let bound = McpToolContext::bind(McpToolBinding {
            project_session_store: Some(AdmittedProjectStore::new(
                &admitted_lease,
                ValidatedAuthorization::Authorized,
            )),
            ..binding(home.path(), Some(&admitted))
        })
        .expect("the admitted project's own lease must bind");
        let (bound_lease, authorization) = bound
            .authorized_project_session_db()
            .expect("the bound store is reported");
        assert!(
            bound_lease.shares_client_with(&admitted_lease),
            "the bound context must report the very lease it was admitted with"
        );
        assert_eq!(authorization, ValidatedAuthorization::Authorized);
    }

    /// A request that resolved no checkout cannot admit a store either: there
    /// would be no identity to check the lease's shard against.
    #[tokio::test]
    async fn a_real_lease_cannot_be_admitted_without_a_resolved_scope() {
        let home = tempfile::tempdir().expect("temp home");
        let (_runtime, lease) = registered_project_store(home.path(), "admitted").await;

        let error = McpToolContext::bind(McpToolBinding {
            project_session_store: Some(AdmittedProjectStore::new(
                &lease,
                ValidatedAuthorization::Authorized,
            )),
            ..binding(home.path(), None)
        })
        .map(|_| ())
        .expect_err("an unscoped store admission must be refused");

        assert_eq!(error.reason_code(), "mcp_tool_binding_scope_unresolved");
    }

    /// The root's verdict is carried, not re-derived: a context bound with an
    /// unauthorized store reports exactly that, so every store-backed handler
    /// denies instead of reading it.
    #[tokio::test]
    async fn an_unauthorized_verdict_survives_binding() {
        let home = tempfile::tempdir().expect("temp home");
        let (_runtime, lease) = registered_project_store(home.path(), "admitted").await;
        let admitted = scope("admitted");

        let bound = McpToolContext::bind(McpToolBinding {
            project_session_store: Some(AdmittedProjectStore::new(
                &lease,
                ValidatedAuthorization::Unauthorized,
            )),
            ..binding(home.path(), Some(&admitted))
        })
        .expect("an unauthorized store is still a coherent binding");

        let (_, authorization) = bound
            .authorized_project_session_db()
            .expect("the store is reported with its verdict");
        assert_eq!(authorization, ValidatedAuthorization::Unauthorized);
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
}
