use std::path::PathBuf;

use tracedecay_lsp::{LspRuntimeFailure, LspRuntimeFuture};

use super::CodeIndexSchedulerRegistryV1;

impl crate::application::lsp_runtime::LspCodeIndexProjectionIdentityPort
    for CodeIndexSchedulerRegistryV1
{
    fn current_identity(
        &self,
        project_root: PathBuf,
        document_relative_path: Option<String>,
    ) -> LspRuntimeFuture<
        Result<crate::application::lsp_runtime::LspCodeIndexProjectionIdentity, LspRuntimeFailure>,
    > {
        let registry = self.clone();
        Box::pin(async move {
            let root = project_root
                .canonicalize()
                .map_err(|_| LspRuntimeFailure::new("lsp-code-index-root-unavailable"))?;
            let current = registry
                .latest_complete_ready(&root)
                .await
                .ok_or_else(|| LspRuntimeFailure::new("lsp-code-index-generation-unavailable"))?;
            let generation = &current.generation;
            let document_content_digest = document_relative_path
                .map(|path| path.replace('\\', "/"))
                .map(|logical_path| {
                    generation
                        .snapshot()
                        .files
                        .iter()
                        .find(|file| file.logical_path == logical_path)
                        .map(|file| file.content_digest.clone())
                        .ok_or_else(|| {
                            LspRuntimeFailure::new("lsp-code-index-document-unavailable")
                        })
                })
                .transpose()?;
            Ok(
                crate::application::lsp_runtime::LspCodeIndexProjectionIdentity {
                    project: generation.manifest().project_id.clone(),
                    repository: generation.snapshot().repository.clone(),
                    worktree: generation.snapshot().worktree.clone(),
                    reference: generation.snapshot().reference.clone(),
                    source_revision: generation.snapshot().source_revision.clone(),
                    code_generation_id: generation.manifest().generation_id.clone(),
                    snapshot_digest: generation.manifest().snapshot_digest.clone(),
                    invalidation_digest: generation.manifest().invalidation_digest.clone(),
                    snapshot_content_digest: generation.snapshot().content_identity.clone(),
                    document_content_digest,
                },
            )
        })
    }
}
