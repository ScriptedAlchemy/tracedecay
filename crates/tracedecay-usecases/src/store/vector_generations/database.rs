impl<'database> DatabaseVectorGenerationStoreV1<'database> {
    pub async fn open(database: &'database Database) -> Result<Self, VectorGenerationStoreErrorV1> {
        let transaction = database
            .begin_write_transaction(VECTOR_GENERATION_STATE_OPERATION)
            .await
            .map_err(storage_error)?;
        let mut legacy_schema = transaction
            .query_engine(
                "SELECT 1
                 FROM sqlite_schema
                 WHERE type = 'table'
                   AND name = 'semantic_vector_generation_state_v1'",
                (),
            )
            .await
            .map_err(storage_error)?;
        let has_legacy_singleton = legacy_schema
            .next()
            .await
            .map_err(storage_error)?
            .is_some();
        drop(legacy_schema);
        if has_legacy_singleton {
            let mut rows = transaction
                .query_engine(
                    "SELECT singleton, revision, state_json
                     FROM semantic_vector_generation_state_v1
                     ORDER BY singleton",
                    (),
                )
                .await
                .map_err(storage_error)?;
            let row = rows
                .next()
                .await
                .map_err(storage_error)?
                .ok_or(VectorGenerationStoreErrorV1::NonemptyLegacySingleton)?;
            let singleton = row.get::<i64>(0).map_err(storage_error)?;
            let revision = row.get::<i64>(1).map_err(storage_error)?;
            let state_json = row.get::<String>(2).map_err(storage_error)?;
            let extra_row = rows.next().await.map_err(storage_error)?.is_some();
            drop(rows);
            let state: serde_json::Value =
                serde_json::from_str(&state_json).map_err(|_| {
                    VectorGenerationStoreErrorV1::NonemptyLegacySingleton
                })?;
            let exact_empty = serde_json::json!({
                "staged": {},
                "published": {
                    "generations": {},
                    "active_generation": null,
                    "legacy_migration_receipts": {},
                    "physical_vector_bindings": {}
                }
            });
            if singleton != 1 || revision != 0 || extra_row || state != exact_empty {
                transaction.rollback().await.map_err(storage_error)?;
                return Err(VectorGenerationStoreErrorV1::NonemptyLegacySingleton);
            }
        }

        transaction
            .execute_batch_engine(VECTOR_GENERATION_RECORD_SCHEMA_V1)
            .await
            .map_err(storage_error)?;
        transaction
            .execute_batch_engine(VECTOR_PAYLOAD_SCHEMA_V1)
            .await
            .map_err(storage_error)?;
        transaction
            .execute_batch_engine(VECTOR_STATE_SLICE_SCHEMA_V1)
            .await
            .map_err(storage_error)?;
        let shard_id_json = serde_json::to_string(
            &database.retained_runtime().binding().shard_id,
        )
        .map_err(storage_error)?;
        transaction
            .execute_engine(
                "INSERT OR IGNORE INTO semantic_vector_active_generation_v1 (
                    singleton, revision, shard_id_json, generation_id
                 ) VALUES (1, 0, ?1, NULL)",
                params![shard_id_json],
            )
            .await
            .map_err(storage_error)?;
        let mut active_rows = transaction
            .query_engine(
                "SELECT shard_id_json
                 FROM semantic_vector_active_generation_v1
                 WHERE singleton = 1",
                (),
            )
            .await
            .map_err(storage_error)?;
        let active_shard_json = active_rows
            .next()
            .await
            .map_err(storage_error)?
            .ok_or_else(|| {
                VectorGenerationStoreErrorV1::Storage(
                    "vector generation active pointer is missing".to_owned(),
                )
            })?
            .get::<String>(0)
            .map_err(storage_error)?;
        drop(active_rows);
        let active_shard: tracedecay_store::StoreShardIdV1 =
            serde_json::from_str(&active_shard_json)
                .map_err(|_| VectorGenerationStoreErrorV1::ShardIdentityMismatch)?;
        if &active_shard != &database.retained_runtime().binding().shard_id {
            transaction.rollback().await.map_err(storage_error)?;
            return Err(VectorGenerationStoreErrorV1::ShardIdentityMismatch);
        }
        if has_legacy_singleton {
            transaction
                .execute_batch_engine("DROP TABLE semantic_vector_generation_state_v1;")
                .await
                .map_err(storage_error)?;
        }
        transaction.commit().await.map_err(storage_error)?;
        Ok(Self { database })
    }

    /// Read the one active immutable generation needed by a request without
    /// entering the writer lane or deserializing staged/inactive generations.
    pub(crate) async fn read_active_generation_for(
        database: &Database,
        embedding_key: &AdmittedEmbeddingProjectionKeyV1,
        source_generation: &CodeGenerationId,
        source_manifest_digest: &ManifestDigest,
    ) -> Result<Option<PublishedVectorGenerationV1>, VectorGenerationStoreErrorV1> {
        Ok(Self::read_active_generation_snapshot_for(
            database,
            embedding_key,
            source_generation,
            source_manifest_digest,
        )
        .await?
        .map(ActiveVectorGenerationSnapshotV1::into_generation))
    }

    /// Read the atomically active immutable generation without entering the
    /// writer lane. Callers must apply their own source/projection admission.
    pub(crate) async fn read_active_generation(
        database: &Database,
    ) -> Result<Option<PublishedVectorGenerationV1>, VectorGenerationStoreErrorV1> {
        Ok(Self::read_active_generation_snapshot(database)
            .await?
            .map(ActiveVectorGenerationSnapshotV1::into_generation))
    }

    async fn read_active_generation_snapshot(
        database: &Database,
    ) -> Result<Option<ActiveVectorGenerationSnapshotV1>, VectorGenerationStoreErrorV1> {
        let pointer = active_generation_pointer(database).await?;
        let Some(generation_id) = pointer.generation_id else {
            return Ok(None);
        };
        let generation = load_published_generation_record(database, &generation_id)
            .await?
            .ok_or_else(|| {
                VectorGenerationStoreErrorV1::Storage(
                    "active vector generation record is missing".to_owned(),
                )
            })?;
        Ok(Some(ActiveVectorGenerationSnapshotV1 {
            revision: pointer.revision,
            generation,
        }))
    }

    pub(crate) async fn read_active_generation_snapshot_for(
        database: &Database,
        embedding_key: &AdmittedEmbeddingProjectionKeyV1,
        source_generation: &CodeGenerationId,
        source_manifest_digest: &ManifestDigest,
    ) -> Result<Option<ActiveVectorGenerationSnapshotV1>, VectorGenerationStoreErrorV1> {
        let Some(snapshot) = Self::read_active_generation_snapshot(database).await? else {
            return Ok(None);
        };
        if snapshot.generation.embedding_key() != embedding_key
            || snapshot.generation.source_generation() != source_generation
            || snapshot.generation.source_manifest_digest() != source_manifest_digest
        {
            return Ok(None);
        }
        Ok(Some(snapshot))
    }

    pub(crate) async fn active_snapshot_is_current(
        database: &Database,
        revision: i64,
        generation_id: &VectorGenerationIdV1,
    ) -> Result<bool, VectorGenerationStoreErrorV1> {
        let shard_id_json =
            serde_json::to_string(&database.retained_runtime().binding().shard_id)
                .map_err(storage_error)?;
        let mut rows = database
            .engine_conn()
            .query(
                "SELECT 1
                 FROM semantic_vector_active_generation_v1
                 WHERE singleton = 1
                   AND revision = ?1
                   AND generation_id = ?2
                   AND shard_id_json = ?3",
                params![
                    revision,
                    generation_id.as_digest().as_str(),
                    shard_id_json
                ],
            )
            .await
            .map_err(storage_error)?;
        let is_current = rows.next().await.map_err(storage_error)?.is_some();
        drop(rows);
        Ok(is_current)
    }

    pub(crate) async fn read_generation(
        database: &Database,
        generation_id: &VectorGenerationIdV1,
    ) -> Result<Option<PublishedVectorGenerationV1>, VectorGenerationStoreErrorV1> {
        load_published_generation_record(database, generation_id).await
    }

    pub async fn begin_generation(
        &self,
        plan: VectorGenerationPlanV1,
    ) -> Result<VectorGenerationBuildIdV1, VectorGenerationStoreErrorV1> {
        if let Some(base_generation) = plan.base_generation.as_ref()
            && load_published_generation_record(self.database, base_generation)
                .await?
                .is_none()
        {
            return Err(VectorGenerationStoreErrorV1::IncompatibleBaseGeneration);
        }
        let (build_id, staged) = new_staged_generation(plan.clone())?;
        let mut state = staged_record_state(&build_id, staged);
        let pending_slices = seal_external_state(&mut state, &BTreeSet::new())?;
        let owned_slices = pending_slices.keys().cloned().collect();
        let record_json = staged_record_json(&state, &build_id)?;
        let transaction = self
            .database
            .begin_write_transaction(VECTOR_GENERATION_STATE_OPERATION)
            .await
            .map_err(storage_error)?;
        write_state_slices(&transaction, VECTOR_STATE_SLICE_TABLE_V1, &pending_slices).await?;
        write_generation_resource_owners(
            &transaction,
            &build_id,
            &BTreeSet::new(),
            &owned_slices,
        )
        .await?;
        let inserted = transaction
            .execute_engine(
                "INSERT OR IGNORE INTO semantic_vector_generation_v1 (
                    build_id, revision, lifecycle, generation_id, record_json
                 ) VALUES (?1, 0, 'staged', NULL, ?2)",
                params![build_id.0.as_str(), record_json],
            )
            .await
            .map_err(storage_error)?;
        transaction.commit().await.map_err(storage_error)?;
        if inserted == 0 {
            let existing = load_staged_generation_record(self.database, &build_id).await?;
            if existing.staged.plan != plan {
                return Err(VectorGenerationStoreErrorV1::InvalidPlan(
                    "build identity collision".to_owned(),
                ));
            }
        }
        Ok(build_id)
    }

    pub async fn rebuild_generation(
        &self,
        plan: VectorGenerationPlanV1,
    ) -> Result<VectorGenerationBuildIdV1, VectorGenerationStoreErrorV1> {
        let (build_id, _) = new_staged_generation(plan.clone())?;
        let _ = self.cancel_generation(&build_id).await?;
        self.begin_generation(plan).await
    }

    pub async fn cancel_generation(
        &self,
        build_id: &VectorGenerationBuildIdV1,
    ) -> Result<bool, VectorGenerationStoreErrorV1> {
        let transaction = self
            .database
            .begin_write_transaction(VECTOR_GENERATION_STATE_OPERATION)
            .await
            .map_err(storage_error)?;
        let changed = transaction
            .execute_engine(
                "DELETE FROM semantic_vector_generation_v1
                 WHERE build_id = ?1
                   AND lifecycle = 'staged'",
                params![build_id.0.as_str()],
            )
            .await
            .map_err(storage_error)?;
        if changed == 1 {
            transaction
                .execute_engine(
                    "INSERT OR IGNORE INTO semantic_vector_generation_retired_v1 (build_id)
                     VALUES (?1)",
                    params![build_id.0.as_str()],
                )
                .await
                .map_err(storage_error)?;
        }
        transaction.commit().await.map_err(storage_error)?;
        Ok(changed == 1)
    }

    pub async fn commit_batch(
        &self,
        build_id: &VectorGenerationBuildIdV1,
        expected_checkpoint: Option<&VectorProjectionCheckpointV1>,
        prepared: PreparedVectorGenerationV1,
    ) -> Result<VectorProjectionCheckpointV1, VectorGenerationStoreErrorV1> {
        let loaded = load_staged_generation_record(self.database, build_id).await?;
        let mut state = staged_record_state(build_id, loaded.staged);
        if let Some(base_generation_id) = state
            .staged
            .get(build_id)
            .and_then(|staged| staged.plan.base_generation.as_ref())
            .cloned()
        {
            let base = load_published_generation_record(self.database, &base_generation_id)
                .await?
                .ok_or(VectorGenerationStoreErrorV1::IncompatibleBaseGeneration)?;
            state
                .published
                .generations
                .insert(base_generation_id, base);
        }
        let checkpoint =
            state.commit_batch_ref(build_id, expected_checkpoint, &prepared)?;
        state.published.generations.clear();
        let pending_slices = seal_external_state(&mut state, &loaded.durable_slices)?;
        let owned_slices = pending_slices.keys().cloned().collect();
        let record_json = staged_record_json(&state, build_id)?;
        let batch_payloads = prepared
            .receipt
            .receipts
            .iter()
            .filter_map(|receipt| {
                state
                    .staged
                    .get(build_id)
                    .and_then(|staged| staged.vectors.get(&receipt.chunk_id))
                    .map(|vector| vector.output_digest.clone())
            })
            .collect::<BTreeSet<_>>();
        let transaction = self
            .database
            .begin_write_transaction(VECTOR_GENERATION_STATE_OPERATION)
            .await
            .map_err(storage_error)?;
        write_vector_payloads(
            &transaction,
            VECTOR_PAYLOAD_TABLE_V1,
            &state,
            &loaded.durable_payloads,
        )
        .await?;
        write_state_slices(&transaction, VECTOR_STATE_SLICE_TABLE_V1, &pending_slices).await?;
        write_generation_resource_owners(
            &transaction,
            build_id,
            &batch_payloads,
            &owned_slices,
        )
        .await?;
        let changed = transaction
            .execute_engine(
                "UPDATE semantic_vector_generation_v1
                 SET revision = revision + 1,
                     record_json = ?1
                 WHERE build_id = ?2
                   AND lifecycle = 'staged'
                   AND revision = ?3",
                params![record_json, build_id.0.as_str(), loaded.revision],
            )
            .await
            .map_err(storage_error)?;
        if changed != 1 {
            transaction.rollback().await.map_err(storage_error)?;
            return Err(VectorGenerationStoreErrorV1::ConcurrentMutation);
        }
        transaction.commit().await.map_err(storage_error)?;
        Ok(checkpoint)
    }

    pub async fn publish_generation(
        &self,
        build_id: &VectorGenerationBuildIdV1,
        expected_active_generation: Option<&VectorGenerationIdV1>,
    ) -> Result<VectorGenerationPublicationV1, VectorGenerationStoreErrorV1> {
        let loaded = load_staged_generation_record(self.database, build_id).await?;
        let generation = publishable_generation(loaded.staged)?;
        let pointer = active_generation_pointer(self.database).await?;
        expected_active_matches(
            pointer.generation_id.as_ref(),
            expected_active_generation,
        )?;
        let generation_id = generation.generation_id().clone();
        let manifest_digest = generation.manifest_digest().clone();
        let checkpoint = generation.checkpoint().clone();
        let record_json = serde_json::to_string(&generation).map_err(storage_error)?;
        let shard_id_json = serde_json::to_string(
            &self.database.retained_runtime().binding().shard_id,
        )
        .map_err(storage_error)?;
        let transaction = self
            .database
            .begin_write_transaction(VECTOR_GENERATION_STATE_OPERATION)
            .await
            .map_err(storage_error)?;
        let record_changed = transaction
            .execute_engine(
                "UPDATE semantic_vector_generation_v1
                 SET revision = revision + 1,
                     lifecycle = 'published',
                     generation_id = ?1,
                     record_json = ?2
                 WHERE build_id = ?3
                   AND lifecycle = 'staged'
                   AND revision = ?4",
                params![
                    generation_id.as_digest().as_str(),
                    record_json,
                    build_id.0.as_str(),
                    loaded.revision
                ],
            )
            .await
            .map_err(storage_error)?;
        if record_changed != 1 {
            transaction.rollback().await.map_err(storage_error)?;
            return Err(VectorGenerationStoreErrorV1::ConcurrentMutation);
        }
        let pointer_changed = transaction
            .execute_engine(
                "UPDATE semantic_vector_active_generation_v1
                 SET revision = revision + 1,
                     generation_id = ?1
                 WHERE singleton = 1
                   AND revision = ?2
                   AND shard_id_json = ?3
                   AND generation_id IS ?4",
                params![
                    generation_id.as_digest().as_str(),
                    pointer.revision,
                    shard_id_json,
                    expected_active_generation
                        .map(|generation_id| generation_id.as_digest().as_str())
                ],
            )
            .await
            .map_err(storage_error)?;
        if pointer_changed != 1 {
            transaction.rollback().await.map_err(storage_error)?;
            return Err(VectorGenerationStoreErrorV1::StaleActiveGeneration);
        }
        transaction.commit().await.map_err(storage_error)?;
        Ok(VectorGenerationPublicationV1 {
            generation_id,
            manifest_digest,
            checkpoint,
        })
    }

    pub async fn activate_generation(
        &self,
        generation_id: &VectorGenerationIdV1,
        expected_active_generation: Option<&VectorGenerationIdV1>,
    ) -> Result<VectorGenerationPublicationV1, VectorGenerationStoreErrorV1> {
        let generation = load_published_generation_record(self.database, generation_id)
            .await?
            .ok_or(VectorGenerationStoreErrorV1::IncompatibleBaseGeneration)?;
        let pointer = active_generation_pointer(self.database).await?;
        expected_active_matches(
            pointer.generation_id.as_ref(),
            expected_active_generation,
        )?;
        self.swap_active_generation(
            &pointer,
            Some(generation_id),
            expected_active_generation,
        )
        .await?;
        Ok(VectorGenerationPublicationV1 {
            generation_id: generation_id.clone(),
            manifest_digest: generation.manifest_digest().clone(),
            checkpoint: generation.checkpoint().clone(),
        })
    }

    pub async fn deactivate_generation(
        &self,
        expected_active_generation: Option<&VectorGenerationIdV1>,
    ) -> Result<(), VectorGenerationStoreErrorV1> {
        let pointer = active_generation_pointer(self.database).await?;
        expected_active_matches(
            pointer.generation_id.as_ref(),
            expected_active_generation,
        )?;
        self.swap_active_generation(&pointer, None, expected_active_generation)
            .await
    }

    pub async fn active_generation_id(
        &self,
    ) -> Result<Option<VectorGenerationIdV1>, VectorGenerationStoreErrorV1> {
        Ok(active_generation_pointer(self.database).await?.generation_id)
    }

    /// Code generations retained by immutable vector-generation rows.
    pub async fn retained_source_generations(
        &self,
    ) -> Result<BTreeSet<CodeGenerationId>, VectorGenerationStoreErrorV1> {
        let mut rows = self
            .database
            .engine_conn()
            .query(
                "SELECT CAST(json_extract(record_json, '$.source_generation') AS TEXT)
                 FROM semantic_vector_generation_v1
                 WHERE lifecycle = 'published'
                 ORDER BY generation_id",
                (),
            )
            .await
            .map_err(storage_error)?;
        let mut retained = BTreeSet::new();
        while let Some(row) = rows.next().await.map_err(storage_error)? {
            retained.insert(
                CodeGenerationId::try_from(row.get::<String>(0).map_err(storage_error)?)
                    .map_err(storage_error)?,
            );
        }
        Ok(retained)
    }

    /// As [`Self::retained_source_generations`], under an existing writer
    /// fence so a concurrent publication cannot change the retention pins.
    pub async fn retained_source_generations_in_transaction(
        transaction: &tracedecay_runtime_core::db::DatabaseWriteTransaction<'_>,
    ) -> Result<BTreeSet<CodeGenerationId>, VectorGenerationStoreErrorV1> {
        let mut rows = transaction
            .query_engine(
                "SELECT CAST(json_extract(record_json, '$.source_generation') AS TEXT)
                 FROM semantic_vector_generation_v1
                 WHERE lifecycle = 'published'
                 ORDER BY generation_id",
                (),
            )
            .await
            .map_err(storage_error)?;
        let mut retained = BTreeSet::new();
        while let Some(row) = rows.next().await.map_err(storage_error)? {
            retained.insert(
                CodeGenerationId::try_from(row.get::<String>(0).map_err(storage_error)?)
                    .map_err(storage_error)?,
            );
        }
        Ok(retained)
    }

    /// The checkpoint of one staged build, or `None` when no build is staged
    /// under that identity yet.
    pub async fn staged_checkpoint(
        &self,
        build_id: &VectorGenerationBuildIdV1,
    ) -> Result<Option<VectorProjectionCheckpointV1>, VectorGenerationStoreErrorV1> {
        match load_staged_generation_record(self.database, build_id).await {
            Ok(loaded) => Ok(Some(loaded.staged.checkpoint)),
            Err(VectorGenerationStoreErrorV1::UnknownBuild) => Ok(None),
            Err(error) => Err(error),
        }
    }

    pub async fn active_checkpoint(
        &self,
    ) -> Result<Option<VectorProjectionCheckpointV1>, VectorGenerationStoreErrorV1> {
        Ok(self
            .active_generation()
            .await?
            .map(|generation| generation.checkpoint().clone()))
    }

    pub async fn active_generation(
        &self,
    ) -> Result<Option<PublishedVectorGenerationV1>, VectorGenerationStoreErrorV1> {
        Self::read_active_generation(self.database).await
    }

    pub async fn active_generation_for(
        &self,
        embedding_key: &AdmittedEmbeddingProjectionKeyV1,
        source_generation: &CodeGenerationId,
        source_manifest_digest: &ManifestDigest,
    ) -> Result<Option<PublishedVectorGenerationV1>, VectorGenerationStoreErrorV1> {
        Self::read_active_generation_for(
            self.database,
            embedding_key,
            source_generation,
            source_manifest_digest,
        )
        .await
    }

    pub async fn generation(
        &self,
        generation_id: &VectorGenerationIdV1,
    ) -> Result<Option<PublishedVectorGenerationV1>, VectorGenerationStoreErrorV1> {
        Self::read_generation(self.database, generation_id).await
    }

    pub async fn physical_vector_values(
        &self,
        generation_id: &VectorGenerationIdV1,
        chunk_id: &CodeSearchChunkId,
    ) -> Result<Option<Arc<[f32]>>, VectorGenerationStoreErrorV1> {
        Ok(self
            .generation(generation_id)
            .await?
            .and_then(|generation| {
                generation
                    .vectors()
                    .get(chunk_id)
                    .map(|vector| Arc::<[f32]>::from(vector.values.clone()))
            }))
    }

    async fn swap_active_generation(
        &self,
        pointer: &ActiveGenerationPointerV1,
        next_generation: Option<&VectorGenerationIdV1>,
        expected_generation: Option<&VectorGenerationIdV1>,
    ) -> Result<(), VectorGenerationStoreErrorV1> {
        let shard_id_json = serde_json::to_string(
            &self.database.retained_runtime().binding().shard_id,
        )
        .map_err(storage_error)?;
        let changed = self
            .database
            .execute_write_engine(
                VECTOR_GENERATION_STATE_OPERATION,
                "UPDATE semantic_vector_active_generation_v1
                 SET revision = revision + 1,
                     generation_id = ?1
                 WHERE singleton = 1
                   AND revision = ?2
                   AND shard_id_json = ?3
                   AND generation_id IS ?4",
                params![
                    next_generation.map(|generation| generation.as_digest().as_str()),
                    pointer.revision,
                    shard_id_json,
                    expected_generation.map(|generation| generation.as_digest().as_str())
                ],
            )
            .await
            .map_err(storage_error)?;
        if changed == 1 {
            Ok(())
        } else {
            Err(VectorGenerationStoreErrorV1::StaleActiveGeneration)
        }
    }

}
