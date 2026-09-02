//! Admission and scope-resolution ports for MCP search / branch-diff executors.
//!
//! The concrete daemon grant types stay in the composition root. This crate
//! names only the methods the executors call.

use std::path::Path;

use tracedecay_application::ResolvedScope;
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
