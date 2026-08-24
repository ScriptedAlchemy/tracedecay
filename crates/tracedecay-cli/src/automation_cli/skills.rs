use crate::cli::AutomationSkillsAction;

/// Managed-skill commands operate on the profile authority and then run the
/// same project-scoped deployment reconciliation used by automatic curation.
/// There is no separate operator export/install phase.
pub(super) async fn handle_automation_skills_command(
    action: AutomationSkillsAction,
) -> tracedecay::errors::Result<()> {
    use tracedecay_agent_hosts::automation::managed_skills::{
        ManagedSkillDraft, ManagedSkillProvenance, ManagedSkillSource, ManagedSkillUpdate,
        apply_managed_skill_update, archive_managed_skill, create_managed_skill,
        disable_managed_skill, list_managed_skills, load_managed_skill, restore_managed_skill,
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
        AutomationSkillsAction::Create {
            id,
            title,
            summary,
            category,
            body,
            pinned,
        } => {
            let skill = create_managed_skill(
                &profile_root,
                ManagedSkillDraft {
                    id,
                    title,
                    summary,
                    category,
                    targets: tracedecay_agent_hosts::automation::managed_skills::default_managed_skill_targets(
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
                tracedecay_agent_hosts::automation::managed_skills::set_managed_skill_pinned(
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
            let current = load_managed_skill(&profile_root, &id).await?;
            apply_managed_skill_update(
                &profile_root,
                &id,
                &current.metadata.checksum,
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
        AutomationSkillsAction::Disable { id } => disable_managed_skill(&profile_root, &id).await?,
        AutomationSkillsAction::Archive { id } => archive_managed_skill(&profile_root, &id).await?,
        AutomationSkillsAction::Restore { id } => restore_managed_skill(&profile_root, &id).await?,
    };
    let deployment = deploy_skills_to_current_project(&profile_root)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "skill": skill,
            "deployment": deployment,
        }))?
    );
    Ok(())
}

fn deploy_skills_to_current_project(
    profile_root: &std::path::Path,
) -> tracedecay::errors::Result<
    tracedecay_agent_hosts::automation::skill_writer::ManagedSkillDeploymentReceipt,
> {
    let current =
        std::env::current_dir().map_err(|error| tracedecay::errors::TraceDecayError::Config {
            message: format!("resolve current project for managed-skill deployment: {error}"),
        })?;
    let project_root =
        tracedecay_agent_hosts::automation::skill_materialization::resolve_project_root(&current);
    Ok(
        tracedecay_agent_hosts::automation::skill_writer::deploy_managed_skills_to_project(
            profile_root,
            &project_root,
        ),
    )
}

fn print_managed_skill(skill: &tracedecay_agent_hosts::automation::managed_skills::ManagedSkill) {
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
