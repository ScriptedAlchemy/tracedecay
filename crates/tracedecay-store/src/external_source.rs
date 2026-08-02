//! Store contracts for atomic external-source commits.
//!
//! The production adapter supplies the transaction. These types make the
//! required compare-and-set, source frontier, snapshot-completion, and
//! projection state one serializable operation without introducing a second
//! writer or source registry.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_domain::{
    ComponentVersion, DomainError, ManifestDigest, ResolutionAuthorizationV1, RetrievalAnchorId,
    SanitizationReceiptRefV1, SourceAggregateFrontierV1, SourceBindingIdentityV1, SourceBindingV1,
    SourceContentStateV1, SourceDefinitionV1, SourceDeletionSemanticsV1, SourceNativeObjectIdV1,
    SourceObjectObservationV1, SourceObjectRevisionV1, SourcePartitionIdV1,
    SourceSnapshotCompletionV1, canonical_sha256,
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
    #[error("external source authority revision compare-and-set failed")]
    AuthorityRevisionConflict,
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
    #[error("external source commit exceeds the definition partition limit")]
    TooManyPartitions,
    #[error("external source native object changed partition ownership")]
    ObjectPartitionConflict,
    #[error("external source native object revision conflicts with immutable history")]
    RevisionConflict,
    #[error("external source object transition does not match current lineage")]
    LineageConflict,
    #[error("external source observation evidence does not match its authority")]
    EvidenceConflict,
}

pub type SourceStoreResult<T> = Result<T, SourceStoreErrorV1>;

/// Records that one in-memory value already passed its own integrity checks.
///
/// External-source records are content-addressed and immutable: every field is
/// private, no accessor hands out a mutable borrow, and the only in-module
/// assembly path rebuilds a value and re-verifies it through
/// [`SourceStoreStateV1::validated`]. Re-canonicalizing and re-hashing the same
/// bytes therefore cannot change a verdict, and a single external-source write
/// used to do exactly that four times over the whole store: the executor
/// validates the loaded state, `apply_source_commit` validates it again, the
/// assembled successor validates once, and the persist path validates it a
/// fourth time — each sweep re-hashing every historical receipt, every stored
/// mutation, and every projection.
///
/// The memo keeps the fail-closed gate intact. It is never serialized, so a
/// value decoded from durable bytes always starts unverified and is fully
/// validated on first contact; it is only carried across a `clone`, where the
/// clone is by construction the same value that was verified.
#[derive(Debug, Default)]
struct ValidationMemoV1(AtomicBool);

impl ValidationMemoV1 {
    fn is_verified(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }

    fn mark_verified(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    fn clear(&mut self) {
        *self.0.get_mut() = false;
    }
}

impl Clone for ValidationMemoV1 {
    fn clone(&self) -> Self {
        Self(AtomicBool::new(self.is_verified()))
    }
}

/// The memo is provenance, never content: two records with equal fields are
/// equal whether or not either has been verified yet.
impl PartialEq for ValidationMemoV1 {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl Eq for ValidationMemoV1 {}

/// Required proof references for one sanitized external-source revision.
///
/// The binding and observation coordinates are repeated deliberately: durable
/// decoding can validate that a receipt, anchor, and authorization decision
/// were committed for this exact source revision rather than merely being
/// present somewhere in the same transaction.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourceObservationEvidenceV1 {
    binding: SourceBindingIdentityV1,
    partition: SourcePartitionIdV1,
    native_object: SourceNativeObjectIdV1,
    revision: SourceObjectRevisionV1,
    sanitized_digest: ManifestDigest,
    sanitization_receipt: SanitizationReceiptRefV1,
    retrieval_anchor: RetrievalAnchorId,
    authorization: ResolutionAuthorizationV1,
    source_authorization_digest: ManifestDigest,
    snapshot_completion_digest: Option<ManifestDigest>,
    evidence_digest: ManifestDigest,
    #[serde(skip)]
    verified: ValidationMemoV1,
}

impl SourceObservationEvidenceV1 {
    pub fn new(
        binding: SourceBindingIdentityV1,
        partition: SourcePartitionIdV1,
        observation: &SourceObjectObservationV1,
        sanitization_receipt: SanitizationReceiptRefV1,
        retrieval_anchor: RetrievalAnchorId,
        authorization: ResolutionAuthorizationV1,
        source_authorization_digest: ManifestDigest,
    ) -> SourceStoreResult<Self> {
        Self::new_internal(
            binding,
            partition,
            observation,
            sanitization_receipt,
            retrieval_anchor,
            authorization,
            source_authorization_digest,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn new_internal(
        binding: SourceBindingIdentityV1,
        partition: SourcePartitionIdV1,
        observation: &SourceObjectObservationV1,
        sanitization_receipt: SanitizationReceiptRefV1,
        retrieval_anchor: RetrievalAnchorId,
        authorization: ResolutionAuthorizationV1,
        source_authorization_digest: ManifestDigest,
        snapshot_completion_digest: Option<ManifestDigest>,
    ) -> SourceStoreResult<Self> {
        let evidence_digest = canonical_sha256(&(
            "tracedecay.external-source.observation-evidence.v1",
            &binding,
            &partition,
            observation.native_object(),
            observation.revision(),
            observation.sanitized_digest(),
            &sanitization_receipt,
            &retrieval_anchor,
            &authorization,
            &source_authorization_digest,
            &snapshot_completion_digest,
        ))?;
        let evidence = Self {
            binding,
            partition,
            native_object: observation.native_object().clone(),
            revision: observation.revision().clone(),
            sanitized_digest: observation.sanitized_digest().clone(),
            sanitization_receipt,
            retrieval_anchor,
            authorization,
            source_authorization_digest,
            snapshot_completion_digest,
            evidence_digest,
            verified: ValidationMemoV1::default(),
        };
        evidence.validate_against(&evidence.binding, &evidence.partition, observation)?;
        Ok(evidence)
    }

    pub fn sanitization_receipt(&self) -> &SanitizationReceiptRefV1 {
        &self.sanitization_receipt
    }

    pub fn binding(&self) -> &SourceBindingIdentityV1 {
        &self.binding
    }

    pub fn partition(&self) -> &SourcePartitionIdV1 {
        &self.partition
    }

    pub fn retrieval_anchor(&self) -> &RetrievalAnchorId {
        &self.retrieval_anchor
    }

    pub fn authorization(&self) -> &ResolutionAuthorizationV1 {
        &self.authorization
    }

    pub fn source_authorization_digest(&self) -> &ManifestDigest {
        &self.source_authorization_digest
    }

    pub fn snapshot_completion_digest(&self) -> Option<&ManifestDigest> {
        self.snapshot_completion_digest.as_ref()
    }

    pub fn evidence_digest(&self) -> &ManifestDigest {
        &self.evidence_digest
    }

    pub fn validate_against(
        &self,
        binding: &SourceBindingIdentityV1,
        partition: &SourcePartitionIdV1,
        observation: &SourceObjectObservationV1,
    ) -> SourceStoreResult<()> {
        self.validate_self()?;
        if &self.binding != binding
            || &self.partition != partition
            || &self.native_object != observation.native_object()
            || &self.revision != observation.revision()
            || &self.sanitized_digest != observation.sanitized_digest()
            || self.authorization.privacy_domain_id != binding.privacy_domain
        {
            return Err(SourceStoreErrorV1::EvidenceConflict);
        }
        Ok(())
    }

    /// Argument-independent half of [`Self::validate_against`].
    ///
    /// The cross-checks above compare against a caller-supplied binding,
    /// partition and observation, so they run on every call. Everything here
    /// reads only this record's own immutable fields, so it is verified once
    /// per value; a digest mismatch is still `EvidenceConflict`, exactly as
    /// when the recompute trailed the comparisons.
    fn validate_self(&self) -> SourceStoreResult<()> {
        if self.verified.is_verified() {
            return Ok(());
        }
        self.binding.validate()?;
        self.partition.validate()?;
        self.native_object.validate()?;
        self.revision.validate()?;
        self.sanitized_digest.validate()?;
        self.sanitization_receipt.validate()?;
        self.retrieval_anchor.validate()?;
        self.authorization.validate()?;
        self.source_authorization_digest.validate()?;
        self.snapshot_completion_digest
            .as_ref()
            .map_or(Ok(()), ManifestDigest::validate)?;
        self.evidence_digest.validate()?;
        let expected = canonical_sha256(&(
            "tracedecay.external-source.observation-evidence.v1",
            &self.binding,
            &self.partition,
            &self.native_object,
            &self.revision,
            &self.sanitized_digest,
            &self.sanitization_receipt,
            &self.retrieval_anchor,
            &self.authorization,
            &self.source_authorization_digest,
            &self.snapshot_completion_digest,
        ))?;
        if expected != self.evidence_digest {
            return Err(SourceStoreErrorV1::EvidenceConflict);
        }
        self.verified.mark_verified();
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SourceObjectTransitionV1 {
    Initial,
    Successor,
    Correction,
    Tombstone,
    Reappearance,
}

/// One explicit immutable revision plus the intended relationship to the
/// native object's current revision.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourceObjectMutationV1 {
    observation: SourceObjectObservationV1,
    predecessor: Option<SourceObjectRevisionV1>,
    transition: SourceObjectTransitionV1,
    evidence: SourceObservationEvidenceV1,
    mutation_digest: ManifestDigest,
    #[serde(skip)]
    verified: ValidationMemoV1,
}

impl SourceObjectMutationV1 {
    pub fn new(
        observation: SourceObjectObservationV1,
        predecessor: Option<SourceObjectRevisionV1>,
        transition: SourceObjectTransitionV1,
        evidence: SourceObservationEvidenceV1,
    ) -> SourceStoreResult<Self> {
        let mutation_digest = canonical_sha256(&(
            "tracedecay.external-source.object-mutation.v1",
            &observation,
            &predecessor,
            transition,
            &evidence,
        ))?;
        let mutation = Self {
            observation,
            predecessor,
            transition,
            evidence,
            mutation_digest,
            verified: ValidationMemoV1::default(),
        };
        mutation.validate_shape()?;
        Ok(mutation)
    }

    pub fn observation(&self) -> &SourceObjectObservationV1 {
        &self.observation
    }

    pub fn predecessor(&self) -> Option<&SourceObjectRevisionV1> {
        self.predecessor.as_ref()
    }

    pub fn transition(&self) -> SourceObjectTransitionV1 {
        self.transition
    }

    pub fn evidence(&self) -> &SourceObservationEvidenceV1 {
        &self.evidence
    }

    pub fn mutation_digest(&self) -> &ManifestDigest {
        &self.mutation_digest
    }

    fn validate_shape(&self) -> SourceStoreResult<()> {
        if self.verified.is_verified() {
            return Ok(());
        }
        self.observation.validate()?;
        self.evidence.validate_against(
            self.evidence.binding(),
            self.evidence.partition(),
            &self.observation,
        )?;
        self.predecessor
            .as_ref()
            .map_or(Ok(()), SourceObjectRevisionV1::validate)?;
        self.mutation_digest.validate()?;
        if self.observation.content_state() == SourceContentStateV1::TemporarilyUnavailable {
            return Err(SourceStoreErrorV1::LineageConflict);
        }
        let deleted =
            self.observation.content_state() == SourceContentStateV1::AuthoritativeDeleted;
        match (self.transition, self.predecessor.is_some(), deleted) {
            (SourceObjectTransitionV1::Initial, false, false)
            | (SourceObjectTransitionV1::Successor, true, false)
            | (SourceObjectTransitionV1::Correction, true, false)
            | (SourceObjectTransitionV1::Tombstone, true, true)
            | (SourceObjectTransitionV1::Reappearance, true, false) => {}
            _ => return Err(SourceStoreErrorV1::LineageConflict),
        }
        let expected = canonical_sha256(&(
            "tracedecay.external-source.object-mutation.v1",
            &self.observation,
            &self.predecessor,
            self.transition,
            &self.evidence,
        ))?;
        if expected != self.mutation_digest {
            return Err(SourceStoreErrorV1::RevisionConflict);
        }
        self.verified.mark_verified();
        Ok(())
    }

    fn validate_against(
        &self,
        binding: &SourceBindingIdentityV1,
        partition: &SourcePartitionIdV1,
    ) -> SourceStoreResult<()> {
        self.validate_shape()?;
        self.evidence
            .validate_against(binding, partition, &self.observation)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourceObjectLineageV1 {
    native_object: SourceNativeObjectIdV1,
    partition: SourcePartitionIdV1,
    predecessor: SourceObjectRevisionV1,
    successor: SourceObjectRevisionV1,
    transition: SourceObjectTransitionV1,
    lineage_digest: ManifestDigest,
    #[serde(skip)]
    verified: ValidationMemoV1,
}

impl SourceObjectLineageV1 {
    fn new(
        partition: SourcePartitionIdV1,
        mutation: &SourceObjectMutationV1,
    ) -> SourceStoreResult<Self> {
        let predecessor = mutation
            .predecessor()
            .cloned()
            .ok_or(SourceStoreErrorV1::LineageConflict)?;
        let native_object = mutation.observation().native_object().clone();
        let successor = mutation.observation().revision().clone();
        let transition = mutation.transition();
        if transition == SourceObjectTransitionV1::Initial {
            return Err(SourceStoreErrorV1::LineageConflict);
        }
        let lineage_digest = canonical_sha256(&(
            "tracedecay.external-source.object-lineage.v1",
            &native_object,
            &partition,
            &predecessor,
            &successor,
            transition,
        ))?;
        Ok(Self {
            native_object,
            partition,
            predecessor,
            successor,
            transition,
            lineage_digest,
            verified: ValidationMemoV1::default(),
        })
    }

    pub fn transition(&self) -> SourceObjectTransitionV1 {
        self.transition
    }

    pub fn native_object(&self) -> &SourceNativeObjectIdV1 {
        &self.native_object
    }

    pub fn partition(&self) -> &SourcePartitionIdV1 {
        &self.partition
    }

    pub fn predecessor(&self) -> &SourceObjectRevisionV1 {
        &self.predecessor
    }

    pub fn successor(&self) -> &SourceObjectRevisionV1 {
        &self.successor
    }

    pub fn lineage_digest(&self) -> &ManifestDigest {
        &self.lineage_digest
    }

    fn validate(&self) -> SourceStoreResult<()> {
        if self.verified.is_verified() {
            return Ok(());
        }
        self.native_object.validate()?;
        self.partition.validate()?;
        self.predecessor.validate()?;
        self.successor.validate()?;
        self.lineage_digest.validate()?;
        if self.predecessor == self.successor
            || self.transition == SourceObjectTransitionV1::Initial
        {
            return Err(SourceStoreErrorV1::LineageConflict);
        }
        let expected = canonical_sha256(&(
            "tracedecay.external-source.object-lineage.v1",
            &self.native_object,
            &self.partition,
            &self.predecessor,
            &self.successor,
            self.transition,
        ))?;
        if expected != self.lineage_digest {
            return Err(SourceStoreErrorV1::LineageConflict);
        }
        self.verified.mark_verified();
        Ok(())
    }
}

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
    mutations: Vec<SourceObjectMutationV1>,
    snapshot_completion: Option<SourceSnapshotCompletionV1>,
    #[serde(skip)]
    verified: ValidationMemoV1,
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
        mutations: Vec<SourceObjectMutationV1>,
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
            mutations,
            snapshot_completion,
            verified: ValidationMemoV1::default(),
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

    pub fn mutations(&self) -> &[SourceObjectMutationV1] {
        &self.mutations
    }

    pub fn snapshot_completion(&self) -> Option<&SourceSnapshotCompletionV1> {
        self.snapshot_completion.as_ref()
    }

    pub fn validate(&self) -> SourceStoreResult<()> {
        if self.verified.is_verified() {
            return Ok(());
        }
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
        if self.next_frontier.partitions().len() > usize::from(self.definition.max_partitions)
            || self.expected_frontier.as_ref().is_some_and(|frontier| {
                frontier.partitions().len() > usize::from(self.definition.max_partitions)
            })
        {
            return Err(SourceStoreErrorV1::TooManyPartitions);
        }
        let next_partition = self
            .next_frontier
            .partition(&self.partition)
            .ok_or(SourceStoreErrorV1::FrontierConflict)?;
        if self.mutations.len() > MAX_SOURCE_COMMIT_OBSERVATIONS_V1 {
            return Err(SourceStoreErrorV1::TooManyObjects);
        }
        let mut seen = BTreeSet::new();
        for mutation in &self.mutations {
            mutation.validate_against(&binding, &self.partition)?;
            if !seen.insert(mutation.observation().native_object().clone()) {
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
                    .mutations
                    .iter()
                    .filter(|mutation| {
                        mutation.observation().content_state()
                            != SourceContentStateV1::AuthoritativeDeleted
                    })
                    .map(|mutation| mutation.observation().native_object().clone())
                    .collect::<BTreeSet<_>>();
                if !staged_live.is_subset(completion.present_objects()) {
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
        if let Some(expected) = &self.expected_frontier {
            for (partition, prior) in expected.partitions() {
                if partition != &self.partition
                    && self.next_frontier.partition(partition) != Some(prior)
                {
                    return Err(SourceStoreErrorV1::FrontierConflict);
                }
            }
            let expected_len = expected.partitions().len()
                + usize::from(expected.partition(&self.partition).is_none());
            if self.next_frontier.partitions().len() != expected_len {
                return Err(SourceStoreErrorV1::FrontierConflict);
            }
        } else if self.next_frontier.partitions().len() != 1 {
            return Err(SourceStoreErrorV1::FrontierConflict);
        }
        self.verified.mark_verified();
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
    mutations: Vec<SourceObjectMutationV1>,
    effects: Vec<SourceProjectionEffectV1>,
    lineage: Vec<SourceObjectLineageV1>,
    receipt_digest: ManifestDigest,
    #[serde(skip)]
    verified: ValidationMemoV1,
}

impl SourceProjectionCommitV1 {
    fn new(
        projector: ComponentVersion,
        source_frontier: SourceAggregateFrontierV1,
        mutations: Vec<SourceObjectMutationV1>,
        effects: Vec<SourceProjectionEffectV1>,
        lineage: Vec<SourceObjectLineageV1>,
    ) -> SourceStoreResult<Self> {
        projector.validate()?;
        source_frontier.validate()?;
        validate_projection_payload(&source_frontier, &mutations, &effects, &lineage)?;
        let receipt_digest = canonical_sha256(&(
            "tracedecay.external-source.projection-commit.v1",
            &projector,
            &source_frontier,
            &mutations,
            &effects,
            &lineage,
        ))?;
        // Every check `validate` performs just ran, and `receipt_digest` was
        // computed from these exact fields, so the equality it re-derives holds
        // by construction.
        let verified = ValidationMemoV1::default();
        verified.mark_verified();
        Ok(Self {
            projector,
            source_frontier,
            mutations,
            effects,
            lineage,
            receipt_digest,
            verified,
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

    pub fn mutations(&self) -> &[SourceObjectMutationV1] {
        &self.mutations
    }

    pub fn lineage(&self) -> &[SourceObjectLineageV1] {
        &self.lineage
    }

    pub fn receipt_digest(&self) -> &ManifestDigest {
        &self.receipt_digest
    }

    pub fn validate(&self) -> SourceStoreResult<()> {
        if self.verified.is_verified() {
            return Ok(());
        }
        self.projector.validate()?;
        self.source_frontier.validate()?;
        validate_projection_payload(
            &self.source_frontier,
            &self.mutations,
            &self.effects,
            &self.lineage,
        )?;
        let expected = canonical_sha256(&(
            "tracedecay.external-source.projection-commit.v1",
            &self.projector,
            &self.source_frontier,
            &self.mutations,
            &self.effects,
            &self.lineage,
        ))?;
        if expected != self.receipt_digest {
            return Err(SourceStoreErrorV1::Domain(DomainError::DigestMismatch));
        }
        self.verified.mark_verified();
        Ok(())
    }
}

fn validate_projection_payload(
    source_frontier: &SourceAggregateFrontierV1,
    mutations: &[SourceObjectMutationV1],
    effects: &[SourceProjectionEffectV1],
    lineage: &[SourceObjectLineageV1],
) -> SourceStoreResult<()> {
    if mutations.len() != effects.len() {
        return Err(SourceStoreErrorV1::RevisionConflict);
    }
    let mut expected_lineage = Vec::new();
    for (mutation, effect) in mutations.iter().zip(effects) {
        let partition = mutation.evidence().partition();
        if source_frontier.partition(partition).is_none() {
            return Err(SourceStoreErrorV1::ObjectPartitionConflict);
        }
        mutation.validate_against(source_frontier.binding(), partition)?;
        effect.observation().validate()?;
        let effect_is_tombstone = matches!(effect, SourceProjectionEffectV1::Tombstone(_));
        let mutation_is_tombstone =
            mutation.observation().content_state() == SourceContentStateV1::AuthoritativeDeleted;
        if effect.observation() != mutation.observation()
            || effect_is_tombstone != mutation_is_tombstone
        {
            return Err(SourceStoreErrorV1::RevisionConflict);
        }
        if mutation.predecessor().is_some() {
            expected_lineage.push(SourceObjectLineageV1::new(partition.clone(), mutation)?);
        }
    }
    for edge in lineage {
        edge.validate()?;
    }
    if lineage != expected_lineage {
        return Err(SourceStoreErrorV1::LineageConflict);
    }
    Ok(())
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourceCommitReceiptV1 {
    idempotency_key: ManifestDigest,
    request_digest: ManifestDigest,
    source_frontier: SourceAggregateFrontierV1,
    projection: SourceProjectionCommitV1,
    #[serde(skip)]
    verified: ValidationMemoV1,
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
            verified: ValidationMemoV1::default(),
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
        if self.verified.is_verified() {
            return Ok(());
        }
        self.idempotency_key.validate()?;
        self.request_digest.validate()?;
        self.source_frontier.validate()?;
        self.projection.validate()?;
        if self.projection.source_frontier() != &self.source_frontier {
            return Err(SourceStoreErrorV1::FrontierConflict);
        }
        self.verified.mark_verified();
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourceAuthorityPublicationV1 {
    definition: SourceDefinitionV1,
    binding: SourceBindingV1,
    expected_definition_digest: ManifestDigest,
    expected_binding_digest: ManifestDigest,
    idempotency_key: ManifestDigest,
    request_digest: ManifestDigest,
}

impl SourceAuthorityPublicationV1 {
    pub fn new(
        definition: &SourceDefinitionV1,
        binding: &SourceBindingV1,
        expected_definition_digest: ManifestDigest,
        expected_binding_digest: ManifestDigest,
        idempotency_key: ManifestDigest,
        request_digest: ManifestDigest,
    ) -> SourceStoreResult<Self> {
        let publication = Self {
            definition: definition.clone(),
            binding: binding.clone(),
            expected_definition_digest,
            expected_binding_digest,
            idempotency_key,
            request_digest,
        };
        publication.validate()?;
        Ok(publication)
    }

    pub fn validate(&self) -> SourceStoreResult<()> {
        self.definition.validate()?;
        self.binding.validate_against(&self.definition)?;
        self.expected_definition_digest.validate()?;
        self.expected_binding_digest.validate()?;
        self.idempotency_key.validate()?;
        self.request_digest.validate()?;
        Ok(())
    }

    pub fn binding(&self) -> &SourceBindingV1 {
        &self.binding
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourceAuthorityPublicationReceiptV1 {
    idempotency_key: ManifestDigest,
    request_digest: ManifestDigest,
    prior_definition_digest: ManifestDigest,
    prior_binding_digest: ManifestDigest,
    definition_digest: ManifestDigest,
    binding_digest: ManifestDigest,
}

impl SourceAuthorityPublicationReceiptV1 {
    pub fn idempotency_key(&self) -> &ManifestDigest {
        &self.idempotency_key
    }

    pub fn request_digest(&self) -> &ManifestDigest {
        &self.request_digest
    }

    pub fn definition_digest(&self) -> &ManifestDigest {
        &self.definition_digest
    }

    pub fn binding_digest(&self) -> &ManifestDigest {
        &self.binding_digest
    }

    fn validate(&self) -> SourceStoreResult<()> {
        self.idempotency_key.validate()?;
        self.request_digest.validate()?;
        self.prior_definition_digest.validate()?;
        self.prior_binding_digest.validate()?;
        self.definition_digest.validate()?;
        self.binding_digest.validate()?;
        if self.prior_definition_digest == self.definition_digest
            || self.prior_binding_digest == self.binding_digest
        {
            return Err(SourceStoreErrorV1::AuthorityRevisionConflict);
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
    definition_history: BTreeMap<u64, SourceDefinitionV1>,
    binding_history: BTreeMap<u64, SourceBindingV1>,
    authority_receipt_history: BTreeMap<ManifestDigest, SourceAuthorityPublicationReceiptV1>,
    source_frontier: SourceAggregateFrontierV1,
    projection: SourceProjectionCommitV1,
    projected_objects: BTreeMap<SourceNativeObjectIdV1, SourceObjectObservationV1>,
    object_partitions: BTreeMap<SourceNativeObjectIdV1, SourcePartitionIdV1>,
    revision_history: BTreeMap<SourceNativeObjectIdV1, Vec<SourceObjectMutationV1>>,
    lineage: Vec<SourceObjectLineageV1>,
    receipt_history: BTreeMap<ManifestDigest, SourceCommitReceiptV1>,
    receipt: SourceCommitReceiptV1,
    #[serde(skip)]
    verified: ValidationMemoV1,
}

impl SourceStoreStateV1 {
    /// Verify a freshly assembled successor state.
    ///
    /// The assembly paths start from a clone of an already-verified state and
    /// then replace fields, so the inherited memo describes the predecessor,
    /// not this value. Clearing it first makes this the successor's first
    /// contact and forces the full sweep; the components it carries over keep
    /// their own memos, so the sweep costs O(new records), not O(store).
    fn validated(mut self) -> SourceStoreResult<Self> {
        self.verified.clear();
        self.validate()?;
        Ok(self)
    }

    pub fn source_frontier(&self) -> &SourceAggregateFrontierV1 {
        &self.source_frontier
    }

    pub fn definition(&self) -> &SourceDefinitionV1 {
        &self.definition
    }

    pub fn binding(&self) -> &SourceBindingV1 {
        &self.binding
    }

    pub fn definition_history(&self) -> &BTreeMap<u64, SourceDefinitionV1> {
        &self.definition_history
    }

    pub fn binding_history(&self) -> &BTreeMap<u64, SourceBindingV1> {
        &self.binding_history
    }

    pub fn authority_receipts(
        &self,
    ) -> &BTreeMap<ManifestDigest, SourceAuthorityPublicationReceiptV1> {
        &self.authority_receipt_history
    }

    pub fn commit_receipts(&self) -> &BTreeMap<ManifestDigest, SourceCommitReceiptV1> {
        &self.receipt_history
    }

    pub fn projected_objects(
        &self,
    ) -> &BTreeMap<SourceNativeObjectIdV1, SourceObjectObservationV1> {
        &self.projected_objects
    }

    pub fn receipt(&self) -> &SourceCommitReceiptV1 {
        &self.receipt
    }

    pub fn receipt_by_idempotency_key(
        &self,
        idempotency_key: &ManifestDigest,
    ) -> Option<&SourceCommitReceiptV1> {
        self.receipt_history.get(idempotency_key)
    }

    pub fn object_partition(
        &self,
        native_object: &SourceNativeObjectIdV1,
    ) -> Option<&SourcePartitionIdV1> {
        self.object_partitions.get(native_object)
    }

    pub fn revision_history(
        &self,
        native_object: &SourceNativeObjectIdV1,
    ) -> Option<&[SourceObjectMutationV1]> {
        self.revision_history.get(native_object).map(Vec::as_slice)
    }

    pub fn lineage(&self) -> &[SourceObjectLineageV1] {
        &self.lineage
    }

    pub fn validate(&self) -> SourceStoreResult<()> {
        if self.verified.is_verified() {
            return Ok(());
        }
        self.definition.validate()?;
        self.binding.validate_against(&self.definition)?;
        if self.definition_history.get(&self.definition.revision) != Some(&self.definition)
            || self.binding_history.get(&self.binding.binding_revision) != Some(&self.binding)
            || self.definition_history.keys().next_back() != Some(&self.definition.revision)
            || self.binding_history.keys().next_back() != Some(&self.binding.binding_revision)
        {
            return Err(SourceStoreErrorV1::AuthorityRevisionConflict);
        }
        let mut prior_definition_revision = None;
        for (revision, definition) in &self.definition_history {
            definition.validate()?;
            if definition.source_id != self.definition.source_id
                || definition.revision != *revision
                || prior_definition_revision.is_some_and(|prior| *revision != prior + 1)
            {
                return Err(SourceStoreErrorV1::DefinitionConflict);
            }
            prior_definition_revision = Some(*revision);
        }
        let binding_identity = self.binding.immutable_identity()?;
        let mut prior_binding_revision = None;
        for (revision, binding) in &self.binding_history {
            binding.validate()?;
            if binding.immutable_identity()? != binding_identity
                || binding.binding_revision != *revision
                || self
                    .definition_history
                    .get(&binding.definition_revision)
                    .is_none_or(|definition| {
                        definition.definition_digest != binding.definition_digest
                    })
                || prior_binding_revision.is_some_and(|prior| *revision != prior + 1)
            {
                return Err(SourceStoreErrorV1::BindingConflict);
            }
            prior_binding_revision = Some(*revision);
        }
        for (key, receipt) in &self.authority_receipt_history {
            receipt.validate()?;
            if key != receipt.idempotency_key() {
                return Err(SourceStoreErrorV1::IdempotencyConflict);
            }
        }
        self.source_frontier.validate()?;
        self.projection.validate()?;
        self.receipt.validate()?;
        for (idempotency_key, receipt) in &self.receipt_history {
            idempotency_key.validate()?;
            receipt.validate()?;
            if receipt.idempotency_key() != idempotency_key
                || receipt.source_frontier().binding() != &binding_identity
                || receipt
                    .source_frontier()
                    .partitions()
                    .iter()
                    .any(|(partition, historical)| {
                        self.source_frontier
                            .partition(partition)
                            .is_none_or(|current| historical.sequence() > current.sequence())
                    })
            {
                return Err(SourceStoreErrorV1::IdempotencyConflict);
            }
        }
        if self.receipt_history.get(self.receipt.idempotency_key()) != Some(&self.receipt) {
            return Err(SourceStoreErrorV1::IdempotencyConflict);
        }
        if self.source_frontier.binding() != &binding_identity
            || self.projection.source_frontier() != &self.source_frontier
            || self.receipt.source_frontier() != &self.source_frontier
        {
            return Err(SourceStoreErrorV1::BindingConflict);
        }
        if self.source_frontier.partitions().len() > usize::from(self.definition.max_partitions) {
            return Err(SourceStoreErrorV1::TooManyPartitions);
        }
        let mut expected_lineage = Vec::new();
        for (native_object, observation) in &self.projected_objects {
            native_object.validate()?;
            observation.validate()?;
            if native_object != observation.native_object() {
                return Err(SourceStoreErrorV1::SnapshotCompletionMismatch);
            }
            let partition = self
                .object_partitions
                .get(native_object)
                .ok_or(SourceStoreErrorV1::ObjectPartitionConflict)?;
            let history = self
                .revision_history
                .get(native_object)
                .ok_or(SourceStoreErrorV1::RevisionConflict)?;
            if history.is_empty()
                || history.last().map(SourceObjectMutationV1::observation) != Some(observation)
            {
                return Err(SourceStoreErrorV1::RevisionConflict);
            }
            let mut revisions = BTreeSet::new();
            for (index, mutation) in history.iter().enumerate() {
                mutation.validate_against(&binding_identity, partition)?;
                if mutation.observation().native_object() != native_object
                    || !revisions.insert(mutation.observation().revision().digest().clone())
                {
                    return Err(SourceStoreErrorV1::RevisionConflict);
                }
                if index == 0 {
                    if mutation.transition() != SourceObjectTransitionV1::Initial
                        || mutation.predecessor().is_some()
                    {
                        return Err(SourceStoreErrorV1::LineageConflict);
                    }
                } else {
                    validate_transition(Some(history[index - 1].observation()), mutation)?;
                    expected_lineage.push(SourceObjectLineageV1::new(partition.clone(), mutation)?);
                }
            }
        }
        if self.projected_objects.len() != self.object_partitions.len()
            || self.projected_objects.len() != self.revision_history.len()
        {
            return Err(SourceStoreErrorV1::RevisionConflict);
        }
        for mutation in self.projection.mutations() {
            let native_object = mutation.observation().native_object();
            if self.projected_objects.get(native_object) != Some(mutation.observation())
                || self
                    .revision_history
                    .get(native_object)
                    .into_iter()
                    .flatten()
                    .filter(|stored| *stored == mutation)
                    .count()
                    != 1
            {
                return Err(SourceStoreErrorV1::RevisionConflict);
            }
        }
        for edge in &self.lineage {
            edge.validate()?;
            let partition = self
                .object_partitions
                .get(&edge.native_object)
                .ok_or(SourceStoreErrorV1::ObjectPartitionConflict)?;
            if partition != &edge.partition {
                return Err(SourceStoreErrorV1::ObjectPartitionConflict);
            }
        }
        if self.lineage.len() != expected_lineage.len()
            || expected_lineage
                .iter()
                .any(|expected| self.lineage.iter().filter(|edge| *edge == expected).count() != 1)
            || self
                .projection
                .lineage()
                .iter()
                .any(|edge| self.lineage.iter().filter(|stored| *stored == edge).count() != 1)
        {
            return Err(SourceStoreErrorV1::LineageConflict);
        }
        self.verified.mark_verified();
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SourceCommitApplyOutcomeV1 {
    Committed(Box<SourceStoreStateV1>),
    ExactDuplicate(Box<SourceCommitReceiptV1>),
}

pub fn apply_source_authority_publication(
    current: &SourceStoreStateV1,
    publication: SourceAuthorityPublicationV1,
) -> SourceStoreResult<SourceStoreStateV1> {
    current.validate()?;
    publication.validate()?;
    if let Some(receipt) = current
        .authority_receipt_history
        .get(&publication.idempotency_key)
    {
        return if receipt.request_digest() == &publication.request_digest {
            Ok(current.clone())
        } else {
            Err(SourceStoreErrorV1::IdempotencyConflict)
        };
    }
    if publication.binding.immutable_identity()? != current.binding.immutable_identity()? {
        return Err(SourceStoreErrorV1::BindingConflict);
    }
    if publication.expected_definition_digest != current.definition.definition_digest
        || publication.expected_binding_digest != current.binding.binding_digest
        || publication.definition.revision != current.definition.revision.saturating_add(1)
        || publication.binding.binding_revision
            != current.binding.binding_revision.saturating_add(1)
    {
        return Err(SourceStoreErrorV1::AuthorityRevisionConflict);
    }
    let receipt = SourceAuthorityPublicationReceiptV1 {
        idempotency_key: publication.idempotency_key.clone(),
        request_digest: publication.request_digest,
        prior_definition_digest: current.definition.definition_digest.clone(),
        prior_binding_digest: current.binding.binding_digest.clone(),
        definition_digest: publication.definition.definition_digest.clone(),
        binding_digest: publication.binding.binding_digest.clone(),
    };
    let mut next = current.clone();
    next.definition = publication.definition;
    next.binding = publication.binding;
    next.definition_history
        .insert(next.definition.revision, next.definition.clone());
    next.binding_history
        .insert(next.binding.binding_revision, next.binding.clone());
    next.authority_receipt_history
        .insert(receipt.idempotency_key.clone(), receipt);
    next.validated()
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
        if &current.definition != commit.definition() {
            return Err(SourceStoreErrorV1::DefinitionConflict);
        }
        if &current.binding != commit.binding() {
            return Err(SourceStoreErrorV1::BindingConflict);
        }
        if let Some(receipt) = current.receipt_by_idempotency_key(commit.idempotency_key()) {
            return if receipt.request_digest() == commit.request_digest() {
                Ok(SourceCommitApplyOutcomeV1::ExactDuplicate(Box::new(
                    receipt.clone(),
                )))
            } else {
                Err(SourceStoreErrorV1::IdempotencyConflict)
            };
        }
        if commit.expected_frontier() != Some(current.source_frontier()) {
            return Err(SourceStoreErrorV1::FrontierConflict);
        }
    } else if commit.expected_frontier().is_some() {
        return Err(SourceStoreErrorV1::FrontierConflict);
    }

    let mut projected_objects =
        current.map_or_else(BTreeMap::new, |state| state.projected_objects.clone());
    let mut object_partitions =
        current.map_or_else(BTreeMap::new, |state| state.object_partitions.clone());
    let mut revision_history =
        current.map_or_else(BTreeMap::new, |state| state.revision_history.clone());
    let mut lineage = current.map_or_else(Vec::new, |state| state.lineage.clone());
    let mut mutations = commit.mutations().to_vec();
    mutations.sort_by(|left, right| {
        left.observation()
            .native_object()
            .digest()
            .as_str()
            .cmp(right.observation().native_object().digest().as_str())
    });
    let mut effects = Vec::new();
    let mut committed_mutations = Vec::new();
    let mut committed_lineage = Vec::new();
    for mutation in mutations {
        apply_object_mutation(
            &commit,
            mutation,
            &mut projected_objects,
            &mut object_partitions,
            &mut revision_history,
            &mut lineage,
            &mut effects,
            &mut committed_mutations,
            &mut committed_lineage,
        )?;
    }
    if let Some(completion) = commit.snapshot_completion() {
        for native_object in completion.present_objects() {
            if object_partitions.get(native_object) != Some(completion.partition())
                || projected_objects
                    .get(native_object)
                    .is_none_or(|observation| {
                        observation.content_state() == SourceContentStateV1::AuthoritativeDeleted
                    })
            {
                return Err(SourceStoreErrorV1::ObjectPartitionConflict);
            }
        }
        if commit.definition().deletion_semantics
            == SourceDeletionSemanticsV1::CompleteSnapshotAbsence
        {
            let absent = projected_objects
                .iter()
                .filter(|(native_object, observation)| {
                    observation.content_state() != SourceContentStateV1::AuthoritativeDeleted
                        && object_partitions.get(*native_object) == Some(completion.partition())
                        && !completion.present_objects().contains(*native_object)
                })
                .map(|(native_object, _)| native_object.clone())
                .collect::<Vec<_>>();
            for native_object in absent {
                let prior = revision_history
                    .get(&native_object)
                    .and_then(|history| history.last())
                    .ok_or(SourceStoreErrorV1::RevisionConflict)?;
                let tombstone =
                    absence_tombstone(commit.binding().immutable_identity()?, completion, prior)?;
                apply_object_mutation(
                    &commit,
                    tombstone,
                    &mut projected_objects,
                    &mut object_partitions,
                    &mut revision_history,
                    &mut lineage,
                    &mut effects,
                    &mut committed_mutations,
                    &mut committed_lineage,
                )?;
            }
        }
    }
    let projection = SourceProjectionCommitV1::new(
        commit.projector().clone(),
        commit.next_frontier().clone(),
        committed_mutations,
        effects,
        committed_lineage,
    )?;
    let receipt = SourceCommitReceiptV1::new(
        commit.idempotency_key().clone(),
        commit.request_digest().clone(),
        commit.next_frontier().clone(),
        projection.clone(),
    )?;
    let mut receipt_history =
        current.map_or_else(BTreeMap::new, |state| state.receipt_history.clone());
    receipt_history.insert(receipt.idempotency_key().clone(), receipt.clone());
    let mut definition_history =
        current.map_or_else(BTreeMap::new, |state| state.definition_history.clone());
    definition_history.insert(commit.definition().revision, commit.definition().clone());
    let mut binding_history =
        current.map_or_else(BTreeMap::new, |state| state.binding_history.clone());
    binding_history.insert(commit.binding().binding_revision, commit.binding().clone());
    Ok(SourceCommitApplyOutcomeV1::Committed(Box::new(
        SourceStoreStateV1 {
            definition: commit.definition().clone(),
            binding: commit.binding().clone(),
            definition_history,
            binding_history,
            authority_receipt_history: current.map_or_else(BTreeMap::new, |state| {
                state.authority_receipt_history.clone()
            }),
            source_frontier: commit.next_frontier().clone(),
            projection,
            projected_objects,
            object_partitions,
            revision_history,
            lineage,
            receipt_history,
            receipt,
            verified: ValidationMemoV1::default(),
        }
        .validated()?,
    )))
}

#[allow(clippy::too_many_arguments)]
fn apply_object_mutation(
    commit: &SourceCommitV1,
    mutation: SourceObjectMutationV1,
    projected_objects: &mut BTreeMap<SourceNativeObjectIdV1, SourceObjectObservationV1>,
    object_partitions: &mut BTreeMap<SourceNativeObjectIdV1, SourcePartitionIdV1>,
    revision_history: &mut BTreeMap<SourceNativeObjectIdV1, Vec<SourceObjectMutationV1>>,
    lineage: &mut Vec<SourceObjectLineageV1>,
    effects: &mut Vec<SourceProjectionEffectV1>,
    committed_mutations: &mut Vec<SourceObjectMutationV1>,
    committed_lineage: &mut Vec<SourceObjectLineageV1>,
) -> SourceStoreResult<()> {
    let native_object = mutation.observation().native_object().clone();
    if let Some(owner) = object_partitions.get(&native_object)
        && owner != commit.partition()
    {
        return Err(SourceStoreErrorV1::ObjectPartitionConflict);
    }
    let prior = projected_objects.get(&native_object);
    if let Some(history) = revision_history.get(&native_object)
        && let Some(existing) = history
            .iter()
            .find(|existing| existing.observation().revision() == mutation.observation().revision())
    {
        return if existing == &mutation {
            Ok(())
        } else {
            Err(SourceStoreErrorV1::RevisionConflict)
        };
    }
    validate_transition(prior, &mutation)?;
    let edge = mutation
        .predecessor()
        .map(|_| SourceObjectLineageV1::new(commit.partition().clone(), &mutation))
        .transpose()?;
    let effect =
        if mutation.observation().content_state() == SourceContentStateV1::AuthoritativeDeleted {
            SourceProjectionEffectV1::Tombstone(mutation.observation().clone())
        } else {
            SourceProjectionEffectV1::Upsert(mutation.observation().clone())
        };
    object_partitions.insert(native_object.clone(), commit.partition().clone());
    projected_objects.insert(native_object.clone(), mutation.observation().clone());
    revision_history
        .entry(native_object)
        .or_default()
        .push(mutation.clone());
    committed_mutations.push(mutation);
    if let Some(edge) = edge {
        lineage.push(edge.clone());
        committed_lineage.push(edge);
    }
    effects.push(effect);
    Ok(())
}

fn validate_transition(
    prior: Option<&SourceObjectObservationV1>,
    mutation: &SourceObjectMutationV1,
) -> SourceStoreResult<()> {
    let next_deleted =
        mutation.observation().content_state() == SourceContentStateV1::AuthoritativeDeleted;
    match prior {
        None if mutation.transition() == SourceObjectTransitionV1::Initial
            && mutation.predecessor().is_none()
            && !next_deleted =>
        {
            Ok(())
        }
        Some(prior)
            if mutation.predecessor() == Some(prior.revision())
                && prior.revision() != mutation.observation().revision() =>
        {
            let prior_deleted = prior.content_state() == SourceContentStateV1::AuthoritativeDeleted;
            match (prior_deleted, next_deleted, mutation.transition()) {
                (false, false, SourceObjectTransitionV1::Successor)
                | (false, false, SourceObjectTransitionV1::Correction)
                | (false, true, SourceObjectTransitionV1::Tombstone)
                | (true, false, SourceObjectTransitionV1::Reappearance) => Ok(()),
                _ => Err(SourceStoreErrorV1::LineageConflict),
            }
        }
        _ => Err(SourceStoreErrorV1::LineageConflict),
    }
}

fn absence_tombstone(
    binding: SourceBindingIdentityV1,
    completion: &SourceSnapshotCompletionV1,
    prior: &SourceObjectMutationV1,
) -> SourceStoreResult<SourceObjectMutationV1> {
    let revision = SourceObjectRevisionV1::new(canonical_sha256(&(
        "tracedecay.external-source.absence-tombstone-revision.v1",
        prior.observation().revision(),
        completion.snapshot(),
    ))?);
    let digest = canonical_sha256(&(
        "tracedecay.external-source.absence-tombstone.v1",
        prior.observation().native_object(),
        &revision,
        completion.completion_digest(),
    ))?;
    let observation = SourceObjectObservationV1::new(
        prior.observation().native_object().clone(),
        revision,
        digest,
        SourceContentStateV1::AuthoritativeDeleted,
    )?;
    let evidence = SourceObservationEvidenceV1::new_internal(
        binding,
        completion.partition().clone(),
        &observation,
        prior.evidence().sanitization_receipt().clone(),
        prior.evidence().retrieval_anchor().clone(),
        prior.evidence().authorization().clone(),
        prior.evidence().source_authorization_digest().clone(),
        Some(completion.completion_digest().clone()),
    )?;
    SourceObjectMutationV1::new(
        observation,
        Some(prior.observation().revision().clone()),
        SourceObjectTransitionV1::Tombstone,
        evidence,
    )
}
