use std::collections::BTreeSet;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracedecay_domain::{
    DomainError, FactEventId, FactId, FactOwnerV1, ManifestDigest, ProvenanceId, RunId,
    SanitizationReceiptV1, canonical_sha256,
};

use super::super::{FactCommitDispositionV1, FactCommitOwnerV1, FactCommitReceiptV1};

const MAX_CURATION_EFFECTS: usize = 256;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MemoryAutomationCurationAddDispositionV1 {
    Added,
    NearDuplicate,
    PossibleConflict,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MemoryAutomationCurationRemoveDispositionV1 {
    Removed,
    AlreadyRemoved,
    NotFound,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MemoryAutomationCurationLinkDispositionV1 {
    Linked,
    AlreadyLinked,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MemoryAutomationCurationRelationProvenanceV1 {
    pub source_label: String,
    pub sanitization_receipt: SanitizationReceiptV1,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MemoryAutomationCurationRelationKindV1 {
    Supports,
    Contradicts,
    Supersedes,
    DerivedFrom,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MemoryAutomationCurationRelationV1 {
    pub kind: MemoryAutomationCurationRelationKindV1,
    pub evidence_fact_ids: Vec<FactId>,
    pub confidence_millionths: u32,
    pub provenance: MemoryAutomationCurationRelationProvenanceV1,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MemoryAutomationCurationMergeV1 {
    pub operation_id: ProvenanceId,
    pub input_digest: String,
    pub winner_fact_id: FactId,
    pub content_updated: bool,
    pub deleted_loser_fact_ids: Vec<FactId>,
    pub commit_receipts: Vec<FactCommitReceiptV1>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum MemoryAutomationCurationOperationEffectV1 {
    Add {
        fact_id: FactId,
        disposition: MemoryAutomationCurationAddDispositionV1,
        closest_fact_id: Option<FactId>,
        similarity_millionths: Option<u32>,
        commit: Option<FactCommitReceiptV1>,
    },
    Update {
        fact_id: FactId,
        trust_delta_millionths: i32,
        commit: FactCommitReceiptV1,
    },
    Merge {
        outcome: MemoryAutomationCurationMergeV1,
    },
    Remove {
        target_fact_id: FactId,
        disposition: MemoryAutomationCurationRemoveDispositionV1,
        remaining_fact_count: u64,
        commit: Option<FactCommitReceiptV1>,
    },
    NormalizeTags {
        fact_id: FactId,
        commit: FactCommitReceiptV1,
    },
    LinkFacts {
        source_fact_id: FactId,
        target_fact_id: FactId,
        relation: MemoryAutomationCurationRelationV1,
        disposition: MemoryAutomationCurationLinkDispositionV1,
        commit: Option<FactCommitReceiptV1>,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MemoryAutomationCurationResultV1 {
    pub owner: FactCommitOwnerV1,
    pub operation_id: ProvenanceId,
    pub input_digest: String,
    pub automation_run_id: RunId,
    pub operation_effects: Vec<MemoryAutomationCurationOperationEffectV1>,
    pub replay_fact_id: Option<FactId>,
    pub replay_event_id: Option<FactEventId>,
    pub changed_fact_ids: Vec<FactId>,
    pub accepted_operations: u64,
    pub facts_added: u64,
    pub facts_updated: u64,
    pub facts_merged: u64,
    pub facts_removed: u64,
    pub normalized_tags: u64,
    pub facts_linked: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MemoryAutomationCurationReceiptV1 {
    pub receipt: MemoryAutomationCurationResultV1,
    pub canonical_digest: ManifestDigest,
}

impl MemoryAutomationCurationReceiptV1 {
    pub fn canonical_digest(&self) -> Result<ManifestDigest, DomainError> {
        canonical_sha256(&(
            "tracedecay.automation-run.curation-receipt.v1",
            &self.receipt,
        ))
    }
}

pub(super) fn curation_receipt_matches(
    run_id: &RunId,
    settled: &MemoryAutomationCurationReceiptV1,
) -> bool {
    let receipt = &settled.receipt;
    let canonical_digest_matches = settled
        .canonical_digest()
        .is_ok_and(|digest| digest == settled.canonical_digest);
    if receipt.automation_run_id != *run_id
        || receipt.operation_id.validate().is_err()
        || !raw_sha256(&receipt.input_digest)
        || !canonical_digest_matches
        || receipt.operation_effects.is_empty()
        || receipt.operation_effects.len() > MAX_CURATION_EFFECTS
        || usize::try_from(receipt.accepted_operations).ok()
            != Some(receipt.operation_effects.len())
        || receipt.changed_fact_ids.len() > MAX_CURATION_EFFECTS
    {
        return false;
    }

    let owner = domain_owner(&receipt.owner);
    if owner.validate().is_err() {
        return false;
    }
    let mut tracker = CurationTracker::new(&receipt.owner);
    for effect in &receipt.operation_effects {
        if !tracker.accept(effect, &owner) {
            return false;
        }
    }

    receipt.replay_fact_id.as_ref() == tracker.replay_fact_id.as_ref()
        && receipt.replay_event_id.as_ref() == tracker.replay_event_id.as_ref()
        && receipt.changed_fact_ids == tracker.changed_fact_ids
        && receipt.facts_added == tracker.facts_added
        && receipt.facts_updated == tracker.facts_updated
        && receipt.facts_merged == tracker.facts_merged
        && receipt.facts_removed == tracker.facts_removed
        && receipt.normalized_tags == tracker.normalized_tags
        && receipt.facts_linked == tracker.facts_linked
}

struct CurationTracker<'a> {
    owner: &'a FactCommitOwnerV1,
    disposition: Option<FactCommitDispositionV1>,
    committed_event_ids: BTreeSet<FactEventId>,
    durable_operation_identities: BTreeSet<String>,
    changed_fact_ids: Vec<FactId>,
    replay_fact_id: Option<FactId>,
    replay_event_id: Option<FactEventId>,
    facts_added: u64,
    facts_updated: u64,
    facts_merged: u64,
    facts_removed: u64,
    normalized_tags: u64,
    facts_linked: u64,
}

impl<'a> CurationTracker<'a> {
    fn new(owner: &'a FactCommitOwnerV1) -> Self {
        Self {
            owner,
            disposition: None,
            committed_event_ids: BTreeSet::new(),
            durable_operation_identities: BTreeSet::new(),
            changed_fact_ids: Vec::new(),
            replay_fact_id: None,
            replay_event_id: None,
            facts_added: 0,
            facts_updated: 0,
            facts_merged: 0,
            facts_removed: 0,
            normalized_tags: 0,
            facts_linked: 0,
        }
    }

    fn accept(
        &mut self,
        effect: &MemoryAutomationCurationOperationEffectV1,
        owner: &FactOwnerV1,
    ) -> bool {
        match effect {
            MemoryAutomationCurationOperationEffectV1::Add {
                fact_id,
                disposition,
                closest_fact_id,
                similarity_millionths,
                commit,
            } => {
                if fact_id.validate_owner(owner).is_err()
                    || closest_fact_id
                        .as_ref()
                        .is_some_and(|fact_id| fact_id.validate_owner(owner).is_err())
                    || !add_snapshot_matches(
                        fact_id,
                        *disposition,
                        closest_fact_id.as_ref(),
                        *similarity_millionths,
                        commit.as_ref(),
                    )
                {
                    return false;
                }
                if let Some(commit) = commit {
                    if !self.accept_commit(commit, fact_id, None, ActiveAssertion::Present) {
                        return false;
                    }
                    self.facts_added = self.facts_added.saturating_add(1);
                    self.append_changed(fact_id);
                }
            }
            MemoryAutomationCurationOperationEffectV1::Update {
                fact_id,
                trust_delta_millionths,
                commit,
            } => {
                if fact_id.validate_owner(owner).is_err()
                    || !(-1_000_000..=1_000_000).contains(trust_delta_millionths)
                    || !self.accept_commit(commit, fact_id, None, ActiveAssertion::Present)
                {
                    return false;
                }
                self.facts_updated = self.facts_updated.saturating_add(1);
                self.append_changed(fact_id);
            }
            MemoryAutomationCurationOperationEffectV1::Merge { outcome } => {
                if !self.accept_merge(outcome, owner) {
                    return false;
                }
            }
            MemoryAutomationCurationOperationEffectV1::Remove {
                target_fact_id,
                disposition,
                commit,
                ..
            } => {
                if target_fact_id.validate_owner(owner).is_err()
                    || !remove_snapshot_matches(*disposition, commit.as_ref())
                {
                    return false;
                }
                if let Some(commit) = commit {
                    if !self.accept_commit(commit, target_fact_id, Some(1), ActiveAssertion::Absent)
                    {
                        return false;
                    }
                    self.facts_removed = self.facts_removed.saturating_add(1);
                    self.append_changed(target_fact_id);
                }
            }
            MemoryAutomationCurationOperationEffectV1::NormalizeTags { fact_id, commit } => {
                if fact_id.validate_owner(owner).is_err()
                    || !self.accept_durable_identity(&(
                        "tracedecay.project-memory.curation-normalize-identity.v1",
                        fact_id,
                    ))
                    || !self.accept_commit(commit, fact_id, Some(2), ActiveAssertion::Present)
                {
                    return false;
                }
                self.normalized_tags = self.normalized_tags.saturating_add(1);
                self.append_changed(fact_id);
            }
            MemoryAutomationCurationOperationEffectV1::LinkFacts {
                source_fact_id,
                target_fact_id,
                relation,
                disposition,
                commit,
            } => {
                if source_fact_id == target_fact_id
                    || !self.accept_durable_identity(&(
                        "tracedecay.project-memory.curation-link-identity.v1",
                        source_fact_id,
                        target_fact_id,
                        relation.kind,
                    ))
                    || !relation_matches_terminal(
                        relation,
                        self.owner,
                        source_fact_id,
                        target_fact_id,
                    )
                    || match disposition {
                        MemoryAutomationCurationLinkDispositionV1::Linked => {
                            commit.as_ref().is_none_or(|commit| {
                                !self.accept_commit(
                                    commit,
                                    source_fact_id,
                                    Some(1),
                                    ActiveAssertion::Any,
                                )
                            })
                        }
                        MemoryAutomationCurationLinkDispositionV1::AlreadyLinked => {
                            commit.is_some()
                        }
                    }
                {
                    return false;
                }
                if commit.is_some() {
                    self.facts_linked = self.facts_linked.saturating_add(1);
                    self.append_changed(source_fact_id);
                    self.append_changed(target_fact_id);
                }
            }
        }
        true
    }

    fn accept_merge(
        &mut self,
        outcome: &MemoryAutomationCurationMergeV1,
        owner: &FactOwnerV1,
    ) -> bool {
        if outcome.operation_id.validate().is_err()
            || !raw_sha256(&outcome.input_digest)
            || outcome.winner_fact_id.validate_owner(owner).is_err()
            || outcome.deleted_loser_fact_ids.is_empty()
            || outcome.deleted_loser_fact_ids.len() > MAX_CURATION_EFFECTS
            || outcome.deleted_loser_fact_ids.iter().any(|fact_id| {
                fact_id == &outcome.winner_fact_id || fact_id.validate_owner(owner).is_err()
            })
            || outcome
                .deleted_loser_fact_ids
                .iter()
                .enumerate()
                .any(|(index, fact_id)| outcome.deleted_loser_fact_ids[..index].contains(fact_id))
            || outcome.commit_receipts.len()
                != outcome.deleted_loser_fact_ids.len() + usize::from(outcome.content_updated)
        {
            return false;
        }

        let mut commit_index = 0;
        if outcome.content_updated {
            if !self.accept_commit(
                &outcome.commit_receipts[0],
                &outcome.winner_fact_id,
                Some(2),
                ActiveAssertion::Present,
            ) {
                return false;
            }
            self.append_changed(&outcome.winner_fact_id);
            commit_index = 1;
        }
        for (loser, commit) in outcome
            .deleted_loser_fact_ids
            .iter()
            .zip(outcome.commit_receipts[commit_index..].iter())
        {
            if !self.accept_commit(commit, loser, Some(2), ActiveAssertion::Absent) {
                return false;
            }
            self.append_changed(loser);
        }
        let Ok(merged) = u64::try_from(outcome.deleted_loser_fact_ids.len()) else {
            return false;
        };
        self.facts_merged = self.facts_merged.saturating_add(merged);
        true
    }

    fn accept_commit(
        &mut self,
        commit: &FactCommitReceiptV1,
        fact_id: &FactId,
        event_count: Option<usize>,
        active_assertion: ActiveAssertion,
    ) -> bool {
        if commit.owner != *self.owner
            || commit.fact_id != *fact_id
            || commit.committed_event_ids.is_empty()
            || event_count.is_some_and(|count| commit.committed_event_ids.len() != count)
            || commit.committed_event_ids.last() != Some(&commit.last_event_id)
            || !active_assertion.matches(&commit.active_assertion_id)
            || self
                .disposition
                .is_some_and(|disposition| disposition != commit.disposition)
            || !commit
                .committed_event_ids
                .iter()
                .all(|event_id| self.committed_event_ids.insert(event_id.clone()))
        {
            return false;
        }
        self.disposition = Some(commit.disposition);
        if self.replay_fact_id.is_none() {
            self.replay_fact_id = Some(commit.fact_id.clone());
            self.replay_event_id = Some(commit.last_event_id.clone());
        }
        true
    }

    fn accept_durable_identity<T: Serialize>(&mut self, material: &T) -> bool {
        canonical_sha256(material).is_ok_and(|digest| {
            self.durable_operation_identities
                .insert(digest.as_str().to_owned())
        })
    }

    fn append_changed(&mut self, fact_id: &FactId) {
        if !self.changed_fact_ids.contains(fact_id) {
            self.changed_fact_ids.push(fact_id.clone());
        }
    }
}

#[derive(Clone, Copy)]
enum ActiveAssertion {
    Any,
    Present,
    Absent,
}

impl ActiveAssertion {
    fn matches(self, assertion_id: &Option<tracedecay_domain::FactAssertionId>) -> bool {
        match self {
            Self::Any => true,
            Self::Present => assertion_id.is_some(),
            Self::Absent => assertion_id.is_none(),
        }
    }
}

fn add_snapshot_matches(
    fact_id: &FactId,
    disposition: MemoryAutomationCurationAddDispositionV1,
    closest_fact_id: Option<&FactId>,
    similarity_millionths: Option<u32>,
    commit: Option<&FactCommitReceiptV1>,
) -> bool {
    let comparison_matches = closest_fact_id.is_some_and(|closest| closest != fact_id)
        && similarity_millionths.is_some_and(|value| value <= 1_000_000);
    match disposition {
        MemoryAutomationCurationAddDispositionV1::Added => {
            commit.is_some() && closest_fact_id.is_none() && similarity_millionths.is_none()
        }
        MemoryAutomationCurationAddDispositionV1::NearDuplicate => {
            (commit.is_none()
                && closest_fact_id == Some(fact_id)
                && similarity_millionths == Some(1_000_000))
                || (commit.is_some() && comparison_matches)
        }
        MemoryAutomationCurationAddDispositionV1::PossibleConflict => {
            commit.is_some() && comparison_matches
        }
    }
}

fn remove_snapshot_matches(
    disposition: MemoryAutomationCurationRemoveDispositionV1,
    commit: Option<&FactCommitReceiptV1>,
) -> bool {
    match disposition {
        MemoryAutomationCurationRemoveDispositionV1::Removed => commit.is_some(),
        MemoryAutomationCurationRemoveDispositionV1::AlreadyRemoved
        | MemoryAutomationCurationRemoveDispositionV1::NotFound => commit.is_none(),
    }
}

fn domain_owner(owner: &FactCommitOwnerV1) -> FactOwnerV1 {
    match owner {
        FactCommitOwnerV1::Profile => FactOwnerV1::Profile,
        FactCommitOwnerV1::Project { project_id } => FactOwnerV1::Project {
            project_id: project_id.clone(),
        },
    }
}

fn relation_matches_terminal(
    relation: &MemoryAutomationCurationRelationV1,
    owner: &FactCommitOwnerV1,
    source_fact_id: &FactId,
    target_fact_id: &FactId,
) -> bool {
    let owner = domain_owner(owner);
    let evidence_is_canonical = !relation.evidence_fact_ids.is_empty()
        && relation.evidence_fact_ids.len() <= MAX_CURATION_EFFECTS
        && relation
            .evidence_fact_ids
            .iter()
            .all(|fact_id| fact_id.validate_owner(&owner).is_ok())
        && relation
            .evidence_fact_ids
            .windows(2)
            .all(|pair| pair[0] < pair[1]);
    let source_label = &relation.provenance.source_label;
    source_fact_id.validate_owner(&owner).is_ok()
        && target_fact_id.validate_owner(&owner).is_ok()
        && evidence_is_canonical
        && relation.confidence_millionths <= 1_000_000
        && !source_label.is_empty()
        && source_label.len() <= 4_096
        && source_label.trim() == source_label
        && !source_label.chars().any(char::is_control)
        && relation
            .provenance
            .sanitization_receipt
            .disposition()
            .permits_durable_payload()
        && relation
            .provenance
            .sanitization_receipt
            .payload()
            .is_some_and(|payload| payload.byte_len() > 0)
}

fn raw_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[cfg(test)]
#[path = "curation/tests.rs"]
mod tests;
