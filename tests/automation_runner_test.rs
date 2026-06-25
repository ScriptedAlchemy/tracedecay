use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::{json, Value};
use tempfile::tempdir;

use tracedecay::automation::backend::{
    AgentTaskBackend, AgentTaskFailureClass, AgentTaskKind, AgentTaskRequest, AgentTaskResponse,
};
use tracedecay::automation::config::{
    AutomationBackend, AutomationConfig, AutomationHostMode, AutomationTaskConfig,
    AutomationTaskSet,
};
use tracedecay::automation::fact_proposals::{
    apply_fact_proposal, list_fact_proposals, FactProposalState,
};
use tracedecay::automation::managed_skills::{
    approve_managed_skill, create_managed_skill_draft, load_managed_skill, ManagedSkillDraft,
    ManagedSkillProvenance, ManagedSkillSource, ManagedSkillState, ManagedSupportFile,
};
use tracedecay::automation::run_ledger::{
    append_run_record, load_run_records, read_run_artifact_payload, AutomationRunLedgerRecord,
    AutomationRunStatus, AutomationTrigger,
};
use tracedecay::automation::runner::{
    run_memory_curator_with_backend, run_session_reflector_with_backend,
    run_skill_writer_with_backend, MemoryCuratorAutomationOptions,
    SessionReflectorAutomationOptions, SkillWriterAutomationOptions,
};
use tracedecay::errors::TraceDecayError;
use tracedecay::global_db::GlobalDb;
use tracedecay::memory::encoding::HolographicEncoder;
use tracedecay::sessions::cursor::resolve_hermes_profile_session_db_path;
use tracedecay::sessions::lcm::{LcmGrepSort, LcmScope};
use tracedecay::sessions::{SessionMessageRecord, SessionRecord};
use tracedecay::tracedecay::{current_timestamp, TraceDecay};

static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct JsonBackend {
    calls: AtomicUsize,
    output: Value,
    model: Option<String>,
}

impl JsonBackend {
    fn new(output: Value) -> Self {
        Self::new_with_model(output, Some("fixture-model"))
    }

    fn new_with_model(output: Value, model: Option<&str>) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            output,
            model: model.map(str::to_string),
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl AgentTaskBackend for JsonBackend {
    fn run_task(
        &self,
        request: &AgentTaskRequest,
    ) -> tracedecay::errors::Result<AgentTaskResponse> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        assert_eq!(request.task, AgentTaskKind::MemoryCurator);
        assert_request_contract(request, "memory_curator", "memory_curator:v1", "ops");
        assert!(
            request.prompt.contains("TraceDecay memory curation review"),
            "runner should build a task prompt from the curation messages"
        );
        assert_eq!(
            request.context["llm_review"]["status"],
            json!("needs_llm_review")
        );
        Ok(AgentTaskResponse {
            run_id: request.run_id.clone(),
            task: request.task,
            output_text: self.output.to_string(),
            output_json: Some(self.output.clone()),
            model: self.model.clone(),
            input_tokens: Some(10),
            output_tokens: Some(20),
        })
    }
}

struct SessionJsonBackend {
    calls: AtomicUsize,
    output: Value,
}

struct SkillJsonBackend {
    calls: AtomicUsize,
    output: Value,
    expected_activation_policy: &'static str,
}

struct SkillTextBackend {
    calls: AtomicUsize,
    output: &'static str,
}

struct InspectSkillWriterUsageBackend;

struct InspectSkillWriterUnderusedBackend;

struct FailingBackend {
    calls: AtomicUsize,
    task: AgentTaskKind,
    message: &'static str,
}

struct MalformedTextBackend {
    calls: AtomicUsize,
    task: AgentTaskKind,
    output: &'static str,
}

impl SkillJsonBackend {
    fn new(output: Value) -> Self {
        Self::with_activation_policy(output, "pending_approval_only")
    }

    fn with_activation_policy(output: Value, expected_activation_policy: &'static str) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            output,
            expected_activation_policy,
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl AgentTaskBackend for SkillJsonBackend {
    fn run_task(
        &self,
        request: &AgentTaskRequest,
    ) -> tracedecay::errors::Result<AgentTaskResponse> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        assert_eq!(request.task, AgentTaskKind::SkillWriter);
        assert_request_contract(request, "skill_writer", "skill_writer:v1", "skills");
        assert!(request.prompt.contains("managed skill creates or updates"));
        assert_eq!(request.context["apply"], json!(false));
        assert_eq!(
            request.context["activation_policy"],
            json!(self.expected_activation_policy)
        );
        assert!(request.context["skill_writer_evidence"]["hits"]
            .as_array()
            .is_some_and(|hits| !hits.is_empty()));
        let evidence = &request.context["skill_writer_evidence"];
        assert!(evidence["skill_usage_summaries"].is_array());
        assert!(evidence["stale_recommendations"].is_array());
        assert!(evidence["skill_improvement_recommendations"].is_array());
        if evidence["existing_managed_skills"]
            .as_array()
            .is_some_and(|skills| !skills.is_empty())
        {
            assert!(evidence["skill_usage_summaries"]
                .as_array()
                .is_some_and(|summaries| !summaries.is_empty()));
            assert!(evidence["stale_recommendations"]
                .as_array()
                .is_some_and(|recommendations| !recommendations.is_empty()));
            assert!(evidence["skill_improvement_recommendations"]
                .as_array()
                .is_some_and(|recommendations| !recommendations.is_empty()));
        }
        if let Some(support) = evidence["existing_managed_skills"]
            .as_array()
            .and_then(|skills| skills.first())
            .and_then(|skill| skill["support_files"].as_array())
            .and_then(|files| files.first())
        {
            assert_eq!(support["bytes"], json!(13));
            assert!(support["sha256"].as_str().is_some_and(|hash| {
                hash.starts_with("sha256:") && hash.len() == "sha256:".len() + 64
            }));
            assert_eq!(support["text_preview"], json!("old checklist"));
            assert_eq!(support["text_preview_chars"], json!(1200));
            assert_eq!(support["text_truncated"], json!(false));
        }
        Ok(AgentTaskResponse {
            run_id: request.run_id.clone(),
            task: request.task,
            output_text: self.output.to_string(),
            output_json: Some(self.output.clone()),
            model: Some("fixture-model".to_string()),
            input_tokens: Some(10),
            output_tokens: Some(20),
        })
    }
}

impl SkillTextBackend {
    fn new(output: &'static str) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            output,
        }
    }
}

impl AgentTaskBackend for InspectSkillWriterUsageBackend {
    fn run_task(
        &self,
        request: &AgentTaskRequest,
    ) -> tracedecay::errors::Result<AgentTaskResponse> {
        assert_eq!(request.task, AgentTaskKind::SkillWriter);
        assert_request_contract(request, "skill_writer", "skill_writer:v1", "skills");
        let summaries = request.context["skill_writer_evidence"]["skill_usage_summaries"]
            .as_array()
            .expect("skill usage summaries should be present");
        let summary = summaries
            .iter()
            .find(|summary| summary["skill_id"] == "automation-run-review")
            .expect("skill writer evidence should include automation-run-review usage");
        assert_eq!(summary["view_count"], json!(1));
        assert_eq!(summary["last_viewed_at"], json!(1_715_000_111_i64));
        assert!(summary["targets"]
            .as_array()
            .is_some_and(|targets| targets.contains(&json!("codex"))));
        let underused = request.context["skill_writer_evidence"]["underused_tool_families"]
            .as_array()
            .expect("underused tool families should be present");
        let code_search = underused
            .iter()
            .find(|family| family["family"] == "code_search")
            .expect("code_search family should be present");
        assert_eq!(code_search["relevant_events"], json!(1));
        assert_eq!(code_search["usage_events"], json!(0));
        assert_eq!(code_search["underused"], json!(true));
        Ok(AgentTaskResponse {
            run_id: request.run_id.clone(),
            task: request.task,
            output_text: json!({"skills": []}).to_string(),
            output_json: Some(json!({"skills": []})),
            model: Some("fixture-model".to_string()),
            input_tokens: Some(10),
            output_tokens: Some(20),
        })
    }
}

impl AgentTaskBackend for InspectSkillWriterUnderusedBackend {
    fn run_task(
        &self,
        request: &AgentTaskRequest,
    ) -> tracedecay::errors::Result<AgentTaskResponse> {
        assert_eq!(request.task, AgentTaskKind::SkillWriter);
        assert_request_contract(request, "skill_writer", "skill_writer:v1", "skills");
        let families = request.context["skill_writer_evidence"]["underused_tool_families"]
            .as_array()
            .expect("underused tool family evidence should be present");
        let code_search = families
            .iter()
            .find(|family| family["family"] == "code_search")
            .expect("code_search underuse evidence should be present");
        assert_eq!(code_search["relevant_events"], json!(1));
        assert_eq!(code_search["usage_events"], json!(0));
        assert_eq!(code_search["missed_events"], json!(1));
        assert_eq!(code_search["underused"], json!(true));
        let recommendations = request.context["skill_writer_evidence"]
            ["skill_improvement_recommendations"]
            .as_array()
            .expect("skill improvement recommendations should be present");
        assert!(recommendations.iter().any(|recommendation| {
            recommendation["id"] == "underused_tool_family:code_search"
                && recommendation["recommendation"] == "add_or_patch_skill_guidance"
                && recommendation["source"] == "session_tool_usage"
        }));
        Ok(AgentTaskResponse {
            run_id: request.run_id.clone(),
            task: request.task,
            output_text: json!({"skills": []}).to_string(),
            output_json: Some(json!({"skills": []})),
            model: Some("fixture-model".to_string()),
            input_tokens: Some(10),
            output_tokens: Some(20),
        })
    }
}

struct EnvVarGuard {
    key: &'static str,
    previous: Option<std::ffi::OsString>,
}

impl EnvVarGuard {
    fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
        let previous = std::env::var_os(key);
        std::env::set_var(key, value);
        Self { key, previous }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        if let Some(previous) = self.previous.take() {
            std::env::set_var(self.key, previous);
        } else {
            std::env::remove_var(self.key);
        }
    }
}

impl FailingBackend {
    fn new(task: AgentTaskKind) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            task,
            message: "codex app-server backend executable 'codex' was not found",
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl AgentTaskBackend for FailingBackend {
    fn run_task(
        &self,
        request: &AgentTaskRequest,
    ) -> tracedecay::errors::Result<AgentTaskResponse> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        assert_eq!(request.task, self.task);
        Err(TraceDecayError::Config {
            message: self.message.to_string(),
        })
    }
}

impl AgentTaskBackend for SkillTextBackend {
    fn run_task(
        &self,
        request: &AgentTaskRequest,
    ) -> tracedecay::errors::Result<AgentTaskResponse> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        assert_eq!(request.task, AgentTaskKind::SkillWriter);
        Ok(AgentTaskResponse {
            run_id: request.run_id.clone(),
            task: request.task,
            output_text: self.output.to_string(),
            output_json: None,
            model: Some("fixture-model".to_string()),
            input_tokens: Some(10),
            output_tokens: Some(20),
        })
    }
}

impl MalformedTextBackend {
    fn new(task: AgentTaskKind, output: &'static str) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            task,
            output,
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl AgentTaskBackend for MalformedTextBackend {
    fn run_task(
        &self,
        request: &AgentTaskRequest,
    ) -> tracedecay::errors::Result<AgentTaskResponse> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        assert_eq!(request.task, self.task);
        let (task_key, prompt_version, required_property) = match self.task {
            AgentTaskKind::MemoryCurator => ("memory_curator", "memory_curator:v1", "ops"),
            AgentTaskKind::SessionReflector => {
                ("session_reflector", "session_reflector:v1", "facts")
            }
            AgentTaskKind::SkillWriter => ("skill_writer", "skill_writer:v1", "skills"),
        };
        assert_request_contract(request, task_key, prompt_version, required_property);
        Ok(AgentTaskResponse {
            run_id: request.run_id.clone(),
            task: request.task,
            output_text: self.output.to_string(),
            output_json: None,
            model: Some("fixture-model".to_string()),
            input_tokens: Some(10),
            output_tokens: Some(20),
        })
    }
}

impl SessionJsonBackend {
    fn new(output: Value) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            output,
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl AgentTaskBackend for SessionJsonBackend {
    fn run_task(
        &self,
        request: &AgentTaskRequest,
    ) -> tracedecay::errors::Result<AgentTaskResponse> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        assert_eq!(request.task, AgentTaskKind::SessionReflector);
        assert_request_contract(
            request,
            "session_reflector",
            "session_reflector:v1",
            "facts",
        );
        assert!(request.prompt.contains("durable memory facts"));
        assert_eq!(request.context["apply"], json!(false));
        assert!(request.context["session_reflection_evidence"]["hits"]
            .as_array()
            .is_some_and(|hits| !hits.is_empty()));
        Ok(AgentTaskResponse {
            run_id: request.run_id.clone(),
            task: request.task,
            output_text: self.output.to_string(),
            output_json: Some(self.output.clone()),
            model: Some("fixture-model".to_string()),
            input_tokens: Some(10),
            output_tokens: Some(20),
        })
    }
}

struct InspectSessionEvidenceBackend;

impl AgentTaskBackend for InspectSessionEvidenceBackend {
    fn run_task(
        &self,
        request: &AgentTaskRequest,
    ) -> tracedecay::errors::Result<AgentTaskResponse> {
        assert_eq!(request.task, AgentTaskKind::SessionReflector);
        assert_request_contract(
            request,
            "session_reflector",
            "session_reflector:v1",
            "facts",
        );
        let evidence = &request.context["session_reflection_evidence"];
        assert_eq!(evidence["storage_scope"], json!("hermes_profile"));
        assert!(evidence["hermes_home"].as_str().is_some());
        assert_eq!(evidence["provider"], json!("cursor"));
        assert_eq!(evidence["query"], json!("profile-only banana"));
        assert_eq!(evidence["scope"], json!("session"));
        assert_eq!(evidence["session_id"], json!("hermes-reflect-1"));
        assert_eq!(evidence["include_summaries"], json!(false));
        assert_eq!(evidence["sort"], json!("relevance"));
        assert_eq!(evidence["source"], json!("hermes_profile_lcm"));
        assert_eq!(evidence["role"], json!("assistant"));
        assert_eq!(evidence["start_time"], json!(1_715_100_000_i64));
        assert_eq!(evidence["end_time"], json!(1_715_100_010_i64));
        let hits = evidence["hits"].as_array().expect("hits array");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0]["session_id"], json!("hermes-reflect-1"));
        assert!(hits[0]["snippet"]
            .as_str()
            .unwrap()
            .contains("profile-only banana"));
        Ok(AgentTaskResponse {
            run_id: request.run_id.clone(),
            task: request.task,
            output_text: json!({"facts": []}).to_string(),
            output_json: Some(json!({"facts": []})),
            model: Some("fixture-model".to_string()),
            input_tokens: Some(10),
            output_tokens: Some(20),
        })
    }
}

fn assert_request_contract(
    request: &AgentTaskRequest,
    task_key: &str,
    prompt_version: &str,
    required_property: &str,
) {
    assert_eq!(request.contract.task_key, task_key);
    assert_eq!(request.contract.prompt_version, prompt_version);
    assert!(request.contract.strict_json);
    assert_eq!(request.contract.response_schema["type"], json!("object"));
    assert_eq!(
        request.contract.response_schema["required"][0],
        json!(required_property)
    );
    assert_eq!(
        request.contract.response_schema["properties"][required_property]["type"],
        json!("array")
    );
    assert!(request.input_hash.starts_with("sha256:"));
    assert_ne!(
        request.evidence_hash.as_deref(),
        Some(request.input_hash.as_str())
    );
}

fn assert_noop_fallback_record(
    record: &AutomationRunLedgerRecord,
    task: AgentTaskKind,
    task_key: &str,
    expected_output: Value,
) {
    assert_eq!(record.task, task);
    assert_eq!(record.task_key.as_deref(), Some(task_key));
    assert_eq!(record.status, AutomationRunStatus::Failed);
    assert_eq!(record.reviewed_count, 0);
    assert_eq!(record.accepted_count, 0);
    assert_eq!(record.rejected_count, 0);
    assert_eq!(record.proposed_ops.as_ref(), Some(&expected_output));
    assert!(record
        .output_hash
        .as_deref()
        .is_some_and(|hash| hash.starts_with("sha256:")));
    assert_eq!(
        record.fallback_status.as_deref(),
        Some("backend_failed_noop")
    );
    assert_eq!(
        record.error_classification,
        Some(AgentTaskFailureClass::Unavailable)
    );
    assert_eq!(record.error_retryable, Some(true));
    assert!(record.evidence_hash.is_some());
    assert!(record.input_hash.is_some());
    assert!(record
        .error
        .as_deref()
        .is_some_and(|error| error.contains("executable")));
}

#[tokio::test]
async fn memory_curator_runner_skips_when_automation_is_disabled() {
    let temp = tempdir().unwrap();
    let cg = init_project(temp.path()).await;
    let backend = JsonBackend::new(json!({"ops": []}));

    let run = run_memory_curator_with_backend(
        &cg,
        &AutomationConfig::default(),
        &backend,
        MemoryCuratorAutomationOptions::default(),
    )
    .await
    .unwrap();

    assert_eq!(backend.calls(), 0);
    assert_eq!(run.ledger_record.status, AutomationRunStatus::Skipped);
    assert_eq!(
        run.ledger_record.error.as_deref(),
        Some("automation_disabled")
    );
    let records = load_run_records(&cg.store_layout().dashboard_root, 10)
        .await
        .unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].run_id, run.run_id);
}

#[tokio::test]
async fn memory_curator_runner_validates_backend_ops_and_records_ledger() {
    let temp = tempdir().unwrap();
    let cg = init_project(temp.path()).await;
    seed_duplicate_facts(&cg).await;
    let backend = JsonBackend::new(json!({
        "ops": [
            {
                "cluster_id": "cluster-0000",
                "op": "delete",
                "fact_id": 102,
                "confidence": 0.98,
                "reason": "near duplicate of fact 101"
            },
            {
                "cluster_id": "cluster-0000",
                "op": "delete",
                "fact_id": 999,
                "confidence": 0.98,
                "reason": "hallucinated id should be rejected"
            }
        ]
    }));
    let config = AutomationConfig {
        enabled: true,
        backend: AutomationBackend::CodexAppServer,
        host_mode: AutomationHostMode::Standalone,
        model: Some("configured-model".to_string()),
        tasks: AutomationTaskSet {
            memory_curator: AutomationTaskConfig {
                enabled: true,
                schedule: Some("manual".to_string()),
                ..AutomationTaskConfig::default()
            },
            ..AutomationTaskSet::default()
        },
        ..AutomationConfig::default()
    };

    let run = run_memory_curator_with_backend(
        &cg,
        &config,
        &backend,
        MemoryCuratorAutomationOptions {
            trigger: AutomationTrigger::ManualCli,
            max_clusters: 4,
            min_confidence: 0.5,
            run_id: None,
        },
    )
    .await
    .unwrap();

    assert_eq!(backend.calls(), 1);
    assert_eq!(run.ledger_record.schema_version, 2);
    assert_eq!(run.ledger_record.status, AutomationRunStatus::Succeeded);
    assert_eq!(
        run.ledger_record.task_key.as_deref(),
        Some("memory_curator")
    );
    assert_eq!(
        run.ledger_record.prompt_version.as_deref(),
        Some("memory_curator:v1")
    );
    assert_eq!(run.ledger_record.accepted_count, 1);
    assert_eq!(run.ledger_record.rejected_count, 1);
    assert_eq!(run.ledger_record.reviewed_count, 2);
    assert_eq!(run.ledger_record.skipped_count, 0);
    assert_eq!(run.ledger_record.backend, "codex_app_server");
    assert_eq!(run.ledger_record.host_mode.as_deref(), Some("standalone"));
    assert_eq!(run.ledger_record.model.as_deref(), Some("fixture-model"));
    assert!(run
        .ledger_record
        .evidence_hash
        .as_deref()
        .is_some_and(|hash| hash.starts_with("sha256:")));
    assert!(run
        .ledger_record
        .input_hash
        .as_deref()
        .is_some_and(|hash| hash.starts_with("sha256:")));
    assert!(run
        .ledger_record
        .output_hash
        .as_deref()
        .is_some_and(|hash| hash.starts_with("sha256:")));
    assert_eq!(
        run.ledger_record.applied_ops.as_ref().unwrap()[0]["fact_id"],
        json!(102)
    );
    assert_eq!(
        run.ledger_record.rejected_ops.as_ref().unwrap()[0]["rejected_reason"],
        json!("fact_id 999 was not in reviewed evidence")
    );
    assert_eq!(
        run.ledger_record.validation_report.as_ref().unwrap()["clusters_reviewed"],
        json!(1)
    );
    assert_eq!(
        run.ledger_record.validation_report.as_ref().unwrap()["apply_policy"]["decision"],
        json!("requires_dashboard_approval")
    );
    assert_eq!(
        run.ledger_record.validation_report.as_ref().unwrap()["apply_policy"]
            ["permanent_delete_count"],
        json!(1)
    );
    assert_eq!(
        run.ledger_record.validation_report.as_ref().unwrap()["apply_policy"]["mutates_store"],
        json!(false)
    );
    assert_eq!(
        run.report["automation_apply_policy"]["approval_required"],
        json!(true)
    );
    assert_eq!(
        run.ledger_record.report_ref.as_ref().unwrap()["run_id"],
        json!(run.run_id)
    );
    let artifact_kinds: Vec<&str> = run
        .ledger_record
        .artifacts
        .iter()
        .map(|artifact| artifact.kind.as_str())
        .collect();
    assert_eq!(
        artifact_kinds,
        vec![
            "traces",
            "feedback",
            "generated_evals",
            "validation_gate",
            "optimizer_diagnosis",
            "codex_handoff"
        ]
    );
    let validation_artifact = run
        .ledger_record
        .artifacts
        .iter()
        .find(|artifact| artifact.kind == "validation_gate")
        .unwrap();
    let validation_payload = read_run_artifact_payload(
        &cg.store_layout().dashboard_root,
        &run.run_id,
        validation_artifact,
    )
    .await
    .unwrap();
    assert_eq!(
        validation_payload["task_validation"]["decision"],
        json!("passed_with_rejections")
    );
    assert_eq!(validation_payload["loop_stage"], json!("validation_gate"));
    assert_eq!(
        validation_payload["improvement_gate"]["decision"],
        json!("ready_for_optimizer_review")
    );
    assert_eq!(
        validation_payload["improvement_gate"]["feedback_status"],
        json!("derived_from_validation")
    );
    assert_eq!(
        validation_payload["improvement_gate"]["generated_evals_status"],
        json!("passed")
    );
    assert_eq!(
        validation_payload["improvement_gate"]["criteria"]["has_feedback"],
        json!(true)
    );
    assert_eq!(
        validation_payload["improvement_gate"]["criteria"]["has_generated_evals"],
        json!(true)
    );
    assert_eq!(
        validation_payload["improvement_gate"]["criteria"]["auto_apply_allowed"],
        json!(false)
    );
    assert_eq!(
        validation_payload["improvement_gate"]["source_refs"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    assert_eq!(
        validation_payload["improvement_gate"]["optimizer_status"],
        json!("ready_for_optimizer_review")
    );
    assert!(validation_payload["improvement_gate"]["artifact_refs"]
        .as_array()
        .unwrap()
        .iter()
        .any(|reference| reference["kind"] == json!("generated_evals")
            && reference["sha256"]
                .as_str()
                .is_some_and(|hash| hash.starts_with("sha256:"))));
    let feedback_artifact = run
        .ledger_record
        .artifacts
        .iter()
        .find(|artifact| artifact.kind == "feedback")
        .unwrap();
    let feedback_payload = read_run_artifact_payload(
        &cg.store_layout().dashboard_root,
        &run.run_id,
        feedback_artifact,
    )
    .await
    .unwrap();
    assert_eq!(feedback_payload["status"], json!("derived_from_validation"));
    assert_eq!(feedback_payload["loop_stage"], json!("feedback"));
    assert_eq!(feedback_payload["source_refs"][0]["kind"], json!("traces"));
    assert_eq!(feedback_payload["summary"]["accepted_count"], json!(1));
    assert_eq!(feedback_payload["summary"]["rejected_count"], json!(1));
    assert_eq!(feedback_payload["summary"]["reviewed_count"], json!(2));
    assert_eq!(feedback_payload["human"], json!([]));
    assert!(feedback_payload["artifact_refs"]
        .as_array()
        .unwrap()
        .iter()
        .any(|reference| reference["kind"] == json!("traces")
            && reference["sha256"]
                .as_str()
                .is_some_and(|hash| hash.starts_with("sha256:"))));
    assert_eq!(feedback_payload["model"].as_array().unwrap().len(), 2);
    assert!(feedback_payload["model"]
        .as_array()
        .unwrap()
        .iter()
        .any(|entry| entry["outcome"] == json!("accepted")));
    assert!(feedback_payload["model"]
        .as_array()
        .unwrap()
        .iter()
        .any(|entry| entry["outcome"] == json!("rejected")
            && entry["reason"] == json!("fact_id 999 was not in reviewed evidence")));

    let eval_artifact = run
        .ledger_record
        .artifacts
        .iter()
        .find(|artifact| artifact.kind == "generated_evals")
        .unwrap();
    let eval_payload = read_run_artifact_payload(
        &cg.store_layout().dashboard_root,
        &run.run_id,
        eval_artifact,
    )
    .await
    .unwrap();
    assert_eq!(eval_payload["status"], json!("generated_from_validation"));
    assert_eq!(eval_payload["loop_stage"], json!("generated_evals"));
    assert_eq!(eval_payload["promotion"]["auto_apply"], json!(false));
    assert_eq!(eval_payload["source_refs"][0]["kind"], json!("traces"));
    assert_eq!(eval_payload["source_refs"][1]["kind"], json!("feedback"));
    assert_eq!(
        eval_payload["eval_definitions"].as_array().unwrap().len(),
        2
    );
    assert_eq!(
        eval_payload["format"],
        json!("tracedecay_automation_eval:v1")
    );
    assert_eq!(eval_payload["runner"]["type"], json!("validation_replay"));
    assert_eq!(
        eval_payload["runner"]["commands"][0],
        json!(
            "cargo test --test automation_runner_test memory_curator_runner_validates_backend_ops_and_records_ledger -- --nocapture"
        )
    );
    assert_eq!(
        eval_payload["runner"]["artifact_api"],
        json!(format!(
            "/api/automation/runs/{}/artifacts/generated_evals",
            run.run_id
        ))
    );
    assert_eq!(
        eval_payload["runner"]["inputs"]["artifact_kind"],
        json!("generated_evals")
    );
    assert_eq!(
        eval_payload["runner"]["inputs"]["expected_eval_count"],
        json!(2)
    );
    assert!(eval_payload["runner"]["inputs"]["validation_report_hash"]
        .as_str()
        .is_some_and(|hash| hash.starts_with("sha256:")));
    assert_eq!(
        eval_payload["runner"]["checks"].as_array().unwrap().len(),
        3
    );
    assert_eq!(eval_payload["runner"]["status"], json!("passed"));
    assert_eq!(
        eval_payload["runner"]["results"][0]["check"],
        json!("accepted_count_matches")
    );
    assert_eq!(
        eval_payload["runner"]["results"][0]["status"],
        json!("passed")
    );
    assert_eq!(eval_payload["promotion"]["state"], json!("validated"));
    assert_eq!(
        eval_payload["promotion"]["requires_human_review"],
        json!(true)
    );
    assert!(eval_payload["artifact_refs"]
        .as_array()
        .unwrap()
        .iter()
        .any(|reference| reference["kind"] == json!("feedback")
            && reference["sha256"]
                .as_str()
                .is_some_and(|hash| hash.starts_with("sha256:"))));
    assert!(eval_payload["eval_definitions"]
        .as_array()
        .unwrap()
        .iter()
        .any(|entry| entry["expected_outcome"] == json!("accepted")
            && entry["eval_id"] == json!("memory_curator:accepted:0")
            && entry["source_feedback_ref"] == json!("accepted:0")
            && entry["schema_version"] == json!(1)
            && entry["kind"] == json!("automation_validation_regression")
            && entry["harness"]["type"] == json!("cargo_test_filter")
            && entry["harness"]["commands"][0]
                == json!("cargo test --test automation_runner_test memory_curator")
            && entry["fixture"]["candidate"].is_object()
            && entry["source_feedback"]["artifact_kind"] == json!("feedback")
            && entry["source_feedback"]["feedback_id"] == json!("accepted:0")
            && entry["assertions"][0]["type"] == json!("outcome_equals")));
    assert!(eval_payload["eval_definitions"]
        .as_array()
        .unwrap()
        .iter()
        .any(|entry| entry["expected_outcome"] == json!("rejected")
            && entry["eval_id"] == json!("memory_curator:rejected:0")
            && entry["source_feedback_ref"] == json!("rejected:0")
            && entry["source_feedback"]["outcome"] == json!("rejected")
            && entry["expected"]["reason"] == json!("fact_id 999 was not in reviewed evidence")
            && entry["input"]["evidence_hash"] == json!(run.ledger_record.evidence_hash)
            && entry["input"]["input_hash"] == json!(run.ledger_record.input_hash)));
    assert_eq!(
        eval_payload["result_refs"][0]["kind"],
        json!("validation_report")
    );

    let optimizer_artifact = run
        .ledger_record
        .artifacts
        .iter()
        .find(|artifact| artifact.kind == "optimizer_diagnosis")
        .unwrap();
    let optimizer_payload = read_run_artifact_payload(
        &cg.store_layout().dashboard_root,
        &run.run_id,
        optimizer_artifact,
    )
    .await
    .unwrap();
    assert_eq!(optimizer_payload["status"], json!("generated"));
    assert_eq!(
        optimizer_payload["loop_stage"],
        json!("optimizer_diagnosis")
    );
    assert_eq!(optimizer_payload["signals"]["accepted_count"], json!(1));
    assert_eq!(optimizer_payload["signals"]["rejected_count"], json!(1));
    assert_eq!(optimizer_payload["signals"]["reviewed_count"], json!(2));
    assert_eq!(
        optimizer_payload["signals"]["validation_gate_decision"],
        json!("ready_for_optimizer_review")
    );
    assert!(optimizer_payload["artifact_refs"]
        .as_array()
        .unwrap()
        .iter()
        .any(|reference| reference["kind"] == json!("traces")));
    assert!(optimizer_payload["diagnostic_inputs"]
        .as_array()
        .unwrap()
        .iter()
        .any(|reference| reference["kind"] == json!("generated_evals")
            && reference["sha256"]
                .as_str()
                .is_some_and(|hash| hash.starts_with("sha256:"))));
    assert_eq!(optimizer_payload["blockers"], json!([]));
    assert_eq!(
        optimizer_payload["recommendations"][0]["id"],
        json!("review_rejections")
    );
    assert_eq!(
        optimizer_payload["ranked_changes"][0]["priority"],
        json!("high")
    );
    assert_eq!(
        optimizer_payload["ranked_changes"][0]["ready_for_codex_handoff"],
        json!(true)
    );
    let handoff_artifact = run
        .ledger_record
        .artifacts
        .iter()
        .find(|artifact| artifact.kind == "codex_handoff")
        .unwrap();
    let handoff_payload = read_run_artifact_payload(
        &cg.store_layout().dashboard_root,
        &run.run_id,
        handoff_artifact,
    )
    .await
    .unwrap();
    assert_eq!(handoff_payload["task"], json!("memory_curator"));
    assert_eq!(handoff_payload["loop_stage"], json!("codex_handoff"));
    assert_eq!(handoff_payload["status"], json!("ready_for_review"));
    assert_eq!(
        handoff_payload["readiness"]["validation_gate_decision"],
        json!("ready_for_optimizer_review")
    );
    assert_eq!(handoff_payload["readiness"]["eval_count"], json!(2));
    assert_eq!(
        handoff_payload["readiness"]["auto_apply_allowed"],
        json!(false)
    );
    assert_eq!(
        handoff_payload["machine_summary"]["next_stage"],
        json!("codex_review")
    );
    assert_eq!(
        handoff_payload["validation_requirements"]["must_not_auto_apply"],
        json!(true)
    );
    assert_eq!(
        handoff_payload["source_refs"][0]["kind"],
        json!("validation_gate")
    );
    assert!(handoff_payload["artifact_manifest"]["refs"]
        .as_array()
        .unwrap()
        .iter()
        .any(|reference| reference["kind"] == json!("optimizer_diagnosis")));
    assert_eq!(
        handoff_payload["artifact_manifest"]["api_list"],
        json!(format!("/api/automation/runs/{}/artifacts", run.run_id))
    );
    assert_eq!(
        handoff_payload["artifact_manifest"]["api_payloads"]["generated_evals"],
        json!(format!(
            "/api/automation/runs/{}/artifacts/generated_evals",
            run.run_id
        ))
    );
    assert_eq!(
        handoff_payload["eval_replay"]["artifact_api"],
        json!(format!(
            "/api/automation/runs/{}/artifacts/generated_evals",
            run.run_id
        ))
    );
    assert_eq!(
        handoff_payload["eval_replay"]["commands"][0],
        json!(
            "cargo test --test automation_runner_test memory_curator_runner_validates_backend_ops_and_records_ledger -- --nocapture"
        )
    );
    assert!(handoff_payload["request"]["evidence_hash"]
        .as_str()
        .is_some_and(|hash| hash.starts_with("sha256:")));
    assert_eq!(run.report["llm_apply"]["ops"][0]["fact_id"], json!(102));
    assert_eq!(
        run.report["llm_apply"]["rejected_ops"][0]["rejected_reason"],
        json!("fact_id 999 was not in reviewed evidence")
    );

    let records = load_run_records(&cg.store_layout().dashboard_root, 10)
        .await
        .unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].run_id, run.run_id);
    assert_eq!(records[0].accepted_count, 1);
    assert_eq!(records[0].rejected_count, 1);
    assert_eq!(records[0].artifacts.len(), 6);
    assert!(
        fact_exists(&cg, 102).await,
        "dry-run memory curator must not delete accepted ops before approval"
    );
}

#[tokio::test]
async fn memory_curator_runner_artifacts_block_handoff_without_validation_examples() {
    let temp = tempdir().unwrap();
    let cg = init_project(temp.path()).await;
    seed_duplicate_facts(&cg).await;
    let backend = JsonBackend::new(json!({ "ops": [] }));
    let config = AutomationConfig {
        enabled: true,
        backend: AutomationBackend::CodexAppServer,
        host_mode: AutomationHostMode::Standalone,
        tasks: AutomationTaskSet {
            memory_curator: AutomationTaskConfig {
                enabled: true,
                schedule: Some("manual".to_string()),
                ..AutomationTaskConfig::default()
            },
            ..AutomationTaskSet::default()
        },
        ..AutomationConfig::default()
    };

    let run = run_memory_curator_with_backend(
        &cg,
        &config,
        &backend,
        MemoryCuratorAutomationOptions {
            trigger: AutomationTrigger::ManualCli,
            max_clusters: 4,
            min_confidence: 0.5,
            run_id: None,
        },
    )
    .await
    .unwrap();

    assert_eq!(run.ledger_record.accepted_count, 0);
    assert_eq!(run.ledger_record.rejected_count, 0);
    assert_eq!(run.ledger_record.reviewed_count, 0);

    let eval_payload = read_artifact(&cg, &run.run_id, &run.ledger_record, "generated_evals").await;
    assert_eq!(eval_payload["summary"]["eval_count"], json!(0));
    assert_eq!(
        eval_payload["promotion"]["state"],
        json!("blocked_no_examples")
    );
    assert_eq!(eval_payload["eval_definitions"], json!([]));

    let validation_payload =
        read_artifact(&cg, &run.run_id, &run.ledger_record, "validation_gate").await;
    assert_eq!(
        validation_payload["task_validation"]["decision"],
        json!("no_valid_changes")
    );
    assert_eq!(
        validation_payload["improvement_gate"]["decision"],
        json!("blocked_pending_feedback_or_evals")
    );
    assert_eq!(
        validation_payload["improvement_gate"]["generated_evals_status"],
        json!("blocked_no_generated_evals")
    );
    assert_eq!(
        validation_payload["improvement_gate"]["optimizer_status"],
        json!("blocked")
    );
    assert_eq!(
        validation_payload["improvement_gate"]["handoff_status"],
        json!("blocked")
    );

    let optimizer_payload =
        read_artifact(&cg, &run.run_id, &run.ledger_record, "optimizer_diagnosis").await;
    assert_eq!(
        optimizer_payload["blockers"][0]["id"],
        json!("pending_feedback_or_evals")
    );

    let handoff_payload =
        read_artifact(&cg, &run.run_id, &run.ledger_record, "codex_handoff").await;
    assert_eq!(handoff_payload["status"], json!("blocked"));
    assert_eq!(
        handoff_payload["readiness"]["blockers"][0]["id"],
        json!("pending_feedback_or_evals")
    );
}

#[tokio::test]
async fn memory_curator_runner_artifacts_mark_handoff_ready_for_accepted_only_examples() {
    let temp = tempdir().unwrap();
    let cg = init_project(temp.path()).await;
    seed_duplicate_facts(&cg).await;
    let backend = JsonBackend::new(json!({
        "ops": [{
            "cluster_id": "cluster-0000",
            "op": "delete",
            "fact_id": 102,
            "confidence": 0.98,
            "reason": "near duplicate of fact 101"
        }]
    }));
    let config = AutomationConfig {
        enabled: true,
        backend: AutomationBackend::CodexAppServer,
        host_mode: AutomationHostMode::Standalone,
        tasks: AutomationTaskSet {
            memory_curator: AutomationTaskConfig {
                enabled: true,
                schedule: Some("manual".to_string()),
                ..AutomationTaskConfig::default()
            },
            ..AutomationTaskSet::default()
        },
        ..AutomationConfig::default()
    };

    let run = run_memory_curator_with_backend(
        &cg,
        &config,
        &backend,
        MemoryCuratorAutomationOptions {
            trigger: AutomationTrigger::ManualCli,
            max_clusters: 4,
            min_confidence: 0.5,
            run_id: None,
        },
    )
    .await
    .unwrap();

    assert_eq!(run.ledger_record.accepted_count, 1);
    assert_eq!(run.ledger_record.rejected_count, 0);

    let eval_payload = read_artifact(&cg, &run.run_id, &run.ledger_record, "generated_evals").await;
    assert_eq!(eval_payload["runner"]["status"], json!("passed"));
    assert_eq!(eval_payload["promotion"]["state"], json!("validated"));

    let validation_payload =
        read_artifact(&cg, &run.run_id, &run.ledger_record, "validation_gate").await;
    assert_eq!(
        validation_payload["task_validation"]["decision"],
        json!("passed")
    );
    assert_eq!(
        validation_payload["improvement_gate"]["decision"],
        json!("ready_for_handoff")
    );
    assert_eq!(
        validation_payload["improvement_gate"]["handoff_status"],
        json!("ready")
    );
    assert_eq!(
        validation_payload["improvement_gate"]["generated_evals_status"],
        json!("passed")
    );
    assert_eq!(
        validation_payload["improvement_gate"]["optimizer_status"],
        json!("ready_for_handoff")
    );

    let optimizer_payload =
        read_artifact(&cg, &run.run_id, &run.ledger_record, "optimizer_diagnosis").await;
    assert_eq!(optimizer_payload["blockers"], json!([]));

    let handoff_payload =
        read_artifact(&cg, &run.run_id, &run.ledger_record, "codex_handoff").await;
    assert_eq!(handoff_payload["status"], json!("ready_for_review"));
    assert_eq!(
        handoff_payload["readiness"]["validation_gate_decision"],
        json!("ready_for_handoff")
    );
    assert_eq!(
        handoff_payload["machine_summary"]["next_stage"],
        json!("codex_review")
    );
}

#[tokio::test]
async fn memory_curator_runner_auto_apply_is_blocked_by_dashboard_approval() {
    let temp = tempdir().unwrap();
    let cg = init_project(temp.path()).await;
    seed_duplicate_facts(&cg).await;
    let backend = JsonBackend::new(json!({
        "ops": [{
            "cluster_id": "cluster-0000",
            "op": "delete",
            "fact_id": 102,
            "confidence": 0.98,
            "reason": "near duplicate of fact 101"
        }]
    }));
    let config = AutomationConfig {
        enabled: true,
        backend: AutomationBackend::CodexAppServer,
        host_mode: AutomationHostMode::Standalone,
        auto_apply_memory_ops: true,
        require_dashboard_approval: true,
        tasks: AutomationTaskSet {
            memory_curator: AutomationTaskConfig {
                enabled: true,
                schedule: Some("manual".to_string()),
                ..AutomationTaskConfig::default()
            },
            ..AutomationTaskSet::default()
        },
        ..AutomationConfig::default()
    };

    let run = run_memory_curator_with_backend(
        &cg,
        &config,
        &backend,
        MemoryCuratorAutomationOptions {
            trigger: AutomationTrigger::ManualCli,
            max_clusters: 4,
            min_confidence: 0.5,
            run_id: None,
        },
    )
    .await
    .unwrap();

    assert_eq!(backend.calls(), 1);
    assert_eq!(
        run.report["automation_apply_policy"]["decision"],
        json!("requires_dashboard_approval")
    );
    assert_eq!(
        run.report["automation_apply_policy"]["auto_apply_memory_ops"],
        json!(true)
    );
    assert_eq!(
        run.report["automation_apply_policy"]["mutates_store"],
        json!(false)
    );
    assert_eq!(run.report["llm_apply"]["applied"], Value::Null);
    assert!(
        fact_exists(&cg, 102).await,
        "dashboard approval must block permanent delete auto-apply"
    );
}

#[tokio::test]
async fn memory_curator_runner_auto_applies_only_when_approval_is_not_required() {
    let temp = tempdir().unwrap();
    let cg = init_project(temp.path()).await;
    seed_duplicate_facts(&cg).await;
    let backend = JsonBackend::new(json!({
        "ops": [{
            "cluster_id": "cluster-0000",
            "op": "delete",
            "fact_id": 102,
            "confidence": 0.98,
            "reason": "near duplicate of fact 101"
        }]
    }));
    let config = AutomationConfig {
        enabled: true,
        backend: AutomationBackend::CodexAppServer,
        host_mode: AutomationHostMode::Standalone,
        auto_apply_memory_ops: true,
        require_dashboard_approval: false,
        tasks: AutomationTaskSet {
            memory_curator: AutomationTaskConfig {
                enabled: true,
                schedule: Some("manual".to_string()),
                ..AutomationTaskConfig::default()
            },
            ..AutomationTaskSet::default()
        },
        ..AutomationConfig::default()
    };

    let run = run_memory_curator_with_backend(
        &cg,
        &config,
        &backend,
        MemoryCuratorAutomationOptions {
            trigger: AutomationTrigger::ManualCli,
            max_clusters: 4,
            min_confidence: 0.5,
            run_id: None,
        },
    )
    .await
    .unwrap();

    assert_eq!(backend.calls(), 1);
    assert_eq!(
        run.report["automation_apply_policy"]["decision"],
        json!("auto_apply_allowed")
    );
    assert_eq!(
        run.report["automation_apply_policy"]["mutates_store"],
        json!(true)
    );
    assert_eq!(run.report["llm_apply"]["applied"], json!(1));
    assert_eq!(
        run.report["llm_apply"]["results"][0]["status"],
        json!("deleted")
    );
    assert!(
        !fact_exists(&cg, 102).await,
        "explicit no-approval auto-apply policy should delete accepted fact"
    );
}

#[tokio::test]
async fn memory_curator_runner_ledgers_malformed_backend_output() {
    let temp = tempdir().unwrap();
    let cg = init_project(temp.path()).await;
    seed_duplicate_facts(&cg).await;
    let backend = MalformedTextBackend::new(AgentTaskKind::MemoryCurator, "not json");
    let config = AutomationConfig {
        enabled: true,
        backend: AutomationBackend::CodexAppServer,
        host_mode: AutomationHostMode::Standalone,
        tasks: AutomationTaskSet {
            memory_curator: AutomationTaskConfig {
                enabled: true,
                schedule: Some("manual".to_string()),
                ..AutomationTaskConfig::default()
            },
            ..AutomationTaskSet::default()
        },
        ..AutomationConfig::default()
    };

    let err = run_memory_curator_with_backend(
        &cg,
        &config,
        &backend,
        MemoryCuratorAutomationOptions {
            trigger: AutomationTrigger::ManualCli,
            max_clusters: 4,
            min_confidence: 0.5,
            run_id: None,
        },
    )
    .await
    .unwrap_err();

    assert_eq!(backend.calls(), 1);
    assert!(
        err.to_string().contains("expected ident") || err.to_string().contains("expected value"),
        "unexpected error: {err}"
    );
    let records = load_run_records(&cg.store_layout().dashboard_root, 10)
        .await
        .unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].task, AgentTaskKind::MemoryCurator);
    assert_eq!(records[0].task_key.as_deref(), Some("memory_curator"));
    assert_eq!(records[0].status, AutomationRunStatus::Failed);
    assert_eq!(records[0].model.as_deref(), Some("fixture-model"));
    assert!(records[0].evidence_hash.is_some());
    assert!(records[0].input_hash.is_some());
    assert!(records[0].proposed_ops.is_none());
    assert!(records[0].error.as_deref().is_some_and(|error| {
        error.contains("expected ident") || error.contains("expected value")
    }));
    assert_eq!(
        records[0].error_classification,
        Some(AgentTaskFailureClass::MalformedOutput)
    );
    assert_eq!(records[0].error_retryable, Some(false));
}

#[tokio::test]
async fn memory_curator_runner_records_noop_fallback_when_backend_run_task_fails() {
    let temp = tempdir().unwrap();
    let cg = init_project(temp.path()).await;
    seed_duplicate_facts(&cg).await;
    let backend = FailingBackend::new(AgentTaskKind::MemoryCurator);
    let config = AutomationConfig {
        enabled: true,
        backend: AutomationBackend::CodexAppServer,
        host_mode: AutomationHostMode::Standalone,
        tasks: AutomationTaskSet {
            memory_curator: AutomationTaskConfig {
                enabled: true,
                schedule: Some("manual".to_string()),
                ..AutomationTaskConfig::default()
            },
            ..AutomationTaskSet::default()
        },
        ..AutomationConfig::default()
    };

    let run = run_memory_curator_with_backend(
        &cg,
        &config,
        &backend,
        MemoryCuratorAutomationOptions {
            trigger: AutomationTrigger::ManualCli,
            max_clusters: 4,
            min_confidence: 0.5,
            run_id: None,
        },
    )
    .await
    .unwrap();

    assert_eq!(backend.calls(), 1);
    assert_noop_fallback_record(
        &run.ledger_record,
        AgentTaskKind::MemoryCurator,
        "memory_curator",
        json!({ "ops": [] }),
    );
    assert!(run
        .ledger_record
        .error
        .as_deref()
        .is_some_and(|error| error.contains("executable")));
    let records = load_run_records(&cg.store_layout().dashboard_root, 10)
        .await
        .unwrap();
    assert_eq!(records.len(), 1);
    assert_noop_fallback_record(
        &records[0],
        AgentTaskKind::MemoryCurator,
        "memory_curator",
        json!({ "ops": [] }),
    );
}

#[tokio::test]
async fn session_reflector_runner_skips_when_task_is_disabled() {
    let temp = tempdir().unwrap();
    let cg = init_project(temp.path()).await;
    let backend = SessionJsonBackend::new(json!({"facts": []}));
    let config = AutomationConfig {
        enabled: true,
        backend: AutomationBackend::CodexAppServer,
        host_mode: AutomationHostMode::Standalone,
        ..AutomationConfig::default()
    };

    let run = run_session_reflector_with_backend(
        &cg,
        &config,
        &backend,
        SessionReflectorAutomationOptions::default(),
    )
    .await
    .unwrap();

    assert_eq!(backend.calls(), 0);
    assert_eq!(run.ledger_record.task, AgentTaskKind::SessionReflector);
    assert_eq!(run.ledger_record.status, AutomationRunStatus::Skipped);
    assert_eq!(
        run.ledger_record.error.as_deref(),
        Some("session_reflector_disabled")
    );
}

#[tokio::test]
async fn session_reflector_runner_validates_fact_proposals_without_applying() {
    let temp = tempdir().unwrap();
    let cg = init_project(temp.path()).await;
    seed_session_evidence(&cg).await;
    seed_duplicate_facts(&cg).await;
    let backend = SessionJsonBackend::new(json!({
        "facts": [
            {
                "content": "The project requires durable session reflection facts to stay approval gated",
                "category": "project",
                "tags": ["automation", "memory"],
                "entities": ["TraceDecay"],
                "trust": 0.72,
                "source_span": {"session_id": "session-reflect-1", "message_id": "session-reflect-1-message-001"},
                "reason": "Repeated session evidence describes the required approval gate"
            },
            {
                "content": "Cache invalidation policy must be explicit",
                "category": "project",
                "tags": ["cache"],
                "entities": ["TraceDecay"],
                "trust": 0.9,
                "source_span": {"session_id": "session-reflect-1", "message_id": "session-reflect-1-message-001"},
                "reason": "duplicate should be rejected"
            },
            {
                "content": "Uncited session reflection facts must not be accepted",
                "category": "project",
                "tags": ["automation"],
                "entities": ["TraceDecay"],
                "trust": 0.7,
                "reason": "missing citation should be rejected"
            },
            {
                "content": "Session reflection citations must point at bounded evidence",
                "category": "project",
                "tags": ["automation"],
                "entities": ["TraceDecay"],
                "trust": 0.7,
                "source_span": {"session_id": "session-reflect-1", "message_id": "missing-message"},
                "reason": "bogus citation should be rejected"
            },
            {
                "content": "Session reflection facts require calibrated trust",
                "category": "project",
                "tags": ["automation"],
                "entities": ["TraceDecay"],
                "source_span": {"session_id": "session-reflect-1", "message_id": "session-reflect-1-message-001"},
                "reason": "missing trust should be rejected"
            },
            {
                "content": "Session reflection facts require a rationale",
                "category": "project",
                "tags": ["automation"],
                "entities": ["TraceDecay"],
                "trust": 0.7,
                "source_span": {"session_id": "session-reflect-1", "message_id": "session-reflect-1-message-001"}
            },
            {
                "content": "Session reflector uses trust rather than confidence",
                "category": "project",
                "tags": ["automation"],
                "entities": ["TraceDecay"],
                "trust": 0.7,
                "confidence": 0.9,
                "source_span": {"session_id": "session-reflect-1", "message_id": "session-reflect-1-message-001"},
                "reason": "confidence should be rejected"
            },
            {
                "content": "",
                "category": "project"
            }
        ]
    }));
    let config = AutomationConfig {
        enabled: true,
        backend: AutomationBackend::CodexAppServer,
        host_mode: AutomationHostMode::Standalone,
        model: Some("configured-model".to_string()),
        tasks: AutomationTaskSet {
            session_reflector: AutomationTaskConfig {
                enabled: true,
                schedule: Some("manual".to_string()),
                ..AutomationTaskConfig::default()
            },
            ..AutomationTaskSet::default()
        },
        ..AutomationConfig::default()
    };

    let run = run_session_reflector_with_backend(
        &cg,
        &config,
        &backend,
        SessionReflectorAutomationOptions {
            trigger: AutomationTrigger::ManualCli,
            provider: "cursor".to_string(),
            query: "durable session reflection".to_string(),
            evidence_limit: 5,
            run_id: None,
            ..SessionReflectorAutomationOptions::default()
        },
    )
    .await
    .unwrap();

    assert_eq!(backend.calls(), 1);
    assert_eq!(run.ledger_record.task, AgentTaskKind::SessionReflector);
    assert_eq!(run.ledger_record.status, AutomationRunStatus::Succeeded);
    assert_eq!(run.ledger_record.accepted_count, 1);
    assert_eq!(run.ledger_record.rejected_count, 7);
    assert_eq!(
        run.report["accepted_facts"][0]["add_fact_request"]["source"],
        json!("session_reflector")
    );
    assert_eq!(
        run.report["accepted_facts"][0]["add_fact_request"]["category"],
        json!("project")
    );
    assert_eq!(
        run.report["accepted_facts"][0]["add_fact_request"]["metadata"]["source_span"],
        json!({"session_id": "session-reflect-1", "message_id": "session-reflect-1-message-001"})
    );
    assert_eq!(
        run.report["accepted_facts"][0]["add_fact_request"]["metadata"]["trust_reason"],
        json!("Repeated session evidence describes the required approval gate")
    );
    let rejected = run.report["rejected_facts"].as_array().unwrap();
    assert!(rejected
        .iter()
        .any(|value| value["reason"].as_str().unwrap().contains("duplicate")));
    let has_rejection_reason = |reason: &str| {
        rejected
            .iter()
            .any(|value| value["reason"] == json!(reason))
    };
    assert!(has_rejection_reason("content is required"));
    assert!(has_rejection_reason("source_span is required"));
    assert!(has_rejection_reason(
        "source_span must cite a bounded session reflection evidence hit"
    ));
    assert!(has_rejection_reason("trust is required"));
    assert!(has_rejection_reason("reason is required"));
    assert!(has_rejection_reason(
        "confidence is not supported; use trust"
    ));
    let proposals = list_fact_proposals(
        &cg.store_layout().dashboard_root,
        Some(FactProposalState::PendingApproval),
        10,
    )
    .await
    .unwrap();
    assert_eq!(proposals.len(), 1);
    assert_eq!(proposals[0].run_id, run.run_id);
    assert_eq!(
        proposals[0].add_fact_request.as_ref().unwrap().content,
        "The project requires durable session reflection facts to stay approval gated"
    );
    assert_eq!(
        proposals[0].validation.as_ref().unwrap()["dedupe"]["near_duplicate_threshold"],
        json!(0.9)
    );
    assert_eq!(
        run.report["proposal_ids"][0],
        json!(proposals[0].proposal_id)
    );
    assert!(run.ledger_record.applied_ops.is_none());
    assert_eq!(
        run.ledger_record.validation_report.as_ref().unwrap()["pending_proposals"]["proposal_ids"]
            [0],
        json!(proposals[0].proposal_id)
    );
    assert_eq!(
        run.ledger_record.validation_report.as_ref().unwrap()["pending_proposals"]
            ["accepted_facts"][0]["add_fact_request"]["content"],
        json!("The project requires durable session reflection facts to stay approval gated")
    );
    let artifact_kinds: Vec<&str> = run
        .ledger_record
        .artifacts
        .iter()
        .map(|artifact| artifact.kind.as_str())
        .collect();
    assert_eq!(
        artifact_kinds,
        vec![
            "traces",
            "feedback",
            "generated_evals",
            "validation_gate",
            "optimizer_diagnosis",
            "codex_handoff"
        ]
    );
    let eval_payload = read_artifact(&cg, &run.run_id, &run.ledger_record, "generated_evals").await;
    assert_eq!(eval_payload["task"], json!("session_reflector"));
    assert_eq!(eval_payload["summary"]["eval_count"], json!(8));
    assert!(eval_payload["eval_definitions"]
        .as_array()
        .unwrap()
        .iter()
        .any(
            |entry| entry["eval_id"] == json!("session_reflector:accepted:0")
                && entry["harness"]["commands"][0]
                    == json!("cargo test --test automation_runner_test session_reflector")
        ));
    assert_eq!(
        eval_payload["runner"]["commands"][0],
        json!(
            "cargo test --test automation_runner_test session_reflector_runner_validates_fact_proposals_without_applying -- --nocapture"
        )
    );
    let handoff_payload =
        read_artifact(&cg, &run.run_id, &run.ledger_record, "codex_handoff").await;
    assert_eq!(handoff_payload["task"], json!("session_reflector"));
    assert_eq!(
        handoff_payload["next_actions"][0],
        json!("review pending fact proposals")
    );
    assert_eq!(
        handoff_payload["eval_replay"]["commands"][0],
        json!(
            "cargo test --test automation_runner_test session_reflector_runner_validates_fact_proposals_without_applying -- --nocapture"
        )
    );
    let before_apply = cg
        .search_facts(tracedecay::memory::types::SearchFactsRequest {
            query: "durable session reflection facts approval gated".to_string(),
            category: Some(tracedecay::memory::types::MemoryCategory::Project),
            limit: Some(10),
            min_trust: Some(0.1),
            include_why: false,
        })
        .await
        .unwrap();
    assert!(
        before_apply
            .iter()
            .all(|hit| hit.fact.source.as_deref() != Some("session_reflector")),
        "session reflector should not write accepted facts before proposal approval"
    );

    let project_db = cg.open_project_store_db().await.unwrap();
    let applied = apply_fact_proposal(
        &cg.store_layout().dashboard_root,
        project_db.conn(),
        &proposals[0].proposal_id,
        Some("test".to_string()),
    )
    .await
    .unwrap();
    assert_eq!(applied.state, FactProposalState::Applied);
    assert!(applied.apply_outcome.is_some());
    let after_apply = cg
        .search_facts(tracedecay::memory::types::SearchFactsRequest {
            query: "durable session reflection facts approval gated".to_string(),
            category: Some(tracedecay::memory::types::MemoryCategory::Project),
            limit: Some(10),
            min_trust: Some(0.1),
            include_why: false,
        })
        .await
        .unwrap();
    assert!(
        after_apply
            .iter()
            .any(|hit| hit.fact.source.as_deref() == Some("session_reflector")),
        "approving the proposal should apply it to the fact store"
    );

    let records = load_run_records(&cg.store_layout().dashboard_root, 10)
        .await
        .unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].run_id, run.run_id);
    assert_eq!(records[0].accepted_count, 1);
    assert_eq!(records[0].rejected_count, 7);
    assert!(records[0].applied_ops.is_none());
}

#[tokio::test]
async fn session_reflector_runner_reads_hermes_profile_lcm_with_filters() {
    let temp = tempdir().unwrap();
    let cg = init_project(temp.path()).await;
    seed_session_evidence(&cg).await;

    let hermes_home = tempdir().unwrap();
    let profile_db_path = resolve_hermes_profile_session_db_path(hermes_home.path()).unwrap();
    let profile_db = GlobalDb::open_at(&profile_db_path)
        .await
        .expect("hermes profile session db open");
    seed_session_message_in_db(
        &profile_db,
        hermes_home.path(),
        SeedSessionMessage {
            provider: "cursor",
            session_id: "hermes-reflect-1",
            message_id: "hermes-reflect-1-message-001",
            role: "assistant",
            timestamp: 1_715_100_005,
            text: "Hermes profile-only banana evidence should feed session reflection.",
            source: Some("hermes_profile_lcm"),
        },
    )
    .await;
    seed_session_message_in_db(
        &profile_db,
        hermes_home.path(),
        SeedSessionMessage {
            provider: "cursor",
            session_id: "hermes-reflect-1",
            message_id: "hermes-reflect-1-message-002",
            role: "user",
            timestamp: 1_715_100_006,
            text: "Hermes profile-only banana distractor has the wrong role.",
            source: Some("hermes_profile_lcm"),
        },
    )
    .await;

    let backend = InspectSessionEvidenceBackend;
    let config = AutomationConfig {
        enabled: true,
        backend: AutomationBackend::CodexAppServer,
        host_mode: AutomationHostMode::Standalone,
        tasks: AutomationTaskSet {
            session_reflector: AutomationTaskConfig {
                enabled: true,
                schedule: Some("manual".to_string()),
                ..AutomationTaskConfig::default()
            },
            ..AutomationTaskSet::default()
        },
        ..AutomationConfig::default()
    };

    let run = run_session_reflector_with_backend(
        &cg,
        &config,
        &backend,
        SessionReflectorAutomationOptions {
            trigger: AutomationTrigger::ManualCli,
            storage_scope: "hermes_profile".to_string(),
            hermes_home: Some(hermes_home.path().to_path_buf()),
            provider: "cursor".to_string(),
            query: "profile-only banana".to_string(),
            scope: LcmScope::Session,
            session_id: Some("hermes-reflect-1".to_string()),
            include_summaries: false,
            evidence_limit: 5,
            sort: LcmGrepSort::Relevance,
            source: Some("hermes_profile_lcm".to_string()),
            role: Some("assistant".to_string()),
            start_time: Some(1_715_100_000),
            end_time: Some(1_715_100_010),
            run_id: None,
        },
    )
    .await
    .unwrap();

    assert_eq!(run.ledger_record.status, AutomationRunStatus::Succeeded);
    assert_eq!(run.ledger_record.accepted_count, 0);
    assert_eq!(run.ledger_record.rejected_count, 0);
}

#[tokio::test]
async fn session_reflector_runner_ledgers_malformed_backend_output() {
    let temp = tempdir().unwrap();
    let cg = init_project(temp.path()).await;
    seed_session_evidence(&cg).await;
    let backend = MalformedTextBackend::new(AgentTaskKind::SessionReflector, "not json");
    let config = AutomationConfig {
        enabled: true,
        backend: AutomationBackend::CodexAppServer,
        host_mode: AutomationHostMode::Standalone,
        tasks: AutomationTaskSet {
            session_reflector: AutomationTaskConfig {
                enabled: true,
                schedule: Some("manual".to_string()),
                ..AutomationTaskConfig::default()
            },
            ..AutomationTaskSet::default()
        },
        ..AutomationConfig::default()
    };

    let err = run_session_reflector_with_backend(
        &cg,
        &config,
        &backend,
        SessionReflectorAutomationOptions {
            trigger: AutomationTrigger::ManualCli,
            provider: "cursor".to_string(),
            query: "durable session reflection".to_string(),
            evidence_limit: 5,
            run_id: None,
            ..SessionReflectorAutomationOptions::default()
        },
    )
    .await
    .unwrap_err();

    assert_eq!(backend.calls(), 1);
    assert!(
        err.to_string().contains("expected ident") || err.to_string().contains("expected value"),
        "unexpected error: {err}"
    );
    let records = load_run_records(&cg.store_layout().dashboard_root, 10)
        .await
        .unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].task, AgentTaskKind::SessionReflector);
    assert_eq!(records[0].task_key.as_deref(), Some("session_reflector"));
    assert_eq!(records[0].status, AutomationRunStatus::Failed);
    assert_eq!(records[0].model.as_deref(), Some("fixture-model"));
    assert!(records[0].evidence_hash.is_some());
    assert!(records[0].input_hash.is_some());
    assert!(records[0].proposed_ops.is_none());
    assert!(records[0].error.as_deref().is_some_and(|error| {
        error.contains("expected ident") || error.contains("expected value")
    }));
    assert_eq!(
        records[0].error_classification,
        Some(AgentTaskFailureClass::MalformedOutput)
    );
    assert_eq!(records[0].error_retryable, Some(false));
}

#[tokio::test]
async fn session_reflector_runner_ledgers_missing_facts_array() {
    let temp = tempdir().unwrap();
    let cg = init_project(temp.path()).await;
    seed_session_evidence(&cg).await;
    let output = json!({"summary": "no facts"});
    let backend = SessionJsonBackend::new(output.clone());
    let config = AutomationConfig {
        enabled: true,
        backend: AutomationBackend::CodexAppServer,
        host_mode: AutomationHostMode::Standalone,
        tasks: AutomationTaskSet {
            session_reflector: AutomationTaskConfig {
                enabled: true,
                schedule: Some("manual".to_string()),
                ..AutomationTaskConfig::default()
            },
            ..AutomationTaskSet::default()
        },
        ..AutomationConfig::default()
    };

    let err = run_session_reflector_with_backend(
        &cg,
        &config,
        &backend,
        SessionReflectorAutomationOptions {
            trigger: AutomationTrigger::ManualCli,
            provider: "cursor".to_string(),
            query: "durable session reflection".to_string(),
            evidence_limit: 5,
            run_id: None,
            ..SessionReflectorAutomationOptions::default()
        },
    )
    .await
    .unwrap_err();

    assert_eq!(backend.calls(), 1);
    assert!(err
        .to_string()
        .contains("session reflector output must include a facts array"));
    let records = load_run_records(&cg.store_layout().dashboard_root, 10)
        .await
        .unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].task, AgentTaskKind::SessionReflector);
    assert_eq!(records[0].status, AutomationRunStatus::Failed);
    assert_eq!(records[0].model.as_deref(), Some("fixture-model"));
    assert!(records[0].evidence_hash.is_some());
    assert!(records[0].input_hash.is_some());
    assert_eq!(records[0].proposed_ops.as_ref(), Some(&output));
    assert!(
        records[0].error.as_deref().is_some_and(
            |error| error.contains("session reflector output must include a facts array")
        )
    );
    assert_eq!(
        records[0].error_classification,
        Some(AgentTaskFailureClass::MalformedOutput)
    );
    assert_eq!(records[0].error_retryable, Some(false));
}

#[tokio::test]
async fn session_reflector_runner_records_noop_fallback_when_backend_run_task_fails() {
    let temp = tempdir().unwrap();
    let cg = init_project(temp.path()).await;
    seed_session_evidence(&cg).await;
    let backend = FailingBackend::new(AgentTaskKind::SessionReflector);
    let config = AutomationConfig {
        enabled: true,
        backend: AutomationBackend::CodexAppServer,
        host_mode: AutomationHostMode::Standalone,
        tasks: AutomationTaskSet {
            session_reflector: AutomationTaskConfig {
                enabled: true,
                schedule: Some("manual".to_string()),
                ..AutomationTaskConfig::default()
            },
            ..AutomationTaskSet::default()
        },
        ..AutomationConfig::default()
    };

    let run = run_session_reflector_with_backend(
        &cg,
        &config,
        &backend,
        SessionReflectorAutomationOptions {
            trigger: AutomationTrigger::ManualCli,
            provider: "cursor".to_string(),
            query: "durable session reflection".to_string(),
            evidence_limit: 5,
            run_id: None,
            ..SessionReflectorAutomationOptions::default()
        },
    )
    .await
    .unwrap();

    assert_eq!(backend.calls(), 1);
    assert_noop_fallback_record(
        &run.ledger_record,
        AgentTaskKind::SessionReflector,
        "session_reflector",
        json!({ "facts": [] }),
    );
    assert!(run
        .ledger_record
        .error
        .as_deref()
        .is_some_and(|error| error.contains("executable")));
    let records = load_run_records(&cg.store_layout().dashboard_root, 10)
        .await
        .unwrap();
    assert_eq!(records.len(), 1);
    assert_noop_fallback_record(
        &records[0],
        AgentTaskKind::SessionReflector,
        "session_reflector",
        json!({ "facts": [] }),
    );
}

#[tokio::test]
async fn skill_writer_runner_skips_when_task_is_disabled() {
    let temp = tempdir().unwrap();
    let cg = init_project(temp.path()).await;
    let backend = SkillJsonBackend::new(json!({"skills": []}));
    let config = AutomationConfig {
        enabled: true,
        backend: AutomationBackend::CodexAppServer,
        host_mode: AutomationHostMode::Standalone,
        ..AutomationConfig::default()
    };

    let run = run_skill_writer_with_backend(
        &cg,
        &config,
        &backend,
        SkillWriterAutomationOptions {
            profile_root: Some(temp.path().join("profile")),
            ..SkillWriterAutomationOptions::default()
        },
    )
    .await
    .unwrap();

    assert_eq!(backend.calls(), 0);
    assert_eq!(run.ledger_record.task, AgentTaskKind::SkillWriter);
    assert_eq!(run.ledger_record.status, AutomationRunStatus::Skipped);
    assert_eq!(
        run.ledger_record.error.as_deref(),
        Some("skill_writer_disabled")
    );
}

#[tokio::test]
async fn skill_writer_runner_creates_pending_skill_drafts_for_approval() {
    let temp = tempdir().unwrap();
    let profile_root = temp.path().join("profile");
    let cg = init_project(temp.path()).await;
    seed_session_evidence(&cg).await;
    let backend = SkillJsonBackend::new(json!({
        "skills": [
            {
                "id": "automation-run-review",
                "title": "Automation run review",
                "summary": "Review self-improvement automation run ledgers and approval gates.",
                "category": "workflow",
                "targets": ["codex", "opencode"],
                "body_markdown": "Use when reviewing TraceDecay self-improvement runs. Check evidence, rejected ops, and pending approval state before applying changes.",
                "support_files": [
                    {
                        "path": "references/checklist.md",
                        "text": "- Check ledger counts\n- Check pending approval state\n"
                    }
                ],
                "reason": "Session evidence repeats approval-gated automation workflow review."
            },
            {
                "id": "automation-run-review",
                "title": "Duplicate",
                "summary": "Duplicate id should be rejected.",
                "category": "workflow",
                "body_markdown": "Duplicate body."
            },
            {
                "id": "bad/skill",
                "title": "Unsafe",
                "summary": "Unsafe id should be rejected.",
                "category": "workflow",
                "body_markdown": "Unsafe body."
            }
        ]
    }));
    let config = AutomationConfig {
        enabled: true,
        backend: AutomationBackend::CodexAppServer,
        host_mode: AutomationHostMode::Standalone,
        model: Some("configured-model".to_string()),
        tasks: AutomationTaskSet {
            skill_writer: AutomationTaskConfig {
                enabled: true,
                schedule: Some("manual".to_string()),
                ..AutomationTaskConfig::default()
            },
            ..AutomationTaskSet::default()
        },
        ..AutomationConfig::default()
    };

    let run = run_skill_writer_with_backend(
        &cg,
        &config,
        &backend,
        SkillWriterAutomationOptions {
            trigger: AutomationTrigger::ManualCli,
            provider: "cursor".to_string(),
            query: "automation".to_string(),
            evidence_limit: 5,
            profile_root: Some(profile_root.clone()),
            run_id: None,
        },
    )
    .await
    .unwrap();

    assert_eq!(backend.calls(), 1);
    assert_eq!(run.ledger_record.task, AgentTaskKind::SkillWriter);
    assert_eq!(run.ledger_record.status, AutomationRunStatus::Succeeded);
    assert_eq!(run.ledger_record.accepted_count, 1);
    assert_eq!(run.ledger_record.rejected_count, 2);
    assert_eq!(
        run.report["created_skills"][0]["metadata"]["id"],
        json!("automation-run-review")
    );
    assert_eq!(
        run.report["created_skills"][0]["metadata"]["state"],
        json!("pending_approval")
    );
    assert_eq!(
        run.report["created_skills"][0]["proposal_action"],
        json!("create")
    );
    assert_eq!(run.report["created_skills"][0]["action"], json!("create"));
    assert_eq!(
        run.report["created_skills"][0]["proposal_reason"],
        json!("Session evidence repeats approval-gated automation workflow review.")
    );
    assert_eq!(
        run.report["created_skills"][0]["reason"],
        json!("Session evidence repeats approval-gated automation workflow review.")
    );
    assert_eq!(
        run.report["created_skills"][0]["approval_status"],
        json!("pending_approval")
    );
    assert!(run.report["created_skills"][0]["target_checksum"]
        .as_str()
        .is_some_and(|checksum| checksum.starts_with("sha256:")));
    assert_eq!(
        run.report["created_skills"][0]["metadata"]["targets"],
        json!(["codex", "opencode"])
    );
    let artifact_kinds: Vec<&str> = run
        .ledger_record
        .artifacts
        .iter()
        .map(|artifact| artifact.kind.as_str())
        .collect();
    assert_eq!(
        artifact_kinds,
        vec![
            "traces",
            "feedback",
            "generated_evals",
            "validation_gate",
            "optimizer_diagnosis",
            "codex_handoff"
        ]
    );
    let eval_payload = read_artifact(&cg, &run.run_id, &run.ledger_record, "generated_evals").await;
    assert_eq!(eval_payload["task"], json!("skill_writer"));
    assert_eq!(eval_payload["summary"]["eval_count"], json!(3));
    assert!(eval_payload["eval_definitions"]
        .as_array()
        .unwrap()
        .iter()
        .any(|entry| entry["eval_id"] == json!("skill_writer:accepted:0")
            && entry["harness"]["commands"][0]
                == json!("cargo test --test automation_runner_test skill_writer")));
    assert_eq!(
        eval_payload["runner"]["commands"][0],
        json!(
            "cargo test --test automation_runner_test skill_writer_runner_creates_pending_skill_drafts_for_approval -- --nocapture"
        )
    );
    let handoff_payload =
        read_artifact(&cg, &run.run_id, &run.ledger_record, "codex_handoff").await;
    assert_eq!(handoff_payload["task"], json!("skill_writer"));
    assert_eq!(
        handoff_payload["next_actions"][0],
        json!("review managed skill drafts or auto-enabled changes")
    );
    assert_eq!(
        handoff_payload["eval_replay"]["commands"][0],
        json!(
            "cargo test --test automation_runner_test skill_writer_runner_creates_pending_skill_drafts_for_approval -- --nocapture"
        )
    );

    let skill = tracedecay::automation::managed_skills::load_managed_skill(
        &profile_root,
        "automation-run-review",
    )
    .await
    .unwrap();
    assert_eq!(
        skill.metadata.state,
        tracedecay::automation::managed_skills::ManagedSkillState::PendingApproval
    );
    assert_eq!(
        skill.metadata.provenance.source,
        tracedecay::automation::managed_skills::ManagedSkillSource::AutomationRun
    );
    assert_eq!(
        skill.metadata.provenance.run_id.as_deref(),
        Some(run.run_id.as_str())
    );
    assert_eq!(
        skill.metadata.targets,
        vec![
            tracedecay::automation::managed_skills::SkillInstallTarget::Codex,
            tracedecay::automation::managed_skills::SkillInstallTarget::OpenCode,
        ]
    );
    assert!(profile_root
        .join("agent_managed/skills/automation-run-review/references/checklist.md")
        .is_file());

    let records = load_run_records(&cg.store_layout().dashboard_root, 10)
        .await
        .unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].run_id, run.run_id);
    assert_eq!(records[0].accepted_count, 1);
    assert_eq!(records[0].rejected_count, 2);
    assert_eq!(
        records[0].applied_ops.as_ref().unwrap()["created_skills"][0]["action"],
        json!("create")
    );
}

#[tokio::test]
async fn skill_writer_evidence_imports_project_skill_usage_analytics_before_summarizing() {
    let _env_lock = ENV_LOCK.lock().await;
    let temp = tempdir().unwrap();
    let profile_root = temp.path().join("profile");
    let global_db_path = temp.path().join("global.db");
    let _global_db = EnvVarGuard::set("TRACEDECAY_GLOBAL_DB", &global_db_path);
    let cg = init_project(temp.path()).await;
    seed_session_evidence(&cg).await;
    seed_search_underuse_session_evidence(&cg).await;
    create_managed_skill_draft(
        &profile_root,
        ManagedSkillDraft {
            id: "automation-run-review".to_string(),
            title: "Automation run review".to_string(),
            summary: "Review self-improvement automation runs.".to_string(),
            category: "workflow".to_string(),
            targets: tracedecay::automation::managed_skills::default_managed_skill_targets(),
            body_markdown: "Check the run ledger before approving changes.".to_string(),
            support_files: Vec::new(),
            provenance: ManagedSkillProvenance {
                source: ManagedSkillSource::UserDraft,
                actor: "test".to_string(),
                run_id: None,
            },
        },
    )
    .await
    .unwrap();
    approve_managed_skill(&profile_root, "automation-run-review")
        .await
        .unwrap();
    let global_db = GlobalDb::open().await.expect("global db should open");
    global_db
        .append_analytics_event(&tracedecay::global_db::AnalyticsEventInsert {
            provider: "codex".to_string(),
            project_id: GlobalDb::canonical_project_key(cg.project_root()),
            session_id: Some("skill-writer-analytics".to_string()),
            timestamp: 1_715_000_111,
            event_kind: "mcp_tool_call".to_string(),
            hook_name: None,
            tool_name: Some("tracedecay_skill_view".to_string()),
            tool_category: None,
            skill_name: None,
            hint_category: None,
            hint_id: None,
            outcome: Some("success".to_string()),
            metadata_json: Some(
                json!({
                    "function": {
                        "name": "tracedecay_skill_view",
                        "arguments": { "id": "automation-run-review" }
                    }
                })
                .to_string(),
            ),
        })
        .await
        .unwrap();
    let backend = InspectSkillWriterUsageBackend;
    let config = AutomationConfig {
        enabled: true,
        backend: AutomationBackend::CodexAppServer,
        host_mode: AutomationHostMode::Standalone,
        tasks: AutomationTaskSet {
            skill_writer: AutomationTaskConfig {
                enabled: true,
                schedule: Some("manual".to_string()),
                ..AutomationTaskConfig::default()
            },
            ..AutomationTaskSet::default()
        },
        ..AutomationConfig::default()
    };

    let run = run_skill_writer_with_backend(
        &cg,
        &config,
        &backend,
        SkillWriterAutomationOptions {
            trigger: AutomationTrigger::ManualCli,
            provider: "cursor".to_string(),
            query: "automation".to_string(),
            evidence_limit: 5,
            profile_root: Some(profile_root),
            run_id: None,
        },
    )
    .await
    .unwrap();

    assert_eq!(run.ledger_record.status, AutomationRunStatus::Succeeded);
    assert!(run.report["skill_improvement_recommendations"]
        .as_array()
        .is_some_and(
            |recommendations| recommendations.iter().any(|recommendation| {
                recommendation["id"] == "underused_tool_family:code_search"
                    && recommendation["source"] == "session_tool_usage"
            })
        ));
}

#[tokio::test]
async fn skill_writer_evidence_includes_underused_tool_family_summary() {
    let temp = tempdir().unwrap();
    let cg = init_project(temp.path()).await;
    seed_session_evidence(&cg).await;
    seed_search_underuse_session_evidence(&cg).await;
    let backend = InspectSkillWriterUnderusedBackend;
    let config = AutomationConfig {
        enabled: true,
        backend: AutomationBackend::CodexAppServer,
        host_mode: AutomationHostMode::Standalone,
        tasks: AutomationTaskSet {
            skill_writer: AutomationTaskConfig {
                enabled: true,
                schedule: Some("manual".to_string()),
                ..AutomationTaskConfig::default()
            },
            ..AutomationTaskSet::default()
        },
        ..AutomationConfig::default()
    };

    let run = run_skill_writer_with_backend(
        &cg,
        &config,
        &backend,
        SkillWriterAutomationOptions {
            trigger: AutomationTrigger::ManualCli,
            provider: "cursor".to_string(),
            query: "automation".to_string(),
            evidence_limit: 5,
            run_id: None,
            ..SkillWriterAutomationOptions::default()
        },
    )
    .await
    .unwrap();

    assert_eq!(run.ledger_record.status, AutomationRunStatus::Succeeded);
}

#[tokio::test]
async fn skill_writer_runner_auto_enables_when_config_explicitly_allows() {
    let temp = tempdir().unwrap();
    let profile_root = temp.path().join("profile");
    let cg = init_project(temp.path()).await;
    seed_session_evidence(&cg).await;
    create_managed_skill_draft(
        &profile_root,
        ManagedSkillDraft {
            id: "automation-run-review".to_string(),
            title: "Automation run review".to_string(),
            summary: "Review self-improvement automation runs.".to_string(),
            category: "workflow".to_string(),
            targets: tracedecay::automation::managed_skills::default_managed_skill_targets(),
            body_markdown: "Check the run ledger before approving changes.".to_string(),
            support_files: Vec::new(),
            provenance: ManagedSkillProvenance {
                source: ManagedSkillSource::UserDraft,
                actor: "test".to_string(),
                run_id: None,
            },
        },
    )
    .await
    .unwrap();
    let active = approve_managed_skill(&profile_root, "automation-run-review")
        .await
        .unwrap();
    let base_checksum = active.metadata.checksum.clone();
    let backend = SkillJsonBackend::with_activation_policy(
        json!({
            "skills": [
                {
                    "id": "scheduler-review",
                    "title": "Scheduler review",
                    "summary": "Review scheduler decisions before enabling automation.",
                    "category": "workflow",
                    "body_markdown": "Check interval gates, cooldowns, locks, and run ledgers before changing schedules.",
                    "reason": "Session evidence repeats scheduler review."
                },
                {
                    "action": "update",
                    "id": "automation-run-review",
                    "base_checksum": base_checksum,
                    "summary": "Review self-improvement automation runs and activation policy.",
                    "body_markdown": "Check the run ledger, activation policy, and approval state before applying changes.",
                    "reason": "Session evidence repeats approval-gated automation workflow review."
                }
            ]
        }),
        "auto_enable_after_validation",
    );
    let config = AutomationConfig {
        enabled: true,
        backend: AutomationBackend::CodexAppServer,
        host_mode: AutomationHostMode::Standalone,
        auto_enable_skills: true,
        tasks: AutomationTaskSet {
            skill_writer: AutomationTaskConfig {
                enabled: true,
                schedule: Some("manual".to_string()),
                ..AutomationTaskConfig::default()
            },
            ..AutomationTaskSet::default()
        },
        ..AutomationConfig::default()
    };

    let run = run_skill_writer_with_backend(
        &cg,
        &config,
        &backend,
        SkillWriterAutomationOptions {
            trigger: AutomationTrigger::ManualCli,
            provider: "cursor".to_string(),
            query: "automation".to_string(),
            evidence_limit: 5,
            profile_root: Some(profile_root.clone()),
            run_id: None,
        },
    )
    .await
    .unwrap();

    assert_eq!(backend.calls(), 1);
    assert_eq!(run.ledger_record.status, AutomationRunStatus::Succeeded);
    assert_eq!(run.ledger_record.accepted_count, 2);
    assert_eq!(run.ledger_record.rejected_count, 0);
    assert_eq!(run.report["status"], json!("auto_enabled"));
    assert_eq!(
        run.report["activation_policy"],
        json!("auto_enable_after_validation")
    );
    assert_eq!(
        run.report["created_skills"][0]["metadata"]["state"],
        json!("active")
    );
    assert_eq!(
        run.report["updated_skills"][0]["metadata"]["state"],
        json!("active")
    );
    assert_eq!(
        run.report["created_skills"][0]["approval_status"],
        json!("auto_enabled")
    );
    assert_eq!(
        run.report["updated_skills"][0]["proposal_action"],
        json!("update")
    );
    assert_eq!(run.report["updated_skills"][0]["action"], json!("update"));
    assert_eq!(
        run.report["updated_skills"][0]["approval_status"],
        json!("auto_enabled")
    );
    assert_eq!(
        run.report["updated_skills"][0]["base_checksum"],
        json!(base_checksum)
    );

    let created = load_managed_skill(&profile_root, "scheduler-review")
        .await
        .unwrap();
    let updated = load_managed_skill(&profile_root, "automation-run-review")
        .await
        .unwrap();
    assert_eq!(created.metadata.state, ManagedSkillState::Active);
    assert_eq!(updated.metadata.state, ManagedSkillState::Active);
    assert_eq!(
        updated.metadata.summary,
        "Review self-improvement automation runs and activation policy."
    );
    assert!(updated.pending_update.is_none());
}

#[tokio::test]
async fn skill_writer_runner_updates_existing_skills_with_checksum_precondition() {
    let temp = tempdir().unwrap();
    let profile_root = temp.path().join("profile");
    let cg = init_project(temp.path()).await;
    seed_session_evidence(&cg).await;
    create_managed_skill_draft(
        &profile_root,
        ManagedSkillDraft {
            id: "automation-run-review".to_string(),
            title: "Automation run review".to_string(),
            summary: "Review self-improvement automation runs.".to_string(),
            category: "workflow".to_string(),
            targets: tracedecay::automation::managed_skills::default_managed_skill_targets(),
            body_markdown: "Check the run ledger before approving changes.".to_string(),
            support_files: vec![ManagedSupportFile::new(
                "references/old.md",
                b"old checklist".to_vec(),
            )
            .unwrap()],
            provenance: ManagedSkillProvenance {
                source: ManagedSkillSource::UserDraft,
                actor: "test".to_string(),
                run_id: None,
            },
        },
    )
    .await
    .unwrap();
    let active = approve_managed_skill(&profile_root, "automation-run-review")
        .await
        .unwrap();
    let base_checksum = active.metadata.checksum.clone();
    let backend = SkillJsonBackend::new(json!({
        "skills": [
            {
                "action": "update",
                "id": "automation-run-review",
                "base_checksum": base_checksum.clone(),
                "summary": "Review self-improvement automation runs.",
                "reason": "No-op updates should not be counted as accepted."
            },
            {
                "action": "update",
                "id": "automation-run-review",
                "base_checksum": base_checksum.clone(),
                "summary": "Review automation runs, rejected proposals, and approval gates.",
                "targets": ["claude", "kimi"],
                "body_markdown": "Check the run ledger, rejected proposals, and pending approval state before applying changes.",
                "support_files": [
                    {
                        "path": "references/checklist.md",
                        "text": "- Check ledger counts\n- Check rejected proposals\n"
                    }
                ],
                "reason": "Session evidence repeats approval-gated automation workflow review."
            },
            {
                "action": "patch",
                "id": "automation-run-review",
                "base_checksum": "sha256:stale",
                "summary": "Stale patch should be rejected."
            },
            {
                "action": "update",
                "id": "missing-skill",
                "base_checksum": "sha256:missing",
                "summary": "Unknown update should be rejected."
            }
        ]
    }));
    let config = AutomationConfig {
        enabled: true,
        backend: AutomationBackend::CodexAppServer,
        host_mode: AutomationHostMode::Standalone,
        tasks: AutomationTaskSet {
            skill_writer: AutomationTaskConfig {
                enabled: true,
                schedule: Some("manual".to_string()),
                ..AutomationTaskConfig::default()
            },
            ..AutomationTaskSet::default()
        },
        ..AutomationConfig::default()
    };

    let run = run_skill_writer_with_backend(
        &cg,
        &config,
        &backend,
        SkillWriterAutomationOptions {
            trigger: AutomationTrigger::ManualCli,
            provider: "cursor".to_string(),
            query: "automation".to_string(),
            evidence_limit: 5,
            profile_root: Some(profile_root.clone()),
            run_id: None,
        },
    )
    .await
    .unwrap();

    assert_eq!(backend.calls(), 1);
    assert_eq!(run.ledger_record.status, AutomationRunStatus::Succeeded);
    assert_eq!(run.ledger_record.accepted_count, 1);
    assert_eq!(run.ledger_record.rejected_count, 3);
    assert_eq!(run.report["created_skills"], json!([]));
    assert_eq!(
        run.report["updated_skills"][0]["metadata"]["id"],
        json!("automation-run-review")
    );
    assert_eq!(
        run.report["updated_skills"][0]["metadata"]["state"],
        json!("pending_approval")
    );
    assert_eq!(
        run.report["updated_skills"][0]["proposal_action"],
        json!("update")
    );
    assert_eq!(run.report["updated_skills"][0]["action"], json!("update"));
    assert_eq!(
        run.report["updated_skills"][0]["proposal_reason"],
        json!("Session evidence repeats approval-gated automation workflow review.")
    );
    assert_eq!(
        run.report["updated_skills"][0]["reason"],
        json!("Session evidence repeats approval-gated automation workflow review.")
    );
    assert_eq!(
        run.report["updated_skills"][0]["approval_status"],
        json!("staged_update")
    );
    assert_eq!(
        run.report["updated_skills"][0]["base_checksum"],
        json!(base_checksum)
    );
    assert_eq!(
        run.report["updated_skills"][0]["metadata"]["targets"],
        json!(["claude", "kimi"])
    );
    assert!(run.report["updated_skills"][0]["target_checksum"]
        .as_str()
        .is_some_and(|checksum| checksum.starts_with("sha256:")));

    let skill = load_managed_skill(&profile_root, "automation-run-review")
        .await
        .unwrap();
    assert_eq!(skill.metadata.state, ManagedSkillState::Active);
    assert_eq!(
        skill.metadata.summary,
        "Review self-improvement automation runs."
    );
    assert_eq!(skill.metadata.checksum, active.metadata.checksum);
    let pending = skill.pending_update.as_ref().unwrap();
    assert_eq!(pending.metadata.state, ManagedSkillState::PendingApproval);
    assert_eq!(
        pending.metadata.summary,
        "Review automation runs, rejected proposals, and approval gates."
    );
    assert_eq!(
        pending.metadata.targets,
        vec![
            tracedecay::automation::managed_skills::SkillInstallTarget::Claude,
            tracedecay::automation::managed_skills::SkillInstallTarget::Kimi,
        ]
    );
    assert_ne!(pending.metadata.checksum, active.metadata.checksum);
    let skill_dir = profile_root.join("agent_managed/skills/automation-run-review");
    assert!(skill_dir.join("references/old.md").is_file());
    assert!(!skill_dir.join("references/checklist.md").exists());

    let approved = approve_managed_skill(&profile_root, "automation-run-review")
        .await
        .unwrap();
    assert_eq!(approved.metadata.state, ManagedSkillState::Active);
    assert_eq!(
        approved.metadata.summary,
        "Review automation runs, rejected proposals, and approval gates."
    );
    assert!(approved.pending_update.is_none());
    assert!(!skill_dir.join("references/old.md").exists());
    assert!(skill_dir.join("references/checklist.md").is_file());

    let records = load_run_records(&cg.store_layout().dashboard_root, 10)
        .await
        .unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].accepted_count, 1);
    assert_eq!(records[0].rejected_count, 3);
    assert_eq!(
        records[0].proposed_ops.as_ref().unwrap()["updated_skills"][0]["metadata"]["id"],
        json!("automation-run-review")
    );
    assert_eq!(
        records[0].applied_ops.as_ref().unwrap()["updated_skills"][0]["approval_status"],
        json!("staged_update")
    );
}

#[tokio::test]
async fn skill_writer_runner_ledgers_malformed_backend_output() {
    let temp = tempdir().unwrap();
    let profile_root = temp.path().join("profile");
    let cg = init_project(temp.path()).await;
    seed_session_evidence(&cg).await;
    let backend = SkillTextBackend::new("not json");
    let config = AutomationConfig {
        enabled: true,
        backend: AutomationBackend::CodexAppServer,
        host_mode: AutomationHostMode::Standalone,
        tasks: AutomationTaskSet {
            skill_writer: AutomationTaskConfig {
                enabled: true,
                schedule: Some("manual".to_string()),
                ..AutomationTaskConfig::default()
            },
            ..AutomationTaskSet::default()
        },
        ..AutomationConfig::default()
    };

    let err = run_skill_writer_with_backend(
        &cg,
        &config,
        &backend,
        SkillWriterAutomationOptions {
            trigger: AutomationTrigger::ManualCli,
            provider: "cursor".to_string(),
            query: "automation".to_string(),
            evidence_limit: 5,
            profile_root: Some(profile_root),
            run_id: None,
        },
    )
    .await
    .unwrap_err();

    assert!(
        err.to_string().contains("expected ident") || err.to_string().contains("expected value"),
        "unexpected error: {err}"
    );
    let records = load_run_records(&cg.store_layout().dashboard_root, 10)
        .await
        .unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].schema_version, 2);
    assert_eq!(records[0].task, AgentTaskKind::SkillWriter);
    assert_eq!(records[0].task_key.as_deref(), Some("skill_writer"));
    assert_eq!(
        records[0].prompt_version.as_deref(),
        Some("skill_writer:v1")
    );
    assert_eq!(records[0].status, AutomationRunStatus::Failed);
    assert_eq!(records[0].reviewed_count, 0);
    assert_eq!(records[0].skipped_count, 0);
    assert_eq!(records[0].model.as_deref(), Some("fixture-model"));
    assert!(records[0].evidence_hash.is_some());
    assert!(records[0].error.as_deref().is_some_and(|error| {
        error.contains("expected ident") || error.contains("expected value")
    }));
    assert_eq!(
        records[0].error_classification,
        Some(AgentTaskFailureClass::MalformedOutput)
    );
    assert_eq!(records[0].error_retryable, Some(false));
}

#[tokio::test]
async fn skill_writer_runner_ledgers_missing_skills_array() {
    let temp = tempdir().unwrap();
    let profile_root = temp.path().join("profile");
    let cg = init_project(temp.path()).await;
    seed_session_evidence(&cg).await;
    let output = json!({"summary": "no skills"});
    let backend = SkillJsonBackend::new(output.clone());
    let config = AutomationConfig {
        enabled: true,
        backend: AutomationBackend::CodexAppServer,
        host_mode: AutomationHostMode::Standalone,
        tasks: AutomationTaskSet {
            skill_writer: AutomationTaskConfig {
                enabled: true,
                schedule: Some("manual".to_string()),
                ..AutomationTaskConfig::default()
            },
            ..AutomationTaskSet::default()
        },
        ..AutomationConfig::default()
    };

    let err = run_skill_writer_with_backend(
        &cg,
        &config,
        &backend,
        SkillWriterAutomationOptions {
            trigger: AutomationTrigger::ManualCli,
            provider: "cursor".to_string(),
            query: "automation".to_string(),
            evidence_limit: 5,
            profile_root: Some(profile_root),
            run_id: None,
        },
    )
    .await
    .unwrap_err();

    assert_eq!(backend.calls(), 1);
    assert!(err
        .to_string()
        .contains("skill writer output must include a skills array"));
    let records = load_run_records(&cg.store_layout().dashboard_root, 10)
        .await
        .unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].schema_version, 2);
    assert_eq!(records[0].task, AgentTaskKind::SkillWriter);
    assert_eq!(records[0].task_key.as_deref(), Some("skill_writer"));
    assert_eq!(records[0].status, AutomationRunStatus::Failed);
    assert_eq!(records[0].model.as_deref(), Some("fixture-model"));
    assert!(records[0].evidence_hash.is_some());
    assert!(records[0].input_hash.is_some());
    assert_eq!(records[0].proposed_ops.as_ref(), Some(&output));
    assert!(records[0]
        .error
        .as_deref()
        .is_some_and(|error| error.contains("skill writer output must include a skills array")));
    assert_eq!(
        records[0].error_classification,
        Some(AgentTaskFailureClass::MalformedOutput)
    );
    assert_eq!(records[0].error_retryable, Some(false));
}

#[tokio::test]
async fn skill_writer_runner_records_noop_fallback_when_backend_run_task_fails() {
    let temp = tempdir().unwrap();
    let profile_root = temp.path().join("profile");
    let cg = init_project(temp.path()).await;
    seed_session_evidence(&cg).await;
    let backend = FailingBackend::new(AgentTaskKind::SkillWriter);
    let config = AutomationConfig {
        enabled: true,
        backend: AutomationBackend::CodexAppServer,
        host_mode: AutomationHostMode::Standalone,
        tasks: AutomationTaskSet {
            skill_writer: AutomationTaskConfig {
                enabled: true,
                schedule: Some("manual".to_string()),
                ..AutomationTaskConfig::default()
            },
            ..AutomationTaskSet::default()
        },
        ..AutomationConfig::default()
    };

    let run = run_skill_writer_with_backend(
        &cg,
        &config,
        &backend,
        SkillWriterAutomationOptions {
            trigger: AutomationTrigger::ManualCli,
            provider: "cursor".to_string(),
            query: "automation".to_string(),
            evidence_limit: 5,
            run_id: None,
            profile_root: Some(profile_root),
        },
    )
    .await
    .unwrap();

    assert_eq!(backend.calls(), 1);
    assert_noop_fallback_record(
        &run.ledger_record,
        AgentTaskKind::SkillWriter,
        "skill_writer",
        json!({ "skills": [] }),
    );
    assert!(run
        .ledger_record
        .error
        .as_deref()
        .is_some_and(|error| error.contains("executable")));
    let records = load_run_records(&cg.store_layout().dashboard_root, 10)
        .await
        .unwrap();
    assert_eq!(records.len(), 1);
    assert_noop_fallback_record(
        &records[0],
        AgentTaskKind::SkillWriter,
        "skill_writer",
        json!({ "skills": [] }),
    );
}

#[tokio::test]
async fn scheduler_memory_curator_respects_failure_cooldown() {
    let temp = tempdir().unwrap();
    let cg = init_project(temp.path()).await;
    seed_duplicate_facts(&cg).await;
    let config = scheduler_config(Some(3600), Some(3600));
    append_run_record(
        &cg.store_layout().dashboard_root,
        &scheduler_record(
            "previous_failed_run",
            AutomationRunStatus::Failed,
            current_timestamp() - 60,
        ),
    )
    .await
    .unwrap();
    let backend = JsonBackend::new(json!({"ops": []}));

    let run = run_memory_curator_with_backend(
        &cg,
        &config,
        &backend,
        MemoryCuratorAutomationOptions {
            trigger: AutomationTrigger::Scheduler,
            max_clusters: 4,
            min_confidence: 0.5,
            run_id: None,
        },
    )
    .await
    .unwrap();

    assert_eq!(backend.calls(), 0);
    assert_eq!(run.ledger_record.status, AutomationRunStatus::Skipped);
    assert_eq!(
        run.ledger_record.error.as_deref(),
        Some("scheduler_cooldown_active")
    );
}

#[tokio::test]
async fn scheduler_memory_curator_respects_interval_gate() {
    let temp = tempdir().unwrap();
    let cg = init_project(temp.path()).await;
    seed_duplicate_facts(&cg).await;
    let config = scheduler_config(Some(3600), None);
    append_run_record(
        &cg.store_layout().dashboard_root,
        &scheduler_record(
            "previous_successful_run",
            AutomationRunStatus::Succeeded,
            current_timestamp() - 60,
        ),
    )
    .await
    .unwrap();
    let backend = JsonBackend::new(json!({"ops": []}));

    let run = run_memory_curator_with_backend(
        &cg,
        &config,
        &backend,
        MemoryCuratorAutomationOptions {
            trigger: AutomationTrigger::Scheduler,
            max_clusters: 4,
            min_confidence: 0.5,
            run_id: None,
        },
    )
    .await
    .unwrap();

    assert_eq!(backend.calls(), 0);
    assert_eq!(run.ledger_record.status, AutomationRunStatus::Skipped);
    assert_eq!(
        run.ledger_record.error.as_deref(),
        Some("scheduler_interval_not_elapsed")
    );
}

#[tokio::test]
async fn scheduler_session_reflector_respects_interval_gate() {
    let temp = tempdir().unwrap();
    let cg = init_project(temp.path()).await;
    seed_session_evidence(&cg).await;
    let config = scheduler_config(Some(3600), None);
    append_run_record(
        &cg.store_layout().dashboard_root,
        &scheduler_record_for(
            "previous_session_reflector_run",
            AgentTaskKind::SessionReflector,
            AutomationRunStatus::Succeeded,
            current_timestamp() - 60,
        ),
    )
    .await
    .unwrap();
    let backend = SessionJsonBackend::new(json!({"facts": []}));

    let run = run_session_reflector_with_backend(
        &cg,
        &config,
        &backend,
        SessionReflectorAutomationOptions {
            trigger: AutomationTrigger::Scheduler,
            ..SessionReflectorAutomationOptions::default()
        },
    )
    .await
    .unwrap();

    assert_eq!(backend.calls(), 0);
    assert_eq!(run.ledger_record.status, AutomationRunStatus::Skipped);
    assert_eq!(
        run.ledger_record.error.as_deref(),
        Some("scheduler_interval_not_elapsed")
    );
}

#[tokio::test]
async fn scheduler_skill_writer_respects_interval_gate() {
    let temp = tempdir().unwrap();
    let cg = init_project(temp.path()).await;
    seed_session_evidence(&cg).await;
    let config = scheduler_config(Some(3600), None);
    append_run_record(
        &cg.store_layout().dashboard_root,
        &scheduler_record_for(
            "previous_skill_writer_run",
            AgentTaskKind::SkillWriter,
            AutomationRunStatus::Succeeded,
            current_timestamp() - 60,
        ),
    )
    .await
    .unwrap();
    let backend = SkillJsonBackend::new(json!({"skills": []}));

    let run = run_skill_writer_with_backend(
        &cg,
        &config,
        &backend,
        SkillWriterAutomationOptions {
            trigger: AutomationTrigger::Scheduler,
            ..SkillWriterAutomationOptions::default()
        },
    )
    .await
    .unwrap();

    assert_eq!(backend.calls(), 0);
    assert_eq!(run.ledger_record.status, AutomationRunStatus::Skipped);
    assert_eq!(
        run.ledger_record.error.as_deref(),
        Some("scheduler_interval_not_elapsed")
    );
}

#[tokio::test]
async fn scheduler_skill_writer_respects_idle_window_after_manual_run() {
    let temp = tempdir().unwrap();
    let cg = init_project(temp.path()).await;
    seed_session_evidence(&cg).await;
    let mut config = scheduler_config(Some(1), None);
    config.tasks.skill_writer.min_idle_secs = Some(3600);
    let mut record = scheduler_record_for(
        "recent_manual_skill_writer_run",
        AgentTaskKind::SkillWriter,
        AutomationRunStatus::Succeeded,
        current_timestamp() - 60,
    );
    record.trigger = AutomationTrigger::ManualCli;
    append_run_record(&cg.store_layout().dashboard_root, &record)
        .await
        .unwrap();
    let backend = SkillJsonBackend::new(json!({"skills": []}));

    let run = run_skill_writer_with_backend(
        &cg,
        &config,
        &backend,
        SkillWriterAutomationOptions {
            trigger: AutomationTrigger::Scheduler,
            ..SkillWriterAutomationOptions::default()
        },
    )
    .await
    .unwrap();

    assert_eq!(backend.calls(), 0);
    assert_eq!(run.ledger_record.status, AutomationRunStatus::Skipped);
    assert_eq!(
        run.ledger_record.error.as_deref(),
        Some("scheduler_idle_window_active")
    );
}

#[tokio::test]
async fn memory_curator_runner_cleans_up_lock_file() {
    let temp = tempdir().unwrap();
    let cg = init_project(temp.path()).await;
    seed_duplicate_facts(&cg).await;
    let backend = JsonBackend::new(json!({"ops": []}));
    let config = scheduler_config(None, None);

    run_memory_curator_with_backend(
        &cg,
        &config,
        &backend,
        MemoryCuratorAutomationOptions {
            trigger: AutomationTrigger::Scheduler,
            max_clusters: 4,
            min_confidence: 0.5,
            run_id: None,
        },
    )
    .await
    .unwrap();

    assert!(!cg
        .store_layout()
        .dashboard_root
        .join("automation_locks")
        .join("memory_curator.lock")
        .exists());
}

#[tokio::test]
async fn memory_curator_runner_recovers_stale_scheduler_lock_file() {
    let temp = tempdir().unwrap();
    let cg = init_project(temp.path()).await;
    let lock_dir = cg.store_layout().dashboard_root.join("automation_locks");
    fs::create_dir_all(&lock_dir).unwrap();
    let lock_path = lock_dir.join("memory_curator.lock");
    fs::write(&lock_path, "pid=999999\ncreated_at=100\n").unwrap();
    let backend = JsonBackend::new(json!({"ops": []}));
    let mut config = scheduler_config(None, None);
    config.tasks.memory_curator.stale_lock_secs = Some(1);

    let run = run_memory_curator_with_backend(
        &cg,
        &config,
        &backend,
        MemoryCuratorAutomationOptions {
            trigger: AutomationTrigger::Scheduler,
            max_clusters: 4,
            min_confidence: 0.5,
            run_id: None,
        },
    )
    .await
    .unwrap();

    assert_eq!(backend.calls(), 0);
    assert_eq!(run.ledger_record.status, AutomationRunStatus::Skipped);
    assert_eq!(
        run.ledger_record.error.as_deref(),
        Some("scheduler_schedule_manual")
    );
    assert!(!lock_path.exists());
}

#[tokio::test]
async fn scheduler_memory_curator_ledgers_active_lock_skip() {
    let temp = tempdir().unwrap();
    let cg = init_project(temp.path()).await;
    seed_duplicate_facts(&cg).await;
    let lock_dir = cg.store_layout().dashboard_root.join("automation_locks");
    fs::create_dir_all(&lock_dir).unwrap();
    let lock_path = lock_dir.join("memory_curator.lock");
    fs::write(
        &lock_path,
        format!(
            "pid={}\ncreated_at={}\n",
            std::process::id(),
            current_timestamp()
        ),
    )
    .unwrap();
    let backend = JsonBackend::new(json!({"ops": []}));
    let config = scheduler_config(None, None);

    let run = run_memory_curator_with_backend(
        &cg,
        &config,
        &backend,
        MemoryCuratorAutomationOptions {
            trigger: AutomationTrigger::Scheduler,
            max_clusters: 4,
            min_confidence: 0.5,
            run_id: None,
        },
    )
    .await
    .unwrap();

    assert_eq!(backend.calls(), 0);
    assert_eq!(run.ledger_record.status, AutomationRunStatus::Skipped);
    assert_eq!(
        run.ledger_record.error.as_deref(),
        Some("scheduler_lock_active")
    );
    assert!(lock_path.exists());
    let records = load_run_records(&cg.store_layout().dashboard_root, 10)
        .await
        .unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].error.as_deref(), Some("scheduler_lock_active"));
}

#[tokio::test]
async fn manual_memory_curator_run_ignores_scheduler_lock() {
    let temp = tempdir().unwrap();
    let cg = init_project(temp.path()).await;
    seed_duplicate_facts(&cg).await;
    let lock_dir = cg.store_layout().dashboard_root.join("automation_locks");
    fs::create_dir_all(&lock_dir).unwrap();
    let lock_path = lock_dir.join("memory_curator.lock");
    fs::write(
        &lock_path,
        format!(
            "pid={}\ncreated_at={}\n",
            std::process::id(),
            current_timestamp()
        ),
    )
    .unwrap();
    let backend = JsonBackend::new(json!({"ops": []}));
    let config = scheduler_config(None, None);

    let run = run_memory_curator_with_backend(
        &cg,
        &config,
        &backend,
        MemoryCuratorAutomationOptions {
            trigger: AutomationTrigger::ManualCli,
            max_clusters: 4,
            min_confidence: 0.5,
            run_id: None,
        },
    )
    .await
    .unwrap();

    assert_eq!(backend.calls(), 1);
    assert_eq!(run.ledger_record.status, AutomationRunStatus::Succeeded);
    assert!(lock_path.exists());
}

async fn init_project(project_root: &Path) -> TraceDecay {
    fs::create_dir_all(project_root.join("src")).unwrap();
    fs::write(project_root.join("src/lib.rs"), "pub fn fixture() {}\n").unwrap();
    TraceDecay::init(project_root).await.unwrap()
}

async fn seed_session_evidence(cg: &TraceDecay) {
    let db = GlobalDb::open_at(&cg.store_layout().sessions_db_path)
        .await
        .expect("session db open");
    seed_session_message_in_db(
        &db,
        cg.project_root(),
        SeedSessionMessage {
            provider: "cursor",
            session_id: "session-reflect-1",
            message_id: "session-reflect-1-message-001",
            role: "user",
            timestamp: 1_715_000_001,
            text: "Remember durable session reflection facts must remain approval gated for automation workflows.",
            source: None,
        },
    )
    .await;
}

async fn seed_search_underuse_session_evidence(cg: &TraceDecay) {
    let db = GlobalDb::open_at(&cg.store_layout().sessions_db_path)
        .await
        .expect("session db open");
    let session = SessionRecord {
        provider: "cursor".to_string(),
        session_id: "skill-writer-underuse".to_string(),
        project_key: cg.project_root().display().to_string(),
        project_path: cg.project_root().display().to_string(),
        title: Some("Skill writer underuse fixture".to_string()),
        started_at: Some(1_715_000_120),
        ended_at: None,
        transcript_path: None,
        metadata_json: None,
        parent_session_id: None,
        is_subagent: false,
        agent_id: None,
        parent_tool_use_id: None,
    };
    assert!(db.upsert_session(&session).await);
    let message = SessionMessageRecord {
        provider: "cursor".to_string(),
        message_id: "skill-writer-underuse-message-001".to_string(),
        session_id: "skill-writer-underuse".to_string(),
        role: "assistant".to_string(),
        timestamp: Some(1_715_000_121),
        ordinal: 1,
        text: "Repeated automation workflow used shell search with  rg automation src  before drafting a skill.".to_string(),
        kind: Some("message".to_string()),
        model: None,
        tool_names: Some("bash".to_string()),
        source_path: None,
        source_offset: None,
        metadata_json: Some(json!({ "cmd": "rg automation src" }).to_string()),
    };
    assert!(db.upsert_session_message(&message).await);
}

struct SeedSessionMessage<'a> {
    provider: &'a str,
    session_id: &'a str,
    message_id: &'a str,
    role: &'a str,
    timestamp: i64,
    text: &'a str,
    source: Option<&'a str>,
}

async fn seed_session_message_in_db(
    db: &GlobalDb,
    project_root: &Path,
    seed: SeedSessionMessage<'_>,
) {
    let session = SessionRecord {
        provider: seed.provider.to_string(),
        session_id: seed.session_id.to_string(),
        project_key: project_root.display().to_string(),
        project_path: project_root.display().to_string(),
        title: Some("Session reflection fixture".to_string()),
        started_at: Some(seed.timestamp.saturating_sub(1)),
        ended_at: None,
        transcript_path: None,
        metadata_json: None,
        parent_session_id: None,
        is_subagent: false,
        agent_id: None,
        parent_tool_use_id: None,
    };
    assert!(db.upsert_session(&session).await);
    let message = SessionMessageRecord {
        provider: seed.provider.to_string(),
        message_id: seed.message_id.to_string(),
        session_id: seed.session_id.to_string(),
        role: seed.role.to_string(),
        timestamp: Some(seed.timestamp),
        ordinal: 1,
        text: seed.text.to_string(),
        kind: Some("message".to_string()),
        model: None,
        tool_names: None,
        source_path: None,
        source_offset: None,
        metadata_json: seed
            .source
            .map(|source| json!({ "source": source }).to_string()),
    };
    assert!(db.upsert_session_message(&message).await);
}

async fn seed_duplicate_facts(cg: &TraceDecay) {
    let conn = cg.db().conn();
    let vec_a = HolographicEncoder::serialize(&[0.20, 0.35, 0.50]).unwrap();
    let vec_b = HolographicEncoder::serialize(&[0.21, 0.34, 0.49]).unwrap();
    for (fact_id, content, vector, trust_score) in [
        (
            101_i64,
            "Cache invalidation policy must be explicit",
            vec_a,
            0.97_f64,
        ),
        (
            102_i64,
            "Cache invalidation policy must stay explicit",
            vec_b,
            0.95_f64,
        ),
    ] {
        conn.execute(
            "INSERT INTO memory_facts
                (fact_id, content, category, tags, trust_score, retrieval_count, helpful_count,
                 created_at, updated_at, hrr_vector, hrr_algebra, hrr_dim, access_count)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            libsql::params![
                fact_id,
                content,
                "project",
                "[\"cache\",\"policy\"]",
                trust_score,
                0_i64,
                0_i64,
                1_700_000_000_i64 + fact_id,
                1_700_000_100_i64 + fact_id,
                libsql::Value::Blob(vector),
                "amari_fhrr",
                HolographicEncoder::DIMENSIONS as i64,
                0_i64,
            ],
        )
        .await
        .unwrap();
    }
}

async fn fact_exists(cg: &TraceDecay, fact_id: i64) -> bool {
    let conn = cg.db().conn();
    let mut rows = conn
        .query(
            "SELECT 1 FROM memory_facts WHERE fact_id = ?1 LIMIT 1",
            libsql::params![fact_id],
        )
        .await
        .unwrap();
    rows.next().await.unwrap().is_some()
}

async fn read_artifact(
    cg: &TraceDecay,
    run_id: &str,
    record: &AutomationRunLedgerRecord,
    kind: &str,
) -> Value {
    let artifact = record
        .artifacts
        .iter()
        .find(|artifact| artifact.kind == kind)
        .unwrap_or_else(|| panic!("missing {kind} artifact"));
    read_run_artifact_payload(&cg.store_layout().dashboard_root, run_id, artifact)
        .await
        .unwrap()
}

fn scheduler_config(interval_secs: Option<u64>, cooldown_secs: Option<u64>) -> AutomationConfig {
    AutomationConfig {
        enabled: true,
        backend: AutomationBackend::CodexAppServer,
        host_mode: AutomationHostMode::Standalone,
        tasks: AutomationTaskSet {
            memory_curator: AutomationTaskConfig {
                enabled: true,
                schedule: Some("interval".to_string()),
                interval_secs,
                cooldown_secs,
                ..AutomationTaskConfig::default()
            },
            session_reflector: AutomationTaskConfig {
                enabled: true,
                schedule: Some("interval".to_string()),
                interval_secs,
                cooldown_secs,
                ..AutomationTaskConfig::default()
            },
            skill_writer: AutomationTaskConfig {
                enabled: true,
                schedule: Some("interval".to_string()),
                interval_secs,
                cooldown_secs,
                ..AutomationTaskConfig::default()
            },
        },
        ..AutomationConfig::default()
    }
}

fn scheduler_record(
    run_id: &str,
    status: AutomationRunStatus,
    completed_at: i64,
) -> AutomationRunLedgerRecord {
    scheduler_record_for(run_id, AgentTaskKind::MemoryCurator, status, completed_at)
}

fn scheduler_record_for(
    run_id: &str,
    task: AgentTaskKind,
    status: AutomationRunStatus,
    completed_at: i64,
) -> AutomationRunLedgerRecord {
    AutomationRunLedgerRecord {
        schema_version: 2,
        run_id: run_id.to_string(),
        trigger: AutomationTrigger::Scheduler,
        task,
        task_key: Some(test_task_key(task).to_string()),
        backend: "codex_app_server".to_string(),
        host_mode: Some("standalone".to_string()),
        prompt_version: Some(test_prompt_version(task).to_string()),
        response_schema: None,
        strict_json: None,
        model: None,
        status,
        evidence_hash: None,
        input_hash: None,
        output_hash: None,
        proposed_ops: None,
        applied_ops: None,
        rejected_ops: None,
        validation_report: None,
        reviewed_count: 0,
        accepted_count: 0,
        rejected_count: 0,
        skipped_count: usize::from(status == AutomationRunStatus::Skipped),
        error: None,
        error_classification: None,
        error_retryable: None,
        fallback_status: None,
        report_ref: None,
        artifacts: Vec::new(),
        started_at: (completed_at - 1).to_string(),
        completed_at: completed_at.to_string(),
    }
}

fn test_task_key(task: AgentTaskKind) -> &'static str {
    match task {
        AgentTaskKind::MemoryCurator => "memory_curator",
        AgentTaskKind::SessionReflector => "session_reflector",
        AgentTaskKind::SkillWriter => "skill_writer",
    }
}

fn test_prompt_version(task: AgentTaskKind) -> &'static str {
    match task {
        AgentTaskKind::MemoryCurator => "memory_curator:v1",
        AgentTaskKind::SessionReflector => "session_reflector:v1",
        AgentTaskKind::SkillWriter => "skill_writer:v1",
    }
}
