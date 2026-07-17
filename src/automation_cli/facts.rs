use super::daemon_automation_action;
use crate::cli::AutomationFactsAction;
use crate::resolve_cli_project_root;

pub(super) fn fact_list_rpc_args(state: Option<&str>, limit: usize) -> serde_json::Value {
    serde_json::json!({ "action": "fact_list", "state": state, "limit": limit })
}

pub(super) fn fact_view_rpc_args(id: &str) -> serde_json::Value {
    serde_json::json!({ "action": "fact_view", "id": id })
}

pub(super) fn fact_apply_rpc_args(id: &str) -> serde_json::Value {
    serde_json::json!({ "action": "fact_apply", "id": id })
}

pub(super) fn fact_reject_rpc_args(id: &str, reason: Option<&str>) -> serde_json::Value {
    serde_json::json!({ "action": "fact_reject", "id": id, "reason": reason })
}

pub(super) async fn handle_automation_facts_command(
    action: AutomationFactsAction,
) -> tracedecay::errors::Result<()> {
    let path = match &action {
        AutomationFactsAction::List { path, .. }
        | AutomationFactsAction::View { path, .. }
        | AutomationFactsAction::Apply { path, .. }
        | AutomationFactsAction::Reject { path, .. } => path.clone(),
    };
    let project_path = resolve_cli_project_root(path, None, None).await?;
    let payload = match action {
        AutomationFactsAction::List { state, limit, .. } => {
            daemon_automation_action(&project_path, fact_list_rpc_args(state.as_deref(), limit))
                .await?
        }
        AutomationFactsAction::View { id, .. } => {
            daemon_automation_action(&project_path, fact_view_rpc_args(&id)).await?
        }
        AutomationFactsAction::Apply { id, .. } => {
            daemon_automation_action(&project_path, fact_apply_rpc_args(&id)).await?
        }
        AutomationFactsAction::Reject { id, reason, .. } => {
            daemon_automation_action(&project_path, fact_reject_rpc_args(&id, reason.as_deref()))
                .await?
        }
    };
    println!("{}", serde_json::to_string_pretty(&payload)?);
    Ok(())
}
