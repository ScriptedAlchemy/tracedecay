use std::sync::Arc;

use super::session;
use crate::global_db::RegisteredGlobalDb;

/// Database authorities retained by the owning MCP server for its lifetime.
/// Hook and LCM handlers borrow these capabilities; they never rediscover or
/// reopen a session database while dispatching an action.
#[derive(Clone, Copy, Default)]
pub struct SessionAuthorities<'a> {
    pub(crate) project: Option<&'a Arc<RegisteredGlobalDb>>,
    pub(crate) user: Option<&'a Arc<RegisteredGlobalDb>>,
    pub(crate) profile_identity:
        Option<&'a crate::daemon::profile_identity::LocalProfileIdentityAuthorityV1>,
    pub(crate) project_registered: Option<&'a Arc<RegisteredGlobalDb>>,
    pub(crate) profile_registered: Option<&'a Arc<RegisteredGlobalDb>>,
    project_refresh: Option<&'a dyn session::SessionRefreshServicePort>,
    profile_refresh: Option<&'a dyn session::SessionRefreshServicePort>,
    pub(super) project_retrieval:
        Option<&'a dyn session::message_search::SessionRetrievalServicePort>,
    pub(super) profile_retrieval:
        Option<&'a dyn session::message_search::SessionRetrievalServicePort>,
    pub(super) project_retrieval_sweep:
        Option<&'a dyn session::message_search::SessionRetrievalSweepPort>,
    pub(crate) project_lcm: Option<&'a dyn crate::daemon::lcm_authority::MountedLcmAuthorityPort>,
    pub(crate) profile_lcm: Option<&'a dyn crate::daemon::lcm_authority::MountedLcmAuthorityPort>,
}

impl<'a> SessionAuthorities<'a> {
    pub(crate) const fn new(
        project: Option<&'a Arc<RegisteredGlobalDb>>,
        user: Option<&'a Arc<RegisteredGlobalDb>>,
    ) -> Self {
        Self {
            project,
            user,
            profile_identity: None,
            project_registered: None,
            profile_registered: None,
            project_refresh: None,
            profile_refresh: None,
            project_retrieval: None,
            profile_retrieval: None,
            project_retrieval_sweep: None,
            project_lcm: None,
            profile_lcm: None,
        }
    }

    pub(crate) const fn with_registered_databases(
        mut self,
        project: Option<&'a Arc<RegisteredGlobalDb>>,
        profile: Option<&'a Arc<RegisteredGlobalDb>>,
    ) -> Self {
        self.project_registered = project;
        self.profile_registered = profile;
        self
    }

    pub(crate) const fn with_profile_identity(
        mut self,
        profile_identity: Option<
            &'a crate::daemon::profile_identity::LocalProfileIdentityAuthorityV1,
        >,
    ) -> Self {
        self.profile_identity = profile_identity;
        self
    }

    pub(crate) const fn with_refresh_services(
        mut self,
        project: Option<&'a dyn session::SessionRefreshServicePort>,
        profile: Option<&'a dyn session::SessionRefreshServicePort>,
    ) -> Self {
        self.project_refresh = project;
        self.profile_refresh = profile;
        self
    }

    pub(crate) const fn with_retrieval_services(
        mut self,
        project: Option<&'a dyn session::message_search::SessionRetrievalServicePort>,
        profile: Option<&'a dyn session::message_search::SessionRetrievalServicePort>,
    ) -> Self {
        self.project_retrieval = project;
        self.profile_retrieval = profile;
        self
    }

    pub(crate) const fn with_retrieval_sweep(
        mut self,
        sweep: Option<&'a dyn session::message_search::SessionRetrievalSweepPort>,
    ) -> Self {
        self.project_retrieval_sweep = sweep;
        self
    }

    pub(crate) const fn with_lcm_authorities(
        mut self,
        project: Option<&'a dyn crate::daemon::lcm_authority::MountedLcmAuthorityPort>,
        profile: Option<&'a dyn crate::daemon::lcm_authority::MountedLcmAuthorityPort>,
    ) -> Self {
        self.project_lcm = project;
        self.profile_lcm = profile;
        self
    }

    pub(super) const fn refresh_services(self) -> session::SessionRefreshServices<'a> {
        session::SessionRefreshServices::new(self.project_refresh, self.profile_refresh)
    }
}
