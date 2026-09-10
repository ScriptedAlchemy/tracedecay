//! Admission and scope-resolution ports for MCP search / branch-diff executors.
//!
//! The concrete daemon grant types stay in the composition root. This crate
//! names only the methods the executors call.

use std::path::Path;

use tracedecay_contracts::ResolvedScope;
use tracedecay_domain::ProjectId;
use tracedecay_query::code_search;

/// Typed refusal for scope resolution. The composition root deliberately
/// narrows its internal contract errors to this: the executors map every
/// resolution failure onto the search-unavailable vocabulary uniformly.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CodeIndexScopeUnavailableV1;

/// Scope resolver the search/diff executors call for each request root.
pub trait CodeIndexScopeResolverV1: Clone + Send + Sync + 'static {
    fn resolved_scope_for_project(
        &self,
        project_root: &Path,
        project_id: &ProjectId,
    ) -> Result<ResolvedScope, CodeIndexScopeUnavailableV1>;
}

/// Revalidates checkout identity while preserving the scope that minted the route grant.
/// A branch label may move under one admitted repository/worktree without changing that identity.
#[derive(Clone, Debug)]
pub struct RegisteredProjectScopeResolverV1 {
    admitted: ResolvedScope,
}

impl RegisteredProjectScopeResolverV1 {
    pub fn new(admitted: ResolvedScope) -> Self {
        Self { admitted }
    }
}

impl CodeIndexScopeResolverV1 for RegisteredProjectScopeResolverV1 {
    fn resolved_scope_for_project(
        &self,
        project_root: &Path,
        project_id: &ProjectId,
    ) -> Result<ResolvedScope, CodeIndexScopeUnavailableV1> {
        let current = crate::resolved_scope_for_project(project_root, project_id)
            .map_err(|_| CodeIndexScopeUnavailableV1)?;
        if current.project_id != self.admitted.project_id
            || current.repository_id != self.admitted.repository_id
            || current.worktree_id != self.admitted.worktree_id
        {
            return Err(CodeIndexScopeUnavailableV1);
        }
        Ok(self.admitted.clone())
    }
}

/// Closed admission refusal vocabulary the executors map onto search outcomes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CodeIndexMcpAdmissionUnavailableV1 {
    Unauthenticated,
    InvalidGrant,
    CapabilityMismatch,
    ScopeMismatch,
    AuthorizationStale,
    Expired,
    Revoked,
}

impl CodeIndexMcpAdmissionUnavailableV1 {
    #[hotpath::skip]
    pub const fn reason(self) -> &'static str {
        match self {
            Self::Unauthenticated => "mcp_route_unauthenticated",
            Self::InvalidGrant => "mcp_read_grant_invalid",
            Self::CapabilityMismatch => "mcp_read_capability_mismatch",
            Self::ScopeMismatch => "mcp_read_scope_mismatch",
            Self::AuthorizationStale => "mcp_read_authorization_stale",
            Self::Expired => "mcp_read_grant_expired",
            Self::Revoked => "mcp_read_grant_revoked",
        }
    }
}

/// Issued MCP read grant used after `admit_current`.
pub trait CodeIndexMcpReadGrantV1: Clone + Send + Sync {
    fn authorize(
        &self,
        scope: &ResolvedScope,
        authority: Option<&code_search::CodeIndexSearchAuthorityV1>,
    ) -> Result<code_search::CodeIndexSearchAuthorityV1, CodeIndexMcpAdmissionUnavailableV1>;

    fn search_authority(&self) -> code_search::CodeIndexSearchAuthorityV1;
}

/// Route-scoped MCP read admission the executors clone into each request.
pub trait CodeIndexMcpReadAdmissionV1: Clone + Send + Sync + 'static {
    type Grant: CodeIndexMcpReadGrantV1;

    fn route_is_registered(&self) -> bool;

    fn admit_current(
        &self,
        scope: &ResolvedScope,
    ) -> Result<Self::Grant, CodeIndexMcpAdmissionUnavailableV1>;
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::process::Command;

    use super::*;

    fn git(root: &Path, arguments: &[&str]) {
        let status =
            Command::new(tracedecay_runtime_core::git::try_git_program().expect("Git executable"))
                .args(arguments)
                .current_dir(root)
                .status()
                .expect("run Git fixture command");
        assert!(status.success(), "Git failed: {arguments:?}");
    }

    fn repository() -> tempfile::TempDir {
        let root = tempfile::TempDir::new().expect("repository");
        git(root.path(), &["init", "-q", "-b", "main"]);
        git(root.path(), &["config", "user.name", "TraceDecay Test"]);
        git(
            root.path(),
            &["config", "user.email", "tracedecay@example.invalid"],
        );
        std::fs::write(root.path().join("lib.rs"), "pub fn value() {}\n").expect("source");
        git(root.path(), &["add", "lib.rs"]);
        git(root.path(), &["commit", "-qm", "seed"]);
        root
    }

    #[test]
    fn admitted_scope_survives_branch_switch_but_rejects_another_checkout() {
        let project = repository();
        let project_id = ProjectId::new("project.scope-resolver").expect("project id");
        let admitted =
            crate::resolved_scope_for_project(project.path(), &project_id).expect("main scope");
        let resolver = RegisteredProjectScopeResolverV1::new(admitted.clone());

        git(project.path(), &["switch", "-qc", "feature"]);
        let resolved = resolver
            .resolved_scope_for_project(project.path(), &project_id)
            .expect("same checkout after branch switch");
        assert_eq!(resolved, admitted);

        let foreign = repository();
        assert!(
            resolver
                .resolved_scope_for_project(foreign.path(), &project_id)
                .is_err()
        );
    }
}
