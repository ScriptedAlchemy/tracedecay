//! Physical daemon lifecycle and mounted-application readiness for the journey.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;
use tempfile::TempDir;
use tracedecay::storage::PrivateStoreIo;
use tracedecay_application::WorkAttemptListRequestV1;
use tracedecay_application::configuration::{
    ComponentConfigurationState, ConfigurationObservedStateRequestV1,
};
use tracedecay_sdk::client::{Client, ClientError, ConnectionMode};
use tracedecay_sdk::operations::{ApplicationConfigurationObservedState, WorkListAttempts};

use super::{common, wait_until};

pub(super) fn sdk_client(home: &Path, project_id: &str) -> Client {
    let authority = read_daemon_authority(home);
    let endpoint = authority["http_application_endpoint"]
        .as_str()
        .expect("HTTP application endpoint");
    let token = authority["auth_token"].as_str().expect("daemon token");
    let base = format!("http://{endpoint}");
    Client::builder(ConnectionMode::local(&base, project_id, token))
        .origin(&base)
        .timeout(Duration::from_secs(30))
        .build()
        .expect("canonical SDK client")
}

pub(super) fn wait_for_application_mount(client: &Client) -> Vec<ComponentConfigurationState> {
    wait_until("project application mount", || match client
        .execute::<ApplicationConfigurationObservedState>(&ConfigurationObservedStateRequestV1 {})
    {
        Ok(response) => Some(response.result),
        Err(ClientError::Problem(problem)) if problem.kind == "not_found_or_not_authorized" => None,
        Err(error) => panic!("project application mount failed: {error}"),
    })
}

pub(super) fn wait_for_work_mount(client: &Client) {
    wait_until("project Work runtime mount", || {
        match client.execute::<WorkListAttempts>(&WorkAttemptListRequestV1 {
            page_size: 100,
            cursor: None,
        }) {
            Ok(_) => Some(()),
            Err(ClientError::Problem(problem))
                if problem.kind == "not_found_or_not_authorized"
                    || problem.kind == "unavailable" =>
            {
                None
            }
            Err(error) => panic!("project Work runtime mount failed: {error}"),
        }
    });
}

pub(super) fn spawn_project_daemon(home: &Path, project: &Path) -> common::DaemonProcess {
    let profile = home.join(".tracedecay");
    PrivateStoreIo::create_dir_all(&profile).expect("advanced workflow daemon profile");
    let log_path = std::env::var_os("TRACEDECAY_TEST_DAEMON_LOG")
        .map(PathBuf::from)
        .unwrap_or_else(|| profile.join("advanced-workflow-daemon.log"));
    let stderr = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
        .expect("advanced workflow daemon log");
    let mut command = Command::new(env!("CARGO_BIN_EXE_tracedecay"));
    common::apply_tracedecay_home_env(&mut command, home);
    let child = command
        .args(["daemon", "run"])
        .env("TRACEDECAY_TEST_ALLOW_INCOMPLETE_HOLDER_SCAN", "1")
        .current_dir(project)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(stderr))
        .spawn()
        .expect("advanced workflow daemon should start");
    let mut daemon = common::DaemonProcess::new(child);
    let daemon_pid = u64::from(daemon.id());
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        if let Some(status) = daemon.try_wait().expect("daemon status") {
            panic!("advanced workflow daemon exited during startup: {status}");
        }
        let ready = std::fs::read(common::daemon_authority_path(&profile))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .is_some_and(|authority| {
                authority["pid"].as_u64() == Some(daemon_pid)
                    && authority["http_application_endpoint"].as_str().is_some()
                    && authority["auth_token"].as_str().is_some()
            });
        if ready {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for the new daemon application authority"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    daemon
}

pub(super) fn workflow_tempdir() -> TempDir {
    #[cfg(unix)]
    {
        return tempfile::Builder::new()
            .prefix("tracedecay-advanced-workflow-")
            .tempdir_in("/tmp")
            .expect("advanced workflow isolation");
    }
    #[cfg(not(unix))]
    TempDir::new().expect("advanced workflow isolation")
}

fn read_daemon_authority(home: &Path) -> Value {
    wait_until("published daemon HTTP application authority", || {
        let bytes = std::fs::read(common::daemon_authority_path(&home.join(".tracedecay"))).ok()?;
        let authority = serde_json::from_slice::<Value>(&bytes).ok()?;
        authority["http_application_endpoint"].as_str()?;
        authority["auth_token"].as_str()?;
        Some(authority)
    })
}
