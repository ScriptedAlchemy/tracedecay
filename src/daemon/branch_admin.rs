use std::collections::{BTreeSet, HashMap, HashSet};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::json;

use crate::errors::{Result, TraceDecayError};
use crate::mcp::{ErrorCode, JsonRpcRequest, JsonRpcResponse, McpTransport};

#[cfg(unix)]
use super::AutomationSchedulerHandle;
#[cfg(any(unix, test))]
use super::ProjectServerKey;
use super::profile_host_admission_replay::ProfileHostAdmissionReplayRegistry;
use super::{DaemonHandshake, DatabaseOwnerRegistry, authority, write_json_rpc_response};

const BRANCH_ADMIN_TOOL_NAME: &str = "tracedecay_admin_branch";

#[derive(Clone)]
pub(super) enum HostAdmissionBrokerState {
    Available(crate::application::host_admission::SharedHostAdmissionBroker),
    Unavailable(crate::application::host_admission::HostAdmissionOutcome),
}

impl HostAdmissionBrokerState {
    pub(super) fn broker(
        &self,
    ) -> Option<&crate::application::host_admission::SharedHostAdmissionBroker> {
        match self {
            Self::Available(broker) => Some(broker),
            Self::Unavailable(_) => None,
        }
    }

    pub(super) fn unavailable_outcome(
        &self,
    ) -> Option<crate::application::host_admission::HostAdmissionOutcome> {
        match self {
            Self::Available(_) => None,
            Self::Unavailable(outcome) => Some(*outcome),
        }
    }
}

#[cfg(test)]
type ExternalHolderVerifier = fn(&[PathBuf]) -> Result<()>;

/// Coordinates every daemon operation that can create, rekey, or remove a
/// database owner. There is intentionally one gate and one copy of each shared
/// registry so branch administration cannot prove ownership against stale
/// daemon state.
#[derive(Clone)]
pub(super) struct StoreAdministration {
    gate: Arc<tokio::sync::Mutex<()>>,
    project_servers: Arc<tokio::sync::Mutex<DatabaseOwnerRegistry>>,
    global_databases: Arc<tokio::sync::Mutex<HashMap<PathBuf, Arc<crate::global_db::GlobalDb>>>>,
    host_admission_brokers: Arc<
        tokio::sync::Mutex<
            HashMap<PathBuf, crate::application::host_admission::SharedHostAdmissionBroker>,
        >,
    >,
    host_admission_broker_gate: Arc<tokio::sync::Mutex<()>>,
    profile_host_admission_replay: Arc<ProfileHostAdmissionReplayRegistry>,
    #[cfg(unix)]
    automation_schedulers:
        Arc<tokio::sync::Mutex<HashMap<ProjectServerKey, AutomationSchedulerHandle>>>,
    #[cfg(test)]
    external_holder_verifier: Option<ExternalHolderVerifier>,
}

impl Default for StoreAdministration {
    fn default() -> Self {
        Self {
            gate: Arc::new(tokio::sync::Mutex::new(())),
            project_servers: Arc::new(tokio::sync::Mutex::new(DatabaseOwnerRegistry::default())),
            global_databases: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            host_admission_brokers: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            host_admission_broker_gate: Arc::new(tokio::sync::Mutex::new(())),
            profile_host_admission_replay: Arc::new(ProfileHostAdmissionReplayRegistry::default()),
            #[cfg(unix)]
            automation_schedulers: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            #[cfg(test)]
            external_holder_verifier: None,
        }
    }
}

impl StoreAdministration {
    #[cfg(test)]
    pub(super) fn with_project_servers(
        project_servers: Arc<tokio::sync::Mutex<DatabaseOwnerRegistry>>,
    ) -> Self {
        Self {
            project_servers,
            ..Self::default()
        }
    }

    #[cfg(all(test, unix))]
    pub(super) fn with_external_holder_verifier(
        external_holder_verifier: ExternalHolderVerifier,
    ) -> Self {
        Self {
            external_holder_verifier: Some(external_holder_verifier),
            ..Self::default()
        }
    }

    pub(super) fn project_servers(&self) -> &Arc<tokio::sync::Mutex<DatabaseOwnerRegistry>> {
        &self.project_servers
    }

    pub(super) async fn global_database(
        &self,
        path: &Path,
    ) -> Result<Arc<crate::global_db::GlobalDb>> {
        let path = authority::canonical_identity_path(path)?;
        let mut global_databases = self.global_databases.lock().await;
        if let Some(database) = global_databases.get(&path) {
            return Ok(Arc::clone(database));
        }
        let mut database = None;
        for attempt in 0..40 {
            match crate::global_db::GlobalDb::try_open_at(&path).await? {
                Some(opened) => {
                    database = Some(opened);
                    break;
                }
                None if attempt < 39 => {
                    tokio::time::sleep(std::time::Duration::from_millis(25)).await;
                }
                None => break,
            }
        }
        let database = database.ok_or_else(|| TraceDecayError::Config {
            message: format!("daemon global database '{}' is unavailable", path.display()),
        })?;
        let database = Arc::new(database);
        global_databases.insert(path, Arc::clone(&database));
        Ok(database)
    }

    pub(super) async fn user_session_database(
        &self,
        global_db_path: &Path,
    ) -> Result<Arc<crate::global_db::GlobalDb>> {
        let profile_root = global_db_path
            .parent()
            .ok_or_else(|| TraceDecayError::Config {
                message: "could not resolve daemon profile root".to_string(),
            })?;
        self.global_database(&crate::sessions::user_sessions_db_path(profile_root))
            .await
    }

    pub(super) async fn host_admission_broker(
        &self,
        database: &Arc<crate::global_db::GlobalDb>,
    ) -> Result<HostAdmissionBrokerState> {
        let path = authority::canonical_identity_path(database.db_path())?;
        if let Some(broker) = self.host_admission_brokers.lock().await.get(&path).cloned() {
            self.maybe_ensure_user_profile_host_admission_replay(&path, &broker)
                .await;
            return Ok(HostAdmissionBrokerState::Available(broker));
        }

        // Serialize first-open publication without retaining the broker map
        // lock across blocking filesystem work.
        let _open = self.host_admission_broker_gate.lock().await;
        let state = {
            let brokers = self.host_admission_brokers.lock().await;
            if let Some(broker) = brokers.get(&path) {
                HostAdmissionBrokerState::Available(Arc::clone(broker))
            } else {
                drop(brokers);
                let open_path = path.clone();
                let opened = tokio::task::spawn_blocking(move || {
                    crate::application::host_admission::HostAdmissionRuntime::open_for_database(
                        &open_path,
                    )
                })
                .await;
                let state = match opened {
                    Ok(Ok((runtime, _))) => HostAdmissionBrokerState::Available(Arc::new(
                        crate::application::host_admission::HostAdmissionBroker::new(runtime),
                    )),
                    Ok(Err(outcome)) => HostAdmissionBrokerState::Unavailable(outcome),
                    Err(_) => HostAdmissionBrokerState::Unavailable(
                        crate::application::host_admission::HostAdmissionOutcome::retained_unavailable(
                            "spool_runtime_unavailable",
                        ),
                    ),
                };
                if let HostAdmissionBrokerState::Available(broker) = &state {
                    self.host_admission_brokers
                        .lock()
                        .await
                        .insert(path.clone(), Arc::clone(broker));
                }
                state
            }
        };
        if let Some(broker) = state.broker() {
            self.maybe_ensure_user_profile_host_admission_replay(&path, broker)
                .await;
        }
        Ok(state)
    }

    /// Kick the coalesced user-profile replay worker. Never awaits a replay pass.
    pub(super) async fn ensure_user_profile_host_admission_replay(
        &self,
        profile_root: &Path,
        broker: &crate::application::host_admission::SharedHostAdmissionBroker,
        broker_path: &Path,
    ) {
        self.profile_host_admission_replay
            .ensure(broker_path, profile_root, broker)
            .await;
    }

    async fn maybe_ensure_user_profile_host_admission_replay(
        &self,
        broker_path: &Path,
        broker: &crate::application::host_admission::SharedHostAdmissionBroker,
    ) {
        let is_user_sessions = broker_path.file_name().and_then(|name| name.to_str())
            == Some(crate::sessions::USER_SESSIONS_DB_FILENAME);
        if !is_user_sessions {
            return;
        }
        let Some(profile_root) = broker_path.parent() else {
            return;
        };
        self.ensure_user_profile_host_admission_replay(profile_root, broker, broker_path)
            .await;
    }

    #[cfg(test)]
    pub(super) async fn wait_user_profile_host_admission_replay_idle(
        &self,
        broker_path: &Path,
        timeout: std::time::Duration,
    ) -> bool {
        self.profile_host_admission_replay
            .wait_idle(broker_path, timeout)
            .await
    }

    pub(super) async fn shutdown_host_admission_replay(&self) {
        self.profile_host_admission_replay.shutdown().await;
    }

    #[cfg(unix)]
    pub(super) fn automation_schedulers(
        &self,
    ) -> &Arc<tokio::sync::Mutex<HashMap<ProjectServerKey, AutomationSchedulerHandle>>> {
        &self.automation_schedulers
    }

    #[cfg_attr(not(test), allow(clippy::unused_self))]
    fn prove_no_external_branch_store_holders(&self, database_paths: &[PathBuf]) -> Result<()> {
        #[cfg(test)]
        if let Some(external_holder_verifier) = self.external_holder_verifier {
            return external_holder_verifier(database_paths);
        }
        ensure_no_external_branch_store_holders(database_paths)
    }

    /// Acquires writer administration before constructing the supplied future
    /// and holds it until that future completes.
    pub(super) async fn with_writer<Operation, OperationFuture, Output>(
        &self,
        operation: Operation,
    ) -> Output
    where
        Operation: FnOnce() -> OperationFuture,
        OperationFuture: Future<Output = Output>,
    {
        let _writer = self.gate.lock().await;
        operation().await
    }

    /// Resolves the authenticated client's project layout and runs destructive
    /// branch administration against that exact profile-owned store.
    pub(super) async fn execute_branch_admin_for_handshake(
        &self,
        handshake: &DaemonHandshake,
        action: crate::branch::BranchAdminAction,
    ) -> Result<crate::branch::BranchAdminReport> {
        let project_root =
            handshake
                .project_path
                .as_deref()
                .ok_or_else(|| TraceDecayError::Config {
                    message: "branch administration requires a project path".to_string(),
                })?;
        let layout =
            crate::storage::resolve_layout(project_root, &handshake.client_identity.profile_root)?;
        let config = crate::config::load_sync_config(project_root);
        self.execute_branch_admin_in_layout(
            project_root,
            &layout.data_root,
            action,
            config.branch_gc_days,
            config.orphan_db_gc_days,
        )
        .await
    }

    /// Prepares, proves, and commits one destructive branch-store mutation while
    /// excluding every daemon writer. Cached owners fail closed and are left
    /// completely untouched; operators must restart the daemon to release them.
    pub(super) async fn execute_branch_admin_in_layout(
        &self,
        project_root: &Path,
        data_root: &Path,
        action: crate::branch::BranchAdminAction,
        branch_gc_days: u64,
        orphan_db_gc_days: u64,
    ) -> Result<crate::branch::BranchAdminReport> {
        self.with_writer(|| async {
            if let Some(recovery) =
                crate::branch::prepare_pending_branch_admin_recovery(data_root)?
            {
                let database_paths =
                    canonical_branch_database_paths(recovery.database_paths())?;
                {
                    let project_servers = self.project_servers.lock().await;
                    #[cfg(unix)]
                    let scheduler_busy = cached_scheduler_owns_selected(
                        &*self.automation_schedulers.lock().await,
                        &database_paths,
                    );
                    #[cfg(not(unix))]
                    let scheduler_busy = false;
                    ensure_no_cached_store_owners(
                        &project_servers,
                        scheduler_busy,
                        &database_paths,
                    )?;
                }

                let canonical_paths = database_paths.iter().cloned().collect::<Vec<_>>();
                let (fence, states) = crate::db::DatabaseDeletionFence::reacquire(
                    &canonical_paths,
                    recovery.transaction_id(),
                    "recover branch SQLite family deletion",
                )?;
                ensure_recovery_tombstone_states(recovery.disposition(), states)?;
                let fenced_paths = fence.database_paths().collect::<Vec<_>>();
                if fenced_paths.len() != database_paths.len()
                    || fenced_paths
                        .iter()
                        .any(|path| !database_paths.contains(*path))
                {
                    return Err(TraceDecayError::Config {
                        message: "database deletion recovery fence resolved a different branch-store identity set"
                            .to_string(),
                    });
                }

                recovery.recover(
                    |paths| self.prove_no_external_branch_store_holders(paths),
                    |disposition| match disposition {
                        crate::branch::BranchAdminRecoveryDisposition::PreCommitRollback => {
                            fence.rollback_deleting()
                        }
                        crate::branch::BranchAdminRecoveryDisposition::CommittedCleanup => {
                            fence.promote_deleted()
                        }
                    },
                )?;
            }

            let prepared = crate::branch::prepare_branch_admin_mutation(
                project_root,
                data_root,
                action,
                branch_gc_days,
                orphan_db_gc_days,
            )?;
            let database_paths = canonical_branch_database_paths(prepared.database_paths())?;
            if database_paths.is_empty() {
                return prepared.finish_without_database_deletion();
            }

            {
                let project_servers = self.project_servers.lock().await;
                #[cfg(unix)]
                let scheduler_busy = cached_scheduler_owns_selected(
                    &*self.automation_schedulers.lock().await,
                    &database_paths,
                );
                #[cfg(not(unix))]
                let scheduler_busy = false;
                ensure_no_cached_store_owners(
                    &project_servers,
                    scheduler_busy,
                    &database_paths,
                )?;
            }

            let canonical_paths = database_paths.iter().cloned().collect::<Vec<_>>();
            let fence = crate::db::DatabaseDeletionFence::acquire(
                &canonical_paths,
                "delete branch SQLite families",
            )?;
            let fenced_paths = fence.database_paths().collect::<Vec<_>>();
            if fenced_paths.len() != database_paths.len()
                || fenced_paths
                    .iter()
                    .any(|path| !database_paths.contains(*path))
            {
                return Err(TraceDecayError::Config {
                    message:
                        "database deletion fence resolved a different branch-store identity set"
                            .to_string(),
                });
            }
            prepared.commit_with_transaction(
                fence.transaction_id(),
                || fence.publish_deleting(),
                |paths| self.prove_no_external_branch_store_holders(paths),
                || fence.rollback_deleting(),
                || fence.promote_deleted(),
            )
        })
        .await
    }
}

pub(super) struct BranchAdminRequest {
    pub(super) id: serde_json::Value,
    pub(super) action: std::result::Result<crate::branch::BranchAdminAction, String>,
}

pub(super) fn parse_branch_admin_request(line: &str) -> Option<BranchAdminRequest> {
    let request = serde_json::from_str::<JsonRpcRequest>(line.trim()).ok()?;
    if request.method != "tools/call" {
        return None;
    }
    let params = request.params.as_ref()?;
    if params.get("name").and_then(serde_json::Value::as_str) != Some(BRANCH_ADMIN_TOOL_NAME) {
        return None;
    }
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new()));
    Some(BranchAdminRequest {
        id: request.id.unwrap_or(serde_json::Value::Null),
        action: serde_json::from_value(arguments)
            .map_err(|error| format!("invalid branch administration arguments: {error}")),
    })
}

fn canonical_branch_database_paths(paths: &[PathBuf]) -> Result<HashSet<PathBuf>> {
    paths
        .iter()
        .map(|path| authority::canonical_identity_path(path))
        .collect()
}

#[cfg(any(unix, test))]
fn cached_scheduler_owns_selected<Scheduler>(
    automation_schedulers: &HashMap<ProjectServerKey, Scheduler>,
    database_paths: &HashSet<PathBuf>,
) -> bool {
    automation_schedulers
        .keys()
        .any(|key| database_paths.contains(&key.owner.graph_db_path))
}

fn ensure_no_cached_store_owners<Server>(
    project_servers: &DatabaseOwnerRegistry<Server>,
    scheduler_busy: bool,
    database_paths: &HashSet<PathBuf>,
) -> Result<()> {
    let server_busy = project_servers
        .servers
        .keys()
        .any(|key| database_paths.contains(&key.owner.graph_db_path));
    if !server_busy && !scheduler_busy {
        return Ok(());
    }

    let cached_as = match (server_busy, scheduler_busy) {
        (true, true) => "a project server and an automation scheduler",
        (true, false) => "a project server",
        (false, true) => "an automation scheduler",
        (false, false) => return Ok(()),
    };
    let mut paths = database_paths
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>();
    paths.sort();
    Err(TraceDecayError::Config {
        message: format!(
            "branch store administration is busy: selected database(s) {} are still cached by the daemon as {cached_as}; restart the TraceDecay daemon before retrying",
            paths.join(", ")
        ),
    })
}

fn ensure_recovery_tombstone_states(
    disposition: crate::branch::BranchAdminRecoveryDisposition,
    states: crate::db::DatabaseDeletionStates,
) -> Result<()> {
    let valid = match disposition {
        crate::branch::BranchAdminRecoveryDisposition::PreCommitRollback => !states.has_deleted(),
        crate::branch::BranchAdminRecoveryDisposition::CommittedCleanup => !states.has_missing(),
    };
    if valid {
        return Ok(());
    }
    Err(TraceDecayError::Config {
        message: format!(
            "branch deletion recovery found incompatible tombstone states for {disposition:?}: missing={}, deleting={}, deleted={}",
            states.missing(),
            states.deleting(),
            states.deleted(),
        ),
    })
}

fn ensure_no_external_branch_store_holders(database_paths: &[PathBuf]) -> Result<()> {
    let options = crate::open_store_holders::OpenStoreHolderScanOptions {
        include_current_process: true,
        excluded_current_process_fds: BTreeSet::new(),
    };
    let scan = crate::open_store_holders::scan_with_options(database_paths, &options).map_err(
        |error| TraceDecayError::Config {
            message: format!("failed to inspect open branch stores: {error}"),
        },
    )?;
    match scan {
        crate::open_store_holders::OpenStoreHolderScan::Supported(holders)
            if holders.is_empty() =>
        {
            Ok(())
        }
        crate::open_store_holders::OpenStoreHolderScan::Supported(holders) => {
            let details = holders
                .into_iter()
                .map(|holder| format!("pid {} ({})", holder.pid, holder.command))
                .collect::<Vec<_>>()
                .join(", ");
            Err(TraceDecayError::Config {
                message: format!(
                    "cannot delete branch stores while external processes still hold them: {details}"
                ),
            })
        }
        crate::open_store_holders::OpenStoreHolderScan::Unsupported { reason } => {
            Err(TraceDecayError::Config {
                message: format!(
                    "cannot prove branch stores are closed: {reason}; destructive branch operation refused"
                ),
            })
        }
    }
}

fn branch_admin_tool_result(
    report: &crate::branch::BranchAdminReport,
) -> Result<serde_json::Value> {
    Ok(json!({
        "content": [{
            "type": "text",
            "text": serde_json::to_string(report)?,
        }]
    }))
}

pub(super) async fn write_branch_admin_response(
    transport: &mut impl McpTransport,
    request: BranchAdminRequest,
    result: Result<crate::branch::BranchAdminReport>,
) -> Result<()> {
    let response = match (request.action, result) {
        (Err(message), _) => JsonRpcResponse::error(request.id, ErrorCode::InvalidParams, message),
        (Ok(_), Ok(report)) => {
            JsonRpcResponse::success(request.id, branch_admin_tool_result(&report)?)
        }
        (Ok(_), Err(error)) => {
            JsonRpcResponse::error(request.id, ErrorCode::InternalError, error.to_string())
        }
    };
    write_json_rpc_response(transport, &response).await
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::super::{ProjectRouteKey, StoreOwnerKey};
    use super::*;

    #[tokio::test]
    async fn unavailable_spool_does_not_block_unrelated_database_authority() {
        let temp = tempfile::tempdir().unwrap();
        let administration = StoreAdministration::default();
        let blocked_path = temp.path().join("blocked.db");
        let blocked_database = administration.global_database(&blocked_path).await.unwrap();
        std::fs::write(
            temp.path().join(".blocked.db.host-admission"),
            "not a directory",
        )
        .unwrap();

        let blocked = administration
            .host_admission_broker(&blocked_database)
            .await
            .unwrap();
        let outcome = blocked
            .unavailable_outcome()
            .expect("spool open failure must be represented as typed unavailability");
        assert_eq!(
            outcome.status,
            crate::application::host_admission::HostAdmissionStatus::Unavailable
        );
        assert!(blocked.broker().is_none());

        let healthy_database = administration
            .global_database(&temp.path().join("healthy.db"))
            .await
            .unwrap();
        let healthy = administration
            .host_admission_broker(&healthy_database)
            .await
            .unwrap();
        assert!(healthy.broker().is_some());
        administration.shutdown_host_admission_replay().await;
    }

    fn owner(graph_db_path: &str) -> StoreOwnerKey {
        StoreOwnerKey {
            profile_root: PathBuf::from("/profile"),
            global_db_path: PathBuf::from("/profile/global.db"),
            project_id: Some("project".to_string()),
            store_root: PathBuf::from("/profile/projects/project"),
            graph_db_path: PathBuf::from(graph_db_path),
        }
    }

    fn server_key(graph_db_path: &str, scope_prefix: Option<&str>) -> ProjectServerKey {
        ProjectServerKey {
            owner: owner(graph_db_path),
            scope_prefix: scope_prefix.map(str::to_string),
        }
    }

    fn route(project_path: &str, scope_prefix: Option<&str>) -> ProjectRouteKey {
        ProjectRouteKey {
            profile_root: PathBuf::from("/profile"),
            global_db_path: PathBuf::from("/profile/global.db"),
            project_path: PathBuf::from(project_path),
            scope_prefix: scope_prefix.map(str::to_string),
        }
    }

    #[test]
    fn recovery_phase_validation_accepts_only_phase_compatible_mixed_states() {
        let temp = tempfile::tempdir().unwrap();
        let first = temp.path().join("a.db");
        let second = temp.path().join("b.db");

        let fence = crate::db::DatabaseDeletionFence::acquire(
            &[first.clone(), second.clone()],
            "test partial publication",
        )
        .unwrap();
        fence.publish_deleting().unwrap();
        let transaction_id = fence.transaction_id().to_string();
        let first_tombstone = fence.tombstone_paths().next().unwrap().to_path_buf();
        std::fs::remove_file(first_tombstone).unwrap();
        drop(fence);
        let (fence, states) = crate::db::DatabaseDeletionFence::reacquire(
            &[second.clone(), first.clone()],
            &transaction_id,
            "test partial publication recovery",
        )
        .unwrap();
        ensure_recovery_tombstone_states(
            crate::branch::BranchAdminRecoveryDisposition::PreCommitRollback,
            states,
        )
        .unwrap();
        ensure_recovery_tombstone_states(
            crate::branch::BranchAdminRecoveryDisposition::CommittedCleanup,
            states,
        )
        .unwrap_err();
        fence.rollback_deleting().unwrap();
        drop(fence);

        let fence = crate::db::DatabaseDeletionFence::acquire(
            &[first.clone(), second.clone()],
            "test partial promotion",
        )
        .unwrap();
        fence.publish_deleting().unwrap();
        fence.promote_deleted().unwrap();
        let transaction_id = fence.transaction_id().to_string();
        let first_tombstone = fence.tombstone_paths().next().unwrap().to_path_buf();
        let deleted = std::fs::read_to_string(&first_tombstone).unwrap();
        std::fs::write(
            &first_tombstone,
            deleted.replace("state=deleted", "state=deleting"),
        )
        .unwrap();
        drop(fence);
        let (fence, states) = crate::db::DatabaseDeletionFence::reacquire(
            &[second, first],
            &transaction_id,
            "test partial promotion recovery",
        )
        .unwrap();
        ensure_recovery_tombstone_states(
            crate::branch::BranchAdminRecoveryDisposition::CommittedCleanup,
            states,
        )
        .unwrap();
        ensure_recovery_tombstone_states(
            crate::branch::BranchAdminRecoveryDisposition::PreCommitRollback,
            states,
        )
        .unwrap_err();
        fence.promote_deleted().unwrap();
    }

    #[test]
    fn matching_cached_server_and_scheduler_fail_busy_without_mutation() {
        let target_a = server_key("/profile/projects/project/branches/feature.db", None);
        let target_b = server_key("/profile/projects/project/branches/feature.db", Some("src"));
        let survivor = server_key("/profile/projects/project/tracedecay.db", None);
        let target_route_a = route("/repo", None);
        let target_route_b = route("/repo", Some("src"));
        let survivor_route = route("/repo-main", None);
        let target_server_a = Arc::new("target-a");
        let target_server_b = Arc::new("target-b");
        let survivor_server = Arc::new("survivor");
        let mut registry = DatabaseOwnerRegistry::default();
        registry.insert_route(
            target_route_a.clone(),
            target_a.clone(),
            Arc::clone(&target_server_a),
        );
        registry.insert_route(
            target_route_b.clone(),
            target_b.clone(),
            Arc::clone(&target_server_b),
        );
        registry.insert_route(
            survivor_route.clone(),
            survivor.clone(),
            Arc::clone(&survivor_server),
        );
        let scheduler = Arc::new("scheduler");
        let mut schedulers = HashMap::from([(target_b.clone(), Arc::clone(&scheduler))]);
        let selected = HashSet::from([PathBuf::from(
            "/profile/projects/project/branches/feature.db",
        )]);

        let error = ensure_no_cached_store_owners(
            &registry,
            cached_scheduler_owns_selected(&schedulers, &selected),
            &selected,
        )
        .expect_err("matching daemon owners must fail closed");

        let message = error.to_string();
        assert!(message.contains("busy"), "{message}");
        assert!(
            message.contains("restart the TraceDecay daemon"),
            "{message}"
        );
        ensure_no_cached_store_owners(&registry, false, &selected)
            .expect_err("a matching project server alone must fail closed");
        let no_servers: DatabaseOwnerRegistry<Arc<&str>> = DatabaseOwnerRegistry::default();
        ensure_no_cached_store_owners(
            &no_servers,
            cached_scheduler_owns_selected(&schedulers, &selected),
            &selected,
        )
        .expect_err("a matching scheduler alone must fail closed");
        assert!(Arc::ptr_eq(
            registry
                .get_route(&target_route_a)
                .expect("target a route")
                .1,
            &target_server_a
        ));
        assert!(Arc::ptr_eq(
            registry
                .get_route(&target_route_b)
                .expect("target b route")
                .1,
            &target_server_b
        ));
        assert!(Arc::ptr_eq(
            registry
                .get_route(&survivor_route)
                .expect("survivor route")
                .1,
            &survivor_server
        ));
        assert!(Arc::ptr_eq(
            schedulers.get(&target_b).expect("scheduler entry"),
            &scheduler
        ));
        assert_eq!(registry.servers.len(), 3);
        assert_eq!(registry.aliases.len(), 3);
        assert_eq!(schedulers.len(), 1);

        // Keep the maps mutable in this regression test so accidental eviction
        // implementations cannot hide behind immutable test fixtures.
        assert!(schedulers.remove(&survivor).is_none());
    }

    #[test]
    fn unmatched_cached_owners_allow_administration_to_continue() {
        let survivor = server_key("/profile/projects/project/tracedecay.db", None);
        let survivor_route = route("/repo-main", None);
        let survivor_server = Arc::new("survivor");
        let mut registry = DatabaseOwnerRegistry::default();
        registry.insert_route(
            survivor_route.clone(),
            survivor.clone(),
            Arc::clone(&survivor_server),
        );
        let scheduler = Arc::new("scheduler");
        let schedulers = HashMap::from([(survivor.clone(), Arc::clone(&scheduler))]);
        let selected = HashSet::from([PathBuf::from(
            "/profile/projects/project/branches/feature.db",
        )]);

        ensure_no_cached_store_owners(
            &registry,
            cached_scheduler_owns_selected(&schedulers, &selected),
            &selected,
        )
        .expect("unmatched owners must proceed to holder proof and commit");

        assert!(Arc::ptr_eq(
            registry
                .get_route(&survivor_route)
                .expect("survivor route")
                .1,
            &survivor_server
        ));
        assert!(Arc::ptr_eq(
            schedulers.get(&survivor).expect("scheduler entry"),
            &scheduler
        ));
    }

    #[test]
    fn branch_admin_parser_accepts_only_the_hidden_destructive_tool() {
        let request = parse_branch_admin_request(
            &serde_json::json!({
                "jsonrpc": "2.0",
                "id": 7,
                "method": "tools/call",
                "params": {
                    "name": BRANCH_ADMIN_TOOL_NAME,
                    "arguments": { "action": "remove", "branch": "feature/a" }
                }
            })
            .to_string(),
        )
        .expect("branch admin request");
        assert_eq!(request.id, json!(7));
        assert_eq!(
            request.action.expect("valid action"),
            crate::branch::BranchAdminAction::Remove {
                branch: "feature/a".to_string()
            }
        );

        assert!(
            parse_branch_admin_request(
                &serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 8,
                    "method": "tools/call",
                    "params": { "name": "tracedecay_status", "arguments": {} }
                })
                .to_string()
            )
            .is_none()
        );
    }

    #[test]
    fn branch_admin_parser_preserves_invalid_arguments_for_error_response() {
        let request = parse_branch_admin_request(
            &serde_json::json!({
                "jsonrpc": "2.0",
                "id": "bad",
                "method": "tools/call",
                "params": {
                    "name": BRANCH_ADMIN_TOOL_NAME,
                    "arguments": { "action": "remove" }
                }
            })
            .to_string(),
        )
        .expect("recognized hidden tool");
        assert!(
            request
                .action
                .unwrap_err()
                .contains("invalid branch administration arguments")
        );
    }
}
