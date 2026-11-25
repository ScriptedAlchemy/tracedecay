use crate::cli::AutomationRunsAction;
use crate::resolve_cli_project_root;
use tracedecay_automation_runtime::automation::run_ledger::{
    AutomationRunArtifact, AutomationRunLedgerRecord,
};
use tracedecay_contracts::retrieval::{AUTOMATION_RUN_LIST_MAX_LIMIT, AutomationRunListEntryV1};
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_runtime_core::config::ProfileRoot;

/// Run history lives in the daemon-owned registered store, so every read goes
/// through the daemon's run tools rather than a local file.
pub(super) async fn handle_automation_runs_command(
    profile: &ProfileRoot,
    action: AutomationRunsAction,
) -> Result<()> {
    let path = match &action {
        AutomationRunsAction::List { path, .. }
        | AutomationRunsAction::View { path, .. }
        | AutomationRunsAction::Artifact { path, .. } => path.clone(),
    };
    let project_path = resolve_cli_project_root(profile, path, None, None).await?;

    match action {
        AutomationRunsAction::List { limit, json, .. } => {
            let limit = u32::try_from(limit)
                .unwrap_or(AUTOMATION_RUN_LIST_MAX_LIMIT)
                .clamp(1, AUTOMATION_RUN_LIST_MAX_LIMIT);
            let listed = crate::commands::daemon_tool_json(
                profile,
                Some(&project_path),
                "tracedecay_automation_run_list",
                serde_json::json!({ "limit": limit, "format": "json" }),
            )
            .await?;
            let runs: Vec<AutomationRunListEntryV1> = decode_field(&listed, "runs")?;
            let has_more: bool = decode_field(&listed, "has_more")?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "count": runs.len(),
                        "limit": limit,
                        "has_more": has_more,
                        "records": runs,
                    }))?
                );
            } else {
                print_automation_run_list(&runs);
            }
        }
        AutomationRunsAction::View { run_id, json, .. } => {
            let viewed = crate::commands::daemon_tool_json(
                profile,
                Some(&project_path),
                "tracedecay_automation_run_view",
                serde_json::json!({ "run_id": run_id, "format": "json" }),
            )
            .await?;
            let record: AutomationRunLedgerRecord = decode_field(&viewed, "run")?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({ "record": record }))?
                );
            } else {
                print_automation_run_record(&record);
            }
        }
        AutomationRunsAction::Artifact {
            run_id, kind, json, ..
        } => {
            let viewed = crate::commands::daemon_tool_json(
                profile,
                Some(&project_path),
                "tracedecay_automation_run_artifact_view",
                serde_json::json!({ "run_id": run_id, "kind": kind, "format": "json" }),
            )
            .await?;
            let artifact: AutomationRunArtifact = decode_field(&viewed, "artifact")?;
            let payload: serde_json::Value = decode_field(&viewed, "payload")?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "run_id": run_id,
                        "artifact": artifact,
                        "payload": payload,
                    }))?
                );
            } else {
                print_automation_run_artifact(&run_id, &artifact, &payload)?;
            }
        }
    }
    Ok(())
}

fn decode_field<T: serde::de::DeserializeOwned>(
    document: &serde_json::Value,
    field: &str,
) -> Result<T> {
    let value = document
        .get(field)
        .cloned()
        .ok_or_else(|| TraceDecayError::Config {
            message: format!("managed daemon automation run reply omitted '{field}'"),
        })?;
    serde_json::from_value(value).map_err(|error| TraceDecayError::Config {
        message: format!("managed daemon automation run reply has an invalid '{field}': {error}"),
    })
}

fn print_automation_run_list(records: &[AutomationRunListEntryV1]) {
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
            record.task_key,
            record.trigger,
            record.accepted_count,
            record.rejected_count,
            record.completed_at,
            record.error.as_deref().unwrap_or("")
        );
    }
}

fn print_automation_run_record(record: &AutomationRunLedgerRecord) {
    println!("run_id: {}", record.run_id);
    println!("status: {}", record.status.as_str());
    println!("task: {}", record.task_key);
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
    artifact: &AutomationRunArtifact,
    payload: &serde_json::Value,
) -> Result<()> {
    println!("run_id: {run_id}");
    println!("artifact: {}", artifact.kind);
    println!("path: {}", artifact.path);
    if let Some(summary) = artifact.summary.as_deref() {
        println!("summary: {summary}");
    }
    println!("{}", serde_json::to_string_pretty(payload)?);
    Ok(())
}
