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
    pub(crate) project_registered: Option<&'a crate::global_db::RegisteredGlobalDb>,
    pub(crate) profile_registered: Option<&'a crate::global_db::RegisteredGlobalDb>,
    project_refresh: Option<&'a dyn session::SessionRefreshServicePort>,
    profile_refresh: Option<&'a dyn session::SessionRefreshServicePort>,
    pub(super) project_retrieval: Option<&'a dyn session::message_search::SessionRetrievalServicePort>,
    pub(super) profile_retrieval: Option<&'a dyn session::message_search::SessionRetrievalServicePort>,
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
        }
    }

    pub(crate) const fn with_registered_databases(
        mut self,
        project: Option<&'a crate::global_db::RegisteredGlobalDb>,
        profile: Option<&'a crate::global_db::RegisteredGlobalDb>,
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

    pub(super) const fn refresh_services(self) -> session::SessionRefreshServices<'a> {
        session::SessionRefreshServices::new(self.project_refresh, self.profile_refresh)
    }
}
