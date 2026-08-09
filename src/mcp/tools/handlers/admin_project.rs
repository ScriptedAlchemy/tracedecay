//! Unadvertised daemon-owned project operations used by one-shot CLI commands.

use serde::Deserialize;
use serde_json::{Map, Value, json};
use tracedecay_domain::ProvenanceId;
use tracedecay_store::{ProjectMemoryAutomaticFactReceiptV1, ProjectMemoryAutomaticFactStateV1};

use crate::application::memory::{MemoryApplication, MemoryApplicationError};
use crate::errors::{Result, TraceDecayError};
use crate::global_db::RegisteredGlobalDb;
use crate::store::memory::DatabaseFactStore;
use crate::tracedecay::TraceDecay;

use super::super::ToolResult;
use super::json_result;

#[derive(Debug, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
enum AdminProjectAction {
    CounterGet,
    CounterReset,
    StatusAccounting,
    MemoryStatus,
    RuntimeStatus {
        json: bool,
    },
    GitignoreStatus,
    MemoryCurate {
        apply: bool,
        llm: bool,
        llm_ops: Option<Value>,
        max_clusters: usize,
        min_confidence: f64,
    },
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
    },
    AutomationReconcile {
        scope: crate::dashboard::AutomationReconcileScope,
    },
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum AutomationRunTask {
    MemoryCuration,
    SessionReflection,
    SkillWriting,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MemoryCurationOptions {
    max_clusters: usize,
    min_confidence: f64,
}

#[derive(Debug, Deserialize)]
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
                "source": request.source(),
                "tags": request.tags(),
                "entities": request.entities(),
                "trust": request.default_trust(),
                "metadata": request.metadata(),
            }),
        ),
        ("evidence".to_owned(), json!(receipt.evidence())),
        (
            "recorded_at".to_owned(),
            json!(receipt.recorded_at().0.div_euclid(1_000_000)),
        ),
    ]);
    if let Some(fact_id) = receipt.applied_fact_id() {
        value.insert(
            "applied_canonical_fact_id".to_owned(),
            Value::String(fact_id.as_str().to_owned()),
        );
    }
    if let Some(legacy_fact_id) = receipt
        .applied_mapping()
        .and_then(|mapping| mapping.legacy_fact_id())
    {
        value.insert("applied_fact_id".to_owned(), json!(legacy_fact_id));
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
) -> Result<ToolResult> {
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
        AdminProjectAction::MemoryStatus => {
            let status = cg.memory_status().await?;
            let db = cg.open_project_store_db().await?;
            let overview = project_memory_application(cg, &db)?
                .dashboard_overview(1, 1)
                .await
                .map_err(memory_application_error)?;
            let largest_bank_fact_count = overview
                .memory_banks
                .first()
                .map_or(0, |bank| bank.fact_count);
            json!({
                "status": status,
                "largest_bank_fact_count": largest_bank_fact_count,
            })
        }
        AdminProjectAction::RuntimeStatus { json } => {
            let snapshot = crate::runtime_telemetry::collect(cg).await?;
            let output = if json {
                crate::runtime_telemetry::to_pretty_json(&snapshot)
            } else {
                crate::runtime_telemetry::to_text_report(&snapshot)
            };
            json!({ "output": output })
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
        AdminProjectAction::MemoryCurate {
            apply,
            llm,
            llm_ops,
            max_clusters,
            min_confidence,
        } => {
            let options = crate::dashboard::memory_curate::MemoryCurateOptions {
                apply,
                llm,
                llm_ops,
                max_clusters: max_clusters.clamp(1, 50),
                min_confidence: min_confidence.clamp(0.0, 1.0),
            };
            crate::dashboard::memory_curate::run_memory_curate(cg, &options).await?
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
                .list_project_memory_automatic_fact_receipts(state, None, limit)
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
                .get_project_memory_automatic_fact_receipt(apply_id)
                .await
                .map_err(memory_application_error)?
                .ok_or_else(|| TraceDecayError::Config {
                    message: "automatic fact receipt not found".to_string(),
                })?;
            json!({ "receipt": automatic_fact_receipt_json(&receipt) })
        }
        AdminProjectAction::AutomationRun { task, options } => {
            run_automation(cg, task, options).await?
        }
    };
    Ok(json_result(&value))
}

async fn run_automation(cg: &TraceDecay, task: AutomationRunTask, options: Value) -> Result<Value> {
    use tracedecay_agent_hosts::automation::backend::CodexAppServerBackend;
    use tracedecay_agent_hosts::automation::config::{
        AutomationBackend, from_configuration_snapshot,
    };
    use tracedecay_agent_hosts::automation::run_ledger::AutomationTrigger;
    use tracedecay_agent_hosts::automation::runner::{
        MemoryCuratorAutomationOptions, SessionReflectorAutomationOptions,
        SkillWriterAutomationOptions, run_memory_curator_with_backend,
        run_session_reflector_with_backend, run_skill_writer_with_backend,
    };

    let pinned = cg
        .configuration_runtime()
        .client()
        .current()
        .await
        .map_err(|error| TraceDecayError::Config {
            message: format!("automation configuration authority is unavailable: {error}"),
        })?;
    let config = from_configuration_snapshot(&pinned.snapshot)?;
    if config.backend == AutomationBackend::ExternalCommand {
        return Err(TraceDecayError::Config {
            message: "automation backend external_command is not implemented yet".to_string(),
        });
    }
    let backend = CodexAppServerBackend::from_automation_config(&config);

    let run = match task {
        AutomationRunTask::MemoryCuration => {
            let options = decode_options::<MemoryCurationOptions>(options)?;
            serde_json::to_value(
                run_memory_curator_with_backend(
                    cg,
                    &config,
                    &backend,
                    MemoryCuratorAutomationOptions {
                        trigger: AutomationTrigger::ManualCli,
                        run_id: None,
                        max_clusters: options.max_clusters,
                        min_confidence: options.min_confidence,
                    },
                )
                .await?,
            )?
        }
        AutomationRunTask::SessionReflection => {
            let options = decode_options::<SessionReflectionOptions>(options)?;
            serde_json::to_value(
                run_session_reflector_with_backend(
                    cg,
                    &config,
                    &backend,
                    SessionReflectorAutomationOptions {
                        trigger: AutomationTrigger::ManualCli,
                        run_id: None,
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
                    },
                )
                .await?,
            )?
        }
        AutomationRunTask::SkillWriting => {
            let options = decode_options::<SkillWritingOptions>(options)?;
            serde_json::to_value(
                run_skill_writer_with_backend(
                    cg,
                    &config,
                    &backend,
                    SkillWriterAutomationOptions {
                        trigger: AutomationTrigger::ManualCli,
                        run_id: None,
                        provider: options.provider,
                        query: options.query,
                        evidence_limit: options.evidence_limit,
                        ..SkillWriterAutomationOptions::default()
                    },
                )
                .await?,
            )?
        }
    };
    Ok(json!({ "run": run }))
}

fn decode_options<T: serde::de::DeserializeOwned>(options: Value) -> Result<T> {
    serde_json::from_value(options).map_err(|error| TraceDecayError::Config {
        message: format!("invalid tracedecay_admin_project automation options: {error}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
        use crate::memory::types::{AddFactRequest, MemoryCategory};
        use tracedecay_domain::ActorId;

        let owner = cg.project_memory_owner().unwrap();
        let db = cg.open_project_store_db().await.unwrap();
        let memory = MemoryApplication::new(owner.clone(), DatabaseFactStore::new(&db)).unwrap();
        let actor = ActorId::new("automation.session-reflector".to_owned()).unwrap();
        let request = crate::application::memory::automatic_fact_add_command(
            owner,
            AddFactRequest {
                content: content.to_owned(),
                category: MemoryCategory::Decision,
                source: Some("admin-project-test".to_owned()),
                tags: Vec::new(),
                entities: Vec::new(),
                trust: Some(0.9),
                metadata: json!({}),
            },
            "run.admin-project-test",
            apply_id,
            Some(actor),
        )
        .unwrap();
        memory
            .apply_project_memory_automatic_fact(
                ProvenanceId::new(apply_id.to_owned()).unwrap(),
                request,
                tracedecay_store::ProjectMemoryAutomaticFactEvidenceV1::default(),
            )
            .await
            .unwrap()
            .receipt()
            .clone()
    }

    #[tokio::test]
    async fn admin_project_handler_reads_terminal_automatic_fact_receipts() {
        use tracedecay_agent_hosts::automation::run_ledger::{
            AutomationRunStatus, AutomationTrigger,
        };
        use tracedecay_agent_hosts::automation::runner::MemoryCuratorAutomationRun;

        let temp = tempfile::tempdir().unwrap();
        let project_root = temp.path().join("project");
        let profile_root = temp.path().join("profile");
        std::fs::create_dir_all(&project_root).unwrap();
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

        let viewed = tool_json(
            &handle_admin_project(
                &cg,
                json!({ "action": "automatic_fact_receipt_view", "id": apply_id }),
                None,
                None,
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
            viewed["receipt"]["add_fact_request"]["source"],
            "admin-project-test"
        );
        assert!(viewed["receipt"]["applied_canonical_fact_id"].is_string());

        for action in [
            json!({ "action": "fact_apply", "id": apply_id }),
            json!({
                "action": "fact_reject",
                "id": apply_id,
                "reason": "not durable",
            }),
        ] {
            assert!(
                handle_admin_project(&cg, action, None, None).await.is_err(),
                "manual fact mutations must not be accepted"
            );
        }

        let automation = tool_json(
            &handle_admin_project(
                &cg,
                json!({
                    "action": "automation_run",
                    "task": "memory_curation",
                    "options": { "max_clusters": 9, "min_confidence": 0.7 }
                }),
                None,
                None,
            )
            .await
            .unwrap(),
        );
        let run = serde_json::from_value::<MemoryCuratorAutomationRun>(automation["run"].clone())
            .unwrap();
        assert_eq!(run.ledger_record.trigger, AutomationTrigger::ManualCli);
        assert_eq!(run.ledger_record.status, AutomationRunStatus::Skipped);
        assert!(matches!(
            run.report["reason"].as_str(),
            Some("automation_disabled" | "backend_disabled")
        ));

        let owner_after = crate::db::probe_writer_owner(&cg.store_layout().graph_db_path).unwrap();
        assert_eq!(owner_after, owner_before);
    }

    #[test]
    fn admin_project_wire_contract_round_trips_typed_results_without_local_fallback() {
        use tracedecay_agent_hosts::automation::runner::MemoryCuratorAutomationRun;

        assert!(matches!(
            serde_json::from_value::<AdminProjectAction>(json!({ "action": "gitignore_status" }))
                .unwrap(),
            AdminProjectAction::GitignoreStatus
        ));

        for retired_action in [
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

        let run_request = json!({
            "action": "automation_run",
            "task": "memory_curation",
            "options": { "max_clusters": 12, "min_confidence": 0.75 }
        });
        let action = serde_json::from_value::<AdminProjectAction>(run_request).unwrap();
        let AdminProjectAction::AutomationRun { task, options } = action else {
            panic!("manual automation request did not reach automation_run");
        };
        assert!(matches!(task, AutomationRunTask::MemoryCuration));
        let options = decode_options::<MemoryCurationOptions>(options).unwrap();
        assert_eq!(options.max_clusters, 12);
        assert!((options.min_confidence - 0.75).abs() < f64::EPSILON);
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

        let typed_run = serde_json::from_value::<MemoryCuratorAutomationRun>(json!({
            "run_id": "run-5",
            "report": { "status": "ok" },
            "ledger_record": {
                "schema_version": 1,
                "run_id": "run-5",
                "trigger": "manual_cli",
                "task": "memory_curator",
                "backend": "codex-app-server",
                "status": "succeeded",
                "accepted_count": 1,
                "rejected_count": 0,
                "started_at": "2026-01-01T00:00:00Z",
                "completed_at": "2026-01-01T00:00:01Z"
            }
        }))
        .unwrap();
        let response = json!({ "run": serde_json::to_value(&typed_run).unwrap() });
        let client_run = response.get("run").unwrap();
        let round_trip =
            serde_json::from_value::<MemoryCuratorAutomationRun>(client_run.clone()).unwrap();
        assert_eq!(round_trip, typed_run);
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

        let run = serde_json::from_value::<AdminProjectAction>(json!({
            "action": "automation_run",
            "task": "memory_curation",
            "options": { "max_clusters": 12, "min_confidence": 0.75 }
        }))
        .unwrap();
        assert!(matches!(
            run,
            AdminProjectAction::AutomationRun {
                task: AutomationRunTask::MemoryCuration,
                ..
            }
        ));
        assert!(
            decode_options::<MemoryCurationOptions>(json!({
                "max_clusters": 12,
                "min_confidence": 0.75,
                "unknown": true
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
}
