use super::super::skill_usage::SkillUsageRecord;
use super::*;
use crate::automation::AutomationRunControl;
use std::sync::Arc;

static OUTCOME_PERSISTENCE_DB_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

const DAY: i64 = SECS_PER_DAY;

fn outcome_read_control() -> AutomationRunControl {
    AutomationRunControl::from_interrupted(Arc::new(|| false))
}

fn summary(skill_id: &str) -> SkillUsageRecord {
    SkillUsageRecord {
        schema_version: 3,
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
        activated_at: None,
        view_count_at_activation: None,
        use_count_at_activation: None,
    }
}

fn fact_input(
    apply_id: &str,
    state: ProjectMemoryAutomaticFactStateV1,
    canonical_fact_id: Option<&str>,
    recorded_at: i64,
    observation: FactOutcomeObservation,
) -> FactOutcomeInput {
    FactOutcomeInput {
        apply_id: apply_id.to_string(),
        run_id: Some("run_outcomes".to_string()),
        state,
        canonical_fact_id: canonical_fact_id.map(ToOwned::to_owned),
        recorded_at,
        observation,
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
fn skill_outcome_requires_an_activation_timestamp() {
    assert!(skill_outcome(&summary("draft-skill"), 100 * DAY).is_none());
}

#[test]
fn skill_used_after_activation_is_adopted() {
    let mut record = summary("adopted-skill");
    record.activated_at = Some(10 * DAY);
    record.view_count_at_activation = Some(3);
    record.use_count_at_activation = Some(1);
    record.view_count = 5;
    record.use_count = 4;
    record.last_used_at = Some(11 * DAY);

    let outcome = skill_outcome(&record, 12 * DAY).unwrap();
    assert_eq!(outcome.verdict, SkillOutcomeVerdict::Adopted);
    assert_eq!(outcome.views_since_activation, 2);
    assert_eq!(outcome.uses_since_activation, 3);
    assert_eq!(outcome.days_since_activation, 2);
}

#[test]
fn unused_skill_inside_window_is_too_early() {
    let mut record = summary("fresh-skill");
    record.activated_at = Some(10 * DAY);
    record.view_count_at_activation = Some(0);
    record.use_count_at_activation = Some(0);

    let outcome = skill_outcome(&record, 10 * DAY + SKILL_ACTIVATION_WINDOW_SECS - 1).unwrap();
    assert_eq!(outcome.verdict, SkillOutcomeVerdict::TooEarly);
    assert_eq!(outcome.uses_since_activation, 0);
}

#[test]
fn unused_skill_past_window_is_ignored() {
    let mut record = summary("ignored-skill");
    record.activated_at = Some(10 * DAY);
    record.view_count_at_activation = Some(2);
    record.use_count_at_activation = Some(0);
    record.view_count = 4;
    record.last_viewed_at = Some(12 * DAY);

    let outcome = skill_outcome(&record, 10 * DAY + SKILL_ACTIVATION_WINDOW_SECS).unwrap();
    assert_eq!(outcome.verdict, SkillOutcomeVerdict::Ignored);
    assert_eq!(outcome.views_since_activation, 2);
    assert_eq!(outcome.uses_since_activation, 0);
}

#[test]
fn ledger_without_activation_baseline_uses_last_activity_fallback() {
    let mut record = summary("legacy-skill");
    record.activated_at = Some(10 * DAY);
    record.use_count = 2;
    record.last_used_at = Some(11 * DAY);

    let outcome = skill_outcome(&record, 20 * DAY).unwrap();
    assert_eq!(outcome.verdict, SkillOutcomeVerdict::Adopted);
    assert_eq!(outcome.uses_since_activation, 2);

    record.last_used_at = Some(9 * DAY);
    let outcome = skill_outcome(&record, 20 * DAY).unwrap();
    assert_eq!(outcome.verdict, SkillOutcomeVerdict::Ignored);
    assert_eq!(outcome.uses_since_activation, 0);
}

#[test]
fn deleted_fact_yields_deleted_verdict() {
    let outcome = fact_outcome(
        fact_input(
            "apply_fact_dead",
            ProjectMemoryAutomaticFactStateV1::Applied,
            Some("fact:dead"),
            5 * DAY,
            FactOutcomeObservation::Deleted,
        ),
        9 * DAY,
    );
    assert_eq!(outcome.verdict, FactOutcomeVerdict::Deleted);
    assert!(!outcome.still_exists);
    assert_eq!(outcome.days_since_recorded, 4);
    assert_eq!(outcome.retrieval_count, None);
}

#[test]
fn never_recalled_fact_yields_never_recalled_verdict() {
    let outcome = fact_outcome(
        fact_input(
            "apply_fact_idle",
            ProjectMemoryAutomaticFactStateV1::Applied,
            Some("fact:idle"),
            5 * DAY,
            FactOutcomeObservation::Available(telemetry()),
        ),
        9 * DAY,
    );
    assert_eq!(outcome.verdict, FactOutcomeVerdict::NeverRecalled);
    assert!(outcome.still_exists);
}

#[test]
fn recalled_fact_yields_recalled_verdict() {
    let mut telemetry = telemetry();
    telemetry.access_count = 3;
    telemetry.last_recalled_at = Some(8 * DAY);
    let outcome = fact_outcome(
        fact_input(
            "apply_fact_recalled",
            ProjectMemoryAutomaticFactStateV1::Applied,
            Some("fact:recalled"),
            5 * DAY,
            FactOutcomeObservation::Available(telemetry),
        ),
        9 * DAY,
    );
    assert_eq!(outcome.verdict, FactOutcomeVerdict::Recalled);
    assert_eq!(outcome.access_count, Some(3));
}

#[test]
fn recalled_and_helpful_fact_yields_top_verdict() {
    let mut telemetry = telemetry();
    telemetry.access_count = 2;
    telemetry.helpful_count = 1;
    let outcome = fact_outcome(
        fact_input(
            "apply_fact_helpful",
            ProjectMemoryAutomaticFactStateV1::Applied,
            Some("fact:helpful"),
            5 * DAY,
            FactOutcomeObservation::Available(telemetry),
        ),
        9 * DAY,
    );
    assert_eq!(outcome.verdict, FactOutcomeVerdict::RecalledAndHelpful);
}

#[test]
fn quarantined_receipt_preserves_its_terminal_state_without_a_projection() {
    let outcome = fact_outcome(
        fact_input(
            "apply_fact_quarantined",
            ProjectMemoryAutomaticFactStateV1::Quarantined,
            None,
            5 * DAY,
            FactOutcomeObservation::Quarantined,
        ),
        9 * DAY,
    );
    assert_eq!(outcome.apply_id, "apply_fact_quarantined");
    assert_eq!(
        outcome.state,
        ProjectMemoryAutomaticFactStateV1::Quarantined
    );
    assert_eq!(outcome.canonical_fact_id, None);
    assert_eq!(outcome.verdict, FactOutcomeVerdict::Quarantined);
    assert!(!outcome.still_exists);
    assert_eq!(outcome.access_count, None);
    let serialized = serde_json::to_value(outcome).unwrap();
    assert_eq!(serialized["apply_id"], "apply_fact_quarantined");
    assert!(serialized.get("proposal_id").is_none());
    assert!(serialized.get("fact_id").is_none());
}

async fn seed_activated_skill(profile_root: &Path) {
    use super::super::managed_skills::{
        ManagedSkillDraft, ManagedSkillProvenance, ManagedSkillSource, create_managed_skill,
        default_managed_skill_targets,
    };

    create_managed_skill(
        profile_root,
        ManagedSkillDraft {
            id: "outcome-lock-skill".to_string(),
            title: "Outcome lock skill".to_string(),
            summary: "Outcome persistence fixture.".to_string(),
            routing_description:
                "Repeated repository workflows requiring this maintained procedure.".to_owned(),
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
}

async fn seed_applied_fact_database(database_path: &Path) -> tracedecay_runtime_core::db::Database {
    use crate::automation::AutomationRunControl;
    use crate::automation::automatic_facts::{AutomaticFactState, record_session_automatic_facts};
    use tracedecay_domain::FactOwnerV1;
    use tracedecay_runtime_core::db::{Database, DatabaseAuthority, TestDatabaseRuntimeMode};
    use tracedecay_session_memory::fact_store::DatabaseFactStore;
    use tracedecay_session_memory::memory::MemoryApplication;

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
    let interrupted = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let run_control = AutomationRunControl::from_interrupted({
        let interrupted = std::sync::Arc::clone(&interrupted);
        std::sync::Arc::new(move || interrupted.load(std::sync::atomic::Ordering::Acquire))
    });
    let batch = record_session_automatic_facts(
        &memory,
        &run_control,
        "run-outcome-lock",
        None,
        &[json!({
            "add_fact_request": {
                "content": "Keep automation outcome snapshots atomically consistent",
                "category": "project",
                "source_label": "outcome-test",
                "tags": ["automation"],
                "entities": ["TraceDecay"],
                "trust": 0.9,
                "metadata": {}
            }
        })],
    )
    .await
    .unwrap();
    assert!(batch.retry_error.is_none());
    assert_eq!(batch.receipts[0].state, AutomaticFactState::Applied);
    database
}

#[tokio::test]
async fn concurrent_refreshes_preserve_both_snapshot_halves() {
    use tracedecay_domain::FactOwnerV1;
    use tracedecay_session_memory::fact_store::DatabaseFactStore;
    use tracedecay_session_memory::memory::MemoryApplication;

    let _database_guard = OUTCOME_PERSISTENCE_DB_TEST_LOCK.lock().await;
    let temp = tempfile::tempdir().unwrap();
    let profile_root = temp.path().join("profile");
    let dashboard_root = temp.path().join("dashboard");
    seed_activated_skill(&profile_root).await;
    let database = seed_applied_fact_database(&temp.path().join("memory.db")).await;
    let memory =
        MemoryApplication::new(FactOwnerV1::Profile, DatabaseFactStore::new(&database)).unwrap();
    let now = tracedecay_runtime_core::tracedecay::current_timestamp();
    let run_control = outcome_read_control();

    let (skills, facts) = tokio::join!(
        refresh_skill_outcomes(&profile_root, &dashboard_root, now),
        refresh_fact_outcomes(&dashboard_root, &memory, now, run_control.read_control()),
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
    use tracedecay_domain::FactOwnerV1;
    use tracedecay_session_memory::fact_store::DatabaseFactStore;
    use tracedecay_session_memory::memory::MemoryApplication;

    let _database_guard = OUTCOME_PERSISTENCE_DB_TEST_LOCK.lock().await;
    let temp = tempfile::tempdir().unwrap();
    let profile_root = temp.path().join("profile");
    let dashboard_root = temp.path().join("dashboard");
    seed_activated_skill(&profile_root).await;
    let database = seed_applied_fact_database(&temp.path().join("memory.db")).await;
    let memory =
        MemoryApplication::new(FactOwnerV1::Profile, DatabaseFactStore::new(&database)).unwrap();
    let path = automation_outcomes_path(&dashboard_root);
    tokio::fs::create_dir_all(&dashboard_root).await.unwrap();
    let malformed = b"{not-valid-json";
    tokio::fs::write(&path, malformed).await.unwrap();
    let now = tracedecay_runtime_core::tracedecay::current_timestamp();
    let run_control = outcome_read_control();

    let skill_error = refresh_skill_outcomes(&profile_root, &dashboard_root, now)
        .await
        .unwrap_err();
    assert!(skill_error.to_string().contains("failed to parse"));
    assert_eq!(tokio::fs::read(&path).await.unwrap(), malformed);

    let fact_error =
        refresh_fact_outcomes(&dashboard_root, &memory, now, run_control.read_control())
            .await
            .unwrap_err();
    assert!(fact_error.to_string().contains("failed to parse"));
    assert_eq!(tokio::fs::read(&path).await.unwrap(), malformed);
}
