use super::super::managed_skills::{ManagedSkillSource, ManagedSkillState};
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
    summaries
        .iter()
        .map(skill_improvement_recommendation)
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

    keep_recommendation(
        summary,
        "recent or meaningful activity is present",
        evidence,
    )
}

fn skill_improvement_recommendation(summary: &SkillUsageSummary) -> SkillImprovementRecommendation {
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
