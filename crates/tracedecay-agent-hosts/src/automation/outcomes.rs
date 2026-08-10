//! Post-application outcome tracking for automatically curated changes.
//!
//! The automation loops validate and apply skills and facts automatically, but
//! application alone says nothing about whether the change was good. This module
//! measures what happened *after* automatic application:
//!
//! - automatically activated managed skills: adoption derived from the usage ledger
//!   (`adopted` / `ignored` / `too_early`),
//! - automatic-fact receipts: terminal application state plus any post-apply
//!   recall trajectory in the memory store (`recalled_and_helpful` / `recalled`
//!   / `never_recalled` / `deleted` / `quarantined` / `unavailable`).
//!
//! Outcomes are persisted as a snapshot under the dashboard root so the next
//! automation run for the same task can fold real-quality signal into its
//! `feedback` and `generated_evals` artifacts, and so the dashboard can render
//! them read-only.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex, Weak};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tracedecay_store::{
    MAX_PROJECT_MEMORY_AUTOMATIC_FACT_RECEIPTS, ProjectMemoryAutomaticFactReceiptV1,
    ProjectMemoryAutomaticFactStateV1, ProjectMemoryFactAvailabilityV1, ProjectMemoryFactIdV1,
    ProjectMemoryFactProjectionV1, ProjectMemoryFactStore, ProjectMemoryFactTargetV1,
};

use super::backend::AgentTaskKind;
use super::config_error;
use super::managed_skills::{ManagedSkillState, list_managed_skills};
use super::skill_usage::{SkillUsageSummary, summarize_skill_usage};
use crate::application::memory::MemoryApplication;
use crate::errors::{Result, TraceDecayError};

const AUTOMATION_OUTCOMES_FILENAME: &str = "automation_outcomes.json";
/// Outcome refreshes update independent halves of one snapshot. This lock
/// serializes their read-modify-write critical sections for one dashboard.
static AUTOMATION_OUTCOMES_LOCKS: LazyLock<Mutex<HashMap<PathBuf, Weak<tokio::sync::Mutex<()>>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static AUTOMATION_OUTCOMES_TEMP_NONCE: AtomicU64 = AtomicU64::new(0);

/// A skill is `too_early` to judge until this long after activation.
pub const SKILL_ACTIVATION_WINDOW_SECS: i64 = 7 * 24 * 60 * 60;

const SECS_PER_DAY: i64 = 24 * 60 * 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillOutcomeVerdict {
    Adopted,
    Ignored,
    TooEarly,
}

impl SkillOutcomeVerdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Adopted => "adopted",
            Self::Ignored => "ignored",
            Self::TooEarly => "too_early",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FactOutcomeVerdict {
    RecalledAndHelpful,
    Recalled,
    NeverRecalled,
    Deleted,
    Quarantined,
    Unavailable,
}

impl FactOutcomeVerdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RecalledAndHelpful => "recalled_and_helpful",
            Self::Recalled => "recalled",
            Self::NeverRecalled => "never_recalled",
            Self::Deleted => "deleted",
            Self::Quarantined => "quarantined",
            Self::Unavailable => "unavailable",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SkillOutcomeRecord {
    pub skill_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub activated_at: i64,
    pub days_since_activation: i64,
    pub views_since_activation: u64,
    pub uses_since_activation: u64,
    pub verdict: SkillOutcomeVerdict,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FactOutcomeRecord {
    /// Immutable identity of the terminal automatic-fact receipt.
    pub apply_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    pub state: ProjectMemoryAutomaticFactStateV1,
    /// Present only for receipts whose terminal effect applied a canonical fact.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canonical_fact_id: Option<String>,
    pub recorded_at: i64,
    pub days_since_recorded: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retrieval_count: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access_count: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub helpful_count: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unhelpful_count: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_recalled_at: Option<i64>,
    pub still_exists: bool,
    pub verdict: FactOutcomeVerdict,
}

#[derive(Debug, Clone, PartialEq)]
struct FactOutcomeTelemetry {
    retrieval_count: u64,
    access_count: u64,
    helpful_count: u64,
    unhelpful_count: u64,
    last_recalled_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq)]
enum FactOutcomeObservation {
    Available(FactOutcomeTelemetry),
    Deleted,
    Quarantined,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq)]
struct FactOutcomeInput {
    apply_id: String,
    run_id: Option<String>,
    state: ProjectMemoryAutomaticFactStateV1,
    canonical_fact_id: Option<String>,
    recorded_at: i64,
    observation: FactOutcomeObservation,
}

/// Persisted, per-project snapshot of the most recently computed outcomes.
/// Skill and fact halves are refreshed independently because they need
/// different inputs (profile root vs memory store connection).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AutomationOutcomesSnapshot {
    #[serde(default)]
    pub schema_version: u32,
    #[serde(default)]
    pub skills: Vec<SkillOutcomeRecord>,
    #[serde(default)]
    pub facts: Vec<FactOutcomeRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skills_refreshed_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub facts_refreshed_at: Option<i64>,
}

impl AutomationOutcomesSnapshot {
    pub fn is_empty(&self) -> bool {
        self.skills.is_empty() && self.facts.is_empty()
    }
}

/// Computes the adoption verdict for one activated skill. `None` when the
/// skill has never been activated (no post-activation window to measure).
pub fn skill_outcome(summary: &SkillUsageSummary, now_unix: i64) -> Option<SkillOutcomeRecord> {
    let activated_at = summary.activated_at?;
    let secs_since_activation = now_unix.saturating_sub(activated_at);
    let views_since_activation = count_since_activation(
        summary.view_count,
        summary.view_count_at_activation,
        summary.last_viewed_at,
        activated_at,
    );
    let uses_since_activation = count_since_activation(
        summary.use_count,
        summary.use_count_at_activation,
        summary.last_used_at,
        activated_at,
    );
    let verdict = if uses_since_activation > 0 {
        SkillOutcomeVerdict::Adopted
    } else if secs_since_activation < SKILL_ACTIVATION_WINDOW_SECS {
        SkillOutcomeVerdict::TooEarly
    } else {
        SkillOutcomeVerdict::Ignored
    };
    Some(SkillOutcomeRecord {
        skill_id: summary.skill_id.clone(),
        title: summary.title.clone(),
        activated_at,
        days_since_activation: secs_since_activation / SECS_PER_DAY,
        views_since_activation,
        uses_since_activation,
        verdict,
    })
}

/// Activity since activation, preferring the exact baseline captured at
/// activation time. Ledgers written before baselines existed fall back to the
/// last-activity timestamp: activity at or after activation counts the full
/// total (a conservative over-count is fine for adoption detection).
fn count_since_activation(
    total: u64,
    baseline_at_activation: Option<u64>,
    last_activity_at: Option<i64>,
    activated_at: i64,
) -> u64 {
    match baseline_at_activation {
        Some(baseline) => total.saturating_sub(baseline),
        None if last_activity_at.is_some_and(|at| at >= activated_at) => total,
        None => 0,
    }
}

/// Computes the current trajectory from one terminal automatic-fact receipt.
fn fact_outcome(input: FactOutcomeInput, now_unix: i64) -> FactOutcomeRecord {
    let recorded_at = input.recorded_at;
    let mut record = FactOutcomeRecord {
        apply_id: input.apply_id,
        run_id: input.run_id,
        state: input.state,
        canonical_fact_id: input.canonical_fact_id,
        recorded_at,
        days_since_recorded: now_unix.saturating_sub(recorded_at) / SECS_PER_DAY,
        retrieval_count: None,
        access_count: None,
        helpful_count: None,
        unhelpful_count: None,
        last_recalled_at: None,
        still_exists: false,
        verdict: match &input.observation {
            FactOutcomeObservation::Deleted => FactOutcomeVerdict::Deleted,
            FactOutcomeObservation::Quarantined => FactOutcomeVerdict::Quarantined,
            FactOutcomeObservation::Unavailable => FactOutcomeVerdict::Unavailable,
            FactOutcomeObservation::Available(_) => FactOutcomeVerdict::NeverRecalled,
        },
    };
    let FactOutcomeObservation::Available(telemetry) = input.observation else {
        return record;
    };
    record.retrieval_count = Some(telemetry.retrieval_count);
    record.access_count = Some(telemetry.access_count);
    record.helpful_count = Some(telemetry.helpful_count);
    record.unhelpful_count = Some(telemetry.unhelpful_count);
    record.last_recalled_at = telemetry.last_recalled_at;
    record.still_exists = true;
    let recalled = telemetry.access_count > 0 || telemetry.last_recalled_at.is_some();
    record.verdict = if recalled && telemetry.helpful_count > 0 {
        FactOutcomeVerdict::RecalledAndHelpful
    } else if recalled {
        FactOutcomeVerdict::Recalled
    } else {
        FactOutcomeVerdict::NeverRecalled
    };
    record
}

pub fn automation_outcomes_path(dashboard_root: &Path) -> PathBuf {
    dashboard_root.join(AUTOMATION_OUTCOMES_FILENAME)
}

pub async fn load_outcomes_snapshot(dashboard_root: &Path) -> Result<AutomationOutcomesSnapshot> {
    let path = automation_outcomes_path(dashboard_root);
    let bytes = match tokio::fs::read(&path).await {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(AutomationOutcomesSnapshot::default());
        }
        Err(e) => {
            return Err(config_error(format!(
                "failed to read automation outcomes snapshot '{}': {e}",
                path.display()
            )));
        }
    };
    serde_json::from_slice(&bytes).map_err(|e| {
        config_error(format!(
            "failed to parse automation outcomes snapshot '{}': {e}",
            path.display()
        ))
    })
}

pub async fn save_outcomes_snapshot(
    dashboard_root: &Path,
    snapshot: &AutomationOutcomesSnapshot,
) -> Result<()> {
    let lock = outcomes_snapshot_lock(dashboard_root);
    let _guard = lock.lock().await;
    save_outcomes_snapshot_unlocked(dashboard_root, snapshot).await
}

async fn save_outcomes_snapshot_unlocked(
    dashboard_root: &Path,
    snapshot: &AutomationOutcomesSnapshot,
) -> Result<()> {
    let path = automation_outcomes_path(dashboard_root);
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await.map_err(|e| {
            config_error(format!(
                "failed to create automation outcomes directory '{}': {e}",
                parent.display()
            ))
        })?;
    }
    let bytes = serde_json::to_vec_pretty(snapshot).map_err(TraceDecayError::from)?;
    let nonce = AUTOMATION_OUTCOMES_TEMP_NONCE.fetch_add(1, Ordering::Relaxed);
    let temporary = path.with_file_name(format!(
        ".{AUTOMATION_OUTCOMES_FILENAME}.{}.{}.{}.tmp",
        std::process::id(),
        crate::runtime_identity::process_run_id(),
        nonce
    ));
    crate::db::DatabaseAuthority::publish_record_atomically(
        &temporary,
        &path,
        &bytes,
        "automation outcomes snapshot",
    )
}

fn outcomes_snapshot_lock(dashboard_root: &Path) -> Arc<tokio::sync::Mutex<()>> {
    let key = dashboard_root.to_path_buf();
    let mut locks = AUTOMATION_OUTCOMES_LOCKS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    locks.retain(|_, lock| lock.strong_count() > 0);
    if let Some(lock) = locks.get(&key).and_then(Weak::upgrade) {
        return lock;
    }
    let lock = Arc::new(tokio::sync::Mutex::new(()));
    locks.insert(key, Arc::downgrade(&lock));
    lock
}

/// Recomputes skill outcomes from the managed-skill store plus usage ledger
/// and persists them into the snapshot (facts half untouched).
pub async fn refresh_skill_outcomes(
    profile_root: &Path,
    dashboard_root: &Path,
    now_unix: i64,
) -> Result<Vec<SkillOutcomeRecord>> {
    let skills = list_managed_skills(profile_root).await?;
    let summaries = summarize_skill_usage(profile_root, &skills).await?;
    let outcomes = compute_skill_outcomes(&summaries, now_unix);
    let lock = outcomes_snapshot_lock(dashboard_root);
    let _guard = lock.lock().await;
    let mut snapshot = load_outcomes_snapshot(dashboard_root).await?;
    snapshot.schema_version = 3;
    snapshot.skills = outcomes.clone();
    snapshot.skills_refreshed_at = Some(now_unix);
    save_outcomes_snapshot_unlocked(dashboard_root, &snapshot).await?;
    Ok(outcomes)
}

/// Recomputes fact outcomes from terminal automatic-fact receipts, then
/// persists the current telemetry cache (skills half untouched).
pub async fn refresh_fact_outcomes<A: ProjectMemoryFactStore>(
    dashboard_root: &Path,
    application: &MemoryApplication<A>,
    now_unix: i64,
) -> Result<Vec<FactOutcomeRecord>> {
    let outcomes = compute_fact_outcomes(application, now_unix).await?;
    let lock = outcomes_snapshot_lock(dashboard_root);
    let _guard = lock.lock().await;
    let mut snapshot = load_outcomes_snapshot(dashboard_root).await?;
    snapshot.schema_version = 3;
    snapshot.facts = outcomes.clone();
    snapshot.facts_refreshed_at = Some(now_unix);
    save_outcomes_snapshot_unlocked(dashboard_root, &snapshot).await?;
    Ok(outcomes)
}

pub fn compute_skill_outcomes(
    summaries: &[SkillUsageSummary],
    now_unix: i64,
) -> Vec<SkillOutcomeRecord> {
    summaries
        .iter()
        // Disabled/archived skills were already acted on; their adoption
        // outcome is no longer a pending question.
        .filter(|summary| {
            !matches!(
                summary.state,
                Some(ManagedSkillState::Disabled | ManagedSkillState::Archived)
            )
        })
        .filter_map(|summary| skill_outcome(summary, now_unix))
        .collect()
}

pub async fn compute_fact_outcomes<A: ProjectMemoryFactStore>(
    application: &MemoryApplication<A>,
    now_unix: i64,
) -> Result<Vec<FactOutcomeRecord>> {
    let mut outcomes = Vec::new();
    let mut after_apply_id = None;

    loop {
        let page = application
            .list_project_memory_automatic_fact_receipts(
                None,
                after_apply_id.clone(),
                MAX_PROJECT_MEMORY_AUTOMATIC_FACT_RECEIPTS,
            )
            .await
            .map_err(|error| config_error(format!("list automatic fact receipts: {error}")))?;
        let next_after_apply_id = page.next_after_apply_id().cloned();

        for receipt in page.receipts() {
            let projection = if receipt.state() == ProjectMemoryAutomaticFactStateV1::Applied {
                let canonical_fact_id = receipt.applied_fact_id().ok_or_else(|| {
                    config_error(format!(
                        "applied automatic fact receipt '{}' has no canonical fact id",
                        receipt.apply_id().as_str()
                    ))
                })?;
                let target = ProjectMemoryFactTargetV1::Canonical(
                    ProjectMemoryFactIdV1::new(
                        application.owner().clone(),
                        canonical_fact_id.clone(),
                    )
                    .map_err(|error| {
                        config_error(format!(
                            "invalid canonical fact id for automatic receipt '{}': {error}",
                            receipt.apply_id().as_str()
                        ))
                    })?,
                );
                application
                    .get_project_memory_fact(target)
                    .await
                    .map_err(|error| {
                        config_error(format!(
                            "read applied automatic fact receipt '{}': {error}",
                            receipt.apply_id().as_str()
                        ))
                    })?
            } else {
                None
            };
            outcomes.push(fact_outcome(
                fact_outcome_input(receipt, projection.as_ref())?,
                now_unix,
            ));
        }

        let Some(next_after_apply_id) = next_after_apply_id else {
            break;
        };
        after_apply_id = Some(next_after_apply_id);
    }

    Ok(outcomes)
}

fn fact_outcome_input(
    receipt: &ProjectMemoryAutomaticFactReceiptV1,
    projection: Option<&ProjectMemoryFactProjectionV1>,
) -> Result<FactOutcomeInput> {
    let state = receipt.state();
    let apply_id = receipt.apply_id().as_str().to_owned();
    let run_id = receipt.automation_run_id().map(ToOwned::to_owned);
    let recorded_at = receipt.recorded_at().0.div_euclid(1_000_000);

    match state {
        ProjectMemoryAutomaticFactStateV1::Applied => {
            let canonical_fact_id = receipt.applied_fact_id().ok_or_else(|| {
                config_error(format!(
                    "applied automatic fact receipt '{}' has no canonical fact id",
                    receipt.apply_id().as_str()
                ))
            })?;
            let observation = match projection {
                Some(ProjectMemoryFactProjectionV1::Available(fact)) => {
                    let telemetry = fact.telemetry();
                    FactOutcomeObservation::Available(FactOutcomeTelemetry {
                        retrieval_count: telemetry.retrieval_count(),
                        access_count: telemetry.access_count(),
                        helpful_count: telemetry.helpful_count(),
                        unhelpful_count: telemetry.unhelpful_count(),
                        last_recalled_at: telemetry
                            .last_recalled_at()
                            .map(|timestamp| timestamp.0.div_euclid(1_000_000)),
                    })
                }
                Some(ProjectMemoryFactProjectionV1::Unavailable(unavailable)) => {
                    match unavailable.availability() {
                        ProjectMemoryFactAvailabilityV1::Deleted => FactOutcomeObservation::Deleted,
                        ProjectMemoryFactAvailabilityV1::Quarantined => {
                            FactOutcomeObservation::Quarantined
                        }
                        ProjectMemoryFactAvailabilityV1::Unavailable => {
                            FactOutcomeObservation::Unavailable
                        }
                    }
                }
                None => FactOutcomeObservation::Unavailable,
            };
            Ok(FactOutcomeInput {
                apply_id,
                run_id,
                state,
                canonical_fact_id: Some(canonical_fact_id.as_str().to_owned()),
                recorded_at,
                observation,
            })
        }
        ProjectMemoryAutomaticFactStateV1::Quarantined => Ok(FactOutcomeInput {
            apply_id,
            run_id,
            state,
            canonical_fact_id: None,
            recorded_at,
            observation: FactOutcomeObservation::Quarantined,
        }),
    }
}

/// The outcome records relevant to one automation task: the skill writer is
/// judged by skill adoption, fact-producing tasks by automatic-fact trajectory.
fn task_outcomes(
    task: AgentTaskKind,
    snapshot: &AutomationOutcomesSnapshot,
) -> (Vec<&SkillOutcomeRecord>, Vec<&FactOutcomeRecord>) {
    match task {
        AgentTaskKind::SkillWriter => (snapshot.skills.iter().collect(), Vec::new()),
        AgentTaskKind::CombinedReview => (
            snapshot.skills.iter().collect(),
            snapshot.facts.iter().collect(),
        ),
        AgentTaskKind::SessionReflector | AgentTaskKind::MemoryCurator => {
            (Vec::new(), snapshot.facts.iter().collect())
        }
        AgentTaskKind::UserJob => (Vec::new(), Vec::new()),
    }
}

/// The automatic change outcome section embedded in the `feedback` artifact
/// payload.
pub(super) fn outcome_feedback_section(
    task: AgentTaskKind,
    snapshot: &AutomationOutcomesSnapshot,
) -> Value {
    let (skills, facts) = task_outcomes(task, snapshot);
    let skill_verdicts = verdict_counts(skills.iter().map(|record| record.verdict.as_str()));
    let fact_verdicts = verdict_counts(facts.iter().map(|record| record.verdict.as_str()));
    json!({
        "status": if skills.is_empty() && facts.is_empty() {
            "no_outcomes_recorded"
        } else {
            "available"
        },
        "source": "post_activation_outcome_tracking",
        "skills_refreshed_at": snapshot.skills_refreshed_at,
        "facts_refreshed_at": snapshot.facts_refreshed_at,
        "skill_verdicts": skill_verdicts,
        "fact_verdicts": fact_verdicts,
        "skills": skills,
        "facts": facts,
    })
}

/// Generated-eval entries derived from real post-activation outcomes rather
/// than validation-time signals. Kept separate from the validation-replay
/// definitions so the replay gate keeps checking only validation examples.
pub(super) fn outcome_eval_definitions(
    task: AgentTaskKind,
    task_key: &str,
    snapshot: &AutomationOutcomesSnapshot,
) -> Vec<Value> {
    let (skills, facts) = task_outcomes(task, snapshot);
    let mut definitions = Vec::new();
    for record in skills {
        definitions.push(json!({
            "schema_version": 1,
            "eval_id": format!("{task_key}:outcome:skill:{}", record.skill_id),
            "kind": "applied_change_outcome",
            "subject": { "type": "managed_skill", "skill_id": record.skill_id },
            "observed_outcome": record.verdict.as_str(),
            "expected_outcome": "adopted",
            "passed": record.verdict == SkillOutcomeVerdict::Adopted,
            "pending": record.verdict == SkillOutcomeVerdict::TooEarly,
            "metrics": {
                "activated_at": record.activated_at,
                "days_since_activation": record.days_since_activation,
                "views_since_activation": record.views_since_activation,
                "uses_since_activation": record.uses_since_activation,
            },
            "assertions": [{
                "type": "outcome_equals",
                "expected": "adopted",
                "actual": record.verdict.as_str(),
            }],
        }));
    }
    for record in facts {
        let passed = record.state == ProjectMemoryAutomaticFactStateV1::Applied
            && matches!(
                record.verdict,
                FactOutcomeVerdict::RecalledAndHelpful | FactOutcomeVerdict::Recalled
            );
        definitions.push(json!({
            "schema_version": 1,
            "eval_id": format!("{task_key}:outcome:fact:{}", record.apply_id),
            "kind": "applied_change_outcome",
            "subject": {
                "type": "automatic_fact_receipt",
                "apply_id": record.apply_id,
                "state": record.state,
                "canonical_fact_id": record.canonical_fact_id,
            },
            "observed_outcome": record.verdict.as_str(),
            "expected_outcome": "recalled",
            "passed": passed,
            "pending": false,
            "metrics": {
                "recorded_at": record.recorded_at,
                "days_since_recorded": record.days_since_recorded,
                "retrieval_count": record.retrieval_count,
                "access_count": record.access_count,
                "helpful_count": record.helpful_count,
                "unhelpful_count": record.unhelpful_count,
                "still_exists": record.still_exists,
            },
            "assertions": [{
                "type": "outcome_in",
                "expected": ["recalled", "recalled_and_helpful"],
                "actual": record.verdict.as_str(),
            }],
        }));
    }
    definitions
}

fn verdict_counts<'a>(verdicts: impl Iterator<Item = &'a str>) -> Value {
    let mut counts = serde_json::Map::new();
    for verdict in verdicts {
        let entry = counts.entry(verdict.to_string()).or_insert(json!(0));
        if let Some(count) = entry.as_u64() {
            *entry = json!(count + 1);
        }
    }
    Value::Object(counts)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
#[path = "outcomes_test.rs"]
mod outcomes_test;
