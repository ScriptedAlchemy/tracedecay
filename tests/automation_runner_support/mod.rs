#![allow(dead_code, unused_imports)]

pub(crate) use std::fs;
pub(crate) use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

pub(crate) use serde_json::{json, Value};
pub(crate) use tempfile::tempdir;

pub(crate) use tracedecay::automation::backend::{
    AgentTaskBackend, AgentTaskFailureClass, AgentTaskKind, AgentTaskRequest, AgentTaskResponse,
};
pub(crate) use tracedecay::automation::config::{
    AutomationBackend, AutomationConfig, AutomationHostMode, AutomationTaskConfig,
    AutomationTaskSet,
};
pub(crate) use tracedecay::automation::fact_proposals::{
    apply_fact_proposal, list_fact_proposals, FactProposalState,
};
pub(crate) use tracedecay::automation::managed_skills::{
    approve_managed_skill, create_managed_skill_draft, load_managed_skill, ManagedSkillDraft,
    ManagedSkillProvenance, ManagedSkillSource, ManagedSkillState, ManagedSupportFile,
};
pub(crate) use tracedecay::automation::run_ledger::{
    append_run_record, load_run_records, read_run_artifact_payload, AutomationRunLedgerRecord,
    AutomationRunStatus, AutomationTrigger,
};
pub(crate) use tracedecay::automation::runner::{
    run_memory_curator_with_backend, run_session_reflector_with_backend,
    run_skill_writer_with_backend, MemoryCuratorAutomationOptions,
    SessionReflectorAutomationOptions, SkillWriterAutomationOptions,
};
pub(crate) use tracedecay::errors::TraceDecayError;
pub(crate) use tracedecay::global_db::GlobalDb;
pub(crate) use tracedecay::memory::encoding::HolographicEncoder;
pub(crate) use tracedecay::sessions::cursor::resolve_hermes_profile_session_db_path;
pub(crate) use tracedecay::sessions::lcm::{LcmGrepSort, LcmScope};
pub(crate) use tracedecay::sessions::{SessionMessageRecord, SessionRecord};
pub(crate) use tracedecay::tracedecay::{current_timestamp, TraceDecay};

pub(crate) static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

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

pub(crate) struct EnvVarGuard {
    key: &'static str,
    previous: Option<std::ffi::OsString>,
}

impl EnvVarGuard {
    pub(crate) fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
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

pub(crate) struct InspectSessionEvidenceBackend;

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

pub(crate) async fn init_project(project_root: &Path) -> TraceDecay {
    fs::create_dir_all(project_root.join("src")).unwrap();
    fs::write(project_root.join("src/lib.rs"), "pub fn fixture() {}\n").unwrap();
    TraceDecay::init(project_root).await.unwrap()
}

pub(crate) async fn seed_session_evidence(cg: &TraceDecay) {
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

pub(crate) async fn seed_search_underuse_session_evidence(cg: &TraceDecay) {
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

pub(crate) async fn seed_duplicate_facts(cg: &TraceDecay) {
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

pub(crate) async fn fact_exists(cg: &TraceDecay, fact_id: i64) -> bool {
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
    }
}

pub(crate) fn test_prompt_version(task: AgentTaskKind) -> &'static str {
    match task {
        AgentTaskKind::MemoryCurator => "memory_curator:v1",
        AgentTaskKind::SessionReflector => "session_reflector:v1",
        AgentTaskKind::SkillWriter => "skill_writer:v1",
    }
}
