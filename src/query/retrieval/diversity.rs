//! Deterministic diversity-cap stage contracts (Plan 15 pipeline step 9:
//! profile-owned caps per source namespace, source instance, repository,
//! session/thread, logical-copy cluster, and evidence role apply after
//! fusion; a cap must carry its locked evaluation anchor — absent evidence
//! leaves the cap disabled except resource-safety ceilings).

use std::collections::{BTreeMap, BTreeSet};

use thiserror::Error;
use tracedecay_domain::{
    DiversityPolicy, EvidenceRole, ExactClass, FileOccurrenceId, FusedCandidate,
    LogicalCopyClusterId, RankedCandidate, RankingDecision, RankingDecisionKind, RepositoryId,
    SessionOrThreadId, SourceInstanceKey, SourceNamespace,
};

/// Failures of the diversity stage.
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

/// The deterministic diversity-cap stage contract. Caps apply after fusion
/// and preserve the fused total order of the survivors.
pub trait DiversityCapStage {
    /// Apply `policy` to an ordered fused candidate list, recording one
    /// decision per cap application. Disabled caps (no evaluation anchor)
    /// apply only as resource-safety ceilings.
    fn apply_caps(
        &self,
        policy: &DiversityPolicy,
        candidates: Vec<FusedCandidate>,
    ) -> Result<(Vec<RankedCandidate>, Vec<DiversityDecisionV1>), DiversityStageError>;
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DeterministicDiversity;

impl DeterministicDiversity {
    pub fn apply_caps(
        &self,
        policy: &DiversityPolicy,
        candidates: Vec<FusedCandidate>,
    ) -> Result<(Vec<RankedCandidate>, Vec<DiversityDecisionV1>), DiversityStageError> {
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

        let mut counters = CapCounters::default();
        let mut admitted = Vec::new();
        let mut decisions = Vec::new();
        for candidate in candidates {
            // Exact and contradiction evidence is protected: diversity may
            // neither demote exact technical lookup nor erase an admitted
            // contradiction.
            let protected = candidate.exact_class != ExactClass::Approximate
                || candidate
                    .occurrences
                    .iter()
                    .any(|occurrence| occurrence.evidence_role == EvidenceRole::Contradiction);
            let dimensions = if protected {
                Vec::new()
            } else {
                counters.reached_caps(policy, &candidate)
            };
            if dimensions.is_empty() {
                if !protected {
                    counters.admit(&candidate);
                }
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
        let ranked = admitted
            .into_iter()
            .enumerate()
            .map(|(ordinal, candidate)| RankedCandidate {
                candidate,
                final_ordinal: ordinal as u32,
            })
            .collect();
        Ok((ranked, decisions))
    }
}

impl DiversityCapStage for DeterministicDiversity {
    fn apply_caps(
        &self,
        policy: &DiversityPolicy,
        candidates: Vec<FusedCandidate>,
    ) -> Result<(Vec<RankedCandidate>, Vec<DiversityDecisionV1>), DiversityStageError> {
        DeterministicDiversity::apply_caps(self, policy, candidates)
    }
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
        candidate: &FusedCandidate,
    ) -> Vec<&'static str> {
        let keys = CandidateCapKeys::from_candidate(candidate);
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

    fn admit(&mut self, candidate: &FusedCandidate) {
        let keys = CandidateCapKeys::from_candidate(candidate);
        increment_all(&mut self.namespaces, keys.namespaces);
        increment_all(&mut self.instances, keys.instances);
        increment_all(&mut self.repositories, keys.repositories);
        increment_all(&mut self.files, keys.files);
        increment_all(&mut self.sessions, keys.sessions);
        increment_all(&mut self.copy_clusters, keys.copy_clusters);
        increment_all(&mut self.evidence_roles, keys.evidence_roles);
    }
}

struct CandidateCapKeys {
    namespaces: BTreeSet<SourceNamespace>,
    instances: BTreeSet<SourceInstanceKey>,
    repositories: BTreeSet<RepositoryId>,
    files: BTreeSet<FileOccurrenceId>,
    sessions: BTreeSet<SessionOrThreadId>,
    copy_clusters: BTreeSet<LogicalCopyClusterId>,
    evidence_roles: BTreeSet<EvidenceRole>,
}

impl CandidateCapKeys {
    fn from_candidate(candidate: &FusedCandidate) -> Self {
        Self {
            namespaces: candidate
                .occurrences
                .iter()
                .map(|occurrence| occurrence.source_namespace.clone())
                .collect(),
            instances: candidate
                .occurrences
                .iter()
                .map(|occurrence| occurrence.freshness.source_instance.clone())
                .collect(),
            repositories: candidate
                .occurrences
                .iter()
                .filter_map(|occurrence| occurrence.repository_id.clone())
                .collect(),
            files: candidate
                .occurrences
                .iter()
                .filter_map(|occurrence| occurrence.file_occurrence_id.clone())
                .collect(),
            sessions: candidate
                .occurrences
                .iter()
                .filter_map(|occurrence| occurrence.session_or_thread_id.clone())
                .collect(),
            copy_clusters: candidate
                .occurrences
                .iter()
                .filter_map(|occurrence| occurrence.logical_copy_cluster_id.clone())
                .collect(),
            evidence_roles: candidate
                .occurrences
                .iter()
                .map(|occurrence| occurrence.evidence_role)
                .collect(),
        }
    }
}

fn any_reached<K: Ord>(counts: &BTreeMap<K, u32>, keys: &BTreeSet<K>, cap: Option<u32>) -> bool {
    cap.is_some_and(|cap| {
        keys.iter()
            .any(|key| counts.get(key).copied().unwrap_or(0) >= cap)
    })
}

fn increment_all<K: Ord>(counts: &mut BTreeMap<K, u32>, keys: BTreeSet<K>) {
    for key in keys {
        *counts.entry(key).or_default() += 1;
    }
}
