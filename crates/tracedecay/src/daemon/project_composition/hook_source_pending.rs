//! Canonical host-admission input for an exact code-index worktree route.
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tracedecay_code_index_runtime::code_index_scheduler::{
    CodeIndexSourcePendingChangedFutureV1, CodeIndexSourcePendingReaderV1,
};
use tracedecay_contracts::ResolvedScope;
use tracedecay_contracts::code_index_freshness::CodeIndexFreshnessReadFailureV1;
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_host_admission::SharedHostAdmissionBroker;
use tracedecay_mcp::hook_events::{
    DurableHookEventDecodeError, HookEventPlan, decode_durable_hook_event_plan,
};
use tracedecay_project::project::TraceDecay;
use tracedecay_sessions::admission::HostAdmissionOutcome;
use tracedecay_store::StoreShardScopeV1;

use crate::daemon::branch_admin::StoreAdministration;

#[cfg(test)]
mod tests;

struct HookSourcePending {
    broker: SharedHostAdmissionBroker,
    root: PathBuf,
    scope: ResolvedScope,
}

impl CodeIndexSourcePendingReaderV1 for HookSourcePending {
    fn read_pending(&self) -> std::result::Result<bool, CodeIndexFreshnessReadFailureV1> {
        self.broker
            .pending_replay_matches(|payload| {
                let plan = decode_durable_hook_event_plan(payload).map_err(|error| {
                    HostAdmissionOutcome::degraded(match error {
                        DurableHookEventDecodeError::Malformed => "malformed_host_event_plan",
                        DurableHookEventDecodeError::UnsupportedVersion => {
                            "unsupported_host_event_plan"
                        }
                    })
                })?;
                match plan {
                    HookEventPlan::SyncFilesAt { root, scope, .. } => {
                        // Reference is mutable attribution. The canonical route's
                        // project, repository, worktree and byte-exact locator own
                        // the event through checkout changes.
                        Ok(root.as_os_str() == self.root.as_os_str()
                            && scope.project_id == self.scope.project_id
                            && scope.repository_id == self.scope.repository_id
                            && scope.worktree_id == self.scope.worktree_id)
                    }
                    // Shipped unscoped file edits cannot prove a different route.
                    // Keep their source currency unresolved until replay settles.
                    HookEventPlan::SyncFiles(_) => Ok(true),
                    HookEventPlan::AddBranch(_)
                    | HookEventPlan::AddBranchAt { .. }
                    | HookEventPlan::SyncCurrentBranch { .. }
                    | HookEventPlan::DebouncedIncrementalSync(_)
                    | HookEventPlan::RecordTerminalReceipt { .. }
                    | HookEventPlan::MarkTurnIngested { .. }
                    | HookEventPlan::Noop => Ok(false),
                }
            })
            .map_err(|_| CodeIndexFreshnessReadFailureV1::ReadFailed)
    }

    fn revision(&self) -> u64 {
        self.broker.progress_revision()
    }

    fn changed(&self, observed: u64) -> CodeIndexSourcePendingChangedFutureV1<'_> {
        Box::pin(self.broker.wait_for_progress(observed))
    }
}

/// Reuse the same registered project writer/broker as cold admission and full
/// session hosting. Opening it does not settle history or publish host-ingest
/// capabilities on the core server.
pub(in crate::daemon) async fn project_hook_source_pending(
    stores: &StoreAdministration,
    graph: &TraceDecay,
    root: &Path,
    expected_scope: &ResolvedScope,
) -> Result<(
    SharedHostAdmissionBroker,
    Arc<dyn CodeIndexSourcePendingReaderV1>,
)> {
    let registry = stores.registered_profile_database().await?;
    let owner = registry
        .project_registry_context_by_id(expected_scope.project_id.as_str())
        .await?
        .ok_or_else(|| {
            TraceDecayError::project_route(
                "project_route_unavailable",
                false,
                "hook source project is not registered",
            )
        })?;
    let (root, scope) = tracedecay_mcp::scope::resolve_query_scope(&owner, root)
        .map_err(|error| error.into_route_failure().into_error())?;
    expected_scope.validate().map_err(|error| {
        TraceDecayError::project_route("project_route_stale", false, error.to_string())
    })?;
    if scope.project_id != expected_scope.project_id
        || scope.repository_id != expected_scope.repository_id
        || scope.worktree_id != expected_scope.worktree_id
    {
        return Err(TraceDecayError::project_route(
            "project_route_stale",
            false,
            "hook source route identity changed",
        ));
    }
    let database = stores
        .registered_project_session_database(graph.project_root(), graph.store_layout())
        .await?;
    if !matches!(&database.binding().shard_id.scope, StoreShardScopeV1::ProjectSessions { project_id } if project_id == &scope.project_id)
    {
        return Err(TraceDecayError::project_route(
            "project_route_stale",
            false,
            "hook source writer belongs to a different project",
        ));
    }
    let broker = stores.host_admission_broker(&database).await?;
    let reader: Arc<dyn CodeIndexSourcePendingReaderV1> = Arc::new(HookSourcePending {
        broker: Arc::clone(&broker),
        root,
        scope,
    });
    Ok((broker, reader))
}
