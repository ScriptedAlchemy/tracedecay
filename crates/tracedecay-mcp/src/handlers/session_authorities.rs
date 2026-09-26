use tracedecay_contracts::ProfileIdentityReadPort;
use tracedecay_global_db::RegisteredGlobalDbLeaseV1;
#[cfg(any(test, feature = "test-helpers"))]
use tracedecay_project::test_support::host_admission::HostAdmissionTestRuntimeV1;
use tracedecay_runtime_core::background_cpu::ProcessBackgroundCpuV1;
#[cfg(any(test, feature = "test-helpers"))]
use tracedecay_sessions::admission::HostAdmissionScope;

/// Database authorities retained by the owning MCP server for its lifetime.
/// Hook and LCM handlers borrow these capabilities; they never rediscover or
/// reopen a session database while dispatching an action.
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
    pub project_lcm:
        Option<&'a dyn tracedecay_session_runtime::lcm_authority::MountedLcmAuthorityPort>,
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
            project_lcm: None,
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
    pub const fn with_project_lcm_authority(
        mut self,
        project: Option<&'a dyn tracedecay_session_runtime::lcm_authority::MountedLcmAuthorityPort>,
    ) -> Self {
        self.project_lcm = project;
        self
    }
}

/// The session authorities an MCP server retains for a registered test
/// runtime's project and profile session stores.
#[cfg(any(test, feature = "test-helpers"))]
pub fn mcp_session_authorities(runtime: &HostAdmissionTestRuntimeV1) -> SessionAuthorities<'_> {
    SessionAuthorities::new(
        runtime.registered_database_lease(HostAdmissionScope::Project),
        runtime.registered_database_lease(HostAdmissionScope::Profile),
    )
}
