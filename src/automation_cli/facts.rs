use super::{daemon_automation_action, daemon_project_dashboard_root};
use crate::cli::AutomationFactsAction;
use crate::resolve_cli_project_root;

pub(super) fn fact_apply_rpc_args(id: &str) -> serde_json::Value {
    serde_json::json!({ "action": "fact_apply", "id": id })
}

pub(super) async fn handle_automation_facts_command(
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
