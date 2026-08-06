use super::super::skill_usage::SkillUsageRecord;
use super::*;

static OUTCOME_PERSISTENCE_DB_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

const DAY: i64 = SECS_PER_DAY;

fn summary(skill_id: &str) -> SkillUsageRecord {
    SkillUsageRecord {
        schema_version: 1,
        skill_id: skill_id.to_string(),
        title: Some(format!("{skill_id} title")),
        category: Some("maintenance".to_string()),
        state: None,
        pinned: false,
        created_by: None,
        provenance_source: None,
        targets: Vec::new(),
        view_count: 0,
        use_count: 0,
        patch_count: 0,
        first_seen_at: 0,
        last_activity_at: 0,
        last_viewed_at: None,
        last_used_at: None,
        last_patched_at: None,
        approved_at: None,
        view_count_at_approval: None,
        use_count_at_approval: None,
    }
}

fn fact_input(
    proposal_id: &str,
    applied_at: i64,
    telemetry: Option<FactOutcomeTelemetry>,
) -> FactOutcomeInput {
    FactOutcomeInput {
        proposal_id: proposal_id.to_string(),
        run_id: "run_outcomes".to_string(),
        canonical_fact_id: format!("fact:{proposal_id}"),
        fact_id: Some(42),
        applied_at,
        telemetry,
    }
}

fn telemetry() -> FactOutcomeTelemetry {
    FactOutcomeTelemetry {
        retrieval_count: 0,
        access_count: 0,
        helpful_count: 0,
        unhelpful_count: 0,
        last_recalled_at: None,
    }
}

#[test]
fn skill_outcome_requires_an_approval_timestamp() {
    assert!(skill_outcome(&summary("draft-skill"), 100 * DAY).is_none());
}

#[test]
fn skill_used_after_approval_is_adopted() {
    let mut record = summary("adopted-skill");
    record.approved_at = Some(10 * DAY);
    record.view_count_at_approval = Some(3);
    record.use_count_at_approval = Some(1);
    record.view_count = 5;
    record.use_count = 4;
    record.last_used_at = Some(11 * DAY);

    let outcome = skill_outcome(&record, 12 * DAY).unwrap();
    assert_eq!(outcome.verdict, SkillOutcomeVerdict::Adopted);
    assert_eq!(outcome.views_since_approval, 2);
    assert_eq!(outcome.uses_since_approval, 3);
    assert_eq!(outcome.days_since_approval, 2);
}

#[test]
fn unused_skill_inside_window_is_too_early() {
    let mut record = summary("fresh-skill");
    record.approved_at = Some(10 * DAY);
    record.view_count_at_approval = Some(0);
    record.use_count_at_approval = Some(0);

    let outcome = skill_outcome(&record, 10 * DAY + SKILL_ADOPTION_WINDOW_SECS - 1).unwrap();
    assert_eq!(outcome.verdict, SkillOutcomeVerdict::TooEarly);
    assert_eq!(outcome.uses_since_approval, 0);
}

#[test]
fn unused_skill_past_window_is_ignored() {
    let mut record = summary("ignored-skill");
    record.approved_at = Some(10 * DAY);
    record.view_count_at_approval = Some(2);
    record.use_count_at_approval = Some(0);
    record.view_count = 4;
    record.last_viewed_at = Some(12 * DAY);

    let outcome = skill_outcome(&record, 10 * DAY + SKILL_ADOPTION_WINDOW_SECS).unwrap();
    assert_eq!(outcome.verdict, SkillOutcomeVerdict::Ignored);
    assert_eq!(outcome.views_since_approval, 2);
    assert_eq!(outcome.uses_since_approval, 0);
}

#[test]
fn legacy_ledger_without_baseline_uses_last_activity_fallback() {
    let mut record = summary("legacy-skill");
    record.approved_at = Some(10 * DAY);
    record.use_count = 2;
    record.last_used_at = Some(11 * DAY);

    let outcome = skill_outcome(&record, 20 * DAY).unwrap();
    assert_eq!(outcome.verdict, SkillOutcomeVerdict::Adopted);
    assert_eq!(outcome.uses_since_approval, 2);

    record.last_used_at = Some(9 * DAY);
    let outcome = skill_outcome(&record, 20 * DAY).unwrap();
    assert_eq!(outcome.verdict, SkillOutcomeVerdict::Ignored);
    assert_eq!(outcome.uses_since_approval, 0);
}

#[test]
fn deleted_fact_yields_deleted_verdict() {
    let outcome = fact_outcome(fact_input("fact_dead", 5 * DAY, None), 9 * DAY);
    assert_eq!(outcome.verdict, FactOutcomeVerdict::Deleted);
    assert!(!outcome.still_exists);
    assert_eq!(outcome.days_since_applied, 4);
}

#[test]
fn never_recalled_fact_yields_never_recalled_verdict() {
    let outcome = fact_outcome(fact_input("fact_idle", 5 * DAY, Some(telemetry())), 9 * DAY);
    assert_eq!(outcome.verdict, FactOutcomeVerdict::NeverRecalled);
    assert!(outcome.still_exists);
}

#[test]
fn recalled_fact_yields_recalled_verdict() {
    let mut telemetry = telemetry();
    telemetry.access_count = 3;
    telemetry.last_recalled_at = Some(8 * DAY);
    let outcome = fact_outcome(
        fact_input("fact_recalled", 5 * DAY, Some(telemetry)),
        9 * DAY,
    );
    assert_eq!(outcome.verdict, FactOutcomeVerdict::Recalled);
    assert_eq!(outcome.access_count, 3);
}

#[test]
fn recalled_and_helpful_fact_yields_top_verdict() {
    let mut telemetry = telemetry();
    telemetry.access_count = 2;
    telemetry.helpful_count = 1;
    let outcome = fact_outcome(
        fact_input("fact_helpful", 5 * DAY, Some(telemetry)),
        9 * DAY,
    );
    assert_eq!(outcome.verdict, FactOutcomeVerdict::RecalledAndHelpful);
}

#[test]
fn helpful_feedback_without_recall_is_not_recalled_and_helpful() {
    let mut telemetry = telemetry();
    telemetry.helpful_count = 1;
    let outcome = fact_outcome(
        fact_input("fact_feedback_only", 5 * DAY, Some(telemetry)),
        9 * DAY,
    );
    assert_eq!(outcome.verdict, FactOutcomeVerdict::NeverRecalled);
}

#[test]
fn deleted_fact_preserves_canonical_identity_without_numeric_mapping() {
    let mut input = fact_input("fact_no_mapping", 5 * DAY, None);
    input.fact_id = None;
    let outcome = fact_outcome(input, 9 * DAY);
    assert_eq!(outcome.canonical_fact_id, "fact:fact_no_mapping");
    assert_eq!(outcome.fact_id, None);
    assert_eq!(outcome.verdict, FactOutcomeVerdict::Deleted);
    let serialized = serde_json::to_value(outcome).unwrap();
    assert_eq!(serialized["canonical_fact_id"], "fact:fact_no_mapping");
    assert!(serialized.get("fact_id").is_some());
    assert!(serialized["fact_id"].is_null());
}

#[test]
fn legacy_outcome_snapshot_fact_keeps_numeric_mapping() {
    let legacy = json!({
        "proposal_id": "legacy-proposal",
        "run_id": "legacy-run",
        "fact_id": 42,
        "applied_at": 5 * DAY,
        "days_since_applied": 4,
        "retrieval_count": 0,
        "access_count": 0,
        "helpful_count": 0,
        "unhelpful_count": 0,
        "still_exists": false,
        "verdict": "deleted",
    });

    let outcome: FactOutcomeRecord = serde_json::from_value(legacy).unwrap();
    assert_eq!(outcome.canonical_fact_id, "");
    assert_eq!(outcome.fact_id, Some(42));
    assert_eq!(outcome.verdict, FactOutcomeVerdict::Deleted);
}

#[test]
fn outcome_eval_definitions_reflect_task_scope_and_verdicts() {
    let mut adopted = summary("adopted-skill");
    adopted.approved_at = Some(10 * DAY);
    adopted.use_count_at_approval = Some(0);
    adopted.use_count = 1;
    adopted.last_used_at = Some(11 * DAY);
    let snapshot = AutomationOutcomesSnapshot {
        schema_version: 1,
        skills: compute_skill_outcomes(&[adopted], 20 * DAY),
        facts: vec![fact_outcome(
            fact_input("fact_dead", 5 * DAY, None),
            20 * DAY,
        )],
        skills_refreshed_at: Some(20 * DAY),
        facts_refreshed_at: Some(20 * DAY),
    };

    let skill_evals =
        outcome_eval_definitions(AgentTaskKind::SkillWriter, "skill_writer", &snapshot);
    assert_eq!(skill_evals.len(), 1);
    assert_eq!(
        skill_evals[0].get("observed_outcome").unwrap(),
        &json!("adopted")
    );
    assert_eq!(skill_evals[0].get("passed").unwrap(), &json!(true));

    let fact_evals = outcome_eval_definitions(
        AgentTaskKind::SessionReflector,
        "session_reflector",
        &snapshot,
    );
    assert_eq!(fact_evals.len(), 1);
    assert_eq!(
        fact_evals[0].get("observed_outcome").unwrap(),
        &json!("deleted")
    );
    assert_eq!(
        fact_evals[0].pointer("/subject/canonical_fact_id").unwrap(),
        &json!("fact:fact_dead")
    );
    assert_eq!(fact_evals[0].get("passed").unwrap(), &json!(false));
}

#[test]
fn feedback_section_counts_verdicts_per_task() {
    let mut ignored = summary("ignored-skill");
    ignored.approved_at = Some(0);
    ignored.view_count_at_approval = Some(0);
    ignored.use_count_at_approval = Some(0);
    let snapshot = AutomationOutcomesSnapshot {
        schema_version: 1,
        skills: compute_skill_outcomes(&[ignored], 30 * DAY),
        facts: Vec::new(),
        skills_refreshed_at: Some(30 * DAY),
        facts_refreshed_at: None,
    };

    let section = outcome_feedback_section(AgentTaskKind::SkillWriter, &snapshot);
    assert_eq!(section.get("status").unwrap(), &json!("available"));
    assert_eq!(
        section.pointer("/skill_verdicts/ignored").unwrap(),
        &json!(1)
    );

    let empty = outcome_feedback_section(AgentTaskKind::SessionReflector, &snapshot);
    assert_eq!(empty.get("status").unwrap(), &json!("no_outcomes_recorded"));
}

#[tokio::test]
async fn refresh_skill_outcomes_persists_snapshot() {
    use super::super::managed_skills::{
        ManagedSkillDraft, ManagedSkillProvenance, ManagedSkillSource, approve_managed_skill,
        create_managed_skill_draft, default_managed_skill_targets,
    };

    let temp = tempfile::tempdir().unwrap();
    let profile_root = temp.path().join("profile");
    let dashboard_root = temp.path().join("dashboard");
    let skill = create_managed_skill_draft(
        &profile_root,
        ManagedSkillDraft {
            id: "outcome-skill".to_string(),
            title: "Outcome skill".to_string(),
            summary: "Outcome tracking fixture.".to_string(),
            category: "maintenance".to_string(),
            targets: default_managed_skill_targets(),
            body_markdown: "Use when checking outcomes.".to_string(),
            support_files: Vec::new(),
            provenance: ManagedSkillProvenance {
                source: ManagedSkillSource::AutomationRun,
                actor: "tracedecay".to_string(),
                run_id: Some("run_outcomes".to_string()),
            },
        },
    )
    .await
    .unwrap();
    approve_managed_skill(&profile_root, &skill.metadata.id)
        .await
        .unwrap();

    let now = crate::tracedecay::current_timestamp();
    let outcomes = refresh_skill_outcomes(&profile_root, &dashboard_root, now)
        .await
        .unwrap();
    assert_eq!(outcomes.len(), 1);
    assert_eq!(outcomes[0].skill_id, "outcome-skill");
    assert_eq!(outcomes[0].verdict, SkillOutcomeVerdict::TooEarly);

    let snapshot = load_outcomes_snapshot(&dashboard_root).await.unwrap();
    assert_eq!(snapshot.skills, outcomes);
    assert_eq!(snapshot.skills_refreshed_at, Some(now));
    assert!(snapshot.facts.is_empty());
}

async fn seed_approved_skill(profile_root: &Path) {
    use super::super::managed_skills::{
        ManagedSkillDraft, ManagedSkillProvenance, ManagedSkillSource, approve_managed_skill,
        create_managed_skill_draft, default_managed_skill_targets,
    };

    let skill = create_managed_skill_draft(
        profile_root,
        ManagedSkillDraft {
            id: "outcome-lock-skill".to_string(),
            title: "Outcome lock skill".to_string(),
            summary: "Outcome persistence fixture.".to_string(),
            category: "maintenance".to_string(),
            targets: default_managed_skill_targets(),
            body_markdown: "Use when testing outcome persistence.".to_string(),
            support_files: Vec::new(),
            provenance: ManagedSkillProvenance {
                source: ManagedSkillSource::AutomationRun,
                actor: "tracedecay".to_string(),
                run_id: Some("run-outcome-lock".to_string()),
            },
        },
    )
    .await
    .unwrap();
    approve_managed_skill(profile_root, &skill.metadata.id)
        .await
        .unwrap();
}

async fn seed_applied_fact_database(
    dashboard_root: &Path,
    database_path: &Path,
) -> crate::db::Database {
    use crate::application::memory::MemoryApplication;
    use crate::automation::fact_proposals::{apply_fact_proposal, record_session_fact_proposals};
    use crate::db::{Database, DatabaseAuthority, TestDatabaseRuntimeMode};
    use crate::store::memory::DatabaseFactStore;
    use tracedecay_domain::FactOwnerV1;

    crate::register_test_schema_installer();
    let authority =
        DatabaseAuthority::acquire_test(database_path, "outcome persistence test").unwrap();
    let (database, _) = Database::publish_test_runtime(
        database_path,
        &authority,
        TestDatabaseRuntimeMode::Initialize,
    )
    .await
    .unwrap();
    let memory =
        MemoryApplication::new(FactOwnerV1::Profile, DatabaseFactStore::new(&database)).unwrap();
    let records = record_session_fact_proposals(
        &memory,
        dashboard_root,
        "run-outcome-lock",
        None,
        &[json!({
            "add_fact_request": {
                "content": "Keep automation outcome snapshots atomically consistent",
                "category": "project",
                "source": "outcome-test",
                "tags": ["automation"],
                "entities": ["TraceDecay"],
                "trust": 0.9,
                "metadata": {}
            }
        })],
        &[],
    )
    .await
    .unwrap();
    apply_fact_proposal(&memory, dashboard_root, &records[0].proposal_id, None)
        .await
        .unwrap();
    database
}

#[tokio::test]
async fn concurrent_refreshes_preserve_both_snapshot_halves() {
    use crate::application::memory::MemoryApplication;
    use crate::store::memory::DatabaseFactStore;
    use tracedecay_domain::FactOwnerV1;

    let _database_guard = OUTCOME_PERSISTENCE_DB_TEST_LOCK.lock().await;
    let temp = tempfile::tempdir().unwrap();
    let profile_root = temp.path().join("profile");
    let dashboard_root = temp.path().join("dashboard");
    seed_approved_skill(&profile_root).await;
    let database =
        seed_applied_fact_database(&dashboard_root, &temp.path().join("memory.db")).await;
    let memory =
        MemoryApplication::new(FactOwnerV1::Profile, DatabaseFactStore::new(&database)).unwrap();
    let now = crate::tracedecay::current_timestamp();

    let (skills, facts) = tokio::join!(
        refresh_skill_outcomes(&profile_root, &dashboard_root, now),
        refresh_fact_outcomes(&dashboard_root, &memory, now),
    );
    let skills = skills.unwrap();
    let facts = facts.unwrap();
    let snapshot = load_outcomes_snapshot(&dashboard_root).await.unwrap();

    assert_eq!(snapshot.skills, skills);
    assert_eq!(snapshot.facts, facts);
    assert_eq!(snapshot.skills_refreshed_at, Some(now));
    assert_eq!(snapshot.facts_refreshed_at, Some(now));
}

#[tokio::test]
async fn malformed_snapshot_is_never_defaulted_or_overwritten() {
    use crate::application::memory::MemoryApplication;
    use crate::store::memory::DatabaseFactStore;
    use tracedecay_domain::FactOwnerV1;

    let _database_guard = OUTCOME_PERSISTENCE_DB_TEST_LOCK.lock().await;
    let temp = tempfile::tempdir().unwrap();
    let profile_root = temp.path().join("profile");
    let dashboard_root = temp.path().join("dashboard");
    seed_approved_skill(&profile_root).await;
    let database =
        seed_applied_fact_database(&dashboard_root, &temp.path().join("memory.db")).await;
    let memory =
        MemoryApplication::new(FactOwnerV1::Profile, DatabaseFactStore::new(&database)).unwrap();
    let path = automation_outcomes_path(&dashboard_root);
    tokio::fs::create_dir_all(&dashboard_root).await.unwrap();
    let malformed = b"{not-valid-json";
    tokio::fs::write(&path, malformed).await.unwrap();
    let now = crate::tracedecay::current_timestamp();

    let skill_error = refresh_skill_outcomes(&profile_root, &dashboard_root, now)
        .await
        .unwrap_err();
    assert!(skill_error.to_string().contains("failed to parse"));
    assert_eq!(tokio::fs::read(&path).await.unwrap(), malformed);

    let fact_error = refresh_fact_outcomes(&dashboard_root, &memory, now)
        .await
        .unwrap_err();
    assert!(fact_error.to_string().contains("failed to parse"));
    assert_eq!(tokio::fs::read(&path).await.unwrap(), malformed);
}
