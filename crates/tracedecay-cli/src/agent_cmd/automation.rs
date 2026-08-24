use std::path::{Path, PathBuf};

use tracedecay_agent_hosts::automation::config::{
    AutomationBackend, AutomationConfigPatch, AutomationHostMode, AutomationTaskPatch,
};

/// How `install --agent codex --automation` should configure the daemon loop.
#[derive(Debug, Clone, Copy)]
pub(crate) struct CodexAutomationInstall;

pub(super) fn validate_codex_automation_flags(
    agent: Option<&str>,
    automation: Option<CodexAutomationInstall>,
) -> tracedecay::errors::Result<()> {
    if automation.is_none() {
        return Ok(());
    }
    if agent != Some("codex") {
        return Err(tracedecay::errors::TraceDecayError::Config {
            message: "`--automation` is only supported with `--agent codex`".to_string(),
        });
    }
    Ok(())
}

pub(super) fn validate_codex_automation_project_path() -> tracedecay::errors::Result<PathBuf> {
    let project_path =
        std::env::current_dir().map_err(|e| tracedecay::errors::TraceDecayError::Config {
            message: format!("could not determine current project directory: {e}"),
        })?;
    std::fs::canonicalize(&project_path).map_err(|e| tracedecay::errors::TraceDecayError::Config {
        message: format!(
            "could not canonicalize project directory {}: {e}",
            project_path.display()
        ),
    })
}

pub(super) async fn install_codex_daemon_automation(
    project_path: &Path,
    _home: &Path,
    _options: CodexAutomationInstall,
) -> tracedecay::errors::Result<()> {
    let patch = AutomationConfigPatch {
        enabled: Some(true),
        backend: Some(AutomationBackend::CodexAppServer),
        host_mode: Some(AutomationHostMode::Standalone),
        model_id: Some(Some("gpt-5.6-mini".to_owned())),
        memory_curator: codex_daemon_interval_task(15 * 60),
        session_reflector: codex_daemon_interval_task(15 * 60),
        skill_writer: AutomationTaskPatch {
            min_idle_secs: Some(Some(15 * 60)),
            ..codex_daemon_interval_task(60 * 60)
        },
        ..AutomationConfigPatch::default()
    };

    initialize_codex_daemon_automation_project(project_path).await?;
    // This performs a read-CAS-write through the daemon's configuration
    // application boundary. It also leaves an unchanged setting as a no-op,
    // so rerunning install neither creates a sidecar nor advances a revision.
    crate::automation_cli::config::apply_project_automation_patch(project_path, patch).await?;
    eprintln!(
        "\x1b[32m✔\x1b[0m TraceDecay daemon automation is enabled in the daemon-managed project configuration."
    );
    eprintln!(
        "  The daemon scheduler will run memory_curator, session_reflector, and skill_writer via the Codex app-server backend."
    );
    Ok(())
}

async fn initialize_codex_daemon_automation_project(
    project_path: &Path,
) -> tracedecay::errors::Result<()> {
    broker_codex_daemon_automation_project(
        project_path,
        |handshake| async move {
            tracedecay::daemon::call_default_tool(
                &handshake,
                "tracedecay_admin_project",
                serde_json::json!({"action": "counter_get"}),
            )
            .await
            .map(|_| ())
        },
        |_| Ok(()),
    )
    .await
}

pub(super) async fn broker_codex_daemon_automation_project<I, IFut, R, T>(
    project_path: &Path,
    initialize: I,
    complete: R,
) -> tracedecay::errors::Result<T>
where
    I: FnOnce(tracedecay::daemon::DaemonHandshake) -> IFut,
    IFut: std::future::Future<Output = tracedecay::errors::Result<()>>,
    R: FnOnce(&Path) -> tracedecay::errors::Result<T>,
{
    let handshake = tracedecay::daemon::DaemonHandshake::for_current_client(
        Some(project_path.to_path_buf()),
        None,
        false,
        true,
    )?;
    initialize(handshake).await?;
    complete(project_path)
}

fn codex_daemon_interval_task(interval_secs: u64) -> AutomationTaskPatch {
    AutomationTaskPatch {
        enabled: Some(true),
        schedule: Some(Some("interval".to_string())),
        interval_secs: Some(Some(interval_secs)),
        cooldown_secs: Some(Some(5 * 60)),
        ..AutomationTaskPatch::default()
    }
}
