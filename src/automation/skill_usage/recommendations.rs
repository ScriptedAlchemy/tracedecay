use super::super::managed_skills::{ManagedSkillSource, ManagedSkillState};
use super::super::outcomes::{skill_outcome, SkillOutcomeRecord, SkillOutcomeVerdict};
use super::{SkillImprovementRecommendation, SkillStaleRecommendation, SkillUsageSummary};

pub fn stale_skill_recommendations(
    summaries: &[SkillUsageSummary],
    now_unix: i64,
    stale_after_secs: i64,
) -> Vec<SkillStaleRecommendation> {
    summaries
        .iter()
        .map(|summary| stale_skill_recommendation(summary, now_unix, stale_after_secs))
        .collect()
}

pub fn skill_improvement_recommendations(
    summaries: &[SkillUsageSummary],
) -> Vec<SkillImprovementRecommendation> {
    let now_unix = crate::tracedecay::current_timestamp();
    summaries
        .iter()
        .map(|summary| skill_improvement_recommendation(summary, now_unix))
        .collect()
}

fn stale_skill_recommendation(
    summary: &SkillUsageSummary,
    now_unix: i64,
    stale_after_secs: i64,
) -> SkillStaleRecommendation {
    let evidence = usage_evidence(summary);
    if summary.pinned {
        return keep_recommendation(
            summary,
            "pinned skills are excluded from archive recommendations",
            evidence,
        );
    }
    if summary.provenance_source == Some(ManagedSkillSource::UserDraft) {
        return keep_recommendation(
            summary,
            "user-authored skills require explicit user action before archive",
            evidence,
        );
    }
    match summary.state {
        Some(ManagedSkillState::Archived) => {
            return keep_recommendation(summary, "skill is already archived", evidence);
        }
        Some(ManagedSkillState::Disabled) => {
            return keep_recommendation(
                summary,
                "disabled skills are not auto-archive candidates",
                evidence,
            );
        }
        _ => {}
    }

    if summary.view_count == 0 && summary.use_count == 0 && summary.patch_count == 0 {
        return SkillStaleRecommendation {
            skill_id: summary.skill_id.clone(),
            stale: true,
            recommendation: "archive_review".to_string(),
            reason: "no view, use, or patch activity has been recorded".to_string(),
            evidence,
        };
    }

    let age_secs = now_unix.saturating_sub(summary.last_activity_at);
    if age_secs >= stale_after_secs && summary.use_count == 0 && summary.patch_count == 0 {
        return SkillStaleRecommendation {
            skill_id: summary.skill_id.clone(),
            stale: true,
            recommendation: "archive_review".to_string(),
            reason: format!("last viewed {age_secs} seconds ago with no recorded uses or patches"),
            evidence,
        };
    }

    // Outcome feedback: an approved skill that agents never used since
    // approval is a real-quality failure even when views keep it "active".
    if let Some(outcome) = ignored_outcome(summary, now_unix) {
        return SkillStaleRecommendation {
            skill_id: summary.skill_id.clone(),
            stale: true,
            recommendation: "archive_review".to_string(),
            reason: format!(
                "skill was approved {} days ago but has not been used since approval",
                outcome.days_since_approval
            ),
            evidence,
        };
    }

    keep_recommendation(
        summary,
        "recent or meaningful activity is present",
        evidence,
    )
}

/// The post-approval outcome when it is an actionable `ignored` verdict.
fn ignored_outcome(summary: &SkillUsageSummary, now_unix: i64) -> Option<SkillOutcomeRecord> {
    skill_outcome(summary, now_unix).filter(|outcome| {
        outcome.verdict == SkillOutcomeVerdict::Ignored
            && summary.state == Some(ManagedSkillState::Active)
    })
}

fn skill_improvement_recommendation(
    summary: &SkillUsageSummary,
    now_unix: i64,
) -> SkillImprovementRecommendation {
    let evidence = usage_evidence(summary);
    if summary.pinned {
        return no_improvement_recommendation(
            summary,
            "pinned skills require explicit user direction before patch recommendations",
            evidence,
        );
    }
    if summary.provenance_source == Some(ManagedSkillSource::UserDraft) {
        return no_improvement_recommendation(
            summary,
            "user-authored skills require explicit user direction before patch recommendations",
            evidence,
        );
    }
    if matches!(
        summary.state,
        Some(ManagedSkillState::Archived | ManagedSkillState::Disabled)
    ) {
        return no_improvement_recommendation(
            summary,
            "disabled or archived skills are not patch recommendation candidates",
            evidence,
        );
    }

    if summary.patch_count > 0 && summary.use_count == 0 {
        return SkillImprovementRecommendation {
            skill_id: summary.skill_id.clone(),
            improvement: true,
            recommendation: "patch_review".to_string(),
            reason: "skill has been patched but still has no recorded successful uses".to_string(),
            priority: "high".to_string(),
            evidence,
        };
    }
    if summary.patch_count >= 2 && summary.use_count <= summary.patch_count {
        return SkillImprovementRecommendation {
            skill_id: summary.skill_id.clone(),
            improvement: true,
            recommendation: "patch_review".to_string(),
            reason: "repeated patches suggest the skill instructions may still be unstable"
                .to_string(),
            priority: "medium".to_string(),
            evidence,
        };
    }
    if summary.view_count >= 3 && summary.use_count == 0 {
        return SkillImprovementRecommendation {
            skill_id: summary.skill_id.clone(),
            improvement: true,
            recommendation: "clarify_activation".to_string(),
            reason: "skill is repeatedly viewed but never used; activation guidance may be unclear"
                .to_string(),
            priority: "medium".to_string(),
            evidence,
        };
    }

    // Outcome feedback: approval was supposed to put the skill to work, so a
    // post-approval `ignored` verdict is a stronger review signal than raw
    // lifetime counts.
    if let Some(outcome) = ignored_outcome(summary, now_unix) {
        return SkillImprovementRecommendation {
            skill_id: summary.skill_id.clone(),
            improvement: true,
            recommendation: "clarify_activation".to_string(),
            reason: format!(
                "skill has not been used in the {} days since approval; \
                 review whether it should be revised or archived",
                outcome.days_since_approval
            ),
            priority: "medium".to_string(),
            evidence,
        };
    }

    no_improvement_recommendation(
        summary,
        "no repeated correction or failed-use signal is present",
        evidence,
    )
}

fn keep_recommendation(
    summary: &SkillUsageSummary,
    reason: impl Into<String>,
    evidence: Vec<String>,
) -> SkillStaleRecommendation {
    SkillStaleRecommendation {
        skill_id: summary.skill_id.clone(),
        stale: false,
        recommendation: "keep".to_string(),
        reason: reason.into(),
        evidence,
    }
}

fn no_improvement_recommendation(
    summary: &SkillUsageSummary,
    reason: impl Into<String>,
    evidence: Vec<String>,
) -> SkillImprovementRecommendation {
    SkillImprovementRecommendation {
        skill_id: summary.skill_id.clone(),
        improvement: false,
        recommendation: "none".to_string(),
        reason: reason.into(),
        priority: "none".to_string(),
        evidence,
    }
}

fn usage_evidence(summary: &SkillUsageSummary) -> Vec<String> {
    let mut evidence = vec![
        format!("state={}", optional_state_key(summary.state)),
        format!("pinned={}", summary.pinned),
        format!("views={}", summary.view_count),
        format!("uses={}", summary.use_count),
        format!("patches={}", summary.patch_count),
        format!("last_activity_at={}", summary.last_activity_at),
    ];
    if let Some(created_by) = &summary.created_by {
        evidence.push(format!("created_by={created_by}"));
    }
    if let Some(source) = summary.provenance_source {
        evidence.push(format!("provenance_source={}", source_key(source)));
    }
    if let Some(approved_at) = summary.approved_at {
        evidence.push(format!("approved_at={approved_at}"));
    }
    evidence
}

fn optional_state_key(state: Option<ManagedSkillState>) -> &'static str {
    match state {
        Some(ManagedSkillState::PendingApproval) => "pending_approval",
        Some(ManagedSkillState::Active) => "active",
        Some(ManagedSkillState::Disabled) => "disabled",
        Some(ManagedSkillState::Archived) => "archived",
        None => "unknown",
    }
}

fn source_key(source: ManagedSkillSource) -> &'static str {
    match source {
        ManagedSkillSource::AutomationRun => "automation_run",
        ManagedSkillSource::UserDraft => "user_draft",
        ManagedSkillSource::Import => "import",
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::super::super::outcomes::SKILL_ADOPTION_WINDOW_SECS;
    use super::super::SkillUsageRecord;
    use super::*;

    /// An approved, active, automation-authored skill with recent views but
    /// no uses since approval: kept by the plain activity heuristics, so any
    /// stale recommendation must come from the outcome verdict.
    fn ignored_since_approval_summary(now: i64) -> SkillUsageRecord {
        SkillUsageRecord {
            schema_version: 1,
            skill_id: "ignored-skill".to_string(),
            title: Some("Ignored skill".to_string()),
            category: Some("maintenance".to_string()),
            state: Some(ManagedSkillState::Active),
            pinned: false,
            created_by: Some("tracedecay".to_string()),
            provenance_source: Some(ManagedSkillSource::AutomationRun),
            targets: Vec::new(),
            view_count: 2,
            use_count: 0,
            patch_count: 0,
            first_seen_at: 0,
            last_activity_at: now,
            last_viewed_at: Some(now),
            last_used_at: None,
            last_patched_at: None,
            approved_at: Some(now - SKILL_ADOPTION_WINDOW_SECS - 1),
            view_count_at_approval: Some(0),
            use_count_at_approval: Some(0),
        }
    }

    #[test]
    fn ignored_outcome_strengthens_stale_recommendation() {
        let now = 100 * 24 * 60 * 60;
        let summary = ignored_since_approval_summary(now);

        let recommendation = stale_skill_recommendation(&summary, now, 365 * 24 * 60 * 60);
        assert!(recommendation.stale);
        assert_eq!(recommendation.recommendation, "archive_review");
        assert!(recommendation.reason.contains("since approval"));
        assert!(recommendation
            .evidence
            .iter()
            .any(|entry| entry.starts_with("approved_at=")));
    }

    #[test]
    fn adopted_outcome_keeps_skill() {
        let now = 100 * 24 * 60 * 60;
        let mut summary = ignored_since_approval_summary(now);
        summary.use_count = 3;
        summary.last_used_at = Some(now);

        let recommendation = stale_skill_recommendation(&summary, now, 365 * 24 * 60 * 60);
        assert!(!recommendation.stale);
        assert_eq!(recommendation.recommendation, "keep");
    }

    #[test]
    fn too_early_outcome_does_not_flag_stale() {
        let now = 100 * 24 * 60 * 60;
        let mut summary = ignored_since_approval_summary(now);
        summary.approved_at = Some(now - 1);
        summary.view_count_at_approval = Some(2);

        let recommendation = stale_skill_recommendation(&summary, now, 365 * 24 * 60 * 60);
        assert!(!recommendation.stale);
    }

    #[test]
    fn ignored_outcome_triggers_improvement_review() {
        let now = 100 * 24 * 60 * 60;
        let summary = ignored_since_approval_summary(now);

        let recommendation = skill_improvement_recommendation(&summary, now);
        assert!(recommendation.improvement);
        assert_eq!(recommendation.recommendation, "clarify_activation");
        assert!(recommendation.reason.contains("since approval"));
        assert_eq!(recommendation.priority, "medium");
    }

    #[test]
    fn never_approved_skill_gets_no_outcome_driven_improvement() {
        let now = 100 * 24 * 60 * 60;
        let mut summary = ignored_since_approval_summary(now);
        summary.approved_at = None;
        summary.view_count_at_approval = None;
        summary.use_count_at_approval = None;

        let recommendation = skill_improvement_recommendation(&summary, now);
        assert!(!recommendation.improvement);
        assert_eq!(recommendation.recommendation, "none");
    }
}
