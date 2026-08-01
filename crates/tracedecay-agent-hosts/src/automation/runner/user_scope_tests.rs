use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::{Value, json};
use tempfile::TempDir;
use tracedecay_domain::{FactOwnerV1, SessionId, TemporalCoverageCountsV1};
use tracedecay_global_db::RegisteredGlobalDb;
use tracedecay_global_db::tests::harness::RegisteredGlobalDbTestRuntime;

use super::*;
use crate::application::memory::{MemoryApplication, MemoryOperationContext};
use crate::automation::backend::{
    AgentTaskBackend, AgentTaskKind, AgentTaskRequest, AgentTaskResponse,
};
use crate::automation::config::{
    AutomationBackend, AutomationHostMode, AutomationTaskConfig, AutomationTaskSet,
    effective_user_automation_config,
};
use crate::db::{Database, DatabaseAuthority, TestDatabaseRuntimeMode};
use crate::memory::types::{AddFactRequest, MemoryCategory, MemoryGroomingOperation};
use crate::ports::project_runtime::{MemoryCurateOptions, ProfileRuntime, RuntimeFuture};
use crate::store::memory::DatabaseFactStore;

struct FixtureProfileRuntime {
    sessions: Arc<RegisteredGlobalDb>,
    memory: Database,
}

impl ProfileRuntime for FixtureProfileRuntime {
    fn profile_sessions(&self) -> RuntimeFuture<'_, Arc<RegisteredGlobalDb>> {
        Box::pin(async { Ok(Arc::clone(&self.sessions)) })
    }

    fn open_user_memory_db(&self) -> RuntimeFuture<'_, Database> {
        Box::pin(async { Ok(self.memory.clone()) })
    }

    fn curate_user_memory<'a>(
        &'a self,
        _profile_root: &'a std::path::Path,
        _automation_root: &'a std::path::Path,
        options: &'a MemoryCurateOptions,
    ) -> RuntimeFuture<'a, Value> {
        Box::pin(fixture_user_memory_curate(&self.memory, options))
    }
}

struct UserRuntimeHarness {
    profile_root: PathBuf,
    registry: Arc<dyn ProfileRuntime>,
    _session_runtime: RegisteredGlobalDbTestRuntime,
    _directory: TempDir,
}

impl UserRuntimeHarness {
    async fn open(_label: &str) -> Self {
        let directory = tempfile::tempdir().expect("user automation profile");
        let profile_root = directory.path().join("profile");
        let session_runtime = RegisteredGlobalDbTestRuntime::profile(&profile_root)
            .await
            .expect("registered profile session runtime");
        let memory_path = crate::memory::user::user_memory_db_path(&profile_root);
        let authority =
            DatabaseAuthority::acquire_test(&memory_path, "profile automation memory fixture")
                .expect("profile memory authority");
        let (memory, _) = Database::publish_test_runtime(
            &memory_path,
            &authority,
            TestDatabaseRuntimeMode::Initialize,
        )
        .await
        .expect("registered profile memory");
        let registry: Arc<dyn ProfileRuntime> = Arc::new(FixtureProfileRuntime {
            sessions: session_runtime.profile_database_arc(),
            memory,
        });
        Self {
            profile_root,
            registry,
            _session_runtime: session_runtime,
            _directory: directory,
        }
    }

    async fn memory(&self) -> Database {
        self.registry
            .open_user_memory_db()
            .await
            .expect("profile memory")
    }
}

async fn fixture_user_memory_curate(
    database: &Database,
    options: &MemoryCurateOptions,
) -> crate::errors::Result<Value> {
    let owner = FactOwnerV1::Profile;
    let memory = MemoryApplication::new(owner.clone(), DatabaseFactStore::new(database)).map_err(
        |error| TraceDecayError::Config {
            message: format!("initialize profile memory fixture: {error}"),
        },
    )?;
    if options.llm {
        let facts = memory
            .list_facts_untracked_v1(None, None, 100)
            .await
            .map_err(|error| TraceDecayError::Config {
                message: format!("read profile memory fixture: {error}"),
            })?;
        let allowed_fact_ids = facts.iter().map(|fact| fact.fact_id).collect::<Vec<_>>();
        return Ok(json!({
            "llm_review": {
                "status": if facts.len() >= 2 {
                    "needs_llm_review"
                } else {
                    "up_to_date"
                },
                "clusters_reviewed": usize::from(facts.len() >= 2),
                "allowed_fact_ids": allowed_fact_ids,
                "messages": [
                    {
                        "role": "system",
                        "content": "Return strict JSON memory curation operations."
                    },
                    {
                        "role": "user",
                        "content": "Review the bounded profile memory fixture."
                    }
                ]
            }
        }));
    }

    let raw_ops = options
        .llm_ops
        .as_ref()
        .and_then(|value| value.get("ops"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut accepted = Vec::new();
    let mut rejected = Vec::new();
    for operation in raw_ops {
        let kind = operation.get("op").and_then(Value::as_str);
        let confidence = operation
            .get("confidence")
            .and_then(Value::as_f64)
            .unwrap_or(0.0);
        if kind == Some("keep") {
            continue;
        }
        let valid = confidence >= options.min_confidence
            && match kind {
                Some("delete") => operation.get("fact_id").and_then(Value::as_i64).is_some(),
                Some("merge") => {
                    operation.get("winner_id").and_then(Value::as_i64).is_some()
                        && operation
                            .get("loser_ids")
                            .and_then(Value::as_array)
                            .is_some_and(|losers| !losers.is_empty())
                }
                Some(
                    "normalize_tags" | "merge_entities" | "add_alias" | "link_facts"
                    | "repair_vector",
                ) => serde_json::from_value::<MemoryGroomingOperation>(operation.clone()).is_ok(),
                _ => false,
            };
        if valid {
            accepted.push(operation);
        } else {
            rejected.push(json!({
                "op": operation,
                "rejected_reason": "invalid fixture curation operation"
            }));
        }
    }

    let mut applied = 0usize;
    if options.apply {
        let mut grooming = Vec::new();
        for operation in &accepted {
            let context = MemoryOperationContext::generated(
                &owner,
                "apply profile memory fixture curation",
                None,
            )
            .map_err(|error| TraceDecayError::Config {
                message: format!("create profile memory fixture operation: {error}"),
            })?;
            match operation.get("op").and_then(Value::as_str) {
                Some("delete") => {
                    let fact_id = operation
                        .get("fact_id")
                        .and_then(Value::as_i64)
                        .expect("validated delete fixture");
                    applied += usize::from(memory.remove_fact_v1(fact_id, context).await.map_err(
                        |error| TraceDecayError::Config {
                            message: format!("delete profile memory fixture fact: {error}"),
                        },
                    )?);
                }
                Some("merge") => {
                    let winner_id = operation
                        .get("winner_id")
                        .and_then(Value::as_i64)
                        .expect("validated merge winner");
                    let loser_ids = operation
                        .get("loser_ids")
                        .and_then(Value::as_array)
                        .expect("validated merge losers")
                        .iter()
                        .filter_map(Value::as_i64)
                        .collect();
                    let merged_content = operation
                        .get("merged_content")
                        .and_then(Value::as_str)
                        .map(str::to_owned);
                    memory
                        .dashboard_merge_fact_ids_v1(winner_id, loser_ids, merged_content, context)
                        .await
                        .map_err(|error| TraceDecayError::Config {
                            message: format!("merge profile memory fixture facts: {error}"),
                        })?;
                    applied += 1;
                }
                Some(
                    "normalize_tags" | "merge_entities" | "add_alias" | "link_facts"
                    | "repair_vector",
                ) => grooming.push(
                    serde_json::from_value(operation.clone())
                        .expect("validated grooming fixture operation"),
                ),
                _ => {}
            }
        }
        if !grooming.is_empty() {
            let context = MemoryOperationContext::generated(
                &owner,
                "apply profile memory fixture grooming",
                None,
            )
            .map_err(|error| TraceDecayError::Config {
                message: format!("create profile grooming fixture operation: {error}"),
            })?;
            let report = memory
                .dashboard_apply_grooming_v1(grooming, options.min_confidence, context)
                .await
                .map_err(|error| TraceDecayError::Config {
                    message: format!("groom profile memory fixture: {error}"),
                })?;
            applied += report.normalized_tags
                + report.merged_entities
                + report.aliases_added
                + report.facts_linked
                + report.vectors_repaired;
        }
    }

    Ok(json!({
        "llm_apply": {
            "ops": accepted,
            "rejected_ops": rejected,
            "applied": applied
        }
    }))
}

struct JsonBackend {
    task: AgentTaskKind,
    output: Value,
    calls: AtomicUsize,
}

impl JsonBackend {
    fn new(task: AgentTaskKind, output: Value) -> Self {
        Self {
            task,
            output,
            calls: AtomicUsize::new(0),
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
    ) -> tracedecay_automation::Result<AgentTaskResponse> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        assert_eq!(request.task, self.task);
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

enum RetrievalOutcome {
    Complete(Box<AutomationTemporalEvidenceItem>),
    Rejected(&'static str),
    Empty,
}

struct TestRetrieval {
    anchor_session_id: SessionId,
    outcome: RetrievalOutcome,
}

impl TestRetrieval {
    fn message(provider: &str, session_id: &str, message_id: &str, text: &str) -> Self {
        Self {
            anchor_session_id: SessionId::new(session_id).expect("session id"),
            outcome: RetrievalOutcome::Complete(Box::new(AutomationTemporalEvidenceItem {
                anchor_id: "user-scope-anchor".to_string(),
                stable_id: "user-scope-stable".to_string(),
                provider: provider.to_string(),
                session_id: session_id.to_string(),
                message_id: Some(message_id.to_string()),
                source_id: Some("user-scope-occurrence".to_string()),
                store_id: Some(1),
                role: Some("user".to_string()),
                ordinal: Some(1),
                session_total_messages: Some(1),
                knowledge_at_micros: 1_715_100_001_000_000,
                normalized_score_micros: 1_000_000,
                snippet: text.to_string(),
            })),
        }
    }

    fn rejected(reason: &'static str) -> Self {
        Self {
            anchor_session_id: SessionId::new("rejected-user-scope").expect("session id"),
            outcome: RetrievalOutcome::Rejected(reason),
        }
    }

    fn empty() -> Self {
        Self {
            anchor_session_id: SessionId::new("empty-user-scope").expect("session id"),
            outcome: RetrievalOutcome::Empty,
        }
    }
}

impl AutomationSessionRetrieval for TestRetrieval {
    fn anchor_session_id(&self) -> &SessionId {
        &self.anchor_session_id
    }

    fn retrieve(
        &self,
        _query: crate::application::session::SessionTemporalQuery,
    ) -> AutomationSessionRetrievalFuture<'_> {
        Box::pin(async move {
            match &self.outcome {
                RetrievalOutcome::Complete(item) => {
                    AutomationTemporalRetrieval::Complete(AutomationTemporalEvidence {
                        items: vec![item.as_ref().clone()],
                        coverage: TemporalCoverageCountsV1 {
                            visible: 1,
                            hidden: 0,
                            unknown: 0,
                            redacted: 0,
                        },
                    })
                }
                RetrievalOutcome::Rejected(reason) => AutomationTemporalRetrieval::Rejected(reason),
                RetrievalOutcome::Empty => AutomationTemporalRetrieval::CompleteZero,
            }
        })
    }
}

fn enabled_user_config() -> AutomationConfig {
    AutomationConfig {
        enabled: true,
        backend: AutomationBackend::CodexAppServer,
        host_mode: AutomationHostMode::Standalone,
        auto_apply_memory_ops: false,
        tasks: AutomationTaskSet {
            memory_curator: AutomationTaskConfig {
                enabled: true,
                schedule: Some("manual".to_string()),
                ..AutomationTaskConfig::default()
            },
            session_reflector: AutomationTaskConfig {
                enabled: true,
                schedule: Some("manual".to_string()),
                ..AutomationTaskConfig::default()
            },
            skill_writer: AutomationTaskConfig {
                enabled: true,
                schedule: Some("manual".to_string()),
                ..AutomationTaskConfig::default()
            },
        },
        ..AutomationConfig::default()
    }
}

#[tokio::test]
async fn projectless_reflection_writes_registered_profile_memory() {
    let harness = UserRuntimeHarness::open("user-reflection").await;
    let backend = JsonBackend::new(
        AgentTaskKind::SessionReflector,
        json!({
            "facts": [{
                "content": "The user wants projectless conversations stored in profile memory",
                "category": "user_pref",
                "tags": ["memory", "projectless"],
                "entities": ["TraceDecay"],
                "trust": 0.9,
                "source_span": {
                    "session_id": "user-session-1",
                    "message_id": "user-message-1"
                },
                "reason": "The user explicitly stated this durable preference"
            }]
        }),
    );
    let config = effective_user_automation_config(
        &harness.profile_root,
        &AutomationConfig::default(),
        false,
    )
    .await
    .expect("effective user config");
    let retrieval = TestRetrieval::message(
        "hermes",
        "user-session-1",
        "user-message-1",
        "Always keep general conversations in user memory.",
    );

    let run = run_user_session_reflector_with_backend_and_retrieval(
        &harness.profile_root,
        Arc::clone(&harness.registry),
        &config,
        &backend,
        &retrieval,
        SessionReflectorAutomationOptions {
            provider: "hermes".to_string(),
            query: "user memory".to_string(),
            ..SessionReflectorAutomationOptions::default()
        },
    )
    .await
    .expect("profile reflection");

    assert_eq!(run.report["status"], json!("auto_applied"));
    let database = harness.memory().await;
    let memory = MemoryApplication::new(FactOwnerV1::Profile, DatabaseFactStore::new(&database))
        .expect("profile memory authority");
    assert_eq!(
        memory
            .list_facts_untracked_v1(None, None, 10)
            .await
            .expect("profile facts")
            .len(),
        1
    );
    assert!(database.database_path().is_file());
}

#[tokio::test]
async fn projectless_skill_writer_uses_user_ledger() {
    let harness = UserRuntimeHarness::open("user-skill-writer").await;
    let backend = JsonBackend::new(AgentTaskKind::SkillWriter, json!({ "skills": [] }));
    let retrieval = TestRetrieval::message(
        "hermes",
        "user-session-1",
        "user-message-1",
        "Review recurring automation workflows.",
    );

    let run = run_user_skill_writer_with_backend_and_retrieval(
        &harness.profile_root,
        Arc::clone(&harness.registry),
        &enabled_user_config(),
        &backend,
        &retrieval,
        SkillWriterAutomationOptions {
            provider: "hermes".to_string(),
            query: "automation workflows".to_string(),
            ..SkillWriterAutomationOptions::default()
        },
    )
    .await
    .expect("profile skill writer");

    assert_eq!(backend.calls(), 1);
    assert_eq!(run.ledger_record.status, AutomationRunStatus::Succeeded);
    let records = crate::automation::run_ledger::load_run_records(
        &user_automation_root(&harness.profile_root),
        10,
    )
    .await
    .expect("user ledger");
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].task, AgentTaskKind::SkillWriter);
}

#[tokio::test]
async fn terminal_evidence_rejections_do_not_run_user_backends() {
    for reason in [
        "session_evidence_denied",
        "session_evidence_stale",
        "session_evidence_partial",
        "session_evidence_unavailable",
        "session_evidence_budget_exhausted",
        "session_evidence_cancelled",
    ] {
        let harness = UserRuntimeHarness::open("user-rejection").await;
        let retrieval = TestRetrieval::rejected(reason);
        let reflector_backend =
            JsonBackend::new(AgentTaskKind::SessionReflector, json!({ "facts": [] }));
        let skill_backend = JsonBackend::new(AgentTaskKind::SkillWriter, json!({ "skills": [] }));
        let config = enabled_user_config();

        let reflector = run_user_session_reflector_with_backend_and_retrieval(
            &harness.profile_root,
            Arc::clone(&harness.registry),
            &config,
            &reflector_backend,
            &retrieval,
            SessionReflectorAutomationOptions::default(),
        )
        .await
        .expect("rejected reflector");
        let skill = run_user_skill_writer_with_backend_and_retrieval(
            &harness.profile_root,
            Arc::clone(&harness.registry),
            &config,
            &skill_backend,
            &retrieval,
            SkillWriterAutomationOptions::default(),
        )
        .await
        .expect("rejected skill writer");

        assert_eq!(reflector.ledger_record.error.as_deref(), Some(reason));
        assert_eq!(skill.ledger_record.error.as_deref(), Some(reason));
        assert_eq!(reflector_backend.calls(), 0);
        assert_eq!(skill_backend.calls(), 0);
        assert!(!user_automation_root(&harness.profile_root).exists());
    }

    let harness = UserRuntimeHarness::open("user-empty-evidence").await;
    let retrieval = TestRetrieval::empty();
    let reflector_backend =
        JsonBackend::new(AgentTaskKind::SessionReflector, json!({ "facts": [] }));
    let skill_backend = JsonBackend::new(AgentTaskKind::SkillWriter, json!({ "skills": [] }));
    let config = enabled_user_config();
    let reflector = run_user_session_reflector_with_backend_and_retrieval(
        &harness.profile_root,
        Arc::clone(&harness.registry),
        &config,
        &reflector_backend,
        &retrieval,
        SessionReflectorAutomationOptions::default(),
    )
    .await
    .expect("empty reflector");
    let skill = run_user_skill_writer_with_backend_and_retrieval(
        &harness.profile_root,
        Arc::clone(&harness.registry),
        &config,
        &skill_backend,
        &retrieval,
        SkillWriterAutomationOptions::default(),
    )
    .await
    .expect("empty skill writer");
    assert_eq!(
        reflector.ledger_record.error.as_deref(),
        Some("no_session_evidence")
    );
    assert_eq!(
        skill.ledger_record.error.as_deref(),
        Some("no_skill_writer_evidence")
    );
}

#[tokio::test]
async fn projectless_memory_curator_applies_validated_delete() {
    let harness = UserRuntimeHarness::open("user-curator-delete").await;
    let database = harness.memory().await;
    let seeded = seed_user_duplicate_facts(&database).await;
    let backend = JsonBackend::new(
        AgentTaskKind::MemoryCurator,
        json!({
            "ops": [{
                "cluster_id": "cluster-0000",
                "op": "delete",
                "fact_id": seeded.loser_id,
                "confidence": 0.99,
                "reason": "The older duplicate is no longer relevant"
            }]
        }),
    );

    let run = run_user_memory_curator_with_backend(
        &harness.profile_root,
        Arc::clone(&harness.registry),
        &enabled_user_config(),
        &backend,
        MemoryCuratorAutomationOptions::default(),
    )
    .await
    .expect("profile memory curator");

    assert_eq!(run.report["llm_apply"]["applied"], json!(1));
    let memory = MemoryApplication::new(FactOwnerV1::Profile, DatabaseFactStore::new(&database))
        .expect("profile memory authority");
    assert!(
        memory
            .get_fact_v1(seeded.loser_id)
            .await
            .expect("deleted fact")
            .is_none()
    );
}

#[tokio::test]
async fn projectless_memory_curator_merges_and_updates_profile_memory() {
    let harness = UserRuntimeHarness::open("user-curator-merge").await;
    let database = harness.memory().await;
    let seeded = seed_user_duplicate_facts(&database).await;
    let backend = JsonBackend::new(
        AgentTaskKind::MemoryCurator,
        json!({
            "ops": [{
                "cluster_id": "cluster-0000",
                "op": "merge",
                "winner_id": seeded.winner_id,
                "loser_ids": [seeded.loser_id],
                "merged_content": "General projectless conversations belong in profile memory",
                "confidence": 0.99,
                "reason": "Consolidate the duplicate preference"
            }]
        }),
    );

    run_user_memory_curator_with_backend(
        &harness.profile_root,
        Arc::clone(&harness.registry),
        &enabled_user_config(),
        &backend,
        MemoryCuratorAutomationOptions::default(),
    )
    .await
    .expect("profile memory curator");

    let memory = MemoryApplication::new(FactOwnerV1::Profile, DatabaseFactStore::new(&database))
        .expect("profile memory authority");
    let facts = memory
        .list_facts_untracked_v1(None, None, 10)
        .await
        .expect("profile facts");
    assert_eq!(facts.len(), 1);
    assert_eq!(facts[0].fact_id, seeded.winner_id);
    assert_eq!(
        facts[0].content,
        "General projectless conversations belong in profile memory"
    );
}

#[tokio::test]
async fn projectless_memory_curator_grooms_profile_memory() {
    let harness = UserRuntimeHarness::open("user-curator-groom").await;
    let database = harness.memory().await;
    let seeded = seed_user_duplicate_facts(&database).await;
    let backend = JsonBackend::new(
        AgentTaskKind::MemoryCurator,
        json!({
            "ops": [{
                "cluster_id": "cluster-0000",
                "op": "normalize_tags",
                "fact_id": seeded.winner_id,
                "tags": ["memory", "projectless"],
                "evidence_fact_ids": [seeded.loser_id],
                "confidence": 0.99,
                "reason": "Normalize the reviewed preference tags"
            }]
        }),
    );

    run_user_memory_curator_with_backend(
        &harness.profile_root,
        Arc::clone(&harness.registry),
        &enabled_user_config(),
        &backend,
        MemoryCuratorAutomationOptions::default(),
    )
    .await
    .expect("profile memory curator");

    let memory = MemoryApplication::new(FactOwnerV1::Profile, DatabaseFactStore::new(&database))
        .expect("profile memory authority");
    let fact = memory
        .get_fact_v1(seeded.winner_id)
        .await
        .expect("groomed fact")
        .expect("fact remains");
    assert_eq!(fact.tags, ["memory", "projectless"]);
}

#[derive(Clone, Copy)]
struct SeededUserDuplicateFacts {
    winner_id: i64,
    loser_id: i64,
}

async fn seed_user_duplicate_facts(database: &Database) -> SeededUserDuplicateFacts {
    let owner = FactOwnerV1::Profile;
    let memory = MemoryApplication::new(owner.clone(), DatabaseFactStore::new(database))
        .expect("profile memory authority");
    let mut fact_ids = [0_i64; 2];
    for (index, content) in [
        "General conversations belong in user memory.",
        "General conversations belong in user memory!",
    ]
    .into_iter()
    .enumerate()
    {
        let outcome = memory
            .add_fact_v1(
                AddFactRequest {
                    content: content.to_string(),
                    category: MemoryCategory::UserPref,
                    source: Some("user-scope-fixture".to_string()),
                    tags: vec!["memory".to_string()],
                    entities: Vec::new(),
                    trust: Some(0.95),
                    metadata: json!({}),
                },
                MemoryOperationContext::generated(&owner, "seed user duplicate fact", None)
                    .expect("memory operation"),
            )
            .await
            .expect("seed profile fact");
        fact_ids[index] = outcome.fact.expect("compatibility fact").fact_id;
    }
    SeededUserDuplicateFacts {
        winner_id: fact_ids[0],
        loser_id: fact_ids[1],
    }
}
