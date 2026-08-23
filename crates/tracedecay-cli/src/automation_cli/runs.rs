use super::daemon_project_dashboard_root;
use crate::cli::AutomationRunsAction;
use crate::resolve_cli_project_root;

pub(super) async fn handle_automation_runs_command(
    action: AutomationRunsAction,
) -> tracedecay::errors::Result<()> {
    use tracedecay_agent_hosts::automation::run_ledger::{
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
    records: &[tracedecay_agent_hosts::automation::run_ledger::AutomationRunLedgerRecord],
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
            record.task_key.as_deref().unwrap_or_else(|| {
                tracedecay_agent_hosts::automation::backend::task_key(record.task)
            }),
            record.trigger,
            record.accepted_count,
            record.rejected_count,
            record.completed_at,
            record.error.as_deref().unwrap_or("")
        );
    }
}

fn print_automation_run_record(
    record: &tracedecay_agent_hosts::automation::run_ledger::AutomationRunLedgerRecord,
) {
    println!("run_id: {}", record.run_id);
    println!("status: {}", record.status.as_str());
    println!(
        "task: {}",
        record
            .task_key
            .as_deref()
            .unwrap_or_else(|| tracedecay_agent_hosts::automation::backend::task_key(record.task))
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
    artifact: &tracedecay_agent_hosts::automation::run_ledger::AutomationRunArtifact,
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
