use super::*;
use crate::diagnostics::lsp::semantic::ProductionSemanticAuthorities;

#[derive(Clone)]
pub(crate) struct DaemonLspOwnerRegistrar {
    service: DaemonInvocationService,
}

impl DaemonLspOwnerRegistrar {
    pub(crate) fn new(service: &DaemonInvocationService) -> Self {
        Self {
            service: service.clone(),
        }
    }

    pub(crate) async fn register_lsp_owner(
        &self,
        project_root: PathBuf,
        owner: DaemonLspInvocationOwner,
    ) -> Result<(), ProjectRuntimeRegistryError> {
        self.service.install_lsp_owner(project_root, owner).await
    }

    #[cfg(test)]
    pub(crate) async fn register_factory(
        &self,
        project_root: PathBuf,
        factory: Arc<DaemonLspSessionFactory>,
    ) -> Result<(), ProjectRuntimeRegistryError> {
        self.register_factory_for_project(
            project_root,
            UserProfileId::new("profile.test.lsp").expect("test LSP profile"),
            ProjectId::new("project.test.lsp").expect("test LSP project"),
            factory,
        )
        .await
    }

    #[cfg(test)]
    pub(crate) async fn register_factory_for_project(
        &self,
        project_root: PathBuf,
        profile_id: UserProfileId,
        project_id: ProjectId,
        factory: Arc<DaemonLspSessionFactory>,
    ) -> Result<(), ProjectRuntimeRegistryError> {
        self.register_lsp_owner(
            project_root.clone(),
            DaemonLspInvocationOwner::for_test_project(
                factory,
                profile_id,
                project_id,
                project_root,
            ),
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn build_and_register(
        &self,
        project_root: PathBuf,
        scope_grant: CapabilityGrantSnapshot,
        registered_database: crate::global_db::RegisteredGlobalDbLeaseV1,
        database: Database,
        code_index: Arc<crate::daemon::code_index_scheduler::CodeIndexSchedulerRegistryV1>,
        runtime: tokio::runtime::Handle,
        diagnostic_broker: Arc<Mutex<DiagnosticBroker>>,
        languages: &[String],
        root_uri: String,
        timeouts: LspRefreshTimeouts,
        diagnostics_quiet_window: Duration,
        gateway_capabilities: GatewayCapabilities,
    ) -> Result<Arc<DaemonLspSessionFactory>, TraceDecayError> {
        let project_identity = InvocationProjectRuntimeIdentityV1::new(
            registered_database.binding().shard_id.profile_id.clone(),
            scope_grant.scope.project_id.clone(),
            project_root.clone(),
        );
        let feedback_runtime = self
            .service
            .feedback_runtime(Some(&project_root))
            .await
            .ok_or_else(|| TraceDecayError::Config {
                message: "feedback runtime is not registered for the project".to_owned(),
            })?;
        let feedback_cycle_input = self
            .service
            .feedback_cycle_input(Some(&project_root))
            .await
            .ok_or_else(|| TraceDecayError::Config {
                message: "production feedback cycle input is not registered for the project"
                    .to_owned(),
            })?;
        let scope_set_storage = registered_database.authorized_scope_set_storage()?;
        let delivery_settlements = self
            .service
            .delivery_settlement_recorder(Some(&project_root))
            .await
            .ok_or_else(|| TraceDecayError::Config {
                message: "production LSP delivery settlement recorder is unavailable".to_owned(),
            })?;
        let mut gateway_capabilities = gateway_capabilities;
        gateway_capabilities.supports_workspace_folders = true;
        let semantics = production_semantic_authorities(
            runtime.clone(),
            diagnostic_broker.clone(),
            languages,
            project_root.clone(),
            root_uri,
            timeouts,
        )
        .await?;
        let ProductionSemanticAuthorities {
            semantics,
            cancellation,
            upstream_capability_initializer,
        } = semantics;
        let workspace_index = Arc::new(PublishedCodeIndexWorkspaceDocuments::new(
            code_index.as_ref().clone(),
            scope_grant.scope.clone(),
            project_root.clone(),
        ));
        let diagnostic_records = Arc::new(
            tracedecay_usecases::feedback::diagnostics::DatabaseDiagnosticStore::new(database),
        );
        // The invocation handler publishes into the same per-project fan-out
        // that sessions from this factory forward as read-only notifications.
        let native_integration_status = self
            .service
            .native_integration_status_broadcast(&project_root)
            .await;
        let factory = Arc::new(
            lsp_session_factory(
                runtime,
                feedback_runtime,
                code_index.clone() as Arc<dyn LspCodeIndexProjectionIdentityPort>,
                workspace_index,
                diagnostic_records,
                move |_| Arc::clone(&feedback_cycle_input),
                semantics,
                diagnostic_broker,
                diagnostics_quiet_window,
                cancellation,
                gateway_capabilities,
                UpstreamCapabilities::default(),
            )
            .map_err(|error| TraceDecayError::Config {
                message: format!("could not construct LSP session factory: {error:?}"),
            })?
            .with_upstream_capability_initializer(upstream_capability_initializer)
            .with_native_integration_status_port(native_integration_status),
        );
        self.register_lsp_owner(
            project_root,
            DaemonLspInvocationOwner::authorized(
                project_identity,
                factory.clone(),
                scope_grant,
                scope_set_storage,
                delivery_settlements,
            ),
        )
        .await?;
        Ok(factory)
    }
}
