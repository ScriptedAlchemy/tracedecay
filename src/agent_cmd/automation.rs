use std::path::{Path, PathBuf};

use tracedecay::automation::config::{
    AutomationBackend, AutomationConfigPatch, AutomationHostMode, AutomationTaskPatch,
    apply_project_config_patch, load_project_config, project_config_path,
};

/// How `install --agent codex --automation` should configure the daemon loop.
#[derive(Debug, Clone, Copy)]
pub(crate) struct CodexAutomationInstall {
    /// Apply accepted memory-curation ops without dashboard approval
    /// (`--auto-apply`).
    pub(crate) auto_apply: bool,
}

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
    options: CodexAutomationInstall,
) -> tracedecay::errors::Result<PathBuf> {
    let auto_apply = options.auto_apply;
    let dashboard_root = open_or_init_codex_daemon_automation_project(project_path).await?;
    let patch = AutomationConfigPatch {
        enabled: Some(true),
        backend: Some(AutomationBackend::CodexAppServer),
        host_mode: Some(AutomationHostMode::Standalone),
        // Unattended memory-op apply is opt-in: without --auto-apply these
        // stays unset, and re-running the installer never weakens stricter
        // settings a user already chose.
        auto_apply_memory_ops: auto_apply.then_some(true),
        memory_curator: codex_daemon_interval_task(15 * 60),
        session_reflector: codex_daemon_interval_task(15 * 60),
        skill_writer: AutomationTaskPatch {
            min_idle_secs: Some(Some(15 * 60)),
            ..codex_daemon_interval_task(60 * 60)
        },
        ..AutomationConfigPatch::default()
    };

    let global = tracedecay::user_config::UserConfig::load().automation;
    let current = load_project_config(&dashboard_root).await?;
    let (updated, _) = apply_project_config_patch(&dashboard_root, &global, patch).await?;
    if crate::automation_cli::config::automation_config_changed(current.as_ref(), &updated) {
        crate::automation_cli::config::notify_project_automation_scheduler(project_path).await?;
    }
    let path = project_config_path(&dashboard_root);
    eprintln!(
        "\x1b[32m✔\x1b[0m Enabled TraceDecay daemon automation loop at {}",
        path.display()
    );
    eprintln!(
        "  The daemon scheduler will run memory_curator, session_reflector, and skill_writer via the Codex app-server backend."
    );
    if auto_apply {
        eprintln!(
            "\x1b[33m⚠\x1b[0m --auto-apply: accepted memory-curation ops (deletes and merges) will be applied without dashboard approval. There is no archive; removals are permanent."
        );
    }
    if !tracedecay::daemon::daemon_reachable() {
        eprintln!(
            "\x1b[33m⚠\x1b[0m The TraceDecay daemon is not running, so the automation loop will stay idle. Enable it with `tracedecay daemon install-service`."
        );
    }
    Ok(path)
}

async fn open_or_init_codex_daemon_automation_project(
    project_path: &Path,
) -> tracedecay::errors::Result<PathBuf> {
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
        |project_path| {
            tracedecay::storage::resolve_layout_for_current_profile(project_path)
                .map(|layout| layout.dashboard_root)
        },
    )
    .await
}

pub(super) async fn broker_codex_daemon_automation_project<I, IFut, R>(
    project_path: &Path,
    initialize: I,
    resolve_dashboard_root: R,
) -> tracedecay::errors::Result<PathBuf>
where
    I: FnOnce(tracedecay::daemon::DaemonHandshake) -> IFut,
    IFut: std::future::Future<Output = tracedecay::errors::Result<()>>,
    R: FnOnce(&Path) -> tracedecay::errors::Result<PathBuf>,
{
    let handshake = tracedecay::daemon::DaemonHandshake::for_current_client(
        Some(project_path.to_path_buf()),
        None,
        false,
        true,
    )?;
    initialize(handshake).await?;
    resolve_dashboard_root(project_path)
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
