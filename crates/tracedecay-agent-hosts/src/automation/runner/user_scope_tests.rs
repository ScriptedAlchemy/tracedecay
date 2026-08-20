use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::{Value, json};
use tempfile::TempDir;
use tracedecay_domain::configuration::{ConfigurationRevisionId, UserProfileId};
use tracedecay_domain::{Confidence, FactId, FactOwnerV1, SessionId, TemporalCoverageCountsV1};
use tracedecay_global_db::RegisteredGlobalDbLeaseV1;
use tracedecay_global_db::tests::harness::RegisteredGlobalDbTestRuntime;

use super::*;
use crate::application::memory::{
    MemoryApplication, ProjectMemoryFactAddRequest, ProjectMemoryFactAddRequestOutcome,
};
use crate::automation::AutomationRunControl;
use crate::automation::backend::{
    AgentTaskBackend, AgentTaskKind, AgentTaskRequest, AgentTaskResponse,
};
use crate::automation::config::{
    AutomationBackend, AutomationHostMode, AutomationTaskConfig, AutomationTaskSet,
};
use crate::automation::run_ledger::AutomationRunStatus;
use crate::db::{Database, DatabaseAuthority, TestDatabaseRuntimeMode};
use crate::ports::project_runtime::{ProfileRuntime, RuntimeFuture};
use crate::store::memory::DatabaseFactStore;
use tracedecay_runtime_core::store_runtime::VerifiedGraphRuntimePortV1;
use tracedecay_store::{FactStoreError, ProjectMemoryGraphQueryV1};
use tracedecay_usecases::memory::MemoryApplicationError;

mod user_scope_graph_runtime;
use user_scope_graph_runtime::bind_profile_memory_graph_runtime;

struct FixtureProfileRuntime {
    profile_id: UserProfileId,
    sessions: RegisteredGlobalDbLeaseV1,
    memory: Database,
}

impl ProfileRuntime for FixtureProfileRuntime {
    fn profile_id(&self) -> &UserProfileId {
        &self.profile_id
    }

    fn profile_sessions(&self) -> RuntimeFuture<'_, RegisteredGlobalDbLeaseV1> {
        Box::pin(async { Ok(self.sessions.clone()) })
    }

    fn open_user_memory_db(&self) -> RuntimeFuture<'_, Database> {
        Box::pin(async { Ok(self.memory.clone()) })
    }
}

struct UserRuntimeHarness {
    profile_root: PathBuf,
    registry: Arc<dyn ProfileRuntime>,
    /// Strong graph port; the database keeps only a weak binding, so this
    /// handle keeps the profile memory graph mountable for the test lifetime.
    _memory_graph_runtime: Arc<dyn VerifiedGraphRuntimePortV1>,
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
        let (memory, _) = Database::publish_profile_memory_test_runtime(
            &memory_path,
            &authority,
            TestDatabaseRuntimeMode::Initialize,
        )
        .await
        .expect("registered profile memory");
        let memory_graph_runtime = bind_profile_memory_graph_runtime(&memory);
        let registry: Arc<dyn ProfileRuntime> = Arc::new(FixtureProfileRuntime {
            profile_id: UserProfileId::new("profile.automation.fixture").expect("profile id"),
            sessions: session_runtime.profile_database_arc(),
            memory,
        });
        Self {
            profile_root,
            registry,
            _memory_graph_runtime: memory_graph_runtime,
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

fn configuration_revision() -> ConfigurationRevisionId {
    ConfigurationRevisionId::new("config.user-automation-test.v1").expect("configuration revision")
}

fn test_run_control() -> AutomationRunControl {
    let interrupted = Arc::new(std::sync::atomic::AtomicBool::new(false));
    AutomationRunControl::from_interrupted({
        let interrupted = Arc::clone(&interrupted);
        Arc::new(move || interrupted.load(std::sync::atomic::Ordering::Acquire))
    })
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
    ) -> std::result::Result<AgentTaskResponse, tracedecay_automation::backend::AgentTaskError>
    {
        self.calls.fetch_add(1, Ordering::SeqCst);
        assert_eq!(request.task, self.task);
        Ok(AgentTaskResponse {
            run_id: request.run_id.clone(),
            task: request.task,
            output_text: self.output.to_string(),
            output_json: Some(self.output.clone()),
            model: Some("fixture-model".to_string()),
            provider: Some("fixture-provider".to_string()),
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
async fn projectless_reflection_uses_caller_supplied_automation_configuration() {
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
    let config = enabled_user_config();
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
        &test_run_control(),
        &configuration_revision(),
        AutomationTaskIo {
            backend: &backend,
            retrieval: &retrieval,
        },
        SessionReflectorAutomationOptions {
            provider: "hermes".to_string(),
            query: "user memory".to_string(),
            ..SessionReflectorAutomationOptions::default()
        },
    )
    .await
    .expect("profile reflection");

    assert_eq!(run.report["status"], json!("applied"));
    let database = harness.memory().await;
    let memory = MemoryApplication::new(FactOwnerV1::Profile, DatabaseFactStore::new(&database))
        .expect("profile memory authority");
    assert_eq!(
        memory
            .query_current_facts(
                tracedecay_store::CurrentFactsQuery::new(FactOwnerV1::Profile, None, 10)
                    .expect("canonical profile fact query"),
            )
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
        &configuration_revision(),
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
            &test_run_control(),
            &configuration_revision(),
            AutomationTaskIo {
                backend: &reflector_backend,
                retrieval: &retrieval,
            },
            SessionReflectorAutomationOptions::default(),
        )
        .await
        .expect("rejected reflector");
        let skill = run_user_skill_writer_with_backend_and_retrieval(
            &harness.profile_root,
            Arc::clone(&harness.registry),
            &config,
            &configuration_revision(),
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
        &test_run_control(),
        &configuration_revision(),
        AutomationTaskIo {
            backend: &reflector_backend,
            retrieval: &retrieval,
        },
        SessionReflectorAutomationOptions::default(),
    )
    .await
    .expect("empty reflector");
    let skill = run_user_skill_writer_with_backend_and_retrieval(
        &harness.profile_root,
        Arc::clone(&harness.registry),
        &config,
        &configuration_revision(),
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
async fn projectless_memory_curator_quarantines_deprecated_operations() {
    let harness = UserRuntimeHarness::open("user-curator-deprecated").await;
    let database = harness.memory().await;
    let run_control = test_run_control();
    let seeded = seed_user_duplicate_facts(&database, &run_control).await;
    let backend = JsonBackend::new(
        AgentTaskKind::MemoryCurator,
        json!({
            "ops": [{
                "op": "delete",
                "fact_id": seeded.loser_id,
                "confidence": 0.99,
                "reason": "legacy operation must not mutate canonical memory"
            }]
        }),
    );

    let error = run_user_memory_curator_with_backend(
        &harness.profile_root,
        Arc::clone(&harness.registry),
        &enabled_user_config(),
        &configuration_revision(),
        &backend,
        MemoryCuratorAutomationOptions::default(),
        &run_control,
    )
    .await
    .expect_err("deprecated operation must exhaust bounded repair and quarantine");

    assert_eq!(
        error.to_string(),
        "config error: memory curator validation repair budget exhausted; output quarantined"
    );
    let memory = MemoryApplication::new(FactOwnerV1::Profile, DatabaseFactStore::new(&database))
        .expect("profile memory authority");
    assert!(
        memory
            .get_project_memory_fact(
                tracedecay_store::ProjectMemoryFactIdV1::new(FactOwnerV1::Profile, seeded.loser_id)
                    .expect("canonical loser target"),
                run_control.read_control(),
            )
            .await
            .expect("canonical loser projection")
            .is_some()
    );
}

#[tokio::test]
async fn projectless_memory_curator_links_profile_memory_with_canonical_ids() {
    let harness = UserRuntimeHarness::open("user-curator-link").await;
    let database = harness.memory().await;
    let run_control = test_run_control();
    let seeded = seed_user_duplicate_facts(&database, &run_control).await;
    let backend = JsonBackend::new(
        AgentTaskKind::MemoryCurator,
        json!({
            "ops": [{
                "op": "link_facts",
                "source": {
                    "fact_id": seeded.winner_id,
                    "expected_last_event_id": seeded.winner_event_id,
                },
                "target": {
                    "fact_id": seeded.loser_id,
                    "expected_last_event_id": seeded.loser_event_id,
                },
                "relation": "supports",
                "evidence_facts": [{
                    "fact_id": seeded.winner_id,
                    "expected_last_event_id": seeded.winner_event_id,
                }],
                "confidence": 0.99,
                "source_label": "user-scope-fixture",
                "metadata": {"reason": "The durable profile preferences support each other"}
            }]
        }),
    );

    let run = run_user_memory_curator_with_backend(
        &harness.profile_root,
        Arc::clone(&harness.registry),
        &enabled_user_config(),
        &configuration_revision(),
        &backend,
        MemoryCuratorAutomationOptions::default(),
        &run_control,
    )
    .await
    .expect("profile memory curator");

    assert_eq!(run.report["llm_apply"]["applied"], json!(1));
    assert_eq!(
        run.report["llm_apply"]["receipts"][0]["receipt"]["facts_linked"],
        json!(1)
    );
}

#[tokio::test]
async fn projectless_memory_curator_normalizes_profile_fact_tags() {
    let harness = UserRuntimeHarness::open("user-curator-normalize-tags").await;
    let database = harness.memory().await;
    let run_control = test_run_control();
    let seeded = seed_user_duplicate_facts(&database, &run_control).await;
    let backend = JsonBackend::new(
        AgentTaskKind::MemoryCurator,
        json!({
            "ops": [{
                "op": "normalize_tags",
                "target": {
                    "fact_id": seeded.winner_id,
                    "expected_last_event_id": seeded.winner_event_id,
                },
                "tags": ["memory", "projectless"],
                "evidence_facts": [{
                    "fact_id": seeded.loser_id,
                    "expected_last_event_id": seeded.loser_event_id,
                }],
                "confidence": 0.99,
            }]
        }),
    );

    run_user_memory_curator_with_backend(
        &harness.profile_root,
        Arc::clone(&harness.registry),
        &enabled_user_config(),
        &configuration_revision(),
        &backend,
        MemoryCuratorAutomationOptions::default(),
        &run_control,
    )
    .await
    .expect("profile memory curator");

    let memory = MemoryApplication::new(FactOwnerV1::Profile, DatabaseFactStore::new(&database))
        .expect("profile memory authority");
    let fact = memory
        .get_project_memory_fact(
            tracedecay_store::ProjectMemoryFactIdV1::new(FactOwnerV1::Profile, seeded.winner_id)
                .expect("canonical winner target"),
            run_control.read_control(),
        )
        .await
        .expect("normalized fact")
        .expect("fact remains");
    let tracedecay_store::ProjectMemoryFactProjectionV1::Available(fact) = fact else {
        panic!("normalized fact payload must remain available");
    };
    assert_eq!(fact.tags(), ["memory", "projectless"]);
}

#[derive(Clone)]
struct SeededUserDuplicateFacts {
    winner_id: FactId,
    winner_event_id: tracedecay_domain::FactEventId,
    loser_id: FactId,
    loser_event_id: tracedecay_domain::FactEventId,
}

async fn seed_user_duplicate_facts(
    database: &Database,
    run_control: &AutomationRunControl,
) -> SeededUserDuplicateFacts {
    let owner = FactOwnerV1::Profile;
    let memory = MemoryApplication::new(owner.clone(), DatabaseFactStore::new(database))
        .expect("profile memory authority");
    let mut facts = Vec::with_capacity(2);
    for content in [
        "General conversations belong in user memory.",
        "General conversations belong in user memory!",
    ]
    .into_iter()
    {
        let preflight = memory
            .preflight_project_memory_fact_add(
                ProjectMemoryFactAddRequest {
                    content: content.to_string(),
                    category: tracedecay_domain::FactCategoryV1::UserPref,
                    source_label: Some("user-scope-fixture".to_string()),
                    tags: vec!["memory".to_string()],
                    entities: Vec::new(),
                    trust: Some(Confidence::new(0.95).expect("fixture confidence")),
                    metadata: json!({}),
                },
                None,
            )
            .expect("preflight profile fact");
        let write_control = run_control.write_control();
        let outcome = memory
            .add_preflighted_project_memory_fact(preflight, &write_control)
            .await
            .expect("seed profile fact");
        let ProjectMemoryFactAddRequestOutcome::Applied(outcome) = outcome else {
            panic!("fixture add must apply");
        };
        let tracedecay_store::ProjectMemoryFactProjectionV1::Available(fact) = outcome.fact()
        else {
            panic!("fixture fact payload must remain available");
        };
        facts.push((fact.fact_id().clone(), fact.last_event_id().clone()));
    }
    let roots = facts
        .iter()
        .map(|(fact_id, _)| fact_id.clone())
        .collect::<Vec<_>>();
    let mut graph_current = false;
    for _ in 0..512 {
        let query = ProjectMemoryGraphQueryV1::new(owner.clone(), roots.clone(), 4_096)
            .expect("profile graph readiness query");
        match memory
            .project_memory_graph(query, run_control.read_control())
            .await
        {
            Ok(_) => {
                graph_current = true;
                break;
            }
            Err(MemoryApplicationError::Store(
                FactStoreError::GraphConflict | FactStoreError::GraphUnavailable,
            )) => tokio::task::yield_now().await,
            Err(error) => panic!("profile graph readiness failed: {error}"),
        }
    }
    assert!(
        graph_current,
        "profile graph did not reach the seeded facts"
    );
    let (winner_id, winner_event_id) = facts.remove(0);
    let (loser_id, loser_event_id) = facts.remove(0);
    SeededUserDuplicateFacts {
        winner_id,
        winner_event_id,
        loser_id,
        loser_event_id,
    }
}
