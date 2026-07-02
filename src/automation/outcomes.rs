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

use std::path::{Path, PathBuf};

use libsql::Connection;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::backend::AgentTaskKind;
use super::fact_proposals::{load_fact_proposal_store, FactProposalRecord, FactProposalState};
use super::managed_skills::{list_managed_skills, ManagedSkillState};
use super::skill_usage::{summarize_skill_usage, SkillUsageSummary};
use crate::errors::{Result, TraceDecayError};
use crate::memory::store::MemoryStore;
use crate::memory::types::FactRecord;

const AUTOMATION_OUTCOMES_FILENAME: &str = "automation_outcomes.json";

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
    pub fact_id: i64,
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

/// Computes the post-apply verdict for one applied fact proposal. `None`
/// when the proposal never produced a stored fact.
pub fn fact_outcome(
    proposal: &FactProposalRecord,
    fact: Option<&FactRecord>,
    now_unix: i64,
) -> Option<FactOutcomeRecord> {
    if proposal.state != FactProposalState::Applied {
        return None;
    }
    let fact_id = proposal.applied_fact_id?;
    let applied_at = proposal.updated_at;
    let mut record = FactOutcomeRecord {
        proposal_id: proposal.proposal_id.clone(),
        run_id: proposal.run_id.clone(),
        fact_id,
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
    let Some(fact) = fact else {
        return Some(record);
    };
    record.retrieval_count = fact.retrieval_count;
    record.access_count = fact.access_count;
    record.helpful_count = fact.helpful_count;
    record.unhelpful_count = fact.unhelpful_count;
    record.last_recalled_at = fact.last_recalled_at;
    record.still_exists = true;
    let recalled = fact.access_count > 0 || fact.last_recalled_at.is_some();
    record.verdict = if recalled && fact.helpful_count > 0 {
        FactOutcomeVerdict::RecalledAndHelpful
    } else if recalled {
        FactOutcomeVerdict::Recalled
    } else {
        FactOutcomeVerdict::NeverRecalled
    };
    Some(record)
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
    tokio::fs::write(&path, bytes).await.map_err(|e| {
        config_error(format!(
            "failed to write automation outcomes snapshot '{}': {e}",
            path.display()
        ))
    })
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
    let mut snapshot = load_outcomes_snapshot(dashboard_root)
        .await
        .unwrap_or_default();
    snapshot.schema_version = 1;
    snapshot.skills = outcomes.clone();
    snapshot.skills_refreshed_at = Some(now_unix);
    save_outcomes_snapshot(dashboard_root, &snapshot).await?;
    Ok(outcomes)
}

/// Recomputes fact outcomes for applied fact proposals against the memory
/// store and persists them into the snapshot (skills half untouched).
pub async fn refresh_fact_outcomes(
    dashboard_root: &Path,
    conn: &Connection,
    now_unix: i64,
) -> Result<Vec<FactOutcomeRecord>> {
    let proposals = load_fact_proposal_store(dashboard_root).await?.proposals;
    let outcomes = compute_fact_outcomes(&proposals, conn, now_unix).await?;
    let mut snapshot = load_outcomes_snapshot(dashboard_root)
        .await
        .unwrap_or_default();
    snapshot.schema_version = 1;
    snapshot.facts = outcomes.clone();
    snapshot.facts_refreshed_at = Some(now_unix);
    save_outcomes_snapshot(dashboard_root, &snapshot).await?;
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

pub async fn compute_fact_outcomes(
    proposals: &[FactProposalRecord],
    conn: &Connection,
    now_unix: i64,
) -> Result<Vec<FactOutcomeRecord>> {
    let store = MemoryStore::new(conn);
    let mut outcomes = Vec::new();
    for proposal in proposals {
        if proposal.state != FactProposalState::Applied {
            continue;
        }
        let Some(fact_id) = proposal.applied_fact_id else {
            continue;
        };
        let fact = store.get_fact(fact_id).await?;
        if let Some(outcome) = fact_outcome(proposal, fact.as_ref(), now_unix) {
            outcomes.push(outcome);
        }
    }
    Ok(outcomes)
}

/// The outcome records relevant to one automation task: the skill writer is
/// judged by skill adoption, fact-producing tasks by fact recall.
fn task_outcomes(
    task: AgentTaskKind,
    snapshot: &AutomationOutcomesSnapshot,
) -> (Vec<&SkillOutcomeRecord>, Vec<&FactOutcomeRecord>) {
    match task {
        AgentTaskKind::SkillWriter => (snapshot.skills.iter().collect(), Vec::new()),
        AgentTaskKind::SessionReflector | AgentTaskKind::MemoryCurator => {
            (Vec::new(), snapshot.facts.iter().collect())
        }
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

fn config_error(message: String) -> TraceDecayError {
    TraceDecayError::Config { message }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::super::skill_usage::SkillUsageRecord;
    use super::*;
    use crate::memory::types::MemoryCategory;

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

    fn applied_proposal(proposal_id: &str, fact_id: i64, applied_at: i64) -> FactProposalRecord {
        FactProposalRecord {
            schema_version: 1,
            proposal_id: proposal_id.to_string(),
            run_id: "run_outcomes".to_string(),
            evidence_hash: None,
            state: FactProposalState::Applied,
            add_fact_request: None,
            proposal: None,
            validation_reason: None,
            validation: None,
            reviewer: Some("dashboard".to_string()),
            applied_fact_id: Some(fact_id),
            apply_outcome: None,
            created_at: applied_at,
            updated_at: applied_at,
        }
    }

    fn fact(fact_id: i64) -> FactRecord {
        FactRecord {
            fact_id,
            content: "prefers nextest for rust test runs".to_string(),
            category: MemoryCategory::Tool,
            tags: Vec::new(),
            entities: Vec::new(),
            trust_score: 0.6,
            source: None,
            retrieval_count: 0,
            access_count: 0,
            helpful_count: 0,
            unhelpful_count: 0,
            created_at: 0,
            updated_at: 0,
            last_retrieved_at: None,
            last_recalled_at: None,
            last_feedback_at: None,
            metadata: json!({}),
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
        let proposal = applied_proposal("fact_dead", 42, 5 * DAY);
        let outcome = fact_outcome(&proposal, None, 9 * DAY).unwrap();
        assert_eq!(outcome.verdict, FactOutcomeVerdict::Deleted);
        assert!(!outcome.still_exists);
        assert_eq!(outcome.days_since_applied, 4);
    }

    #[test]
    fn never_recalled_fact_yields_never_recalled_verdict() {
        let proposal = applied_proposal("fact_idle", 42, 5 * DAY);
        let outcome = fact_outcome(&proposal, Some(&fact(42)), 9 * DAY).unwrap();
        assert_eq!(outcome.verdict, FactOutcomeVerdict::NeverRecalled);
        assert!(outcome.still_exists);
    }

    #[test]
    fn recalled_fact_yields_recalled_verdict() {
        let proposal = applied_proposal("fact_recalled", 42, 5 * DAY);
        let mut record = fact(42);
        record.access_count = 3;
        record.last_recalled_at = Some(8 * DAY);
        let outcome = fact_outcome(&proposal, Some(&record), 9 * DAY).unwrap();
        assert_eq!(outcome.verdict, FactOutcomeVerdict::Recalled);
        assert_eq!(outcome.access_count, 3);
    }

    #[test]
    fn recalled_and_helpful_fact_yields_top_verdict() {
        let proposal = applied_proposal("fact_helpful", 42, 5 * DAY);
        let mut record = fact(42);
        record.access_count = 2;
        record.helpful_count = 1;
        let outcome = fact_outcome(&proposal, Some(&record), 9 * DAY).unwrap();
        assert_eq!(outcome.verdict, FactOutcomeVerdict::RecalledAndHelpful);
    }

    #[test]
    fn helpful_feedback_without_recall_is_not_recalled_and_helpful() {
        let proposal = applied_proposal("fact_feedback_only", 42, 5 * DAY);
        let mut record = fact(42);
        record.helpful_count = 1;
        let outcome = fact_outcome(&proposal, Some(&record), 9 * DAY).unwrap();
        assert_eq!(outcome.verdict, FactOutcomeVerdict::NeverRecalled);
    }

    #[test]
    fn pending_and_rejected_proposals_produce_no_outcome() {
        let mut proposal = applied_proposal("fact_pending", 42, 5 * DAY);
        proposal.state = FactProposalState::PendingApproval;
        assert!(fact_outcome(&proposal, None, 9 * DAY).is_none());

        let mut proposal = applied_proposal("fact_no_id", 42, 5 * DAY);
        proposal.applied_fact_id = None;
        assert!(fact_outcome(&proposal, None, 9 * DAY).is_none());
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
                &applied_proposal("fact_dead", 42, 5 * DAY),
                None,
                20 * DAY,
            )
            .unwrap()],
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
            approve_managed_skill, create_managed_skill_draft, default_managed_skill_targets,
            ManagedSkillDraft, ManagedSkillProvenance, ManagedSkillSource,
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
}
