use crate::cli::*;
use crate::{parse_lcm_scope_arg, resolve_cli_project_root, tracedecay_bin_on_path};

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
    let cg = crate::serve::ensure_initialized(&project_path).await?;
    let dashboard_root = cg.store_layout().dashboard_root.clone();

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
        apply_fact_proposal, list_fact_proposals, load_fact_proposal, reject_fact_proposal,
        FactProposalState,
    };

    let path = match &action {
        AutomationFactsAction::List { path, .. }
        | AutomationFactsAction::View { path, .. }
        | AutomationFactsAction::Apply { path, .. }
        | AutomationFactsAction::Reject { path, .. } => path.clone(),
    };
    let project_path = resolve_cli_project_root(path, None, None).await?;
    let cg = crate::serve::ensure_initialized(&project_path).await?;
    let dashboard_root = cg.store_layout().dashboard_root.clone();
    let payload = match action {
        AutomationFactsAction::List { state, limit, .. } => {
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
            let proposal = load_fact_proposal(&dashboard_root, &id)
                .await?
                .ok_or_else(|| tracedecay::errors::TraceDecayError::Config {
                    message: format!("fact proposal not found: {id}"),
                })?;
            serde_json::json!({ "proposal": proposal })
        }
        AutomationFactsAction::Apply { id, .. } => {
            let proposal = apply_fact_proposal(
                &dashboard_root,
                cg.db().conn(),
                &id,
                Some("cli".to_string()),
            )
            .await?;
            serde_json::json!({ "proposal": proposal })
        }
        AutomationFactsAction::Reject { id, reason, .. } => {
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
        approve_managed_skill, archive_managed_skill, create_managed_skill_draft,
        disable_managed_skill, list_managed_skills, load_managed_skill, restore_managed_skill,
        update_managed_skill, ManagedSkillDraft, ManagedSkillProvenance, ManagedSkillSource,
        ManagedSkillUpdate,
    };

    let profile_root = tracedecay::storage::default_profile_root()?;
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
            let mut skill = create_managed_skill_draft(
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
                skill.set_pinned(true);
                tracedecay::automation::managed_skills::save_managed_skill(&profile_root, &skill)
                    .await?;
            }
            skill
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
        AutomationSkillsAction::Approve { id } => approve_managed_skill(&profile_root, &id).await?,
        AutomationSkillsAction::Disable { id } => disable_managed_skill(&profile_root, &id).await?,
        AutomationSkillsAction::Archive { id } => archive_managed_skill(&profile_root, &id).await?,
        AutomationSkillsAction::Restore { id } => restore_managed_skill(&profile_root, &id).await?,
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
                tracedecay::automation::skill_targets::install_managed_skills(
                    &profile_root,
                    target.into(),
                    output,
                )?
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
    println!("{}", serde_json::to_string_pretty(&skill)?);
    Ok(())
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
    use tracedecay::automation::backend::CodexAppServerBackend;
    use tracedecay::automation::config::{
        effective_config, load_project_config, AutomationBackend,
    };
    use tracedecay::automation::runner::{
        run_memory_curator_with_backend, run_session_reflector_with_backend,
        run_skill_writer_with_backend, MemoryCuratorAutomationOptions,
        SessionReflectorAutomationOptions, SkillWriterAutomationOptions,
    };

    match action {
        AutomationRunAction::MemoryCuration {
            dry_run,
            max_clusters,
            min_confidence,
            path,
        } => {
            if !dry_run {
                return Err(tracedecay::errors::TraceDecayError::Config {
                    message: "automation run memory-curation only supports --dry-run true"
                        .to_string(),
                });
            }
            let project_path = resolve_cli_project_root(path, None, None).await?;
            let cg = crate::serve::ensure_initialized(&project_path).await?;
            let dashboard_root = cg.store_layout().dashboard_root.clone();
            let global = tracedecay::user_config::UserConfig::load().automation;
            let project = load_project_config(&dashboard_root).await?;
            let effective = effective_config(&global, project.as_ref())?;
            if effective.enabled && effective.backend == AutomationBackend::ExternalCommand {
                return Err(tracedecay::errors::TraceDecayError::Config {
                    message: "automation backend external_command is not implemented yet"
                        .to_string(),
                });
            }
            let backend = CodexAppServerBackend::from_automation_config(&effective);
            let run = run_memory_curator_with_backend(
                &cg,
                &effective,
                &backend,
                MemoryCuratorAutomationOptions {
                    trigger: tracedecay::automation::run_ledger::AutomationTrigger::ManualCli,
                    run_id: None,
                    max_clusters,
                    min_confidence,
                },
            )
            .await?;
            println!("{}", serde_json::to_string_pretty(&run)?);
        }
        AutomationRunAction::SessionReflection {
            dry_run,
            provider,
            query,
            evidence_limit,
            storage_scope,
            hermes_home,
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
            if !dry_run {
                return Err(tracedecay::errors::TraceDecayError::Config {
                    message: "automation run session-reflection only supports --dry-run true"
                        .to_string(),
                });
            }
            let project_path = resolve_cli_project_root(path, None, None).await?;
            let cg = crate::serve::ensure_initialized(&project_path).await?;
            let dashboard_root = cg.store_layout().dashboard_root.clone();
            let global = tracedecay::user_config::UserConfig::load().automation;
            let project = load_project_config(&dashboard_root).await?;
            let effective = effective_config(&global, project.as_ref())?;
            if effective.enabled && effective.backend == AutomationBackend::ExternalCommand {
                return Err(tracedecay::errors::TraceDecayError::Config {
                    message: "automation backend external_command is not implemented yet"
                        .to_string(),
                });
            }
            let backend = CodexAppServerBackend::from_automation_config(&effective);
            let lcm_scope = parse_lcm_scope_arg(&scope)?;
            let lcm_sort = sort
                .parse::<tracedecay::sessions::lcm::LcmGrepSort>()
                .map_err(|()| tracedecay::errors::TraceDecayError::Config {
                    message: format!(
                        "invalid session-reflection --sort '{sort}'; expected recency, relevance, or hybrid"
                    ),
                })?;
            let run = run_session_reflector_with_backend(
                &cg,
                &effective,
                &backend,
                SessionReflectorAutomationOptions {
                    trigger: tracedecay::automation::run_ledger::AutomationTrigger::ManualCli,
                    run_id: None,
                    storage_scope,
                    hermes_home,
                    provider,
                    query,
                    scope: lcm_scope,
                    session_id,
                    include_summaries,
                    evidence_limit,
                    sort: lcm_sort,
                    source,
                    role,
                    start_time,
                    end_time,
                },
            )
            .await?;
            println!("{}", serde_json::to_string_pretty(&run)?);
        }
        AutomationRunAction::SkillWriting {
            dry_run,
            provider,
            query,
            evidence_limit,
            storage_scope,
            hermes_home,
            path,
        } => {
            if !dry_run {
                return Err(tracedecay::errors::TraceDecayError::Config {
                    message: "automation run skill-writing only supports --dry-run true"
                        .to_string(),
                });
            }
            let project_path = resolve_cli_project_root(path, None, None).await?;
            let cg = crate::serve::ensure_initialized(&project_path).await?;
            let dashboard_root = cg.store_layout().dashboard_root.clone();
            let global = tracedecay::user_config::UserConfig::load().automation;
            let project = load_project_config(&dashboard_root).await?;
            let effective = effective_config(&global, project.as_ref())?;
            if effective.enabled && effective.backend == AutomationBackend::ExternalCommand {
                return Err(tracedecay::errors::TraceDecayError::Config {
                    message: "automation backend external_command is not implemented yet"
                        .to_string(),
                });
            }
            let backend = CodexAppServerBackend::from_automation_config(&effective);
            let run = run_skill_writer_with_backend(
                &cg,
                &effective,
                &backend,
                SkillWriterAutomationOptions {
                    trigger: tracedecay::automation::run_ledger::AutomationTrigger::ManualCli,
                    run_id: None,
                    storage_scope,
                    hermes_home,
                    provider,
                    query,
                    evidence_limit,
                    profile_root: None,
                },
            )
            .await?;
            println!("{}", serde_json::to_string_pretty(&run)?);
        }
    }
    Ok(())
}

async fn handle_automation_config_command(
    action: AutomationConfigAction,
) -> tracedecay::errors::Result<()> {
    use tracedecay::automation::config::{
        effective_config, load_project_config, merge_project_config, save_project_config,
        AutomationBackend, AutomationConfigPatch,
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
        let cg = crate::serve::ensure_initialized(&project_path).await?;
        Some((
            cg.store_layout().dashboard_root.clone(),
            load_project_config(&cg.store_layout().dashboard_root).await?,
        ))
    } else {
        None
    };

    let updated = match action {
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
        AutomationConfigAction::Enable { .. } => merge_project_config(
            project_context
                .as_ref()
                .and_then(|(_, project)| project.clone()),
            AutomationConfigPatch {
                enabled: Some(true),
                backend: Some(AutomationBackend::CodexAppServer),
                ..AutomationConfigPatch::default()
            },
        ),
        AutomationConfigAction::Disable { .. } => merge_project_config(
            project_context
                .as_ref()
                .and_then(|(_, project)| project.clone()),
            AutomationConfigPatch {
                enabled: Some(false),
                ..AutomationConfigPatch::default()
            },
        ),
        AutomationConfigAction::Set {
            backend,
            host_mode,
            model,
            timeout_secs,
            scheduler_tick_secs,
            max_tokens,
            temperature,
            require_dashboard_approval,
            auto_apply_memory_ops,
            auto_enable_skills,
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
        } => merge_project_config(
            project_context
                .as_ref()
                .and_then(|(_, project)| project.clone()),
            AutomationConfigPatch {
                backend: backend
                    .as_deref()
                    .map(parse_automation_backend)
                    .transpose()?,
                host_mode: host_mode
                    .as_deref()
                    .map(parse_automation_host_mode)
                    .transpose()?,
                model: model.map(empty_string_or_none_clears),
                timeout_secs,
                scheduler_tick_secs,
                max_tokens: parse_optional_u32(max_tokens, "max_tokens")?,
                temperature: parse_optional_f32(temperature, "temperature")?,
                require_dashboard_approval,
                auto_apply_memory_ops,
                auto_enable_skills,
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
        ),
    };

    if scope == AutomationConfigScope::Global {
        let effective = effective_config(&global, Some(&updated))?;
        user_config.automation = effective.clone();
        if !user_config.save() {
            return Err(tracedecay::errors::TraceDecayError::Config {
                message: "failed to save global automation config".to_string(),
            });
        }
        return print_automation_config(&user_config.automation, None, &effective, true, false);
    }

    let (dashboard_root, _) = project_context.expect("project scope has project context");
    let effective = effective_config(&global, Some(&updated))?;
    save_project_config(&dashboard_root, &updated).await?;
    print_automation_config(&global, Some(&updated), &effective, true, false)
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

fn parse_optional_u32(
    value: Option<String>,
    field: &str,
) -> tracedecay::errors::Result<Option<Option<u32>>> {
    parse_optional_number(value, field, str::parse::<u32>)
}

fn parse_optional_f32(
    value: Option<String>,
    field: &str,
) -> tracedecay::errors::Result<Option<Option<f32>>> {
    parse_optional_number(value, field, str::parse::<f32>)
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
    let payload = serde_json::json!({
        "global": global,
        "project": project,
        "effective": effective,
        "backend_availability": availability,
        "explanation": {
            "source": source,
            "trace_decay_backend_calls": trace_decay_backend_calls,
            "delegated_host": delegated_host,
            "approval_required": effective.require_dashboard_approval,
            "auto_apply_memory_ops": effective.auto_apply_memory_ops,
            "auto_enable_skills": effective.auto_enable_skills,
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
        println!(
            "model: {}",
            effective.model.as_deref().unwrap_or("<provider default>")
        );
        println!("timeout_secs: {}", effective.timeout_secs);
        println!("scheduler_tick_secs: {}", effective.scheduler_tick_secs);
        println!("memory_curator: {}", effective.tasks.memory_curator.enabled);
        if explain {
            println!(
                "session_reflector: {}",
                effective.tasks.session_reflector.enabled
            );
            println!("skill_writer: {}", effective.tasks.skill_writer.enabled);
            println!(
                "require_dashboard_approval: {}",
                effective.require_dashboard_approval
            );
            println!("auto_apply_memory_ops: {}", effective.auto_apply_memory_ops);
            println!("auto_enable_skills: {}", effective.auto_enable_skills);
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
