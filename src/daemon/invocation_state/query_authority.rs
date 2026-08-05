//! Query-authority lifecycle owned by the daemon invocation generation.

use super::*;

impl DaemonInvocationState {
    pub(super) async fn mount_query_authority_for_project(
        &self,
        project_root: &Path,
        scope: &tracedecay_application::ResolvedScope,
    ) -> std::result::Result<(), code_index_scheduler::query_runtime::QueryRuntimeMountErrorV1>
    {
        code_index_scheduler::query_runtime::mount_query_authority_on_project_open(
            &self.code_index_schedulers,
            project_root,
            scope,
            &self.query_authority_provider,
        )
        .await
    }

    pub(super) fn restore_initial_query_authority_for_project(
        &self,
        scope: tracedecay_application::ResolvedScope,
        state: crate::config::retrieval::RetrievalProfileStateV1,
        cursor_keys: Arc<crate::global_db::session_temporal::GlobalDbCursorKeyProvider>,
    ) -> std::result::Result<
        query_authority_provider::QueryAuthorityProviderStatusV1,
        query_authority_provider::QueryAuthorityUpdateErrorV1,
    > {
        self.query_authority_provider
            .install_evaluated_initial_state(scope, state, cursor_keys)
    }

    pub(super) fn query_activation_registrar(
        &self,
        project_root: &Path,
        session_db: Arc<crate::global_db::RegisteredGlobalDb>,
    ) -> Arc<dyn crate::application::semantic_runtime::RetrievalProfileActivationObserverV1> {
        Arc::new(
            query_authority_provider::DaemonQueryActivationRegistrarV1::new(
                self.query_authority_provider.clone(),
                self.code_index_schedulers.clone(),
                project_root.to_path_buf(),
                session_db,
            ),
        )
    }
}
