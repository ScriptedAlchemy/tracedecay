//! Post-approval outcome tracking for automation-applied changes (R10).
//!
//! The automation loops stage skills and facts that humans approve, but
//! approval alone says nothing about whether the change was good. This module
//! measures what happened *after* approval:
//!
//! - approved managed skills: adoption derived from the usage ledger
//!   (`adopted` / `ignored` / `too_early`),
//! - applied fact proposals: post-apply recall trajectory in the memory store
//!   (`recalled_and_helpful` / `recalled` / `never_recalled` / `deleted`).
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
    CompatibilityFactAvailabilityV1, CompatibilityFactHistoryQueryV1, CompatibilityFactIdV1,
    CompatibilityFactProjectionV1, CompatibilityFactProposalRecordV1,
    CompatibilityFactProposalStateV1, CompatibilityFactTargetV1, FactCompatibilityStore,
};

use super::backend::AgentTaskKind;
use super::config_error;
use super::managed_skills::{ManagedSkillState, list_managed_skills};
use super::skill_usage::{SkillUsageSummary, summarize_skill_usage};
use crate::application::memory::MemoryApplication;
use crate::errors::{Result, TraceDecayError};

const AUTOMATION_OUTCOMES_FILENAME: &str = "automation_outcomes.json";
const FACT_OUTCOME_PAGE_LIMIT: usize = 200;

/// Outcome refreshes update independent halves of one snapshot. This lock
/// serializes their read-modify-write critical sections for one dashboard.
static AUTOMATION_OUTCOMES_LOCKS: LazyLock<Mutex<HashMap<PathBuf, Weak<tokio::sync::Mutex<()>>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static AUTOMATION_OUTCOMES_TEMP_NONCE: AtomicU64 = AtomicU64::new(0);

/// A skill is `too_early` to judge until this long after approval.
pub const SKILL_ADOPTION_WINDOW_SECS: i64 = 7 * 24 * 60 * 60;

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
}

impl FactOutcomeVerdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RecalledAndHelpful => "recalled_and_helpful",
            Self::Recalled => "recalled",
            Self::NeverRecalled => "never_recalled",
            Self::Deleted => "deleted",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SkillOutcomeRecord {
    pub skill_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub approved_at: i64,
    pub days_since_approval: i64,
    pub views_since_approval: u64,
    pub uses_since_approval: u64,
    pub verdict: SkillOutcomeVerdict,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FactOutcomeRecord {
    pub proposal_id: String,
    pub run_id: String,
    /// Canonical fact identity from the authoritative compatibility proposal.
    #[serde(default)]
    pub canonical_fact_id: String,
    /// Legacy numeric mapping when the authority durably recorded one.
    #[serde(default)]
    pub fact_id: Option<i64>,
    pub applied_at: i64,
    pub days_since_applied: i64,
    pub retrieval_count: i64,
    pub access_count: i64,
    pub helpful_count: i64,
    pub unhelpful_count: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_recalled_at: Option<i64>,
    pub still_exists: bool,
    pub verdict: FactOutcomeVerdict,
}

#[derive(Debug, Clone, PartialEq)]
struct FactOutcomeTelemetry {
    retrieval_count: i64,
    access_count: i64,
    helpful_count: i64,
    unhelpful_count: i64,
    last_recalled_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq)]
struct FactOutcomeInput {
    proposal_id: String,
    run_id: String,
    canonical_fact_id: String,
    fact_id: Option<i64>,
    applied_at: i64,
    telemetry: Option<FactOutcomeTelemetry>,
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

/// Computes the adoption verdict for one approved skill. `None` when the
/// skill has never been approved (no post-approval window to measure).
pub fn skill_outcome(summary: &SkillUsageSummary, now_unix: i64) -> Option<SkillOutcomeRecord> {
    let approved_at = summary.approved_at?;
    let secs_since_approval = now_unix.saturating_sub(approved_at);
    let views_since_approval = count_since_approval(
        summary.view_count,
        summary.view_count_at_approval,
        summary.last_viewed_at,
        approved_at,
    );
    let uses_since_approval = count_since_approval(
        summary.use_count,
        summary.use_count_at_approval,
        summary.last_used_at,
        approved_at,
    );
    let verdict = if uses_since_approval > 0 {
        SkillOutcomeVerdict::Adopted
    } else if secs_since_approval < SKILL_ADOPTION_WINDOW_SECS {
        SkillOutcomeVerdict::TooEarly
    } else {
        SkillOutcomeVerdict::Ignored
    };
    Some(SkillOutcomeRecord {
        skill_id: summary.skill_id.clone(),
        title: summary.title.clone(),
        approved_at,
        days_since_approval: secs_since_approval / SECS_PER_DAY,
        views_since_approval,
        uses_since_approval,
        verdict,
    })
}

/// Activity since approval, preferring the exact baseline captured at
/// approval time. Ledgers written before baselines existed fall back to the
/// last-activity timestamp: activity at or after approval counts the full
/// total (a conservative over-count is fine for adoption detection).
fn count_since_approval(
    total: u64,
    baseline_at_approval: Option<u64>,
    last_activity_at: Option<i64>,
    approved_at: i64,
) -> u64 {
    match baseline_at_approval {
        Some(baseline) => total.saturating_sub(baseline),
        None if last_activity_at.is_some_and(|at| at >= approved_at) => total,
        None => 0,
    }
}

/// Computes the post-apply verdict from a compatibility-authority projection.
fn fact_outcome(input: FactOutcomeInput, now_unix: i64) -> FactOutcomeRecord {
    let applied_at = input.applied_at;
    let mut record = FactOutcomeRecord {
        proposal_id: input.proposal_id,
        run_id: input.run_id,
        canonical_fact_id: input.canonical_fact_id,
        fact_id: input.fact_id,
        applied_at,
        days_since_applied: now_unix.saturating_sub(applied_at) / SECS_PER_DAY,
        retrieval_count: 0,
        access_count: 0,
        helpful_count: 0,
        unhelpful_count: 0,
        last_recalled_at: None,
        still_exists: false,
        verdict: FactOutcomeVerdict::Deleted,
    };
    let Some(telemetry) = input.telemetry else {
        return record;
    };
    record.retrieval_count = telemetry.retrieval_count;
    record.access_count = telemetry.access_count;
    record.helpful_count = telemetry.helpful_count;
    record.unhelpful_count = telemetry.unhelpful_count;
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
    snapshot.schema_version = 1;
    snapshot.skills = outcomes.clone();
    snapshot.skills_refreshed_at = Some(now_unix);
    save_outcomes_snapshot_unlocked(dashboard_root, &snapshot).await?;
    Ok(outcomes)
}

/// Recomputes fact outcomes after authoritative compatibility reads, then
/// persists the derived sidecar snapshot (skills half untouched).
pub async fn refresh_fact_outcomes<A: FactCompatibilityStore>(
    dashboard_root: &Path,
    application: &MemoryApplication<A>,
    now_unix: i64,
) -> Result<Vec<FactOutcomeRecord>> {
    let outcomes = compute_fact_outcomes(application, now_unix).await?;
    let lock = outcomes_snapshot_lock(dashboard_root);
    let _guard = lock.lock().await;
    let mut snapshot = load_outcomes_snapshot(dashboard_root).await?;
    snapshot.schema_version = 1;
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

pub async fn compute_fact_outcomes<A: FactCompatibilityStore>(
    application: &MemoryApplication<A>,
    now_unix: i64,
) -> Result<Vec<FactOutcomeRecord>> {
    let mut outcomes = Vec::new();
    let mut after_proposal_id = None;

    loop {
        let page = application
            .list_compatibility_fact_proposals(
                Some(CompatibilityFactProposalStateV1::Applied),
                after_proposal_id.clone(),
                FACT_OUTCOME_PAGE_LIMIT,
            )
            .await
            .map_err(|error| config_error(format!("list applied fact proposals: {error}")))?;
        let next_after_proposal_id = page.next_after_proposal_id().cloned();

        for proposal in page.proposals() {
            let canonical_fact_id = proposal.applied_fact_id().ok_or_else(|| {
                config_error(format!(
                    "applied compatibility fact proposal '{}' has no canonical fact id",
                    proposal.proposal_id().as_str()
                ))
            })?;
            let target = CompatibilityFactTargetV1::Canonical(
                CompatibilityFactIdV1::new(proposal.owner().clone(), canonical_fact_id.clone())
                    .map_err(|error| {
                        config_error(format!(
                            "invalid canonical fact id for proposal '{}': {error}",
                            proposal.proposal_id().as_str()
                        ))
                    })?,
            );
            let projection = application
                .get_compatibility_fact(target.clone())
                .await
                .map_err(|error| {
                    config_error(format!(
                        "read applied fact proposal '{}': {error}",
                        proposal.proposal_id().as_str()
                    ))
                })?;
            let applied_at = applied_at_from_lineage(application, &target, proposal).await?;
            if let Some(input) = fact_outcome_input(proposal, projection.as_ref(), applied_at)? {
                outcomes.push(fact_outcome(input, now_unix));
            }
        }

        let Some(next_after_proposal_id) = next_after_proposal_id else {
            break;
        };
        after_proposal_id = Some(next_after_proposal_id);
    }

    Ok(outcomes)
}

fn fact_outcome_input(
    proposal: &CompatibilityFactProposalRecordV1,
    projection: Option<&CompatibilityFactProjectionV1>,
    applied_at: i64,
) -> Result<Option<FactOutcomeInput>> {
    if proposal.state() != CompatibilityFactProposalStateV1::Applied {
        return Err(config_error(format!(
            "fact outcome requested for non-applied proposal '{}'",
            proposal.proposal_id().as_str()
        )));
    }
    let canonical_fact_id = proposal.applied_fact_id().ok_or_else(|| {
        config_error(format!(
            "applied compatibility fact proposal '{}' has no canonical fact id",
            proposal.proposal_id().as_str()
        ))
    })?;
    let telemetry = match projection {
        Some(CompatibilityFactProjectionV1::Available(fact)) => {
            let telemetry = fact.telemetry();
            Some(FactOutcomeTelemetry {
                retrieval_count: outcome_count(telemetry.retrieval_count(), "retrieval count")?,
                access_count: outcome_count(telemetry.access_count(), "access count")?,
                helpful_count: outcome_count(telemetry.helpful_count(), "helpful count")?,
                unhelpful_count: outcome_count(telemetry.unhelpful_count(), "unhelpful count")?,
                last_recalled_at: telemetry
                    .last_recalled_at()
                    .map(|timestamp| timestamp.0 / 1_000_000),
            })
        }
        Some(CompatibilityFactProjectionV1::Unavailable(unavailable)) => {
            match unavailable.availability() {
                CompatibilityFactAvailabilityV1::Deleted => None,
                CompatibilityFactAvailabilityV1::Quarantined
                | CompatibilityFactAvailabilityV1::Unavailable => return Ok(None),
            }
        }
        None => {
            return Err(config_error(format!(
                "applied compatibility fact proposal '{}' has no current projection",
                proposal.proposal_id().as_str()
            )));
        }
    };

    Ok(Some(FactOutcomeInput {
        proposal_id: proposal.proposal_id().as_str().to_owned(),
        run_id: proposal
            .automation_run_id()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| proposal.request().operation_id().as_str())
            .to_owned(),
        canonical_fact_id: canonical_fact_id.as_str().to_owned(),
        fact_id: proposal.legacy_fact_id(),
        applied_at,
        telemetry,
    }))
}

/// The compatibility promotion batch starts its immutable lineage at the
/// promotion timestamp, which remains available after payload deletion.
async fn applied_at_from_lineage<A: FactCompatibilityStore>(
    application: &MemoryApplication<A>,
    target: &CompatibilityFactTargetV1,
    proposal: &CompatibilityFactProposalRecordV1,
) -> Result<i64> {
    let query = CompatibilityFactHistoryQueryV1::new(target.clone(), None, 1).map_err(|error| {
        config_error(format!(
            "build outcome lineage query for proposal '{}': {error}",
            proposal.proposal_id().as_str()
        ))
    })?;
    let history = application
        .get_compatibility_history(query)
        .await
        .map_err(|error| {
            config_error(format!(
                "read outcome lineage for proposal '{}': {error}",
                proposal.proposal_id().as_str()
            ))
        })?;
    let event = history.events().first().ok_or_else(|| {
        config_error(format!(
            "applied compatibility fact proposal '{}' has no lineage",
            proposal.proposal_id().as_str()
        ))
    })?;
    Ok(event.occurred_at().0 / 1_000_000)
}

fn outcome_count(value: u64, field: &str) -> Result<i64> {
    i64::try_from(value).map_err(|_| {
        config_error(format!(
            "compatibility fact {field} exceeds the legacy outcome range"
        ))
    })
}

/// The outcome records relevant to one automation task: the skill writer is
/// judged by skill adoption, fact-producing tasks by fact recall.
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

/// The "outcomes of previously applied changes" section embedded in the
/// `feedback` artifact payload.
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
        "source": "post_approval_outcome_tracking",
        "skills_refreshed_at": snapshot.skills_refreshed_at,
        "facts_refreshed_at": snapshot.facts_refreshed_at,
        "skill_verdicts": skill_verdicts,
        "fact_verdicts": fact_verdicts,
        "skills": skills,
        "facts": facts,
    })
}

/// Generated-eval entries derived from real post-approval outcomes rather
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
                "approved_at": record.approved_at,
                "days_since_approval": record.days_since_approval,
                "views_since_approval": record.views_since_approval,
                "uses_since_approval": record.uses_since_approval,
            },
            "assertions": [{
                "type": "outcome_equals",
                "expected": "adopted",
                "actual": record.verdict.as_str(),
            }],
        }));
    }
    for record in facts {
        let passed = matches!(
            record.verdict,
            FactOutcomeVerdict::RecalledAndHelpful | FactOutcomeVerdict::Recalled
        );
        definitions.push(json!({
            "schema_version": 1,
            "eval_id": format!("{task_key}:outcome:fact:{}", record.proposal_id),
            "kind": "applied_change_outcome",
            "subject": {
                "type": "applied_fact",
                "proposal_id": record.proposal_id,
                "canonical_fact_id": record.canonical_fact_id,
                "fact_id": record.fact_id,
            },
            "observed_outcome": record.verdict.as_str(),
            "expected_outcome": "recalled",
            "passed": passed,
            "pending": false,
            "metrics": {
                "applied_at": record.applied_at,
                "days_since_applied": record.days_since_applied,
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
mod tests {
    use super::super::skill_usage::SkillUsageRecord;
    use super::*;

    static OUTCOME_PERSISTENCE_DB_TEST_LOCK: tokio::sync::Mutex<()> =
        tokio::sync::Mutex::const_new(());

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
        use crate::automation::fact_proposals::{
            apply_fact_proposal, record_session_fact_proposals,
        };
        use crate::db::{Database, DatabaseAuthority};
        use crate::store::memory::DatabaseFactStore;
        use tracedecay_domain::FactOwnerV1;

        let authority =
            DatabaseAuthority::acquire_test(database_path, "outcome persistence test").unwrap();
        let (database, _) = Database::initialize(database_path, &authority)
            .await
            .unwrap();
        let memory =
            MemoryApplication::new(FactOwnerV1::Profile, DatabaseFactStore::new(&database))
                .unwrap();
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
            MemoryApplication::new(FactOwnerV1::Profile, DatabaseFactStore::new(&database))
                .unwrap();
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
            MemoryApplication::new(FactOwnerV1::Profile, DatabaseFactStore::new(&database))
                .unwrap();
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
}
