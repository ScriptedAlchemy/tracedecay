//! Unadvertised daemon-owned project operations used by one-shot CLI commands.

use serde::Deserialize;
use serde_json::{Value, json};

use crate::errors::{Result, TraceDecayError};
use crate::global_db::GlobalDb;
use crate::tracedecay::TraceDecay;

use super::super::ToolResult;

static FACT_APPLY_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

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
    FactApply {
        id: String,
    },
    AutomationRun {
        task: AutomationRunTask,
        options: Value,
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
    scope: crate::sessions::lcm::LcmScope,
    session_id: Option<String>,
    include_summaries: bool,
    sort: crate::sessions::lcm::LcmGrepSort,
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

pub(super) async fn handle_admin_project(
    cg: &TraceDecay,
    args: Value,
    global_db: Option<&GlobalDb>,
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
            let status = cg.project_memory_status().await?;
            let db = cg.open_project_store_db().await?;
            let mut rows = db
                .conn()
                .query("SELECT COALESCE(MAX(fact_count), 0) FROM memory_banks", ())
                .await?;
            let largest_bank_fact_count = rows
                .next()
                .await?
                .and_then(|row| row.get::<i64>(0).ok())
                .unwrap_or(0)
                .max(0) as usize;
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
            crate::dashboard::run_memory_curate(cg, &options).await?
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
        AdminProjectAction::FactApply { id } => {
            let _guard = FACT_APPLY_LOCK.lock().await;
            let db = cg.open_project_store_db().await?;
            let proposal = crate::automation::fact_proposals::apply_fact_proposal(
                &cg.store_layout().dashboard_root,
                db.conn(),
                &id,
                Some("cli".to_string()),
            )
            .await?;
            crate::automation::memory_digest::refresh_memory_digest_after_memory_change(
                db.conn(),
                cg.project_root(),
            )
            .await;
            json!({ "proposal": proposal })
        }
        AdminProjectAction::AutomationRun { task, options } => {
            run_automation(cg, global_db, task, options).await?
        }
    };
    Ok(json_result(&value))
}

async fn run_automation(
    cg: &TraceDecay,
    global_db: Option<&GlobalDb>,
    task: AutomationRunTask,
    options: Value,
) -> Result<Value> {
    use crate::automation::backend::CodexAppServerBackend;
    use crate::automation::config::{AutomationBackend, effective_config, load_project_config};
    use crate::automation::run_ledger::AutomationTrigger;
    use crate::automation::runner::{
        MemoryCuratorAutomationOptions, SessionReflectorAutomationOptions,
        SkillWriterAutomationOptions, run_memory_curator_with_backend,
        run_session_reflector_with_backend, run_skill_writer_with_backend,
    };

    let profile_root = cg
        .open_options()
        .profile_root
        .or_else(|| {
            global_db
                .and_then(|db| db.db_path().parent())
                .map(std::path::Path::to_path_buf)
        })
        .ok_or_else(|| TraceDecayError::Config {
            message: "daemon project has no profile root".to_string(),
        })?;
    let config_path = profile_root.join("config.toml");
    let global: crate::user_config::UserConfig = std::fs::read_to_string(&config_path)
        .map(|contents| crate::user_config::parse_or_warn_default(&config_path, &contents))
        .unwrap_or_default();
    let project = load_project_config(&cg.store_layout().dashboard_root).await?;
    let config = effective_config(&global.automation, project.as_ref())?;
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

fn json_result(value: &Value) -> ToolResult {
    ToolResult::new(
        json!({
            "content": [{
                "type": "text",
                "text": serde_json::to_string(&value).unwrap_or_default(),
            }]
        }),
        Vec::new(),
    )
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

    fn automation_cli_source() -> String {
        [
            include_str!("../../../automation_cli/mod.rs"),
            include_str!("../../../automation_cli/config.rs"),
            include_str!("../../../automation_cli/facts.rs"),
            include_str!("../../../automation_cli/runs.rs"),
            include_str!("../../../automation_cli/skills.rs"),
        ]
        .concat()
    }

    #[tokio::test]
    async fn admin_project_handler_executes_typed_fact_and_automation_round_trips_on_one_authority()
    {
        use crate::automation::fact_proposals::{
            FactProposalRecord, FactProposalState, record_session_fact_proposals,
        };
        use crate::automation::run_ledger::{AutomationRunStatus, AutomationTrigger};
        use crate::automation::runner::MemoryCuratorAutomationRun;

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

        let proposals = record_session_fact_proposals(
            &cg.store_layout().dashboard_root,
            "rpc-run-1",
            None,
            &[json!({
                "add_fact_request": {
                    "content": "Admin project RPC applies this durable fact",
                    "category": "decision",
                    "source": null,
                    "tags": [],
                    "entities": [],
                    "trust": 0.9,
                    "metadata": {}
                }
            })],
            &[],
        )
        .await
        .unwrap();
        let fact = tool_json(
            &handle_admin_project(
                &cg,
                json!({ "action": "fact_apply", "id": proposals[0].proposal_id.clone() }),
                None,
            )
            .await
            .unwrap(),
        );
        let fact = serde_json::from_value::<FactProposalRecord>(fact["proposal"].clone()).unwrap();
        assert_eq!(fact.state, FactProposalState::Applied);
        assert_eq!(fact.reviewer.as_deref(), Some("cli"));

        let automation = tool_json(
            &handle_admin_project(
                &cg,
                json!({
                    "action": "automation_run",
                    "task": "memory_curation",
                    "options": { "max_clusters": 9, "min_confidence": 0.7 }
                }),
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
        let client_source = automation_cli_source();
        let direct_init = ["serve::ensure_", "initialized"].concat();
        let direct_apply = ["apply_fact_", "proposal("].concat();
        assert!(client_source.contains("tracedecay_admin_project"));
        assert!(!client_source.contains(&direct_init));
        assert!(!client_source.contains(&direct_apply));
    }

    #[test]
    fn admin_project_wire_contract_round_trips_typed_results_without_local_fallback() {
        use crate::automation::runner::MemoryCuratorAutomationRun;

        let fact_request = json!({ "action": "fact_apply", "id": "fact_1" });
        let fact = serde_json::from_value::<AdminProjectAction>(fact_request).unwrap();
        assert!(matches!(fact, AdminProjectAction::FactApply { id } if id == "fact_1"));

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

        let client_source = automation_cli_source();
        let direct_init = ["serve::ensure_", "initialized"].concat();
        let direct_apply = ["apply_fact_", "proposal("].concat();
        assert!(client_source.contains("tracedecay_admin_project"));
        assert!(!client_source.contains(&direct_init));
        assert!(!client_source.contains(&direct_apply));
    }

    #[test]
    fn automation_admin_actions_have_stable_strict_schemas() {
        let fact = serde_json::from_value::<AdminProjectAction>(json!({
            "action": "fact_apply",
            "id": "fact_1"
        }))
        .unwrap();
        assert!(matches!(fact, AdminProjectAction::FactApply { id } if id == "fact_1"));

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
        assert_eq!(session.scope, crate::sessions::lcm::LcmScope::Session);
        assert_eq!(session.sort, crate::sessions::lcm::LcmGrepSort::Hybrid);

        let skill = decode_options::<SkillWritingOptions>(json!({
            "provider": "all",
            "query": "repeated workflow",
            "evidence_limit": 13
        }))
        .unwrap();
        assert_eq!(skill.evidence_limit, 13);
    }
}
