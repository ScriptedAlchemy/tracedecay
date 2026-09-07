//! Pairwise overlap detection between managed skills (Hermes curator
//! parity). Detection is purely lexical (token Jaccard on normalized titles
//! and bodies) and deterministic; it never mutates skills.
//!
//! This module is the single skill-overlap authority. Validation — at
//! proposal parse time and again at lifecycle apply time — goes through
//! [`detected_skill_overlap_pair`], which judges exactly one pair and is
//! therefore order-independent and stable: whether two skills validate can
//! never depend on unrelated skills crowding a ranked list. The ranked
//! [`skill_overlap_candidates`] list is a bounded discovery surface for the
//! `skill_writer` evidence bundle only, computed pair-by-pair from the same
//! predicate; it is never a validation authority.

use serde::{Deserialize, Serialize};

use super::super::managed_skills::{ManagedSkill, ManagedSkillState};
use crate::memory::similarity::lexical_overlap;

/// Minimum content (title+summary+body) token Jaccard for a pair to count as
/// a consolidation candidate on its own.
pub const SKILL_OVERLAP_CONTENT_THRESHOLD: f64 = 0.35;
/// Title-token Jaccard that flags a pair when combined with a weaker content
/// overlap floor.
pub const SKILL_OVERLAP_TITLE_THRESHOLD: f64 = 0.5;
/// Content overlap floor that must accompany a high title overlap.
pub const SKILL_OVERLAP_TITLE_CONTENT_FLOOR: f64 = 0.2;
/// Default cap on how many candidate pairs are surfaced as evidence.
pub const DEFAULT_SKILL_OVERLAP_LIMIT: usize = 5;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SkillOverlapCandidate {
    pub skill_a: String,
    pub skill_b: String,
    pub skill_a_pinned: bool,
    pub skill_b_pinned: bool,
    pub title_overlap: f64,
    pub content_overlap: f64,
    pub score: f64,
    pub shared_tokens: Vec<String>,
    pub recommendation: String,
    pub reason: String,
}

/// Order-independent pairwise overlap predicate: the single validation
/// authority shared by proposal parsing and lifecycle application. The
/// decision depends only on the two skills — their consolidation eligibility
/// and the lexical thresholds — never on how a pair ranks against unrelated
/// skills.
pub fn detected_skill_overlap_pair(first: &ManagedSkill, second: &ManagedSkill) -> bool {
    detected_overlap(first, second).is_some()
}

/// The strongest detected overlap partner for `skill` among `others`, judged
/// per pair by the same authority as [`detected_skill_overlap_pair`]. Unlike
/// the ranked discovery list this is never truncated, so a partner cannot be
/// crowded out by unrelated higher-scoring pairs. Score ties break to the
/// lexicographically smaller partner id for determinism.
pub fn detected_skill_overlap_partner<'a, I>(
    skill: &ManagedSkill,
    others: I,
) -> Option<&'a ManagedSkill>
where
    I: IntoIterator<Item = &'a ManagedSkill>,
{
    let mut best: Option<(f64, &'a ManagedSkill)> = None;
    for other in others {
        let Some(candidate) = detected_overlap(skill, other) else {
            continue;
        };
        let replace =
            best.as_ref().is_none_or(
                |(score, current)| match candidate.score.partial_cmp(score) {
                    Some(std::cmp::Ordering::Greater) => true,
                    Some(std::cmp::Ordering::Equal) => other.metadata.id < current.metadata.id,
                    _ => false,
                },
            );
        if replace {
            best = Some((candidate.score, other));
        }
    }
    best.map(|(_, partner)| partner)
}

/// Computes the top overlapping managed-skill pairs above the lexical
/// thresholds, pair-by-pair through the same predicate that backs
/// [`detected_skill_overlap_pair`]. This ranked, truncated view exists to
/// bound the `skill_writer` evidence bundle; validation must use the pairwise
/// authority instead.
pub fn skill_overlap_candidates(
    skills: &[ManagedSkill],
    limit: usize,
) -> Vec<SkillOverlapCandidate> {
    let mut candidates = Vec::new();
    for (index, a) in skills.iter().enumerate() {
        for b in skills.iter().skip(index + 1) {
            if let Some(candidate) = detected_overlap(a, b) {
                candidates.push(candidate);
            }
        }
    }
    candidates.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.skill_a.cmp(&b.skill_a))
            .then_with(|| a.skill_b.cmp(&b.skill_b))
    });
    candidates.truncate(limit);
    candidates
}

/// Consolidation eligibility: archived and disabled skills are out of scope
/// and pinned skills are exempt, matching the Hermes curator.
fn overlap_eligible(skill: &ManagedSkill) -> bool {
    matches!(skill.metadata.state, ManagedSkillState::Active) && !skill.metadata.pinned
}

fn detected_overlap(a: &ManagedSkill, b: &ManagedSkill) -> Option<SkillOverlapCandidate> {
    if a.metadata.id == b.metadata.id || !overlap_eligible(a) || !overlap_eligible(b) {
        return None;
    }
    overlap_candidate(a, b)
}

fn overlap_candidate(a: &ManagedSkill, b: &ManagedSkill) -> Option<SkillOverlapCandidate> {
    let (_, title_overlap, _) = lexical_overlap(&a.metadata.title, &b.metadata.title);
    let (content_payload, content_overlap, _) =
        lexical_overlap(&skill_content_text(a), &skill_content_text(b));
    let overlapping = content_overlap >= SKILL_OVERLAP_CONTENT_THRESHOLD
        || (title_overlap >= SKILL_OVERLAP_TITLE_THRESHOLD
            && content_overlap >= SKILL_OVERLAP_TITLE_CONTENT_FLOOR);
    if !overlapping {
        return None;
    }
    let shared_tokens = content_payload
        .get("shared_tokens")
        .and_then(serde_json::Value::as_array)
        .map(|tokens| {
            tokens
                .iter()
                .filter_map(|token| token.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    Some(SkillOverlapCandidate {
        skill_a: a.metadata.id.clone(),
        skill_b: b.metadata.id.clone(),
        skill_a_pinned: a.metadata.pinned,
        skill_b_pinned: b.metadata.pinned,
        title_overlap,
        content_overlap,
        score: content_overlap.max(title_overlap),
        shared_tokens,
        recommendation: "merge_or_archive_review".to_string(),
        reason: format!(
            "managed skills '{}' and '{}' share {:.0}% of normalized content tokens ({:.0}% of title tokens)",
            a.metadata.id,
            b.metadata.id,
            content_overlap * 100.0,
            title_overlap * 100.0
        ),
    })
}

fn skill_content_text(skill: &ManagedSkill) -> String {
    format!(
        "{}\n{}\n{}",
        skill.metadata.title, skill.metadata.summary, skill.body_markdown
    )
}

#[cfg(test)]
mod tests {
    use super::super::super::managed_skills::{
        ManagedSkillDraft, ManagedSkillProvenance, ManagedSkillSource,
        default_managed_skill_targets,
    };
    use super::*;

    fn skill(id: &str, title: &str, summary: &str, body: &str) -> ManagedSkill {
        let draft = ManagedSkillDraft {
            id: id.to_string(),
            title: title.to_string(),
            summary: summary.to_string(),
            routing_description:
                "Repeated repository workflows requiring this maintained procedure.".to_owned(),
            category: "workflow".to_string(),
            targets: default_managed_skill_targets(),
            body_markdown: body.to_string(),
            support_files: Vec::new(),
            provenance: ManagedSkillProvenance {
                source: ManagedSkillSource::AutomationRun,
                actor: "test".to_string(),
                run_id: None,
            },
        };
        match draft.materialize() {
            Ok(skill) => skill,
            Err(err) => panic!("test fixture skill should materialize: {err}"),
        }
    }

    fn overlapping_pair() -> (ManagedSkill, ManagedSkill) {
        (
            skill(
                "review-automation-runs",
                "Review automation runs",
                "Review automation run ledgers after automatic changes apply.",
                "Check run ledger counts, rejected proposals, and deployment receipts after automatic application.",
            ),
            skill(
                "automation-run-review",
                "Automation run review",
                "Review automatically applied automation run outcomes.",
                "Check run ledger counts, rejected proposals, and deployment receipts after automatic application.",
            ),
        )
    }

    fn unrelated_skill() -> ManagedSkill {
        skill(
            "rust-error-handling",
            "Rust error handling",
            "Prefer thiserror enums over anyhow in library crates.",
            "Model failures with typed enums. Reserve panics for invariants. Convert io errors at module boundaries.",
        )
    }

    #[test]
    fn detects_overlapping_pairs_above_threshold() {
        let (a, b) = overlapping_pair();
        let unrelated = unrelated_skill();
        let candidates = skill_overlap_candidates(&[a, b, unrelated], DEFAULT_SKILL_OVERLAP_LIMIT);

        assert_eq!(candidates.len(), 1);
        let candidate = &candidates[0];
        assert_eq!(candidate.skill_a, "review-automation-runs");
        assert_eq!(candidate.skill_b, "automation-run-review");
        assert!(candidate.content_overlap >= SKILL_OVERLAP_CONTENT_THRESHOLD);
        assert_eq!(candidate.recommendation, "merge_or_archive_review");
        assert!(!candidate.shared_tokens.is_empty());
    }

    #[test]
    fn skips_pairs_where_either_skill_is_pinned() {
        let (mut a, mut b) = overlapping_pair();
        a.set_pinned(true);
        assert!(
            skill_overlap_candidates(&[a.clone(), b.clone()], DEFAULT_SKILL_OVERLAP_LIMIT)
                .is_empty()
        );

        a.set_pinned(false);
        b.set_pinned(true);
        assert!(skill_overlap_candidates(&[a, b], DEFAULT_SKILL_OVERLAP_LIMIT).is_empty());
    }

    #[test]
    fn ignores_archived_and_disabled_skills() {
        let (mut a, b) = overlapping_pair();
        a.set_state(ManagedSkillState::Archived);
        assert!(
            skill_overlap_candidates(&[a.clone(), b.clone()], DEFAULT_SKILL_OVERLAP_LIMIT)
                .is_empty()
        );
        a.set_state(ManagedSkillState::Disabled);
        assert!(skill_overlap_candidates(&[a, b], DEFAULT_SKILL_OVERLAP_LIMIT).is_empty());
    }

    #[test]
    fn respects_the_candidate_limit() {
        let (a, b) = overlapping_pair();
        let c = skill(
            "automation-run-checks",
            "Automation run checks",
            "Review automation run ledgers after automatic changes apply.",
            "Check run ledger counts, rejected proposals, and deployment receipts after automatic application.",
        );
        let candidates = skill_overlap_candidates(&[a, b, c], 1);
        assert_eq!(candidates.len(), 1);
    }

    #[test]
    fn pair_authority_is_order_independent() {
        let (a, b) = overlapping_pair();
        let unrelated = unrelated_skill();

        assert!(detected_skill_overlap_pair(&a, &b));
        assert!(detected_skill_overlap_pair(&b, &a));
        assert!(!detected_skill_overlap_pair(&a, &unrelated));
        assert!(!detected_skill_overlap_pair(&unrelated, &a));
        assert!(
            !detected_skill_overlap_pair(&a, &a),
            "a skill must never be its own overlap partner"
        );
    }

    #[test]
    fn pair_authority_rejects_pinned_and_inactive_skills() {
        let (mut a, mut b) = overlapping_pair();
        a.set_pinned(true);
        assert!(!detected_skill_overlap_pair(&a, &b));
        a.set_pinned(false);
        b.set_state(ManagedSkillState::Archived);
        assert!(!detected_skill_overlap_pair(&a, &b));
        b.set_state(ManagedSkillState::Disabled);
        assert!(!detected_skill_overlap_pair(&a, &b));
    }

    #[test]
    fn partner_discovery_prefers_the_strongest_pair() {
        let (a, twin) = overlapping_pair();
        let weaker = skill(
            "automation-outcome-notes",
            "Automation outcome notes",
            "Review automation run ledgers after automatic changes apply.",
            "Check run ledger counts, then escalate repeated regressions to the owning workflow for review.",
        );
        let twin_score = detected_overlap(&a, &twin)
            .expect("premise: the twin must be a detected pair")
            .score;
        let weaker_score = detected_overlap(&a, &weaker)
            .expect("premise: the weaker skill must also be a detected pair")
            .score;
        assert!(
            twin_score > weaker_score,
            "premise: the twin ({twin_score}) must outscore the weaker pair ({weaker_score})"
        );
        let unrelated = unrelated_skill();
        let skills = [weaker, twin.clone(), unrelated.clone()];

        let partner =
            detected_skill_overlap_partner(&a, &skills).expect("a detected partner exists");
        assert_eq!(partner.metadata.id, twin.metadata.id);
        assert!(
            detected_skill_overlap_partner(&unrelated, &skills[..2]).is_none(),
            "a skill without any pairwise overlap has no partner"
        );
    }
}
