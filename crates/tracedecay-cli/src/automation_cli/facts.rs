use super::daemon_automation_action;
use crate::cli::AutomationFactsAction;
use crate::resolve_cli_project_root;

pub(super) fn automatic_fact_receipt_list_rpc_args(
    state: Option<&str>,
    limit: usize,
) -> serde_json::Value {
    serde_json::json!({
        "action": "automatic_fact_receipt_list",
        "state": state,
        "limit": limit,
    })
}

pub(super) fn automatic_fact_receipt_view_rpc_args(id: &str) -> serde_json::Value {
    serde_json::json!({ "action": "automatic_fact_receipt_view", "id": id })
}

pub(super) async fn handle_automation_facts_command(
    action: AutomationFactsAction,
) -> tracedecay::errors::Result<()> {
    let path = match &action {
        AutomationFactsAction::List { path, .. } | AutomationFactsAction::View { path, .. } => {
            path.clone()
        }
    };
    let project_path = resolve_cli_project_root(path, None, None).await?;
    let payload = match action {
        AutomationFactsAction::List { state, limit, .. } => {
            daemon_automation_action(
                &project_path,
                automatic_fact_receipt_list_rpc_args(state.as_deref(), limit),
            )
            .await?
        }
        AutomationFactsAction::View { id, .. } => {
            daemon_automation_action(&project_path, automatic_fact_receipt_view_rpc_args(&id))
                .await?
        }
    };
    println!("{}", serde_json::to_string_pretty(&payload)?);
    Ok(())
}
