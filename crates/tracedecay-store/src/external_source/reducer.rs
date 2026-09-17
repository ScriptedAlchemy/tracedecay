use super::*;

/// Applies one source commit against the caller's previously read state. The
/// caller is responsible for placing this operation inside its authoritative
/// database transaction.
#[hotpath::measure(label = "store.external_source.apply_commit")]
pub fn apply_source_commit(
    current: Option<&SourceStoreStateV1>,
    commit: SourceCommitV1,
) -> SourceStoreResult<SourceCommitApplyOutcomeV1> {
    let outcome = reduce_source_commit(current.cloned(), commit);
    crate::hotpath_observe::record_source_commit_outcome(&outcome);
    outcome
}

/// Applies a commit while consuming the writer actor's verified state.
///
/// This preserves [`apply_source_commit`]'s validation and CAS semantics while
/// allowing the current-object maps to advance in place.
#[hotpath::measure(label = "store.external_source.apply_commit_owned")]
pub fn apply_source_commit_owned(
    current: Option<SourceStoreStateV1>,
    commit: SourceCommitV1,
) -> SourceStoreResult<SourceCommitApplyOutcomeV1> {
    let outcome = reduce_source_commit(current, commit);
    crate::hotpath_observe::record_source_commit_outcome(&outcome);
    outcome
}

#[hotpath::measure(label = "store.external_source.reduce_commit")]
fn reduce_source_commit(
    current: Option<SourceStoreStateV1>,
    commit: SourceCommitV1,
) -> SourceStoreResult<SourceCommitApplyOutcomeV1> {
    commit.validate()?;
    if let Some(current) = current.as_ref() {
        current.validate()?;
        if &current.definition != commit.definition() {
            return Err(SourceStoreErrorV1::DefinitionConflict);
        }
        if &current.binding != commit.binding() {
            return Err(SourceStoreErrorV1::BindingConflict);
        }
        if current.receipt().idempotency_key() == commit.idempotency_key() {
            return if current.receipt().request_digest() == commit.request_digest() {
                Ok(SourceCommitApplyOutcomeV1::ExactDuplicate(Box::new(
                    current.receipt().clone(),
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

    let (
        mut observed_objects,
        projected_objects,
        mut object_partitions,
        mut latest_mutations,
        projected_mutations,
        projection,
    ) = current.map_or_else(
        || {
            (
                BTreeMap::new(),
                BTreeMap::new(),
                BTreeMap::new(),
                BTreeMap::new(),
                BTreeMap::new(),
                None,
            )
        },
        |state| {
            (
                state.observed_objects,
                state.projected_objects,
                state.object_partitions,
                state.latest_mutations,
                state.projected_mutations,
                state.projection,
            )
        },
    );
    let mut mutations = commit.mutations().to_vec();
    mutations.sort_by(|left, right| {
        left.observation()
            .native_object()
            .digest()
            .as_str()
            .cmp(right.observation().native_object().digest().as_str())
    });
    let mut committed_mutations = Vec::new();
    let mut committed_lineage = Vec::new();
    for mutation in mutations {
        apply_object_mutation(
            &commit,
            mutation,
            &mut observed_objects,
            &mut object_partitions,
            &mut latest_mutations,
            &mut committed_mutations,
            &mut committed_lineage,
        )?;
    }
    if let Some(completion) = commit.snapshot_completion() {
        for native_object in completion.present_objects() {
            if object_partitions.get(native_object) != Some(completion.partition())
                || observed_objects
                    .get(native_object)
                    .is_none_or(|observation| {
                        observation.content_state() == SourceContentStateV1::AuthoritativeDeleted
                    })
            {
                return Err(SourceStoreErrorV1::ObjectPartitionConflict);
            }
        }
    }
    let receipt = SourceCommitReceiptV1::new(&commit, committed_mutations, committed_lineage)?;
    Ok(SourceCommitApplyOutcomeV1::Committed(Box::new(
        SourceStoreStateV1 {
            definition: commit.definition().clone(),
            binding: commit.binding().clone(),
            source_frontier: commit.next_frontier().clone(),
            projection,
            observed_objects,
            projected_objects,
            object_partitions,
            latest_mutations,
            projected_mutations,
            receipt,
            verified: ValidationMemoV1::default(),
        }
        .verified_successor(),
    )))
}

#[allow(clippy::too_many_arguments)]
#[hotpath::measure(label = "store.external_source.apply_object_mutation")]
fn apply_object_mutation(
    commit: &SourceCommitV1,
    mutation: SourceObjectMutationV1,
    observed_objects: &mut BTreeMap<SourceNativeObjectIdV1, SourceObjectObservationV1>,
    object_partitions: &mut BTreeMap<SourceNativeObjectIdV1, SourcePartitionIdV1>,
    latest_mutations: &mut BTreeMap<SourceNativeObjectIdV1, SourceObjectMutationV1>,
    committed_mutations: &mut Vec<SourceObjectMutationV1>,
    committed_lineage: &mut Vec<SourceObjectLineageV1>,
) -> SourceStoreResult<()> {
    let native_object = mutation.observation().native_object().clone();
    if let Some(owner) = object_partitions.get(&native_object)
        && owner != commit.partition()
    {
        return Err(SourceStoreErrorV1::ObjectPartitionConflict);
    }
    let prior = observed_objects.get(&native_object);
    if let Some(existing) = latest_mutations.get(&native_object)
        && existing.observation().revision() == mutation.observation().revision()
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
    object_partitions.insert(native_object.clone(), commit.partition().clone());
    observed_objects.insert(native_object.clone(), mutation.observation().clone());
    latest_mutations.insert(native_object, mutation.clone());
    committed_mutations.push(mutation);
    if let Some(edge) = edge {
        committed_lineage.push(edge);
    }
    Ok(())
}

#[hotpath::measure(label = "store.external_source.validate_transition")]
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

#[hotpath::measure(label = "store.external_source.absence_tombstone")]
pub(super) fn absence_tombstone(
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
