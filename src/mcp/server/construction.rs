//! Cohesive construction dependencies for [`McpServer`](super::McpServer):
//! the construction context, daemon-provided database/authority bundles, and
//! the injectable writer boundaries they carry.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use crate::global_db::RegisteredGlobalDb;
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
pub(crate) struct McpServerConstructionContext {
    pub(crate) cg: Arc<TraceDecay>,
    pub(crate) scope_prefix: Option<String>,
    pub(crate) profile_root: Option<PathBuf>,
    pub(crate) profile_identity:
        Option<crate::daemon::profile_identity::LocalProfileIdentityAuthorityV1>,
    pub(crate) transcript_source_home: Option<PathBuf>,
    pub(crate) global_db: Option<Arc<RegisteredGlobalDb>>,
    pub(crate) accounting_db: Option<Arc<RegisteredGlobalDb>>,
    pub(crate) registry_db: Option<Arc<RegisteredGlobalDb>>,
    pub(crate) session_db: Option<Arc<RegisteredGlobalDb>>,
    pub(crate) user_session_db: Option<Arc<RegisteredGlobalDb>>,
    pub(crate) registered_session_db: Option<Arc<RegisteredGlobalDb>>,
    pub(crate) registered_user_session_db: Option<Arc<RegisteredGlobalDb>>,
    pub(crate) host_admission_broker:
        Option<crate::application::host_admission::SharedHostAdmissionBroker>,
    pub(crate) project_session_refresh_wake:
        Option<crate::daemon::session_temporal_refresh_scheduler::SessionTemporalRefreshWake>,
    pub(crate) user_session_refresh_wake:
        Option<crate::daemon::session_temporal_refresh_scheduler::SessionTemporalRefreshWake>,
    /// When true (daemon-owned project servers), spawn a cancellable worker that
    /// continues bounded host-admission replay passes until idle.
    pub(crate) own_project_host_admission_replay: bool,
    pub(crate) startup_catch_up_enabled: bool,
    pub(crate) automation_scheduler_reconciler:
        Option<crate::dashboard::AutomationSchedulerReconciler>,
    pub(crate) database_owner_reconciler: Option<DatabaseOwnerReconciler>,
    pub(crate) dashboard_automation_writer: crate::dashboard::DashboardAutomationWriter,
    pub(crate) dashboard_doctor_report_reader: Option<crate::dashboard::DoctorReportReader>,
    pub(crate) dashboard_doctor_remediation_dispatcher:
        Option<crate::dashboard::DoctorRemediationDispatcherV1>,
    pub(crate) dashboard_code_index_freshness_reader:
        Option<crate::dashboard::code_index_freshness_api::CodeIndexFreshnessReader>,
    pub(crate) dashboard_feedback_status_reader:
        Option<crate::dashboard::feedback_api::FeedbackStatusReader>,
    pub(crate) diagnostics_lsp:
        Option<Arc<tokio::sync::Mutex<tracedecay_lsp::analyzer::broker::DiagnosticBroker>>>,
    pub(crate) hook_branch_writer: HookBranchWriter,
    pub(crate) background_refresh_writer: BackgroundRefreshWriter,
    pub(crate) code_index_hook_sink: Option<super::CodeIndexHookSink>,
    pub(crate) code_index_publication_identity: Option<super::CodeIndexPublicationIdentityResolver>,
    pub(crate) code_index_search_executor: Option<super::CodeIndexSearchExecutor>,
    pub(crate) code_index_search_authority: Option<super::CodeIndexSearchAuthorityV1>,
    pub(crate) retained_project_graph_resolver: Option<super::RetainedProjectGraphResolver>,
    pub(crate) project_routes: crate::mcp::project_route::SharedHookProjectRouteCache,
    pub(crate) application_invocation_executor:
        Option<Arc<dyn crate::daemon_client::DaemonInvocationExecutor>>,
    pub(crate) project_server_live: Option<Arc<AtomicBool>>,
    #[cfg(any(test, feature = "test-transport"))]
    pub(crate) host_admission_test_runtime:
        Option<Arc<crate::application::host_admission::HostAdmissionTestRuntimeV1>>,
}

pub(crate) struct McpServerWriters {
    dashboard_automation: crate::dashboard::DashboardAutomationWriter,
    hook_branch: HookBranchWriter,
    background_refresh: BackgroundRefreshWriter,
}

pub(crate) struct McpServerDaemonDatabases {
    pub(crate) accounting: Option<Arc<RegisteredGlobalDb>>,
    pub(crate) registry: Arc<RegisteredGlobalDb>,
    pub(crate) project_sessions: Arc<RegisteredGlobalDb>,
    pub(crate) user_sessions: Arc<RegisteredGlobalDb>,
    pub(crate) registered_project_sessions: Arc<crate::global_db::RegisteredGlobalDb>,
    pub(crate) registered_user_sessions: Arc<crate::global_db::RegisteredGlobalDb>,
}

pub(crate) struct McpServerDaemonAuthority {
    pub(crate) profile_identity: crate::daemon::profile_identity::LocalProfileIdentityAuthorityV1,
    pub(crate) transcript_source_home: Option<PathBuf>,
    pub(crate) databases: McpServerDaemonDatabases,
    pub(crate) host_admission_broker:
        Option<crate::application::host_admission::SharedHostAdmissionBroker>,
    pub(crate) project_session_refresh_wake:
        crate::daemon::session_temporal_refresh_scheduler::SessionTemporalRefreshWake,
    pub(crate) user_session_refresh_wake:
        crate::daemon::session_temporal_refresh_scheduler::SessionTemporalRefreshWake,
    pub(crate) database_owner_reconciler: DatabaseOwnerReconciler,
    pub(crate) project_routes: crate::mcp::project_route::SharedHookProjectRouteCache,
    pub(crate) writers: McpServerWriters,
}

pub(crate) struct McpServerDaemonCoreAuthority {
    pub(crate) profile_identity: crate::daemon::profile_identity::LocalProfileIdentityAuthorityV1,
    pub(crate) transcript_source_home: Option<PathBuf>,
    pub(crate) accounting: Option<Arc<RegisteredGlobalDb>>,
    pub(crate) registry: Arc<RegisteredGlobalDb>,
    pub(crate) database_owner_reconciler: DatabaseOwnerReconciler,
    pub(crate) project_routes: crate::mcp::project_route::SharedHookProjectRouteCache,
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
    pub(crate) fn direct(cg: impl Into<Arc<TraceDecay>>, scope_prefix: Option<String>) -> Self {
        Self {
            cg: cg.into(),
            scope_prefix,
            profile_root: None,
            profile_identity: None,
            transcript_source_home: None,
            global_db: None,
            accounting_db: None,
            registry_db: None,
            session_db: None,
            user_session_db: None,
            registered_session_db: None,
            registered_user_session_db: None,
            host_admission_broker: None,
            project_session_refresh_wake: None,
            user_session_refresh_wake: None,
            own_project_host_admission_replay: false,
            startup_catch_up_enabled: true,
            automation_scheduler_reconciler: None,
            database_owner_reconciler: None,
            dashboard_automation_writer: crate::dashboard::standalone_dashboard_automation_writer(),
            dashboard_doctor_report_reader: None,
            dashboard_doctor_remediation_dispatcher: None,
            dashboard_code_index_freshness_reader: None,
            dashboard_feedback_status_reader: None,
            diagnostics_lsp: None,
            hook_branch_writer: direct_hook_branch_writer(),
            background_refresh_writer: direct_background_refresh_writer(),
            code_index_hook_sink: None,
            code_index_publication_identity: None,
            code_index_search_executor: None,
            code_index_search_authority: None,
            retained_project_graph_resolver: None,
            project_routes: crate::mcp::project_route::SharedHookProjectRouteCache::default(),
            application_invocation_executor: None,
            project_server_live: None,
            #[cfg(any(test, feature = "test-transport"))]
            host_admission_test_runtime: None,
        }
    }

    #[cfg(any(test, feature = "test-transport"))]
    pub(crate) fn with_direct_databases(
        mut self,
        global_db: Option<Arc<RegisteredGlobalDb>>,
        registry_db: Option<Arc<RegisteredGlobalDb>>,
        session_db: Option<Arc<RegisteredGlobalDb>>,
        user_session_db: Option<Arc<RegisteredGlobalDb>>,
    ) -> Self {
        self.global_db = global_db;
        self.accounting_db = self.global_db.clone();
        self.registry_db = registry_db;
        self.session_db.clone_from(&session_db);
        self.user_session_db.clone_from(&user_session_db);
        self.registered_session_db = session_db;
        self.registered_user_session_db = user_session_db;
        self
    }

    pub(crate) fn daemon_owned(
        cg: impl Into<Arc<TraceDecay>>,
        scope_prefix: Option<String>,
        authority: McpServerDaemonAuthority,
    ) -> Self {
        let McpServerDaemonAuthority {
            profile_identity,
            transcript_source_home,
            databases,
            host_admission_broker,
            project_session_refresh_wake,
            user_session_refresh_wake,
            database_owner_reconciler,
            project_routes,
            writers,
        } = authority;
        let profile_root = profile_identity.profile_root().to_path_buf();
        let registry = databases.registry;
        Self {
            cg: cg.into(),
            scope_prefix,
            profile_root: Some(profile_root),
            profile_identity: Some(profile_identity),
            transcript_source_home,
            global_db: Some(Arc::clone(&registry)),
            accounting_db: databases.accounting,
            registry_db: Some(registry),
            session_db: Some(databases.project_sessions),
            user_session_db: Some(databases.user_sessions),
            registered_session_db: Some(databases.registered_project_sessions),
            registered_user_session_db: Some(databases.registered_user_sessions),
            host_admission_broker,
            project_session_refresh_wake: Some(project_session_refresh_wake),
            user_session_refresh_wake: Some(user_session_refresh_wake),
            own_project_host_admission_replay: true,
            startup_catch_up_enabled: true,
            automation_scheduler_reconciler: None,
            database_owner_reconciler: Some(database_owner_reconciler),
            dashboard_automation_writer: writers.dashboard_automation,
            dashboard_doctor_report_reader: None,
            dashboard_doctor_remediation_dispatcher: None,
            dashboard_code_index_freshness_reader: None,
            dashboard_feedback_status_reader: None,
            diagnostics_lsp: None,
            hook_branch_writer: writers.hook_branch,
            background_refresh_writer: writers.background_refresh,
            code_index_hook_sink: None,
            code_index_publication_identity: None,
            code_index_search_executor: None,
            code_index_search_authority: None,
            retained_project_graph_resolver: None,
            project_routes,
            application_invocation_executor: None,
            project_server_live: None,
            #[cfg(any(test, feature = "test-transport"))]
            host_admission_test_runtime: None,
        }
    }

    pub(crate) fn daemon_owned_core(
        cg: impl Into<Arc<TraceDecay>>,
        scope_prefix: Option<String>,
        authority: McpServerDaemonCoreAuthority,
    ) -> Self {
        let McpServerDaemonCoreAuthority {
            profile_identity,
            transcript_source_home,
            accounting,
            registry,
            database_owner_reconciler,
            project_routes,
            writers,
        } = authority;
        let profile_root = profile_identity.profile_root().to_path_buf();
        Self {
            cg: cg.into(),
            scope_prefix,
            profile_root: Some(profile_root),
            profile_identity: Some(profile_identity),
            transcript_source_home,
            global_db: Some(Arc::clone(&registry)),
            accounting_db: accounting,
            registry_db: Some(registry),
            session_db: None,
            user_session_db: None,
            registered_session_db: None,
            registered_user_session_db: None,
            host_admission_broker: None,
            project_session_refresh_wake: None,
            user_session_refresh_wake: None,
            own_project_host_admission_replay: false,
            startup_catch_up_enabled: false,
            automation_scheduler_reconciler: None,
            database_owner_reconciler: Some(database_owner_reconciler),
            dashboard_automation_writer: writers.dashboard_automation,
            dashboard_doctor_report_reader: None,
            dashboard_doctor_remediation_dispatcher: None,
            dashboard_code_index_freshness_reader: None,
            dashboard_feedback_status_reader: None,
            diagnostics_lsp: None,
            hook_branch_writer: writers.hook_branch,
            background_refresh_writer: writers.background_refresh,
            code_index_hook_sink: None,
            code_index_publication_identity: None,
            code_index_search_executor: None,
            code_index_search_authority: None,
            retained_project_graph_resolver: None,
            project_routes,
            application_invocation_executor: None,
            project_server_live: None,
            #[cfg(any(test, feature = "test-transport"))]
            host_admission_test_runtime: None,
        }
    }

    /// Inject the daemon-owned code-index scheduler bridge so after-edit hooks
    /// deliver touched paths into the incremental indexing queue.
    /// Installs the code-index generation authority every diagnostic producer
    /// resolves file and generation identity through.
    pub(crate) fn with_code_index_publication_identity(
        mut self,
        resolver: super::CodeIndexPublicationIdentityResolver,
    ) -> Self {
        self.code_index_publication_identity = Some(resolver);
        self
    }

    pub(crate) fn with_code_index_hook_sink(mut self, sink: super::CodeIndexHookSink) -> Self {
        self.code_index_hook_sink = Some(sink);
        self
    }

    pub(crate) fn with_code_index_search_executor(
        mut self,
        executor: super::CodeIndexSearchExecutor,
    ) -> Self {
        self.code_index_search_executor = Some(executor);
        self
    }

    pub(crate) fn with_code_index_search_authority(
        mut self,
        authority: super::CodeIndexSearchAuthorityV1,
    ) -> Self {
        self.code_index_search_authority = Some(authority);
        self
    }

    pub(crate) fn with_application_invocation_executor(
        mut self,
        executor: Arc<dyn crate::daemon_client::DaemonInvocationExecutor>,
    ) -> Self {
        self.application_invocation_executor = Some(executor);
        self
    }

    pub(crate) fn with_project_server_live(mut self, live: Arc<AtomicBool>) -> Self {
        self.project_server_live = Some(live);
        self
    }

    pub(crate) fn with_retained_project_graph_resolver(
        mut self,
        resolver: super::RetainedProjectGraphResolver,
    ) -> Self {
        self.retained_project_graph_resolver = Some(resolver);
        self
    }

    pub(crate) fn with_automation_scheduler_reconciler(
        mut self,
        reconciler: crate::dashboard::AutomationSchedulerReconciler,
    ) -> Self {
        self.automation_scheduler_reconciler = Some(reconciler);
        self
    }

    pub(crate) fn with_startup_catch_up_enabled(mut self, enabled: bool) -> Self {
        self.startup_catch_up_enabled = enabled;
        self
    }

    pub(crate) fn with_dashboard_doctor_report_reader(
        mut self,
        reader: crate::dashboard::DoctorReportReader,
    ) -> Self {
        self.dashboard_doctor_report_reader = Some(reader);
        self
    }

    pub(crate) fn with_dashboard_doctor_remediation_dispatcher(
        mut self,
        dispatcher: crate::dashboard::DoctorRemediationDispatcherV1,
    ) -> Self {
        self.dashboard_doctor_remediation_dispatcher = Some(dispatcher);
        self
    }

    pub(crate) fn with_dashboard_code_index_freshness_reader(
        mut self,
        reader: crate::dashboard::code_index_freshness_api::CodeIndexFreshnessReader,
    ) -> Self {
        self.dashboard_code_index_freshness_reader = Some(reader);
        self
    }

    pub(crate) fn with_dashboard_feedback_status_reader(
        mut self,
        reader: crate::dashboard::feedback_api::FeedbackStatusReader,
    ) -> Self {
        self.dashboard_feedback_status_reader = Some(reader);
        self
    }

    pub(crate) fn with_diagnostics_lsp(
        mut self,
        diagnostics_lsp: Arc<
            tokio::sync::Mutex<tracedecay_lsp::analyzer::broker::DiagnosticBroker>,
        >,
    ) -> Self {
        self.diagnostics_lsp = Some(diagnostics_lsp);
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

#[cfg(test)]
mod tests {
    use std::process::Command;

    use super::*;

    #[tokio::test]
    async fn direct_context_installs_only_the_explicit_search_executor() {
        let _pin = crate::config::PinnedUserDataDir::new();
        let project = tempfile::tempdir().expect("project");
        let git_init = Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(project.path())
            .output()
            .expect("git init");
        assert!(
            git_init.status.success(),
            "git init failed: {}",
            String::from_utf8_lossy(&git_init.stderr)
        );
        let (cg, _runtime) = TraceDecay::init_test_fixture_with_registered_runtime(
            project.path(),
            "project.mcp-construction",
        )
        .await
        .expect("registered graph");
        let executor: crate::mcp::server::CodeIndexSearchExecutor = Arc::new(|_| {
            Box::pin(async {
                crate::mcp::server::CodeIndexSearchOutcomeV1::Unavailable(
                    crate::mcp::server::CodeIndexSearchUnavailableV1 {
                        code_generation: None,
                        reason: crate::mcp::server::CodeIndexSearchUnavailableReasonV1::AuthorityUnavailable,
                        semantic: crate::mcp::server::CodeIndexSemanticStatusV1::Unavailable {
                            reason: "authority_unavailable",
                        },
                        coverage: crate::mcp::server::CodeIndexSearchCoverageV1::unavailable(
                            "authority_unavailable",
                        ),
                    },
                )
            })
        });
        let context = McpServerConstructionContext::direct(cg, None)
            .with_code_index_search_executor(executor);
        assert!(context.code_index_search_executor.is_some());
        assert!(
            context.code_index_search_authority.is_none(),
            "installing an executor must not fabricate route admission"
        );
    }
}
