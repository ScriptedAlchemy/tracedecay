//! Store contracts for normalized external-source commits.
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

mod acquisition;
pub use acquisition::{
    MAX_SOURCE_ACQUISITION_ATTEMPTS_V1, MAX_SOURCE_ACQUISITION_RECEIPTS_V1,
    SourceAcquisitionQueueCasV1, SourceAcquisitionQueueContractErrorV1,
    SourceAcquisitionQueueResultV1, SourceAcquisitionQueueStateV1, SourceAcquisitionRequestV1,
    SourceScheduledRefetchV1,
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

    pub fn validate(&self) -> SourceStoreResult<()> {
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

mod projection;
pub use projection::{SourceProjectionCommitV1, SourceProjectionEffectV1};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourceCommitReceiptV1 {
    idempotency_key: ManifestDigest,
    request_digest: ManifestDigest,
    definition_revision: u64,
    definition_digest: ManifestDigest,
    binding_revision: u64,
    binding_digest: ManifestDigest,
    prior_source_frontier: Option<SourceAggregateFrontierV1>,
    source_frontier: SourceAggregateFrontierV1,
    partition: SourcePartitionIdV1,
    mutations: Vec<SourceObjectMutationV1>,
    lineage: Vec<SourceObjectLineageV1>,
    snapshot_completion: Option<SourceSnapshotCompletionV1>,
    receipt_digest: ManifestDigest,
    #[serde(skip)]
    verified: ValidationMemoV1,
}

impl SourceCommitReceiptV1 {
    fn new(
        commit: &SourceCommitV1,
        mutations: Vec<SourceObjectMutationV1>,
        lineage: Vec<SourceObjectLineageV1>,
    ) -> SourceStoreResult<Self> {
        let idempotency_key = commit.idempotency_key().clone();
        let request_digest = commit.request_digest().clone();
        let definition = commit.definition();
        let binding = commit.binding();
        let prior_source_frontier = commit.expected_frontier().cloned();
        let source_frontier = commit.next_frontier().clone();
        let partition = commit.partition().clone();
        let snapshot_completion = commit.snapshot_completion().cloned();
        let receipt_digest = canonical_sha256(&(
            "tracedecay.external-source.source-commit-receipt.v1",
            &idempotency_key,
            &request_digest,
            definition.revision,
            &definition.definition_digest,
            binding.binding_revision,
            &binding.binding_digest,
            &prior_source_frontier,
            &source_frontier,
            &partition,
            &mutations,
            &lineage,
            &snapshot_completion,
        ))?;
        let receipt = Self {
            idempotency_key,
            request_digest,
            definition_revision: definition.revision,
            definition_digest: definition.definition_digest.clone(),
            binding_revision: binding.binding_revision,
            binding_digest: binding.binding_digest.clone(),
            prior_source_frontier,
            source_frontier,
            partition,
            mutations,
            lineage,
            snapshot_completion,
            receipt_digest,
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

    pub fn definition_revision(&self) -> u64 {
        self.definition_revision
    }

    pub fn definition_digest(&self) -> &ManifestDigest {
        &self.definition_digest
    }

    pub fn binding_revision(&self) -> u64 {
        self.binding_revision
    }

    pub fn binding_digest(&self) -> &ManifestDigest {
        &self.binding_digest
    }

    pub fn source_frontier(&self) -> &SourceAggregateFrontierV1 {
        &self.source_frontier
    }

    pub fn prior_source_frontier(&self) -> Option<&SourceAggregateFrontierV1> {
        self.prior_source_frontier.as_ref()
    }

    pub fn partition(&self) -> &SourcePartitionIdV1 {
        &self.partition
    }

    pub fn mutations(&self) -> &[SourceObjectMutationV1] {
        &self.mutations
    }

    pub fn lineage(&self) -> &[SourceObjectLineageV1] {
        &self.lineage
    }

    pub fn snapshot_completion(&self) -> Option<&SourceSnapshotCompletionV1> {
        self.snapshot_completion.as_ref()
    }

    pub fn receipt_digest(&self) -> &ManifestDigest {
        &self.receipt_digest
    }

    pub fn validate(&self) -> SourceStoreResult<()> {
        if self.verified.is_verified() {
            return Ok(());
        }
        self.idempotency_key.validate()?;
        self.request_digest.validate()?;
        self.definition_digest.validate()?;
        self.binding_digest.validate()?;
        if self.definition_revision == 0 || self.binding_revision == 0 {
            return Err(SourceStoreErrorV1::AuthorityRevisionConflict);
        }
        self.source_frontier.validate()?;
        self.partition.validate()?;
        if self.source_frontier.partition(&self.partition).is_none()
            || self
                .prior_source_frontier
                .as_ref()
                .is_some_and(|frontier| frontier.binding() != self.source_frontier.binding())
        {
            return Err(SourceStoreErrorV1::FrontierConflict);
        }
        for mutation in &self.mutations {
            mutation.validate_against(self.source_frontier.binding(), &self.partition)?;
        }
        for edge in &self.lineage {
            edge.validate()?;
        }
        let expected = canonical_sha256(&(
            "tracedecay.external-source.source-commit-receipt.v1",
            &self.idempotency_key,
            &self.request_digest,
            self.definition_revision,
            &self.definition_digest,
            self.binding_revision,
            &self.binding_digest,
            &self.prior_source_frontier,
            &self.source_frontier,
            &self.partition,
            &self.mutations,
            &self.lineage,
            &self.snapshot_completion,
        ))?;
        if expected != self.receipt_digest {
            return Err(SourceStoreErrorV1::Domain(DomainError::DigestMismatch));
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

    pub fn idempotency_key(&self) -> &ManifestDigest {
        &self.idempotency_key
    }

    pub fn request_digest(&self) -> &ManifestDigest {
        &self.request_digest
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

    pub fn validate(&self) -> SourceStoreResult<()> {
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
    source_frontier: SourceAggregateFrontierV1,
    projection: Option<SourceProjectionCommitV1>,
    observed_objects: BTreeMap<SourceNativeObjectIdV1, SourceObjectObservationV1>,
    projected_objects: BTreeMap<SourceNativeObjectIdV1, SourceObjectObservationV1>,
    object_partitions: BTreeMap<SourceNativeObjectIdV1, SourcePartitionIdV1>,
    latest_mutations: BTreeMap<SourceNativeObjectIdV1, SourceObjectMutationV1>,
    projected_mutations: BTreeMap<SourceNativeObjectIdV1, SourceObjectMutationV1>,
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

    pub fn restore(
        definition: SourceDefinitionV1,
        binding: SourceBindingV1,
        source_frontier: SourceAggregateFrontierV1,
        projection: Option<SourceProjectionCommitV1>,
        observed_mutations: Vec<SourceObjectMutationV1>,
        projected_mutations: Vec<SourceObjectMutationV1>,
        receipt: SourceCommitReceiptV1,
    ) -> SourceStoreResult<Self> {
        let binding_identity = binding.immutable_identity()?;
        let mut observed_objects = BTreeMap::new();
        let mut object_partitions = BTreeMap::new();
        let mut latest_mutations = BTreeMap::new();
        for mutation in observed_mutations {
            let native_object = mutation.observation().native_object().clone();
            mutation.validate_against(&binding_identity, mutation.evidence().partition())?;
            if observed_objects
                .insert(native_object.clone(), mutation.observation().clone())
                .is_some()
                || object_partitions
                    .insert(
                        native_object.clone(),
                        mutation.evidence().partition().clone(),
                    )
                    .is_some()
                || latest_mutations.insert(native_object, mutation).is_some()
            {
                return Err(SourceStoreErrorV1::DuplicateNativeObject);
            }
        }
        let mut projected_objects = BTreeMap::new();
        let mut current_projected_mutations = BTreeMap::new();
        for mutation in projected_mutations {
            let native_object = mutation.observation().native_object().clone();
            mutation.validate_against(&binding_identity, mutation.evidence().partition())?;
            if projected_objects
                .insert(native_object.clone(), mutation.observation().clone())
                .is_some()
                || current_projected_mutations
                    .insert(native_object, mutation)
                    .is_some()
            {
                return Err(SourceStoreErrorV1::DuplicateNativeObject);
            }
        }
        Self {
            definition,
            binding,
            source_frontier,
            projection,
            observed_objects,
            projected_objects,
            object_partitions,
            latest_mutations,
            projected_mutations: current_projected_mutations,
            receipt,
            verified: ValidationMemoV1::default(),
        }
        .validated()
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

    pub fn projected_objects(
        &self,
    ) -> &BTreeMap<SourceNativeObjectIdV1, SourceObjectObservationV1> {
        &self.projected_objects
    }

    pub fn observed_objects(&self) -> &BTreeMap<SourceNativeObjectIdV1, SourceObjectObservationV1> {
        &self.observed_objects
    }

    pub fn projection(&self) -> Option<&SourceProjectionCommitV1> {
        self.projection.as_ref()
    }

    pub fn receipt(&self) -> &SourceCommitReceiptV1 {
        &self.receipt
    }

    pub fn object_partition(
        &self,
        native_object: &SourceNativeObjectIdV1,
    ) -> Option<&SourcePartitionIdV1> {
        self.object_partitions.get(native_object)
    }

    pub fn latest_mutation(
        &self,
        native_object: &SourceNativeObjectIdV1,
    ) -> Option<&SourceObjectMutationV1> {
        self.latest_mutations.get(native_object)
    }

    pub fn projected_mutation(
        &self,
        native_object: &SourceNativeObjectIdV1,
    ) -> Option<&SourceObjectMutationV1> {
        self.projected_mutations.get(native_object)
    }

    pub fn validate(&self) -> SourceStoreResult<()> {
        if self.verified.is_verified() {
            return Ok(());
        }
        self.definition.validate()?;
        self.binding.validate_against(&self.definition)?;
        let binding = self.binding.immutable_identity()?;
        self.source_frontier.validate()?;
        self.receipt.validate()?;
        if self.source_frontier.binding() != &binding
            || self.receipt.source_frontier() != &self.source_frontier
        {
            return Err(SourceStoreErrorV1::BindingConflict);
        }
        if self.source_frontier.partitions().len() > usize::from(self.definition.max_partitions) {
            return Err(SourceStoreErrorV1::TooManyPartitions);
        }
        if self.observed_objects.len() != self.object_partitions.len()
            || self.observed_objects.len() != self.latest_mutations.len()
        {
            return Err(SourceStoreErrorV1::RevisionConflict);
        }
        for (native_object, observation) in &self.observed_objects {
            let partition = self
                .object_partitions
                .get(native_object)
                .ok_or(SourceStoreErrorV1::ObjectPartitionConflict)?;
            let mutation = self
                .latest_mutations
                .get(native_object)
                .ok_or(SourceStoreErrorV1::RevisionConflict)?;
            mutation.validate_against(&binding, partition)?;
            if mutation.observation() != observation {
                return Err(SourceStoreErrorV1::RevisionConflict);
            }
        }
        if self.projected_objects.len() != self.projected_mutations.len() {
            return Err(SourceStoreErrorV1::RevisionConflict);
        }
        for (native_object, observation) in &self.projected_objects {
            let mutation = self
                .projected_mutations
                .get(native_object)
                .ok_or(SourceStoreErrorV1::RevisionConflict)?;
            mutation.validate_against(&binding, mutation.evidence().partition())?;
            if mutation.observation() != observation {
                return Err(SourceStoreErrorV1::RevisionConflict);
            }
        }
        match &self.projection {
            None if !self.projected_objects.is_empty() => {
                return Err(SourceStoreErrorV1::FrontierConflict);
            }
            Some(projection)
                if projection.source_frontier().binding() != &binding
                    || frontier_is_ahead(projection.source_frontier(), &self.source_frontier) =>
            {
                return Err(SourceStoreErrorV1::FrontierConflict);
            }
            Some(projection) => projection.validate()?,
            None => {}
        }
        self.verified.mark_verified();
        Ok(())
    }
}

fn frontier_is_ahead(
    candidate: &SourceAggregateFrontierV1,
    current: &SourceAggregateFrontierV1,
) -> bool {
    candidate.partitions().iter().any(|(partition, candidate)| {
        current
            .partition(partition)
            .is_none_or(|current| candidate.sequence() > current.sequence())
    })
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourcePendingProjectionV1 {
    definition: SourceDefinitionV1,
    binding: SourceBindingV1,
    receipt: SourceCommitReceiptV1,
    expected_projection_frontier: Option<SourceAggregateFrontierV1>,
    projected_mutations: BTreeMap<SourceNativeObjectIdV1, SourceObjectMutationV1>,
}

impl SourcePendingProjectionV1 {
    pub fn new(
        definition: SourceDefinitionV1,
        binding: SourceBindingV1,
        receipt: SourceCommitReceiptV1,
        expected_projection_frontier: Option<SourceAggregateFrontierV1>,
        projected_mutations: Vec<SourceObjectMutationV1>,
    ) -> SourceStoreResult<Self> {
        let mut current = BTreeMap::new();
        for mutation in projected_mutations {
            if current
                .insert(mutation.observation().native_object().clone(), mutation)
                .is_some()
            {
                return Err(SourceStoreErrorV1::DuplicateNativeObject);
            }
        }
        let pending = Self {
            definition,
            binding,
            receipt,
            expected_projection_frontier,
            projected_mutations: current,
        };
        pending.validate()?;
        Ok(pending)
    }

    pub fn from_state(
        state: &SourceStoreStateV1,
        definition: SourceDefinitionV1,
        binding: SourceBindingV1,
        receipt: SourceCommitReceiptV1,
    ) -> SourceStoreResult<Self> {
        Self::new(
            definition,
            binding,
            receipt,
            state
                .projection()
                .map(|projection| projection.source_frontier().clone()),
            state.projected_mutations.values().cloned().collect(),
        )
    }

    pub fn binding_identity(&self) -> SourceStoreResult<SourceBindingIdentityV1> {
        self.binding.immutable_identity().map_err(Into::into)
    }

    pub fn receipt(&self) -> &SourceCommitReceiptV1 {
        &self.receipt
    }

    pub fn expected_projection_frontier(&self) -> Option<&SourceAggregateFrontierV1> {
        self.expected_projection_frontier.as_ref()
    }

    pub fn projected_mutations(&self) -> &BTreeMap<SourceNativeObjectIdV1, SourceObjectMutationV1> {
        &self.projected_mutations
    }

    pub fn validate(&self) -> SourceStoreResult<()> {
        self.definition.validate()?;
        self.binding.validate_against(&self.definition)?;
        self.receipt.validate()?;
        let binding = self.binding.immutable_identity()?;
        if self.receipt.definition_revision() != self.definition.revision
            || self.receipt.definition_digest() != &self.definition.definition_digest
            || self.receipt.binding_revision() != self.binding.binding_revision
            || self.receipt.binding_digest() != &self.binding.binding_digest
        {
            return Err(SourceStoreErrorV1::AuthorityRevisionConflict);
        }
        if self.receipt.source_frontier().binding() != &binding
            || self.receipt.prior_source_frontier() != self.expected_projection_frontier.as_ref()
        {
            return Err(SourceStoreErrorV1::FrontierConflict);
        }
        for mutation in self.projected_mutations.values() {
            mutation.validate_against(&binding, mutation.evidence().partition())?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SourceCommitApplyOutcomeV1 {
    Committed(Box<SourceStoreStateV1>),
    ExactDuplicate(Box<SourceCommitReceiptV1>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SourceProjectionApplyOutcomeV1 {
    Projected(Box<SourceStoreStateV1>),
    ExactDuplicate(Box<SourceProjectionCommitV1>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceAuthorityPublicationApplyOutcomeV1 {
    state: Box<SourceStoreStateV1>,
    receipt: SourceAuthorityPublicationReceiptV1,
}

impl SourceAuthorityPublicationApplyOutcomeV1 {
    pub fn into_parts(self) -> (Box<SourceStoreStateV1>, SourceAuthorityPublicationReceiptV1) {
        (self.state, self.receipt)
    }
}

/// Derives the next view transition from the exact oldest pending receipt.
///
/// This function is pure. Publishing the returned transition is a separate
/// compare-and-set operation through [`apply_source_projection`].
pub fn build_source_projection(
    pending: &SourcePendingProjectionV1,
    projector: ComponentVersion,
) -> SourceStoreResult<SourceProjectionCommitV1> {
    pending.validate()?;
    projector.validate()?;
    let receipt = pending.receipt();
    let mut mutations = receipt.mutations().to_vec();
    let mut effects = mutations
        .iter()
        .map(|mutation| {
            if mutation.observation().content_state() == SourceContentStateV1::AuthoritativeDeleted
            {
                SourceProjectionEffectV1::Tombstone(mutation.observation().clone())
            } else {
                SourceProjectionEffectV1::Upsert(mutation.observation().clone())
            }
        })
        .collect::<Vec<_>>();
    let mut lineage = receipt.lineage().to_vec();
    if pending.definition.deletion_semantics == SourceDeletionSemanticsV1::CompleteSnapshotAbsence
        && let Some(completion) = receipt.snapshot_completion()
    {
        let absent = pending
            .projected_mutations
            .iter()
            .filter(|(native_object, mutation)| {
                mutation.observation().content_state() != SourceContentStateV1::AuthoritativeDeleted
                    && mutation.evidence().partition() == completion.partition()
                    && !completion.present_objects().contains(*native_object)
            })
            .map(|(native_object, _)| native_object.clone())
            .collect::<Vec<_>>();
        for native_object in absent {
            let prior = pending
                .projected_mutations
                .get(&native_object)
                .ok_or(SourceStoreErrorV1::RevisionConflict)?;
            let mutation =
                absence_tombstone(pending.binding.immutable_identity()?, completion, prior)?;
            lineage.push(SourceObjectLineageV1::new(
                completion.partition().clone(),
                &mutation,
            )?);
            effects.push(SourceProjectionEffectV1::Tombstone(
                mutation.observation().clone(),
            ));
            mutations.push(mutation);
        }
    }
    SourceProjectionCommitV1::new(projector, pending, mutations, effects, lineage)
}

/// Publishes one deterministic projection transition with exact source and
/// prior-projection compare-and-set semantics.
pub fn apply_source_projection(
    current: &SourceStoreStateV1,
    pending: &SourcePendingProjectionV1,
    projection: SourceProjectionCommitV1,
) -> SourceStoreResult<SourceProjectionApplyOutcomeV1> {
    current.validate()?;
    pending.validate()?;
    projection.validate()?;
    if let Some(existing) = current.projection()
        && existing.receipt_digest() == projection.receipt_digest()
    {
        return if existing == &projection {
            Ok(SourceProjectionApplyOutcomeV1::ExactDuplicate(Box::new(
                existing.clone(),
            )))
        } else {
            Err(SourceStoreErrorV1::IdempotencyConflict)
        };
    }
    if pending.binding_identity()? != current.binding.immutable_identity()?
        || pending.receipt().source_frontier() != projection.source_frontier()
        || frontier_is_ahead(projection.source_frontier(), current.source_frontier())
        || current.projected_mutations != pending.projected_mutations
    {
        return Err(SourceStoreErrorV1::FrontierConflict);
    }
    if current
        .projection()
        .map(SourceProjectionCommitV1::source_frontier)
        != projection.expected_projection_frontier()
    {
        return Err(SourceStoreErrorV1::FrontierConflict);
    }
    if pending.receipt().receipt_digest() != projection.source_receipt_digest() {
        return Err(SourceStoreErrorV1::IdempotencyConflict);
    }
    let expected = build_source_projection(pending, projection.projector().clone())?;
    if expected != projection {
        return Err(SourceStoreErrorV1::RevisionConflict);
    }
    let mut next = current.clone();
    for (mutation, effect) in projection.mutations().iter().zip(projection.effects()) {
        next.projected_objects.insert(
            effect.observation().native_object().clone(),
            effect.observation().clone(),
        );
        next.projected_mutations.insert(
            mutation.observation().native_object().clone(),
            mutation.clone(),
        );
    }
    next.projection = Some(projection);
    Ok(SourceProjectionApplyOutcomeV1::Projected(Box::new(
        next.validated()?,
    )))
}

pub fn apply_source_authority_publication(
    current: &SourceStoreStateV1,
    publication: SourceAuthorityPublicationV1,
) -> SourceStoreResult<SourceAuthorityPublicationApplyOutcomeV1> {
    current.validate()?;
    publication.validate()?;
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
    Ok(SourceAuthorityPublicationApplyOutcomeV1 {
        state: Box::new(next.validated()?),
        receipt,
    })
}

mod reducer;
use reducer::absence_tombstone;
pub use reducer::apply_source_commit;
