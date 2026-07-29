use std::collections::BTreeMap;

use thiserror::Error;
use tracedecay_domain::{ByteRangeV1, RetrievalAnchorId};

use super::candidates::CandidateChannel;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RankingCandidate {
    pub stable_id: String,
    pub anchor_id: RetrievalAnchorId,
    pub retriever_record_id: String,
    pub channel: CandidateChannel,
    pub raw_score: i64,
    pub knowledge_at_micros: i64,
    pub logical_message: Option<String>,
    pub turn: Option<String>,
    pub session: Option<String>,
    pub source: Option<String>,
    pub evidence_role: Option<String>,
    pub exact_ranges: Vec<ByteRangeV1>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DiversityLimits {
    pub per_logical_message: usize,
    pub per_turn: usize,
    pub per_session: usize,
    pub per_source: usize,
    pub per_evidence_role: usize,
}

impl DiversityLimits {
    pub const fn unbounded() -> Self {
        Self {
            per_logical_message: usize::MAX,
            per_turn: usize::MAX,
            per_session: usize::MAX,
            per_source: usize::MAX,
            per_evidence_role: usize::MAX,
        }
    }
}

impl Default for DiversityLimits {
    fn default() -> Self {
        Self {
            per_logical_message: 1,
            per_turn: 2,
            per_session: 8,
            per_source: 4,
            per_evidence_role: 8,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RankedCandidate {
    pub stable_id: String,
    pub anchor_id: RetrievalAnchorId,
    pub normalized_score_micros: u64,
    pub knowledge_at_micros: i64,
    pub logical_message: Option<String>,
    pub turn: Option<String>,
    pub session: Option<String>,
    pub source: Option<String>,
    pub evidence_role: Option<String>,
    pub contributions: Vec<RetrieverContribution>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RetrieverContribution {
    pub channel: CandidateChannel,
    pub source: Option<String>,
    pub retriever_record_id: String,
    pub retriever_ordinal: u64,
    pub raw_score: i64,
    pub calibrated_score_micros: u64,
    pub exact_ranges: Vec<ByteRangeV1>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum RankTier {
    Approximate = 1,
    ExactPhrase = 2,
    ExactMessage = 3,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum RankingError {
    #[error("duplicate stable_id `{stable_id}` has conflicting ranking metadata across partitions")]
    ConflictingDuplicateMetadata { stable_id: String },
}

pub type RankedResult = Result<Vec<RankedCandidate>, RankingError>;

const TIER_SPAN: u64 = 1_000_000;

pub fn rank_candidates(candidates: &[RankingCandidate], limits: DiversityLimits) -> RankedResult {
    rank_validated_candidates(candidates, limits)
}

#[deprecated(
    note = "compatibility alias; delete after callers migrate to the fallible rank_candidates API"
)]
pub fn try_rank_candidates(
    candidates: &[RankingCandidate],
    limits: DiversityLimits,
) -> RankedResult {
    rank_candidates(candidates, limits)
}

/// Partition key for raw-score normalization. Absent sources stay singleton
/// partitions without colliding with a concrete `source` string value.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum SourcePartitionKey {
    Absent { stable_id: String },
    Present(String),
}

impl SourcePartitionKey {
    fn from_candidate(candidate: &RankingCandidate) -> Self {
        match &candidate.source {
            Some(source) => Self::Present(source.clone()),
            None => Self::Absent {
                stable_id: candidate.stable_id.clone(),
            },
        }
    }
}

fn rank_validated_candidates(
    candidates: &[RankingCandidate],
    limits: DiversityLimits,
) -> RankedResult {
    let candidates = prepare_candidates(candidates)?;
    let mut by_channel_and_source: BTreeMap<
        (CandidateChannel, SourcePartitionKey),
        Vec<&RankingCandidate>,
    > = BTreeMap::new();
    for candidate in &candidates {
        by_channel_and_source
            .entry((
                candidate.channel,
                SourcePartitionKey::from_candidate(candidate),
            ))
            .or_default()
            .push(candidate);
    }

    let mut best_by_id: BTreeMap<String, ScoredFusion> = BTreeMap::new();
    for ((channel, _source), mut channel_candidates) in by_channel_and_source {
        channel_candidates.sort_by(|left, right| {
            right
                .raw_score
                .cmp(&left.raw_score)
                .then_with(|| right.knowledge_at_micros.cmp(&left.knowledge_at_micros))
                .then_with(|| left.stable_id.cmp(&right.stable_id))
        });
        let count = channel_candidates.len() as u64;
        let tier = rank_tier(channel);
        for (index, candidate) in channel_candidates.into_iter().enumerate() {
            let ordinal = count.saturating_sub(index as u64);
            let within_channel =
                (u128::from(ordinal) * u128::from(TIER_SPAN - 1) / u128::from(count)) as u64;
            let contribution = encode_score(tier, within_channel);
            let provenance = RetrieverContribution {
                channel,
                source: candidate.source.clone(),
                retriever_record_id: candidate.retriever_record_id.clone(),
                retriever_ordinal: u64::try_from(index).unwrap_or(u64::MAX),
                raw_score: candidate.raw_score,
                calibrated_score_micros: contribution,
                exact_ranges: candidate.exact_ranges.clone(),
            };
            match best_by_id.get_mut(&candidate.stable_id) {
                Some(existing) => {
                    merge_contribution(existing, tier, contribution, provenance);
                }
                None => {
                    best_by_id.insert(
                        candidate.stable_id.clone(),
                        ScoredFusion {
                            tier,
                            within_tier_score: contribution,
                            ranked: RankedCandidate {
                                stable_id: candidate.stable_id.clone(),
                                anchor_id: candidate.anchor_id.clone(),
                                normalized_score_micros: contribution,
                                knowledge_at_micros: candidate.knowledge_at_micros,
                                logical_message: candidate.logical_message.clone(),
                                turn: candidate.turn.clone(),
                                session: candidate.session.clone(),
                                source: candidate.source.clone(),
                                evidence_role: candidate.evidence_role.clone(),
                                contributions: vec![provenance],
                            },
                        },
                    );
                }
            }
        }
    }

    let mut ranked = best_by_id
        .into_values()
        .map(|fusion| {
            let mut ranked = fusion.ranked;
            ranked.normalized_score_micros = fusion.within_tier_score;
            ranked.contributions.sort_by(|left, right| {
                rank_tier(right.channel)
                    .cmp(&rank_tier(left.channel))
                    .then_with(|| left.channel.cmp(&right.channel))
                    .then_with(|| left.source.cmp(&right.source))
                    .then_with(|| left.retriever_ordinal.cmp(&right.retriever_ordinal))
            });
            ranked
        })
        .collect::<Vec<_>>();
    ranked.sort_by(|left, right| {
        right
            .normalized_score_micros
            .cmp(&left.normalized_score_micros)
            .then_with(|| right.knowledge_at_micros.cmp(&left.knowledge_at_micros))
            .then_with(|| left.stable_id.cmp(&right.stable_id))
    });
    Ok(apply_diversity(ranked, limits))
}

fn prepare_candidates(
    candidates: &[RankingCandidate],
) -> Result<Vec<RankingCandidate>, RankingError> {
    let mut metadata_by_id = BTreeMap::<String, RankingCandidate>::new();

    for candidate in candidates {
        match metadata_by_id.get_mut(&candidate.stable_id) {
            Some(existing) => {
                if metadata_conflicts(existing, candidate) {
                    return Err(RankingError::ConflictingDuplicateMetadata {
                        stable_id: candidate.stable_id.clone(),
                    });
                }
                fill_missing_metadata(existing, candidate);
            }
            None => {
                metadata_by_id.insert(candidate.stable_id.clone(), candidate.clone());
            }
        }
    }

    // An idempotent max-evidence collapse prevents duplicate row count from
    // changing any channel partition's ordinal denominator.
    let mut unique_by_id_channel_and_record =
        BTreeMap::<(String, CandidateChannel, String), RankingCandidate>::new();
    for candidate in candidates {
        let key = (
            candidate.stable_id.clone(),
            candidate.channel,
            candidate.retriever_record_id.clone(),
        );
        match unique_by_id_channel_and_record.get_mut(&key) {
            Some(existing) => {
                if existing.source != candidate.source {
                    return Err(RankingError::ConflictingDuplicateMetadata {
                        stable_id: candidate.stable_id.clone(),
                    });
                }
                let mut exact_ranges = existing.exact_ranges.clone();
                exact_ranges.extend(candidate.exact_ranges.iter().copied());
                if candidate.raw_score > existing.raw_score {
                    *existing = candidate.clone();
                }
                exact_ranges.sort_by_key(|range| (range.start(), range.end()));
                exact_ranges.dedup();
                existing.exact_ranges = exact_ranges;
            }
            None => {
                unique_by_id_channel_and_record.insert(key, candidate.clone());
            }
        }
    }

    for candidate in unique_by_id_channel_and_record.values_mut() {
        let Some(metadata) = metadata_by_id.get(&candidate.stable_id) else {
            return Err(RankingError::ConflictingDuplicateMetadata {
                stable_id: candidate.stable_id.clone(),
            });
        };
        copy_metadata(candidate, metadata);
    }

    Ok(unique_by_id_channel_and_record.into_values().collect())
}

fn copy_metadata(candidate: &mut RankingCandidate, metadata: &RankingCandidate) {
    candidate.anchor_id = metadata.anchor_id.clone();
    candidate.knowledge_at_micros = metadata.knowledge_at_micros;
    candidate
        .logical_message
        .clone_from(&metadata.logical_message);
    candidate.turn.clone_from(&metadata.turn);
    candidate.session.clone_from(&metadata.session);
    candidate.source.clone_from(&metadata.source);
    candidate.evidence_role.clone_from(&metadata.evidence_role);
}

struct ScoredFusion {
    tier: RankTier,
    within_tier_score: u64,
    ranked: RankedCandidate,
}

fn merge_contribution(
    existing: &mut ScoredFusion,
    tier: RankTier,
    contribution: u64,
    provenance: RetrieverContribution,
) {
    existing.ranked.contributions.push(provenance);
    if tier > existing.tier {
        existing.tier = tier;
        existing.within_tier_score = contribution;
    } else if tier == existing.tier {
        existing.within_tier_score = existing.within_tier_score.max(contribution);
    }
}

fn metadata_conflicts(existing: &RankingCandidate, candidate: &RankingCandidate) -> bool {
    existing.anchor_id != candidate.anchor_id
        || existing.knowledge_at_micros != candidate.knowledge_at_micros
        // Source domains are never partially filled: Absent vs Present would
        // reclassify raw scores into another calibration partition.
        || existing.source != candidate.source
        || option_conflicts(
            existing.logical_message.as_deref(),
            candidate.logical_message.as_deref(),
        )
        || option_conflicts(existing.turn.as_deref(), candidate.turn.as_deref())
        || option_conflicts(existing.session.as_deref(), candidate.session.as_deref())
        || option_conflicts(
            existing.evidence_role.as_deref(),
            candidate.evidence_role.as_deref(),
        )
}

fn option_conflicts(left: Option<&str>, right: Option<&str>) -> bool {
    matches!((left, right), (Some(left), Some(right)) if left != right)
}

fn fill_missing_metadata(existing: &mut RankingCandidate, candidate: &RankingCandidate) {
    if existing.logical_message.is_none() {
        existing
            .logical_message
            .clone_from(&candidate.logical_message);
    }
    if existing.turn.is_none() {
        existing.turn.clone_from(&candidate.turn);
    }
    if existing.session.is_none() {
        existing.session.clone_from(&candidate.session);
    }
    if existing.evidence_role.is_none() {
        existing.evidence_role.clone_from(&candidate.evidence_role);
    }
}

const fn rank_tier(channel: CandidateChannel) -> RankTier {
    match channel {
        CandidateChannel::Anchor | CandidateChannel::ExactMessage => RankTier::ExactMessage,
        CandidateChannel::Phrase | CandidateChannel::Span | CandidateChannel::Burst => {
            RankTier::ExactPhrase
        }
        CandidateChannel::Scope
        | CandidateChannel::Entity
        | CandidateChannel::Time
        | CandidateChannel::Lexical
        | CandidateChannel::Summary => RankTier::Approximate,
    }
}

fn encode_score(tier: RankTier, within_tier: u64) -> u64 {
    let capped = if within_tier < TIER_SPAN {
        within_tier
    } else {
        TIER_SPAN - 1
    };
    (tier as u64)
        .saturating_mul(TIER_SPAN)
        .saturating_add(capped)
}

fn apply_diversity(ranked: Vec<RankedCandidate>, limits: DiversityLimits) -> Vec<RankedCandidate> {
    let mut logical_messages = BTreeMap::new();
    let mut turns = BTreeMap::new();
    let mut sessions = BTreeMap::new();
    let mut sources = BTreeMap::new();
    let mut evidence_roles = BTreeMap::new();
    ranked
        .into_iter()
        .filter(|candidate| {
            if at_limit(
                &logical_messages,
                candidate.logical_message.as_deref(),
                limits.per_logical_message,
            ) || at_limit(&turns, candidate.turn.as_deref(), limits.per_turn)
                || at_limit(&sessions, candidate.session.as_deref(), limits.per_session)
                || at_limit(&sources, candidate.source.as_deref(), limits.per_source)
                || at_limit(
                    &evidence_roles,
                    candidate.evidence_role.as_deref(),
                    limits.per_evidence_role,
                )
            {
                return false;
            }
            increment(&mut logical_messages, candidate.logical_message.as_deref());
            increment(&mut turns, candidate.turn.as_deref());
            increment(&mut sessions, candidate.session.as_deref());
            increment(&mut sources, candidate.source.as_deref());
            increment(&mut evidence_roles, candidate.evidence_role.as_deref());
            true
        })
        .collect()
}

fn at_limit(counts: &BTreeMap<String, usize>, key: Option<&str>, limit: usize) -> bool {
    let Some(key) = key else {
        return false;
    };
    counts.get(key).copied().unwrap_or_default() >= limit
}

fn increment(counts: &mut BTreeMap<String, usize>, key: Option<&str>) {
    if let Some(key) = key {
        let count = counts.entry(key.to_string()).or_default();
        *count = count.saturating_add(1);
    }
}

#[cfg(test)]
mod tests {
    use tracedecay_domain::RetrievalAnchorId;

    use super::*;
    use crate::temporal::candidates::CandidateChannel;

    fn candidate(
        stable_id: &str,
        channel: CandidateChannel,
        raw_score: i64,
        logical_message: Option<&str>,
    ) -> RankingCandidate {
        let anchor_id: RetrievalAnchorId =
            serde_json::from_str(&format!("\"{stable_id}\"")).expect("valid anchor");
        RankingCandidate {
            stable_id: stable_id.to_string(),
            anchor_id,
            retriever_record_id: stable_id.to_string(),
            channel,
            raw_score,
            knowledge_at_micros: 1,
            logical_message: logical_message.map(str::to_string),
            turn: None,
            session: Some("session-1".to_string()),
            source: Some("store-a".to_string()),
            evidence_role: Some("message".to_string()),
            exact_ranges: Vec::new(),
        }
    }

    fn ids(ranked: &[RankedCandidate]) -> Vec<&str> {
        ranked.iter().map(|item| item.stable_id.as_str()).collect()
    }

    fn rank(candidates: &[RankingCandidate], limits: DiversityLimits) -> Vec<RankedCandidate> {
        rank_candidates(candidates, limits).expect("ranking succeeds")
    }

    #[test]
    fn ranking_normalizes_within_channels_not_across_raw_store_scales() {
        let first = vec![
            candidate("lex-a", CandidateChannel::Lexical, 20, None),
            candidate("lex-b", CandidateChannel::Lexical, 10, None),
            candidate("sum-a", CandidateChannel::Summary, 2_000, None),
            candidate("sum-b", CandidateChannel::Summary, 1_000, None),
        ];
        let scaled = vec![
            candidate("lex-a", CandidateChannel::Lexical, 2_000_000, None),
            candidate("lex-b", CandidateChannel::Lexical, 1_000_000, None),
            candidate("sum-a", CandidateChannel::Summary, 2, None),
            candidate("sum-b", CandidateChannel::Summary, 1, None),
        ];

        let first_ids = rank(&first, DiversityLimits::unbounded())
            .into_iter()
            .map(|item| item.stable_id)
            .collect::<Vec<_>>();
        let scaled_ids = rank(&scaled, DiversityLimits::unbounded())
            .into_iter()
            .map(|item| item.stable_id)
            .collect::<Vec<_>>();

        assert_eq!(first_ids, scaled_ids);
    }

    #[test]
    fn ranking_uses_stable_tie_breaks_and_logical_diversity() {
        let candidates = vec![
            candidate("b", CandidateChannel::Lexical, 10, Some("same")),
            candidate("a", CandidateChannel::Lexical, 10, Some("same")),
            candidate("c", CandidateChannel::Lexical, 9, Some("other")),
        ];
        let ranked = rank(
            &candidates,
            DiversityLimits {
                per_logical_message: 1,
                ..DiversityLimits::unbounded()
            },
        );

        assert_eq!(ids(&ranked), vec!["a", "c"]);
    }

    #[test]
    fn exact_phrase_channel_precedes_lexical_at_equal_channel_rank() {
        let ranked = rank(
            &[
                candidate("lexical", CandidateChannel::Lexical, 100, None),
                candidate("phrase", CandidateChannel::Phrase, 1, None),
            ],
            DiversityLimits::unbounded(),
        );

        assert_eq!(ranked[0].stable_id, "phrase");
    }

    #[test]
    fn ranking_never_compares_raw_scores_from_different_sources() {
        let mut source_a = candidate("a", CandidateChannel::Lexical, 1, None);
        source_a.source = Some("source-a".to_string());
        let mut source_b = candidate("b", CandidateChannel::Lexical, 10_000, None);
        source_b.source = Some("source-b".to_string());
        let first = rank(
            &[source_a.clone(), source_b.clone()],
            DiversityLimits::unbounded(),
        );

        source_a.raw_score = 1_000_000;
        source_b.raw_score = 1;
        let rescaled = rank(&[source_a, source_b], DiversityLimits::unbounded());

        assert_eq!(ids(&first), ids(&rescaled));
    }

    #[test]
    fn exact_message_tier_cannot_be_displaced_by_any_number_of_approximate_channels() {
        let mut approximate = Vec::new();
        for index in 0..32 {
            let mut hit = candidate(
                &format!("approx-lexical-{index}"),
                CandidateChannel::Lexical,
                10_000 - index,
                None,
            );
            hit.source = Some(format!("src-{index}"));
            approximate.push(hit);
            let mut entity = candidate(
                &format!("approx-entity-{index}"),
                CandidateChannel::Entity,
                9_000 - index,
                None,
            );
            entity.source = Some(format!("ent-{index}"));
            approximate.push(entity);
            let mut summary = candidate(
                &format!("approx-summary-{index}"),
                CandidateChannel::Summary,
                8_000 - index,
                None,
            );
            summary.source = Some(format!("sum-{index}"));
            approximate.push(summary);
        }
        approximate.push(candidate(
            "exact-msg",
            CandidateChannel::ExactMessage,
            1,
            None,
        ));

        let ranked = rank(&approximate, DiversityLimits::unbounded());
        assert_eq!(ranked[0].stable_id, "exact-msg");
        assert!(
            ranked
                .iter()
                .skip(1)
                .all(|candidate| ranked[0].normalized_score_micros
                    > candidate.normalized_score_micros)
        );
    }

    #[test]
    fn exact_phrase_tier_cannot_be_displaced_by_multichannel_approximate_inversion() {
        let mut stacked = vec![
            candidate("exact-phrase", CandidateChannel::Phrase, 1, None),
            candidate("stacked", CandidateChannel::Lexical, 1_000, None),
            candidate("stacked", CandidateChannel::Summary, 1_000, None),
            candidate("stacked", CandidateChannel::Entity, 1_000, None),
            candidate("stacked", CandidateChannel::Time, 1_000, None),
        ];
        for index in 0..16 {
            let mut extra = candidate(
                &format!("approx-{index}"),
                CandidateChannel::Lexical,
                500 - index,
                None,
            );
            extra.source = Some(format!("shard-{index}"));
            stacked.push(extra);
        }

        let ranked = rank(&stacked, DiversityLimits::unbounded());
        assert_eq!(ranked[0].stable_id, "exact-phrase");
    }

    #[test]
    fn ranking_does_not_sum_uncalibrated_channel_weights_across_channels() {
        let fused_same_id = rank(
            &[
                candidate("same", CandidateChannel::Lexical, 100, None),
                candidate("same", CandidateChannel::Summary, 100, None),
                candidate("same", CandidateChannel::Entity, 100, None),
            ],
            DiversityLimits::unbounded(),
        );
        let single_best = rank(
            &[candidate("same", CandidateChannel::Entity, 100, None)],
            DiversityLimits::unbounded(),
        );

        assert_eq!(fused_same_id.len(), 1);
        assert_eq!(
            fused_same_id[0].normalized_score_micros, single_best[0].normalized_score_micros,
            "multi-channel hits must not accumulate uncalibrated weight sums"
        );
    }

    #[test]
    fn exact_message_outranks_exact_phrase_and_phrase_outranks_approximate() {
        let ranked = rank(
            &[
                candidate("approx", CandidateChannel::Entity, 1_000, None),
                candidate("phrase", CandidateChannel::Phrase, 1, None),
                candidate("message", CandidateChannel::ExactMessage, 1, None),
            ],
            DiversityLimits::unbounded(),
        );
        assert_eq!(ids(&ranked), vec!["message", "phrase", "approx"]);
    }

    #[test]
    fn duplicate_stable_id_with_conflicting_metadata_returns_typed_error() {
        let mut left = candidate("dup", CandidateChannel::Lexical, 10, Some("msg-a"));
        left.turn = Some("turn-a".to_string());
        let mut right = candidate("dup", CandidateChannel::Summary, 10, Some("msg-b"));
        right.turn = Some("turn-b".to_string());

        let err = rank_candidates(&[left, right], DiversityLimits::unbounded())
            .expect_err("conflicting metadata must not silently merge");
        assert_eq!(
            err,
            RankingError::ConflictingDuplicateMetadata {
                stable_id: "dup".to_string(),
            }
        );
    }

    #[test]
    #[allow(deprecated)]
    fn duplicate_stable_id_with_source_only_conflict_errors_without_compatibility_selection() {
        let mut left = candidate("dup", CandidateChannel::ExactMessage, 10, Some("msg"));
        left.evidence_role = Some("producer".to_string());
        left.source = Some("source-a".to_string());
        let mut right = left.clone();
        right.source = Some("source-b".to_string());

        let rank_err =
            rank_candidates(&[left.clone(), right.clone()], DiversityLimits::unbounded())
                .expect_err("the canonical API must propagate source conflicts");
        let compatibility_err = try_rank_candidates(&[right, left], DiversityLimits::unbounded())
            .expect_err("the compatibility API must propagate source conflicts");
        assert_eq!(rank_err, compatibility_err);
        assert_eq!(
            rank_err,
            RankingError::ConflictingDuplicateMetadata {
                stable_id: "dup".to_string(),
            }
        );
    }

    #[test]
    fn same_channel_duplicate_scores_require_the_same_calibrated_source() {
        let mut unscoped = candidate("dup", CandidateChannel::Lexical, i64::MAX, Some("msg"));
        unscoped.source = None;
        let mut scoped = unscoped.clone();
        scoped.raw_score = i64::MIN;
        scoped.source = Some("store-a".to_string());

        let err = rank_candidates(&[unscoped, scoped], DiversityLimits::unbounded())
            .expect_err("raw scores from distinct source domains are incomparable");
        assert_eq!(
            err,
            RankingError::ConflictingDuplicateMetadata {
                stable_id: "dup".to_string(),
            }
        );
    }

    #[test]
    fn duplicate_multiplicity_does_not_change_unrelated_scores_or_final_order() {
        let unique = vec![
            candidate("a", CandidateChannel::Lexical, 100, Some("a")),
            candidate("b", CandidateChannel::Lexical, 90, Some("b")),
            candidate("c", CandidateChannel::Lexical, 80, Some("c")),
        ];
        let baseline = rank(&unique, DiversityLimits::unbounded());

        let mut duplicated = unique.clone();
        duplicated.extend(std::iter::repeat_n(unique[1].clone(), 4));
        let with_duplicates = rank(&duplicated, DiversityLimits::unbounded());

        assert_eq!(ids(&with_duplicates), ids(&baseline));
        for stable_id in ["a", "c"] {
            let baseline_score = baseline
                .iter()
                .find(|candidate| candidate.stable_id == stable_id)
                .expect("baseline candidate")
                .normalized_score_micros;
            let duplicate_score = with_duplicates
                .iter()
                .find(|candidate| candidate.stable_id == stable_id)
                .expect("candidate after duplicate collapse")
                .normalized_score_micros;
            assert_eq!(
                duplicate_score, baseline_score,
                "duplicate multiplicity changed unrelated candidate {stable_id}"
            );
        }
    }

    #[test]
    fn same_stable_id_fuses_valid_evidence_across_distinct_channels() {
        let mut lexical = candidate("same", CandidateChannel::Lexical, 100, Some("msg"));
        lexical.evidence_role = Some("producer".to_string());
        let mut phrase = lexical.clone();
        phrase.channel = CandidateChannel::Phrase;
        phrase.raw_score = 1;

        let ranked = rank(&[lexical, phrase], DiversityLimits::unbounded());

        assert_eq!(ranked.len(), 1);
        assert_eq!(ranked[0].stable_id, "same");
        assert_eq!(ranked[0].evidence_role.as_deref(), Some("producer"));
        assert_eq!(
            ranked[0]
                .contributions
                .iter()
                .map(|contribution| contribution.channel)
                .collect::<Vec<_>>(),
            [CandidateChannel::Phrase, CandidateChannel::Lexical]
        );
        assert_eq!(
            ranked[0]
                .contributions
                .iter()
                .map(|contribution| contribution.raw_score)
                .collect::<Vec<_>>(),
            [1, 100]
        );
        assert!(
            ranked[0].normalized_score_micros >= encode_score(RankTier::ExactPhrase, 0),
            "the strongest distinct channel must survive fusion"
        );
    }

    #[test]
    fn exact_message_preserves_producer_evidence_metadata() {
        let mut exact = candidate(
            "exact-producer",
            CandidateChannel::ExactMessage,
            i64::MIN,
            Some("exact message"),
        );
        exact.evidence_role = Some("producer".to_string());
        exact.source = Some("cursor".to_string());
        exact.exact_ranges = vec![ByteRangeV1::new(7, 19).expect("exact byte range")];
        let approximate = candidate(
            "approximate-neighbor",
            CandidateChannel::Lexical,
            i64::MAX,
            None,
        );

        let ranked = rank(&[approximate, exact], DiversityLimits::unbounded());

        assert_eq!(ranked.len(), 2);
        assert_eq!(ranked[0].stable_id, "exact-producer");
        assert_eq!(ranked[0].logical_message.as_deref(), Some("exact message"));
        assert_eq!(ranked[0].evidence_role.as_deref(), Some("producer"));
        assert_eq!(ranked[0].source.as_deref(), Some("cursor"));
        assert_eq!(
            ranked[0].contributions[0].exact_ranges,
            [ByteRangeV1::new(7, 19).expect("exact byte range")]
        );
        assert!(ranked[0].normalized_score_micros >= encode_score(RankTier::ExactMessage, 0));
    }

    #[test]
    fn repeated_exact_occurrence_ranges_fuse_deterministically() {
        let mut first = candidate("same", CandidateChannel::ExactMessage, 1, None);
        first.retriever_record_id = "occurrence-1".to_string();
        first.exact_ranges = vec![
            ByteRangeV1::new(8, 12).expect("second range"),
            ByteRangeV1::new(1, 5).expect("first range"),
        ];
        let mut duplicate = first.clone();
        duplicate.exact_ranges = vec![ByteRangeV1::new(1, 5).expect("duplicate range")];

        let forward = rank(
            &[first.clone(), duplicate.clone()],
            DiversityLimits::unbounded(),
        );
        let reversed = rank(&[duplicate, first], DiversityLimits::unbounded());

        assert_eq!(forward, reversed);
        assert_eq!(
            forward[0].contributions[0].exact_ranges,
            [
                ByteRangeV1::new(1, 5).expect("first range"),
                ByteRangeV1::new(8, 12).expect("second range"),
            ]
        );
    }

    #[test]
    fn duplicate_stable_id_compatible_metadata_merges_without_first_partition_inheritance() {
        let mut lexical = candidate("dup", CandidateChannel::Lexical, 10, None);
        lexical.logical_message = None;
        lexical.turn = Some("turn-1".to_string());
        let mut phrase = candidate("dup", CandidateChannel::Phrase, 1, Some("msg-1"));
        phrase.turn = None;

        let ranked = rank(&[lexical, phrase], DiversityLimits::unbounded());
        assert_eq!(ranked.len(), 1);
        assert_eq!(ranked[0].logical_message.as_deref(), Some("msg-1"));
        assert_eq!(ranked[0].turn.as_deref(), Some("turn-1"));
        assert!(ranked[0].normalized_score_micros >= encode_score(RankTier::ExactPhrase, 0));
    }

    #[test]
    #[allow(deprecated)]
    fn conflicting_duplicate_metadata_propagates_through_every_public_api() {
        let mut left = candidate("dup", CandidateChannel::Lexical, 10, Some("zzz"));
        left.source = Some("source-z".to_string());
        let mut right = candidate("dup", CandidateChannel::Lexical, 9, Some("aaa"));
        right.source = Some("source-a".to_string());

        let canonical =
            rank_candidates(&[left.clone(), right.clone()], DiversityLimits::unbounded())
                .expect_err("canonical API must reject conflicting metadata");
        let compatibility = try_rank_candidates(&[right, left], DiversityLimits::unbounded())
            .expect_err("compatibility API must reject conflicting metadata");
        assert_eq!(canonical, compatibility);
        assert_eq!(
            canonical,
            RankingError::ConflictingDuplicateMetadata {
                stable_id: "dup".to_string(),
            }
        );
    }

    #[test]
    fn exact_tiers_are_disjoint_and_scores_are_finitely_bounded() {
        let ranked = rank(
            &[
                candidate("approx", CandidateChannel::Lexical, i64::MAX, None),
                candidate("phrase", CandidateChannel::Phrase, i64::MIN, None),
                candidate("message", CandidateChannel::ExactMessage, i64::MIN, None),
            ],
            DiversityLimits::unbounded(),
        );

        let score = |stable_id| {
            ranked
                .iter()
                .find(|candidate| candidate.stable_id == stable_id)
                .expect("ranked candidate")
                .normalized_score_micros
        };
        assert!((TIER_SPAN..(2 * TIER_SPAN)).contains(&score("approx")));
        assert!(((2 * TIER_SPAN)..(3 * TIER_SPAN)).contains(&score("phrase")));
        assert!(((3 * TIER_SPAN)..(4 * TIER_SPAN)).contains(&score("message")));
        assert!(
            ranked
                .iter()
                .all(|candidate| { candidate.normalized_score_micros < 4 * TIER_SPAN })
        );
    }

    #[test]
    fn ranking_ordering_is_deterministic_under_input_permutation() {
        let mut candidates = vec![
            candidate("c", CandidateChannel::Summary, 3, Some("c")),
            candidate("a", CandidateChannel::Lexical, 10, Some("a")),
            candidate("b", CandidateChannel::Phrase, 1, Some("b")),
            candidate("d", CandidateChannel::ExactMessage, 1, Some("d")),
        ];
        let baseline = rank(&candidates, DiversityLimits::unbounded());
        candidates.reverse();
        let reversed = rank(&candidates, DiversityLimits::unbounded());
        assert_eq!(baseline, reversed);
        assert_eq!(ids(&baseline), vec!["d", "b", "a", "c"]);
    }

    #[test]
    fn absent_source_partitions_do_not_collide_with_nul_prefixed_source_strings() {
        let mut absent = candidate("b", CandidateChannel::Lexical, 1, Some("b"));
        absent.source = None;
        let mut colliding = candidate("a", CandidateChannel::Lexical, 100, Some("a"));
        colliding.source = Some("\0b".to_string());

        let ranked = rank(&[absent, colliding], DiversityLimits::unbounded());
        assert_eq!(ids(&ranked), vec!["a", "b"]);
        assert_eq!(
            ranked[0].normalized_score_micros, ranked[1].normalized_score_micros,
            "singleton absent/present partitions must not share a raw-score denominator"
        );
    }

    #[test]
    fn absent_and_present_sources_for_same_stable_id_conflict() {
        let mut absent = candidate("dup", CandidateChannel::Lexical, 10, Some("msg"));
        absent.source = None;
        let mut present = absent.clone();
        present.channel = CandidateChannel::Phrase;
        present.source = Some("store-a".to_string());

        let err = rank_candidates(&[absent, present], DiversityLimits::unbounded())
            .expect_err("Absent vs Present source must not fuse");
        assert_eq!(
            err,
            RankingError::ConflictingDuplicateMetadata {
                stable_id: "dup".to_string(),
            }
        );
    }

    #[test]
    fn conflicting_knowledge_timestamps_are_rejected_as_metadata_conflicts() {
        let mut left = candidate("dup", CandidateChannel::Lexical, 10, Some("msg"));
        left.knowledge_at_micros = 10;
        let mut right = candidate("dup", CandidateChannel::Summary, 10, Some("msg"));
        right.knowledge_at_micros = 20;

        let err = rank_candidates(&[left, right], DiversityLimits::unbounded())
            .expect_err("knowledge_at is ranking metadata and must not silently max-merge");
        assert_eq!(
            err,
            RankingError::ConflictingDuplicateMetadata {
                stable_id: "dup".to_string(),
            }
        );
    }

    #[test]
    fn ranking_ties_use_newest_knowledge_then_stable_id() {
        let mut newer_b = candidate("b", CandidateChannel::Lexical, 10, Some("b"));
        newer_b.knowledge_at_micros = 20;
        let mut older_a = candidate("a", CandidateChannel::Lexical, 10, Some("a"));
        older_a.knowledge_at_micros = 10;
        let mut newer_c = candidate("c", CandidateChannel::Lexical, 10, Some("c"));
        newer_c.knowledge_at_micros = 20;
        let ranked = rank(
            &[newer_b.clone(), older_a.clone(), newer_c.clone()],
            DiversityLimits::unbounded(),
        );
        assert_eq!(ids(&ranked), vec!["b", "c", "a"]);

        // Force equal normalized scores via singleton partitions.
        newer_b.source = Some("src-b".to_string());
        older_a.source = Some("src-a".to_string());
        newer_c.source = Some("src-c".to_string());

        let ranked = rank(&[newer_b, older_a, newer_c], DiversityLimits::unbounded());
        assert_eq!(ids(&ranked), vec!["b", "c", "a"]);
    }

    #[test]
    fn diversity_limits_enforce_every_dimension_independently() {
        let mk = |stable_id: &str,
                  logical: &str,
                  turn: &str,
                  session: &str,
                  source: &str,
                  role: &str| {
            let mut hit = candidate(stable_id, CandidateChannel::Lexical, 10, Some(logical));
            hit.turn = Some(turn.to_string());
            hit.session = Some(session.to_string());
            hit.source = Some(source.to_string());
            hit.evidence_role = Some(role.to_string());
            hit
        };
        let cases = [
            (
                "logical_message",
                DiversityLimits {
                    per_logical_message: 1,
                    ..DiversityLimits::unbounded()
                },
                mk("a", "shared", "t1", "s1", "src1", "r1"),
                mk("b", "shared", "t2", "s2", "src2", "r2"),
            ),
            (
                "turn",
                DiversityLimits {
                    per_turn: 1,
                    ..DiversityLimits::unbounded()
                },
                mk("a", "m1", "shared", "s1", "src1", "r1"),
                mk("b", "m2", "shared", "s2", "src2", "r2"),
            ),
            (
                "session",
                DiversityLimits {
                    per_session: 1,
                    ..DiversityLimits::unbounded()
                },
                mk("a", "m1", "t1", "shared", "src1", "r1"),
                mk("b", "m2", "t2", "shared", "src2", "r2"),
            ),
            (
                "source",
                DiversityLimits {
                    per_source: 1,
                    ..DiversityLimits::unbounded()
                },
                mk("a", "m1", "t1", "s1", "shared", "r1"),
                mk("b", "m2", "t2", "s2", "shared", "r2"),
            ),
            (
                "evidence_role",
                DiversityLimits {
                    per_evidence_role: 1,
                    ..DiversityLimits::unbounded()
                },
                mk("a", "m1", "t1", "s1", "src1", "shared"),
                mk("b", "m2", "t2", "s2", "src2", "shared"),
            ),
        ];
        for (dimension, limits, first, second) in cases {
            let ranked = rank(&[first, second], limits);
            assert_eq!(
                ids(&ranked),
                vec!["a"],
                "{dimension} diversity limit must drop the later duplicate"
            );
        }
    }

    #[test]
    fn fused_ranking_is_permutation_invariant_with_partial_metadata() {
        let mut lexical = candidate("same", CandidateChannel::Lexical, 50, None);
        lexical.turn = Some("turn-1".to_string());
        lexical.logical_message = None;
        let mut phrase = candidate("same", CandidateChannel::Phrase, 1, Some("msg-1"));
        phrase.turn = None;
        let other = candidate("other", CandidateChannel::Summary, 10, Some("other"));

        let mut forward = vec![lexical.clone(), phrase.clone(), other.clone()];
        let baseline = rank(&forward, DiversityLimits::unbounded());
        forward.reverse();
        let reversed = rank(&forward, DiversityLimits::unbounded());
        assert_eq!(baseline, reversed);
        assert_eq!(ids(&baseline), vec!["same", "other"]);
        assert_eq!(baseline[0].logical_message.as_deref(), Some("msg-1"));
        assert_eq!(baseline[0].turn.as_deref(), Some("turn-1"));
        assert!(baseline[0].normalized_score_micros >= encode_score(RankTier::ExactPhrase, 0));
    }
}
