#![allow(dead_code, unused_imports)]

pub(crate) use std::fs;
pub(crate) use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

pub(crate) use serde_json::{Value, json};
pub(crate) use tempfile::tempdir;

pub(crate) use tracedecay::application::host_admission::{
    HostAdmissionScope, HostAdmissionTestRuntimeV1,
};
pub(crate) use tracedecay::automation::backend::{
    AgentTaskBackend, AgentTaskFailureClass, AgentTaskKind, AgentTaskRequest, AgentTaskResponse,
};
pub(crate) use tracedecay::automation::config::{
    AutomationBackend, AutomationConfig, AutomationHostMode, AutomationTaskConfig,
    AutomationTaskSet,
};
pub(crate) use tracedecay::automation::fact_proposals::{
    FactProposalState, apply_fact_proposal, list_fact_proposals,
};
pub(crate) use tracedecay::automation::managed_skills::{
    ManagedSkillDraft, ManagedSkillProvenance, ManagedSkillSource, ManagedSkillState,
    ManagedSupportFile, approve_managed_skill, create_managed_skill_draft, load_managed_skill,
};
pub(crate) use tracedecay::automation::run_ledger::{
    AutomationRunLedgerRecord, AutomationRunStatus, AutomationTrigger, append_run_record,
    load_run_records, read_run_artifact_payload,
};
pub(crate) use tracedecay::automation::runner::{
    AutomationSessionRetrieval, AutomationSessionRetrievalFuture, AutomationTemporalEvidence,
    AutomationTemporalEvidenceItem, AutomationTemporalRetrieval, CombinedReviewAutomationOptions,
    CombinedReviewDispatch, MemoryCuratorAutomationOptions, SessionReflectorAutomationOptions,
    SkillWriterAutomationOptions, run_memory_curator_with_backend,
    run_skill_writer_with_backend_and_retrieval,
};
pub(crate) use tracedecay::errors::TraceDecayError;
pub(crate) use tracedecay::memory::encoding::HolographicEncoder;
pub(crate) use tracedecay::sessions::{SessionMessageRecord, SessionRecord};
pub(crate) use tracedecay::tracedecay::{TraceDecay, current_timestamp};
use tracedecay_domain::{ProjectId, SessionId, TemporalCoverageCountsV1};

pub(crate) static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

pub(crate) struct FixtureAutomationSessionRetrieval {
    anchor_session_id: SessionId,
}

impl FixtureAutomationSessionRetrieval {
    pub(crate) fn new(_cg: &TraceDecay) -> Self {
        Self {
            anchor_session_id: SessionId::new("session-reflect-1").unwrap(),
        }
    }
}

impl AutomationSessionRetrieval for FixtureAutomationSessionRetrieval {
    fn anchor_session_id(&self) -> &SessionId {
        &self.anchor_session_id
    }

    fn retrieve<'a>(
        &'a self,
        query: tracedecay::application::session::SessionTemporalQuery,
    ) -> AutomationSessionRetrievalFuture<'a> {
        assert_eq!(
            query.temporal_mode(),
            tracedecay_domain::TemporalModeV1::Forensic
        );
        assert_eq!(
            query.freshness_policy(),
            tracedecay::application::session::SessionFreshnessPolicy::RequireFresh
        );
        Box::pin(async move {
            let provider = query.provider().unwrap_or("cursor").to_string();
            let session_id = match query.retrieval_scope() {
                tracedecay::application::session::SessionRetrievalScope::Session(session_id) => {
                    session_id.as_str().to_string()
                }
                tracedecay::application::session::SessionRetrievalScope::AllSessionsInAuthorizedRoot => {
                    "session-reflect-1".to_string()
                }
            };
            let message_id = if session_id == "project-reflect-1" {
                "project-reflect-1-message-001"
            } else {
                "session-reflect-1-message-001"
            };
            AutomationTemporalRetrieval::Complete(AutomationTemporalEvidence {
                coverage: TemporalCoverageCountsV1 {
                    visible: 1,
                    hidden: 0,
                    unknown: 0,
                    redacted: 0,
                },
                items: vec![AutomationTemporalEvidenceItem {
                    anchor_id: "fixture-anchor-1".to_string(),
                    stable_id: "fixture-stable-1".to_string(),
                    provider,
                    session_id,
                    message_id: Some(message_id.to_string()),
                    source_id: Some("fixture-occurrence-1".to_string()),
                    store_id: Some(1),
                    role: Some("user".to_string()),
                    ordinal: Some(1),
                    session_total_messages: Some(1),
                    knowledge_at_micros: 1_715_000_000_000_000,
                    normalized_score_micros: 1_000_000,
                    snippet: json!({
                        "provider": query.provider().unwrap_or("cursor"),
                        "ordinal": 1,
                        "session_total_messages": 1,
                        "store_id": 1,
                        "text": query.query(),
                        "tool_names": "bash",
                        "metadata_json": "{\"cmd\":\"rg automation src\"}",
                    })
                    .to_string(),
                }],
            })
        })
    }
}

pub(crate) struct StaticAutomationSessionRetrieval {
    anchor_session_id: SessionId,
    item: AutomationTemporalEvidenceItem,
}

impl StaticAutomationSessionRetrieval {
    pub(crate) fn message(session_id: &str, message_id: &str, text: &str) -> Self {
        Self::message_for_provider("cursor", session_id, message_id, text)
    }

    pub(crate) fn message_for_provider(
        provider: &str,
        session_id: &str,
        message_id: &str,
        text: &str,
    ) -> Self {
        Self {
            anchor_session_id: SessionId::new(session_id).unwrap(),
            item: AutomationTemporalEvidenceItem {
                anchor_id: "static-anchor".to_string(),
                stable_id: "static-stable".to_string(),
                provider: provider.to_string(),
                session_id: session_id.to_string(),
                message_id: Some(message_id.to_string()),
                source_id: Some("static-occurrence".to_string()),
                store_id: Some(1),
                role: Some("user".to_string()),
                ordinal: Some(1),
                session_total_messages: Some(1),
                knowledge_at_micros: 1_715_000_001_000_000,
                normalized_score_micros: 1_000_000,
                snippet: text.to_string(),
            },
        }
    }
}

impl AutomationSessionRetrieval for StaticAutomationSessionRetrieval {
    fn anchor_session_id(&self) -> &SessionId {
        &self.anchor_session_id
    }

    fn retrieve<'a>(
        &'a self,
        query: tracedecay::application::session::SessionTemporalQuery,
    ) -> AutomationSessionRetrievalFuture<'a> {
        assert_eq!(
            query.temporal_mode(),
            tracedecay_domain::TemporalModeV1::Forensic
        );
        assert_eq!(
            query.freshness_policy(),
            tracedecay::application::session::SessionFreshnessPolicy::RequireFresh
        );
        Box::pin(async move {
            AutomationTemporalRetrieval::Complete(AutomationTemporalEvidence {
                items: vec![self.item.clone()],
                coverage: TemporalCoverageCountsV1 {
                    visible: 1,
                    hidden: 0,
                    unknown: 0,
                    redacted: 0,
                },
            })
        })
    }
}

pub(crate) struct RejectedAutomationSessionRetrieval {
    anchor_session_id: SessionId,
    reason: &'static str,
}

pub(crate) struct EmptyAutomationSessionRetrieval {
    anchor_session_id: SessionId,
}

impl EmptyAutomationSessionRetrieval {
    pub(crate) fn new() -> Self {
        Self {
            anchor_session_id: SessionId::new("empty-automation-fixture").unwrap(),
        }
    }
}

impl AutomationSessionRetrieval for EmptyAutomationSessionRetrieval {
    fn anchor_session_id(&self) -> &SessionId {
        &self.anchor_session_id
    }

    fn retrieve<'a>(
        &'a self,
        _query: tracedecay::application::session::SessionTemporalQuery,
    ) -> AutomationSessionRetrievalFuture<'a> {
        Box::pin(async { AutomationTemporalRetrieval::CompleteZero })
    }
}

impl RejectedAutomationSessionRetrieval {
    pub(crate) fn new(reason: &'static str) -> Self {
        Self {
            anchor_session_id: SessionId::new("rejected-automation-fixture").unwrap(),
            reason,
        }
    }
}

impl AutomationSessionRetrieval for RejectedAutomationSessionRetrieval {
    fn anchor_session_id(&self) -> &SessionId {
        &self.anchor_session_id
    }

    fn retrieve<'a>(
        &'a self,
        query: tracedecay::application::session::SessionTemporalQuery,
    ) -> AutomationSessionRetrievalFuture<'a> {
        assert_eq!(
            query.temporal_mode(),
            tracedecay_domain::TemporalModeV1::Forensic
        );
        assert_eq!(
            query.freshness_policy(),
            tracedecay::application::session::SessionFreshnessPolicy::RequireFresh
        );
        Box::pin(async move { AutomationTemporalRetrieval::Rejected(self.reason) })
    }
}

pub(crate) async fn run_session_reflector_with_backend(
    cg: &TraceDecay,
    config: &AutomationConfig,
    backend: &dyn AgentTaskBackend,
    options: SessionReflectorAutomationOptions,
) -> tracedecay::errors::Result<tracedecay::automation::runner::SessionReflectorAutomationRun> {
    let retrieval = FixtureAutomationSessionRetrieval::new(cg);
    tracedecay::automation::runner::run_session_reflector_with_backend_and_retrieval(
        cg, config, backend, &retrieval, options,
    )
    .await
}

pub(crate) async fn run_skill_writer_with_backend(
    cg: &TraceDecay,
    config: &AutomationConfig,
    backend: &dyn AgentTaskBackend,
    options: SkillWriterAutomationOptions,
) -> tracedecay::errors::Result<tracedecay::automation::runner::SkillWriterAutomationRun> {
    let retrieval = FixtureAutomationSessionRetrieval::new(cg);
    tracedecay::automation::runner::run_skill_writer_with_backend_and_retrieval(
        cg, config, backend, &retrieval, options,
    )
    .await
}

pub(crate) async fn run_combined_review_with_backend(
    cg: &TraceDecay,
    config: &AutomationConfig,
    backend: &dyn AgentTaskBackend,
    options: CombinedReviewAutomationOptions,
) -> tracedecay::errors::Result<CombinedReviewDispatch> {
    let retrieval = FixtureAutomationSessionRetrieval::new(cg);
    tracedecay::automation::runner::run_combined_review_with_backend_and_retrieval(
        cg, config, backend, &retrieval, options,
    )
    .await
}

pub(crate) fn project_memory_owner(cg: &TraceDecay) -> tracedecay_domain::FactOwnerV1 {
    let project_id = cg
        .store_layout()
        .identity
        .project_id
        .clone()
        .expect("initialized test project has an authoritative project id");
    tracedecay_domain::FactOwnerV1::Project {
        project_id: tracedecay_domain::ProjectId::new(project_id)
            .expect("initialized test project id is valid"),
    }
}

pub(crate) struct JsonBackend {
    calls: AtomicUsize,
    output: Value,
    model: Option<String>,
}

impl JsonBackend {
    pub(crate) fn new(output: Value) -> Self {
        Self::new_with_model(output, Some("fixture-model"))
    }

    pub(crate) fn new_with_model(output: Value, model: Option<&str>) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            output,
            model: model.map(str::to_string),
        }
    }

    pub(crate) fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl AgentTaskBackend for JsonBackend {
    fn run_task(
        &self,
        request: &AgentTaskRequest,
    ) -> tracedecay_automation::Result<AgentTaskResponse> {
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

pub(crate) struct SessionJsonBackend {
    calls: AtomicUsize,
    output: Value,
}

pub(crate) struct SkillJsonBackend {
    calls: AtomicUsize,
    output: Value,
    expected_activation_policy: &'static str,
}

pub(crate) struct SkillTextBackend {
    calls: AtomicUsize,
    output: &'static str,
}

pub(crate) struct InspectSkillWriterUsageBackend;

pub(crate) struct InspectSkillWriterUnderusedBackend;

pub(crate) struct FailingBackend {
    calls: AtomicUsize,
    task: AgentTaskKind,
    message: &'static str,
}

pub(crate) struct MalformedTextBackend {
    calls: AtomicUsize,
    task: AgentTaskKind,
    output: &'static str,
}

impl SkillJsonBackend {
    pub(crate) fn new(output: Value) -> Self {
        Self::with_activation_policy(output, "pending_approval_only")
    }

    pub(crate) fn with_activation_policy(
        output: Value,
        expected_activation_policy: &'static str,
    ) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            output,
            expected_activation_policy,
        }
    }

    pub(crate) fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl AgentTaskBackend for SkillJsonBackend {
    fn run_task(
        &self,
        request: &AgentTaskRequest,
    ) -> tracedecay_automation::Result<AgentTaskResponse> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        assert_eq!(request.task, AgentTaskKind::SkillWriter);
        assert_request_contract(request, "skill_writer", "skill_writer:v2", "skills");
        assert!(request.prompt.contains("managed skill creates or updates"));
        assert_eq!(request.context["apply"], json!(false));
        assert_eq!(
            request.context["activation_policy"],
            json!(self.expected_activation_policy)
        );
        assert!(
            request.context["skill_writer_evidence"]["hits"]
                .as_array()
                .is_some_and(|hits| !hits.is_empty())
        );
        let evidence = &request.context["skill_writer_evidence"];
        assert!(evidence["skill_usage_summaries"].is_array());
        assert!(evidence["stale_recommendations"].is_array());
        assert!(evidence["skill_improvement_recommendations"].is_array());
        if evidence["existing_managed_skills"]
            .as_array()
            .is_some_and(|skills| !skills.is_empty())
        {
            assert!(
                evidence["skill_usage_summaries"]
                    .as_array()
                    .is_some_and(|summaries| !summaries.is_empty())
            );
            assert!(
                evidence["stale_recommendations"]
                    .as_array()
                    .is_some_and(|recommendations| !recommendations.is_empty())
            );
            assert!(
                evidence["skill_improvement_recommendations"]
                    .as_array()
                    .is_some_and(|recommendations| !recommendations.is_empty())
            );
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
    pub(crate) fn new(output: &'static str) -> Self {
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
    ) -> tracedecay_automation::Result<AgentTaskResponse> {
        assert_eq!(request.task, AgentTaskKind::SkillWriter);
        assert_request_contract(request, "skill_writer", "skill_writer:v2", "skills");
        let summaries = request.context["skill_writer_evidence"]["skill_usage_summaries"]
            .as_array()
            .expect("skill usage summaries should be present");
        let summary = summaries
            .iter()
            .find(|summary| summary["skill_id"] == "automation-run-review")
            .expect("skill writer evidence should include automation-run-review usage");
        assert_eq!(summary["view_count"], json!(1));
        assert_eq!(summary["last_viewed_at"], json!(1_715_000_111_i64));
        assert!(
            summary["targets"]
                .as_array()
                .is_some_and(|targets| targets.contains(&json!("codex")))
        );
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
    ) -> tracedecay_automation::Result<AgentTaskResponse> {
        assert_eq!(request.task, AgentTaskKind::SkillWriter);
        assert_request_contract(request, "skill_writer", "skill_writer:v2", "skills");
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
        let recommendations =
            request.context["skill_writer_evidence"]["skill_improvement_recommendations"]
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

pub(crate) struct EnvVarGuard {
    key: &'static str,
    previous: Option<std::ffi::OsString>,
}

impl EnvVarGuard {
    pub(crate) fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
        let previous = std::env::var_os(key);
        unsafe {
            std::env::set_var(key, value);
        }
        Self { key, previous }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        unsafe {
            if let Some(previous) = self.previous.take() {
                std::env::set_var(self.key, previous);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }
}

/// Pins the profile database override at the test project's isolated session
/// store. Callers must hold [`ENV_LOCK`] while the guard is alive.
pub(crate) fn isolate_global_db(cg: &TraceDecay) -> EnvVarGuard {
    EnvVarGuard::set("TRACEDECAY_GLOBAL_DB", &cg.store_layout().sessions_db_path)
}

impl FailingBackend {
    pub(crate) fn new(task: AgentTaskKind) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            task,
            message: "codex app-server backend executable 'codex' was not found",
        }
    }

    pub(crate) fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl AgentTaskBackend for FailingBackend {
    fn run_task(
        &self,
        request: &AgentTaskRequest,
    ) -> tracedecay_automation::Result<AgentTaskResponse> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        assert_eq!(request.task, self.task);
        Err(tracedecay_automation::AutomationError::config(self.message))
    }
}

impl AgentTaskBackend for SkillTextBackend {
    fn run_task(
        &self,
        request: &AgentTaskRequest,
    ) -> tracedecay_automation::Result<AgentTaskResponse> {
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
    pub(crate) fn new(task: AgentTaskKind, output: &'static str) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            task,
            output,
        }
    }

    pub(crate) fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl AgentTaskBackend for MalformedTextBackend {
    fn run_task(
        &self,
        request: &AgentTaskRequest,
    ) -> tracedecay_automation::Result<AgentTaskResponse> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        assert_eq!(request.task, self.task);
        let (task_key, prompt_version, required_property) = match self.task {
            AgentTaskKind::MemoryCurator => ("memory_curator", "memory_curator:v1", "ops"),
            AgentTaskKind::SessionReflector => {
                ("session_reflector", "session_reflector:v2", "facts")
            }
            AgentTaskKind::SkillWriter => ("skill_writer", "skill_writer:v2", "skills"),
            AgentTaskKind::CombinedReview => ("combined_review", "combined_review:v1", "facts"),
            AgentTaskKind::UserJob => unreachable!("user jobs are not strict-JSON tasks"),
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
    pub(crate) fn new(output: Value) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            output,
        }
    }

    pub(crate) fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl AgentTaskBackend for SessionJsonBackend {
    fn run_task(
        &self,
        request: &AgentTaskRequest,
    ) -> tracedecay_automation::Result<AgentTaskResponse> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        assert_eq!(request.task, AgentTaskKind::SessionReflector);
        assert_request_contract(
            request,
            "session_reflector",
            "session_reflector:v2",
            "facts",
        );
        assert!(request.prompt.contains("durable memory facts"));
        assert_eq!(request.context["apply"], json!(false));
        assert!(
            request.context["session_reflection_evidence"]["hits"]
                .as_array()
                .is_some_and(|hits| !hits.is_empty())
        );
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

pub(crate) struct CombinedJsonBackend {
    calls: AtomicUsize,
    output: Value,
}

impl CombinedJsonBackend {
    pub(crate) fn new(output: Value) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            output,
        }
    }

    pub(crate) fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl AgentTaskBackend for CombinedJsonBackend {
    fn run_task(
        &self,
        request: &AgentTaskRequest,
    ) -> tracedecay_automation::Result<AgentTaskResponse> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        assert_eq!(request.task, AgentTaskKind::CombinedReview);
        assert_eq!(request.contract.task_key, "combined_review");
        assert_eq!(request.contract.prompt_version, "combined_review:v1");
        assert!(request.contract.strict_json);
        assert_eq!(
            request.contract.response_schema["required"],
            json!(["facts", "skills"])
        );
        // The combined prompt must compose both per-task prompts.
        assert!(request.prompt.contains("durable memory facts"));
        assert!(request.prompt.contains("managed skill creates or updates"));
        assert_eq!(request.context["apply"], json!(false));
        assert!(request.context["activation_policy"].is_string());
        assert!(
            request.context["session_reflection_evidence"]["hits"]
                .as_array()
                .is_some_and(|hits| !hits.is_empty())
        );
        assert!(
            request.context["skill_writer_evidence"]["hits"]
                .as_array()
                .is_some_and(|hits| !hits.is_empty())
        );
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

pub(crate) struct InspectSessionEvidenceBackend;

impl AgentTaskBackend for InspectSessionEvidenceBackend {
    fn run_task(
        &self,
        request: &AgentTaskRequest,
    ) -> tracedecay_automation::Result<AgentTaskResponse> {
        assert_eq!(request.task, AgentTaskKind::SessionReflector);
        assert_request_contract(
            request,
            "session_reflector",
            "session_reflector:v2",
            "facts",
        );
        let evidence = &request.context["session_reflection_evidence"];
        assert!(evidence.get("storage_scope").is_none());
        assert!(evidence.get("hermes_home").is_none());
        assert_eq!(evidence["provider"], json!("cursor"));
        assert!(
            evidence["query"]
                .as_str()
                .is_some_and(|query| query.contains("banana"))
        );
        assert_eq!(evidence["scope"], json!("session"));
        assert_eq!(evidence["session_id"], json!("project-reflect-1"));
        assert_eq!(evidence["include_summaries"], json!(false));
        assert_eq!(evidence["sort"], json!("relevance"));
        assert_eq!(evidence["source"], json!("project_lcm"));
        assert_eq!(evidence["role"], json!("assistant"));
        assert_eq!(evidence["start_time"], json!(1_715_100_000_i64));
        assert_eq!(evidence["end_time"], json!(1_715_100_010_i64));
        assert_eq!(evidence["evidence_mode"], json!("grep_only"));
        assert_eq!(evidence["recent_session_slices"], json!(null));
        let hits = evidence["hits"].as_array().expect("hits array");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0]["session_id"], json!("project-reflect-1"));
        assert!(
            hits[0]["snippet"]
                .as_str()
                .is_some_and(|text| !text.is_empty())
        );
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

/// Asserts the session reflector received session-replay evidence for one
/// recently active session (with no keyword grep hits) and replies with the
/// configured facts output.
pub(crate) struct SessionReplayEvidenceBackend {
    output: Value,
    expected_session_id: &'static str,
    expected_message_id: &'static str,
}

impl SessionReplayEvidenceBackend {
    pub(crate) fn new(
        output: Value,
        expected_session_id: &'static str,
        expected_message_id: &'static str,
    ) -> Self {
        Self {
            output,
            expected_session_id,
            expected_message_id,
        }
    }
}

impl AgentTaskBackend for SessionReplayEvidenceBackend {
    fn run_task(
        &self,
        request: &AgentTaskRequest,
    ) -> tracedecay_automation::Result<AgentTaskResponse> {
        assert_eq!(request.task, AgentTaskKind::SessionReflector);
        assert_request_contract(
            request,
            "session_reflector",
            "session_reflector:v2",
            "facts",
        );
        let evidence = &request.context["session_reflection_evidence"];
        assert_eq!(evidence["evidence_mode"], json!("session_replay_with_grep"));
        assert_eq!(
            evidence["hits"][0]["message_id"],
            json!(self.expected_message_id)
        );
        let slices = &evidence["recent_session_slices"];
        assert_eq!(slices["mode"], json!("recent_sessions"));
        assert_eq!(slices["session_selection"], json!("recent_activity"));
        assert_eq!(slices["bounds"]["head_turns"], json!(4));
        assert_eq!(slices["bounds"]["tail_turns"], json!(4));
        let sessions = slices["sessions"].as_array().expect("replay sessions");
        assert_eq!(sessions.len(), 1);
        assert_eq!(
            sessions[0]["session_id"],
            json!(self.expected_session_id),
            "replay slice should target the recently active session"
        );
        let head = sessions[0]["head"].as_array().expect("head turns");
        assert!(
            head.iter()
                .any(|message| message["message_id"] == json!(self.expected_message_id)),
            "replayed head turns should include the seeded message"
        );
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

/// Asserts the skill writer received session-replay evidence (with no
/// keyword grep hits) and replies with an empty skills array.
pub(crate) struct SkillWriterReplayEvidenceBackend {
    expected_session_id: &'static str,
}

impl SkillWriterReplayEvidenceBackend {
    pub(crate) fn new(expected_session_id: &'static str) -> Self {
        Self {
            expected_session_id,
        }
    }
}

impl AgentTaskBackend for SkillWriterReplayEvidenceBackend {
    fn run_task(
        &self,
        request: &AgentTaskRequest,
    ) -> tracedecay_automation::Result<AgentTaskResponse> {
        assert_eq!(request.task, AgentTaskKind::SkillWriter);
        assert_request_contract(request, "skill_writer", "skill_writer:v2", "skills");
        let evidence = &request.context["skill_writer_evidence"];
        assert_eq!(evidence["evidence_mode"], json!("session_replay_with_grep"));
        assert_eq!(
            evidence["hits"][0]["session_id"],
            json!(self.expected_session_id)
        );
        let slices = &evidence["recent_session_slices"];
        assert_eq!(slices["mode"], json!("recent_sessions"));
        assert_eq!(slices["session_selection"], json!("recent_activity"));
        let sessions = slices["sessions"].as_array().expect("replay sessions");
        assert!(
            sessions
                .iter()
                .any(|session| session["session_id"] == json!(self.expected_session_id)),
            "replay slices should include the recently active session"
        );
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

pub(crate) fn assert_request_contract(
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

pub(crate) fn assert_noop_fallback_record(
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
    assert!(
        record
            .output_hash
            .as_deref()
            .is_some_and(|hash| hash.starts_with("sha256:"))
    );
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
    assert!(
        record
            .error
            .as_deref()
            .is_some_and(|error| error.contains("executable"))
    );
}

pub(crate) async fn init_project(project_root: &Path) -> TraceDecay {
    fs::create_dir_all(project_root.join("src")).unwrap();
    fs::write(project_root.join("src/lib.rs"), "pub fn fixture() {}\n").unwrap();
    TraceDecay::init(project_root).await.unwrap()
}

#[cfg(feature = "test-transport")]
pub(crate) async fn project_session_runtime(
    cg: &TraceDecay,
) -> std::sync::Arc<HostAdmissionTestRuntimeV1> {
    cg.test_runtime_for_test()
        .expect("project graph should retain its registered test runtime")
}

#[cfg(feature = "test-transport")]
pub(crate) async fn seed_session_evidence(cg: &TraceDecay) {
    let db = project_session_runtime(cg).await;
    seed_session_message_in_db(
        &db,
        cg.project_root(),
        SeedSessionMessage {
            provider: "cursor",
            session_id: "session-reflect-1",
            message_id: "session-reflect-1-message-001",
            role: "user",
            timestamp: 1_715_000_001,
            text: "Remember TraceDecay automation should manage durable session reflection facts directly.",
            source: None,
        },
    )
    .await;
}

#[cfg(feature = "test-transport")]
pub(crate) async fn seed_search_underuse_session_evidence(cg: &TraceDecay) {
    let db = project_session_runtime(cg).await;
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
    assert!(
        db.upsert_session_for_test(HostAdmissionScope::Project, &session)
            .await
            .unwrap()
    );
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
    assert!(
        db.upsert_session_message_for_test(HostAdmissionScope::Project, &message)
            .await
            .unwrap()
    );
}

/// Seeds one session message at `timestamp` so the scheduler observes LCM
/// session activity at that instant.
#[cfg(feature = "test-transport")]
pub(crate) async fn seed_session_activity(cg: &TraceDecay, timestamp: i64) {
    let db = project_session_runtime(cg).await;
    seed_session_message_in_db(
        &db,
        cg.project_root(),
        SeedSessionMessage {
            provider: "cursor",
            session_id: "activity-fixture",
            message_id: &format!("activity-fixture-message-{timestamp}"),
            role: "user",
            timestamp,
            // Matches the default session_reflector and skill_writer grep
            // queries so evidence-driven runs see this message as a hit.
            text: "Remember this repeated workflow correction: prefer the skill tool pattern.",
            source: None,
        },
    )
    .await;
}

pub(crate) struct SeedSessionMessage<'a> {
    pub(crate) provider: &'a str,
    pub(crate) session_id: &'a str,
    pub(crate) message_id: &'a str,
    pub(crate) role: &'a str,
    pub(crate) timestamp: i64,
    pub(crate) text: &'a str,
    pub(crate) source: Option<&'a str>,
}

pub(crate) async fn seed_session_message_in_db(
    db: &HostAdmissionTestRuntimeV1,
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
    assert!(
        db.upsert_session_for_test(HostAdmissionScope::Project, &session)
            .await
            .unwrap()
    );
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
    assert!(
        db.upsert_session_message_for_test(HostAdmissionScope::Project, &message)
            .await
            .unwrap()
    );
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct SeededDuplicateFacts {
    pub(crate) winner_id: i64,
    pub(crate) loser_id: i64,
}

pub(crate) async fn seed_duplicate_facts(cg: &TraceDecay) -> SeededDuplicateFacts {
    use tracedecay::application::memory::{MemoryApplication, MemoryOperationContext};
    use tracedecay::memory::types::{AddFactRequest, MemoryCategory};
    use tracedecay::store::memory::DatabaseFactStore;

    let owner = project_memory_owner(cg);
    let memory = MemoryApplication::new(owner.clone(), DatabaseFactStore::new(cg.db())).unwrap();
    let mut fact_ids = [0_i64; 2];
    for (index, (content, trust)) in [
        ("Cache invalidation policy must be explicit", 0.97),
        ("Cache invalidation policy must stay explicit", 0.95),
    ]
    .into_iter()
    .enumerate()
    {
        let outcome = memory
            .add_fact_v1(
                AddFactRequest {
                    content: content.to_string(),
                    category: MemoryCategory::Project,
                    source: None,
                    tags: vec!["cache".to_string(), "policy".to_string()],
                    entities: Vec::new(),
                    trust: Some(trust),
                    metadata: json!({}),
                },
                MemoryOperationContext::generated(&owner, "seed automation duplicate fact", None)
                    .unwrap(),
            )
            .await
            .unwrap();
        fact_ids[index] = outcome.fact.expect("seeded fact must be projected").fact_id;
    }
    SeededDuplicateFacts {
        winner_id: fact_ids[0],
        loser_id: fact_ids[1],
    }
}

pub(crate) async fn fact_exists(cg: &TraceDecay, fact_id: i64) -> bool {
    cg.get_fact(fact_id).await.unwrap().is_some()
}

pub(crate) async fn read_artifact(
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

/// Standalone automation config with only the skill writer task enabled on a
/// manual schedule; override fields with struct update syntax where needed.
pub(crate) fn enabled_skill_writer_config() -> AutomationConfig {
    AutomationConfig {
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
    }
}

/// Manual-trigger skill writer options matching the seeded "automation" fixture
/// evidence, rooted at the given managed-skill profile directory.
pub(crate) fn manual_skill_writer_options(profile_root: &Path) -> SkillWriterAutomationOptions {
    SkillWriterAutomationOptions {
        provider: "cursor".to_string(),
        query: "automation".to_string(),
        evidence_limit: 5,
        profile_root: Some(profile_root.to_path_buf()),
        ..SkillWriterAutomationOptions::default()
    }
}

pub(crate) fn scheduler_config(
    interval_secs: Option<u64>,
    cooldown_secs: Option<u64>,
) -> AutomationConfig {
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

pub(crate) fn scheduler_record(
    run_id: &str,
    status: AutomationRunStatus,
    completed_at: i64,
) -> AutomationRunLedgerRecord {
    scheduler_record_for(run_id, AgentTaskKind::MemoryCurator, status, completed_at)
}

pub(crate) fn scheduler_record_for(
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
        backend_attempt_count: 0,
        backend_attempts: Vec::new(),
        fallback_status: None,
        report_ref: None,
        artifacts: Vec::new(),
        started_at: (completed_at - 1).to_string(),
        completed_at: completed_at.to_string(),
    }
}

pub(crate) fn test_task_key(task: AgentTaskKind) -> &'static str {
    match task {
        AgentTaskKind::MemoryCurator => "memory_curator",
        AgentTaskKind::SessionReflector => "session_reflector",
        AgentTaskKind::SkillWriter => "skill_writer",
        AgentTaskKind::CombinedReview => "combined_review",
        AgentTaskKind::UserJob => "user_job",
    }
}

pub(crate) fn test_prompt_version(task: AgentTaskKind) -> &'static str {
    match task {
        AgentTaskKind::MemoryCurator => "memory_curator:v1",
        AgentTaskKind::SessionReflector => "session_reflector:v2",
        AgentTaskKind::SkillWriter => "skill_writer:v2",
        AgentTaskKind::CombinedReview => "combined_review:v1",
        AgentTaskKind::UserJob => "user_job:v1",
    }
}
