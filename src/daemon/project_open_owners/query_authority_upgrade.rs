//! Deferred query-authority mounts covering the cold-open generation gap.
//!
//! Both project-open query-authority mounts resolve the privacy domain from an
//! already-complete current code generation, but a fresh project publishes its
//! first generation asynchronously after admission, so the open-time mount can
//! lose that race. A physical restart restores a sealed generation as `Noop`
//! and does not republish, so waiters must also poll the serving slot: a
//! publication that will never repeat must not deny search for the rest of the
//! daemon session. Retry the exact mount that failed when this project's
//! generation becomes serving, mirroring the deferred feedback-cycle upgrade.

use std::path::{Path, PathBuf};
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
        session_db: crate::global_db::RegisteredGlobalDbLeaseV1,
    },
}

/// Waits for the first complete code-index generation of `project_root` and
/// then retries the query-authority mount. Exits when the mount reaches any
/// terminal outcome or the publication channel closes (daemon shutdown).
///
/// The open-time mount runs before code-index activation, so the first ready
/// check usually misses. A later `Published` event wakes the waiter on a
/// fresh build; a restart that restores the same sealed generation records
/// `Noop` and never repeats that event, so the serving slot is polled too.
pub(super) fn spawn_deferred_query_authority_mount(
    owner: &crate::mcp::McpServer,
    invocation: DaemonInvocationState,
    project_root: PathBuf,
    scope: ResolvedScope,
    mount: DeferredQueryAuthorityMountV1,
) -> bool {
    owner.spawn_background_task(async move {
        let mut publications = invocation
            .code_index_schedulers
            .subscribe_generation_publications();
        let mut ready_poll = tokio::time::interval(std::time::Duration::from_secs(1));
        ready_poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
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
            tokio::select! {
                _ = ready_poll.tick() => {}
                publication = publications.recv() => match publication {
                    Ok(publication) if publication.project_root == project_root => {}
                    // Another project's publication; loop for the next signal.
                    Ok(_) => {}
                    // A lagged receiver dropped publications; one of them may have
                    // been this project's, so attempt the mount anyway.
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                }
            }
        }
    })
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
