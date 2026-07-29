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

/// One review queue's reading: a real count, or the reason it could not be
/// read.
///
/// The two variants must stay inseparable. These queues are the only signal
/// telling a human that agent-proposed facts and skill drafts are waiting for
/// approval, so a failed read reported as `0` says "nothing needs your review"
/// and silently suppresses the approval step of the automation pipeline. There
/// is deliberately no `Default`: a caller that cannot read a queue has to name
/// the reason rather than fall back to a zero.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PendingReviewCount {
    /// The queue was read; it holds this many items awaiting review.
    Counted(usize),
    /// The queue could not be read, for the stated reason.
    Unreadable(String),
}

impl PendingReviewCount {
    pub fn unreadable(reason: impl Into<String>) -> Self {
        Self::Unreadable(reason.into())
    }

    /// The count, or `None` when the queue was never successfully read.
    pub fn count(&self) -> Option<usize> {
        match self {
            Self::Counted(count) => Some(*count),
            Self::Unreadable(_) => None,
        }
    }

    pub fn reason(&self) -> Option<&str> {
        match self {
            Self::Counted(_) => None,
            Self::Unreadable(reason) => Some(reason),
        }
    }

    pub fn is_unreadable(&self) -> bool {
        matches!(self, Self::Unreadable(_))
    }
}

/// Human labels for the two queues, used wherever a reader is told which one
/// could not be read.
pub const FACT_PROPOSAL_QUEUE: &str = "fact-proposal";
pub const SKILL_DRAFT_QUEUE: &str = "skill-draft";

/// Readings of staged automation output. Each queue is read independently, so
/// one unavailable authority never suppresses the other's real count.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutomationPendingCounts {
    /// Fact proposals in `pending_approval` state.
    pub fact_proposals: PendingReviewCount,
    /// Managed skills awaiting review: drafts in `pending_approval` state
    /// plus active skills carrying a staged `pending_update`.
    pub skills: PendingReviewCount,
}

impl AutomationPendingCounts {
    /// Items actually counted as awaiting review. A queue that could not be
    /// read contributes nothing here and is reported by [`Self::unreadable`]
    /// instead — it is never summed in as a zero.
    pub fn counted_total(&self) -> usize {
        self.fact_proposals.count().unwrap_or(0) + self.skills.count().unwrap_or(0)
    }

    /// The queues that could not be read, as `(queue label, reason)`.
    pub fn unreadable(&self) -> Vec<(&'static str, &str)> {
        [
            (FACT_PROPOSAL_QUEUE, &self.fact_proposals),
            (SKILL_DRAFT_QUEUE, &self.skills),
        ]
        .into_iter()
        .filter_map(|(label, count)| count.reason().map(|reason| (label, reason)))
        .collect()
    }

    pub fn has_unreadable(&self) -> bool {
        self.fact_proposals.is_unreadable() || self.skills.is_unreadable()
    }

    /// Every queue was read and every one is empty — the only state that may
    /// be presented as "nothing awaits review".
    pub fn is_verified_empty(&self) -> bool {
        !self.has_unreadable() && self.counted_total() == 0
    }
}

/// Persisted marker of the last batch we notified about, so a notice fires at
/// most once per new batch (new run id or changed pending counts).
///
/// The two counts are `Option` because "we could not read this queue" is a
/// distinct batch from "this queue holds nothing", and a notice must fire
/// again when a queue crosses between them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutomationNoticeState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run_id: Option<String>,
    #[serde(default)]
    pub pending_fact_proposals: Option<usize>,
    #[serde(default)]
    pub pending_skills: Option<usize>,
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

/// Pending fact proposals held by the project authority.
///
/// A read failure yields [`PendingReviewCount::Unreadable`], never a zero: the
/// caller may still serve its request, but it has to say the queue is unknown
/// rather than report an empty one.
pub async fn count_pending_fact_proposals<A: FactCompatibilityStore>(
    memory: &MemoryApplication<A>,
) -> PendingReviewCount {
    match memory.count_pending_compatibility_fact_proposals().await {
        Ok(count) => match usize::try_from(count) {
            Ok(count) => PendingReviewCount::Counted(count),
            Err(error) => PendingReviewCount::unreadable(format!(
                "the pending fact-proposal count {count} is not representable here: {error}"
            )),
        },
        Err(error) => PendingReviewCount::unreadable(format!(
            "the project fact authority could not be read: {error}"
        )),
    }
}

/// Managed skills awaiting review in the user profile store.
///
/// A profile with no managed-skill store yet is a genuine zero (nothing has
/// ever been staged); only a real read failure is reported as unreadable.
pub async fn count_pending_managed_skills(profile_root: &Path) -> PendingReviewCount {
    match list_managed_skills(profile_root).await {
        Ok(skills) => PendingReviewCount::Counted(
            skills
                .iter()
                .filter(|skill| {
                    skill.metadata.state == ManagedSkillState::PendingApproval
                        || skill.pending_update.is_some()
                })
                .count(),
        ),
        Err(error) => PendingReviewCount::unreadable(format!(
            "the managed skill store could not be read: {error}"
        )),
    }
}

/// Reads both review queues. Independent reads: an unavailable fact authority
/// leaves the skill count intact and vice versa.
pub async fn count_pending_automation_output<A: FactCompatibilityStore>(
    memory: &MemoryApplication<A>,
    profile_root: &Path,
) -> AutomationPendingCounts {
    AutomationPendingCounts {
        fact_proposals: count_pending_fact_proposals(memory).await,
        skills: count_pending_managed_skills(profile_root).await,
    }
}

/// Decides whether a notice should fire for the current batch. Fires when
/// something is pending OR a queue could not be read, AND that batch differs
/// from what was last notified (different latest run id, different counts, or
/// a queue that has crossed between readable and unreadable).
pub fn should_notify(
    previous: Option<&AutomationNoticeState>,
    latest_run_id: Option<&str>,
    counts: &AutomationPendingCounts,
) -> bool {
    if counts.is_verified_empty() {
        return false;
    }
    match previous {
        None => true,
        Some(state) => {
            state.last_run_id.as_deref() != latest_run_id
                || state.pending_fact_proposals != counts.fact_proposals.count()
                || state.pending_skills != counts.skills.count()
        }
    }
}

/// Formats the compact one-line notice, or `None` only when every queue was
/// read and every one is empty. A queue nobody could read is reported as
/// unknown rather than passed over in silence, because silence here reads as
/// "nothing awaits your review".
pub fn staged_notice_message(counts: &AutomationPendingCounts) -> Option<String> {
    if counts.is_verified_empty() {
        return None;
    }
    let mut parts = Vec::new();
    if let Some(count) = counts.fact_proposals.count().filter(|count| *count > 0) {
        parts.push(format!(
            "{count} fact proposal{}",
            if count == 1 { "" } else { "s" }
        ));
    }
    if let Some(count) = counts.skills.count().filter(|count| *count > 0) {
        parts.push(format!(
            "{count} skill draft{}",
            if count == 1 { "" } else { "s" }
        ));
    }
    let mut sentence = String::from("TraceDecay automation: ");
    if !parts.is_empty() {
        sentence.push_str(&parts.join(" and "));
        sentence.push_str(if counts.counted_total() == 1 {
            " awaits review"
        } else {
            " await review"
        });
    }
    let unreadable = counts.unreadable();
    if !unreadable.is_empty() {
        if !parts.is_empty() {
            sentence.push_str("; ");
        }
        let labels: Vec<&str> = unreadable.iter().map(|(label, _)| *label).collect();
        sentence.push_str("the ");
        sentence.push_str(&labels.join(" and "));
        sentence.push_str(if labels.len() == 1 {
            " queue could not be read, so pending review is unknown"
        } else {
            " queues could not be read, so pending review is unknown"
        });
    }
    sentence.push_str(" — dashboard Memory and Skills tabs.");
    Some(sentence)
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
    if counts.is_verified_empty() {
        return None;
    }
    let latest_run_id = load_run_records(dashboard_root, 1)
        .await
        .ok()
        .and_then(|records| records.into_iter().next())
        .map(|record| record.run_id);
    let previous = load_notice_state(dashboard_root).await;
    if !should_notify(previous.as_ref(), latest_run_id.as_deref(), &counts) {
        return None;
    }
    let message = staged_notice_message(&counts)?;
    let state = AutomationNoticeState {
        last_run_id: latest_run_id,
        pending_fact_proposals: counts.fact_proposals.count(),
        pending_skills: counts.skills.count(),
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
            fact_proposals: PendingReviewCount::Counted(facts),
            skills: PendingReviewCount::Counted(skills),
        }
    }

    #[test]
    fn message_pluralizes_visible_fact_and_skill_reviews() {
        assert_eq!(staged_notice_message(&counts(0, 0)), None);
        assert_eq!(
            staged_notice_message(&counts(2, 1)).unwrap(),
            "TraceDecay automation: 2 fact proposals and 1 skill draft await review — dashboard Memory and Skills tabs."
        );
        assert_eq!(
            staged_notice_message(&counts(1, 0)).unwrap(),
            "TraceDecay automation: 1 fact proposal awaits review — dashboard Memory and Skills tabs."
        );
        assert_eq!(
            staged_notice_message(&counts(0, 3)).unwrap(),
            "TraceDecay automation: 3 skill drafts await review — dashboard Memory and Skills tabs."
        );
    }

    #[test]
    fn unreadable_queue_is_reported_rather_than_read_as_empty() {
        let both_unreadable = AutomationPendingCounts {
            fact_proposals: PendingReviewCount::unreadable("db locked"),
            skills: PendingReviewCount::unreadable("profile root missing"),
        };
        assert!(!both_unreadable.is_verified_empty());
        assert_eq!(both_unreadable.counted_total(), 0);
        assert_eq!(
            both_unreadable.unreadable(),
            vec![
                (FACT_PROPOSAL_QUEUE, "db locked"),
                (SKILL_DRAFT_QUEUE, "profile root missing"),
            ]
        );
        assert_eq!(
            staged_notice_message(&both_unreadable).unwrap(),
            "TraceDecay automation: the fact-proposal and skill-draft queues could not be read, so pending review is unknown — dashboard Memory and Skills tabs."
        );

        // One real count beside one unreadable queue: both are stated, and the
        // unreadable one is never folded into the total as a zero.
        let mixed = AutomationPendingCounts {
            fact_proposals: PendingReviewCount::Counted(2),
            skills: PendingReviewCount::unreadable("profile root missing"),
        };
        assert_eq!(mixed.counted_total(), 2);
        assert_eq!(
            staged_notice_message(&mixed).unwrap(),
            "TraceDecay automation: 2 fact proposals await review; the skill-draft queue could not be read, so pending review is unknown — dashboard Memory and Skills tabs."
        );

        // Only an all-read, all-empty batch may go silent.
        assert!(counts(0, 0).is_verified_empty());
        assert_eq!(staged_notice_message(&counts(0, 0)), None);
    }

    #[test]
    fn notify_fires_once_per_batch() {
        // Nothing pending: never notify.
        assert!(!should_notify(None, Some("run-1"), &counts(0, 0)));
        // First sighting of a pending batch: notify.
        assert!(should_notify(None, Some("run-1"), &counts(2, 1)));
        assert!(should_notify(None, Some("run-1"), &counts(2, 0)));
        let seen = AutomationNoticeState {
            last_run_id: Some("run-1".to_string()),
            pending_fact_proposals: Some(2),
            pending_skills: Some(1),
        };
        // Same batch again: stay quiet.
        assert!(!should_notify(Some(&seen), Some("run-1"), &counts(2, 1)));
        // New run appended: notify again.
        assert!(should_notify(Some(&seen), Some("run-2"), &counts(2, 1)));
        // Every pending-review count change rearms the notice.
        assert!(should_notify(Some(&seen), Some("run-1"), &counts(3, 1)));
        assert!(should_notify(Some(&seen), Some("run-1"), &counts(2, 2)));
    }

    #[test]
    fn notify_rearms_when_a_queue_becomes_unreadable() {
        let seen = AutomationNoticeState {
            last_run_id: Some("run-1".to_string()),
            pending_fact_proposals: Some(2),
            pending_skills: Some(1),
        };
        let went_dark = AutomationPendingCounts {
            fact_proposals: PendingReviewCount::Counted(2),
            skills: PendingReviewCount::unreadable("profile root missing"),
        };
        assert!(should_notify(Some(&seen), Some("run-1"), &went_dark));
        // Still dark on the next tick of the same batch: do not repeat.
        let dark_seen = AutomationNoticeState {
            last_run_id: Some("run-1".to_string()),
            pending_fact_proposals: Some(2),
            pending_skills: None,
        };
        assert!(!should_notify(Some(&dark_seen), Some("run-1"), &went_dark));
    }
}
