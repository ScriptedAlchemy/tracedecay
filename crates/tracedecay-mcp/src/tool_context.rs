//! The single production context a moved MCP handler family reads.
//!
//! The composition root resolves project admission, the carried request
//! deadline, cancellation, and every code-index authority *before* handler
//! dispatch, then hands the whole admitted set across this one boundary.
//! Handlers never reach back into daemon internals and never reconstruct an
//! authority for themselves: an authority the daemon did not admit stays a
//! typed `None` here, and each handler turns that absence into its own
//! unavailable state rather than a locally minted substitute.

use std::path::Path;

use tracedecay_contracts::{CancellationSignal, Deadline};
use tracedecay_global_db::RegisteredGlobalDbLeaseV1;
use tracedecay_query::code_search::{
    CodeIndexBranchDiffExecutor, CodeIndexSearchAuthorityV1, CodeIndexSearchExecutor,
};

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
    project_session_db: Option<&'a RegisteredGlobalDbLeaseV1>,
    code_index_search_executor: Option<&'a CodeIndexSearchExecutor>,
    code_index_branch_diff_executor: Option<&'a CodeIndexBranchDiffExecutor>,
    code_index_search_authority: Option<&'a CodeIndexSearchAuthorityV1>,
}

impl<'a> McpToolContext<'a> {
    /// Binds the admitted worktree root every handler resolves paths against.
    ///
    /// Every other authority is absent until the root adds it, so a
    /// standalone (non-daemon) server produces a context that reports typed
    /// unavailability instead of a half-built daemon surface.
    pub fn new(project_root: &'a Path) -> Self {
        Self {
            project_root,
            active_branch: None,
            deadline: None,
            cancellation: None,
            project_session_db: None,
            code_index_search_executor: None,
            code_index_branch_diff_executor: None,
            code_index_search_authority: None,
        }
    }

    /// The branch git resolved for the admitted worktree, when it has one.
    #[must_use]
    pub fn with_active_branch(mut self, active_branch: Option<&'a str>) -> Self {
        self.active_branch = active_branch;
        self
    }

    /// The caller's carried deadline and cancellation. Handlers propagate
    /// both into bounded walks so a cancelled call stops at its next
    /// checkpoint instead of running to completion.
    #[must_use]
    pub fn with_request_control(
        mut self,
        deadline: Option<&'a Deadline>,
        cancellation: Option<&'a CancellationSignal>,
    ) -> Self {
        self.deadline = deadline;
        self.cancellation = cancellation;
        self
    }

    /// The registered project session store this call may read. Selector
    /// isolation is settled by the root before dispatch: the lease already
    /// names the selected project's store, so a handler cannot widen scope.
    #[must_use]
    pub fn with_project_session_db(
        mut self,
        project_session_db: Option<&'a RegisteredGlobalDbLeaseV1>,
    ) -> Self {
        self.project_session_db = project_session_db;
        self
    }

    /// The daemon-owned code-index search and branch-diff executors plus the
    /// authorization the daemon proved for them. The authority travels with
    /// the executors because neither is usable without the other.
    #[must_use]
    pub fn with_code_index_authorities(
        mut self,
        search_executor: Option<&'a CodeIndexSearchExecutor>,
        branch_diff_executor: Option<&'a CodeIndexBranchDiffExecutor>,
        authority: Option<&'a CodeIndexSearchAuthorityV1>,
    ) -> Self {
        self.code_index_search_executor = search_executor;
        self.code_index_branch_diff_executor = branch_diff_executor;
        self.code_index_search_authority = authority;
        self
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

    #[must_use]
    pub fn project_session_db(&self) -> Option<&'a RegisteredGlobalDbLeaseV1> {
        self.project_session_db
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
}
