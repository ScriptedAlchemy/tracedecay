//! Daemon-owned admission for project-scoped PR9 MCP reads.
//!
//! Direct MCP servers never construct this grant. The daemon derives the
//! principal from its authenticated durable profile identity and binds the
//! authorization revision to the registered project owner and exact resolved
//! repository/worktree/ref scope.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use thiserror::Error;
use tracedecay_application::ResolvedScope;
use tracedecay_domain::{
    AuthorizationRevision, BrainId, CapabilityId, PrincipalId, ProjectId, UserProfileId, UtcMicros,
    canonical_sha256,
};

use super::profile_identity::LocalProfileIdentityAuthorityV1;

pub(crate) const PR9_MCP_READ_CAPABILITY_V1: &str = "capability.application.code-index.search-read";
const PR9_MCP_GRANT_HORIZON: Duration = Duration::from_hours(24);
const AUTHORIZATION_REVISION_DOMAIN_V1: &str = "tracedecay.daemon.pr9-mcp-read-authorization.v1";
const PRINCIPAL_DOMAIN_V1: &str = "tracedecay.daemon.pr9-mcp-profile-principal.v1";

#[derive(Clone)]
pub(crate) struct Pr9McpReadAdmissionV1 {
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
pub(crate) struct Pr9McpReadAdmissionProviderV1 {
    identity: LocalProfileIdentityAuthorityV1,
    project_id: ProjectId,
    route_registered: Arc<AtomicBool>,
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub(crate) enum Pr9McpAdmissionUnavailableV1 {
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

impl Pr9McpAdmissionUnavailableV1 {
    pub(crate) const fn reason(self) -> &'static str {
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

pub(crate) fn admit_pr9_mcp_read(
    identity: Option<&LocalProfileIdentityAuthorityV1>,
    project_id: &ProjectId,
    scope: &ResolvedScope,
    route_registered: Arc<AtomicBool>,
) -> Result<Pr9McpReadAdmissionV1, Pr9McpAdmissionUnavailableV1> {
    let identity = identity.ok_or(Pr9McpAdmissionUnavailableV1::Unauthenticated)?;
    admit_pr9_mcp_read_at(
        identity.brain_id(),
        identity.profile_id(),
        project_id,
        scope,
        now_micros(),
        route_registered,
    )
}

impl Pr9McpReadAdmissionProviderV1 {
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
    ) -> Result<Pr9McpReadAdmissionV1, Pr9McpAdmissionUnavailableV1> {
        admit_pr9_mcp_read(
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

fn admit_pr9_mcp_read_at(
    brain_id: &BrainId,
    profile_id: &UserProfileId,
    project_id: &ProjectId,
    scope: &ResolvedScope,
    issued_at: UtcMicros,
    route_registered: Arc<AtomicBool>,
) -> Result<Pr9McpReadAdmissionV1, Pr9McpAdmissionUnavailableV1> {
    scope
        .validate()
        .map_err(|_| Pr9McpAdmissionUnavailableV1::InvalidGrant)?;
    if scope.project_id != *project_id || issued_at.0 <= 0 {
        return Err(Pr9McpAdmissionUnavailableV1::InvalidGrant);
    }
    let expires_at = UtcMicros(
        issued_at
            .0
            .saturating_add(i64::try_from(PR9_MCP_GRANT_HORIZON.as_micros()).unwrap_or(i64::MAX)),
    );
    if expires_at <= issued_at {
        return Err(Pr9McpAdmissionUnavailableV1::InvalidGrant);
    }
    let capabilities = std::iter::once(PR9_MCP_READ_CAPABILITY_V1)
        .map(CapabilityId::new)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| Pr9McpAdmissionUnavailableV1::InvalidGrant)?;
    let principal_digest = canonical_sha256(&(PRINCIPAL_DOMAIN_V1, brain_id, profile_id))
        .map_err(|_| Pr9McpAdmissionUnavailableV1::InvalidGrant)?;
    let principal = PrincipalId::new(format!(
        "principal.pr9-mcp.{}",
        principal_digest.as_str().trim_start_matches("sha256:")
    ))
    .map_err(|_| Pr9McpAdmissionUnavailableV1::InvalidGrant)?;
    let revision_digest = canonical_sha256(&(
        AUTHORIZATION_REVISION_DOMAIN_V1,
        brain_id,
        profile_id,
        project_id,
        &scope.scope_digest,
        &capabilities,
    ))
    .map_err(|_| Pr9McpAdmissionUnavailableV1::InvalidGrant)?;
    let authorization_revision = AuthorizationRevision::new(format!(
        "authorization.pr9-mcp.{}",
        revision_digest.as_str().trim_start_matches("sha256:")
    ))
    .map_err(|_| Pr9McpAdmissionUnavailableV1::InvalidGrant)?;
    Ok(Pr9McpReadAdmissionV1 {
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

impl Pr9McpReadAdmissionV1 {
    pub(crate) fn search_authority(&self) -> crate::mcp::server::CodeIndexSearchAuthorityV1 {
        crate::mcp::server::CodeIndexSearchAuthorityV1 {
            principal: self.principal.clone(),
            authorization_revision: self.authorization_revision.clone(),
        }
    }

    pub(crate) fn authorize(
        &self,
        scope: &ResolvedScope,
        supplied: Option<&crate::mcp::server::CodeIndexSearchAuthorityV1>,
    ) -> Result<crate::mcp::server::CodeIndexSearchAuthorityV1, Pr9McpAdmissionUnavailableV1> {
        self.authorize_at(scope, supplied, PR9_MCP_READ_CAPABILITY_V1, now_micros())
    }

    fn authorize_at(
        &self,
        scope: &ResolvedScope,
        supplied: Option<&crate::mcp::server::CodeIndexSearchAuthorityV1>,
        requested_capability: &str,
        observed_at: UtcMicros,
    ) -> Result<crate::mcp::server::CodeIndexSearchAuthorityV1, Pr9McpAdmissionUnavailableV1> {
        if !self.route_registered.load(Ordering::Acquire) {
            return Err(Pr9McpAdmissionUnavailableV1::Revoked);
        }
        if observed_at < self.issued_at || observed_at >= self.expires_at {
            return Err(Pr9McpAdmissionUnavailableV1::Expired);
        }
        if !self
            .capabilities
            .iter()
            .any(|capability| capability.as_str() == requested_capability)
        {
            return Err(Pr9McpAdmissionUnavailableV1::CapabilityMismatch);
        }
        if scope.project_id != self.project_id
            || scope.scope_digest != self.scope.scope_digest
            || scope != &self.scope
        {
            return Err(Pr9McpAdmissionUnavailableV1::ScopeMismatch);
        }
        let supplied = supplied.ok_or(Pr9McpAdmissionUnavailableV1::Unauthenticated)?;
        let expected = self.search_authority();
        if supplied != &expected {
            return Err(Pr9McpAdmissionUnavailableV1::AuthorizationStale);
        }
        Ok(expected)
    }

    #[allow(dead_code)]
    pub(crate) fn revoke(&self) {
        self.route_registered.store(false, Ordering::Release);
    }
}

fn now_micros() -> UtcMicros {
    UtcMicros(
        i64::try_from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |duration| duration.as_micros()),
        )
        .unwrap_or(i64::MAX),
    )
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    use tracedecay_application::ResolvedScope;
    use tracedecay_domain::{
        AuthorizationRevision, BrainId, ProjectId, RefId, RepositoryId, UserProfileId, UtcMicros,
        WorktreeId,
    };

    use super::{Pr9McpAdmissionUnavailableV1, admit_pr9_mcp_read_at};

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

    fn admission(scope: &ResolvedScope) -> super::Pr9McpReadAdmissionV1 {
        admit_pr9_mcp_read_at(
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
                super::PR9_MCP_READ_CAPABILITY_V1,
                UtcMicros(11),
            ),
            Ok(authority.clone())
        );
        assert_eq!(
            admission.authorize_at(
                &other_project,
                Some(&authority),
                super::PR9_MCP_READ_CAPABILITY_V1,
                UtcMicros(11),
            ),
            Err(Pr9McpAdmissionUnavailableV1::ScopeMismatch)
        );
        assert_eq!(
            admission.authorize_at(
                &other_worktree,
                Some(&authority),
                super::PR9_MCP_READ_CAPABILITY_V1,
                UtcMicros(11),
            ),
            Err(Pr9McpAdmissionUnavailableV1::ScopeMismatch)
        );
    }

    #[test]
    fn stale_expired_and_revoked_grants_fail_closed() {
        let scope = scope("project.one", "worktree.one");
        let admission = admission(&scope);
        let authority = admission.search_authority();
        let stale = crate::mcp::server::CodeIndexSearchAuthorityV1 {
            principal: authority.principal.clone(),
            authorization_revision: id::<AuthorizationRevision>("authorization.stale"),
        };

        assert_eq!(
            admission.authorize_at(
                &scope,
                Some(&stale),
                super::PR9_MCP_READ_CAPABILITY_V1,
                UtcMicros(11),
            ),
            Err(Pr9McpAdmissionUnavailableV1::AuthorizationStale)
        );
        assert_eq!(
            admission.authorize_at(
                &scope,
                Some(&authority),
                super::PR9_MCP_READ_CAPABILITY_V1,
                admission.expires_at,
            ),
            Err(Pr9McpAdmissionUnavailableV1::Expired)
        );
        admission.revoke();
        assert_eq!(
            admission.authorize_at(
                &scope,
                Some(&authority),
                super::PR9_MCP_READ_CAPABILITY_V1,
                UtcMicros(11),
            ),
            Err(Pr9McpAdmissionUnavailableV1::Revoked)
        );
    }

    #[test]
    fn refreshed_grants_keep_revision_for_same_scope_and_change_it_for_new_scope() {
        let active_scope = scope("project.one", "worktree.one");
        let refreshed = admit_pr9_mcp_read_at(
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
        let moved = admit_pr9_mcp_read_at(
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
            super::admit_pr9_mcp_read(
                None,
                &scope.project_id,
                &scope,
                Arc::new(AtomicBool::new(true)),
            ),
            Err(Pr9McpAdmissionUnavailableV1::Unauthenticated)
        ));
    }
}
