use std::path::Path;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use tracedecay_agent_hosts::automation::{AutomationRunControl, run_ledger::AutomationTrigger};
use tracedecay_application::{CancellationSignal, Deadline, RequestId, now_micros};
use tracedecay_domain::ProvenanceId;
use tracedecay_store::{ProjectMemoryAutomaticFactReceiptV1, ProjectMemoryAutomaticFactStateV1};

use crate::errors::{Result, TraceDecayError};
use crate::global_db::RegisteredGlobalDb;
use crate::store::memory::DatabaseFactStore;
use crate::tracedecay::TraceDecay;
use tracedecay_usecases::memory::{MemoryApplication, MemoryApplicationError};

use super::super::ToolResult;
use super::json_result;

mod automation_terminal;
use automation_terminal::{
    admission_conflict_value, automation_run_observer, decode_options, pre_admission_problem_value,
    require_observation, run_skill_writer as run_admitted_skill_writer,
    settle_retained_run as automation_run_value,
    terminal_response_value as automation_terminal_response,
};

#[derive(Debug, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
enum AdminProjectAction {
    CounterGet,
    CounterReset,
    StatusAccounting,
    GitignoreStatus,
    Bench {
        queries_toml: Option<String>,
        json: bool,
        max_nodes: usize,
    },
    AutomaticFactReceiptList {
        state: Option<String>,
        limit: usize,
    },
    AutomaticFactReceiptView {
        id: String,
    },
    AutomationRun {
        task: AutomationRunTask,
        options: Value,
        #[serde(default)]
        trigger: AutomationTrigger,
    },
    AutomationReconcile {
        scope: crate::dashboard::AutomationReconcileScope,
    },
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum AutomationRunTask {
    SessionReflection,
    SkillWriting,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SessionReflectionOptions {
    provider: String,
    query: String,
    evidence_limit: usize,
    scope: tracedecay_agent_hosts::ports::session_evidence::LcmScope,
    session_id: Option<String>,
    include_summaries: bool,
    sort: tracedecay_agent_hosts::ports::session_evidence::LcmGrepSort,
    source: Option<String>,
    role: Option<String>,
    start_time: Option<i64>,
    end_time: Option<i64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SkillWritingOptions {
    provider: String,
    query: String,
    evidence_limit: usize,
}

fn project_memory_application<'a>(
    cg: &TraceDecay,
    db: &'a crate::db::Database,
) -> Result<MemoryApplication<DatabaseFactStore<'a>>> {
    let owner = cg.project_memory_owner()?;
    MemoryApplication::new(owner, DatabaseFactStore::new(db)).map_err(memory_application_error)
}

fn memory_application_error(error: MemoryApplicationError) -> TraceDecayError {
    TraceDecayError::Config {
        message: format!("project memory application failed: {error}"),
    }
}

fn admin_project_run_control(
    deadline: Deadline,
    cancellation: CancellationSignal,
) -> AutomationRunControl {
    AutomationRunControl::from_interrupted(Arc::new(move || {
        cancellation.is_cancelled() || deadline.is_elapsed_at(now_micros())
    }))
}

fn parse_automatic_fact_apply_id(value: String) -> Result<ProvenanceId> {
    ProvenanceId::new(value).map_err(|error| TraceDecayError::Config {
        message: format!("invalid automatic fact apply id: {error}"),
    })
}

fn parse_automatic_fact_state(value: &str) -> Result<ProjectMemoryAutomaticFactStateV1> {
    let normalized = value.trim().replace('-', "_");
    match normalized.as_str() {
        "applied" => Ok(ProjectMemoryAutomaticFactStateV1::Applied),
        "quarantined" => Ok(ProjectMemoryAutomaticFactStateV1::Quarantined),
        _ => Err(TraceDecayError::Config {
            message: format!(
                "invalid automatic fact state `{value}`; expected applied or quarantined"
            ),
        }),
    }
}

fn automatic_fact_state_name(state: ProjectMemoryAutomaticFactStateV1) -> &'static str {
    match state {
        ProjectMemoryAutomaticFactStateV1::Applied => "applied",
        ProjectMemoryAutomaticFactStateV1::Quarantined => "quarantined",
    }
}

fn automatic_fact_receipt_json(receipt: &ProjectMemoryAutomaticFactReceiptV1) -> Value {
    let request = receipt.request();
    let mut value = Map::from_iter([
        (
            "apply_id".to_owned(),
            Value::String(receipt.apply_id().as_str().to_owned()),
        ),
        (
            "state".to_owned(),
            Value::String(automatic_fact_state_name(receipt.state()).to_owned()),
        ),
        (
            "operation_id".to_owned(),
            Value::String(request.operation_id().as_str().to_owned()),
        ),
        (
            "add_fact_request".to_owned(),
            json!({
                "content": request.content(),
                "category": request.category(),
                "source_label": request.source_label(),
                "tags": request.tags(),
                "entities": request.entities(),
                "trust": request.default_trust(),
                "metadata": request.metadata(),
            }),
        ),
        ("evidence".to_owned(), json!(receipt.evidence())),
        (
            "recorded_at_micros".to_owned(),
            json!(receipt.recorded_at().0),
        ),
    ]);
    if let Some(fact_id) = receipt.applied_fact_id() {
        value.insert(
            "applied_fact_id".to_owned(),
            Value::String(fact_id.as_str().to_owned()),
        );
    }
    if let Some(reason) = receipt.quarantine_reason() {
        value.insert(
            "quarantine_reason".to_owned(),
            Value::String(reason.to_owned()),
        );
    }
    Value::Object(value)
}

pub(super) async fn handle_admin_project(
    cg: &TraceDecay,
    args: Value,
    global_db: Option<&RegisteredGlobalDb>,
    automation_scheduler_reconciler: Option<crate::dashboard::AutomationSchedulerReconciler>,
    profile_root: Option<&Path>,
    daemon_invocation_service: Option<&crate::daemon::DaemonInvocationService>,
    application_request_id: Option<RequestId>,
    application_deadline: Deadline,
    application_cancellation: CancellationSignal,
) -> Result<ToolResult> {
    let run_control = admin_project_run_control(
        application_deadline.clone(),
        application_cancellation.clone(),
    );
    let action: AdminProjectAction =
        serde_json::from_value(args).map_err(|error| TraceDecayError::Config {
            message: format!("invalid tracedecay_admin_project arguments: {error}"),
        })?;
    let value = match action {
        AdminProjectAction::CounterGet => json!({ "counter": cg.get_local_counter().await? }),
        AdminProjectAction::CounterReset => {
            cg.reset_local_counter().await?;
            json!({ "reset": true })
        }
        AdminProjectAction::AutomationReconcile { scope } => {
            if scope != crate::dashboard::AutomationReconcileScope::Project {
                return Err(TraceDecayError::Config {
                    message:
                        "profile automation reconciliation requires a projectless daemon request"
                            .to_string(),
                });
            }
            let outcome = match automation_scheduler_reconciler {
                Some(reconcile) => reconcile().await,
                None => crate::dashboard::AutomationSchedulerReconcileOutcome::OwnerUnavailable,
            };
            json!({ "scope": "project", "outcome": outcome })
        }
        AdminProjectAction::StatusAccounting => {
            let global_db = global_db.ok_or_else(|| TraceDecayError::Config {
                message: "daemon global database is unavailable".to_string(),
            })?;
            let tokens_saved = cg.get_tokens_saved().await.unwrap_or(0);
            global_db.upsert(cg.project_root(), tokens_saved).await;
            let global_tokens_saved = global_db
                .global_tokens_saved()
                .await
                .map(|total| total.saturating_sub(tokens_saved))
                .filter(|total| *total > 0);
            json!({
                "tokens_saved": tokens_saved,
                "global_tokens_saved": global_tokens_saved,
            })
        }
        AdminProjectAction::GitignoreStatus => {
            let configuration = cg
                .configuration_runtime()
                .client()
                .current()
                .await
                .map_err(|error| TraceDecayError::Config {
                    message: format!("configuration authority unavailable: {error}"),
                })?;
            json!({
                "git_ignore": configuration.config.git_ignore,
                "revision_id": configuration.revision_id.as_str(),
            })
        }
        AdminProjectAction::Bench {
            queries_toml,
            json,
            max_nodes,
        } => {
            let report = crate::bench::run_bench_with_toml(
                cg,
                queries_toml
                    .as_deref()
                    .unwrap_or(crate::bench::DEFAULT_QUERIES_TOML),
                crate::bench::BenchOptions {
                    format: crate::bench::OutputFormat::Json,
                    max_nodes,
                },
            )
            .await?;
            let output = if json {
                crate::bench::format_report_json(&report)
            } else {
                crate::bench::format_report_console(&report)
            };
            json!({ "output": output })
        }
        AdminProjectAction::AutomaticFactReceiptList { state, limit } => {
            let db = cg.open_project_store_db().await?;
            let memory = project_memory_application(cg, &db)?;
            let state = state
                .as_deref()
                .map(parse_automatic_fact_state)
                .transpose()?;
            let page = memory
                .list_project_memory_automatic_fact_receipts(
                    state,
                    None,
                    limit,
                    run_control.read_control(),
                )
                .await
                .map_err(memory_application_error)?;
            let receipts = page
                .receipts()
                .iter()
                .map(automatic_fact_receipt_json)
                .collect::<Vec<_>>();
            json!({
                "availability": { "state": "available" },
                "count": receipts.len(),
                "receipts": receipts,
                "next_after_apply_id": page
                    .next_after_apply_id()
                    .map(ProvenanceId::as_str),
            })
        }
        AdminProjectAction::AutomaticFactReceiptView { id } => {
            let apply_id = parse_automatic_fact_apply_id(id)?;
            let db = cg.open_project_store_db().await?;
            let memory = project_memory_application(cg, &db)?;
            let receipt = memory
                .get_project_memory_automatic_fact_receipt(apply_id, run_control.read_control())
                .await
                .map_err(memory_application_error)?
                .ok_or_else(|| TraceDecayError::Config {
                    message: "automatic fact receipt not found".to_string(),
                })?;
            json!({ "receipt": automatic_fact_receipt_json(&receipt) })
        }
        AdminProjectAction::AutomationRun {
            task,
            options,
            trigger,
        } => {
            run_automation(
                cg,
                profile_root,
                task,
                options,
                trigger,
                daemon_invocation_service,
                application_request_id,
                application_deadline,
                application_cancellation,
                &run_control,
            )
            .await?
        }
    };
    let semantic_error = automation_response_is_semantic_error(&value);
    Ok(json_result(&value).with_semantic_error(semantic_error))
}

fn automation_response_is_semantic_error(value: &Value) -> bool {
    matches!(
        value.get("kind").and_then(Value::as_str),
        Some("problem" | "conflict")
    )
}

async fn run_automation(
    cg: &TraceDecay,
    profile_root: Option<&Path>,
    task: AutomationRunTask,
    options: Value,
    trigger: AutomationTrigger,
    daemon_invocation_service: Option<&crate::daemon::DaemonInvocationService>,
    application_request_id: Option<RequestId>,
    application_deadline: Deadline,
    application_cancellation: CancellationSignal,
    run_control: &AutomationRunControl,
) -> Result<Value> {
    use tracedecay_agent_hosts::automation::backend::CodexAppServerBackend;
    use tracedecay_agent_hosts::automation::config::from_configuration_snapshot;
    use tracedecay_agent_hosts::automation::runner::{
        SessionReflectorAutomationOptions, SkillWriterAutomationOptions,
        run_session_reflector_with_backend_for_retained_settlement,
    };

    let invocation_service = require_observation(daemon_invocation_service)?;
    let producer = crate::daemon::project_automation_observation_producer(
        invocation_service,
        cg.project_root(),
    )
    .await
    .ok_or_else(|| TraceDecayError::Config {
        message: "manual automation observation authority is unavailable".to_owned(),
    })?;

    let pinned = cg
        .configuration_runtime()
        .client()
        .current()
        .await
        .map_err(|error| TraceDecayError::Config {
            message: format!("automation configuration authority is unavailable: {error}"),
        })?;
    let config = from_configuration_snapshot(&pinned.snapshot)?;
    let configuration_digest =
        crate::daemon::automation_effect::pinned_automation_configuration_digest(
            &pinned.revision_id,
            &pinned.snapshot.effective_behavior_digest,
            &pinned.snapshot.resolution_provenance_digest,
        )?;
    let backend = CodexAppServerBackend::from_automation_config(&config);
    let request_id = application_request_id.ok_or_else(|| TraceDecayError::Config {
        message: "manual automation application request identity is unavailable".to_owned(),
    })?;

    let run = match task {
        AutomationRunTask::SessionReflection => {
            let options = decode_options::<SessionReflectionOptions>(options)?;
            let run_id = request_id.as_str().to_owned();
            let run_options = SessionReflectorAutomationOptions {
                trigger,
                run_id: Some(run_id.clone()),
                provider: options.provider,
                query: options.query,
                scope: options.scope,
                session_id: options.session_id,
                include_summaries: options.include_summaries,
                evidence_limit: options.evidence_limit,
                sort: options.sort,
                source: options.source,
                role: options.role,
                start_time: options.start_time,
                end_time: options.end_time,
                ..SessionReflectorAutomationOptions::default()
            };
            let admission = crate::daemon::automation_effect::AutomationEffectAuthority::prepare(
                invocation_service,
                cg,
                cg.project_root(),
                &cg.store_layout().dashboard_root,
                request_id,
                application_deadline,
                &application_cancellation,
                now_micros(),
                configuration_digest,
                crate::daemon::automation_effect::session_reflector_run_request(
                    &run_id,
                    &run_options,
                )?,
            )
            .await?;
            let effect = match admission {
                crate::daemon::automation_effect::AutomationEffectAdmission::Execute(effect) => {
                    effect
                }
                crate::daemon::automation_effect::AutomationEffectAdmission::Replay(terminal) => {
                    return automation_terminal_response(&terminal);
                }
                crate::daemon::automation_effect::AutomationEffectAdmission::Conflict => {
                    return Ok(admission_conflict_value());
                }
                crate::daemon::automation_effect::AutomationEffectAdmission::PreAdmissionProblem(
                    problem,
                ) => return pre_admission_problem_value(problem),
            };
            let (run, ledger_record) = automation_run_value(
                run_session_reflector_with_backend_for_retained_settlement(
                    cg,
                    &config,
                    run_control,
                    &pinned.revision_id,
                    &backend,
                    run_options,
                )
                .await,
                effect,
                automation_run_observer(
                    Arc::clone(&producer),
                    cg.project_root().to_path_buf(),
                    "manual_mcp",
                ),
            )
            .await?;
            if run.get("kind").and_then(Value::as_str) == Some("problem") {
                return Ok(run);
            }
            if ledger_record.is_none() {
                return Err(TraceDecayError::Config {
                    message: "settled session reflector run lost its observation ledger".to_owned(),
                });
            }
            run
        }
        AutomationRunTask::SkillWriting => {
            let options = decode_options::<SkillWritingOptions>(options)?;
            let profile_root = profile_root.ok_or_else(|| TraceDecayError::Config {
                message: "automation skill writing requires exact daemon profile authority"
                    .to_owned(),
            })?;
            let run_id = request_id.as_str().to_owned();
            let run_options = SkillWriterAutomationOptions {
                trigger,
                run_id: Some(run_id.clone()),
                provider: options.provider,
                query: options.query,
                evidence_limit: options.evidence_limit,
                profile_root: Some(profile_root.to_path_buf()),
                ..SkillWriterAutomationOptions::default()
            };
            let (run, ledger_record) = run_admitted_skill_writer(
                invocation_service,
                cg,
                request_id,
                application_deadline,
                &application_cancellation,
                configuration_digest,
                &config,
                &pinned.revision_id,
                &backend,
                run_options,
                automation_run_observer(
                    Arc::clone(&producer),
                    cg.project_root().to_path_buf(),
                    "manual_mcp",
                ),
            )
            .await?;
            if run.get("kind").and_then(Value::as_str) == Some("problem") {
                return Ok(run);
            }
            if ledger_record.is_none() {
                return Ok(run);
            }
            run
        }
    };
    Ok(json!({ "run": run }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn automation_conflict_is_reported_as_an_mcp_semantic_error() {
        assert!(automation_response_is_semantic_error(
            &admission_conflict_value()
        ));
        assert!(!automation_response_is_semantic_error(
            &json!({ "run": { "status": "completed" } })
        ));
    }

    fn test_application_control() -> (Deadline, CancellationSignal) {
        (
            Deadline::new(tracedecay_domain::UtcMicros(i64::MAX)).unwrap(),
            CancellationSignal::active("cancel.admin-project-test").unwrap(),
        )
    }

    fn tool_json(result: &ToolResult) -> Value {
        let text = result.value["content"][0]["text"]
            .as_str()
            .expect("admin project result should contain JSON text");
        serde_json::from_str(text).expect("admin project result should be valid JSON")
    }

    async fn seed_automatic_fact_receipt(
        cg: &TraceDecay,
        apply_id: &str,
        content: &str,
    ) -> ProjectMemoryAutomaticFactReceiptV1 {
        use tracedecay_domain::{ActorId, Confidence, FactCategoryV1};
        use tracedecay_usecases::memory::ProjectMemoryFactAddRequest;

        let owner = cg.project_memory_owner().unwrap();
        let db = cg.open_project_store_db().await.unwrap();
        let memory = MemoryApplication::new(owner.clone(), DatabaseFactStore::new(&db)).unwrap();
        let actor = ActorId::new("automation.session-reflector".to_owned()).unwrap();
        let request = tracedecay_usecases::memory::automatic_fact_add_command(
            owner,
            ProjectMemoryFactAddRequest {
                content: content.to_owned(),
                category: FactCategoryV1::Decision,
                source_label: Some("admin-project-test".to_owned()),
                tags: Vec::new(),
                entities: Vec::new(),
                trust: Some(Confidence::new(0.9).unwrap()),
                metadata: json!({}),
            },
            "run.admin-project-test",
            apply_id,
            Some(actor),
        )
        .unwrap();
        let run_control = AutomationRunControl::from_interrupted(Arc::new(|| false));
        let write_control = run_control.write_control();
        memory
            .apply_project_memory_automatic_fact(
                ProvenanceId::new(apply_id.to_owned()).unwrap(),
                request,
                tracedecay_store::ProjectMemoryAutomaticFactEvidenceV1::default(),
                &write_control,
            )
            .await
            .unwrap()
            .receipt()
            .clone()
    }

    #[tokio::test]
    async fn admin_project_handler_reads_terminal_automatic_fact_receipts() {
        let temp = tempfile::tempdir().unwrap();
        let project_root = temp.path().join("project");
        let profile_root = temp.path().join("profile");
        std::fs::create_dir_all(&project_root).unwrap();
        std::fs::create_dir_all(&profile_root).unwrap();
        let project_root = std::fs::canonicalize(project_root).unwrap();
        let profile_root = std::fs::canonicalize(profile_root).unwrap();
        let cg = TraceDecay::init_with_options(
            &project_root,
            crate::tracedecay::TraceDecayOpenOptions {
                global_db_path: Some(profile_root.join("global.db")),
                profile_root: Some(profile_root),
            },
        )
        .await
        .unwrap();
        let owner_before = crate::db::probe_writer_owner(&cg.store_layout().graph_db_path).unwrap();

        let apply_id = "automatic-fact.rpc.read-only";
        seed_automatic_fact_receipt(
            &cg,
            apply_id,
            "Admin project RPC reads this terminal automatic fact receipt",
        )
        .await;

        let (deadline, cancellation) = test_application_control();
        let applied = tool_json(
            &handle_admin_project(
                &cg,
                json!({
                    "action": "automatic_fact_receipt_list",
                    "state": "applied",
                    "limit": 50,
                }),
                None,
                None,
                None,
                None,
                None,
                deadline,
                cancellation,
            )
            .await
            .unwrap(),
        );
        assert_eq!(applied["count"], 1);
        assert_eq!(applied["availability"]["state"], "available");
        assert!(
            applied["receipts"]
                .as_array()
                .unwrap()
                .iter()
                .all(|receipt| receipt["state"] == "applied")
        );

        let (deadline, cancellation) = test_application_control();
        let viewed = tool_json(
            &handle_admin_project(
                &cg,
                json!({ "action": "automatic_fact_receipt_view", "id": apply_id }),
                None,
                None,
                None,
                None,
                None,
                deadline,
                cancellation,
            )
            .await
            .unwrap(),
        );
        assert_eq!(viewed["receipt"]["apply_id"], apply_id);
        assert_eq!(viewed["receipt"]["state"], "applied");
        assert_eq!(
            viewed["receipt"]["add_fact_request"]["content"],
            "Admin project RPC reads this terminal automatic fact receipt"
        );
        assert_eq!(
            viewed["receipt"]["add_fact_request"]["category"],
            "decision"
        );
        assert_eq!(viewed["receipt"]["add_fact_request"]["trust"], json!(0.9));
        assert_eq!(
            viewed["receipt"]["add_fact_request"]["source_label"],
            "admin-project-test"
        );
        assert!(viewed["receipt"]["applied_fact_id"].is_string());

        for action in [
            json!({ "action": "fact_apply", "id": apply_id }),
            json!({
                "action": "fact_reject",
                "id": apply_id,
                "reason": "not durable",
            }),
        ] {
            let (deadline, cancellation) = test_application_control();
            assert!(
                handle_admin_project(
                    &cg,
                    action,
                    None,
                    None,
                    None,
                    None,
                    None,
                    deadline,
                    cancellation,
                )
                .await
                .is_err(),
                "manual fact mutations must not be accepted"
            );
        }

        let owner_after = crate::db::probe_writer_owner(&cg.store_layout().graph_db_path).unwrap();
        assert_eq!(owner_after, owner_before);
    }

    #[test]
    fn admin_project_wire_contract_round_trips_typed_results_without_local_fallback() {
        assert!(matches!(
            serde_json::from_value::<AdminProjectAction>(json!({ "action": "gitignore_status" }))
                .unwrap(),
            AdminProjectAction::GitignoreStatus
        ));

        for retired_action in [
            json!({
                "action": "memory_curate",
                "apply": true,
                "llm": false,
                "llm_ops": null,
                "fact_review_limit": 12,
                "min_confidence": 0.75,
            }),
            json!({ "action": "fact_apply", "id": "fact_1" }),
            json!({
                "action": "fact_reject",
                "id": "fact_1",
                "reason": "not durable",
            }),
            json!({ "action": "fact_list", "state": "applied", "limit": 50 }),
            json!({ "action": "fact_view", "id": "fact_1" }),
        ] {
            assert!(serde_json::from_value::<AdminProjectAction>(retired_action).is_err());
        }

        let list = serde_json::from_value::<AdminProjectAction>(json!({
            "action": "automatic_fact_receipt_list",
            "state": "applied",
            "limit": 50,
        }))
        .unwrap();
        assert!(matches!(
            list,
            AdminProjectAction::AutomaticFactReceiptList { state: Some(state), limit: 50 }
                if state == "applied"
        ));
        let view = serde_json::from_value::<AdminProjectAction>(json!({
            "action": "automatic_fact_receipt_view",
            "id": "fact_1",
        }))
        .unwrap();
        assert!(matches!(
            view,
            AdminProjectAction::AutomaticFactReceiptView { id } if id == "fact_1"
        ));

        assert!(
            serde_json::from_value::<AdminProjectAction>(json!({
                "action": "automation_run",
                "task": "memory_curation",
                "options": { "fact_review_limit": 12, "min_confidence": 0.75 }
            }))
            .is_err()
        );
        assert!(matches!(
            serde_json::from_value::<AdminProjectAction>(json!({
                "action": "automation_reconcile",
                "scope": "project"
            }))
            .unwrap(),
            AdminProjectAction::AutomationReconcile {
                scope: crate::dashboard::AutomationReconcileScope::Project
            }
        ));
    }

    #[test]
    fn automation_admin_actions_have_stable_strict_schemas() {
        assert!(
            serde_json::from_value::<AdminProjectAction>(json!({
                "action": "fact_apply",
                "id": "fact_1"
            }))
            .is_err()
        );
        for retired in ["pending_approval", "applying", "rejected_validation"] {
            assert!(parse_automatic_fact_state(retired).is_err());
        }
        assert_eq!(
            parse_automatic_fact_state("applied").unwrap(),
            ProjectMemoryAutomaticFactStateV1::Applied
        );
        assert_eq!(
            parse_automatic_fact_state(" quarantined ").unwrap(),
            ProjectMemoryAutomaticFactStateV1::Quarantined
        );

        assert!(
            serde_json::from_value::<AdminProjectAction>(json!({
                "action": "automation_run",
                "task": "memory_curation",
                "options": { "fact_review_limit": 12, "min_confidence": 0.75 }
            }))
            .is_err()
        );

        let session = decode_options::<SessionReflectionOptions>(json!({
            "provider": "claude",
            "query": "decisions",
            "evidence_limit": 11,
            "scope": "session",
            "session_id": "session-3",
            "include_summaries": false,
            "sort": "hybrid",
            "source": "assistant",
            "role": "user",
            "start_time": 10,
            "end_time": 20
        }))
        .unwrap();
        assert_eq!(
            session.scope,
            tracedecay_agent_hosts::ports::session_evidence::LcmScope::Session
        );
        assert_eq!(
            session.sort,
            tracedecay_agent_hosts::ports::session_evidence::LcmGrepSort::Hybrid
        );

        let skill = decode_options::<SkillWritingOptions>(json!({
            "provider": "all",
            "query": "repeated workflow",
            "evidence_limit": 13
        }))
        .unwrap();
        assert_eq!(skill.evidence_limit, 13);
    }

    #[test]
    fn manual_automation_without_observation_authority_fails_closed() {
        let Err(error) = require_observation(None) else {
            panic!("manual automation must not run without observation authority");
        };

        assert!(matches!(
            error,
            TraceDecayError::Config { message }
                if message == "manual automation observation authority is unavailable"
        ));
    }
}
