use std::path::PathBuf;
#[cfg(unix)]
use std::process::Command;
use std::sync::Arc;

#[cfg(unix)]
use serde_json::Value;
#[cfg(unix)]
use serde_json::json;
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
#[cfg(unix)]
use tokio::task::JoinHandle;

#[cfg(unix)]
use super::scheduler::{AutomationSchedulerExitBarrier, AutomationSchedulerLifecycle};
#[cfg(unix)]
use super::{
    AutomationSchedulerHandle, DaemonEngine, MemoryRepairSchedulerHandle, drain_client_tasks,
};
use super::{
    DaemonClientIdentity, DaemonHandshake, DaemonLifecycle, DatabaseOwnerRegistry, ProjectRouteKey,
    ProjectServerKey, StoreAdministration, StoreOwnerKey,
};

mod bootstrap;
mod compatibility;
mod handshake;
mod lifecycle;
mod logging;
mod ownership;
mod replay;
mod restart_proxy;
mod scheduler_config;
mod scheduler_shutdown;
mod socket;

fn test_client_identity() -> DaemonClientIdentity {
    test_client_identity_for(PathBuf::from("/profiles/client"))
}

fn test_client_identity_for(profile_root: PathBuf) -> DaemonClientIdentity {
    DaemonClientIdentity {
        global_db_path: profile_root.join("global.db"),
        profile_root,
    }
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
    if remaining.is_zero() {
        panic!("{message}");
    }
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
        *released.lock().unwrap_or_else(|error| error.into_inner()) = true;
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
        let mut ready = released.lock().unwrap_or_else(|error| error.into_inner());
        while !*ready {
            ready = changed
                .wait(ready)
                .unwrap_or_else(|error| error.into_inner());
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
