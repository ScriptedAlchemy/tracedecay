pub(crate) mod config;
mod facts;
mod runs;
mod skills;

use crate::cli::AutomationAction;

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

pub(crate) async fn handle_automation_command(
    action: AutomationAction,
) -> tracedecay::errors::Result<()> {
    match action {
        AutomationAction::Config { action } => {
            config::handle_automation_config_command(action).await
        }
        AutomationAction::Runs { action } => runs::handle_automation_runs_command(action).await,
        AutomationAction::Skills { action } => {
            skills::handle_automation_skills_command(action).await
        }
        AutomationAction::Facts { action } => facts::handle_automation_facts_command(action).await,
    }
}

#[cfg(test)]
mod tests {
    use super::facts::{
        automatic_fact_receipt_list_rpc_args, automatic_fact_receipt_view_rpc_args,
    };

    #[test]
    fn automatic_fact_receipt_rpc_requests_preserve_arguments() {
        assert_eq!(
            automatic_fact_receipt_list_rpc_args(Some("applied"), 50),
            serde_json::json!({
                "action": "automatic_fact_receipt_list",
                "state": "applied",
                "limit": 50,
            })
        );
        assert_eq!(
            automatic_fact_receipt_view_rpc_args("fact_7"),
            serde_json::json!({ "action": "automatic_fact_receipt_view", "id": "fact_7" })
        );
    }

    #[test]
    fn automation_reconcile_request_binds_project_scope() {
        assert_eq!(
            super::config::project_automation_reconcile_args(),
            serde_json::json!({
                "action": "automation_reconcile",
                "scope": "project"
            })
        );
    }
}
