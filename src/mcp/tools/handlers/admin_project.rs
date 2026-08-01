//! Unadvertised daemon-owned project operations used by one-shot CLI commands.

use serde::Deserialize;
use serde_json::{Map, Value, json};
use tracedecay_domain::{ActorId, ProvenanceId};
use tracedecay_store::{
    CompatibilityFactProposalPromotionV1, CompatibilityFactProposalRecordV1,
    CompatibilityFactProposalStateV1, FactCompatibilityStoreError, FactProposalStoreError,
    FactStoreError,
};

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
    FactList {
        state: Option<String>,
        limit: usize,
    },
    FactView {
        id: String,
    },
    FactApply {
        id: String,
    },
    FactReject {
        id: String,
        reason: Option<String>,
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

fn fact_list_unavailable_payload(error: &MemoryApplicationError) -> Option<Value> {
    let reason = match error {
        MemoryApplicationError::Compatibility(
            FactCompatibilityStoreError::Store(FactStoreError::Storage { source, .. })
            | FactCompatibilityStoreError::Proposal(
                FactProposalStoreError::Store(FactStoreError::Storage { source, .. })
                | FactProposalStoreError::Storage { source, .. },
            ),
        ) => {
            let message = source.to_string();
            if message.contains("no such table")
                && ["memory_v2_proposal_current", "memory_v2_proposals"]
                    .iter()
                    .any(|table| message.contains(table))
            {
                "compatibility_proposal_bank_absent"
            } else if message.contains("no such column") {
                "compatibility_proposal_authority_incompatible"
            } else {
                "compatibility_proposal_authority_unavailable"
            }
        }
        MemoryApplicationError::Compatibility(
            FactCompatibilityStoreError::Store(FactStoreError::Contract(_))
            | FactCompatibilityStoreError::Proposal(FactProposalStoreError::Store(
                FactStoreError::Contract(_),
            )),
        ) => "compatibility_proposal_authority_incompatible",
        _ => return None,
    };
    Some(json!({
        "availability": {
            "state": "unavailable",
            "reason": reason,
        },
        "count": 0,
        "proposals": [],
        "next_after_proposal_id": null,
    }))
}

fn parse_proposal_id(value: String) -> Result<ProvenanceId> {
    ProvenanceId::new(value).map_err(|error| TraceDecayError::Config {
        message: format!("invalid fact proposal id: {error}"),
    })
}

fn cli_reviewer() -> Result<ActorId> {
    ActorId::new("cli".to_owned()).map_err(|error| TraceDecayError::Config {
        message: format!("invalid fact proposal reviewer: {error}"),
    })
}

fn parse_fact_proposal_state(value: &str) -> Result<CompatibilityFactProposalStateV1> {
    let normalized = value.trim().replace('-', "_");
    match normalized.as_str() {
        "pending" | "pending_approval" => Ok(CompatibilityFactProposalStateV1::PendingApproval),
        "applying" => Ok(CompatibilityFactProposalStateV1::Applying),
        "applied" => Ok(CompatibilityFactProposalStateV1::Applied),
        "rejected" | "rejected_validation" => Ok(CompatibilityFactProposalStateV1::Rejected),
        "quarantined" => Ok(CompatibilityFactProposalStateV1::Quarantined),
        _ => Err(TraceDecayError::Config {
            message: format!(
                "invalid fact proposal state `{value}`; expected pending_approval, applying, applied, rejected, or quarantined"
            ),
        }),
    }
}

fn fact_proposal_state_name(state: CompatibilityFactProposalStateV1) -> &'static str {
    match state {
        CompatibilityFactProposalStateV1::PendingApproval => "pending_approval",
        CompatibilityFactProposalStateV1::Applying => "applying",
        CompatibilityFactProposalStateV1::Applied => "applied",
        CompatibilityFactProposalStateV1::Rejected => "rejected",
        CompatibilityFactProposalStateV1::Quarantined => "quarantined",
    }
}

fn fact_proposal_json(proposal: &CompatibilityFactProposalRecordV1) -> Value {
    let request = proposal.request();
    let mut value = Map::from_iter([
        (
            "proposal_id".to_owned(),
            Value::String(proposal.proposal_id().as_str().to_owned()),
        ),
        ("revision".to_owned(), json!(proposal.revision().get())),
        (
            "state".to_owned(),
            Value::String(fact_proposal_state_name(proposal.state()).to_owned()),
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
    ]);
    if let Some(fact_id) = proposal.applied_fact_id() {
        value.insert(
            "applied_canonical_fact_id".to_owned(),
            Value::String(fact_id.as_str().to_owned()),
        );
    }
    if let Some(legacy_fact_id) = proposal.legacy_fact_id() {
        value.insert("applied_fact_id".to_owned(), json!(legacy_fact_id));
    }
    if let Some(reviewer) = proposal.reviewer() {
        value.insert(
            "reviewer".to_owned(),
            Value::String(reviewer.as_str().to_owned()),
        );
    }
    if let Some(reason) = proposal.reason() {
        value.insert("reason".to_owned(), Value::String(reason.to_owned()));
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
                .dashboard_overview_v1(1, 1)
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
        AdminProjectAction::FactList { state, limit } => {
            let db = cg.open_project_store_db().await?;
            let memory = project_memory_application(cg, &db)?;
            let state = state
                .as_deref()
                .map(parse_fact_proposal_state)
                .transpose()?;
            match memory
                .list_compatibility_fact_proposals(state, None, limit)
                .await
            {
                Ok(page) => {
                    let proposals = page
                        .proposals()
                        .iter()
                        .map(fact_proposal_json)
                        .collect::<Vec<_>>();
                    json!({
                        "availability": { "state": "available" },
                        "count": proposals.len(),
                        "proposals": proposals,
                        "next_after_proposal_id": page
                            .next_after_proposal_id()
                            .map(ProvenanceId::as_str),
                    })
                }
                Err(error) => fact_list_unavailable_payload(&error)
                    .ok_or_else(|| memory_application_error(error))?,
            }
        }
        AdminProjectAction::FactView { id } => {
            let proposal_id = parse_proposal_id(id)?;
            let db = cg.open_project_store_db().await?;
            let memory = project_memory_application(cg, &db)?;
            let proposal = memory
                .get_compatibility_fact_proposal(proposal_id)
                .await
                .map_err(memory_application_error)?
                .ok_or_else(|| TraceDecayError::Config {
                    message: "fact proposal not found".to_string(),
                })?;
            json!({ "proposal": fact_proposal_json(&proposal) })
        }
        AdminProjectAction::FactApply { id } => {
            let proposal_id = parse_proposal_id(id)?;
            let db = cg.open_project_store_db().await?;
            let memory = project_memory_application(cg, &db)?;
            let proposal = memory
                .get_compatibility_fact_proposal(proposal_id.clone())
                .await
                .map_err(memory_application_error)?
                .ok_or_else(|| TraceDecayError::Config {
                    message: "fact proposal not found".to_string(),
                })?;
            let promotion = CompatibilityFactProposalPromotionV1::new(
                memory.owner().clone(),
                proposal_id,
                proposal.revision(),
                Some(cli_reviewer()?),
            )
            .map_err(|error| TraceDecayError::Config {
                message: format!("invalid fact proposal promotion: {error}"),
            })?;
            let proposal = memory
                .promote_compatibility_fact_proposal(promotion)
                .await
                .map_err(memory_application_error)?;
            crate::automation::memory_digest::refresh_memory_digest_after_memory_change(
                &memory,
                cg.project_root(),
            )
            .await;
            json!({ "proposal": fact_proposal_json(&proposal) })
        }
        AdminProjectAction::FactReject { id, reason } => {
            let proposal_id = parse_proposal_id(id)?;
            let db = cg.open_project_store_db().await?;
            let memory = project_memory_application(cg, &db)?;
            let current = memory
                .get_compatibility_fact_proposal(proposal_id.clone())
                .await
                .map_err(memory_application_error)?
                .ok_or_else(|| TraceDecayError::Config {
                    message: "fact proposal not found".to_string(),
                })?;
            let proposal = memory
                .reject_compatibility_fact_proposal(
                    proposal_id,
                    current.revision(),
                    cli_reviewer()?,
                    reason.unwrap_or_else(|| "rejected by cli".to_string()),
                )
                .await
                .map_err(memory_application_error)?;
            json!({ "proposal": fact_proposal_json(&proposal) })
        }
        AdminProjectAction::AutomationRun { task, options } => {
            run_automation(cg, global_db, task, options).await?
        }
    };
    Ok(json_result(&value))
}

async fn run_automation(
    cg: &TraceDecay,
    global_db: Option<&RegisteredGlobalDb>,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn tool_json(result: &ToolResult) -> Value {
        let text = result.value["content"][0]["text"]
            .as_str()
            .expect("admin project result should contain JSON text");
        serde_json::from_str(text).expect("admin project result should be valid JSON")
    }

    async fn seed_compatibility_fact_proposal(
        cg: &TraceDecay,
        proposal_id: &str,
        content: &str,
    ) -> CompatibilityFactProposalRecordV1 {
        use tracedecay_domain::{Confidence, FactCategoryV1};
        use tracedecay_store::CompatibilityFactAddCommandV1;

        let owner = cg.project_memory_owner().unwrap();
        let db = cg.open_project_store_db().await.unwrap();
        let memory = MemoryApplication::new(owner.clone(), DatabaseFactStore::new(&db)).unwrap();
        let actor = ActorId::new("automation.session-reflector".to_owned()).unwrap();
        let request = CompatibilityFactAddCommandV1::new(
            owner,
            ProvenanceId::new(format!("automation.operation.{proposal_id}")).unwrap(),
            content.to_owned(),
            FactCategoryV1::Decision,
            None,
            vec![],
            vec![],
            json!({}),
            Confidence::new(0.9).unwrap(),
            Some(actor.clone()),
        )
        .unwrap();
        memory
            .submit_compatibility_fact_proposal(
                ProvenanceId::new(proposal_id.to_owned()).unwrap(),
                request,
                Some(actor),
            )
            .await
            .unwrap()
    }

    #[test]
    fn missing_compatibility_proposal_bank_is_a_typed_unavailable_list() {
        let error = MemoryApplicationError::Compatibility(
            tracedecay_store::FactCompatibilityStoreError::Store(
                tracedecay_store::FactStoreError::Storage {
                    operation: "read compatibility fact projection",
                    source: Box::new(std::io::Error::other(
                        "no such table: memory_v2_proposal_current",
                    )),
                },
            ),
        );

        assert_eq!(
            fact_list_unavailable_payload(&error),
            Some(json!({
                "availability": {
                    "state": "unavailable",
                    "reason": "compatibility_proposal_bank_absent",
                },
                "count": 0,
                "proposals": [],
                "next_after_proposal_id": null,
            }))
        );
    }

    #[tokio::test]
    async fn admin_project_handler_executes_typed_fact_and_automation_round_trips_on_one_authority()
    {
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

        let apply_id = "proposal.rpc.apply";
        let reject_id = "proposal.rpc.reject";
        seed_compatibility_fact_proposal(
            &cg,
            apply_id,
            "Admin project RPC applies this durable fact",
        )
        .await;
        seed_compatibility_fact_proposal(
            &cg,
            reject_id,
            "Admin project RPC rejects this durable fact",
        )
        .await;

        let pending = tool_json(
            &handle_admin_project(
                &cg,
                json!({
                    "action": "fact_list",
                    "state": "pending_approval",
                    "limit": 50,
                }),
                None,
                None,
            )
            .await
            .unwrap(),
        );
        assert_eq!(pending["count"], 2);
        assert_eq!(pending["availability"]["state"], "available");
        assert!(
            pending["proposals"]
                .as_array()
                .unwrap()
                .iter()
                .all(|proposal| proposal["state"] == "pending_approval")
        );

        let viewed = tool_json(
            &handle_admin_project(
                &cg,
                json!({ "action": "fact_view", "id": apply_id }),
                None,
                None,
            )
            .await
            .unwrap(),
        );
        assert_eq!(viewed["proposal"]["proposal_id"], apply_id);
        assert_eq!(viewed["proposal"]["state"], "pending_approval");
        assert_eq!(
            viewed["proposal"]["operation_id"],
            "automation.operation.proposal.rpc.apply"
        );
        assert_eq!(
            viewed["proposal"]["add_fact_request"]["content"],
            "Admin project RPC applies this durable fact"
        );
        assert_eq!(
            viewed["proposal"]["add_fact_request"]["category"],
            "decision"
        );
        assert_eq!(viewed["proposal"]["add_fact_request"]["trust"], json!(0.9));
        assert!(viewed["proposal"]["add_fact_request"]["source"].is_null());
        let viewed_proposal = viewed["proposal"].as_object().unwrap();
        assert!(!viewed_proposal.contains_key("applied_canonical_fact_id"));
        assert!(!viewed_proposal.contains_key("applied_fact_id"));

        let fact = tool_json(
            &handle_admin_project(
                &cg,
                json!({ "action": "fact_apply", "id": apply_id }),
                None,
                None,
            )
            .await
            .unwrap(),
        );
        assert_eq!(fact["proposal"]["proposal_id"], apply_id);
        assert_eq!(fact["proposal"]["state"], "applied");
        assert_eq!(fact["proposal"]["reviewer"], "cli");
        assert!(fact["proposal"]["applied_canonical_fact_id"].is_string());
        let applied_proposal = fact["proposal"].as_object().unwrap();
        assert!(matches!(
            applied_proposal.get("applied_fact_id"),
            None | Some(Value::Number(_))
        ));

        let rejected = tool_json(
            &handle_admin_project(
                &cg,
                json!({
                    "action": "fact_reject",
                    "id": reject_id,
                    "reason": "not durable",
                }),
                None,
                None,
            )
            .await
            .unwrap(),
        );
        assert_eq!(rejected["proposal"]["proposal_id"], reject_id);
        assert_eq!(rejected["proposal"]["state"], "rejected");
        assert_eq!(rejected["proposal"]["reviewer"], "cli");
        assert_eq!(rejected["proposal"]["reason"], "not durable");

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
        use crate::automation::runner::MemoryCuratorAutomationRun;

        assert!(matches!(
            serde_json::from_value::<AdminProjectAction>(json!({ "action": "gitignore_status" }))
                .unwrap(),
            AdminProjectAction::GitignoreStatus
        ));

        let fact_request = json!({ "action": "fact_apply", "id": "fact_1" });
        let fact = serde_json::from_value::<AdminProjectAction>(fact_request).unwrap();
        assert!(matches!(fact, AdminProjectAction::FactApply { id } if id == "fact_1"));

        let list = serde_json::from_value::<AdminProjectAction>(json!({
            "action": "fact_list",
            "state": "pending_approval",
            "limit": 50,
        }))
        .unwrap();
        assert!(matches!(
            list,
            AdminProjectAction::FactList { state: Some(state), limit: 50 }
                if state == "pending_approval"
        ));
        let view = serde_json::from_value::<AdminProjectAction>(json!({
            "action": "fact_view",
            "id": "fact_1",
        }))
        .unwrap();
        assert!(matches!(view, AdminProjectAction::FactView { id } if id == "fact_1"));
        let reject = serde_json::from_value::<AdminProjectAction>(json!({
            "action": "fact_reject",
            "id": "fact_1",
            "reason": "not durable",
        }))
        .unwrap();
        assert!(matches!(
            reject,
            AdminProjectAction::FactReject { id, reason: Some(reason) }
                if id == "fact_1" && reason == "not durable"
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
        let fact = serde_json::from_value::<AdminProjectAction>(json!({
            "action": "fact_apply",
            "id": "fact_1"
        }))
        .unwrap();
        assert!(matches!(fact, AdminProjectAction::FactApply { id } if id == "fact_1"));
        assert_eq!(
            parse_fact_proposal_state("pending_approval").unwrap(),
            CompatibilityFactProposalStateV1::PendingApproval
        );
        assert_eq!(
            parse_fact_proposal_state("rejected_validation").unwrap(),
            CompatibilityFactProposalStateV1::Rejected
        );
        assert_eq!(
            parse_fact_proposal_state(" rejected-validation ").unwrap(),
            CompatibilityFactProposalStateV1::Rejected
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
