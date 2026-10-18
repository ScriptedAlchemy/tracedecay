use super::super::managed_skills::{ManagedSkillSource, ManagedSkillState};
use super::{SkillImprovementRecommendation, SkillStaleRecommendation, SkillUsageSummary};

pub fn stale_skill_recommendations(
    summaries: &[SkillUsageSummary],
) -> Vec<SkillStaleRecommendation> {
    summaries.iter().map(stale_skill_recommendation).collect()
}

pub fn skill_improvement_recommendations(
    summaries: &[SkillUsageSummary],
) -> Vec<SkillImprovementRecommendation> {
    summaries
        .iter()
        .map(skill_improvement_recommendation)
        .collect()
}

fn stale_skill_recommendation(summary: &SkillUsageSummary) -> SkillStaleRecommendation {
    let evidence = usage_evidence(summary);
    if summary.pinned {
        return keep_recommendation(
            summary,
            "pinned skills are excluded from archive recommendations",
            evidence,
        );
    }
    if summary.provenance_source == Some(ManagedSkillSource::User) {
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

    // No production host records a skill use, so absent activity is missing
    // telemetry rather than evidence that the skill is stale.
    keep_recommendation(
        summary,
        "skill use is not measured, so absent activity is unknown, not stale",
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
    if summary.provenance_source == Some(ManagedSkillSource::User) {
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

    if summary.patch_count >= 2 && summary.use_count <= summary.patch_count {
        return SkillImprovementRecommendation {
            skill_id: summary.skill_id.clone(),
            improvement: true,
            recommendation: "repair_candidate".to_string(),
            reason: "repeated patches suggest the skill instructions may still be unstable"
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
        format!(
            "use_telemetry={}",
            if summary.last_used_at.is_some() {
                "observed"
            } else {
                "unobserved"
            }
        ),
        format!("patches={}", summary.patch_count),
        format!("last_activity_at={}", summary.last_activity_at),
    ];
    if let Some(created_by) = &summary.created_by {
        evidence.push(format!("created_by={created_by}"));
    }
    if let Some(source) = summary.provenance_source {
        evidence.push(format!("provenance_source={}", source_key(source)));
    }
    if let Some(activated_at) = summary.activated_at {
        evidence.push(format!("activated_at={activated_at}"));
    }
    evidence
}

fn optional_state_key(state: Option<ManagedSkillState>) -> &'static str {
    match state {
        Some(ManagedSkillState::Active) => "active",
        Some(ManagedSkillState::Disabled) => "disabled",
        Some(ManagedSkillState::Archived) => "archived",
        None => "unknown",
    }
}

fn source_key(source: ManagedSkillSource) -> &'static str {
    match source {
        ManagedSkillSource::AutomationRun => "automation_run",
        ManagedSkillSource::User => "user",
        ManagedSkillSource::Import => "import",
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::super::SkillUsageRecord;
    use super::*;
    use std::collections::BTreeSet;

    const DAY: i64 = 24 * 60 * 60;

    fn automation_skill(skill_id: &str) -> SkillUsageRecord {
        SkillUsageRecord {
            schema_version: 3,
            skill_id: skill_id.to_string(),
            title: Some(format!("{skill_id} title")),
            category: Some("maintenance".to_string()),
            state: Some(ManagedSkillState::Active),
            pinned: false,
            created_by: Some("tracedecay".to_string()),
            provenance_source: Some(ManagedSkillSource::AutomationRun),
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
            imported_analytics_events: BTreeSet::new(),
        }
    }

    fn activated_long_ago(skill_id: &str, now: i64) -> SkillUsageRecord {
        let mut summary = automation_skill(skill_id);
        summary.activated_at = Some(now - 200 * DAY);
        summary.last_activity_at = now - 200 * DAY;
        summary.view_count_at_activation = Some(0);
        summary.use_count_at_activation = Some(0);
        summary
    }

    fn by_id<'a, T>(items: &'a [T], skill_id: &str, id: fn(&T) -> &str) -> &'a T {
        items.iter().find(|item| id(item) == skill_id).unwrap()
    }

    #[test]
    fn absent_skill_activity_is_unknown_not_an_archive_signal() {
        let now = tracedecay_runtime_core::tracedecay::current_timestamp();
        let idle = activated_long_ago("idle-skill", now);
        let mut viewed_once = activated_long_ago("viewed-once-skill", now);
        viewed_once.view_count = 1;
        viewed_once.last_viewed_at = Some(now - 150 * DAY);
        viewed_once.last_activity_at = now - 150 * DAY;
        let mut pinned = activated_long_ago("pinned-skill", now);
        pinned.pinned = true;
        let mut user = activated_long_ago("user-skill", now);
        user.provenance_source = Some(ManagedSkillSource::User);

        let recommendations = stale_skill_recommendations(&[idle, viewed_once, pinned, user]);
        let id: fn(&SkillStaleRecommendation) -> &str = |recommendation| &recommendation.skill_id;

        for skill_id in ["idle-skill", "viewed-once-skill"] {
            let recommendation = by_id(&recommendations, skill_id, id);
            assert!(!recommendation.stale, "{recommendation:?}");
            assert_eq!(recommendation.recommendation, "keep");
            assert!(
                recommendation.reason.contains("unknown"),
                "{recommendation:?}"
            );
            assert!(
                recommendation
                    .evidence
                    .contains(&"use_telemetry=unobserved".to_string()),
                "{recommendation:?}"
            );
        }
        let pinned = by_id(&recommendations, "pinned-skill", id);
        assert!(!pinned.stale);
        assert!(pinned.reason.contains("pinned"));
        let user = by_id(&recommendations, "user-skill", id);
        assert!(!user.stale);
        assert!(user.reason.contains("user-authored"));
    }

    #[test]
    fn only_repeated_patches_recommend_skill_repair() {
        let now = tracedecay_runtime_core::tracedecay::current_timestamp();
        let mut viewed_unused = activated_long_ago("viewed-unused-skill", now);
        viewed_unused.view_count = 3;
        viewed_unused.last_viewed_at = Some(now - DAY);
        let mut patched_once = activated_long_ago("patched-once-skill", now);
        patched_once.patch_count = 1;
        patched_once.last_patched_at = Some(now - DAY);
        let unused = activated_long_ago("unused-skill", now);
        let mut patched_twice = activated_long_ago("patched-twice-skill", now);
        patched_twice.patch_count = 2;
        patched_twice.last_patched_at = Some(now - DAY);

        let recommendations = skill_improvement_recommendations(&[
            viewed_unused,
            patched_once,
            unused,
            patched_twice,
        ]);
        let id: fn(&SkillImprovementRecommendation) -> &str =
            |recommendation| &recommendation.skill_id;

        for skill_id in ["viewed-unused-skill", "patched-once-skill", "unused-skill"] {
            let recommendation = by_id(&recommendations, skill_id, id);
            assert!(!recommendation.improvement, "{recommendation:?}");
            assert_eq!(recommendation.recommendation, "none");
        }
        let repair = by_id(&recommendations, "patched-twice-skill", id);
        assert!(repair.improvement);
        assert_eq!(repair.recommendation, "repair_candidate");
        assert_eq!(repair.priority, "medium");
    }
}
