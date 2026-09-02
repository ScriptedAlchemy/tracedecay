use super::*;

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
    definition_revision: u64,
    definition_digest: ManifestDigest,
    binding_revision: u64,
    binding_digest: ManifestDigest,
    expected_projection_frontier: Option<SourceAggregateFrontierV1>,
    source_frontier: SourceAggregateFrontierV1,
    source_receipt_digest: ManifestDigest,
    mutations: Vec<SourceObjectMutationV1>,
    effects: Vec<SourceProjectionEffectV1>,
    lineage: Vec<SourceObjectLineageV1>,
    receipt_digest: ManifestDigest,
    #[serde(skip)]
    verified: ValidationMemoV1,
}

impl SourceProjectionCommitV1 {
    pub(super) fn new(
        projector: ComponentVersion,
        pending: &SourcePendingProjectionV1,
        mutations: Vec<SourceObjectMutationV1>,
        effects: Vec<SourceProjectionEffectV1>,
        lineage: Vec<SourceObjectLineageV1>,
    ) -> SourceStoreResult<Self> {
        let definition = &pending.definition;
        let binding = &pending.binding;
        let expected_projection_frontier = pending.expected_projection_frontier.clone();
        let source_frontier = pending.receipt.source_frontier().clone();
        let source_receipt_digest = pending.receipt.receipt_digest().clone();
        projector.validate()?;
        definition.validate()?;
        binding.validate_against(definition)?;
        source_frontier.validate()?;
        source_receipt_digest.validate()?;
        validate_projection_payload(&source_frontier, &mutations, &effects, &lineage)?;
        let receipt_digest = canonical_sha256(&(
            "tracedecay.external-source.projection-commit.v1",
            &projector,
            definition.revision,
            &definition.definition_digest,
            binding.binding_revision,
            &binding.binding_digest,
            &expected_projection_frontier,
            &source_frontier,
            &source_receipt_digest,
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
            definition_revision: definition.revision,
            definition_digest: definition.definition_digest.clone(),
            binding_revision: binding.binding_revision,
            binding_digest: binding.binding_digest.clone(),
            expected_projection_frontier,
            source_frontier,
            source_receipt_digest,
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

    pub fn expected_projection_frontier(&self) -> Option<&SourceAggregateFrontierV1> {
        self.expected_projection_frontier.as_ref()
    }

    pub fn source_receipt_digest(&self) -> &ManifestDigest {
        &self.source_receipt_digest
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
        self.definition_digest.validate()?;
        self.binding_digest.validate()?;
        self.source_frontier.validate()?;
        self.source_receipt_digest.validate()?;
        if self.definition_revision == 0 || self.binding_revision == 0 {
            return Err(SourceStoreErrorV1::AuthorityRevisionConflict);
        }
        if self
            .expected_projection_frontier
            .as_ref()
            .is_some_and(|frontier| frontier.binding() != self.source_frontier.binding())
        {
            return Err(SourceStoreErrorV1::BindingConflict);
        }
        validate_projection_payload(
            &self.source_frontier,
            &self.mutations,
            &self.effects,
            &self.lineage,
        )?;
        let expected = canonical_sha256(&(
            "tracedecay.external-source.projection-commit.v1",
            &self.projector,
            self.definition_revision,
            &self.definition_digest,
            self.binding_revision,
            &self.binding_digest,
            &self.expected_projection_frontier,
            &self.source_frontier,
            &self.source_receipt_digest,
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
