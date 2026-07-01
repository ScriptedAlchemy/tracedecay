use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::artifacts::sha256_json;
use super::backend::{AgentTaskBackend, AgentTaskKind, AgentTaskRequest, AgentTaskResponse};
use super::config::AutomationConfig;
use super::fact_proposals::{
    apply_fact_proposal, record_session_fact_proposals, FactProposalRecord, FactProposalState,
};
use super::lifecycle::{
    failed_backend_fallback_report, AgentTaskRunContext, BackendTaskRun, SchedulerGate,
};
use super::managed_skills::list_managed_skills;
use super::run_ledger::{AutomationRunLedgerRecord, AutomationTrigger};
use super::session_reflector::validate_fact_proposals;
use super::skill_usage::{
    ingest_project_analytics_events, stale_skill_recommendations, summarize_skill_usage,
};
use super::skill_writer::{
    activation_policy as skill_writer_activation_policy, skill_improvement_recommendations,
    support_file_evidence as skill_writer_support_file_evidence,
    validate_and_apply_skill_proposals,
};
use super::text::truncate_chars_for_prompt;
use crate::analytics::{underused_tool_family_signals, ToolUsageObservation};
use crate::errors::{Result, TraceDecayError};
use crate::global_db::GlobalDb;
use crate::sessions::cursor::{
    resolve_hermes_profile_session_db_readonly, HermesProfileDbReadOnly,
};
use crate::sessions::lcm::{LcmGrepRequest, LcmGrepSort, LcmScope};
use crate::tracedecay::{current_timestamp, TraceDecay};

pub use super::memory_curator::{
    run_memory_curator_with_backend, MemoryCuratorAutomationOptions, MemoryCuratorAutomationRun,
};

const SKILL_ANALYTICS_IMPORT_LIMIT: usize = 2_000;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionReflectorAutomationOptions {
    #[serde(default)]
    pub trigger: AutomationTrigger,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default = "default_lcm_storage_scope")]
    pub storage_scope: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hermes_home: Option<PathBuf>,
    #[serde(default = "default_session_provider")]
    pub provider: String,
    #[serde(default = "default_session_reflection_query")]
    pub query: String,
    #[serde(default = "default_lcm_grep_scope")]
    pub scope: LcmScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default = "default_include_summaries")]
    pub include_summaries: bool,
    #[serde(default = "default_session_evidence_limit")]
    pub evidence_limit: usize,
    #[serde(default = "default_lcm_grep_sort")]
    pub sort: LcmGrepSort,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_time: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_time: Option<i64>,
}

impl Default for SessionReflectorAutomationOptions {
    fn default() -> Self {
        Self {
            trigger: AutomationTrigger::ManualCli,
            run_id: None,
            storage_scope: default_lcm_storage_scope(),
            hermes_home: None,
            provider: default_session_provider(),
            query: default_session_reflection_query(),
            scope: default_lcm_grep_scope(),
            session_id: None,
            include_summaries: default_include_summaries(),
            evidence_limit: default_session_evidence_limit(),
            sort: default_lcm_grep_sort(),
            source: None,
            role: None,
            start_time: None,
            end_time: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionReflectorAutomationRun {
    pub run_id: String,
    pub report: Value,
    pub ledger_record: AutomationRunLedgerRecord,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend_response: Option<AgentTaskResponse>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SkillWriterAutomationOptions {
    #[serde(default)]
    pub trigger: AutomationTrigger,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default = "default_skill_writer_provider")]
    pub provider: String,
    #[serde(default = "default_skill_writer_query")]
    pub query: String,
    #[serde(default = "default_skill_writer_evidence_limit")]
    pub evidence_limit: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_root: Option<PathBuf>,
}

impl Default for SkillWriterAutomationOptions {
    fn default() -> Self {
        Self {
            trigger: AutomationTrigger::ManualCli,
            run_id: None,
            provider: default_skill_writer_provider(),
            query: default_skill_writer_query(),
            evidence_limit: default_skill_writer_evidence_limit(),
            profile_root: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SkillWriterAutomationRun {
    pub run_id: String,
    pub report: Value,
    pub ledger_record: AutomationRunLedgerRecord,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend_response: Option<AgentTaskResponse>,
}

struct SkillWriterEvidenceBundle {
    profile_root: PathBuf,
    evidence: Value,
    evidence_hash: Option<String>,
}

enum SkillWriterEvidenceOutcome {
    Ready(SkillWriterEvidenceBundle),
    Skipped {
        reason: &'static str,
        evidence_hash: Option<String>,
    },
}

enum SessionReflectorLcmStore {
    Available(PathBuf),
    NotIngested,
}

pub async fn run_session_reflector_with_backend(
    cg: &TraceDecay,
    config: &AutomationConfig,
    backend: &dyn AgentTaskBackend,
    options: SessionReflectorAutomationOptions,
) -> Result<SessionReflectorAutomationRun> {
    let run = AgentTaskRunContext::new(
        cg.store_layout().dashboard_root.clone(),
        options.run_id.clone(),
        "session_reflector",
        options.trigger,
        config,
        AgentTaskKind::SessionReflector,
    );
    let provider = normalized_non_empty(&options.provider).unwrap_or_else(default_session_provider);
    let query =
        normalized_non_empty(&options.query).unwrap_or_else(default_session_reflection_query);
    let evidence_limit = options.evidence_limit.clamp(1, 50);
    let storage_scope =
        normalized_non_empty(&options.storage_scope).unwrap_or_else(default_lcm_storage_scope);
    let session_id = options.session_id.as_deref().and_then(normalized_non_empty);
    let source = options.source.as_deref().and_then(normalized_non_empty);
    let role = options.role.as_deref().and_then(normalized_non_empty);

    let _run_lock = match run.gate().await? {
        SchedulerGate::Proceed(lock) => lock,
        SchedulerGate::Skip(reason) => {
            return skipped_session_reflector_run(&run, reason, None).await;
        }
    };

    let sessions_db_path = match session_reflector_lcm_db_path(cg, &storage_scope, &options)? {
        SessionReflectorLcmStore::Available(path) => path,
        SessionReflectorLcmStore::NotIngested => {
            return skipped_session_reflector_run(&run, "lcm_not_ingested", None).await;
        }
    };
    if !sessions_db_path.is_file() {
        return skipped_session_reflector_run(&run, "lcm_not_ingested", None).await;
    }
    let Some(lcm_db) = GlobalDb::open_read_only_at(&sessions_db_path).await else {
        return skipped_session_reflector_run(&run, "lcm_unavailable", None).await;
    };
    let hits = lcm_db
        .lcm_grep(LcmGrepRequest {
            provider: provider.clone(),
            query: query.clone(),
            scope: options.scope,
            session_id: session_id.clone(),
            include_summaries: options.include_summaries,
            limit: evidence_limit,
            sort: options.sort,
            source: source.clone(),
            role: role.clone(),
            start_time: options.start_time,
            end_time: options.end_time,
        })
        .await
        .map_err(|e| TraceDecayError::Config {
            message: format!("failed to build session reflection evidence: {e}"),
        })?;
    let evidence = json!({
        "storage_scope": storage_scope,
        "hermes_home": options.hermes_home.as_ref().map(|path| path.display().to_string()),
        "provider": provider,
        "query": query,
        "scope": options.scope,
        "session_id": session_id,
        "include_summaries": options.include_summaries,
        "sort": options.sort,
        "source": source,
        "role": role,
        "start_time": options.start_time,
        "end_time": options.end_time,
        "hits": hits,
    });
    let evidence_hash = Some(sha256_json(&evidence));
    if evidence
        .get("hits")
        .and_then(Value::as_array)
        .is_none_or(Vec::is_empty)
    {
        return skipped_session_reflector_run(&run, "no_session_evidence", evidence_hash).await;
    }

    let request = AgentTaskRequest::new(
        run.run_id.clone(),
        AgentTaskKind::SessionReflector,
        build_session_reflector_prompt(&evidence),
        evidence_hash.clone(),
        json!({
            "session_reflection_evidence": evidence,
            "apply": false,
        }),
    );
    let input_hash = Some(request.input_hash.clone());
    let finalizer = run.finalizer(input_hash.clone());
    let response = match finalizer
        .run_backend_or_fallback(backend, &request, evidence_hash.clone())
        .await?
    {
        BackendTaskRun::Response(response) => response,
        BackendTaskRun::Fallback(record) => {
            let record = *record;
            return Ok(SessionReflectorAutomationRun {
                run_id: record.run_id.clone(),
                report: failed_backend_fallback_report(&record),
                ledger_record: record,
                backend_response: None,
            });
        }
    };
    let (proposed_ops, proposals) = finalizer
        .response_output_array(
            &response,
            evidence_hash.clone(),
            "facts",
            "session reflector output must include a facts array",
        )
        .await?;
    let (accepted_facts, rejected_facts) =
        validate_fact_proposals(cg, &proposals, &evidence).await?;
    let accepted_count = accepted_facts.len();
    let rejected_count = rejected_facts.len();
    let mut proposal_records = record_session_fact_proposals(
        &run.dashboard_root,
        &run.run_id,
        evidence_hash.as_deref(),
        &accepted_facts,
        &rejected_facts,
    )
    .await?;
    let auto_apply_facts = config.auto_apply_memory_ops && !config.require_dashboard_approval;
    let applied_fact_proposals = if auto_apply_facts {
        auto_apply_session_fact_proposals(
            cg,
            &run.dashboard_root,
            std::mem::take(&mut proposal_records),
        )
        .await?
    } else {
        Vec::new()
    };
    if auto_apply_facts {
        proposal_records = applied_fact_proposals.clone();
    }
    let proposal_ids: Vec<String> = proposal_records
        .iter()
        .map(|record| record.proposal_id.clone())
        .collect();
    let applied_proposal_ids: Vec<String> = applied_fact_proposals
        .iter()
        .filter(|record| record.state == FactProposalState::Applied)
        .map(|record| record.proposal_id.clone())
        .collect();
    let applied_fact_ids: Vec<i64> = applied_fact_proposals
        .iter()
        .filter_map(|record| record.applied_fact_id)
        .collect();
    let session_fact_apply_policy = json!({
        "decision": if auto_apply_facts && accepted_count > 0 {
            "auto_apply_allowed"
        } else if config.require_dashboard_approval && accepted_count > 0 {
            "requires_dashboard_approval"
        } else if accepted_count > 0 {
            "proposal_only"
        } else {
            "no_valid_facts"
        },
        "auto_apply_memory_ops": config.auto_apply_memory_ops,
        "require_dashboard_approval": config.require_dashboard_approval,
        "mutates_store": !applied_proposal_ids.is_empty(),
        "applied_proposal_ids": applied_proposal_ids,
        "applied_fact_ids": applied_fact_ids,
    });
    let report = json!({
        "status": if auto_apply_facts && accepted_count > 0 { "auto_applied" } else { "needs_approval" },
        "dry_run": !(auto_apply_facts && accepted_count > 0),
        "task": "session_reflector",
        "evidence_hash": evidence_hash,
        "accepted_facts": accepted_facts,
        "rejected_facts": rejected_facts,
        "proposal_ids": proposal_ids,
        "proposal_records": proposal_records,
        "session_fact_apply_policy": session_fact_apply_policy,
    });
    let mut record = finalizer.success_record(
        &response,
        report
            .get("evidence_hash")
            .and_then(Value::as_str)
            .map(str::to_string),
        Some(json!({
            "facts": proposed_ops.get("facts").cloned().unwrap_or_else(|| json!([])),
            "accepted_facts": report.get("accepted_facts").cloned().unwrap_or_else(|| json!([])),
            "rejected_facts": report.get("rejected_facts").cloned().unwrap_or_else(|| json!([])),
            "proposal_ids": report.get("proposal_ids").cloned().unwrap_or_else(|| json!([])),
        })),
        accepted_count,
        rejected_count,
    );
    record.applied_ops = report
        .pointer("/session_fact_apply_policy/applied_proposal_ids")
        .filter(|value| value.as_array().is_some_and(|items| !items.is_empty()))
        .cloned();
    record.rejected_ops = report.get("rejected_facts").cloned();
    record.validation_report = Some(json!({
        "status": report.get("status").cloned().unwrap_or_else(|| json!("needs_approval")),
        "dry_run": report.get("dry_run").cloned().unwrap_or(json!(true)),
        "accepted_count": accepted_count,
        "rejected_count": rejected_count,
        "session_fact_apply_policy": report.get("session_fact_apply_policy").cloned().unwrap_or_else(|| json!({})),
        "pending_proposals": {
            "proposal_ids": report.get("proposal_ids").cloned().unwrap_or_else(|| json!([])),
            "accepted_facts": report.get("accepted_facts").cloned().unwrap_or_else(|| json!([])),
        },
    }));
    let record = finalizer
        .append_success_record(&request, &response, record)
        .await?;

    Ok(SessionReflectorAutomationRun {
        run_id: run.run_id,
        report,
        ledger_record: record,
        backend_response: Some(response),
    })
}

async fn auto_apply_session_fact_proposals(
    cg: &TraceDecay,
    dashboard_root: &std::path::Path,
    proposal_records: Vec<FactProposalRecord>,
) -> Result<Vec<FactProposalRecord>> {
    let project_db = cg.open_project_store_db().await?;
    let mut applied = Vec::with_capacity(proposal_records.len());
    for record in proposal_records {
        if record.state != FactProposalState::PendingApproval {
            applied.push(record);
            continue;
        }
        applied.push(
            apply_fact_proposal(
                dashboard_root,
                project_db.conn(),
                &record.proposal_id,
                Some("session_reflector:auto_apply".to_string()),
            )
            .await?,
        );
    }
    Ok(applied)
}

pub async fn run_skill_writer_with_backend(
    cg: &TraceDecay,
    config: &AutomationConfig,
    backend: &dyn AgentTaskBackend,
    options: SkillWriterAutomationOptions,
) -> Result<SkillWriterAutomationRun> {
    let run = AgentTaskRunContext::new(
        cg.store_layout().dashboard_root.clone(),
        options.run_id.clone(),
        "skill_writer",
        options.trigger,
        config,
        AgentTaskKind::SkillWriter,
    );
    let _run_lock = match run.gate().await? {
        SchedulerGate::Proceed(lock) => lock,
        SchedulerGate::Skip(reason) => {
            return skipped_skill_writer_run(&run, reason, None).await;
        }
    };

    let evidence_bundle = match build_skill_writer_evidence(cg, options).await? {
        SkillWriterEvidenceOutcome::Ready(bundle) => bundle,
        SkillWriterEvidenceOutcome::Skipped {
            reason,
            evidence_hash,
        } => return skipped_skill_writer_run(&run, reason, evidence_hash).await,
    };
    let SkillWriterEvidenceBundle {
        profile_root,
        evidence,
        evidence_hash,
    } = evidence_bundle;

    let activation_policy = skill_writer_activation_policy(config);
    let request = AgentTaskRequest::new(
        run.run_id.clone(),
        AgentTaskKind::SkillWriter,
        build_skill_writer_prompt(&evidence),
        evidence_hash.clone(),
        json!({
            "skill_writer_evidence": evidence,
            "apply": false,
            "activation_policy": activation_policy,
        }),
    );
    let input_hash = Some(request.input_hash.clone());
    let finalizer = run.finalizer(input_hash.clone());
    let response = match finalizer
        .run_backend_or_fallback(backend, &request, evidence_hash.clone())
        .await?
    {
        BackendTaskRun::Response(response) => response,
        BackendTaskRun::Fallback(record) => {
            let record = *record;
            return Ok(SkillWriterAutomationRun {
                run_id: record.run_id.clone(),
                report: failed_backend_fallback_report(&record),
                ledger_record: record,
                backend_response: None,
            });
        }
    };
    let (proposed_ops, proposals) = finalizer
        .response_output_array(
            &response,
            evidence_hash.clone(),
            "skills",
            "skill writer output must include a skills array",
        )
        .await?;
    let (created_skills, updated_skills, rejected_skills) =
        match validate_and_apply_skill_proposals(
            &profile_root,
            &run.run_id,
            &proposals,
            config.auto_enable_skills,
        )
        .await
        {
            Ok(result) => result,
            Err(err) => {
                finalizer
                    .append_failed_record(
                        response.model.clone(),
                        evidence_hash,
                        Some(proposed_ops),
                        err.to_string(),
                    )
                    .await?;
                return Err(err);
            }
        };
    let accepted_count = created_skills.len() + updated_skills.len();
    let rejected_count = rejected_skills.len();
    let report = json!({
        "status": if config.auto_enable_skills { "auto_enabled" } else { "needs_approval" },
        "dry_run": true,
        "task": "skill_writer",
        "evidence_hash": evidence_hash,
        "activation_policy": activation_policy,
        "created_skills": created_skills,
        "updated_skills": updated_skills,
        "rejected_skills": rejected_skills,
        "skill_improvement_recommendations": request.context
            .pointer("/skill_writer_evidence/skill_improvement_recommendations")
            .cloned()
            .unwrap_or_else(|| json!([])),
    });
    let mut record = finalizer.success_record(
        &response,
        report
            .get("evidence_hash")
            .and_then(Value::as_str)
            .map(str::to_string),
        Some(json!({
            "skills": proposed_ops.get("skills").cloned().unwrap_or_else(|| json!([])),
            "created_skills": report.get("created_skills").cloned().unwrap_or_else(|| json!([])),
            "updated_skills": report.get("updated_skills").cloned().unwrap_or_else(|| json!([])),
            "rejected_skills": report.get("rejected_skills").cloned().unwrap_or_else(|| json!([])),
        })),
        accepted_count,
        rejected_count,
    );
    record.applied_ops = Some(json!({
        "created_skills": report.get("created_skills").cloned().unwrap_or_else(|| json!([])),
        "updated_skills": report.get("updated_skills").cloned().unwrap_or_else(|| json!([])),
    }));
    record.rejected_ops = report.get("rejected_skills").cloned();
    record.validation_report = Some(json!({
        "status": report.get("status").cloned().unwrap_or_else(|| json!("needs_approval")),
        "dry_run": true,
        "activation_policy": activation_policy,
        "accepted_count": accepted_count,
        "rejected_count": rejected_count,
    }));
    let record = finalizer
        .append_success_record(&request, &response, record)
        .await?;

    Ok(SkillWriterAutomationRun {
        run_id: run.run_id,
        report,
        ledger_record: record,
        backend_response: Some(response),
    })
}

async fn build_skill_writer_evidence(
    cg: &TraceDecay,
    options: SkillWriterAutomationOptions,
) -> Result<SkillWriterEvidenceOutcome> {
    let profile_root = match options.profile_root {
        Some(path) => path,
        None => crate::storage::default_profile_root()?,
    };
    let provider =
        normalized_non_empty(&options.provider).unwrap_or_else(default_skill_writer_provider);
    let query = normalized_non_empty(&options.query).unwrap_or_else(default_skill_writer_query);
    let evidence_limit = options.evidence_limit.clamp(1, 50);

    let sessions_db_path = cg.store_layout().sessions_db_path.clone();
    if !sessions_db_path.is_file() {
        return Ok(SkillWriterEvidenceOutcome::Skipped {
            reason: "lcm_not_ingested",
            evidence_hash: None,
        });
    }
    let Some(lcm_db) = GlobalDb::open_read_only_at(&sessions_db_path).await else {
        return Ok(SkillWriterEvidenceOutcome::Skipped {
            reason: "lcm_unavailable",
            evidence_hash: None,
        });
    };
    let hits = lcm_db
        .lcm_grep(LcmGrepRequest {
            provider: provider.clone(),
            query: query.clone(),
            scope: LcmScope::All,
            session_id: None,
            include_summaries: true,
            limit: evidence_limit,
            sort: LcmGrepSort::Recency,
            source: None,
            role: None,
            start_time: None,
            end_time: None,
        })
        .await
        .map_err(|e| TraceDecayError::Config {
            message: format!("failed to build skill writer evidence: {e}"),
        })?;
    let existing_skills = list_managed_skills(&profile_root).await?;
    let global_db = GlobalDb::open().await;
    ingest_project_analytics_events(
        &profile_root,
        cg.project_root(),
        global_db.as_ref(),
        SKILL_ANALYTICS_IMPORT_LIMIT,
    )
    .await?;
    let skill_usage_summaries = summarize_skill_usage(&profile_root, &existing_skills).await?;
    let stale_recommendations = stale_skill_recommendations(
        &skill_usage_summaries,
        current_timestamp(),
        60 * 60 * 24 * 90,
    );
    let underused_tool_families = lcm_db
        .session_tool_usage_rows(10_000)
        .await
        .map(|rows| {
            underused_tool_family_signals(rows.iter().map(|row| ToolUsageObservation {
                tool_names: Some(row.tool_names.as_str()),
                metadata_json: Some(row.metadata_json.as_str()),
                text: Some(row.text.as_str()),
            }))
        })
        .unwrap_or_default();
    let skill_improvement_recommendations = skill_improvement_recommendations(
        &hits,
        &skill_usage_summaries,
        &stale_recommendations,
        &underused_tool_families,
    );
    let evidence = json!({
        "provider": provider,
        "query": query,
        "hits": hits,
        "skill_usage_summaries": skill_usage_summaries,
        "stale_recommendations": stale_recommendations,
        "underused_tool_families": underused_tool_families,
        "skill_improvement_recommendations": skill_improvement_recommendations,
        "existing_managed_skills": existing_skills
            .iter()
            .map(|skill| json!({
                "id": skill.metadata.id,
                "title": skill.metadata.title,
                "summary": skill.metadata.summary,
                "category": skill.metadata.category,
                "state": skill.metadata.state,
                "pinned": skill.metadata.pinned,
                "checksum": skill.metadata.checksum,
                "updated_at": skill.metadata.updated_at,
                "body_markdown": truncate_chars_for_prompt(&skill.body_markdown, 4000),
                "support_files": skill.support_files
                    .iter()
                    .map(skill_writer_support_file_evidence)
                    .collect::<Vec<_>>(),
            }))
            .collect::<Vec<_>>(),
    });
    let evidence_hash = Some(sha256_json(&evidence));
    if evidence
        .get("hits")
        .and_then(Value::as_array)
        .is_none_or(Vec::is_empty)
    {
        return Ok(SkillWriterEvidenceOutcome::Skipped {
            reason: "no_skill_writer_evidence",
            evidence_hash,
        });
    }

    Ok(SkillWriterEvidenceOutcome::Ready(
        SkillWriterEvidenceBundle {
            profile_root,
            evidence,
            evidence_hash,
        },
    ))
}

async fn skipped_session_reflector_run(
    run: &AgentTaskRunContext<'_>,
    reason: &str,
    evidence_hash: Option<String>,
) -> Result<SessionReflectorAutomationRun> {
    let (report, record) = run
        .skipped_parts(evidence_hash, reason, Some("session_reflector"))
        .await?;
    Ok(SessionReflectorAutomationRun {
        run_id: run.run_id.clone(),
        report,
        ledger_record: record,
        backend_response: None,
    })
}

async fn skipped_skill_writer_run(
    run: &AgentTaskRunContext<'_>,
    reason: &str,
    evidence_hash: Option<String>,
) -> Result<SkillWriterAutomationRun> {
    let (report, record) = run
        .skipped_parts(evidence_hash, reason, Some("skill_writer"))
        .await?;
    Ok(SkillWriterAutomationRun {
        run_id: run.run_id.clone(),
        report,
        ledger_record: record,
        backend_response: None,
    })
}

fn build_session_reflector_prompt(evidence: &Value) -> String {
    format!(
        "Review these bounded TraceDecay session snippets and propose only durable memory facts. Return only JSON with a facts array. Each fact must include content, category, optional tags, optional entities, trust, source_span, and reason. Category must be one of general, user_pref, project, tool, decision, or code_area. Use trust, not confidence. source_span must cite one bounded evidence hit by session_id plus message_id for raw messages, by store_id for raw messages, or by node_id for summaries. Do not include secrets or ephemeral status.\n{}",
        serde_json::to_string_pretty(evidence).unwrap_or_else(|_| "{}".to_string())
    )
}

fn build_skill_writer_prompt(evidence: &Value) -> String {
    format!(
        "Review these bounded TraceDecay session snippets and propose only reusable managed skills for repeated workflows, corrections, or tool-use patterns. Return only JSON with a skills array of managed skill creates or updates. New skills may omit action or use action=create and must include id, title, summary, category, body_markdown, optional targets, optional support_files with text content, and reason. Targets, when present, must be an array using cursor, codex, claude, agents, opencode, kimi, or kiro; Hermes is host-owned and must not be targeted. Updates must use action=update or action=patch, include id and base_checksum, and include at least one changed field among title, summary, category, targets, body_markdown/body, support_files, or pinned. For updates, support_files is a complete replacement list, not a partial file patch. Activation is controlled only by the runner policy; do not assume activation from your response.\n{}",
        serde_json::to_string_pretty(evidence).unwrap_or_else(|_| "{}".to_string())
    )
}

fn normalized_non_empty(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

fn session_reflector_lcm_db_path(
    cg: &TraceDecay,
    storage_scope: &str,
    options: &SessionReflectorAutomationOptions,
) -> Result<SessionReflectorLcmStore> {
    match storage_scope {
        "project_local" => Ok(SessionReflectorLcmStore::Available(
            cg.store_layout().sessions_db_path.clone(),
        )),
        "hermes_profile" => {
            let hermes_home = options.hermes_home.as_ref().ok_or_else(|| TraceDecayError::Config {
                message: "session_reflector hermes_profile storage requires hermes_home".to_string(),
            })?;
            match resolve_hermes_profile_session_db_readonly(hermes_home) {
                HermesProfileDbReadOnly::Exists(path) => Ok(SessionReflectorLcmStore::Available(path)),
                HermesProfileDbReadOnly::NotIngested(_) => Ok(SessionReflectorLcmStore::NotIngested),
                HermesProfileDbReadOnly::ConfigError(message) => Err(TraceDecayError::Config {
                    message: format!("invalid session_reflector hermes_home: {message}"),
                }),
            }
        }
        other => Err(TraceDecayError::Config {
            message: format!(
                "unknown session_reflector storage_scope '{other}'; expected project_local or hermes_profile"
            ),
        }),
    }
}

fn default_session_provider() -> String {
    "cursor".to_string()
}

fn default_skill_writer_provider() -> String {
    "all".to_string()
}

fn default_lcm_storage_scope() -> String {
    "project_local".to_string()
}

fn default_lcm_grep_scope() -> LcmScope {
    LcmScope::All
}

fn default_include_summaries() -> bool {
    true
}

fn default_lcm_grep_sort() -> LcmGrepSort {
    LcmGrepSort::Recency
}

fn default_session_reflection_query() -> String {
    "remember prefer decision requirement workflow".to_string()
}

fn default_session_evidence_limit() -> usize {
    20
}

fn default_skill_writer_query() -> String {
    "workflow correction repeated skill tool pattern".to_string()
}

fn default_skill_writer_evidence_limit() -> usize {
    20
}
