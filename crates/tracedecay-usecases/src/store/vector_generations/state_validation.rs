impl FakeVectorGenerationStoreV1 {
    /// Rebuild the derived physical-byte index for every published generation.
    ///
    /// The generation map is moved aside rather than cloned: interning only
    /// touches only the derived `physical_vectors` pool, so a deep
    /// copy of every published generation — the whole float corpus, once per
    /// load — bought nothing but the borrow.
    /// In-memory stand-in for the payload table, used by restart tests that
    /// round-trip the state document without a database behind it.
    #[cfg(test)]
    fn hydrate_from(&mut self, reference: &Self) {
        let mut payloads = BTreeMap::new();
        reference.visit_vectors(&mut |vector| {
            payloads.insert(vector.output_digest.clone(), vector.values.clone());
        });
        self.visit_vectors_mut(&mut |vector| {
            if vector.values.is_empty()
                && let Some(values) = payloads.get(&vector.output_digest)
            {
                vector.values.clone_from(values);
            }
        });
    }

    fn ensure_physical_reuse_index(&mut self) -> Result<(), VectorGenerationStoreErrorV1> {
        let generations = std::mem::take(&mut self.published.generations);
        let mut outcome = Ok(());
        for generation in generations.values() {
            outcome = intern_generation_vectors(
                &self.physical_vector_pool,
                &mut self.published,
                generation,
            );
            if outcome.is_err() {
                break;
            }
        }
        self.published.generations = generations;
        outcome
    }
}

fn physical_vector_reuse_key(
    embedding_key: &AdmittedEmbeddingProjectionKeyV1,
    vector: &ProjectedChunkVectorV1,
) -> Result<(ManifestDigest, PhysicalVectorReuseKeyV1), VectorGenerationStoreErrorV1> {
    if embedding_key.projection_key() != &vector.projection_key {
        return Err(VectorGenerationStoreErrorV1::BatchIdentityMismatch);
    }
    let reuse_key = PhysicalVectorReuseKeyV1 {
        canonical_chunk_digest: vector.chunk_digest.clone(),
        projection_key: vector.projection_key.clone(),
        admitted_embedding_key: embedding_key.clone(),
        privacy_domain: embedding_key.privacy_domain().clone(),
        privacy_key_epoch: embedding_key.privacy_key_epoch(),
    };
    let physical_id = canonical_sha256(&(PHYSICAL_VECTOR_REUSE_DIGEST_DOMAIN, &reuse_key))
        .map_err(|error| VectorGenerationStoreErrorV1::Storage(error.to_string()))?;
    Ok((physical_id, reuse_key))
}

fn intern_generation_vectors(
    physical_vector_pool: &PhysicalVectorBytePoolV1,
    published: &mut PublishedStateV1,
    generation: &PublishedVectorGenerationV1,
) -> Result<(), VectorGenerationStoreErrorV1> {
    for vector in generation.vectors.values() {
        let (physical_id, reuse_key) =
            physical_vector_reuse_key(&generation.embedding_key, vector)?;
        match published.physical_vectors.get(&physical_id) {
            Some(existing)
                if existing.reuse_key != reuse_key
                    || existing.values.0.as_ref() != vector.values.as_slice() =>
            {
                return Err(VectorGenerationStoreErrorV1::PhysicalVectorConflict);
            }
            Some(_) => {}
            None => {}
        }
        let shared = physical_vector_pool.intern(&reuse_key, &vector.values)?;
        published.physical_vectors.insert(
            physical_id.clone(),
            PhysicalVectorPayloadV1 {
                reuse_key,
                values: SharedVectorBytesV1(shared),
            },
        );
    }
    Ok(())
}

fn validate_loaded_state(
    state: &FakeVectorGenerationStoreV1,
) -> Result<(), VectorGenerationStoreErrorV1> {
    if let Some(active) = &state.published.active_generation
        && !state.published.generations.contains_key(active)
    {
        return Err(VectorGenerationStoreErrorV1::Storage(
            "active vector generation pointer is dangling".to_string(),
        ));
    }
    for (generation_id, generation) in &state.published.generations {
        if generation.generation_id() != generation_id {
            return Err(VectorGenerationStoreErrorV1::Storage(
                "published generation map key does not match record id".to_string(),
            ));
        }
        generation.validate_persisted()?;
        for (chunk_id, vector) in generation.vectors.iter() {
            let (physical_id, expected_key) =
                physical_vector_reuse_key(generation.embedding_key(), vector)?;
            let physical = state
                .published
                .physical_vectors
                .get(&physical_id)
                .ok_or_else(|| {
                    VectorGenerationStoreErrorV1::Storage(format!(
                        "published vector {chunk_id} has no derived physical byte entry"
                    ))
                })?;
            if physical.reuse_key != expected_key
                || physical.values.0.as_ref() != vector.values.as_slice()
            {
                return Err(VectorGenerationStoreErrorV1::Storage(format!(
                    "published vector {chunk_id} physical byte binding drifted"
                )));
            }
        }
    }
    for staged in state.staged.values() {
        if let Some(embedding_key) = &staged.embedding_key {
            for vector in staged.vectors.values() {
                validate_vector_row(&staged.plan, embedding_key, vector)?;
            }
        }
        let canonical = staged.tombstones.keys().cloned().collect::<BTreeSet<_>>();
        if staged.tombstones.len() != canonical.len() {
            return Err(VectorGenerationStoreErrorV1::Storage(
                "staged tombstones contain duplicate chunk ids".to_string(),
            ));
        }
        for chunk_id in staged.tombstones.keys() {
            if staged.vectors.contains_key(chunk_id) {
                return Err(VectorGenerationStoreErrorV1::Storage(format!(
                    "staged generation retains both vector and tombstone for {chunk_id}"
                )));
            }
        }
    }
    Ok(())
}

fn validate_published_receipts(
    generation: &PublishedVectorGenerationV1,
) -> Result<(), VectorGenerationStoreErrorV1> {
    let checkpoint = generation.checkpoint();
    if checkpoint.target_projection_key != *generation.projection_key()
        || checkpoint.source_generation != *generation.source_generation()
        || checkpoint.source_manifest_digest != *generation.source_manifest_digest()
        || checkpoint.completed_batches == 0
        || checkpoint.completed_batches != generation.receipts().len() as u64
    {
        return Err(VectorGenerationStoreErrorV1::Storage(
            "published generation checkpoint is incomplete or incompatible".to_owned(),
        ));
    }
    let last = generation.receipts().last().ok_or_else(|| {
        VectorGenerationStoreErrorV1::Storage(
            "published generation has no projection receipt".to_owned(),
        )
    })?;
    if checkpoint.last_request_digest.as_ref() != Some(&last.request_digest)
        || checkpoint.last_publication_digest.as_ref() != Some(&last.publication_digest)
    {
        return Err(VectorGenerationStoreErrorV1::Storage(
            "published generation checkpoint does not name its last receipt".to_owned(),
        ));
    }

    let mut effects = BTreeSet::new();
    for batch in generation.receipts() {
        if batch.target_projection_key != *generation.projection_key()
            || batch.source_generation != *generation.source_generation()
            || expected_publication_digest(batch).map_err(storage_error)?
                != batch.publication_digest
            || batch.reused_count
                != batch
                    .receipts
                    .iter()
                    .filter(|receipt| receipt.operation == ProjectionOperationV1::Reused)
                    .count() as u64
        {
            return Err(VectorGenerationStoreErrorV1::Storage(
                "published projection batch receipt is incompatible".to_owned(),
            ));
        }
        for receipt in &batch.receipts {
            if !effects.insert(receipt.chunk_id.clone())
                || receipt.projection_key != *generation.projection_key()
                || receipt.request_digest != batch.request_digest
                || receipt.source_generation != *generation.source_generation()
                || receipt.source_manifest_digest != batch.source_manifest_digest
            {
                return Err(VectorGenerationStoreErrorV1::Storage(
                    "published chunk receipt is duplicated or incompatible".to_owned(),
                ));
            }
            match receipt.operation {
                ProjectionOperationV1::Added | ProjectionOperationV1::Updated => {
                    let vector = generation.vectors().get(&receipt.chunk_id);
                    if receipt.outcome != ProjectionOutcomeV1::Applied
                        || vector.is_none()
                        || receipt.current_chunk_digest.as_ref()
                            != vector.map(|vector| &vector.chunk_digest)
                        || receipt.output_digest.as_ref()
                            != vector.map(|vector| &vector.output_digest)
                        || generation
                            .tombstone_digests()
                            .contains_key(&receipt.chunk_id)
                    {
                        return Err(VectorGenerationStoreErrorV1::Storage(
                            "published applied receipt has no matching vector".to_owned(),
                        ));
                    }
                }
                ProjectionOperationV1::Reused => {
                    let vector = generation.vectors().get(&receipt.chunk_id);
                    if receipt.outcome != ProjectionOutcomeV1::Reused
                        || vector.is_none()
                        || receipt.prior_chunk_digest.as_ref()
                            != vector.map(|vector| &vector.chunk_digest)
                        || receipt.current_chunk_digest.as_ref()
                            != vector.map(|vector| &vector.chunk_digest)
                        || receipt.output_digest.is_some()
                        || generation
                            .tombstone_digests()
                            .contains_key(&receipt.chunk_id)
                    {
                        return Err(VectorGenerationStoreErrorV1::Storage(
                            "published reused receipt has no matching vector".to_owned(),
                        ));
                    }
                }
                ProjectionOperationV1::Deleted => {
                    if receipt.outcome != ProjectionOutcomeV1::Applied
                        || receipt.current_chunk_digest.is_some()
                        || receipt.output_digest.is_some()
                        || receipt.prior_chunk_digest.as_ref()
                            != generation.tombstone_digests().get(&receipt.chunk_id)
                        || generation.vectors().contains_key(&receipt.chunk_id)
                    {
                        return Err(VectorGenerationStoreErrorV1::Storage(
                            "published deletion receipt has no matching tombstone".to_owned(),
                        ));
                    }
                }
            }
        }
    }

    let expected_effects = generation
        .vectors()
        .keys()
        .chain(generation.tombstone_digests().keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    if effects != expected_effects {
        return Err(VectorGenerationStoreErrorV1::Storage(
            "published generation receipt membership is incomplete".to_owned(),
        ));
    }
    Ok(())
}
