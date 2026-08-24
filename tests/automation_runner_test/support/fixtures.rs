use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use serde_json::{Value, json};
use tracedecay::host_admission::HostAdmissionTestRuntimeV1;
use tracedecay::tracedecay::{TraceDecay, TraceDecayOpenOptions};
use tracedecay_agent_hosts::automation::automatic_facts::record_session_automatic_facts;
use tracedecay_agent_hosts::automation::run_ledger::{
    AutomationRunLedgerRecord, read_run_artifact_payload,
};
use tracedecay_sessions::runtime::{SessionMessageRecord, SessionRecord};
use tracedecay_usecases::host_admission::HostAdmissionScope;

use super::{project_memory_owner, test_automation_run_control};

/// Profile shard for a fixture project, pinned inside the fixture's own
/// temporary tree.
///
/// A bare `TraceDecay::init` resolves the profile from `TRACEDECAY_DATA_DIR`,
/// and `.cargo/config.toml` points that at the DURABLE, workspace-resident
/// `target/test-profile/.tracedecay`. Pairing a durable profile with a
/// `TempDir` project root is exactly the combination
/// `project_registry::ephemeral_root_rejection` refuses ("project root
/// '/tmp/.tmpXXXX' is under the OS temporary directory and cannot be
/// registered as a durable authority in profile '...'"), and every fixture in
/// this binary would additionally serialize on that one profile's exclusive
/// lifecycle lease. The hermetic escape hatch
/// (`TraceDecay::standalone_test_open_options`) is `cfg(test)`/`test-transport`
/// gated, so it is inactive for this integration binary and cannot be relied
/// on — the fixture must pin the profile itself, the same shape
/// `memory_eval_test::initialize_fixture_project` already uses.
///
/// The shard lives under the project's own `.tracedecay/` marker directory so
/// it is ephemeral (satisfying the guard), unique per fixture (no cross-test
/// lease contention), and invisible to the indexer. It is deliberately NOT
/// pre-created: `load_or_create_pinned` only applies the mandatory 0700
/// restriction to a root it created itself, and a pre-created root would then
/// fail `validate_private_profile_root`.
pub(crate) fn fixture_open_options(project_root: &Path) -> TraceDecayOpenOptions {
    let profile_root = project_root.join(".tracedecay").join("fixture-profile");
    TraceDecayOpenOptions {
        global_db_path: Some(profile_root.join("global.db")),
        profile_root: Some(profile_root),
    }
}

pub(crate) async fn init_project(project_root: &Path) -> TraceDecay {
    fs::create_dir_all(project_root.join("src")).unwrap();
    fs::write(project_root.join("src/lib.rs"), "pub fn fixture() {}\n").unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    TraceDecay::init_with_options(&project_root, fixture_open_options(&project_root))
        .await
        .unwrap()
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

#[derive(Debug, Clone)]
pub(crate) struct SeededDuplicateFacts {
    pub(crate) winner_id: String,
    pub(crate) winner_event_id: String,
    pub(crate) loser_id: String,
    pub(crate) loser_event_id: String,
}

pub(crate) async fn seed_duplicate_facts(cg: &TraceDecay) -> SeededDuplicateFacts {
    use tracedecay::store::memory::DatabaseFactStore;
    use tracedecay_usecases::memory::MemoryApplication;

    let owner = project_memory_owner(cg);
    let memory = MemoryApplication::new(owner.clone(), DatabaseFactStore::new(cg.db())).unwrap();
    let mut fact_ids = Vec::with_capacity(2);
    for (index, (content, trust)) in [
        ("Cache invalidation policy must be explicit", 0.97),
        ("Cache invalidation policy must be explicit!", 0.95),
    ]
    .into_iter()
    .enumerate()
    {
        let batch = record_session_automatic_facts(
            &memory,
            &test_automation_run_control(Arc::new(AtomicBool::new(false))),
            &format!("run.memory-curator-seed-{index}"),
            Some("evidence.memory-curator-seed"),
            &[json!({
                "add_fact_request": {
                    "content": content,
                    "category": "project",
                    "source_label": "memory-curator-test-seed",
                    "tags": ["cache", "policy"],
                    "entities": [],
                    "trust": trust,
                    "metadata": {},
                },
                "validation": {"status": "accepted"},
            })],
        )
        .await
        .unwrap();
        assert!(batch.retry_error.is_none());
        assert_eq!(batch.receipts.len(), 1);
        fact_ids.push(
            batch.receipts[0]
                .applied_fact_id
                .clone()
                .expect("seeded fact must have a canonical id"),
        );
    }
    assert_ne!(
        fact_ids[0], fact_ids[1],
        "curator fixture must persist two distinct canonical facts"
    );
    let facts = memory
        .query_current_facts(tracedecay_store::CurrentFactsQuery::new(owner, None, 10).unwrap())
        .await
        .unwrap();
    assert_eq!(facts.len(), 2, "curator fixture must expose both facts");
    let winner_id = fact_ids.remove(0);
    let loser_id = fact_ids.remove(0);
    let event_id = |fact_id: &str| {
        facts
            .iter()
            .find(|fact| fact.fact_id().as_str() == fact_id)
            .expect("seeded fact projection")
            .last_event_id()
            .as_str()
            .to_owned()
    };
    SeededDuplicateFacts {
        winner_event_id: event_id(&winner_id),
        loser_event_id: event_id(&loser_id),
        winner_id,
        loser_id,
    }
}

pub(crate) async fn fact_exists(
    cg: &TraceDecay,
    fact_id: &str,
    read_control: &tracedecay_store::FactReadControl,
) -> bool {
    use tracedecay::store::memory::DatabaseFactStore;
    use tracedecay_domain::FactId;
    use tracedecay_store::{ProjectMemoryFactIdV1, ProjectMemoryFactProjectionV1};
    use tracedecay_usecases::memory::MemoryApplication;

    let owner = project_memory_owner(cg);
    let memory = MemoryApplication::new(owner.clone(), DatabaseFactStore::new(cg.db())).unwrap();
    let fact_id = FactId::new(fact_id.to_owned()).unwrap();
    let fact_id = ProjectMemoryFactIdV1::new(owner, fact_id).unwrap();
    matches!(
        memory
            .get_project_memory_fact(fact_id, read_control)
            .await
            .unwrap(),
        Some(ProjectMemoryFactProjectionV1::Available(_))
    )
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
