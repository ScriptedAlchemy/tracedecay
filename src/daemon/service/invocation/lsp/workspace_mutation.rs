use super::*;

impl DaemonInvocationService {
    pub(crate) async fn pending_lsp_workspace_mutation(
        &self,
        lsp_registry: &Arc<Mutex<LspSessionRegistry>>,
        session: DaemonLspSessionAccess,
        now_ms: u64,
    ) -> Result<Option<tracedecay_lsp::WorkspaceFolderMutation>, DaemonInvocationProblem> {
        let access = self.authenticate(lsp_registry, session, now_ms).await?;
        let sessions = self.lsp_sessions.lock().await;
        let runtime = sessions
            .get(access.session_id())
            .ok_or(DaemonInvocationProblem::NotFoundOrNotAuthorized)?;
        Ok(runtime.actor.pending_workspace_folder_mutation())
    }

    pub(crate) async fn reject_lsp_workspace_mutation(
        &self,
        lsp_registry: &Arc<Mutex<LspSessionRegistry>>,
        session: DaemonLspSessionAccess,
        mutation: &tracedecay_lsp::WorkspaceFolderMutation,
        now_ms: u64,
    ) -> Result<(), DaemonInvocationProblem> {
        let access = self.authenticate(lsp_registry, session, now_ms).await?;
        let mut sessions = self.lsp_sessions.lock().await;
        let runtime = sessions
            .get_mut(access.session_id())
            .ok_or(DaemonInvocationProblem::NotFoundOrNotAuthorized)?;
        runtime
            .actor
            .reject_workspace_folder_mutation(mutation)
            .map_err(|_| DaemonInvocationProblem::NotFoundOrNotAuthorized)
    }

    pub(crate) async fn apply_lsp_workspace_mutation(
        &self,
        lsp_registry: &Arc<Mutex<LspSessionRegistry>>,
        session: DaemonLspSessionAccess,
        mutation: &tracedecay_lsp::WorkspaceFolderMutation,
        workspace: AuthorizedLspWorkspace,
        providers: PreparedFederatedLspProviderRoutes,
        now_ms: u64,
    ) -> Result<(), DaemonInvocationProblem> {
        let access = self.authenticate(lsp_registry, session, now_ms).await?;
        let mut sessions = self.lsp_sessions.lock().await;
        let runtime = sessions
            .get_mut(access.session_id())
            .ok_or(DaemonInvocationProblem::NotFoundOrNotAuthorized)?;
        runtime
            .actor
            .apply_workspace_folder_mutation(mutation, workspace)
            .map_err(|_| DaemonInvocationProblem::NotFoundOrNotAuthorized)?;
        runtime.providers.replace(providers);
        Ok(())
    }

    pub(crate) async fn prepare_lsp_workspace_providers(
        &self,
        workspace: &AuthorizedLspWorkspace,
    ) -> Option<PreparedFederatedLspProviderRoutes> {
        let factories = if let Some(digest) = workspace.scope_set_digest() {
            self.authorized_lsp_workspaces
                .lock()
                .await
                .get(digest)?
                .factories
                .clone()
        } else {
            let root = workspace
                .resolve_root_uri(workspace.anchor_root_uri())
                .ok()?
                .clone();
            let uri = url::Url::parse(root.uri()).ok()?;
            let path = uri.to_file_path().ok()?.canonicalize().ok()?;
            vec![(root, self.lsp_owner(Some(&path)).await?.factory)]
        };
        FederatedLspProviderAuthority::prepare_replacement(workspace, factories)
    }
}
