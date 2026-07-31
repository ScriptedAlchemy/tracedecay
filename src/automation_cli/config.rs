use super::daemon_project_dashboard_root;
use crate::cli::{AutomationConfigAction, AutomationConfigScope};
use crate::resolve_cli_project_root;

pub(crate) fn automation_config_changed(
    current: Option<&tracedecay::automation::config::AutomationConfigPatch>,
    updated: &tracedecay::automation::config::AutomationConfigPatch,
) -> bool {
    current != Some(updated)
}

pub(crate) fn project_automation_reconcile_args() -> serde_json::Value {
    serde_json::json!({
        "action": "automation_reconcile",
        "scope": "project"
    })
}

pub(crate) fn profile_automation_reconcile_args() -> serde_json::Value {
    serde_json::json!({
        "action": "automation_reconcile",
        "scope": "profile"
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

pub(crate) async fn notify_profile_automation_schedulers() -> tracedecay::errors::Result<()> {
    crate::commands::daemon_tool_json(
        None,
        "tracedecay_admin_project",
        profile_automation_reconcile_args(),
    )
    .await
    .map(|_| ())
}

pub(super) async fn handle_automation_config_command(
    action: AutomationConfigAction,
) -> tracedecay::errors::Result<()> {
    use tracedecay::automation::config::{
        AutomationBackend, AutomationConfigPatch, apply_project_config_patch, effective_config,
        load_project_config,
    };

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

    let mut user_config = tracedecay::user_config::UserConfig::load();
    let global = user_config.automation.clone();
    let project_context = if scope == AutomationConfigScope::Project {
        let project_path = resolve_cli_project_root(path.clone(), None, None).await?;
        let dashboard_root = daemon_project_dashboard_root(&project_path).await?;
        Some((
            project_path,
            dashboard_root.clone(),
            load_project_config(&dashboard_root).await?,
        ))
    } else {
        None
    };

    let patch = match action {
        AutomationConfigAction::Get { json, .. } => {
            let project = project_context
                .as_ref()
                .and_then(|(_, _, project)| project.as_ref());
            let effective = effective_config(&global, project)?;
            print_automation_config(&global, project, &effective, json, false)?;
            return Ok(());
        }
        AutomationConfigAction::Explain { json, .. } => {
            let project = project_context
                .as_ref()
                .and_then(|(_, _, project)| project.as_ref());
            let effective = effective_config(&global, project)?;
            print_automation_config(&global, project, &effective, json, true)?;
            return Ok(());
        }
        AutomationConfigAction::Enable { .. } => AutomationConfigPatch {
            enabled: Some(true),
            backend: Some(AutomationBackend::CodexAppServer),
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
            auto_apply_memory_ops,
            auto_enable_skills,
            export_memory_digest,
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
        } => AutomationConfigPatch {
            backend: backend
                .as_deref()
                .map(parse_automation_backend)
                .transpose()?,
            host_mode: host_mode
                .as_deref()
                .map(parse_automation_host_mode)
                .transpose()?,
            timeout_secs,
            scheduler_tick_secs,
            auto_apply_memory_ops,
            auto_enable_skills,
            export_memory_digest,
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
        },
    };

    if scope == AutomationConfigScope::Global {
        let effective = effective_config(&global, Some(&patch))?;
        let changed = effective != global;
        if !changed {
            return print_automation_config(&global, None, &effective, true, false);
        }
        user_config.automation = effective.clone();
        match user_config.save_with_recovery() {
            Ok(Some(backup)) => {
                eprintln!(
                    "note: the previous config.toml was corrupt and was backed up to {} before regenerating",
                    backup.display()
                );
            }
            Ok(None) => {}
            Err(err) => {
                return Err(tracedecay::errors::TraceDecayError::Config {
                    message: format!("failed to save global automation config: {err}"),
                });
            }
        }
        notify_profile_automation_schedulers().await?;
        return print_automation_config(&user_config.automation, None, &effective, true, false);
    }

    let (project_path, dashboard_root, current) =
        project_context.expect("project scope has project context");
    let (project, effective) = apply_project_config_patch(&dashboard_root, &global, patch).await?;
    if automation_config_changed(current.as_ref(), &project) {
        notify_project_automation_scheduler(&project_path).await?;
    }
    print_automation_config(&global, Some(&project), &effective, true, false)
}

fn automation_task_patch(
    enabled: Option<bool>,
    schedule: Option<String>,
    interval_secs: Option<String>,
    cooldown_secs: Option<String>,
    min_idle_secs: Option<String>,
    stale_lock_secs: Option<String>,
    task: &str,
) -> tracedecay::errors::Result<tracedecay::automation::config::AutomationTaskPatch> {
    Ok(tracedecay::automation::config::AutomationTaskPatch {
        enabled,
        schedule: schedule.map(empty_string_or_none_clears),
        interval_secs: parse_optional_u64(interval_secs, &format!("{task} interval_secs"))?,
        cooldown_secs: parse_optional_u64(cooldown_secs, &format!("{task} cooldown_secs"))?,
        min_idle_secs: parse_optional_u64(min_idle_secs, &format!("{task} min_idle_secs"))?,
        stale_lock_secs: parse_optional_u64(stale_lock_secs, &format!("{task} stale_lock_secs"))?,
    })
}

fn empty_string_or_none_clears(value: String) -> Option<String> {
    if string_clears_optional(&value) {
        None
    } else {
        Some(value)
    }
}

fn string_clears_optional(value: &str) -> bool {
    value.is_empty() || value.eq_ignore_ascii_case("none")
}

fn parse_optional_u64(
    value: Option<String>,
    field: &str,
) -> tracedecay::errors::Result<Option<Option<u64>>> {
    parse_optional_number(value, field, str::parse::<u64>)
}

fn parse_optional_number<T, E>(
    value: Option<String>,
    field: &str,
    parse: impl FnOnce(&str) -> std::result::Result<T, E>,
) -> tracedecay::errors::Result<Option<Option<T>>>
where
    E: std::fmt::Display,
{
    let Some(value) = value else {
        return Ok(None);
    };
    if string_clears_optional(&value) {
        return Ok(Some(None));
    }
    parse(&value)
        .map(Some)
        .map(Some)
        .map_err(|err| tracedecay::errors::TraceDecayError::Config {
            message: format!("invalid automation config value for {field}: {err}"),
        })
}

fn print_automation_config(
    global: &tracedecay::automation::config::AutomationConfig,
    project: Option<&tracedecay::automation::config::AutomationConfigPatch>,
    effective: &tracedecay::automation::config::AutomationConfig,
    json: bool,
    explain: bool,
) -> tracedecay::errors::Result<()> {
    let availability = tracedecay::automation::backend::backend_availability(effective);
    let source = if project.is_some() {
        "project"
    } else {
        "global"
    };
    let trace_decay_backend_calls = effective.enabled
        && matches!(
            effective.backend,
            tracedecay::automation::config::AutomationBackend::CodexAppServer
        )
        && effective.host_mode == tracedecay::automation::config::AutomationHostMode::Standalone;
    let delegated_host =
        effective.host_mode == tracedecay::automation::config::AutomationHostMode::DelegatedHost;
    // `require_dashboard_approval` is deprecated, but the legacy memory flag
    // remains an explicit apply/proposal-only switch.
    let memory_ops_policy = if effective.auto_apply_memory_ops {
        "validate_then_apply"
    } else {
        "validate_then_propose"
    };
    let apply_mode = if effective.auto_apply_memory_ops {
        "autonomous"
    } else {
        "proposal_only"
    };
    let skills_policy = if effective.auto_enable_skills {
        "auto_enable"
    } else {
        "draft_for_approval"
    };
    let payload = serde_json::json!({
        "global": global,
        "project": project,
        "effective": effective,
        "backend_availability": availability,
        "explanation": {
            "source": source,
            "trace_decay_backend_calls": trace_decay_backend_calls,
            "delegated_host": delegated_host,
            "auto_apply_memory_ops": effective.auto_apply_memory_ops,
            "auto_apply_memory_ops_legacy_config_only": false,
            "auto_enable_skills": effective.auto_enable_skills,
            "export_memory_digest": effective.export_memory_digest,
            "effective_apply_policy": {
                "mode": apply_mode,
                "human_approval_required": !effective.auto_apply_memory_ops,
                "dashboard_approval": "deprecated",
                "memory_ops": memory_ops_policy,
                "skills": skills_policy,
            },
        },
    });
    if json {
        println!("{}", serde_json::to_string_pretty(&payload)?);
    } else {
        println!("enabled: {}", effective.enabled);
        println!("backend: {:?}", effective.backend);
        println!("host_mode: {:?}", effective.host_mode);
        if explain {
            println!("source: {source}");
            println!("trace_decay_backend_calls: {trace_decay_backend_calls}");
            println!("delegated_host: {delegated_host}");
        }
        println!("backend_available: {}", availability.available);
        if let Some(executable) = availability.executable.as_deref() {
            println!("backend_executable: {executable}");
        }
        if let Some(reason) = availability.reason.as_deref() {
            println!("backend_reason: {reason}");
        }
        println!("model: auto");
        println!("timeout_secs: {}", effective.timeout_secs);
        println!("scheduler_tick_secs: {}", effective.scheduler_tick_secs);
        println!("memory_curator: {}", effective.tasks.memory_curator.enabled);
        println!("effective_apply_policy: {apply_mode}");
        if explain {
            println!(
                "session_reflector: {}",
                effective.tasks.session_reflector.enabled
            );
            println!("skill_writer: {}", effective.tasks.skill_writer.enabled);
            println!(
                "auto_apply_memory_ops: {} (legacy-compatible apply gate)",
                effective.auto_apply_memory_ops
            );
            println!("auto_enable_skills: {}", effective.auto_enable_skills);
            println!("export_memory_digest: {}", effective.export_memory_digest);
            println!(
                "apply_policy.human_approval_required: {}",
                !effective.auto_apply_memory_ops
            );
            println!("apply_policy.dashboard_approval: deprecated");
            println!("apply_policy.memory_ops: {memory_ops_policy}");
            println!("apply_policy.skills: {skills_policy}");
        }
    }
    Ok(())
}

fn parse_automation_backend(
    value: &str,
) -> tracedecay::errors::Result<tracedecay::automation::config::AutomationBackend> {
    use tracedecay::automation::config::AutomationBackend;
    match value {
        "disabled" => Ok(AutomationBackend::Disabled),
        "codex-app-server" | "codex_app_server" => Ok(AutomationBackend::CodexAppServer),
        _ => Err(tracedecay::errors::TraceDecayError::Config {
            message: format!(
                "unknown automation backend '{value}' (expected disabled, codex-app-server)"
            ),
        }),
    }
}

fn parse_automation_host_mode(
    value: &str,
) -> tracedecay::errors::Result<tracedecay::automation::config::AutomationHostMode> {
    use tracedecay::automation::config::AutomationHostMode;
    match value {
        "standalone" => Ok(AutomationHostMode::Standalone),
        "delegated-host" | "delegated_host" | "hermes-hosted" | "hermes_hosted" => {
            Ok(AutomationHostMode::DelegatedHost)
        }
        _ => Err(tracedecay::errors::TraceDecayError::Config {
            message: format!(
                "unknown automation host mode '{value}' (expected standalone, delegated-host)"
            ),
        }),
    }
}
