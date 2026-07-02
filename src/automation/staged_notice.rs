//! Surfacing of staged automation output (R5, Hermes parity).
//!
//! Automation runs stage skill drafts and fact proposals for human review, but
//! historically the approval queue was only visible in the dashboard or the
//! run ledger. This module derives cheap pending-review counts and decides —
//! with a persisted dedupe state — when a compact one-line notice should be
//! surfaced to the user (via the MCP server's response nudge path and the
//! daemon's `event=automation_staged` log line).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::fact_proposals::{load_fact_proposal_store, FactProposalState};
use super::managed_skills::{list_managed_skills, ManagedSkillState};
use super::run_ledger::load_run_records;
use crate::errors::{Result, TraceDecayError};

const NOTICE_STATE_FILENAME: &str = "automation_notice_seen.json";

/// Counts of automation output awaiting human review.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AutomationPendingCounts {
    /// Fact proposals in `pending_approval` state.
    pub pending_fact_proposals: usize,
    /// Managed skills awaiting review: drafts in `pending_approval` state
    /// plus active skills carrying a staged `pending_update`.
    pub pending_skills: usize,
}

impl AutomationPendingCounts {
    pub fn total(self) -> usize {
        self.pending_fact_proposals + self.pending_skills
    }
}

/// Persisted marker of the last batch we notified about, so a notice fires at
/// most once per new batch (new run id or changed pending counts).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutomationNoticeState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run_id: Option<String>,
    #[serde(default)]
    pub pending_fact_proposals: usize,
    #[serde(default)]
    pub pending_skills: usize,
}

pub fn notice_state_path(dashboard_root: &Path) -> PathBuf {
    dashboard_root.join(NOTICE_STATE_FILENAME)
}

pub async fn load_notice_state(dashboard_root: &Path) -> Option<AutomationNoticeState> {
    let bytes = tokio::fs::read(notice_state_path(dashboard_root))
        .await
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}

pub async fn save_notice_state(dashboard_root: &Path, state: &AutomationNoticeState) -> Result<()> {
    let path = notice_state_path(dashboard_root);
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await.map_err(|e| {
            config_error(format!("failed to create automation notice directory: {e}"))
        })?;
    }
    let bytes = serde_json::to_vec_pretty(state).map_err(TraceDecayError::from)?;
    tokio::fs::write(&path, bytes)
        .await
        .map_err(|e| config_error(format!("failed to write automation notice state: {e}")))
}

/// Counts pending fact proposals (project dashboard store) and pending
/// managed skills (user profile store). Best-effort: unreadable or missing
/// stores count as zero so callers never fail a request over a notice.
pub async fn count_pending_automation_output(
    dashboard_root: &Path,
    profile_root: &Path,
) -> AutomationPendingCounts {
    let pending_fact_proposals =
        load_fact_proposal_store(dashboard_root)
            .await
            .map_or(0, |store| {
                store
                    .proposals
                    .iter()
                    .filter(|proposal| proposal.state == FactProposalState::PendingApproval)
                    .count()
            });
    let pending_skills = list_managed_skills(profile_root).await.map_or(0, |skills| {
        skills
            .iter()
            .filter(|skill| {
                skill.metadata.state == ManagedSkillState::PendingApproval
                    || skill.pending_update.is_some()
            })
            .count()
    });
    AutomationPendingCounts {
        pending_fact_proposals,
        pending_skills,
    }
}

/// Decides whether a notice should fire for the current pending batch.
/// Fires only when something is pending AND the batch differs from what was
/// last notified (different latest run id or different pending counts).
pub fn should_notify(
    previous: Option<&AutomationNoticeState>,
    latest_run_id: Option<&str>,
    counts: AutomationPendingCounts,
) -> bool {
    if counts.total() == 0 {
        return false;
    }
    match previous {
        None => true,
        Some(state) => {
            state.last_run_id.as_deref() != latest_run_id
                || state.pending_fact_proposals != counts.pending_fact_proposals
                || state.pending_skills != counts.pending_skills
        }
    }
}

/// Formats the compact one-line notice, or `None` when nothing is pending.
pub fn staged_notice_message(counts: AutomationPendingCounts) -> Option<String> {
    if counts.total() == 0 {
        return None;
    }
    let mut parts = Vec::new();
    if counts.pending_skills > 0 {
        parts.push(format!(
            "{} skill draft{}",
            counts.pending_skills,
            if counts.pending_skills == 1 { "" } else { "s" }
        ));
    }
    if counts.pending_fact_proposals > 0 {
        parts.push(format!(
            "{} fact proposal{}",
            counts.pending_fact_proposals,
            if counts.pending_fact_proposals == 1 {
                ""
            } else {
                "s"
            }
        ));
    }
    Some(format!(
        "TraceDecay automation: {} await{} review — dashboard Curation tab or tracedecay_fact_store.",
        parts.join(" and "),
        if counts.total() == 1 { "s" } else { "" },
    ))
}

/// One-shot check used by the MCP server: derives pending counts, dedupes
/// against the persisted notice state, and returns the notice line to surface
/// (persisting the new state) when a new batch awaits review.
pub async fn maybe_automation_staged_notice(
    dashboard_root: &Path,
    profile_root: &Path,
) -> Option<String> {
    let counts = count_pending_automation_output(dashboard_root, profile_root).await;
    if counts.total() == 0 {
        return None;
    }
    let latest_run_id = load_run_records(dashboard_root, 1)
        .await
        .ok()
        .and_then(|records| records.into_iter().next())
        .map(|record| record.run_id);
    let previous = load_notice_state(dashboard_root).await;
    if !should_notify(previous.as_ref(), latest_run_id.as_deref(), counts) {
        return None;
    }
    let message = staged_notice_message(counts)?;
    let state = AutomationNoticeState {
        last_run_id: latest_run_id,
        pending_fact_proposals: counts.pending_fact_proposals,
        pending_skills: counts.pending_skills,
    };
    // Best-effort persistence: a failed write only risks a repeat notice.
    let _ = save_notice_state(dashboard_root, &state).await;
    Some(message)
}

fn config_error(message: String) -> TraceDecayError {
    TraceDecayError::Config { message }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::automation::fact_proposals::{
        save_fact_proposal_store, FactProposalRecord, FactProposalStore,
    };
    use crate::automation::managed_skill_model::{
        ManagedSkill, ManagedSkillMetadata, ManagedSkillProvenance, ManagedSkillSource,
    };
    use crate::automation::managed_skills::save_managed_skill;

    fn counts(facts: usize, skills: usize) -> AutomationPendingCounts {
        AutomationPendingCounts {
            pending_fact_proposals: facts,
            pending_skills: skills,
        }
    }

    fn proposal(id: &str, state: FactProposalState) -> FactProposalRecord {
        FactProposalRecord {
            schema_version: 1,
            proposal_id: id.to_string(),
            run_id: "run-1".to_string(),
            evidence_hash: None,
            state,
            add_fact_request: None,
            proposal: None,
            validation_reason: None,
            validation: None,
            reviewer: None,
            applied_fact_id: None,
            apply_outcome: None,
            created_at: 1,
            updated_at: 1,
        }
    }

    fn skill(id: &str, state: ManagedSkillState) -> ManagedSkill {
        let mut skill = ManagedSkill {
            metadata: ManagedSkillMetadata {
                id: id.to_string(),
                title: "Test skill".to_string(),
                summary: "A test skill.".to_string(),
                category: "testing".to_string(),
                targets: crate::automation::managed_skills::default_managed_skill_targets(),
                state,
                pinned: false,
                checksum: String::new(),
                created_at: 1,
                updated_at: 1,
                provenance: ManagedSkillProvenance {
                    source: ManagedSkillSource::AutomationRun,
                    actor: "test".to_string(),
                    run_id: Some("run-1".to_string()),
                },
            },
            body_markdown: "# Test skill\n\nBody.".to_string(),
            support_files: Vec::new(),
            pending_update: None,
        };
        skill.refresh_checksum();
        skill
    }

    #[test]
    fn message_pluralizes_and_skips_empty_parts() {
        assert_eq!(staged_notice_message(counts(0, 0)), None);
        assert_eq!(
            staged_notice_message(counts(2, 1)).unwrap(),
            "TraceDecay automation: 1 skill draft and 2 fact proposals await review — dashboard Curation tab or tracedecay_fact_store."
        );
        assert_eq!(
            staged_notice_message(counts(1, 0)).unwrap(),
            "TraceDecay automation: 1 fact proposal awaits review — dashboard Curation tab or tracedecay_fact_store."
        );
        assert_eq!(
            staged_notice_message(counts(0, 3)).unwrap(),
            "TraceDecay automation: 3 skill drafts await review — dashboard Curation tab or tracedecay_fact_store."
        );
    }

    #[test]
    fn notify_fires_once_per_batch() {
        // Nothing pending: never notify.
        assert!(!should_notify(None, Some("run-1"), counts(0, 0)));
        // First sighting of a pending batch: notify.
        assert!(should_notify(None, Some("run-1"), counts(2, 1)));
        let seen = AutomationNoticeState {
            last_run_id: Some("run-1".to_string()),
            pending_fact_proposals: 2,
            pending_skills: 1,
        };
        // Same batch again: stay quiet.
        assert!(!should_notify(Some(&seen), Some("run-1"), counts(2, 1)));
        // New run appended: notify again.
        assert!(should_notify(Some(&seen), Some("run-2"), counts(2, 1)));
        // Same run but pending counts moved (e.g. a second batch staged
        // before any run-ledger append was observed): notify.
        assert!(should_notify(Some(&seen), Some("run-1"), counts(3, 1)));
    }

    #[tokio::test]
    async fn counts_pending_output_across_stores() {
        let dir = tempfile::tempdir().unwrap();
        let dashboard_root = dir.path().join("dashboard");
        let profile_root = dir.path().join("profile");

        // Empty stores count as zero rather than erroring.
        let empty = count_pending_automation_output(&dashboard_root, &profile_root).await;
        assert_eq!(empty, counts(0, 0));

        save_fact_proposal_store(
            &dashboard_root,
            &FactProposalStore {
                schema_version: 1,
                proposals: vec![
                    proposal("p1", FactProposalState::PendingApproval),
                    proposal("p2", FactProposalState::PendingApproval),
                    proposal("p3", FactProposalState::Applied),
                ],
            },
        )
        .await
        .unwrap();
        save_managed_skill(
            &profile_root,
            &skill("draft-skill", ManagedSkillState::PendingApproval),
        )
        .await
        .unwrap();
        save_managed_skill(
            &profile_root,
            &skill("active-skill", ManagedSkillState::Active),
        )
        .await
        .unwrap();

        let counted = count_pending_automation_output(&dashboard_root, &profile_root).await;
        assert_eq!(counted, counts(2, 1));
    }

    #[tokio::test]
    async fn notice_fires_once_then_rearms_on_new_batch() {
        let dir = tempfile::tempdir().unwrap();
        let dashboard_root = dir.path().join("dashboard");
        let profile_root = dir.path().join("profile");

        save_fact_proposal_store(
            &dashboard_root,
            &FactProposalStore {
                schema_version: 1,
                proposals: vec![proposal("p1", FactProposalState::PendingApproval)],
            },
        )
        .await
        .unwrap();

        let first = maybe_automation_staged_notice(&dashboard_root, &profile_root).await;
        assert_eq!(
            first.unwrap(),
            "TraceDecay automation: 1 fact proposal awaits review — dashboard Curation tab or tracedecay_fact_store."
        );
        // Same batch: deduped.
        assert!(
            maybe_automation_staged_notice(&dashboard_root, &profile_root)
                .await
                .is_none()
        );

        // A second proposal lands: new batch, notice fires again.
        save_fact_proposal_store(
            &dashboard_root,
            &FactProposalStore {
                schema_version: 1,
                proposals: vec![
                    proposal("p1", FactProposalState::PendingApproval),
                    proposal("p2", FactProposalState::PendingApproval),
                ],
            },
        )
        .await
        .unwrap();
        let second = maybe_automation_staged_notice(&dashboard_root, &profile_root).await;
        assert!(second.unwrap().contains("2 fact proposals"));
    }
}
