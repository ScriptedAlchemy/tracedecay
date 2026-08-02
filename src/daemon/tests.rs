use std::path::PathBuf;
#[cfg(unix)]
use std::process::Command;
use std::sync::Arc;
#[cfg(unix)]
use std::sync::PoisonError;

#[cfg(unix)]
use serde_json::Value;
use serde_json::json;
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
#[cfg(unix)]
use tokio::task::JoinHandle;
use tracedecay_query::code_search;

#[cfg(unix)]
use super::explicit_git_state;
#[cfg(unix)]
use super::scheduler::{AutomationSchedulerExitBarrier, AutomationSchedulerLifecycle};
#[cfg(unix)]
use super::{
    AutomationSchedulerHandle, DaemonEngine, MemoryRepairSchedulerHandle, drain_client_tasks,
};
use super::{
    DaemonClientIdentity, DaemonHandshake, DaemonLifecycle, DatabaseOwnerRegistry, ProjectRouteKey,
    ProjectServerKey, StoreAdministration, StoreOwnerKey, multi_root_family_allows,
};

mod bootstrap;
mod code_index_hydration;
mod compatibility;
mod handshake;
mod lifecycle;
mod logging;
mod multi_root_journey;
mod ownership;
mod replay;
mod restart_proxy;
mod rmcp_route;
mod scheduler_config;
mod scheduler_shutdown;
mod socket;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ObservedMcpRoute {
    Rmcp,
    Legacy,
}

fn observed_mcp_routes()
-> &'static std::sync::Mutex<std::collections::HashMap<String, Vec<ObservedMcpRoute>>> {
    static ROUTES: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<String, Vec<ObservedMcpRoute>>>,
    > = std::sync::OnceLock::new();
    ROUTES.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

fn register_mcp_route_observer(client_instance_id: &str) {
    observed_mcp_routes()
        .lock()
        .expect("route observer")
        .insert(client_instance_id.to_owned(), Vec::new());
}

pub(super) fn record_mcp_route(client_instance_id: &str, route: ObservedMcpRoute) {
    if let Some(routes) = observed_mcp_routes()
        .lock()
        .expect("route observer")
        .get_mut(client_instance_id)
    {
        routes.push(route);
    }
}

async fn wait_for_mcp_routes(client_instance_id: &str, expected: &[ObservedMcpRoute]) {
    tokio::time::timeout(std::time::Duration::from_secs(20), async {
        loop {
            let observed = observed_mcp_routes()
                .lock()
                .expect("route observer")
                .get(client_instance_id)
                .cloned()
                .unwrap_or_default();
            if observed.len() >= expected.len() {
                assert_eq!(observed, expected);
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("production MCP route was not observed");
}

fn test_client_identity() -> DaemonClientIdentity {
    test_client_identity_for(PathBuf::from("/profiles/client"))
}

#[cfg(unix)]
#[test]
fn multi_root_git_generation_reads_each_explicit_root() {
    fn init(root: &std::path::Path) {
        let status = Command::new("git")
            .arg("init")
            .arg("--quiet")
            .arg(root)
            .status()
            .expect("git init");
        assert!(status.success());
    }

    let first = TempDir::new().expect("first root");
    let second = TempDir::new().expect("second root");
    init(first.path());
    init(second.path());
    std::fs::write(first.path().join("first.txt"), "first").expect("first source");
    std::fs::write(second.path().join("second.txt"), "second").expect("second source");

    let first_generation = explicit_git_state(first.path()).expect("first generation");
    let second_generation = explicit_git_state(second.path()).expect("second generation");
    assert_ne!(first_generation, second_generation);

    std::fs::write(first.path().join("third.txt"), "third").expect("changed first source");
    assert_ne!(
        explicit_git_state(first.path()).expect("updated first generation"),
        first_generation
    );
    assert_eq!(
        explicit_git_state(second.path()).expect("stable second generation"),
        second_generation
    );
}

#[test]
fn multi_root_families_refuse_cross_family_fallback() {
    use crate::application_surface::ApplicationSurfaceOperation;
    use tracedecay_application::MultiRootOperationV1;

    let git = MultiRootOperationV1::Git { request: json!({}) };
    assert!(multi_root_family_allows(
        &git,
        ApplicationSurfaceOperation::GitStatus
    ));
    assert!(!multi_root_family_allows(
        &git,
        ApplicationSurfaceOperation::CodePhraseSearch
    ));
    let query = MultiRootOperationV1::Query { request: json!({}) };
    assert!(multi_root_family_allows(
        &query,
        ApplicationSurfaceOperation::CodePhraseSearch
    ));
    assert!(!multi_root_family_allows(
        &query,
        ApplicationSurfaceOperation::GitStatus
    ));
}

fn test_client_identity_for(profile_root: PathBuf) -> DaemonClientIdentity {
    DaemonClientIdentity {
        global_db_path: profile_root.join("global.db"),
        profile_root,
    }
}

fn prepare_test_profile_root(profile_root: &std::path::Path) {
    std::fs::create_dir_all(profile_root).expect("create test profile root");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(profile_root, std::fs::Permissions::from_mode(0o700))
            .expect("secure test profile root");
    }
}

#[test]
fn daemon_test_transcript_source_home_is_profile_parent() {
    let isolated_home = TempDir::new().expect("isolated home");
    let profile_root = isolated_home.path().join("profile");

    assert_eq!(
        super::daemon_transcript_source_home(&profile_root).as_deref(),
        Some(isolated_home.path())
    );
}

fn test_store_administration_for_profile(profile_root: &std::path::Path) -> StoreAdministration {
    prepare_test_profile_root(profile_root);
    let profile_identity = crate::daemon::profile_identity::load_or_create(profile_root)
        .expect("load test profile identity");
    StoreAdministration::default().with_profile_identity(profile_identity)
}

#[cfg(unix)]
fn test_daemon_engine_for_profile(profile_root: &std::path::Path) -> DaemonEngine {
    prepare_test_profile_root(profile_root);
    let profile_identity = crate::daemon::profile_identity::load_or_create(profile_root)
        .expect("load test profile identity");
    DaemonEngine::default().with_profile_identity(profile_identity)
}

fn enter_test_daemon_database_scope(
    profile_root: &std::path::Path,
    label: &str,
) -> crate::db::DaemonDatabaseScope {
    crate::db::enter_daemon_database_scope(profile_root, 1, label)
        .expect("enter test daemon database scope")
}

async fn initialize_test_project(
    project_root: &std::path::Path,
    client_identity: &DaemonClientIdentity,
) -> crate::storage::StoreLayout {
    prepare_test_profile_root(&client_identity.profile_root);
    let lifecycle = crate::lifecycle_lease::acquire_exclusive_for_profile(
        &client_identity.profile_root,
        "daemon test fixture initialization",
    )
    .expect("acquire fixture lifecycle authority");
    let _database_scope = crate::db::enter_maintenance_database_scope(
        &lifecycle,
        &client_identity.profile_root,
        "daemon test fixture initialization",
    )
    .expect("enter fixture maintenance database scope");
    let project = crate::tracedecay::TraceDecay::init_with_exclusive_maintenance(
        project_root,
        crate::tracedecay::TraceDecayOpenOptions {
            profile_root: Some(client_identity.profile_root.clone()),
            global_db_path: Some(client_identity.global_db_path.clone()),
        },
        &lifecycle,
    )
    .await
    .expect("initialize project");
    let store_layout = project.store_layout().clone();
    project.close();
    store_layout
}

fn test_handshake_defaults() -> DaemonHandshake {
    DaemonHandshake {
        project_path: None,
        scope_prefix: None,
        timings: false,
        allow_init: false,
        allow_initialize_root_routing: false,
        client_identity: test_client_identity(),
        client_version: super::binary_version().to_string(),
        client_instance_id: crate::runtime_identity::process_run_id().to_string(),
        tool_list_changed_capable: false,
        catalog_version: String::new(),
    }
}

#[test]
fn search_request_controls_distinguish_cancellation_and_timeout() {
    let cancellation =
        tracedecay_application::CancellationSignal::active("cancellation.search-test")
            .expect("cancellation");
    let deadline =
        tracedecay_application::Deadline::new(tracedecay_domain::UtcMicros(10)).expect("deadline");

    assert_eq!(
        super::mcp_search_request_termination(Some(&deadline), Some(&cancellation), 9),
        None
    );
    assert_eq!(
        super::mcp_search_request_termination(Some(&deadline), Some(&cancellation), 10),
        Some(code_search::CodeIndexSearchUnavailableReasonV1::TimedOut)
    );
    cancellation.cancel(tracedecay_domain::UtcMicros(8));
    assert_eq!(
        super::mcp_search_request_termination(Some(&deadline), Some(&cancellation), 10),
        Some(code_search::CodeIndexSearchUnavailableReasonV1::Cancelled)
    );
}

#[test]
fn search_scope_resolution_failure_is_authority_unavailable() {
    assert!(matches!(
        super::code_index_scope_unavailable(),
        code_search::CodeIndexSearchOutcomeV1::Unavailable(
            code_search::CodeIndexSearchUnavailableV1 {
                reason: code_search::CodeIndexSearchUnavailableReasonV1::AuthorityUnavailable,
                ..
            }
        )
    ));
}

#[test]
fn an_unservable_search_reports_every_lane_down() {
    let code_search::CodeIndexSearchOutcomeV1::Unavailable(unavailable) =
        super::code_index_scope_unavailable()
    else {
        panic!("an unresolvable scope has no servable lane");
    };

    assert!(
        !unavailable.coverage.any_servable(),
        "a typed failure must only be returned when no lane could serve"
    );
    assert_eq!(
        unavailable
            .coverage
            .degraded_or_fail(unavailable.reason)
            .unwrap_err(),
        code_search::CodeIndexSearchUnavailableReasonV1::AuthorityUnavailable,
        "all lanes down must fail fast with the typed reason, never block"
    );
}

#[cfg(unix)]
fn test_automation_scheduler_handle(task: JoinHandle<()>) -> AutomationSchedulerHandle {
    AutomationSchedulerHandle::for_test(task)
}

#[cfg(unix)]
async fn wait_for_automation_scheduler_state(
    engine: &DaemonEngine,
    deadline: tokio::time::Instant,
    description: &str,
    mut matches: impl FnMut(
        &std::collections::HashMap<ProjectServerKey, AutomationSchedulerHandle>,
    ) -> bool,
) {
    let message = format!("timed out waiting for {description}");
    tokio::time::timeout(remaining_test_budget(deadline, &message), async {
        loop {
            let changed = engine.automation_scheduler_state_changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            let schedulers = engine
                .store_administration
                .automation_schedulers()
                .lock()
                .await;
            if matches(&schedulers) {
                return;
            }
            drop(schedulers);
            changed.await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{message}"));
}

#[cfg(unix)]
async fn wait_for_finished_task(
    task: &JoinHandle<()>,
    deadline: tokio::time::Instant,
    description: &str,
) {
    let message = format!("timed out waiting for {description}");
    tokio::time::timeout(remaining_test_budget(deadline, &message), async {
        while !task.is_finished() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{message}"));
}

#[cfg(unix)]
fn remaining_test_budget(deadline: tokio::time::Instant, message: &str) -> std::time::Duration {
    let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
    assert!(!remaining.is_zero(), "{message}");
    remaining
}

#[cfg(unix)]
#[derive(Clone)]
struct NoncooperativeTaskRelease {
    state: Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>,
}

#[cfg(unix)]
impl NoncooperativeTaskRelease {
    fn release(&self) {
        let (released, changed) = &*self.state;
        *released.lock().unwrap_or_else(PoisonError::into_inner) = true;
        changed.notify_all();
    }
}

#[cfg(unix)]
impl Drop for NoncooperativeTaskRelease {
    fn drop(&mut self) {
        self.release();
    }
}

#[cfg(unix)]
fn spawn_noncooperative_test_task() -> (
    JoinHandle<()>,
    tokio::sync::oneshot::Receiver<()>,
    tokio::sync::oneshot::Receiver<()>,
    NoncooperativeTaskRelease,
) {
    let release = NoncooperativeTaskRelease {
        state: Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new())),
    };
    let task_release = release.clone();
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let (completed_tx, completed_rx) = tokio::sync::oneshot::channel();
    let task = tokio::task::spawn_blocking(move || {
        let _ = started_tx.send(());
        let (released, changed) = &*task_release.state;
        let mut ready = released.lock().unwrap_or_else(PoisonError::into_inner);
        while !*ready {
            ready = changed.wait(ready).unwrap_or_else(PoisonError::into_inner);
        }
        let _ = completed_tx.send(());
    });
    (task, started_rx, completed_rx, release)
}

#[cfg(unix)]
fn scheduled_automation_patch(enabled: bool) -> crate::automation::config::AutomationConfigPatch {
    crate::automation::config::AutomationConfigPatch {
        enabled: Some(enabled),
        backend: Some(crate::automation::config::AutomationBackend::CodexAppServer),
        memory_curator: crate::automation::config::AutomationTaskPatch {
            enabled: Some(true),
            schedule: Some(Some("every:5m".to_string())),
            ..crate::automation::config::AutomationTaskPatch::default()
        },
        ..crate::automation::config::AutomationConfigPatch::default()
    }
}

#[cfg(unix)]
async fn save_scheduled_automation(dashboard_root: &std::path::Path, enabled: bool) {
    crate::automation::config::save_project_config(
        dashboard_root,
        &scheduled_automation_patch(enabled),
    )
    .await
    .expect("save scheduled automation config");
}
