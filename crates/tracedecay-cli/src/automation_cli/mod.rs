pub(crate) mod config;
mod facts;
mod runs;
mod skills;

use crate::cli::AutomationAction;
use tracedecay_runtime_core::config::ProfileRoot;

async fn daemon_project_dashboard_root(
    profile: &ProfileRoot,
    project_path: &std::path::Path,
) -> tracedecay_domain::errors::Result<std::path::PathBuf> {
    let context = crate::commands::daemon_tool_json(
        profile,
        Some(project_path),
        "tracedecay_active_project",
        serde_json::json!({ "format": "json" }),
    )
    .await?;
    let data_root = context
        .get("storage")
        .and_then(|storage| storage.get("data_root"))
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| tracedecay_domain::errors::TraceDecayError::Config {
            message: "managed daemon returned no active project data_root".to_string(),
        })?;
    Ok(std::path::PathBuf::from(data_root).join("dashboard"))
}

pub(crate) async fn handle_automation_command(
    profile: &ProfileRoot,
    action: AutomationAction,
) -> tracedecay_domain::errors::Result<()> {
    match action {
        AutomationAction::Config { action } => {
            hotpath::future!(
                config::handle_automation_config_command(profile, action),
                label = "cli.automation.config"
            )
            .await
        }
        AutomationAction::Runs { action } => {
            hotpath::future!(
                runs::handle_automation_runs_command(profile, action),
                label = "cli.automation.runs"
            )
            .await
        }
        AutomationAction::Skills { action } => {
            hotpath::future!(
                skills::handle_automation_skills_command(profile, action),
                label = "cli.automation.skills"
            )
            .await
        }
        AutomationAction::Facts { action } => {
            hotpath::future!(
                facts::handle_automation_facts_command(profile, action),
                label = "cli.automation.facts"
            )
            .await
        }
    }
}
