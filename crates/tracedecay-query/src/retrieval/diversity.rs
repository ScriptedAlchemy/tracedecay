//! Deterministic diversity-cap stage contracts (Plan 15 pipeline step 9:
//! profile-owned caps per source namespace, source instance, repository,
//! session/thread, logical-copy cluster, and evidence role apply after
//! fusion; a cap must carry its locked evaluation anchor — absent evidence
//! leaves the cap disabled except resource-safety ceilings).

use std::collections::{BTreeMap, BTreeSet};

use thiserror::Error;
use tracedecay_domain::{
    DiversityPolicy, EvidenceRole, ExactClass, FileOccurrenceId, FusedCandidate,
    LogicalCopyClusterId, OccurrenceProvenance, RankedCandidate, RankingDecision,
    RankingDecisionKind, RepositoryId, SessionOrThreadId, SourceInstanceKey, SourceNamespace,
};

use super::stage_counters;

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum DiversityStageError {
    #[error("an enabled diversity cap lacks its locked evaluation anchor")]
    CapWithoutEvidenceAnchor,
    #[error("contract violation: {0}")]
    Contract(String),
}

/// One recorded diversity-cap decision (Plan 15: `RankingDecision` records
/// each diversity-cap decision).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiversityDecisionV1 {
    pub capped: Vec<tracedecay_domain::RetrievalAnchorId>,
    pub decision: RankingDecision,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DeterministicDiversity;

impl DeterministicDiversity {
    #[hotpath::measure(label = "query.diversity.apply")]
    pub fn apply_caps(
        &self,
        policy: &DiversityPolicy,
        candidates: Vec<FusedCandidate>,
    ) -> Result<(Vec<RankedCandidate>, Vec<DiversityDecisionV1>), DiversityStageError> {
        self.apply_caps_preserving(policy, candidates, &[])
    }

    pub(crate) fn apply_caps_preserving(
        &self,
        policy: &DiversityPolicy,
        candidates: Vec<FusedCandidate>,
        incumbents: &[RankedCandidate],
    ) -> Result<(Vec<RankedCandidate>, Vec<DiversityDecisionV1>), DiversityStageError> {
        let candidate_count = candidates.len();
        let enabled = [
            policy.per_source_namespace,
            policy.per_source_instance,
            policy.per_repository,
            policy.per_file,
            policy.per_session_or_thread,
            policy.per_copy_cluster,
            policy.per_evidence_role,
        ]
        .into_iter()
        .any(|cap| cap.is_some());
        if enabled && policy.evaluation_result_anchor.is_none() {
            return Err(DiversityStageError::CapWithoutEvidenceAnchor);
        }
        // Absent caps are a no-op over candidate data: the fused order is the
        // final order, and no key or counter is ever derived.
        let (admitted, decisions) = if enabled {
            cap_candidates(policy, candidates, incumbents)
        } else {
            (candidates, Vec::new())
        };
        let ranked = admitted
            .into_iter()
            .enumerate()
            .map(|(ordinal, candidate)| RankedCandidate {
                candidate,
                final_ordinal: ordinal as u32,
            })
            .collect::<Vec<_>>();
        hotpath::gauge!("query.diversity.candidates").set(candidate_count);
        hotpath::gauge!("query.diversity.results").set(ranked.len());
        hotpath::gauge!("query.diversity.capped").set(decisions.len());
        Ok((ranked, decisions))
    }
}

/// Apply at least one enabled cap, deriving each unprotected candidate's keys
/// once and reusing them for both the reached-cap check and the admission
/// counters.
fn cap_candidates(
    policy: &DiversityPolicy,
    candidates: Vec<FusedCandidate>,
    incumbents: &[RankedCandidate],
) -> (Vec<FusedCandidate>, Vec<DiversityDecisionV1>) {
    let mut counters = CapCounters::default();
    // An optional lane may change fused rank, but it cannot retroactively
    // take a bounded slot already granted by the canonical fallback. Reserve
    // those slots from the newly fused candidates so their current scores,
    // occurrences, and comparator order remain authoritative.
    let incumbent_ids = incumbents
        .iter()
        .map(|ranked| {
            (
                ranked.candidate.anchor_id.clone(),
                ranked.candidate.logical_evidence_id.clone(),
            )
        })
        .collect::<BTreeSet<_>>();
    for candidate in &candidates {
        if incumbent_ids.contains(&(
            candidate.anchor_id.clone(),
            candidate.logical_evidence_id.clone(),
        )) && !is_protected(candidate)
        {
            counters.admit(&CandidateCapKeys::derive(policy, candidate));
        }
    }
    let mut admitted = Vec::with_capacity(candidates.len());
    let mut decisions = Vec::new();
    for candidate in candidates {
        // Exact and contradiction evidence is protected: diversity may
        // neither demote exact technical lookup nor erase an admitted
        // contradiction, and protected evidence never counts against a cap.
        if is_protected(&candidate) {
            admitted.push(candidate);
            continue;
        }
        if incumbent_ids.contains(&(
            candidate.anchor_id.clone(),
            candidate.logical_evidence_id.clone(),
        )) {
            admitted.push(candidate);
            continue;
        }
        let keys = CandidateCapKeys::derive(policy, &candidate);
        let dimensions = counters.reached_caps(policy, &keys);
        if dimensions.is_empty() {
            counters.admit(&keys);
            admitted.push(candidate);
            continue;
        }

        let decision = RankingDecision {
            kind: RankingDecisionKind::DiversityCap,
            retriever: None,
            policy_anchor: policy.evaluation_result_anchor.clone(),
            evidence_anchor: candidate
                .occurrences
                .first()
                .map(|occurrence| occurrence.retriever_evidence_anchor.clone()),
            detail: format!("capped by {}", dimensions.join(",")),
        };
        decisions.push(DiversityDecisionV1 {
            capped: vec![candidate.anchor_id],
            decision,
        });
    }
    (admitted, decisions)
}

fn is_protected(candidate: &FusedCandidate) -> bool {
    candidate.exact_class != ExactClass::Approximate
        || candidate
            .occurrences
            .iter()
            .any(|occurrence| occurrence.evidence_role == EvidenceRole::Contradiction)
}

#[derive(Default)]
struct CapCounters {
    namespaces: BTreeMap<SourceNamespace, u32>,
    instances: BTreeMap<SourceInstanceKey, u32>,
    repositories: BTreeMap<RepositoryId, u32>,
    files: BTreeMap<FileOccurrenceId, u32>,
    sessions: BTreeMap<SessionOrThreadId, u32>,
    copy_clusters: BTreeMap<LogicalCopyClusterId, u32>,
    evidence_roles: BTreeMap<EvidenceRole, u32>,
}

impl CapCounters {
    fn reached_caps(
        &self,
        policy: &DiversityPolicy,
        keys: &CandidateCapKeys<'_>,
    ) -> Vec<&'static str> {
        let mut dimensions = Vec::new();
        if any_reached(
            &self.namespaces,
            &keys.namespaces,
            policy.per_source_namespace,
        ) {
            dimensions.push("source_namespace");
        }
        if any_reached(&self.instances, &keys.instances, policy.per_source_instance) {
            dimensions.push("source_instance");
        }
        if any_reached(
            &self.repositories,
            &keys.repositories,
            policy.per_repository,
        ) {
            dimensions.push("repository");
        }
        if any_reached(&self.files, &keys.files, policy.per_file) {
            dimensions.push("file");
        }
        if any_reached(&self.sessions, &keys.sessions, policy.per_session_or_thread) {
            dimensions.push("session_or_thread");
        }
        if any_reached(
            &self.copy_clusters,
            &keys.copy_clusters,
            policy.per_copy_cluster,
        ) {
            dimensions.push("logical_copy_cluster");
        }
        if any_reached(
            &self.evidence_roles,
            &keys.evidence_roles,
            policy.per_evidence_role,
        ) {
            dimensions.push("evidence_role");
        }
        dimensions
    }

    fn admit(&mut self, keys: &CandidateCapKeys<'_>) {
        increment_all(&mut self.namespaces, &keys.namespaces);
        increment_all(&mut self.instances, &keys.instances);
        increment_all(&mut self.repositories, &keys.repositories);
        increment_all(&mut self.files, &keys.files);
        increment_all(&mut self.sessions, &keys.sessions);
        increment_all(&mut self.copy_clusters, &keys.copy_clusters);
        increment_all(&mut self.evidence_roles, &keys.evidence_roles);
    }
}

/// The distinct cap keys of one candidate, borrowed from its occurrences and
/// collected only for the dimensions the policy enables. A disabled
/// dimension stays empty, so it costs neither a clone nor a counter entry.
struct CandidateCapKeys<'a> {
    namespaces: BTreeSet<&'a SourceNamespace>,
    instances: BTreeSet<&'a SourceInstanceKey>,
    repositories: BTreeSet<&'a RepositoryId>,
    files: BTreeSet<&'a FileOccurrenceId>,
    sessions: BTreeSet<&'a SessionOrThreadId>,
    copy_clusters: BTreeSet<&'a LogicalCopyClusterId>,
    evidence_roles: BTreeSet<&'a EvidenceRole>,
}

impl<'a> CandidateCapKeys<'a> {
    fn derive(policy: &DiversityPolicy, candidate: &'a FusedCandidate) -> Self {
        stage_counters::record_cap_key_derivation();
        let occurrences = &candidate.occurrences;
        Self {
            namespaces: enabled_keys(policy.per_source_namespace, occurrences, |occurrence| {
                Some(&occurrence.source_namespace)
            }),
            instances: enabled_keys(policy.per_source_instance, occurrences, |occurrence| {
                Some(&occurrence.freshness.source_instance)
            }),
            repositories: enabled_keys(policy.per_repository, occurrences, |occurrence| {
                occurrence.repository_id.as_ref()
            }),
            files: enabled_keys(policy.per_file, occurrences, |occurrence| {
                occurrence.file_occurrence_id.as_ref()
            }),
            sessions: enabled_keys(policy.per_session_or_thread, occurrences, |occurrence| {
                occurrence.session_or_thread_id.as_ref()
            }),
            copy_clusters: enabled_keys(policy.per_copy_cluster, occurrences, |occurrence| {
                occurrence.logical_copy_cluster_id.as_ref()
            }),
            evidence_roles: enabled_keys(policy.per_evidence_role, occurrences, |occurrence| {
                Some(&occurrence.evidence_role)
            }),
        }
    }
}

fn enabled_keys<'a, K: Ord>(
    cap: Option<u32>,
    occurrences: &'a [OccurrenceProvenance],
    key: impl Fn(&'a OccurrenceProvenance) -> Option<&'a K>,
) -> BTreeSet<&'a K> {
    if cap.is_none() {
        return BTreeSet::new();
    }
    occurrences.iter().filter_map(key).collect()
}

fn any_reached<K: Ord>(counts: &BTreeMap<K, u32>, keys: &BTreeSet<&K>, cap: Option<u32>) -> bool {
    cap.is_some_and(|cap| {
        keys.iter()
            .any(|key| counts.get(*key).copied().unwrap_or(0) >= cap)
    })
}

/// Count each distinct key once per admitted candidate, owning a key only
/// the first time a counter needs it.
fn increment_all<K: Ord + Clone>(counts: &mut BTreeMap<K, u32>, keys: &BTreeSet<&K>) {
    for key in keys {
        match counts.get_mut(*key) {
            Some(count) => *count += 1,
            None => {
                counts.insert((*key).clone(), 1);
            }
        }
    }
}

#[cfg(test)]
mod cap_key_tests {
    use super::*;
    use tracedecay_domain::{
        FreshnessCompatibilityV1, RetrievalAnchorId, SourceFreshness, UtcMicros,
    };

    fn id<T>(value: &str) -> T
    where
        T: TryFrom<String>,
        <T as TryFrom<String>>::Error: std::fmt::Debug,
    {
        T::try_from(value.to_owned()).expect("valid fixture identity")
    }

    fn occurrence(name: &str, file: &str) -> OccurrenceProvenance {
        OccurrenceProvenance {
            source_occurrence_id: id(&format!("occurrence.{name}")),
            file_occurrence_id: Some(id(file)),
            retriever_evidence_anchor: RetrievalAnchorId::new(format!("evidence.{name}"))
                .expect("valid evidence anchor"),
            source_namespace: id("namespace.code"),
            repository_id: Some(id("repository.fixture")),
            session_or_thread_id: Some(id(&format!("session.{name}"))),
            logical_copy_cluster_id: Some(id("copy.fixture")),
            logical_copy_evidence_anchor: Some(
                RetrievalAnchorId::new("copy-evidence.fixture").expect("valid copy anchor"),
            ),
            evidence_role: EvidenceRole::Primary,
            freshness: SourceFreshness {
                source_namespace: id("namespace.code"),
                source_instance: id(&format!("instance.{name}")),
                source_watermark: Some(7),
                projection_watermark: Some(7),
                observed_at: UtcMicros(7),
                source_generation: Some(1),
                generation_lag: Some(0),
                compatibility: FreshnessCompatibilityV1::Current,
                policy_revision: id("policy.v1"),
            },
        }
    }

    fn candidate() -> FusedCandidate {
        FusedCandidate {
            anchor_id: RetrievalAnchorId::new("anchor.fixture").expect("valid anchor"),
            logical_evidence_id: id("logical.fixture"),
            occurrences: vec![
                occurrence("a", "file.shared"),
                occurrence("b", "file.shared"),
                occurrence("c", "file.other"),
            ],
            exact_class: ExactClass::Approximate,
            utility_micros: 0,
            contributions: Vec::new(),
            freshness: Vec::new(),
            decisions: Vec::new(),
        }
    }

    fn policy(per_file: Option<u32>) -> DiversityPolicy {
        DiversityPolicy {
            policy_id: id("diversity.fixture.v1"),
            evaluation_result_anchor: Some(
                RetrievalAnchorId::new("evaluation.fixture").expect("valid anchor"),
            ),
            per_source_namespace: None,
            per_source_instance: None,
            per_repository: None,
            per_file,
            per_session_or_thread: None,
            per_copy_cluster: None,
            per_evidence_role: None,
        }
    }

    #[test]
    fn only_enabled_dimensions_collect_keys_and_repeats_count_once() {
        let candidate = candidate();
        let keys = CandidateCapKeys::derive(&policy(Some(2)), &candidate);

        // Two occurrences share a file: the candidate contributes one
        // distinct file key, and the disabled dimensions collect nothing
        // even though every occurrence carries a value for them.
        assert_eq!(
            keys.files
                .iter()
                .map(|file| file.as_str())
                .collect::<Vec<_>>(),
            vec!["file.other", "file.shared"]
        );
        assert!(keys.namespaces.is_empty());
        assert!(keys.instances.is_empty());
        assert!(keys.repositories.is_empty());
        assert!(keys.sessions.is_empty());
        assert!(keys.copy_clusters.is_empty());
        assert!(keys.evidence_roles.is_empty());

        let mut counters = CapCounters::default();
        counters.admit(&keys);
        assert_eq!(counters.files.len(), 2);
        assert!(counters.files.values().all(|count| *count == 1));
        assert!(counters.namespaces.is_empty());
        assert!(counters.sessions.is_empty());
    }

    #[test]
    fn enabled_cap_without_evaluation_anchor_is_refused_before_any_work() {
        let mut policy = policy(Some(1));
        policy.evaluation_result_anchor = None;
        stage_counters::reset();
        let error = DeterministicDiversity
            .apply_caps(&policy, vec![candidate()])
            .expect_err("an enabled cap needs its evaluation anchor");
        assert_eq!(error, DiversityStageError::CapWithoutEvidenceAnchor);
        assert_eq!(stage_counters::snapshot().cap_key_derivations, 0);
    }
}
