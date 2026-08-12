//! Deferred query-authority mounts covering the cold-open generation gap.
//!
//! Both project-open query-authority mounts resolve the privacy domain from an
//! already-complete current code generation, but a fresh project publishes its
//! first generation asynchronously after admission, so the open-time mount can
//! lose that race. Losing it must not deny every callable code query for the
//! rest of the daemon session: retry the exact mount that failed when the
//! scheduler publishes a generation for this project, mirroring the deferred
//! feedback-cycle upgrade.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tracedecay_application::ResolvedScope;

use super::DaemonInvocationState;
use crate::daemon::code_index_scheduler::query_runtime::QueryRuntimeMountErrorV1;

/// Which project-open mount to retry once a generation exists.
pub(super) enum DeferredQueryAuthorityMountV1 {
    /// The configured accepted authority (committed activation present).
    Configured {
        profile_id: tracedecay_domain::configuration::UserProfileId,
    },
    /// The checked-in core exact/lexical/graph fallback (no committed
    /// activation); cursor keys are reloaded at attempt time from the same
    /// durable session store the open-time mount used.
    CoreFallback {
        session_db: Arc<crate::global_db::RegisteredGlobalDb>,
    },
}

/// Waits for the first complete code-index generation of `project_root` and
/// then retries the query-authority mount. Exits when the mount reaches any
/// terminal outcome or the publication channel closes (daemon shutdown).
pub(super) fn spawn_deferred_query_authority_mount(
    invocation: DaemonInvocationState,
    project_root: PathBuf,
    scope: ResolvedScope,
    mount: DeferredQueryAuthorityMountV1,
) {
    tokio::spawn(async move {
        let mut publications = invocation
            .code_index_schedulers
            .subscribe_generation_publications();
        // Generations build on demand and the failed open-time mount may have
        // raced a publish that already landed; answer immediately when a
        // complete generation is already ready instead of waiting for a
        // publication that will never repeat.
        if invocation
            .code_index_schedulers
            .latest_complete_ready_for_scope(&scope)
            .await
            .is_some()
            && try_deferred_mount(&invocation, &project_root, &scope, &mount).await
                != DeferredMountAttemptV1::AwaitNextPublication
        {
            return;
        }
        loop {
            match publications.recv().await {
                Ok(publication) if publication.project_root == project_root => {}
                Ok(_) => continue,
                // A lagged receiver dropped publications; one of them may have
                // been this project's, so attempt the mount anyway.
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
            }
            if try_deferred_mount(&invocation, &project_root, &scope, &mount).await
                != DeferredMountAttemptV1::AwaitNextPublication
            {
                return;
            }
        }
    });
}

/// One deferred mount attempt, terminal unless the generation is still
/// unpublished for this exact scope.
#[derive(PartialEq, Eq)]
enum DeferredMountAttemptV1 {
    Terminal,
    AwaitNextPublication,
}

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
        DeferredQueryAuthorityMountV1::CoreFallback { session_db } => {
            match session_db.load_session_cursor_key_provider_result().await {
                Ok(cursor_keys) => {
                    invocation
                        .mount_core_query_authority_for_project(project_root, scope, &cursor_keys)
                        .await
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
            }
        }
    };
    match outcome {
        Ok(()) => {
            tracing::info!(
                event = "query_authority_mount",
                outcome = "mounted",
                project_id = %scope.project_id,
                deferred = true,
            );
            DeferredMountAttemptV1::Terminal
        }
        // The publication may belong to a different repository scope or the
        // freshness ladder may still be settling; keep waiting for the next
        // publication.
        Err(QueryRuntimeMountErrorV1::GenerationUnavailable) => {
            DeferredMountAttemptV1::AwaitNextPublication
        }
        Err(error) => {
            tracing::warn!(
                event = "query_authority_mount",
                outcome = "deferred_failed",
                project_id = %scope.project_id,
                reason = %error,
                "deferred query authority mount abandoned"
            );
            DeferredMountAttemptV1::Terminal
        }
    }
}
