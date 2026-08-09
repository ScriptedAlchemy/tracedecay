use crate::cli::{AutomationSkillsAction, AutomationSkillsInstallTarget};
use crate::update_cmd::tracedecay_bin_on_path;

pub(super) async fn handle_automation_skills_command(
    action: AutomationSkillsAction,
) -> tracedecay::errors::Result<()> {
    use tracedecay::automation::managed_skills::{
        ManagedSkillDraft, ManagedSkillProvenance, ManagedSkillSource, ManagedSkillUpdate,
        archive_managed_skill, create_managed_skill, disable_managed_skill, list_managed_skills,
        load_managed_skill, restore_managed_skill, update_managed_skill,
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
        AutomationSkillsAction::Create {
            id,
            title,
            summary,
            category,
            body,
            pinned,
        } => {
            refresh_exports = true;
            let skill = create_managed_skill(
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
                        source: ManagedSkillSource::User,
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
            refresh_exports = true;
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
