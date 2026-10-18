use crate::cli::AutomationFactsAction;
use crate::resolve_cli_project_root;
use tracedecay_contracts::retrieval::{
    AdminProjectResultV1, AdminProjectSurfaceRequestV1, AutomaticFactReceiptStateV1,
};
use tracedecay_runtime_core::config::ProfileRoot;

fn receipt_state(state: &str) -> tracedecay_domain::errors::Result<AutomaticFactReceiptStateV1> {
    serde_json::from_value(serde_json::Value::String(state.to_owned())).map_err(|_| {
        tracedecay_domain::errors::TraceDecayError::Config {
            message: format!(
                "invalid automatic fact state `{state}`; expected applied or quarantined"
            ),
        }
    })
}

pub(super) async fn handle_automation_facts_command(
    profile: &ProfileRoot,
    action: AutomationFactsAction,
) -> tracedecay_domain::errors::Result<()> {
    let path = match &action {
        AutomationFactsAction::List { path, .. } | AutomationFactsAction::View { path, .. } => {
            path.clone()
        }
    };
    let project_path = resolve_cli_project_root(profile, path, None, None).await?;
    let request = match action {
        AutomationFactsAction::List { state, limit, .. } => {
            AdminProjectSurfaceRequestV1::AutomaticFactReceiptList {
                state: state.as_deref().map(receipt_state).transpose()?,
                limit,
            }
        }
        AutomationFactsAction::View { id, .. } => {
            AdminProjectSurfaceRequestV1::AutomaticFactReceiptView { id }
        }
    };
    let payload = match crate::commands::admin_project(profile, &project_path, request).await? {
        result @ (AdminProjectResultV1::AutomaticFactReceiptList(_)
        | AdminProjectResultV1::AutomaticFactReceiptView(_)) => serde_json::to_value(result)?,
        _ => return Err(crate::commands::unexpected_admin_project_result()),
    };
    println!("{}", serde_json::to_string_pretty(&payload)?);
    Ok(())
}
