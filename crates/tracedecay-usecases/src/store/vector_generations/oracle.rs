impl FakeVectorGenerationStoreV1 {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn begin_generation(
        &mut self,
        plan: VectorGenerationPlanV1,
    ) -> Result<VectorGenerationBuildIdV1, VectorGenerationStoreErrorV1> {
        validate_plan(&plan)?;
        if let Some(base_id) = &plan.base_generation {
            self.published
                .generations
                .get(base_id)
                .ok_or(VectorGenerationStoreErrorV1::IncompatibleBaseGeneration)?;
        }
        let (build_id, staged) = new_staged_generation(plan.clone())?;
        if let Some(existing) = self.staged.get(&build_id) {
            if existing.plan == plan {
                return Ok(build_id);
            }
            return Err(VectorGenerationStoreErrorV1::InvalidPlan(
                "build identity collision".to_string(),
            ));
        }
        self.staged.insert(build_id.clone(), staged);
        Ok(build_id)
    }

    /// Discard any checkpointed execution for the same deterministic build
    /// identity and restart projection from its authoritative query inputs.
    /// Already-published generations and the active pointer are untouched.
    pub fn rebuild_generation(
        &mut self,
        plan: VectorGenerationPlanV1,
    ) -> Result<VectorGenerationBuildIdV1, VectorGenerationStoreErrorV1> {
        let build_id = self.begin_generation(plan.clone())?;
        let checkpoint = VectorProjectionCheckpointV1 {
            target_projection_key: plan.target_projection_key.clone(),
            source_generation: plan.source_generation.clone(),
            source_manifest_digest: plan.source_manifest_digest.clone(),
            completed_batches: 0,
            last_request_digest: None,
            last_publication_digest: None,
        };
        self.staged.insert(
            build_id.clone(),
            StagedVectorGenerationV1 {
                plan,
                embedding_key: None,
                vectors: ExternalV1::default(),
                tombstones: ExternalV1::default(),
                tombstone_ids: ExternalV1::default(),
                batches: ExternalV1::default(),
                receipts: ExternalV1::default(),
                committed_chunk_effects: ExternalV1::default(),
                checkpoint,
            },
        );
        Ok(build_id)
    }

    /// Discard one unpublished build without changing any immutable
    /// generation or the active pointer. This is the cancellation boundary
    /// for asynchronous projection work.
    pub fn cancel_generation(&mut self, build_id: &VectorGenerationBuildIdV1) -> bool {
        self.staged.remove(build_id).is_some()
    }

    /// Atomically commit one batch's vector effects, tombstones, Plan 25
    /// receipt, and next checkpoint. Any validation failure leaves the prior
    /// staged state and checkpoint unchanged.
    pub fn commit_batch(
        &mut self,
        build_id: &VectorGenerationBuildIdV1,
        expected_checkpoint: Option<&VectorProjectionCheckpointV1>,
        prepared: PreparedVectorGenerationV1,
    ) -> Result<VectorProjectionCheckpointV1, VectorGenerationStoreErrorV1> {
        self.commit_batch_ref(build_id, expected_checkpoint, &prepared)
    }

    /// Borrowing form of [`Self::commit_batch`]. The persistent adapter drives
    /// this one so a whole-corpus batch is never copied just to satisfy a
    /// retryable mutation closure.
    pub(crate) fn commit_batch_ref(
        &mut self,
        build_id: &VectorGenerationBuildIdV1,
        expected_checkpoint: Option<&VectorProjectionCheckpointV1>,
        prepared: &PreparedVectorGenerationV1,
    ) -> Result<VectorProjectionCheckpointV1, VectorGenerationStoreErrorV1> {
        let current = self
            .staged
            .get(build_id)
            .cloned()
            .ok_or(VectorGenerationStoreErrorV1::UnknownBuild)?;
        if let Some(existing) = current
            .batches
            .iter()
            .find(|batch| batch.request.request_digest == prepared.request.request_digest)
        {
            if existing == prepared {
                return Ok(current.checkpoint);
            }
            return Err(VectorGenerationStoreErrorV1::ConflictingBatchReplay);
        }
        if current.checkpoint.completed_batches == 0 {
            if expected_checkpoint.is_some() {
                return Err(VectorGenerationStoreErrorV1::StaleCheckpoint);
            }
        } else if expected_checkpoint != Some(&current.checkpoint) {
            return Err(VectorGenerationStoreErrorV1::StaleCheckpoint);
        }

        validate_batch_identity(&current.plan, prepared)?;
        validate_base_generation_for_batch(&self.published, &current.plan, prepared)?;
        verify_batch_receipt(&prepared.request, &prepared.receipt)
            .map_err(SemanticProjectionErrorV1::from)?;
        let mut next = current;
        if let Some(key) = &next.embedding_key {
            if key != &prepared.embedding_key {
                return Err(VectorGenerationStoreErrorV1::BatchIdentityMismatch);
            }
        } else {
            next.embedding_key = Some(prepared.embedding_key.clone());
        }

        let vector_by_chunk = prepared
            .vectors
            .iter()
            .map(|vector| (vector.chunk_id.clone(), vector))
            .collect::<BTreeMap<_, _>>();
        let tombstone_by_chunk = prepared
            .tombstones
            .iter()
            .map(|tombstone| (tombstone.chunk_id.clone(), tombstone))
            .collect::<BTreeMap<_, _>>();
        if vector_by_chunk.len() != prepared.vectors.len()
            || tombstone_by_chunk.len() != prepared.tombstones.len()
        {
            return Err(VectorGenerationStoreErrorV1::BatchIdentityMismatch);
        }

        for receipt in &prepared.receipt.receipts {
            if !next
                .committed_chunk_effects
                .insert(receipt.chunk_id.clone())
            {
                return Err(VectorGenerationStoreErrorV1::DuplicateChunkEffect(
                    receipt.chunk_id.clone(),
                ));
            }
            match receipt.operation {
                ProjectionOperationV1::Added | ProjectionOperationV1::Updated => {
                    let vector = vector_by_chunk.get(&receipt.chunk_id).ok_or_else(|| {
                        VectorGenerationStoreErrorV1::MissingAppliedVector(receipt.chunk_id.clone())
                    })?;
                    validate_prepared_vector_row(prepared, vector)?;
                    if receipt.outcome != ProjectionOutcomeV1::Applied
                        || receipt.output_digest.as_ref() != Some(&vector.output_digest)
                    {
                        return Err(VectorGenerationStoreErrorV1::BatchIdentityMismatch);
                    }
                    next.tombstones.remove(&receipt.chunk_id);
                    next.tombstone_ids.retain(|chunk_id| chunk_id != &receipt.chunk_id);
                    let mut rebound = (*vector).clone();
                    rebound.source_manifest_digest = next.plan.source_manifest_digest.clone();
                    next.vectors.insert(receipt.chunk_id.clone(), rebound);
                }
                ProjectionOperationV1::Deleted => {
                    let tombstone = tombstone_by_chunk
                        .get(&receipt.chunk_id)
                        .ok_or(VectorGenerationStoreErrorV1::BatchIdentityMismatch)?;
                    if receipt.prior_chunk_digest.as_ref() != Some(&tombstone.prior_chunk_digest) {
                        return Err(VectorGenerationStoreErrorV1::BatchIdentityMismatch);
                    }
                    validate_base_digest(&self.published, &next.plan, receipt)?;
                    next.vectors.remove(&receipt.chunk_id);
                    next.tombstones.insert(
                        receipt.chunk_id.clone(),
                        tombstone.prior_chunk_digest.clone(),
                    );
                    next.tombstone_ids.push(receipt.chunk_id.clone());
                }
                ProjectionOperationV1::Reused => {
                    let base = base_vector(&self.published, &next.plan, &receipt.chunk_id)?;
                    if next.plan.target_projection_key != base.projection_key
                        || receipt.prior_chunk_digest.as_ref() != Some(&base.chunk_digest)
                        || receipt.current_chunk_digest.as_ref() != Some(&base.chunk_digest)
                    {
                        return Err(VectorGenerationStoreErrorV1::MissingBaseVector(
                            receipt.chunk_id.clone(),
                        ));
                    }
                    let mut rebound = base.clone();
                    rebound.source_generation = next.plan.source_generation.clone();
                    rebound.source_manifest_digest = next.plan.source_manifest_digest.clone();
                    next.vectors.insert(receipt.chunk_id.clone(), rebound);
                }
            }
        }
        if vector_by_chunk.len()
            != prepared
                .receipt
                .receipts
                .iter()
                .filter(|receipt| {
                    matches!(
                        receipt.operation,
                        ProjectionOperationV1::Added | ProjectionOperationV1::Updated
                    )
                })
                .count()
            || tombstone_by_chunk.len()
                != prepared
                    .receipt
                    .receipts
                    .iter()
                    .filter(|receipt| receipt.operation == ProjectionOperationV1::Deleted)
                    .count()
        {
            return Err(VectorGenerationStoreErrorV1::BatchIdentityMismatch);
        }

        next.checkpoint.completed_batches += 1;
        next.checkpoint.last_request_digest = Some(prepared.request.request_digest.clone());
        next.checkpoint.last_publication_digest = Some(prepared.receipt.publication_digest.clone());
        next.receipts.push(prepared.receipt.clone());
        next.tombstone_ids.sort();
        next.tombstone_ids.dedup();
        next.batches.push(prepared.clone());
        let checkpoint = next.checkpoint.clone();
        self.staged.insert(build_id.clone(), next);
        Ok(checkpoint)
    }

    /// Validate a fully staged immutable generation and atomically publish
    /// both its record and active pointer. Partial generations remain in
    /// `staged` and are never returned by active-generation reads.
    pub fn publish_generation(
        &mut self,
        build_id: &VectorGenerationBuildIdV1,
        expected_active_generation: Option<&VectorGenerationIdV1>,
    ) -> Result<VectorGenerationPublicationV1, VectorGenerationStoreErrorV1> {
        if self.published.active_generation.as_ref() != expected_active_generation {
            return Err(VectorGenerationStoreErrorV1::StaleActiveGeneration);
        }
        let staged = self
            .staged
            .get(build_id)
            .cloned()
            .ok_or(VectorGenerationStoreErrorV1::UnknownBuild)?;
        let generation = publishable_generation(staged)?;
        let generation_id = generation.generation_id.clone();
        let manifest_digest = generation.manifest_digest.clone();
        // Decide the whole publication against the current state before
        // touching it, so the swap needs no defensive deep copy of every
        // published generation.
        let replays_existing = match self.published.generations.get(&generation_id) {
            Some(existing) => {
                if !existing.same_vector_content(&generation) {
                    return Err(VectorGenerationStoreErrorV1::ImmutableGenerationConflict);
                }
                true
            }
            None => false,
        };
        if self.fail_before_publication_swap {
            self.fail_before_publication_swap = false;
            return Err(VectorGenerationStoreErrorV1::InjectedPublicationFailure);
        }
        intern_generation_vectors(&self.physical_vector_pool, &mut self.published, &generation)?;
        let checkpoint = if replays_existing {
            self.published
                .generations
                .get(&generation_id)
                .ok_or(VectorGenerationStoreErrorV1::ImmutableGenerationConflict)?
                .checkpoint
                .clone()
        } else {
            let checkpoint = generation.checkpoint.clone();
            self.published
                .generations
                .insert(generation_id.clone(), generation);
            checkpoint
        };
        self.published.active_generation = Some(generation_id.clone());
        self.staged.remove(build_id);
        Ok(VectorGenerationPublicationV1 {
            generation_id,
            manifest_digest,
            checkpoint,
        })
    }

    pub fn active_generation_id(&self) -> Option<&VectorGenerationIdV1> {
        self.published.active_generation.as_ref()
    }

    /// Atomically repoint reads to an already-published immutable generation.
    pub fn activate_generation(
        &mut self,
        generation_id: &VectorGenerationIdV1,
        expected_active_generation: Option<&VectorGenerationIdV1>,
    ) -> Result<VectorGenerationPublicationV1, VectorGenerationStoreErrorV1> {
        if self.published.active_generation.as_ref() != expected_active_generation {
            return Err(VectorGenerationStoreErrorV1::StaleActiveGeneration);
        }
        let generation = self
            .published
            .generations
            .get(generation_id)
            .ok_or(VectorGenerationStoreErrorV1::IncompatibleBaseGeneration)?;
        generation.validate_persisted()?;
        let publication = VectorGenerationPublicationV1 {
            generation_id: generation.generation_id().clone(),
            manifest_digest: generation.manifest_digest().clone(),
            checkpoint: generation.checkpoint().clone(),
        };
        if self.fail_before_publication_swap {
            self.fail_before_publication_swap = false;
            return Err(VectorGenerationStoreErrorV1::InjectedPublicationFailure);
        }
        self.published.active_generation = Some(generation_id.clone());
        Ok(publication)
    }

    /// Atomically disable semantic reads while retaining immutable generations
    /// for an exact offline rollback.
    pub fn deactivate_generation(
        &mut self,
        expected_active_generation: Option<&VectorGenerationIdV1>,
    ) -> Result<(), VectorGenerationStoreErrorV1> {
        if self.published.active_generation.as_ref() != expected_active_generation {
            return Err(VectorGenerationStoreErrorV1::StaleActiveGeneration);
        }
        if self.fail_before_publication_swap {
            self.fail_before_publication_swap = false;
            return Err(VectorGenerationStoreErrorV1::InjectedPublicationFailure);
        }
        self.published.active_generation = None;
        Ok(())
    }

    pub fn active_checkpoint(&self) -> Option<&VectorProjectionCheckpointV1> {
        self.active_generation()
            .map(PublishedVectorGenerationV1::checkpoint)
    }

    /// The checkpoint of one staged build, which is how a resumed run learns
    /// how many of its batches are already durable.
    pub fn staged_checkpoint(
        &self,
        build_id: &VectorGenerationBuildIdV1,
    ) -> Option<&VectorProjectionCheckpointV1> {
        self.staged.get(build_id).map(|staged| &staged.checkpoint)
    }

    pub fn active_generation(&self) -> Option<&PublishedVectorGenerationV1> {
        self.active_generation_id()
            .and_then(|id| self.published.generations.get(id))
    }

    /// Return the active immutable generation only when every query-facing
    /// projection and source identity matches exactly. A staged replacement
    /// is never considered, so incompatible searches omit semantics rather
    /// than reading stale or partial rows.
    pub fn active_generation_for(
        &self,
        embedding_key: &AdmittedEmbeddingProjectionKeyV1,
        source_generation: &CodeGenerationId,
        source_manifest_digest: &ManifestDigest,
    ) -> Option<&PublishedVectorGenerationV1> {
        self.active_generation().filter(|generation| {
            generation.embedding_key() == embedding_key
                && generation.source_generation() == source_generation
                && generation.source_manifest_digest() == source_manifest_digest
        })
    }

    pub fn generation(
        &self,
        generation_id: &VectorGenerationIdV1,
    ) -> Option<&PublishedVectorGenerationV1> {
        self.published.generations.get(generation_id)
    }

    /// Resolve the shared immutable vector bytes behind one logical generation
    /// occurrence. The returned allocation is reused only inside the exact
    /// projection/privacy authority named by the generation.
    pub fn physical_vector_values(
        &self,
        generation_id: &VectorGenerationIdV1,
        chunk_id: &CodeSearchChunkId,
    ) -> Option<Arc<[f32]>> {
        let generation = self
            .published
            .generations
            .get(generation_id)?;
        let vector = generation.vectors().get(chunk_id)?;
        let (physical_id, _) =
            physical_vector_reuse_key(generation.embedding_key(), vector).ok()?;
        self.published
            .physical_vectors
            .get(&physical_id)
            .map(|payload| Arc::clone(&payload.values.0))
    }

    pub fn fail_before_publication_swap_once(&mut self) {
        self.fail_before_publication_swap = true;
    }
}

fn publishable_generation(
    staged: StagedVectorGenerationV1,
) -> Result<PublishedVectorGenerationV1, VectorGenerationStoreErrorV1> {
    let expected = staged
        .plan
        .expected_chunk_ids
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    let actual = staged.vectors.keys().cloned().collect::<BTreeSet<_>>();
    if expected != actual || staged.batches.is_empty() {
        return Err(VectorGenerationStoreErrorV1::IncompleteGeneration);
    }
    let embedding_key = staged
        .embedding_key
        .clone()
        .ok_or(VectorGenerationStoreErrorV1::IncompleteGeneration)?;
    for vector in staged.vectors.values() {
        validate_vector_row(&staged.plan, &embedding_key, vector)?;
    }
    let manifest_digest =
        generation_identity_digest(&staged.plan, &staged.vectors, &staged.tombstones)?;
    let generation = PublishedVectorGenerationV1 {
        generation_id: VectorGenerationIdV1::new(manifest_digest.clone()),
        projection_key: staged.plan.target_projection_key,
        source_generation: staged.plan.source_generation,
        source_manifest_digest: staged.plan.source_manifest_digest,
        base_generation: staged.plan.base_generation,
        embedding_key,
        vectors: staged.vectors,
        tombstones: staged.tombstone_ids,
        tombstone_digests: staged.tombstones,
        receipts: staged.receipts,
        checkpoint: staged.checkpoint,
        manifest_digest,
    };
    generation.validate_persisted()?;
    Ok(generation)
}

fn new_staged_generation(
    plan: VectorGenerationPlanV1,
) -> Result<
    (VectorGenerationBuildIdV1, StagedVectorGenerationV1),
    VectorGenerationStoreErrorV1,
> {
    validate_plan(&plan)?;
    let digest = canonical_sha256(&(VECTOR_GENERATION_BUILD_DIGEST_DOMAIN, &plan))
        .map_err(|error| VectorGenerationStoreErrorV1::InvalidPlan(error.to_string()))?;
    let checkpoint = VectorProjectionCheckpointV1 {
        target_projection_key: plan.target_projection_key.clone(),
        source_generation: plan.source_generation.clone(),
        source_manifest_digest: plan.source_manifest_digest.clone(),
        completed_batches: 0,
        last_request_digest: None,
        last_publication_digest: None,
    };
    Ok((
        VectorGenerationBuildIdV1(digest),
        StagedVectorGenerationV1 {
            plan,
            embedding_key: None,
            vectors: ExternalV1::default(),
            tombstones: ExternalV1::default(),
            tombstone_ids: ExternalV1::default(),
            batches: ExternalV1::default(),
            receipts: ExternalV1::default(),
            committed_chunk_effects: ExternalV1::default(),
            checkpoint,
        },
    ))
}
