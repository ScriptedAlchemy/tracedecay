use crate::cli::{AutomationConfigAction, AutomationConfigScope};
use crate::resolve_cli_project_root;

pub(crate) fn project_automation_reconcile_args() -> serde_json::Value {
    serde_json::json!({
        "action": "automation_reconcile",
        "scope": "project"
    })
}

pub(crate) async fn notify_project_automation_scheduler(
    project_path: &std::path::Path,
) -> tracedecay::errors::Result<()> {
    crate::commands::daemon_tool_json(
        Some(project_path),
        "tracedecay_admin_project",
        project_automation_reconcile_args(),
    )
    .await
    .map(|_| ())
}

pub(super) async fn handle_automation_config_command(
    action: AutomationConfigAction,
) -> tracedecay::errors::Result<()> {
    use tracedecay_agent_hosts::automation::config::{AutomationBackend, AutomationConfigPatch};

    let path = match &action {
        AutomationConfigAction::Get { path, .. }
        | AutomationConfigAction::Explain { path, .. }
        | AutomationConfigAction::Enable { path, .. }
        | AutomationConfigAction::Disable { path, .. }
        | AutomationConfigAction::Set { path, .. } => path.clone(),
    };
    let scope = match &action {
        AutomationConfigAction::Get { scope, .. }
        | AutomationConfigAction::Explain { scope, .. }
        | AutomationConfigAction::Enable { scope, .. }
        | AutomationConfigAction::Disable { scope, .. }
        | AutomationConfigAction::Set { scope, .. } => *scope,
    };
    if scope != AutomationConfigScope::Project {
        return Err(config_error(
            "automation settings are project-scoped in the V2 configuration control plane; use --scope project",
        ));
    }

    let requested = resolve_cli_project_root(path, None, None).await?;
    let resolved = crate::commands::resolve_project_scope(requested).await?;
    let current = load_canonical_automation_config(&resolved.project_path).await?;
    let patch = match action {
        AutomationConfigAction::Get { json, .. } => {
            print_automation_config(&current, json, false)?;
            return Ok(());
        }
        AutomationConfigAction::Explain { json, .. } => {
            print_automation_config(&current, json, true)?;
            return Ok(());
        }
        AutomationConfigAction::Enable { .. } => AutomationConfigPatch {
            enabled: Some(true),
            backend: Some(AutomationBackend::CodexAppServer),
            model_id: Some(Some("gpt-5.6-mini".to_owned())),
            ..AutomationConfigPatch::default()
        },
        AutomationConfigAction::Disable { .. } => AutomationConfigPatch {
            enabled: Some(false),
            ..AutomationConfigPatch::default()
        },
        AutomationConfigAction::Set {
            backend,
            host_mode,
            timeout_secs,
            scheduler_tick_secs,
            memory_curator,
            memory_curator_schedule,
            memory_curator_interval_secs,
            memory_curator_cooldown_secs,
            memory_curator_min_idle_secs,
            memory_curator_stale_lock_secs,
            session_reflector,
            session_reflector_schedule,
            session_reflector_interval_secs,
            session_reflector_cooldown_secs,
            session_reflector_min_idle_secs,
            session_reflector_stale_lock_secs,
            skill_writer,
            skill_writer_schedule,
            skill_writer_interval_secs,
            skill_writer_cooldown_secs,
            skill_writer_min_idle_secs,
            skill_writer_stale_lock_secs,
            ..
        } => {
            let backend = backend
                .as_deref()
                .map(parse_automation_backend)
                .transpose()?;
            AutomationConfigPatch {
                backend,
                host_mode: host_mode
                    .as_deref()
                    .map(parse_automation_host_mode)
                    .transpose()?,
                model_id: (backend == Some(AutomationBackend::CodexAppServer)
                    && current.model_id.is_none())
                .then(|| Some("gpt-5.6-mini".to_owned())),
                timeout_secs,
                scheduler_tick_secs,
                memory_curator: automation_task_patch(
                    memory_curator,
                    memory_curator_schedule,
                    memory_curator_interval_secs,
                    memory_curator_cooldown_secs,
                    memory_curator_min_idle_secs,
                    memory_curator_stale_lock_secs,
                    "memory_curator",
                )?,
                session_reflector: automation_task_patch(
                    session_reflector,
                    session_reflector_schedule,
                    session_reflector_interval_secs,
                    session_reflector_cooldown_secs,
                    session_reflector_min_idle_secs,
                    session_reflector_stale_lock_secs,
                    "session_reflector",
                )?,
                skill_writer: automation_task_patch(
                    skill_writer,
                    skill_writer_schedule,
                    skill_writer_interval_secs,
                    skill_writer_cooldown_secs,
                    skill_writer_min_idle_secs,
                    skill_writer_stale_lock_secs,
                    "skill_writer",
                )?,
                ..AutomationConfigPatch::default()
            }
        }
    };

    let effective = apply_project_automation_patch(&resolved.project_path, patch).await?;
    print_automation_config(&effective, true, false)
}

pub(crate) async fn load_canonical_automation_config(
    project_path: &std::path::Path,
) -> tracedecay::errors::Result<tracedecay_agent_hosts::automation::config::AutomationConfig> {
    match crate::commands::current_project_setting(
        project_path,
        tracedecay_domain::configuration::AUTOMATION_SETTINGS_SETTING_KEY,
    )
    .await?
    {
        tracedecay_domain::configuration::ConfigurationValueV1::AutomationSettings(config) => {
            tracedecay_agent_hosts::automation::config::validate_config(&config)?;
            Ok(config)
        }
        _ => Err(config_error(
            "automation setting has the wrong canonical value kind",
        )),
    }
}

pub(crate) async fn apply_project_automation_patch(
    project_path: &std::path::Path,
    patch: tracedecay_agent_hosts::automation::config::AutomationConfigPatch,
) -> tracedecay::errors::Result<tracedecay_agent_hosts::automation::config::AutomationConfig> {
    let resolved = crate::commands::resolve_project_scope(project_path.to_path_buf()).await?;
    let current = load_canonical_automation_config(&resolved.project_path).await?;
    let effective =
        tracedecay_agent_hosts::automation::config::effective_config(&current, Some(&patch))?;
    if effective != current {
        let expected_revision =
            crate::commands::current_configuration_revision(&resolved.project_path).await?;
        let mutation = crate::commands::project_configuration_set(
            &resolved.project_id,
            tracedecay_domain::configuration::AUTOMATION_SETTINGS_SETTING_KEY,
            tracedecay_domain::configuration::ConfigurationValueV1::AutomationSettings(
                effective.clone(),
            ),
        )?;
        let receipt = crate::commands::mutate_project_configuration(
            &resolved.project_path,
            &resolved.project_id,
            expected_revision,
            vec![mutation],
        )
        .await?;
        crate::commands::report_configuration_receipt(receipt.as_ref());
        notify_project_automation_scheduler(&resolved.project_path).await?;
    }
    Ok(effective)
}

fn automation_task_patch(
    enabled: Option<bool>,
    schedule: Option<String>,
    interval_secs: Option<String>,
    cooldown_secs: Option<String>,
    min_idle_secs: Option<String>,
    stale_lock_secs: Option<String>,
    task: &str,
) -> tracedecay::errors::Result<tracedecay_agent_hosts::automation::config::AutomationTaskPatch> {
    Ok(
        tracedecay_agent_hosts::automation::config::AutomationTaskPatch {
            enabled,
            schedule: schedule.map(empty_string_or_none_clears),
            interval_secs: parse_optional_u64(interval_secs, &format!("{task} interval_secs"))?,
            cooldown_secs: parse_optional_u64(cooldown_secs, &format!("{task} cooldown_secs"))?,
            min_idle_secs: parse_optional_u64(min_idle_secs, &format!("{task} min_idle_secs"))?,
            stale_lock_secs: parse_optional_u64(
                stale_lock_secs,
                &format!("{task} stale_lock_secs"),
            )?,
            // The budget-backoff window is patched through the configuration
            // surfaces (dashboard/application PATCH); the CLI does not expose
            // a flag for it.
            session_evidence_budget_backoff_secs: None,
        },
    )
}

fn empty_string_or_none_clears(value: String) -> Option<String> {
    (!string_clears_optional(&value)).then_some(value)
}

fn string_clears_optional(value: &str) -> bool {
    value.is_empty() || value.eq_ignore_ascii_case("none")
}

fn parse_optional_u64(
    value: Option<String>,
    field: &str,
) -> tracedecay::errors::Result<Option<Option<u64>>> {
    let Some(value) = value else {
        return Ok(None);
    };
    if string_clears_optional(&value) {
        return Ok(Some(None));
    }
    value
        .parse::<u64>()
        .map(|value| Some(Some(value)))
        .map_err(|error| {
            config_error(format!(
                "invalid automation config value for {field}: {error}"
            ))
        })
}

fn print_automation_config(
    effective: &tracedecay_agent_hosts::automation::config::AutomationConfig,
    json: bool,
    explain: bool,
) -> tracedecay::errors::Result<()> {
    let availability = tracedecay_agent_hosts::automation::backend::backend_availability(effective);
    let trace_decay_backend_calls = effective.enabled
        && effective.backend
            == tracedecay_agent_hosts::automation::config::AutomationBackend::CodexAppServer
        && effective.host_mode
            == tracedecay_agent_hosts::automation::config::AutomationHostMode::Standalone;
    let payload = serde_json::json!({
        "source": "daemon_pinned_snapshot",
        "effective": effective,
        "backend_availability": availability,
        "explanation": {
            "trace_decay_backend_calls": trace_decay_backend_calls,
            "delegated_host": effective.host_mode
                == tracedecay_agent_hosts::automation::config::AutomationHostMode::DelegatedHost,
            "automatic_memory_apply": true,
            "automatic_skill_activation": true,
        },
    });
    if json {
        println!("{}", serde_json::to_string_pretty(&payload)?);
    } else {
        println!("enabled: {}", effective.enabled);
        println!("backend: {:?}", effective.backend);
        println!("host_mode: {:?}", effective.host_mode);
        println!("backend_available: {}", availability.available);
        if let Some(executable) = availability.executable.as_deref() {
            println!("backend_executable: {executable}");
        }
        if let Some(reason) = availability.reason.as_deref() {
            println!("backend_reason: {reason}");
        }
        println!(
            "model_id: {}",
            effective.model_id.as_deref().unwrap_or("disabled")
        );
        println!("timeout_secs: {}", effective.timeout_secs);
        println!("scheduler_tick_secs: {}", effective.scheduler_tick_secs);
        println!("memory_curator: {}", effective.tasks.memory_curator.enabled);
        if explain {
            println!("source: daemon_pinned_snapshot");
            println!("trace_decay_backend_calls: {trace_decay_backend_calls}");
            println!("automatic_memory_apply: true");
            println!("automatic_skill_activation: true");
        }
    }
    Ok(())
}

fn parse_automation_backend(
    value: &str,
) -> tracedecay::errors::Result<tracedecay_agent_hosts::automation::config::AutomationBackend> {
    use tracedecay_agent_hosts::automation::config::AutomationBackend;
    match value {
        "disabled" => Ok(AutomationBackend::Disabled),
        "codex-app-server" | "codex_app_server" => Ok(AutomationBackend::CodexAppServer),
        _ => Err(config_error(format!(
            "unknown automation backend '{value}' (expected disabled, codex-app-server)"
        ))),
    }
}

fn parse_automation_host_mode(
    value: &str,
) -> tracedecay::errors::Result<tracedecay_agent_hosts::automation::config::AutomationHostMode> {
    use tracedecay_agent_hosts::automation::config::AutomationHostMode;
    match value {
        "standalone" => Ok(AutomationHostMode::Standalone),
        "delegated-host" | "delegated_host" => Ok(AutomationHostMode::DelegatedHost),
        _ => Err(config_error(format!(
            "unknown automation host mode '{value}' (expected standalone, delegated-host)"
        ))),
    }
}

fn config_error(message: impl Into<String>) -> tracedecay::errors::TraceDecayError {
    tracedecay::errors::TraceDecayError::Config {
        message: message.into(),
    }
}
