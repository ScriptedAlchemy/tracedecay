use crate::cli::*;
use crate::parse_lcm_scope_arg;
use crate::resolve_cli_project_root;
use crate::update_cmd::tracedecay_bin_on_path;

async fn daemon_project_dashboard_root(
    project_path: &std::path::Path,
) -> tracedecay::errors::Result<std::path::PathBuf> {
    let context = crate::commands::daemon_tool_json(
        Some(project_path),
        "tracedecay_active_project",
        serde_json::json!({ "format": "json" }),
    )
    .await?;
    let data_root = context
        .get("storage")
        .and_then(|storage| storage.get("data_root"))
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| tracedecay::errors::TraceDecayError::Config {
            message: "managed daemon returned no active project data_root".to_string(),
        })?;
    Ok(std::path::PathBuf::from(data_root).join("dashboard"))
}

async fn daemon_automation_action(
    project_path: &std::path::Path,
    args: serde_json::Value,
) -> tracedecay::errors::Result<serde_json::Value> {
    crate::commands::daemon_tool_json(Some(project_path), "tracedecay_admin_project", args).await
}

fn fact_apply_rpc_args(id: &str) -> serde_json::Value {
    serde_json::json!({ "action": "fact_apply", "id": id })
}

fn automation_run_rpc_request(
    action: AutomationRunAction,
) -> tracedecay::errors::Result<(Option<String>, serde_json::Value)> {
    let request = match action {
        AutomationRunAction::MemoryCuration {
            max_clusters,
            min_confidence,
            path,
        } => (
            path,
            serde_json::json!({
                "action": "automation_run",
                "task": "memory_curation",
                "options": {
                    "max_clusters": max_clusters,
                    "min_confidence": min_confidence,
                },
            }),
        ),
        AutomationRunAction::SessionReflection {
            provider,
            query,
            evidence_limit,
            scope,
            session_id,
            include_summaries,
            sort,
            source,
            role,
            start_time,
            end_time,
            path,
        } => {
            parse_lcm_scope_arg(&scope)?;
            sort.parse::<tracedecay::sessions::lcm::LcmGrepSort>()
                .map_err(|()| tracedecay::errors::TraceDecayError::Config {
                    message: format!(
                        "invalid session-reflection --sort '{sort}'; expected recency, relevance, or hybrid"
                    ),
                })?;
            (
                path,
                serde_json::json!({
                    "action": "automation_run",
                    "task": "session_reflection",
                    "options": {
                        "provider": provider,
                        "query": query,
                        "evidence_limit": evidence_limit,
                        "scope": scope,
                        "session_id": session_id,
                        "include_summaries": include_summaries,
                        "sort": sort,
                        "source": source,
                        "role": role,
                        "start_time": start_time,
                        "end_time": end_time,
                    },
                }),
            )
        }
        AutomationRunAction::SkillWriting {
            provider,
            query,
            evidence_limit,
            path,
        } => (
            path,
            serde_json::json!({
                "action": "automation_run",
                "task": "skill_writing",
                "options": {
                    "provider": provider,
                    "query": query,
                    "evidence_limit": evidence_limit,
                },
            }),
        ),
    };
    Ok(request)
}

fn automation_run_result(
    payload: &serde_json::Value,
) -> tracedecay::errors::Result<&serde_json::Value> {
    payload
        .get("run")
        .ok_or_else(|| tracedecay::errors::TraceDecayError::Config {
            message: "daemon automation response omitted run".to_string(),
        })
}

pub(crate) async fn handle_automation_command(
    action: AutomationAction,
) -> tracedecay::errors::Result<()> {
    match action {
        AutomationAction::Config { action } => handle_automation_config_command(action).await,
        AutomationAction::Run { action } => handle_automation_run_command(action).await,
        AutomationAction::Runs { action } => handle_automation_runs_command(action).await,
        AutomationAction::Skills { action } => handle_automation_skills_command(action).await,
        AutomationAction::Facts { action } => handle_automation_facts_command(action).await,
    }
}

async fn handle_automation_runs_command(
    action: AutomationRunsAction,
) -> tracedecay::errors::Result<()> {
    use tracedecay::automation::run_ledger::{
        find_run_record, load_run_records, read_run_artifact_payload,
    };

    let path = match &action {
        AutomationRunsAction::List { path, .. }
        | AutomationRunsAction::View { path, .. }
        | AutomationRunsAction::Artifact { path, .. } => path.clone(),
    };
    let project_path = resolve_cli_project_root(path, None, None).await?;
    let dashboard_root = daemon_project_dashboard_root(&project_path).await?;

    match action {
        AutomationRunsAction::List { limit, json, .. } => {
            let limit = limit.min(200);
            let records = load_run_records(&dashboard_root, limit).await?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "dashboard_root": dashboard_root,
                        "count": records.len(),
                        "limit": limit,
                        "records": records,
                    }))?
                );
            } else {
                print_automation_run_list(&records);
            }
        }
        AutomationRunsAction::View { run_id, json, .. } => {
            let record = find_run_record(&dashboard_root, &run_id)
                .await?
                .ok_or_else(|| tracedecay::errors::TraceDecayError::Config {
                    message: format!("automation run not found: {run_id}"),
                })?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "dashboard_root": dashboard_root,
                        "record": record,
                    }))?
                );
            } else {
                print_automation_run_record(&record);
            }
        }
        AutomationRunsAction::Artifact {
            run_id, kind, json, ..
        } => {
            let record = find_run_record(&dashboard_root, &run_id)
                .await?
                .ok_or_else(|| tracedecay::errors::TraceDecayError::Config {
                    message: format!("automation run not found: {run_id}"),
                })?;
            let artifact = record
                .artifacts
                .iter()
                .find(|artifact| artifact.kind == kind)
                .ok_or_else(|| tracedecay::errors::TraceDecayError::Config {
                    message: format!("automation run artifact not found: {run_id}/{kind}"),
                })?;
            let payload =
                read_run_artifact_payload(&dashboard_root, &record.run_id, artifact).await?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "dashboard_root": dashboard_root,
                        "run_id": record.run_id,
                        "artifact": artifact,
                        "payload": payload,
                    }))?
                );
            } else {
                print_automation_run_artifact(&record.run_id, artifact, &payload)?;
            }
        }
    }
    Ok(())
}

fn print_automation_run_list(
    records: &[tracedecay::automation::run_ledger::AutomationRunLedgerRecord],
) {
    if records.is_empty() {
        println!("No automation runs.");
        return;
    }
    println!("RUN ID\tSTATUS\tTASK\tTRIGGER\tACCEPTED\tREJECTED\tCOMPLETED\tERROR");
    for record in records {
        println!(
            "{}\t{}\t{}\t{:?}\t{}\t{}\t{}\t{}",
            record.run_id,
            record.status.as_str(),
            record
                .task_key
                .as_deref()
                .unwrap_or_else(|| tracedecay::automation::backend::task_key(record.task)),
            record.trigger,
            record.accepted_count,
            record.rejected_count,
            record.completed_at,
            record.error.as_deref().unwrap_or("")
        );
    }
}

fn print_automation_run_record(
    record: &tracedecay::automation::run_ledger::AutomationRunLedgerRecord,
) {
    println!("run_id: {}", record.run_id);
    println!("status: {}", record.status.as_str());
    println!(
        "task: {}",
        record
            .task_key
            .as_deref()
            .unwrap_or_else(|| tracedecay::automation::backend::task_key(record.task))
    );
    println!("trigger: {:?}", record.trigger);
    println!("backend: {}", record.backend);
    if let Some(model) = record.model.as_deref() {
        println!("model: {model}");
    }
    println!("accepted_count: {}", record.accepted_count);
    println!("rejected_count: {}", record.rejected_count);
    println!("reviewed_count: {}", record.reviewed_count);
    if let Some(error) = record.error.as_deref() {
        println!("error: {error}");
    }
    if !record.artifacts.is_empty() {
        println!("artifacts:");
        for artifact in &record.artifacts {
            println!(
                "- {}\t{}\t{}",
                artifact.kind,
                artifact.path,
                artifact.summary.as_deref().unwrap_or("")
            );
        }
    }
}

fn print_automation_run_artifact(
    run_id: &str,
    artifact: &tracedecay::automation::run_ledger::AutomationRunArtifact,
    payload: &serde_json::Value,
) -> tracedecay::errors::Result<()> {
    println!("run_id: {run_id}");
    println!("artifact: {}", artifact.kind);
    println!("path: {}", artifact.path);
    if let Some(summary) = artifact.summary.as_deref() {
        println!("summary: {summary}");
    }
    println!("{}", serde_json::to_string_pretty(payload)?);
    Ok(())
}

async fn handle_automation_facts_command(
    action: AutomationFactsAction,
) -> tracedecay::errors::Result<()> {
    use tracedecay::automation::fact_proposals::{
        FactProposalState, list_fact_proposals, load_fact_proposal, reject_fact_proposal,
    };

    let path = match &action {
        AutomationFactsAction::List { path, .. }
        | AutomationFactsAction::View { path, .. }
        | AutomationFactsAction::Apply { path, .. }
        | AutomationFactsAction::Reject { path, .. } => path.clone(),
    };
    let project_path = resolve_cli_project_root(path, None, None).await?;
    let payload = match action {
        AutomationFactsAction::List { state, limit, .. } => {
            let dashboard_root = daemon_project_dashboard_root(&project_path).await?;
            let state = match state {
                Some(value) => Some(FactProposalState::parse(&value)?),
                None => None,
            };
            let proposals = list_fact_proposals(&dashboard_root, state, limit).await?;
            serde_json::json!({
                "dashboard_root": dashboard_root,
                "count": proposals.len(),
                "proposals": proposals,
            })
        }
        AutomationFactsAction::View { id, .. } => {
            let dashboard_root = daemon_project_dashboard_root(&project_path).await?;
            let proposal = load_fact_proposal(&dashboard_root, &id)
                .await?
                .ok_or_else(|| tracedecay::errors::TraceDecayError::Config {
                    message: format!("fact proposal not found: {id}"),
                })?;
            serde_json::json!({ "proposal": proposal })
        }
        AutomationFactsAction::Apply { id, .. } => {
            daemon_automation_action(&project_path, fact_apply_rpc_args(&id)).await?
        }
        AutomationFactsAction::Reject { id, reason, .. } => {
            let dashboard_root = daemon_project_dashboard_root(&project_path).await?;
            let proposal =
                reject_fact_proposal(&dashboard_root, &id, Some("cli".to_string()), reason).await?;
            serde_json::json!({ "proposal": proposal })
        }
    };
    println!("{}", serde_json::to_string_pretty(&payload)?);
    Ok(())
}

async fn handle_automation_skills_command(
    action: AutomationSkillsAction,
) -> tracedecay::errors::Result<()> {
    use tracedecay::automation::managed_skills::{
        ManagedSkillDraft, ManagedSkillProvenance, ManagedSkillSource, ManagedSkillUpdate,
        approve_managed_skill, archive_managed_skill, create_managed_skill_draft,
        disable_managed_skill, list_managed_skills, load_managed_skill, restore_managed_skill,
        update_managed_skill,
    };

    let profile_root = tracedecay::storage::default_profile_root()?;
    let mut refresh_exports = false;
    let skill = match action {
        AutomationSkillsAction::List { json } => {
            let skills = list_managed_skills(&profile_root).await?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "profile_root": profile_root,
                        "count": skills.len(),
                        "skills": skills,
                    }))?
                );
            } else if skills.is_empty() {
                println!("No managed skills.");
            } else {
                for skill in skills {
                    println!(
                        "{}\t{:?}\t{}",
                        skill.metadata.id, skill.metadata.state, skill.metadata.title
                    );
                }
            }
            return Ok(());
        }
        AutomationSkillsAction::View { id, json } => {
            let skill = load_managed_skill(&profile_root, &id).await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&skill)?);
            } else {
                print_managed_skill(&skill);
            }
            return Ok(());
        }
        AutomationSkillsAction::Draft {
            id,
            title,
            summary,
            category,
            body,
            pinned,
        } => {
            let skill = create_managed_skill_draft(
                &profile_root,
                ManagedSkillDraft {
                    id,
                    title,
                    summary,
                    category,
                    targets: tracedecay::automation::managed_skills::default_managed_skill_targets(
                    ),
                    body_markdown: body,
                    support_files: Vec::new(),
                    provenance: ManagedSkillProvenance {
                        source: ManagedSkillSource::UserDraft,
                        actor: "cli".to_string(),
                        run_id: None,
                    },
                },
            )
            .await?;
            if pinned {
                tracedecay::automation::managed_skills::set_managed_skill_pinned(
                    &profile_root,
                    &skill.metadata.id,
                    true,
                )
                .await?
            } else {
                skill
            }
        }
        AutomationSkillsAction::Update {
            id,
            title,
            summary,
            category,
            body,
            pinned,
        } => {
            update_managed_skill(
                &profile_root,
                &id,
                ManagedSkillUpdate {
                    title,
                    summary,
                    category,
                    body_markdown: body,
                    pinned,
                    ..ManagedSkillUpdate::default()
                },
            )
            .await?
        }
        AutomationSkillsAction::Approve { id } => {
            refresh_exports = true;
            approve_managed_skill(&profile_root, &id).await?
        }
        AutomationSkillsAction::Disable { id } => {
            refresh_exports = true;
            disable_managed_skill(&profile_root, &id).await?
        }
        AutomationSkillsAction::Archive { id } => {
            refresh_exports = true;
            archive_managed_skill(&profile_root, &id).await?
        }
        AutomationSkillsAction::Restore { id } => {
            refresh_exports = true;
            restore_managed_skill(&profile_root, &id).await?
        }
        AutomationSkillsAction::Install {
            target,
            output,
            plugin_artifact,
            json,
        } => {
            let output = std::path::Path::new(&output);
            let summary = if plugin_artifact {
                if target != AutomationSkillsInstallTarget::Codex {
                    return Err(tracedecay::errors::TraceDecayError::Config {
                        message:
                            "--plugin-artifact is currently supported only with --target codex"
                                .to_string(),
                    });
                }
                let tracedecay_bin = tracedecay_bin_on_path()?;
                tracedecay::agents::codex::export_codex_plugin_artifact(
                    &profile_root,
                    output,
                    &tracedecay_bin,
                )?
            } else {
                let summary = tracedecay::automation::skill_targets::install_managed_skills(
                    &profile_root,
                    target.into(),
                    output,
                )?;
                // The shareable Codex plugin artifact intentionally omits the
                // memory digest (personal memory must not ship in a bundle);
                // direct host installs export it alongside the skills.
                tracedecay::automation::memory_digest::sync_memory_digest_export(
                    &profile_root,
                    target.into(),
                    output,
                )?;
                summary
            };
            if json {
                println!("{}", serde_json::to_string_pretty(&summary)?);
            } else {
                println!(
                    "Exported {} managed skill(s) to {}",
                    summary.exported_count,
                    summary.output.display()
                );
            }
            return Ok(());
        }
    };
    if refresh_exports {
        refresh_managed_skill_exports_for_cli(&profile_root);
    }
    println!("{}", serde_json::to_string_pretty(&skill)?);
    Ok(())
}

fn refresh_managed_skill_exports_for_cli(profile_root: &std::path::Path) {
    let Some(home) = tracedecay::agents::home_dir() else {
        return;
    };
    let start = std::env::current_dir().unwrap_or_else(|_| home.clone());
    let project_root = tracedecay::automation::skill_materialization::resolve_project_root(&start);
    for report in
        tracedecay::agents::export_managed_skills_to_agent_hosts(&home, &project_root, profile_root)
    {
        if let Some(error) = report.error {
            eprintln!(
                "warning: failed to refresh managed skill exports for {}: {}",
                report.agent, error
            );
        }
    }
    // Materialize active managed skills as real, host-loadable SKILL.md files
    // into every detected `.claude`/`.codex` skills directory (project + global).
    tracedecay::automation::skill_materialization::reconcile_after_activation(
        profile_root,
        &project_root,
    );
}

fn print_managed_skill(skill: &tracedecay::automation::managed_skills::ManagedSkill) {
    println!("id: {}", skill.metadata.id);
    println!("title: {}", skill.metadata.title);
    println!("summary: {}", skill.metadata.summary);
    println!("category: {}", skill.metadata.category);
    println!("state: {:?}", skill.metadata.state);
    println!("pinned: {}", skill.metadata.pinned);
    println!("checksum: {}", skill.metadata.checksum);
    println!();
    println!("{}", skill.body_markdown);
}

async fn handle_automation_run_command(
    action: AutomationRunAction,
) -> tracedecay::errors::Result<()> {
    let (path, args) = automation_run_rpc_request(action)?;
    let project_path = resolve_cli_project_root(path, None, None).await?;
    let payload = daemon_automation_action(&project_path, args).await?;
    let run = automation_run_result(&payload)?;
    println!("{}", serde_json::to_string_pretty(run)?);
    Ok(())
}

async fn handle_automation_config_command(
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
        let project_path = resolve_cli_project_root(path, None, None).await?;
        let dashboard_root = daemon_project_dashboard_root(&project_path).await?;
        Some((
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
                .and_then(|(_, project)| project.as_ref());
            let effective = effective_config(&global, project)?;
            print_automation_config(&global, project, &effective, json, false)?;
            return Ok(());
        }
        AutomationConfigAction::Explain { json, .. } => {
            let project = project_context
                .as_ref()
                .and_then(|(_, project)| project.as_ref());
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
        return print_automation_config(&user_config.automation, None, &effective, true, false);
    }

    let (dashboard_root, _) = project_context.expect("project scope has project context");
    let (project, effective) = apply_project_config_patch(&dashboard_root, &global, patch).await?;
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
    // Automation applies validated memory output autonomously;
    // `require_dashboard_approval` and `auto_apply_memory_ops` are retained
    // only for legacy config compatibility and do not gate curation runs.
    let memory_ops_policy = "validate_then_apply";
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
            "auto_apply_memory_ops_legacy_config_only": true,
            "auto_enable_skills": effective.auto_enable_skills,
            "export_memory_digest": effective.export_memory_digest,
            "effective_apply_policy": {
                "mode": "autonomous",
                "human_approval_required": false,
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
        println!("effective_apply_policy: autonomous");
        if explain {
            println!(
                "session_reflector: {}",
                effective.tasks.session_reflector.enabled
            );
            println!("skill_writer: {}", effective.tasks.skill_writer.enabled);
            println!(
                "auto_apply_memory_ops: {} (legacy; autonomous curation always applies)",
                effective.auto_apply_memory_ops
            );
            println!("auto_enable_skills: {}", effective.auto_enable_skills);
            println!("export_memory_digest: {}", effective.export_memory_digest);
            println!("apply_policy.human_approval_required: false");
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn automation_rpc_requests_preserve_fact_and_manual_run_arguments() {
        assert_eq!(
            fact_apply_rpc_args("fact-7"),
            serde_json::json!({ "action": "fact_apply", "id": "fact-7" })
        );

        let (path, request) = automation_run_rpc_request(AutomationRunAction::MemoryCuration {
            max_clusters: 9,
            min_confidence: 0.7,
            path: Some("/repo".to_string()),
        })
        .unwrap();
        assert_eq!(path.as_deref(), Some("/repo"));
        assert_eq!(
            request,
            serde_json::json!({
                "action": "automation_run",
                "task": "memory_curation",
                "options": { "max_clusters": 9, "min_confidence": 0.7 },
            })
        );

        let (path, request) = automation_run_rpc_request(AutomationRunAction::SessionReflection {
            provider: "claude".to_string(),
            query: "decisions".to_string(),
            evidence_limit: 11,
            scope: "session".to_string(),
            session_id: Some("session-3".to_string()),
            include_summaries: false,
            sort: "hybrid".to_string(),
            source: Some("assistant".to_string()),
            role: Some("user".to_string()),
            start_time: Some(10),
            end_time: Some(20),
            path: None,
        })
        .unwrap();
        assert_eq!(path, None);
        assert_eq!(
            request,
            serde_json::json!({
                "action": "automation_run",
                "task": "session_reflection",
                "options": {
                    "provider": "claude",
                    "query": "decisions",
                    "evidence_limit": 11,
                    "scope": "session",
                    "session_id": "session-3",
                    "include_summaries": false,
                    "sort": "hybrid",
                    "source": "assistant",
                    "role": "user",
                    "start_time": 10,
                    "end_time": 20,
                },
            })
        );

        let (_, request) = automation_run_rpc_request(AutomationRunAction::SkillWriting {
            provider: "all".to_string(),
            query: "repeated workflow".to_string(),
            evidence_limit: 13,
            path: None,
        })
        .unwrap();
        assert_eq!(
            request,
            serde_json::json!({
                "action": "automation_run",
                "task": "skill_writing",
                "options": {
                    "provider": "all",
                    "query": "repeated workflow",
                    "evidence_limit": 13,
                },
            })
        );
    }

    #[test]
    fn automation_rpc_preserves_response_and_has_no_local_database_fallback() {
        let payload = serde_json::json!({ "run": { "run_id": "run-5", "status": "ok" } });
        assert_eq!(automation_run_result(&payload).unwrap(), &payload["run"]);
        assert!(automation_run_result(&serde_json::json!({})).is_err());

        let source = include_str!("automation_cli.rs");
        let direct_init = ["serve::ensure_", "initialized"].concat();
        let direct_apply = ["apply_fact_", "proposal("].concat();
        assert!(!source.contains(&direct_init));
        assert!(!source.contains(&direct_apply));
        assert!(source.contains("tracedecay_admin_project"));
    }
}
