use crate::support::*;
use tracedecay::application::memory::{MemoryApplication, MemoryOperationContext};
use tracedecay::automation::config::effective_user_automation_config;
use tracedecay::automation::runner::{
    run_user_memory_curator_with_backend, run_user_session_reflector_with_backend,
    run_user_skill_writer_with_backend, user_automation_root,
};
use tracedecay::db::Database;
use tracedecay::memory::types::{AddFactRequest, MemoryCategory};
use tracedecay::memory::user::{open_user_memory_db, user_memory_db_path};
use tracedecay::sessions::user_sessions_db_path;
use tracedecay::store::memory::DatabaseFactStore;
use tracedecay_domain::FactOwnerV1;

async fn seed_user_session(profile_root: &Path) {
    let db = GlobalDb::open_at(&user_sessions_db_path(profile_root))
        .await
        .expect("user sessions db open");
    seed_session_message_in_db(
        &db,
        profile_root,
        SeedSessionMessage {
            provider: "hermes",
            session_id: "user-session-1",
            message_id: "user-message-1",
            role: "user",
            timestamp: 1_715_100_001,
            text: "Remember this preference: always keep general conversations in user memory and review recurring automation workflows.",
            source: Some("hermes"),
        },
    )
    .await;
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
async fn projectless_reflection_reads_user_sessions_and_writes_user_memory() {
    let temp = tempdir().unwrap();
    let profile_root = temp.path();
    seed_user_session(profile_root).await;
    let backend = SessionJsonBackend::new(json!({
        "facts": [{
            "content": "The user wants general projectless conversations stored in profile-level user memory",
            "category": "user_pref",
            "tags": ["memory", "projectless"],
            "entities": ["TraceDecay"],
            "trust": 0.9,
            "source_span": {"session_id": "user-session-1", "message_id": "user-message-1"},
            "reason": "The user explicitly stated this durable preference"
        }]
    }));
    let config =
        effective_user_automation_config(profile_root, &AutomationConfig::default(), false)
            .await
            .unwrap();

    let run = run_user_session_reflector_with_backend(
        profile_root,
        &config,
        &backend,
        SessionReflectorAutomationOptions {
            provider: "hermes".to_string(),
            query: "user memory".to_string(),
            ..SessionReflectorAutomationOptions::default()
        },
    )
    .await
    .unwrap();

    assert_eq!(run.report["status"], json!("auto_applied"));
    assert_eq!(run.report["dry_run"], json!(false));
    let user_db = open_user_memory_db(profile_root).await.unwrap();
    let count: i64 = user_db
        .conn()
        .query("SELECT COUNT(*) FROM memory_facts", ())
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap();
    assert_eq!(count, 1);
    assert!(user_memory_db_path(profile_root).is_file());
    assert!(
        user_automation_root(profile_root)
            .join("automation_runs.jsonl")
            .is_file()
    );
    assert!(
        !profile_root
            .join("dashboard/automation_runs.jsonl")
            .exists()
    );
}

#[tokio::test]
async fn projectless_skill_writer_reads_user_sessions_and_uses_user_ledger() {
    let temp = tempdir().unwrap();
    let profile_root = temp.path();
    seed_user_session(profile_root).await;
    let backend = SkillJsonBackend::new(json!({"skills": []}));

    let run = run_user_skill_writer_with_backend(
        profile_root,
        &enabled_user_config(),
        &backend,
        SkillWriterAutomationOptions {
            provider: "hermes".to_string(),
            query: "automation workflows".to_string(),
            ..SkillWriterAutomationOptions::default()
        },
    )
    .await
    .unwrap();

    assert_eq!(backend.calls(), 1);
    assert_eq!(run.ledger_record.status, AutomationRunStatus::Succeeded);
    let records = load_run_records(&user_automation_root(profile_root), 10)
        .await
        .unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].task, AgentTaskKind::SkillWriter);
}

#[tokio::test]
async fn projectless_memory_curator_applies_validated_delete_to_user_memory() {
    let temp = tempdir().unwrap();
    let profile_root = temp.path();
    let db = open_user_memory_db(profile_root).await.unwrap();
    let seeded = seed_user_duplicate_facts(&db).await;
    drop(db);
    let backend = JsonBackend::new(json!({
        "ops": [{
            "cluster_id": "cluster-0000",
            "op": "delete",
            "fact_id": seeded.loser_id,
            "confidence": 0.99,
            "reason": "The older duplicate is no longer relevant"
        }]
    }));

    let run = run_user_memory_curator_with_backend(
        profile_root,
        &enabled_user_config(),
        &backend,
        MemoryCuratorAutomationOptions::default(),
    )
    .await
    .unwrap();

    assert_eq!(run.report["dry_run"], json!(false));
    assert_eq!(
        run.report["llm_apply"]["applied"],
        json!(1),
        "delete curation report: {}",
        run.report
    );
    let db = crate::common::open_test_database(&user_memory_db_path(profile_root))
        .await
        .unwrap()
        .0;
    let count: i64 = db
        .conn()
        .query(
            "SELECT COUNT(*) FROM memory_facts WHERE fact_id = ?1",
            libsql::params![seeded.loser_id],
        )
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap();
    assert_eq!(count, 0);
}

#[tokio::test]
async fn projectless_memory_curator_merges_and_updates_user_memory() {
    let temp = tempdir().unwrap();
    let profile_root = temp.path();
    let db = open_user_memory_db(profile_root).await.unwrap();
    let seeded = seed_user_duplicate_facts(&db).await;
    drop(db);
    let backend = JsonBackend::new(json!({
        "ops": [{
            "cluster_id": "cluster-0000",
            "op": "merge",
            "winner_id": seeded.winner_id,
            "loser_ids": [seeded.loser_id],
            "merged_content": "General projectless conversations belong in profile-level user memory",
            "confidence": 0.99,
            "reason": "Consolidate the duplicate preference into its current wording"
        }]
    }));

    let run = run_user_memory_curator_with_backend(
        profile_root,
        &enabled_user_config(),
        &backend,
        MemoryCuratorAutomationOptions::default(),
    )
    .await
    .unwrap();

    assert_eq!(
        run.report["llm_apply"]["applied"],
        json!(1),
        "merge curation report: {}",
        run.report
    );
    let db = crate::common::open_test_database(&user_memory_db_path(profile_root))
        .await
        .unwrap()
        .0;
    let mut rows = db
        .conn()
        .query(
            "SELECT fact_id, content FROM memory_facts ORDER BY fact_id",
            (),
        )
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    assert_eq!(row.get::<i64>(0).unwrap(), seeded.winner_id);
    assert_eq!(
        row.get::<String>(1).unwrap(),
        "General projectless conversations belong in profile-level user memory"
    );
    assert!(rows.next().await.unwrap().is_none());
}

#[tokio::test]
async fn projectless_memory_curator_grooms_user_memory() {
    let temp = tempdir().unwrap();
    let profile_root = temp.path();
    let db = open_user_memory_db(profile_root).await.unwrap();
    let seeded = seed_user_duplicate_facts(&db).await;
    drop(db);
    let backend = JsonBackend::new(json!({
        "ops": [{
            "cluster_id": "cluster-0000",
            "op": "normalize_tags",
            "fact_id": seeded.winner_id,
            "tags": ["memory", "projectless"],
            "evidence_fact_ids": [seeded.winner_id, seeded.loser_id],
            "confidence": 0.99,
            "reason": "Normalize the reviewed preference tags"
        }]
    }));

    let run = run_user_memory_curator_with_backend(
        profile_root,
        &enabled_user_config(),
        &backend,
        MemoryCuratorAutomationOptions::default(),
    )
    .await
    .unwrap();

    assert_eq!(
        run.report["llm_apply"]["applied"],
        json!(1),
        "grooming curation report: {}",
        run.report
    );
    let db = crate::common::open_test_database(&user_memory_db_path(profile_root))
        .await
        .unwrap()
        .0;
    let tags: String = db
        .conn()
        .query(
            "SELECT tags FROM memory_facts WHERE fact_id = ?1",
            libsql::params![seeded.winner_id],
        )
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap();
    assert_eq!(tags, "[\"memory\",\"projectless\"]");
}

#[derive(Clone, Copy)]
struct SeededUserDuplicateFacts {
    winner_id: i64,
    loser_id: i64,
}

async fn seed_user_duplicate_facts(db: &Database) -> SeededUserDuplicateFacts {
    let owner = FactOwnerV1::Profile;
    let memory = MemoryApplication::new(owner.clone(), DatabaseFactStore::new(db))
        .expect("initialize profile memory authority for duplicate fixture");
    let mut fact_ids = [0_i64; 2];
    for (index, content) in [
        // The authority preserves these as distinct facts, while the FHRR
        // encoder intentionally gives them the same token vector. That keeps
        // this curation fixture in the reviewed duplicate cluster without
        // bypassing the application with a hand-written legacy vector.
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
                    .expect("derive user duplicate fixture operation identity"),
            )
            .await
            .expect("seed user duplicate fixture through memory authority");
        fact_ids[index] = outcome
            .fact
            .expect("user duplicate fixture add must project a compatibility fact")
            .fact_id;
    }
    assert_ne!(fact_ids[0], fact_ids[1], "fixture facts must stay distinct");
    SeededUserDuplicateFacts {
        winner_id: fact_ids[0],
        loser_id: fact_ids[1],
    }
}
