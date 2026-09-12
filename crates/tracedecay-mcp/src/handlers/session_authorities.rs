use tracedecay_contracts::ProfileIdentityReadPort;
use tracedecay_global_db::RegisteredGlobalDbLeaseV1;
use tracedecay_runtime_core::background_cpu::ProcessBackgroundCpuV1;

/// Database authorities retained by the owning MCP server for its lifetime.
/// Hook and LCM handlers borrow these capabilities; they never rediscover or
/// reopen a session database while dispatching an action.
///
/// `profile_retained_authority` stays a daemon lease because
/// `retained_catalog` still calls `execute_profile_retained_application`
/// with `DaemonSessionRuntimeRegistryV1`.
#[derive(Clone, Default)]
pub struct SessionAuthorities<'a> {
    /// Registered project session store; ingestion, retrieval, and project
    /// host admission are all derived from this one lease.
    pub project: Option<&'a RegisteredGlobalDbLeaseV1>,
    /// Registered profile (user-scope) session store.
    pub user: Option<&'a RegisteredGlobalDbLeaseV1>,
    pub profile_identity: Option<std::sync::Arc<dyn ProfileIdentityReadPort>>,
    /// The process background CPU authority host observation capture prepares
    /// under; absent on direct servers, where capture fails closed.
    pub background_cpu: Option<std::sync::Arc<ProcessBackgroundCpuV1>>,
    pub profile_retained_authority:
        Option<&'a tracedecay_session_runtime::retained::ProfileRetainedConnectionAuthorityV1>,
    pub project_lcm:
        Option<&'a dyn tracedecay_session_runtime::lcm_authority::MountedLcmAuthorityPort>,
    pub profile_lcm:
        Option<&'a dyn tracedecay_session_runtime::lcm_authority::MountedLcmAuthorityPort>,
    /// Daemon-wide profile session refresh service serving profile-scoped
    /// `tracedecay_session_refresh_*` calls on this connection.
    pub profile_session_refresh:
        Option<&'a dyn tracedecay_session_runtime::retained::RetainedSessionRefreshPortV1>,
}

impl<'a> SessionAuthorities<'a> {
    #[hotpath::skip]
    pub const fn new(
        project: Option<&'a RegisteredGlobalDbLeaseV1>,
        user: Option<&'a RegisteredGlobalDbLeaseV1>,
    ) -> Self {
        Self {
            project,
            user,
            profile_identity: None,
            background_cpu: None,
            profile_retained_authority: None,
            project_lcm: None,
            profile_lcm: None,
            profile_session_refresh: None,
        }
    }

    #[must_use]
    pub fn with_profile_identity(
        mut self,
        profile_identity: Option<std::sync::Arc<dyn ProfileIdentityReadPort>>,
    ) -> Self {
        self.profile_identity = profile_identity;
        self
    }

    #[must_use]
    pub fn with_background_cpu(
        mut self,
        background_cpu: Option<std::sync::Arc<ProcessBackgroundCpuV1>>,
    ) -> Self {
        self.background_cpu = background_cpu;
        self
    }

    #[must_use]
    #[hotpath::skip]
    pub const fn with_profile_retained_authority(
        mut self,
        authority: Option<
            &'a tracedecay_session_runtime::retained::ProfileRetainedConnectionAuthorityV1,
        >,
    ) -> Self {
        self.profile_retained_authority = authority;
        self
    }

    #[must_use]
    #[hotpath::skip]
    pub const fn with_lcm_authorities(
        mut self,
        project: Option<&'a dyn tracedecay_session_runtime::lcm_authority::MountedLcmAuthorityPort>,
        profile: Option<&'a dyn tracedecay_session_runtime::lcm_authority::MountedLcmAuthorityPort>,
    ) -> Self {
        self.project_lcm = project;
        self.profile_lcm = profile;
        self
    }

    #[must_use]
    #[hotpath::skip]
    pub const fn with_profile_session_refresh(
        mut self,
        refresh: Option<&'a dyn tracedecay_session_runtime::retained::RetainedSessionRefreshPortV1>,
    ) -> Self {
        self.profile_session_refresh = refresh;
        self
    }
}
