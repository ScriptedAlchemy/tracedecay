//! Surfacing of staged automation output (R5, Hermes parity).
//!
//! Automation runs may stage fact proposals and skill drafts for review.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tracedecay_store::FactCompatibilityStore;

use super::config_error;
use super::managed_skills::{ManagedSkillState, list_managed_skills};
use super::run_ledger::load_run_records;
use crate::application::memory::MemoryApplication;
use crate::errors::{Result, TraceDecayError};

const NOTICE_STATE_FILENAME: &str = "automation_notice_seen.json";

/// Counts of staged automation output.
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

/// Counts pending fact proposals (project authority) and pending
/// managed skills (user profile store). Best-effort: unreadable or missing
/// stores count as zero so callers never fail a request over a notice.
pub async fn count_pending_automation_output<A: FactCompatibilityStore>(
    memory: &MemoryApplication<A>,
    profile_root: &Path,
) -> AutomationPendingCounts {
    let pending_fact_proposals = memory
        .count_pending_compatibility_fact_proposals()
        .await
        .ok()
        .and_then(|count| usize::try_from(count).ok())
        .unwrap_or(0);
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
    if counts.pending_skills > 0 {
        parts.push(format!(
            "{} skill draft{}",
            counts.pending_skills,
            if counts.pending_skills == 1 { "" } else { "s" }
        ));
    }
    Some(format!(
        "TraceDecay automation: {} await{} review — dashboard Memory and Skills tabs.",
        parts.join(" and "),
        if counts.total() == 1 { "s" } else { "" },
    ))
}

/// One-shot check used by the MCP server: derives pending counts, dedupes
/// against the persisted notice state, and returns the notice line to surface
/// (persisting the new state) when a new automation batch awaits review.
pub async fn maybe_automation_staged_notice<A: FactCompatibilityStore>(
    memory: &MemoryApplication<A>,
    dashboard_root: &Path,
    profile_root: &Path,
) -> Option<String> {
    let counts = count_pending_automation_output(memory, profile_root).await;
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

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn counts(facts: usize, skills: usize) -> AutomationPendingCounts {
        AutomationPendingCounts {
            pending_fact_proposals: facts,
            pending_skills: skills,
        }
    }

    #[test]
    fn message_pluralizes_visible_fact_and_skill_reviews() {
        assert_eq!(staged_notice_message(counts(0, 0)), None);
        assert_eq!(
            staged_notice_message(counts(2, 1)).unwrap(),
            "TraceDecay automation: 2 fact proposals and 1 skill draft await review — dashboard Memory and Skills tabs."
        );
        assert_eq!(
            staged_notice_message(counts(1, 0)).unwrap(),
            "TraceDecay automation: 1 fact proposal awaits review — dashboard Memory and Skills tabs."
        );
        assert_eq!(
            staged_notice_message(counts(0, 3)).unwrap(),
            "TraceDecay automation: 3 skill drafts await review — dashboard Memory and Skills tabs."
        );
    }

    #[test]
    fn notify_fires_once_per_batch() {
        // Nothing pending: never notify.
        assert!(!should_notify(None, Some("run-1"), counts(0, 0)));
        // First sighting of a pending batch: notify.
        assert!(should_notify(None, Some("run-1"), counts(2, 1)));
        assert!(should_notify(None, Some("run-1"), counts(2, 0)));
        let seen = AutomationNoticeState {
            last_run_id: Some("run-1".to_string()),
            pending_fact_proposals: 2,
            pending_skills: 1,
        };
        // Same batch again: stay quiet.
        assert!(!should_notify(Some(&seen), Some("run-1"), counts(2, 1)));
        // New run appended: notify again.
        assert!(should_notify(Some(&seen), Some("run-2"), counts(2, 1)));
        // Every pending-review count change rearms the notice.
        assert!(should_notify(Some(&seen), Some("run-1"), counts(3, 1)));
        assert!(should_notify(Some(&seen), Some("run-1"), counts(2, 2)));
    }
}
