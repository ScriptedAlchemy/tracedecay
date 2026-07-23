//! Cohesive construction dependencies for [`McpServer`](super::McpServer):
//! the construction context, daemon-provided database/authority bundles, and
//! the injectable writer boundaries they carry.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use crate::global_db::GlobalDb;
use crate::tracedecay::TraceDecay;

use super::hook_writes::{
    BackgroundRefreshWriter, HookBranchWriter, direct_background_refresh_writer,
    direct_hook_branch_writer,
};

/// Updates daemon ownership routing after this server changes physical graph DB.
/// Implementations must not call back into this `McpServer`: reconciliation is
/// awaited while the graph write guard is held so readers see the swap and
/// registry rekey atomically.
pub(crate) type DatabaseOwnerReconciler = Arc<
    dyn Fn(Arc<TraceDecay>) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync + 'static,
>;

/// Cohesive dependencies used to construct an MCP server.
///
/// Keeping these values together makes explicit that all of them describe one
/// server instance, rather than independent configuration parameters.
pub(crate) struct McpServerConstructionContext {
    pub(crate) cg: TraceDecay,
    pub(crate) scope_prefix: Option<String>,
    pub(crate) profile_root: Option<PathBuf>,
    pub(crate) global_db: Option<Arc<GlobalDb>>,
    pub(crate) registry_db: Option<Arc<GlobalDb>>,
    pub(crate) session_db: Option<Arc<GlobalDb>>,
    pub(crate) user_session_db: Option<Arc<GlobalDb>>,
    pub(crate) host_admission_broker:
        Option<crate::application::host_admission::SharedHostAdmissionBroker>,
    pub(crate) project_session_refresh_wake:
        Option<crate::daemon::session_temporal_refresh_scheduler::SessionTemporalRefreshWake>,
    pub(crate) user_session_refresh_wake:
        Option<crate::daemon::session_temporal_refresh_scheduler::SessionTemporalRefreshWake>,
    /// When true (daemon-owned project servers), spawn a cancellable worker that
    /// continues bounded host-admission replay passes until idle.
    pub(crate) own_project_host_admission_replay: bool,
    pub(crate) allow_default_registry_fallback: bool,
    pub(crate) automation_scheduler_reconciler:
        Option<crate::dashboard::AutomationSchedulerReconciler>,
    pub(crate) database_owner_reconciler: Option<DatabaseOwnerReconciler>,
    pub(crate) dashboard_automation_writer: crate::dashboard::DashboardAutomationWriter,
    pub(crate) hook_branch_writer: HookBranchWriter,
    pub(crate) background_refresh_writer: BackgroundRefreshWriter,
    pub(crate) code_index_hook_sink: Option<super::CodeIndexHookSink>,
}

pub(crate) struct McpServerWriters {
    dashboard_automation: crate::dashboard::DashboardAutomationWriter,
    hook_branch: HookBranchWriter,
    background_refresh: BackgroundRefreshWriter,
}

pub(crate) struct McpServerDaemonDatabases {
    pub(crate) accounting: Option<Arc<GlobalDb>>,
    pub(crate) registry: Arc<GlobalDb>,
    pub(crate) project_sessions: Arc<GlobalDb>,
    pub(crate) user_sessions: Arc<GlobalDb>,
}

pub(crate) struct McpServerDaemonAuthority {
    pub(crate) profile_root: PathBuf,
    pub(crate) databases: McpServerDaemonDatabases,
    pub(crate) host_admission_broker:
        Option<crate::application::host_admission::SharedHostAdmissionBroker>,
    pub(crate) project_session_refresh_wake:
        crate::daemon::session_temporal_refresh_scheduler::SessionTemporalRefreshWake,
    pub(crate) user_session_refresh_wake:
        crate::daemon::session_temporal_refresh_scheduler::SessionTemporalRefreshWake,
    pub(crate) database_owner_reconciler: DatabaseOwnerReconciler,
    pub(crate) writers: McpServerWriters,
}

impl McpServerWriters {
    pub(crate) fn daemon_owned(
        dashboard_automation: crate::dashboard::DashboardAutomationWriter,
        hook_branch: HookBranchWriter,
        background_refresh: BackgroundRefreshWriter,
    ) -> Self {
        Self {
            dashboard_automation,
            hook_branch,
            background_refresh,
        }
    }
}

impl McpServerConstructionContext {
    pub(crate) fn direct(cg: TraceDecay, scope_prefix: Option<String>) -> Self {
        Self {
            cg,
            scope_prefix,
            profile_root: None,
            global_db: None,
            registry_db: None,
            session_db: None,
            user_session_db: None,
            host_admission_broker: None,
            project_session_refresh_wake: None,
            user_session_refresh_wake: None,
            own_project_host_admission_replay: false,
            allow_default_registry_fallback: true,
            automation_scheduler_reconciler: None,
            database_owner_reconciler: None,
            dashboard_automation_writer: crate::dashboard::direct_dashboard_automation_writer(),
            hook_branch_writer: direct_hook_branch_writer(),
            background_refresh_writer: direct_background_refresh_writer(),
            code_index_hook_sink: None,
        }
    }

    pub(crate) fn with_direct_databases(
        mut self,
        global_db: Option<Arc<GlobalDb>>,
        registry_db: Option<Arc<GlobalDb>>,
        session_db: Option<Arc<GlobalDb>>,
        user_session_db: Option<Arc<GlobalDb>>,
        allow_default_registry_fallback: bool,
    ) -> Self {
        self.global_db = global_db;
        self.registry_db = registry_db;
        self.session_db = session_db;
        self.user_session_db = user_session_db;
        self.allow_default_registry_fallback = allow_default_registry_fallback;
        self
    }

    pub(crate) fn daemon_owned(
        cg: TraceDecay,
        scope_prefix: Option<String>,
        authority: McpServerDaemonAuthority,
    ) -> Self {
        let McpServerDaemonAuthority {
            profile_root,
            databases,
            host_admission_broker,
            project_session_refresh_wake,
            user_session_refresh_wake,
            database_owner_reconciler,
            writers,
        } = authority;
        Self {
            cg,
            scope_prefix,
            profile_root: Some(profile_root),
            global_db: databases.accounting,
            registry_db: Some(databases.registry),
            session_db: Some(databases.project_sessions),
            user_session_db: Some(databases.user_sessions),
            host_admission_broker,
            project_session_refresh_wake: Some(project_session_refresh_wake),
            user_session_refresh_wake: Some(user_session_refresh_wake),
            own_project_host_admission_replay: true,
            allow_default_registry_fallback: false,
            automation_scheduler_reconciler: None,
            database_owner_reconciler: Some(database_owner_reconciler),
            dashboard_automation_writer: writers.dashboard_automation,
            hook_branch_writer: writers.hook_branch,
            background_refresh_writer: writers.background_refresh,
            code_index_hook_sink: None,
        }
    }

    /// Inject the daemon-owned code-index scheduler bridge so after-edit hooks
    /// deliver touched paths into the incremental indexing queue.
    pub(crate) fn with_code_index_hook_sink(mut self, sink: super::CodeIndexHookSink) -> Self {
        self.code_index_hook_sink = Some(sink);
        self
    }

    #[cfg(unix)]
    pub(crate) fn with_automation_scheduler_reconciler(
        mut self,
        reconciler: crate::dashboard::AutomationSchedulerReconciler,
    ) -> Self {
        self.automation_scheduler_reconciler = Some(reconciler);
        self
    }

    #[cfg(test)]
    pub(crate) fn with_owned_project_host_admission_replay(mut self) -> Self {
        self.own_project_host_admission_replay = true;
        self
    }

    #[cfg(test)]
    pub(crate) fn with_database_owner_reconciler(
        mut self,
        reconciler: DatabaseOwnerReconciler,
    ) -> Self {
        self.database_owner_reconciler = Some(reconciler);
        self
    }

    #[cfg(test)]
    pub(crate) fn with_hook_branch_writer(mut self, writer: HookBranchWriter) -> Self {
        self.hook_branch_writer = writer;
        self
    }

    #[cfg(test)]
    pub(crate) fn with_background_refresh_writer(
        mut self,
        writer: BackgroundRefreshWriter,
    ) -> Self {
        self.background_refresh_writer = writer;
        self
    }
}
