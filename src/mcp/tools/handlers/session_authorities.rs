use crate::global_db::RegisteredGlobalDbLeaseV1;

/// Database authorities retained by the owning MCP server for its lifetime.
/// Hook and LCM handlers borrow these capabilities; they never rediscover or
/// reopen a session database while dispatching an action.
#[derive(Clone, Copy, Default)]
pub struct SessionAuthorities<'a> {
    pub(crate) project: Option<&'a RegisteredGlobalDbLeaseV1>,
    pub(crate) user: Option<&'a RegisteredGlobalDbLeaseV1>,
    pub(crate) profile_identity:
        Option<&'a crate::daemon::profile_identity::LocalProfileIdentityAuthorityV1>,
    pub(crate) profile_retained_authority:
        Option<&'a crate::daemon::retained_owner::ProfileRetainedConnectionAuthorityV1>,
    pub(crate) project_registered: Option<&'a RegisteredGlobalDbLeaseV1>,
    pub(crate) profile_registered: Option<&'a RegisteredGlobalDbLeaseV1>,
    pub(crate) project_lcm: Option<&'a dyn crate::daemon::lcm_authority::MountedLcmAuthorityPort>,
    pub(crate) profile_lcm: Option<&'a dyn crate::daemon::lcm_authority::MountedLcmAuthorityPort>,
}

impl<'a> SessionAuthorities<'a> {
    pub(crate) const fn new(
        project: Option<&'a RegisteredGlobalDbLeaseV1>,
        user: Option<&'a RegisteredGlobalDbLeaseV1>,
    ) -> Self {
        Self {
            project,
            user,
            profile_identity: None,
            profile_retained_authority: None,
            project_registered: None,
            profile_registered: None,
            project_lcm: None,
            profile_lcm: None,
        }
    }

    pub(crate) const fn with_registered_databases(
        mut self,
        project: Option<&'a RegisteredGlobalDbLeaseV1>,
        profile: Option<&'a RegisteredGlobalDbLeaseV1>,
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

    pub(crate) const fn with_profile_retained_authority(
        mut self,
        authority: Option<&'a crate::daemon::retained_owner::ProfileRetainedConnectionAuthorityV1>,
    ) -> Self {
        self.profile_retained_authority = authority;
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
}
