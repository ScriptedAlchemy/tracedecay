//! The single validated admitted binding a moved MCP handler family reads.
//!
//! The composition root resolves project admission, the carried request
//! deadline, cancellation, and every code-index authority *before* handler
//! dispatch, then hands the whole admitted set across this one boundary.
//!
//! Construction is one validated step, not a builder chain: an authority
//! arrives already paired with the checkout the daemon admitted it for, and
//! [`McpToolContext::bind`] refuses a binding whose parts name different
//! checkouts. That refusal is the selector-isolation gate for the moved
//! families — a store lease opened for one project can never be presented
//! alongside another project's resolved scope, because the pair travels as one
//! value and every named scope is cross-checked before any handler runs.
//!
//! Absence stays typed. An authority the daemon never admitted is `None` here
//! and each handler turns that into its own unavailable state; the context
//! neither mints a substitute nor upgrades a missing authorization into an
//! apparent capability.

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
    #[error("admitted {authority} scope is not self-consistent")]
    ScopeInvalid { authority: &'static str },
    #[error(
        "project session store is admitted for checkout {store} but this request resolved {request}"
    )]
    ProjectStoreScopeMismatch { request: String, store: String },
    #[error(
        "code index authorities are admitted for checkout {code_index} but this request resolved {request}"
    )]
    CodeIndexScopeMismatch { request: String, code_index: String },
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
            Self::ProjectStoreScopeMismatch { .. } => "mcp_tool_binding_store_scope_mismatch",
            Self::CodeIndexScopeMismatch { .. } => "mcp_tool_binding_code_index_scope_mismatch",
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

/// The registered project session store, paired with the checkout the daemon
/// opened it for.
///
/// The pair is the isolation proof: a handler receives the lease only together
/// with the scope it belongs to, so it cannot read a store the daemon opened
/// for a different project.
#[derive(Clone, Copy)]
pub struct AdmittedProjectStore<'a> {
    /// The checkout this store was opened for, when a project route had
    /// resolved one at admission time.
    pub scope: Option<&'a ResolvedScope>,
    pub lease: &'a RegisteredGlobalDbLeaseV1,
}

/// Daemon-owned code-index executors with the authorization proved for them.
///
/// The authority travels with the executors because neither is usable without
/// the other, and grouping them means a root cannot wire an executor while
/// dropping its authorization on the floor.
#[derive(Clone, Copy)]
pub struct AdmittedCodeIndex<'a> {
    /// The checkout these executors were admitted for, when a project route
    /// had resolved one at admission time.
    pub scope: Option<&'a ResolvedScope>,
    /// The authorization the daemon proved. Absent stays absent: the executor
    /// itself denies an unauthorized request, and reporting that as a missing
    /// *capability* instead would hide a real authorization failure.
    pub authority: Option<&'a CodeIndexSearchAuthorityV1>,
    pub search: Option<&'a CodeIndexSearchExecutor>,
    pub branch_diff: Option<&'a CodeIndexBranchDiffExecutor>,
}

/// Everything the composition root admits for one MCP tool call.
#[derive(Clone, Copy)]
pub struct McpToolBinding<'a> {
    /// The admitted worktree root every handler resolves paths against.
    pub project_root: &'a Path,
    /// The branch git resolved for that worktree, when it has one.
    pub active_branch: Option<&'a str>,
    pub controls: RequestControls<'a>,
    /// The scope the daemon's project route resolved for this request. Absent
    /// on a standalone server and on the core server that answers before
    /// project-open publication mounts a route.
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
    /// The one checkout every named authority in this binding agreed on.
    admitted_scope: Option<&'a ResolvedScope>,
    project_session_db: Option<&'a RegisteredGlobalDbLeaseV1>,
    code_index_search_executor: Option<&'a CodeIndexSearchExecutor>,
    code_index_branch_diff_executor: Option<&'a CodeIndexBranchDiffExecutor>,
    code_index_search_authority: Option<&'a CodeIndexSearchAuthorityV1>,
}

impl<'a> McpToolContext<'a> {
    /// Validates one admitted binding and freezes it for the call.
    ///
    /// Every scope the binding names must be self-consistent and must identify
    /// the same checkout, and a code-index admission must carry an executor.
    /// Nothing is defaulted or repaired: a binding that does not prove one
    /// coherent request scope is refused whole.
    pub fn bind(binding: McpToolBinding<'a>) -> std::result::Result<Self, McpToolBindingError> {
        if !binding.project_root.is_absolute() {
            return Err(McpToolBindingError::RelativeProjectRoot {
                root: binding.project_root.display().to_string(),
            });
        }
        let store_scope = binding
            .project_session_store
            .as_ref()
            .and_then(|store| store.scope);
        let code_index_scope = binding
            .code_index
            .as_ref()
            .and_then(|code_index| code_index.scope);
        let admitted_scope =
            verify_admitted_checkouts(binding.scope, store_scope, code_index_scope)?;
        if let Some(code_index) = &binding.code_index
            && code_index.search.is_none()
            && code_index.branch_diff.is_none()
        {
            return Err(McpToolBindingError::CodeIndexWithoutExecutor);
        }

        Ok(Self {
            project_root: binding.project_root,
            active_branch: binding.active_branch,
            deadline: binding.controls.deadline,
            cancellation: binding.controls.cancellation,
            admitted_scope,
            project_session_db: binding
                .project_session_store
                .as_ref()
                .map(|store| store.lease),
            code_index_search_executor: binding
                .code_index
                .as_ref()
                .and_then(|code_index| code_index.search),
            code_index_branch_diff_executor: binding
                .code_index
                .as_ref()
                .and_then(|code_index| code_index.branch_diff),
            code_index_search_authority: binding
                .code_index
                .as_ref()
                .and_then(|code_index| code_index.authority),
        })
    }

    /// A binding for a server with no daemon admission at all.
    ///
    /// A standalone MCP server knows the worktree it was started in and
    /// nothing else, so every daemon authority is absent and each handler
    /// reports its own unavailable state.
    pub fn standalone(project_root: &'a Path) -> std::result::Result<Self, McpToolBindingError> {
        Self::bind(McpToolBinding {
            project_root,
            active_branch: None,
            controls: RequestControls::default(),
            scope: None,
            project_session_store: None,
            code_index: None,
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

    /// The one checkout this call is admitted for, when any authority named it.
    #[must_use]
    pub fn admitted_scope(&self) -> Option<&'a ResolvedScope> {
        self.admitted_scope
    }

    #[must_use]
    pub fn project_session_db(&self) -> Option<&'a RegisteredGlobalDbLeaseV1> {
        self.project_session_db
    }

    /// The admitted project store together with the authorization binding it
    /// proved.
    ///
    /// A handler must not decide for itself that a store read is authorized.
    /// [`Self::bind`] already proved this lease belongs to the checkout this
    /// call is admitted for, and that proof is what this pair reports; with no
    /// admitted store there is nothing to authorize and the caller receives
    /// the typed absence instead.
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
    /// which must name the checkout this call was admitted for.
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

/// Refuses a graph admitted for a different checkout than this call.
///
/// With no admitted scope there is no selected project to isolate from: the
/// binding named no store and no scoped executor, so the graph's own admission
/// is the only identity in play and it answers for itself.
fn verify_scope_isolation(admitted: Option<&ResolvedScope>, graph: &ResolvedScope) -> Result<()> {
    let Some(admitted) = admitted else {
        return Ok(());
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

/// Proves every checkout a binding names is the same one, and returns it.
///
/// The request scope leads when the daemon's route resolved one; otherwise the
/// first authority that names a checkout carries the only identity this call
/// has, and the rest must agree with it. A binding that names two checkouts is
/// a cross-project leak, not a preference to reconcile.
fn verify_admitted_checkouts<'a>(
    request: Option<&'a ResolvedScope>,
    store: Option<&'a ResolvedScope>,
    code_index: Option<&'a ResolvedScope>,
) -> std::result::Result<Option<&'a ResolvedScope>, McpToolBindingError> {
    validate_scope(request, "request")?;
    validate_scope(store, "project session store")?;
    validate_scope(code_index, "code index")?;

    let admitted = request.or(store).or(code_index);
    if let (Some(admitted), Some(store)) = (admitted, store)
        && !admitted.identifies_same_checkout(store)
    {
        return Err(McpToolBindingError::ProjectStoreScopeMismatch {
            request: checkout_label(admitted),
            store: checkout_label(store),
        });
    }
    if let (Some(admitted), Some(code_index)) = (admitted, code_index)
        && !admitted.identifies_same_checkout(code_index)
    {
        return Err(McpToolBindingError::CodeIndexScopeMismatch {
            request: checkout_label(admitted),
            code_index: checkout_label(code_index),
        });
    }
    Ok(admitted)
}

fn validate_scope(
    scope: Option<&ResolvedScope>,
    authority: &'static str,
) -> std::result::Result<(), McpToolBindingError> {
    match scope {
        Some(scope) if scope.validate().is_err() => {
            Err(McpToolBindingError::ScopeInvalid { authority })
        }
        _ => Ok(()),
    }
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

    /// A relative root cannot anchor path resolution or identity, so it is
    /// refused instead of silently joined against the process directory.
    #[test]
    fn a_relative_project_root_is_refused() {
        let error = McpToolContext::standalone(Path::new("relative/root"))
            .expect_err("relative root must be refused");
        assert_eq!(error.reason_code(), "mcp_tool_binding_root_not_absolute");
    }

    /// A store lease opened for one project must never be readable through a
    /// context admitted for another: that is the cross-project selector leak
    /// the binding exists to prevent.
    #[test]
    fn a_store_from_another_project_is_refused() {
        let request = scope("admitted");
        let foreign = scope("foreign");

        let error = verify_admitted_checkouts(Some(&request), Some(&foreign), None)
            .expect_err("a foreign store scope must be refused");

        assert_eq!(error.reason_code(), "mcp_tool_binding_store_scope_mismatch");
        let message = error.to_string();
        assert!(message.contains("project.foreign"), "got {message:?}");
        assert!(message.contains("project.admitted"), "got {message:?}");
    }

    /// The same store presented with the scope it was opened for binds, so the
    /// mismatch refusal is proving identity rather than rejecting every store.
    #[test]
    fn a_store_from_the_admitted_project_binds() {
        let request = scope("admitted");

        let admitted = verify_admitted_checkouts(Some(&request), Some(&request), None)
            .expect("a store admitted for this request must bind");

        assert_eq!(
            admitted.map(|scope| scope.project_id.as_str()),
            Some("project.admitted")
        );
    }

    /// Code-index executors admitted for another checkout are refused for the
    /// same reason a foreign store is: they would answer from a different
    /// project's sealed generations.
    #[test]
    fn code_index_executors_from_another_project_are_refused() {
        let request = scope("admitted");
        let foreign = scope("foreign");

        let error = verify_admitted_checkouts(Some(&request), None, Some(&foreign))
            .expect_err("foreign code index scope must be refused");

        assert_eq!(
            error.reason_code(),
            "mcp_tool_binding_code_index_scope_mismatch"
        );
    }

    /// Before a route resolves, the store lease carries the only checkout
    /// identity this call has, and a code-index authority admitted for another
    /// project must still be refused against it.
    #[test]
    fn an_unrouted_call_isolates_against_the_store_checkout() {
        let store = scope("admitted");
        let foreign = scope("foreign");

        let admitted = verify_admitted_checkouts(None, Some(&store), Some(&store))
            .expect("one agreed checkout must bind without a route");
        assert_eq!(
            admitted.map(|scope| scope.project_id.as_str()),
            Some("project.admitted")
        );

        let error = verify_admitted_checkouts(None, Some(&store), Some(&foreign))
            .expect_err("a foreign code index must be refused against the store checkout");
        assert_eq!(
            error.reason_code(),
            "mcp_tool_binding_code_index_scope_mismatch"
        );
    }

    /// A code-index admission with no executor is an empty capability claim:
    /// handlers would report the index as mounted while nothing can answer.
    #[test]
    fn a_code_index_admission_without_an_executor_is_refused() {
        let temp = tempfile::tempdir().expect("temp root");
        let request = scope("admitted");

        let error = McpToolContext::bind(McpToolBinding {
            code_index: Some(AdmittedCodeIndex {
                scope: Some(&request),
                authority: None,
                search: None,
                branch_diff: None,
            }),
            ..binding(temp.path(), Some(&request))
        })
        .expect_err("an executorless code index admission must be refused");

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
}
