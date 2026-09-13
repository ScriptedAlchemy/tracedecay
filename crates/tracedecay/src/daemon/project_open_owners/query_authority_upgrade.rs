//! Daemon spawn wrapper for deferred query-authority mounts.
//!
//! Wait/retry lives in `tracedecay-code-index-runtime`. This module only
//! orders the background task onto the admitted project owner.

use std::path::{Path, PathBuf};
use tracedecay_code_index_runtime::code_index_scheduler::query_runtime::{
    DeferredMountAttemptV1, classify_deferred_query_authority_mount,
    retry_deferred_query_authority_until_serving,
};
use tracedecay_contracts::ResolvedScope;

use super::DaemonInvocationState;

pub(super) use tracedecay_code_index_runtime::code_index_scheduler::query_runtime::DeferredQueryAuthorityMountV1;

/// Spawns the deferred query-authority waiter on the project owner.
///
/// A route whose code index is disabled never seats a text generation, so this
/// returns `false` and never parks a waiter.
pub(super) fn spawn_deferred_query_authority_mount(
    owner: &crate::mcp::McpServer,
    invocation: DaemonInvocationState,
    project_root: PathBuf,
    scope: ResolvedScope,
    mount: DeferredQueryAuthorityMountV1,
) -> bool {
    // Same contract as the deferred advisory owner: a route whose code index
    // is disabled never seats a text generation, so this retry has nothing to
    // wait for. Parking it would poll the shared store once a second for the
    // daemon's life and re-mount on every other route's publication.
    if tracedecay_code_index_runtime::project_reads::code_index_disabled_for_scope(
        &invocation.code_index_schedulers,
        &scope,
    ) {
        tracing::info!(
            event = "query_authority_mount",
            outcome = "code_index_disabled",
            project_id = %scope.project_id,
            deferred = true,
            "route indexes no code by contract; deferred query authority is terminal"
        );
        return false;
    }
    owner.spawn_background_task(hotpath::future!(
        async move {
            let schedulers = invocation.code_index_schedulers.clone();
            retry_deferred_query_authority_until_serving(&schedulers, project_root.clone(), || {
                let invocation = invocation.clone();
                let project_root = project_root.clone();
                let scope = scope.clone();
                let mount = mount.clone();
                async move { try_deferred_mount(&invocation, &project_root, &scope, &mount).await }
            })
            .await;
        },
        label = "daemon.project.query_authority_deferred"
    ))
}

#[hotpath::measure(label = "daemon.project.query_authority_retry", future = true)]
async fn try_deferred_mount(
    invocation: &DaemonInvocationState,
    project_root: &Path,
    scope: &ResolvedScope,
    mount: &DeferredQueryAuthorityMountV1,
) -> DeferredMountAttemptV1 {
    let outcome = match mount {
        DeferredQueryAuthorityMountV1::Configured { profile_id } => {
            invocation
                .mount_query_authority_for_project(project_root, profile_id, scope)
                .await
        }
        DeferredQueryAuthorityMountV1::CoreFallback {
            session_db,
            committed_revision,
        } => match session_db.load_session_cursor_key_provider_result().await {
            Ok(cursor_keys) => {
                if let Some(committed_revision) = committed_revision {
                    invocation
                        .mount_core_query_authority_for_committed_fallback(
                            project_root,
                            scope,
                            committed_revision,
                            &cursor_keys,
                        )
                        .await
                } else {
                    invocation
                        .mount_core_query_authority_for_project(project_root, scope, &cursor_keys)
                        .await
                }
            }
            Err(error) => {
                tracing::warn!(
                    event = "query_authority_mount",
                    outcome = "deferred_failed",
                    project_id = %scope.project_id,
                    reason = %error,
                    "durable query cursor key is unavailable; deferred mount abandoned"
                );
                return DeferredMountAttemptV1::Terminal;
            }
        },
    };
    classify_deferred_query_authority_mount(scope, outcome)
}
