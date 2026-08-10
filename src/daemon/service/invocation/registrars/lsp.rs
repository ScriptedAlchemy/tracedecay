use super::*;

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
        self.register_lsp_owner(project_root, DaemonLspInvocationOwner::new(factory))
            .await
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn build_and_register(
        &self,
        project_root: PathBuf,
        scope_grant: CapabilityGrantSnapshot,
        registered_database: Arc<crate::global_db::RegisteredGlobalDb>,
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
        let upstream_capabilities = UpstreamCapabilities {
            supports_diagnostics: semantics.analyzer_available,
            semantic: semantics.semantic_capabilities.clone(),
        };
        let workspace_index = Arc::new(PublishedCodeIndexWorkspaceDocuments::new(
            code_index.as_ref().clone(),
            scope_grant.scope.clone(),
            project_root.clone(),
        ));
        let diagnostic_records = Arc::new(
            tracedecay_usecases::feedback::diagnostics::DatabaseDiagnosticStore::new(database),
        );
        let factory = Arc::new(
            lsp_session_factory(
                runtime,
                feedback_runtime,
                code_index.clone() as Arc<dyn LspCodeIndexProjectionIdentityPort>,
                workspace_index,
                diagnostic_records,
                move |_| Arc::clone(&feedback_cycle_input),
                semantics.semantics,
                diagnostic_broker,
                diagnostics_quiet_window,
                semantics.cancellation,
                gateway_capabilities,
                upstream_capabilities,
            )
            .map_err(|error| TraceDecayError::Config {
                message: format!("could not construct LSP session factory: {error:?}"),
            })?,
        );
        self.register_lsp_owner(
            project_root,
            DaemonLspInvocationOwner::authorized(
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
