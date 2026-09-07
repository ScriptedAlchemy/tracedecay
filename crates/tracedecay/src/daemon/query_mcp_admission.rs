//! Daemon-owned admission for project-scoped query MCP reads.
//!
//! Direct MCP servers never construct this grant. The daemon derives the
//! principal from its authenticated durable profile identity and binds the
//! authorization revision to the registered project owner and exact resolved
//! repository/worktree/ref scope.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use thiserror::Error;
use tracedecay_application::{ResolvedScope, now_micros};
use tracedecay_domain::{
    AuthorizationRevision, BrainId, CapabilityId, PrincipalId, ProjectId, UserProfileId, UtcMicros,
    canonical_sha256,
};
use tracedecay_query::code_search;

use tracedecay_code_index_runtime::mcp_admission::{
    CodeIndexMcpAdmissionUnavailableV1, CodeIndexMcpReadAdmissionV1, CodeIndexMcpReadGrantV1,
};
use tracedecay_daemon_identity::profile_identity::LocalProfileIdentityAuthorityV1;

pub(crate) const QUERY_MCP_READ_CAPABILITY_V1: &str =
    "capability.application.code-index.search-read";
const QUERY_MCP_GRANT_HORIZON: Duration = Duration::from_hours(24);
const AUTHORIZATION_REVISION_DOMAIN_V1: &str = "tracedecay.query-read-authorization.v1";
const PRINCIPAL_DOMAIN_V1: &str = "tracedecay.query-profile-principal.v1";

#[derive(Clone)]
pub(crate) struct QueryMcpReadAdmissionV1 {
    project_id: ProjectId,
    scope: ResolvedScope,
    principal: PrincipalId,
    authorization_revision: AuthorizationRevision,
    capabilities: Vec<CapabilityId>,
    issued_at: UtcMicros,
    expires_at: UtcMicros,
    route_registered: Arc<AtomicBool>,
}

#[derive(Clone)]
pub(crate) struct QueryMcpReadAdmissionProviderV1 {
    identity: LocalProfileIdentityAuthorityV1,
    project_id: ProjectId,
    route_registered: Arc<AtomicBool>,
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub(crate) enum QueryMcpAdmissionUnavailableV1 {
    #[error("the MCP route has no daemon-authenticated profile actor")]
    Unauthenticated,
    #[error("the MCP read grant is invalid")]
    InvalidGrant,
    #[error("the MCP read grant does not cover the requested capability")]
    CapabilityMismatch,
    #[error("the MCP read grant does not cover the requested project scope")]
    ScopeMismatch,
    #[error("the MCP read grant authorization revision is stale")]
    AuthorizationStale,
    #[error("the MCP read grant expired")]
    Expired,
    #[error("the MCP read grant was revoked")]
    Revoked,
}

/// Tallies one admission refusal against its exact policy reason. The reason
/// set is the closed [`QueryMcpAdmissionUnavailableV1`] enum, so every gauge
/// key stays compile-time static.
fn record_query_admission_refusal(reason: QueryMcpAdmissionUnavailableV1) {
    match reason {
        QueryMcpAdmissionUnavailableV1::Unauthenticated => {
            hotpath::gauge!("daemon.query_admission.refused.unauthenticated").inc(1.0);
        }
        QueryMcpAdmissionUnavailableV1::InvalidGrant => {
            hotpath::gauge!("daemon.query_admission.refused.invalid_grant").inc(1.0);
        }
        QueryMcpAdmissionUnavailableV1::CapabilityMismatch => {
            hotpath::gauge!("daemon.query_admission.refused.capability_mismatch").inc(1.0);
        }
        QueryMcpAdmissionUnavailableV1::ScopeMismatch => {
            hotpath::gauge!("daemon.query_admission.refused.scope_mismatch").inc(1.0);
        }
        QueryMcpAdmissionUnavailableV1::AuthorizationStale => {
            hotpath::gauge!("daemon.query_admission.refused.authorization_stale").inc(1.0);
        }
        QueryMcpAdmissionUnavailableV1::Expired => {
            hotpath::gauge!("daemon.query_admission.refused.expired").inc(1.0);
        }
        QueryMcpAdmissionUnavailableV1::Revoked => {
            hotpath::gauge!("daemon.query_admission.refused.revoked").inc(1.0);
        }
    }
}

pub(crate) fn admit_query_mcp_read(
    identity: Option<&LocalProfileIdentityAuthorityV1>,
    project_id: &ProjectId,
    scope: &ResolvedScope,
    route_registered: Arc<AtomicBool>,
) -> Result<QueryMcpReadAdmissionV1, QueryMcpAdmissionUnavailableV1> {
    let admission = match identity {
        Some(identity) => admit_query_mcp_read_at(
            identity.brain_id(),
            identity.profile_id(),
            project_id,
            scope,
            now_micros(),
            route_registered,
        ),
        None => Err(QueryMcpAdmissionUnavailableV1::Unauthenticated),
    };
    match &admission {
        Ok(_) => {
            hotpath::gauge!("daemon.query_admission.admitted").inc(1.0);
        }
        Err(reason) => record_query_admission_refusal(*reason),
    }
    admission
}

impl QueryMcpReadAdmissionProviderV1 {
    pub(crate) fn new(
        identity: LocalProfileIdentityAuthorityV1,
        project_id: ProjectId,
        route_registered: Arc<AtomicBool>,
    ) -> Self {
        Self {
            identity,
            project_id,
            route_registered,
        }
    }

    pub(crate) fn admit_current(
        &self,
        scope: &ResolvedScope,
    ) -> Result<QueryMcpReadAdmissionV1, QueryMcpAdmissionUnavailableV1> {
        admit_query_mcp_read(
            Some(&self.identity),
            &self.project_id,
            scope,
            Arc::clone(&self.route_registered),
        )
    }

    pub(crate) fn route_is_registered(&self) -> bool {
        self.route_registered.load(Ordering::Acquire)
    }
}

#[hotpath::measure(label = "daemon.query_mcp.admit")]
fn admit_query_mcp_read_at(
    brain_id: &BrainId,
    profile_id: &UserProfileId,
    project_id: &ProjectId,
    scope: &ResolvedScope,
    issued_at: UtcMicros,
    route_registered: Arc<AtomicBool>,
) -> Result<QueryMcpReadAdmissionV1, QueryMcpAdmissionUnavailableV1> {
    scope
        .validate()
        .map_err(|_| QueryMcpAdmissionUnavailableV1::InvalidGrant)?;
    if scope.project_id != *project_id || issued_at.0 <= 0 {
        return Err(QueryMcpAdmissionUnavailableV1::InvalidGrant);
    }
    let expires_at =
        UtcMicros(issued_at.0.saturating_add(
            i64::try_from(QUERY_MCP_GRANT_HORIZON.as_micros()).unwrap_or(i64::MAX),
        ));
    if expires_at <= issued_at {
        return Err(QueryMcpAdmissionUnavailableV1::InvalidGrant);
    }
    let capabilities = std::iter::once(QUERY_MCP_READ_CAPABILITY_V1)
        .map(CapabilityId::new)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| QueryMcpAdmissionUnavailableV1::InvalidGrant)?;
    let principal_digest = canonical_sha256(&(PRINCIPAL_DOMAIN_V1, brain_id, profile_id))
        .map_err(|_| QueryMcpAdmissionUnavailableV1::InvalidGrant)?;
    let principal = PrincipalId::new(format!(
        "principal.query-mcp.{}",
        principal_digest.as_str().trim_start_matches("sha256:")
    ))
    .map_err(|_| QueryMcpAdmissionUnavailableV1::InvalidGrant)?;
    let revision_digest = canonical_sha256(&(
        AUTHORIZATION_REVISION_DOMAIN_V1,
        brain_id,
        profile_id,
        project_id,
        &scope.scope_digest,
        &capabilities,
    ))
    .map_err(|_| QueryMcpAdmissionUnavailableV1::InvalidGrant)?;
    let authorization_revision = AuthorizationRevision::new(format!(
        "authorization.query-mcp.{}",
        revision_digest.as_str().trim_start_matches("sha256:")
    ))
    .map_err(|_| QueryMcpAdmissionUnavailableV1::InvalidGrant)?;
    Ok(QueryMcpReadAdmissionV1 {
        project_id: project_id.clone(),
        scope: scope.clone(),
        principal,
        authorization_revision,
        capabilities,
        issued_at,
        expires_at,
        route_registered,
    })
}

impl From<QueryMcpAdmissionUnavailableV1> for CodeIndexMcpAdmissionUnavailableV1 {
    fn from(value: QueryMcpAdmissionUnavailableV1) -> Self {
        match value {
            QueryMcpAdmissionUnavailableV1::Unauthenticated => Self::Unauthenticated,
            QueryMcpAdmissionUnavailableV1::InvalidGrant => Self::InvalidGrant,
            QueryMcpAdmissionUnavailableV1::CapabilityMismatch => Self::CapabilityMismatch,
            QueryMcpAdmissionUnavailableV1::ScopeMismatch => Self::ScopeMismatch,
            QueryMcpAdmissionUnavailableV1::AuthorizationStale => Self::AuthorizationStale,
            QueryMcpAdmissionUnavailableV1::Expired => Self::Expired,
            QueryMcpAdmissionUnavailableV1::Revoked => Self::Revoked,
        }
    }
}

impl CodeIndexMcpReadGrantV1 for QueryMcpReadAdmissionV1 {
    fn authorize(
        &self,
        scope: &ResolvedScope,
        authority: Option<&code_search::CodeIndexSearchAuthorityV1>,
    ) -> Result<code_search::CodeIndexSearchAuthorityV1, CodeIndexMcpAdmissionUnavailableV1> {
        QueryMcpReadAdmissionV1::authorize(self, scope, authority).map_err(Into::into)
    }

    fn search_authority(&self) -> code_search::CodeIndexSearchAuthorityV1 {
        QueryMcpReadAdmissionV1::search_authority(self)
    }
}

impl CodeIndexMcpReadAdmissionV1 for QueryMcpReadAdmissionProviderV1 {
    type Grant = QueryMcpReadAdmissionV1;

    fn route_is_registered(&self) -> bool {
        QueryMcpReadAdmissionProviderV1::route_is_registered(self)
    }

    fn admit_current(
        &self,
        scope: &ResolvedScope,
    ) -> Result<Self::Grant, CodeIndexMcpAdmissionUnavailableV1> {
        QueryMcpReadAdmissionProviderV1::admit_current(self, scope).map_err(Into::into)
    }
}

impl QueryMcpReadAdmissionV1 {
    pub(crate) fn search_authority(&self) -> code_search::CodeIndexSearchAuthorityV1 {
        code_search::CodeIndexSearchAuthorityV1 {
            principal: self.principal.clone(),
            authorization_revision: self.authorization_revision.clone(),
        }
    }

    pub(crate) fn authorize(
        &self,
        scope: &ResolvedScope,
        supplied: Option<&code_search::CodeIndexSearchAuthorityV1>,
    ) -> Result<code_search::CodeIndexSearchAuthorityV1, QueryMcpAdmissionUnavailableV1> {
        let authorized =
            self.authorize_at(scope, supplied, QUERY_MCP_READ_CAPABILITY_V1, now_micros());
        match &authorized {
            Ok(_) => {
                hotpath::gauge!("daemon.query_admission.authorized").inc(1.0);
            }
            Err(reason) => record_query_admission_refusal(*reason),
        }
        authorized
    }

    #[hotpath::measure(label = "daemon.query_mcp.authorize")]
    fn authorize_at(
        &self,
        scope: &ResolvedScope,
        supplied: Option<&code_search::CodeIndexSearchAuthorityV1>,
        requested_capability: &str,
        observed_at: UtcMicros,
    ) -> Result<code_search::CodeIndexSearchAuthorityV1, QueryMcpAdmissionUnavailableV1> {
        if !self.route_registered.load(Ordering::Acquire) {
            return Err(QueryMcpAdmissionUnavailableV1::Revoked);
        }
        if observed_at < self.issued_at || observed_at >= self.expires_at {
            return Err(QueryMcpAdmissionUnavailableV1::Expired);
        }
        if !self
            .capabilities
            .iter()
            .any(|capability| capability.as_str() == requested_capability)
        {
            return Err(QueryMcpAdmissionUnavailableV1::CapabilityMismatch);
        }
        if scope.project_id != self.project_id
            || scope.scope_digest != self.scope.scope_digest
            || scope != &self.scope
        {
            return Err(QueryMcpAdmissionUnavailableV1::ScopeMismatch);
        }
        let supplied = supplied.ok_or(QueryMcpAdmissionUnavailableV1::Unauthenticated)?;
        let expected = self.search_authority();
        if supplied != &expected {
            return Err(QueryMcpAdmissionUnavailableV1::AuthorizationStale);
        }
        Ok(expected)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use tracedecay_application::ResolvedScope;
    use tracedecay_domain::{
        AuthorizationRevision, BrainId, ProjectId, RefId, RepositoryId, UserProfileId, UtcMicros,
        WorktreeId,
    };
    use tracedecay_query::code_search;

    use super::{QueryMcpAdmissionUnavailableV1, admit_query_mcp_read_at};

    fn id<T>(value: &str) -> T
    where
        T: TryFrom<String>,
        T::Error: std::fmt::Debug,
    {
        T::try_from(value.to_owned()).expect("typed fixture id")
    }

    fn scope(project: &str, worktree: &str) -> ResolvedScope {
        ResolvedScope::new(
            id::<ProjectId>(project),
            id::<RepositoryId>("repository.fixture"),
            id::<WorktreeId>(worktree),
            Some(id::<RefId>("refs/heads/main")),
        )
        .expect("scope")
    }

    fn admission(scope: &ResolvedScope) -> super::QueryMcpReadAdmissionV1 {
        admit_query_mcp_read_at(
            &id::<BrainId>("brain.fixture"),
            &id::<UserProfileId>("profile.fixture"),
            &scope.project_id,
            scope,
            UtcMicros(10),
            Arc::new(AtomicBool::new(true)),
        )
        .expect("admission")
    }

    #[test]
    fn route_admission_is_exactly_project_and_worktree_scoped() {
        let admitted_scope = scope("project.one", "worktree.one");
        let other_project = scope("project.two", "worktree.one");
        let other_worktree = scope("project.one", "worktree.two");
        let admission = admission(&admitted_scope);
        let authority = admission.search_authority();

        assert_eq!(
            admission.authorize_at(
                &admitted_scope,
                Some(&authority),
                super::QUERY_MCP_READ_CAPABILITY_V1,
                UtcMicros(11),
            ),
            Ok(authority.clone())
        );
        assert_eq!(
            admission.authorize_at(
                &other_project,
                Some(&authority),
                super::QUERY_MCP_READ_CAPABILITY_V1,
                UtcMicros(11),
            ),
            Err(QueryMcpAdmissionUnavailableV1::ScopeMismatch)
        );
        assert_eq!(
            admission.authorize_at(
                &other_worktree,
                Some(&authority),
                super::QUERY_MCP_READ_CAPABILITY_V1,
                UtcMicros(11),
            ),
            Err(QueryMcpAdmissionUnavailableV1::ScopeMismatch)
        );
    }

    #[test]
    fn stale_expired_and_revoked_grants_fail_closed() {
        let scope = scope("project.one", "worktree.one");
        let route_registered = Arc::new(AtomicBool::new(true));
        let admission = admit_query_mcp_read_at(
            &id::<BrainId>("brain.fixture"),
            &id::<UserProfileId>("profile.fixture"),
            &scope.project_id,
            &scope,
            UtcMicros(10),
            Arc::clone(&route_registered),
        )
        .expect("admission");
        let authority = admission.search_authority();
        let stale = code_search::CodeIndexSearchAuthorityV1 {
            principal: authority.principal.clone(),
            authorization_revision: id::<AuthorizationRevision>("authorization.stale"),
        };

        assert_eq!(
            admission.authorize_at(
                &scope,
                Some(&stale),
                super::QUERY_MCP_READ_CAPABILITY_V1,
                UtcMicros(11),
            ),
            Err(QueryMcpAdmissionUnavailableV1::AuthorizationStale)
        );
        assert_eq!(
            admission.authorize_at(
                &scope,
                Some(&authority),
                super::QUERY_MCP_READ_CAPABILITY_V1,
                admission.expires_at,
            ),
            Err(QueryMcpAdmissionUnavailableV1::Expired)
        );
        route_registered.store(false, Ordering::Release);
        assert_eq!(
            admission.authorize_at(
                &scope,
                Some(&authority),
                super::QUERY_MCP_READ_CAPABILITY_V1,
                UtcMicros(11),
            ),
            Err(QueryMcpAdmissionUnavailableV1::Revoked)
        );
    }

    #[test]
    fn refreshed_grants_keep_revision_for_same_scope_and_change_it_for_new_scope() {
        let active_scope = scope("project.one", "worktree.one");
        let refreshed = admit_query_mcp_read_at(
            &id::<BrainId>("brain.fixture"),
            &id::<UserProfileId>("profile.fixture"),
            &active_scope.project_id,
            &active_scope,
            UtcMicros(20),
            Arc::new(AtomicBool::new(true)),
        )
        .expect("refreshed admission");
        let original = admission(&active_scope);
        assert_eq!(original.search_authority(), refreshed.search_authority());
        assert_ne!(original.issued_at, refreshed.issued_at);
        assert_ne!(original.expires_at, refreshed.expires_at);

        let other_scope = scope("project.one", "worktree.two");
        let moved = admit_query_mcp_read_at(
            &id::<BrainId>("brain.fixture"),
            &id::<UserProfileId>("profile.fixture"),
            &other_scope.project_id,
            &other_scope,
            UtcMicros(20),
            Arc::new(AtomicBool::new(true)),
        )
        .expect("moved admission");
        assert_ne!(original.search_authority(), moved.search_authority());
    }

    #[test]
    fn absent_authenticated_actor_is_unavailable() {
        let scope = scope("project.one", "worktree.one");
        assert!(matches!(
            super::admit_query_mcp_read(
                None,
                &scope.project_id,
                &scope,
                Arc::new(AtomicBool::new(true)),
            ),
            Err(QueryMcpAdmissionUnavailableV1::Unauthenticated)
        ));
    }
}
