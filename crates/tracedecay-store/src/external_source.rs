//! Store contracts for atomic external-source commits.
//!
//! The production adapter supplies the transaction. These types make the
//! required compare-and-set, source frontier, snapshot-completion, and
//! projection state one serializable operation without introducing a second
//! writer or source registry.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_domain::{
    ComponentVersion, DomainError, ManifestDigest, SourceAggregateFrontierV1, SourceBindingV1,
    SourceContentStateV1, SourceDefinitionV1, SourceNativeObjectIdV1, SourceObjectObservationV1,
    SourceObjectRevisionV1, SourcePartitionIdV1, SourceSnapshotCompletionV1, canonical_sha256,
};

pub const MAX_SOURCE_COMMIT_OBSERVATIONS_V1: usize = 10_000;

#[derive(Debug, Error)]
pub enum SourceStoreErrorV1 {
    #[error("external source domain contract is invalid")]
    Domain(#[from] DomainError),
    #[error("external source definition changed without publication")]
    DefinitionConflict,
    #[error("external source binding changed across immutable dimensions")]
    BindingConflict,
    #[error("external source frontier compare-and-set failed")]
    FrontierConflict,
    #[error("external source idempotency key was reused with a different request")]
    IdempotencyConflict,
    #[error("external source commit has inconsistent snapshot completion")]
    SnapshotCompletionMismatch,
    #[error("external source commit contains duplicate native objects")]
    DuplicateNativeObject,
    #[error("external source commit exceeds the bounded object limit")]
    TooManyObjects,
}

pub type SourceStoreResult<T> = Result<T, SourceStoreErrorV1>;

/// One atomic source-side mutation. The database adapter persists its source
/// frontier, immutable sanitized observations, derived projection, and
/// snapshot completion in one transaction.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourceCommitV1 {
    definition: SourceDefinitionV1,
    binding: SourceBindingV1,
    partition: SourcePartitionIdV1,
    projector: ComponentVersion,
    idempotency_key: ManifestDigest,
    request_digest: ManifestDigest,
    expected_frontier: Option<SourceAggregateFrontierV1>,
    next_frontier: SourceAggregateFrontierV1,
    observations: Vec<SourceObjectObservationV1>,
    snapshot_completion: Option<SourceSnapshotCompletionV1>,
}

impl SourceCommitV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        definition: SourceDefinitionV1,
        binding: SourceBindingV1,
        partition: SourcePartitionIdV1,
        projector: ComponentVersion,
        idempotency_key: ManifestDigest,
        request_digest: ManifestDigest,
        expected_frontier: Option<SourceAggregateFrontierV1>,
        next_frontier: SourceAggregateFrontierV1,
        observations: Vec<SourceObjectObservationV1>,
        snapshot_completion: Option<SourceSnapshotCompletionV1>,
    ) -> SourceStoreResult<Self> {
        let commit = Self {
            definition,
            binding,
            partition,
            projector,
            idempotency_key,
            request_digest,
            expected_frontier,
            next_frontier,
            observations,
            snapshot_completion,
        };
        commit.validate()?;
        Ok(commit)
    }

    pub fn definition(&self) -> &SourceDefinitionV1 {
        &self.definition
    }

    pub fn binding(&self) -> &SourceBindingV1 {
        &self.binding
    }

    pub fn partition(&self) -> &SourcePartitionIdV1 {
        &self.partition
    }

    pub fn projector(&self) -> &ComponentVersion {
        &self.projector
    }

    pub fn idempotency_key(&self) -> &ManifestDigest {
        &self.idempotency_key
    }

    pub fn request_digest(&self) -> &ManifestDigest {
        &self.request_digest
    }

    pub fn expected_frontier(&self) -> Option<&SourceAggregateFrontierV1> {
        self.expected_frontier.as_ref()
    }

    pub fn next_frontier(&self) -> &SourceAggregateFrontierV1 {
        &self.next_frontier
    }

    pub fn observations(&self) -> &[SourceObjectObservationV1] {
        &self.observations
    }

    pub fn snapshot_completion(&self) -> Option<&SourceSnapshotCompletionV1> {
        self.snapshot_completion.as_ref()
    }

    pub fn validate(&self) -> SourceStoreResult<()> {
        self.definition.validate()?;
        self.binding.validate_against(&self.definition)?;
        self.partition.validate()?;
        self.projector.validate()?;
        self.idempotency_key.validate()?;
        self.request_digest.validate()?;
        let binding = self.binding.immutable_identity()?;
        if self.next_frontier.binding() != &binding {
            return Err(SourceStoreErrorV1::BindingConflict);
        }
        if self
            .expected_frontier
            .as_ref()
            .is_some_and(|frontier| frontier.binding() != &binding)
        {
            return Err(SourceStoreErrorV1::BindingConflict);
        }
        let next_partition = self
            .next_frontier
            .partition(&self.partition)
            .ok_or(SourceStoreErrorV1::FrontierConflict)?;
        if self.observations.len() > MAX_SOURCE_COMMIT_OBSERVATIONS_V1 {
            return Err(SourceStoreErrorV1::TooManyObjects);
        }
        let mut seen = BTreeSet::new();
        for observation in &self.observations {
            observation.validate()?;
            if !seen.insert(observation.native_object().clone()) {
                return Err(SourceStoreErrorV1::DuplicateNativeObject);
            }
        }
        match (&self.snapshot_completion, next_partition.coverage()) {
            (Some(completion), tracedecay_domain::SourceCoverageV1::Complete) => {
                completion.validate()?;
                if completion.partition() != &self.partition
                    || next_partition.snapshot() != Some(completion.snapshot())
                {
                    return Err(SourceStoreErrorV1::SnapshotCompletionMismatch);
                }
                let staged_live = self
                    .observations
                    .iter()
                    .filter(|observation| {
                        observation.content_state() != SourceContentStateV1::AuthoritativeDeleted
                    })
                    .map(|observation| observation.native_object().clone())
                    .collect::<BTreeSet<_>>();
                if staged_live != *completion.present_objects() {
                    return Err(SourceStoreErrorV1::SnapshotCompletionMismatch);
                }
            }
            (None, tracedecay_domain::SourceCoverageV1::Complete)
            | (Some(_), tracedecay_domain::SourceCoverageV1::Partial)
            | (Some(_), tracedecay_domain::SourceCoverageV1::Unknown) => {
                return Err(SourceStoreErrorV1::SnapshotCompletionMismatch);
            }
            (None, tracedecay_domain::SourceCoverageV1::Partial)
            | (None, tracedecay_domain::SourceCoverageV1::Unknown) => {}
        }
        if let Some(expected) = &self.expected_frontier {
            expected.validate()?;
            let expected_sequence = expected
                .partition(&self.partition)
                .map_or(0, |frontier| frontier.sequence());
            if next_partition.sequence() != expected_sequence.saturating_add(1) {
                return Err(SourceStoreErrorV1::FrontierConflict);
            }
        } else if next_partition.sequence() != 1 {
            return Err(SourceStoreErrorV1::FrontierConflict);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind", content = "observation")]
pub enum SourceProjectionEffectV1 {
    Upsert(SourceObjectObservationV1),
    Tombstone(SourceObjectObservationV1),
}

impl SourceProjectionEffectV1 {
    pub fn observation(&self) -> &SourceObjectObservationV1 {
        match self {
            Self::Upsert(observation) | Self::Tombstone(observation) => observation,
        }
    }
}

/// Pure, deterministic projection of one committed source page.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourceProjectionCommitV1 {
    projector: ComponentVersion,
    source_frontier: SourceAggregateFrontierV1,
    effects: Vec<SourceProjectionEffectV1>,
    receipt_digest: ManifestDigest,
}

impl SourceProjectionCommitV1 {
    fn new(
        projector: ComponentVersion,
        source_frontier: SourceAggregateFrontierV1,
        effects: Vec<SourceProjectionEffectV1>,
    ) -> SourceStoreResult<Self> {
        projector.validate()?;
        source_frontier.validate()?;
        for effect in &effects {
            effect.observation().validate()?;
        }
        let receipt_digest = canonical_sha256(&(
            "tracedecay.external-source.projection-commit.v1",
            &projector,
            &source_frontier,
            &effects,
        ))?;
        Ok(Self {
            projector,
            source_frontier,
            effects,
            receipt_digest,
        })
    }

    pub fn projector(&self) -> &ComponentVersion {
        &self.projector
    }

    pub fn source_frontier(&self) -> &SourceAggregateFrontierV1 {
        &self.source_frontier
    }

    pub fn effects(&self) -> &[SourceProjectionEffectV1] {
        &self.effects
    }

    pub fn receipt_digest(&self) -> &ManifestDigest {
        &self.receipt_digest
    }

    pub fn validate(&self) -> SourceStoreResult<()> {
        self.projector.validate()?;
        self.source_frontier.validate()?;
        for effect in &self.effects {
            effect.observation().validate()?;
        }
        let expected = canonical_sha256(&(
            "tracedecay.external-source.projection-commit.v1",
            &self.projector,
            &self.source_frontier,
            &self.effects,
        ))?;
        if expected != self.receipt_digest {
            return Err(SourceStoreErrorV1::Domain(DomainError::DigestMismatch));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourceCommitReceiptV1 {
    idempotency_key: ManifestDigest,
    request_digest: ManifestDigest,
    source_frontier: SourceAggregateFrontierV1,
    projection: SourceProjectionCommitV1,
}

impl SourceCommitReceiptV1 {
    fn new(
        idempotency_key: ManifestDigest,
        request_digest: ManifestDigest,
        source_frontier: SourceAggregateFrontierV1,
        projection: SourceProjectionCommitV1,
    ) -> SourceStoreResult<Self> {
        let receipt = Self {
            idempotency_key,
            request_digest,
            source_frontier,
            projection,
        };
        receipt.validate()?;
        Ok(receipt)
    }

    pub fn idempotency_key(&self) -> &ManifestDigest {
        &self.idempotency_key
    }

    pub fn request_digest(&self) -> &ManifestDigest {
        &self.request_digest
    }

    pub fn source_frontier(&self) -> &SourceAggregateFrontierV1 {
        &self.source_frontier
    }

    pub fn projection(&self) -> &SourceProjectionCommitV1 {
        &self.projection
    }

    pub fn validate(&self) -> SourceStoreResult<()> {
        self.idempotency_key.validate()?;
        self.request_digest.validate()?;
        self.source_frontier.validate()?;
        self.projection.validate()?;
        if self.projection.source_frontier() != &self.source_frontier {
            return Err(SourceStoreErrorV1::FrontierConflict);
        }
        Ok(())
    }
}

/// The exact durable state a project Database stores under its existing writer
/// authority. It is a source-local state record, not a second database or
/// cross-provider registry.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourceStoreStateV1 {
    definition: SourceDefinitionV1,
    binding: SourceBindingV1,
    source_frontier: SourceAggregateFrontierV1,
    projection: SourceProjectionCommitV1,
    projected_objects: BTreeMap<SourceNativeObjectIdV1, SourceObjectObservationV1>,
    receipt: SourceCommitReceiptV1,
}

impl SourceStoreStateV1 {
    fn new(
        definition: SourceDefinitionV1,
        binding: SourceBindingV1,
        source_frontier: SourceAggregateFrontierV1,
        projection: SourceProjectionCommitV1,
        projected_objects: BTreeMap<SourceNativeObjectIdV1, SourceObjectObservationV1>,
        receipt: SourceCommitReceiptV1,
    ) -> SourceStoreResult<Self> {
        let state = Self {
            definition,
            binding,
            source_frontier,
            projection,
            projected_objects,
            receipt,
        };
        state.validate()?;
        Ok(state)
    }

    pub fn source_frontier(&self) -> &SourceAggregateFrontierV1 {
        &self.source_frontier
    }

    pub fn projected_objects(
        &self,
    ) -> &BTreeMap<SourceNativeObjectIdV1, SourceObjectObservationV1> {
        &self.projected_objects
    }

    pub fn receipt(&self) -> &SourceCommitReceiptV1 {
        &self.receipt
    }

    pub fn validate(&self) -> SourceStoreResult<()> {
        self.definition.validate()?;
        self.binding.validate_against(&self.definition)?;
        self.source_frontier.validate()?;
        self.projection.validate()?;
        self.receipt.validate()?;
        let binding_identity = self.binding.immutable_identity()?;
        if self.source_frontier.binding() != &binding_identity
            || self.projection.source_frontier() != &self.source_frontier
            || self.receipt.source_frontier() != &self.source_frontier
        {
            return Err(SourceStoreErrorV1::BindingConflict);
        }
        for (native_object, observation) in &self.projected_objects {
            native_object.validate()?;
            observation.validate()?;
            if native_object != observation.native_object() {
                return Err(SourceStoreErrorV1::SnapshotCompletionMismatch);
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SourceCommitApplyOutcomeV1 {
    Committed(Box<SourceStoreStateV1>),
    ExactDuplicate(Box<SourceCommitReceiptV1>),
}

/// Applies one source commit against the caller's previously read state. The
/// caller is responsible for placing this operation inside its authoritative
/// database transaction.
pub fn apply_source_commit(
    current: Option<&SourceStoreStateV1>,
    commit: SourceCommitV1,
) -> SourceStoreResult<SourceCommitApplyOutcomeV1> {
    commit.validate()?;
    if let Some(current) = current {
        current.validate()?;
        if current.receipt().idempotency_key() == commit.idempotency_key() {
            return if current.receipt().request_digest() == commit.request_digest() {
                Ok(SourceCommitApplyOutcomeV1::ExactDuplicate(Box::new(
                    current.receipt().clone(),
                )))
            } else {
                Err(SourceStoreErrorV1::IdempotencyConflict)
            };
        }
        if &current.definition != commit.definition() {
            return Err(SourceStoreErrorV1::DefinitionConflict);
        }
        if current.binding.immutable_identity()? != commit.binding().immutable_identity()? {
            return Err(SourceStoreErrorV1::BindingConflict);
        }
        if commit.expected_frontier() != Some(current.source_frontier()) {
            return Err(SourceStoreErrorV1::FrontierConflict);
        }
    } else if commit.expected_frontier().is_some() {
        return Err(SourceStoreErrorV1::FrontierConflict);
    }

    let mut projected_objects =
        current.map_or_else(BTreeMap::new, |state| state.projected_objects.clone());
    let mut observations = commit.observations().to_vec();
    observations.sort_by(|left, right| {
        left.native_object()
            .digest()
            .as_str()
            .cmp(right.native_object().digest().as_str())
    });
    let mut effects = Vec::new();
    for observation in observations {
        if projected_objects.get(observation.native_object()) == Some(&observation) {
            continue;
        }
        let effect = if observation.content_state() == SourceContentStateV1::AuthoritativeDeleted {
            SourceProjectionEffectV1::Tombstone(observation.clone())
        } else {
            SourceProjectionEffectV1::Upsert(observation.clone())
        };
        projected_objects.insert(observation.native_object().clone(), observation);
        effects.push(effect);
    }
    if let Some(completion) = commit.snapshot_completion() {
        let absent = projected_objects
            .iter()
            .filter(|(native_object, observation)| {
                observation.content_state() != SourceContentStateV1::AuthoritativeDeleted
                    && !completion.present_objects().contains(*native_object)
            })
            .map(|(native_object, observation)| (native_object.clone(), observation.clone()))
            .collect::<Vec<_>>();
        for (native_object, observation) in absent {
            let tombstone = absence_tombstone(&observation, completion)?;
            projected_objects.insert(native_object, tombstone.clone());
            effects.push(SourceProjectionEffectV1::Tombstone(tombstone));
        }
    }
    let projection = SourceProjectionCommitV1::new(
        commit.projector().clone(),
        commit.next_frontier().clone(),
        effects,
    )?;
    let receipt = SourceCommitReceiptV1::new(
        commit.idempotency_key().clone(),
        commit.request_digest().clone(),
        commit.next_frontier().clone(),
        projection.clone(),
    )?;
    Ok(SourceCommitApplyOutcomeV1::Committed(Box::new(
        SourceStoreStateV1::new(
            commit.definition().clone(),
            commit.binding().clone(),
            commit.next_frontier().clone(),
            projection,
            projected_objects,
            receipt,
        )?,
    )))
}

fn absence_tombstone(
    prior: &SourceObjectObservationV1,
    completion: &SourceSnapshotCompletionV1,
) -> SourceStoreResult<SourceObjectObservationV1> {
    let revision = SourceObjectRevisionV1::new(canonical_sha256(&(
        "tracedecay.external-source.absence-tombstone-revision.v1",
        prior.revision(),
        completion.snapshot(),
    ))?);
    let digest = canonical_sha256(&(
        "tracedecay.external-source.absence-tombstone.v1",
        prior.native_object(),
        &revision,
        completion.completion_digest(),
    ))?;
    Ok(SourceObjectObservationV1::new(
        prior.native_object().clone(),
        revision,
        digest,
        SourceContentStateV1::AuthoritativeDeleted,
    )?)
}
