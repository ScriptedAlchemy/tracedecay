use std::path::PathBuf;

use tracedecay_lsp::{LspRuntimeFailure, LspRuntimeFuture};

use super::super::identity::IndexingIdentityV1;
use super::CodeIndexSchedulerRegistryV1;

impl tracedecay_application::lsp_runtime::LspCodeIndexProjectionIdentityPort
    for CodeIndexSchedulerRegistryV1
{
    fn current_identity(
        &self,
        project_root: PathBuf,
        document_relative_path: Option<String>,
    ) -> LspRuntimeFuture<
        Result<
            tracedecay_application::lsp_runtime::LspCodeIndexProjectionIdentity,
            LspRuntimeFailure,
        >,
    > {
        let registry = self.clone();
        Box::pin(async move {
            let root = project_root
                .canonicalize()
                .map_err(|_| LspRuntimeFailure::new("lsp-code-index-root-unavailable"))?;
            let identity_root = root.clone();
            let live_identity =
                tokio::task::spawn_blocking(move || IndexingIdentityV1::resolve(&identity_root))
                    .await
                    .map_err(|_| LspRuntimeFailure::new("lsp-code-index-identity-task-failed"))?
                    .map_err(|_| LspRuntimeFailure::new("lsp-code-index-identity-unavailable"))?;
            // Bracket the current-generation fence with the cheap Git identity
            // read. A concurrent checkout, commit, or publication then refuses
            // instead of pairing one generation with another HEAD.
            let retained = registry
                .latest_text_serving_for_root(&root)
                .await
                .ok_or_else(|| LspRuntimeFailure::new("lsp-code-index-generation-unavailable"))?;
            let scope = tracedecay_contracts::ResolvedScope::new(
                retained.metadata().manifest().project_id.clone(),
                live_identity.repository_id().clone(),
                live_identity.worktree_id().clone(),
                live_identity.head_ref().cloned(),
            )
            .map_err(|_| LspRuntimeFailure::new("lsp-code-index-scope-unavailable"))?;
            let current = registry
                .latest_text_serving_freshness_for_scope(&scope)
                .await
                .map(|(current, _)| current)
                .ok_or_else(|| LspRuntimeFailure::new("lsp-code-index-generation-unavailable"))?;
            let confirmation_root = root.clone();
            let confirmed_identity = tokio::task::spawn_blocking(move || {
                IndexingIdentityV1::resolve(&confirmation_root)
            })
            .await
            .map_err(|_| LspRuntimeFailure::new("lsp-code-index-identity-task-failed"))?
            .map_err(|_| LspRuntimeFailure::new("lsp-code-index-identity-unavailable"))?;
            if confirmed_identity != live_identity {
                return Err(LspRuntimeFailure::new("lsp-code-index-identity-changed"));
            }
            let generation = current.metadata();
            let snapshot = generation.snapshot();
            if live_identity.repository_id() != &snapshot.repository
                || snapshot.worktree.as_ref() != Some(live_identity.worktree_id())
                || snapshot.reference.as_ref() != live_identity.head_ref()
                || snapshot
                    .source_revision
                    .as_ref()
                    .is_some_and(|revision| Some(revision) != live_identity.head_commit())
            {
                return Err(LspRuntimeFailure::new("lsp-code-index-identity-changed"));
            }
            let document_identity = document_relative_path
                .map(|path| path.replace('\\', "/"))
                .map(|logical_path| {
                    generation
                        .snapshot()
                        .files
                        .iter()
                        .find(|file| file.logical_path == logical_path)
                        .map(|file| (file.file_occurrence_id.clone(), file.content_digest.clone()))
                        .ok_or_else(|| {
                            LspRuntimeFailure::new("lsp-code-index-document-unavailable")
                        })
                })
                .transpose()?;
            let (document_file_occurrence_id, document_content_digest) = document_identity.unzip();
            Ok(
                tracedecay_application::lsp_runtime::LspCodeIndexProjectionIdentity {
                    project: generation.manifest().project_id.clone(),
                    repository: snapshot.repository.clone(),
                    worktree: snapshot.worktree.clone(),
                    reference: snapshot.reference.clone(),
                    head_commit_id: confirmed_identity.head_commit().cloned(),
                    source_revision: snapshot.source_revision.clone(),
                    code_generation_id: generation.manifest().generation_id.clone(),
                    snapshot_digest: generation.manifest().snapshot_digest.clone(),
                    invalidation_digest: generation.manifest().invalidation_digest.clone(),
                    snapshot_content_digest: snapshot.content_identity.clone(),
                    document_file_occurrence_id,
                    document_content_digest,
                },
            )
        })
    }
}
