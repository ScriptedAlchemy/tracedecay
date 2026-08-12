use super::{daemon_automation_action, daemon_project_dashboard_root};
use crate::cli::{AutomationRunAction, AutomationRunsAction};
use crate::{parse_lcm_scope_arg, resolve_cli_project_root};

pub(super) fn automation_run_rpc_request(
    action: AutomationRunAction,
) -> tracedecay::errors::Result<(Option<String>, serde_json::Value)> {
    let request = match action {
        AutomationRunAction::MemoryCuration {
            fact_review_limit,
            min_confidence,
            path,
        } => (
            path,
            serde_json::json!({
                "action": "automation_run",
                "task": "memory_curation",
                "options": {
                    "fact_review_limit": fact_review_limit,
                    "min_confidence": min_confidence,
                },
            }),
        ),
        AutomationRunAction::SessionReflection {
            provider,
            query,
            evidence_limit,
            scope,
            session_id,
            include_summaries,
            sort,
            source,
            role,
            start_time,
            end_time,
            path,
        } => {
            parse_lcm_scope_arg(&scope)?;
            sort.parse::<tracedecay_sessions::runtime::lcm::LcmGrepSort>()
                .map_err(|()| tracedecay::errors::TraceDecayError::Config {
                    message: format!(
                        "invalid session-reflection --sort '{sort}'; expected recency, relevance, or hybrid"
                    ),
                })?;
            (
                path,
                serde_json::json!({
                    "action": "automation_run",
                    "task": "session_reflection",
                    "options": {
                        "provider": provider,
                        "query": query,
                        "evidence_limit": evidence_limit,
                        "scope": scope,
                        "session_id": session_id,
                        "include_summaries": include_summaries,
                        "sort": sort,
                        "source": source,
                        "role": role,
                        "start_time": start_time,
                        "end_time": end_time,
                    },
                }),
            )
        }
        AutomationRunAction::SkillWriting {
            provider,
            query,
            evidence_limit,
            path,
        } => (
            path,
            serde_json::json!({
                "action": "automation_run",
                "task": "skill_writing",
                "options": {
                    "provider": provider,
                    "query": query,
                    "evidence_limit": evidence_limit,
                },
            }),
        ),
    };
    Ok(request)
}

pub(super) enum AutomationRunRpcOutcome<'a> {
    Run(&'a serde_json::Value),
    Problem(tracedecay_application::ApplicationProblemEnvelope),
}

pub(super) fn automation_run_result(
    payload: &serde_json::Value,
) -> tracedecay::errors::Result<AutomationRunRpcOutcome<'_>> {
    if payload.get("kind").and_then(serde_json::Value::as_str) == Some("problem") {
        let problem =
            payload
                .get("value")
                .ok_or_else(|| tracedecay::errors::TraceDecayError::Config {
                    message: "daemon automation problem omitted its typed value".to_owned(),
                })?;
        return serde_json::from_value(problem.clone())
            .map(AutomationRunRpcOutcome::Problem)
            .map_err(|error| tracedecay::errors::TraceDecayError::Config {
                message: format!("daemon automation problem is invalid: {error}"),
            });
    }
    payload
        .get("run")
        .map(AutomationRunRpcOutcome::Run)
        .ok_or_else(|| tracedecay::errors::TraceDecayError::Config {
            message: "daemon automation response omitted run".to_string(),
        })
}

pub(super) async fn handle_automation_run_command(
    action: AutomationRunAction,
) -> tracedecay::errors::Result<()> {
    let (path, args) = automation_run_rpc_request(action)?;
    let project_path = resolve_cli_project_root(path, None, None).await?;
    let payload = daemon_automation_action(&project_path, args).await?;
    match automation_run_result(&payload)? {
        AutomationRunRpcOutcome::Run(run) => {
            println!("{}", serde_json::to_string_pretty(run)?);
            Ok(())
        }
        AutomationRunRpcOutcome::Problem(problem) => {
            eprintln!("{}", serde_json::to_string_pretty(&problem)?);
            Err(tracedecay::errors::TraceDecayError::Config {
                message: problem.problem.message,
            })
        }
    }
}

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
