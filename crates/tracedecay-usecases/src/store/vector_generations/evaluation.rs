impl<'database> DatabaseVectorEvaluationStoreV1<'database> {
    pub(crate) async fn open(
        database: &'database Database,
        evaluation_id: impl Into<String>,
    ) -> Result<Self, VectorGenerationStoreErrorV1> {
        let evaluation_id = evaluation_id.into();
        if evaluation_id.is_empty()
            || evaluation_id.len() > 256
            || evaluation_id.trim() != evaluation_id
            || evaluation_id.chars().any(char::is_control)
        {
            return Err(VectorGenerationStoreErrorV1::Storage(
                "semantic evaluation identity is invalid".to_owned(),
            ));
        }
        let initial_state = serde_json::to_string(&FakeVectorGenerationStoreV1::default())
            .map_err(storage_error)?;
        let inserted = database
            .execute_write_engine(
                VECTOR_GENERATION_STATE_OPERATION,
                "INSERT INTO semantic_vector_evaluation_state_v1 (
                    evaluation_id, revision, state_json
                 ) VALUES (?1, 0, ?2)",
                params![evaluation_id.clone(), initial_state],
            )
            .await
            .map_err(storage_error)?;
        if inserted != 1 {
            return Err(VectorGenerationStoreErrorV1::Storage(
                "semantic evaluation state could not be initialized".to_owned(),
            ));
        }
        Ok(Self {
            database,
            evaluation_id,
        })
    }

    pub(crate) async fn rebuild_generation(
        &self,
        plan: VectorGenerationPlanV1,
    ) -> Result<VectorGenerationBuildIdV1, VectorGenerationStoreErrorV1> {
        self.mutate_state(|state| state.rebuild_generation(plan.clone()))
            .await
    }

    pub(crate) async fn commit_batch(
        &self,
        build_id: &VectorGenerationBuildIdV1,
        expected_checkpoint: Option<&VectorProjectionCheckpointV1>,
        prepared: PreparedVectorGenerationV1,
    ) -> Result<VectorProjectionCheckpointV1, VectorGenerationStoreErrorV1> {
        self.mutate_state(|state| {
            state.commit_batch(build_id, expected_checkpoint, prepared.clone())
        })
        .await
    }

    pub(crate) async fn publish_generation(
        &self,
        build_id: &VectorGenerationBuildIdV1,
        expected_active_generation: Option<&VectorGenerationIdV1>,
    ) -> Result<VectorGenerationPublicationV1, VectorGenerationStoreErrorV1> {
        self.mutate_state(|state| state.publish_generation(build_id, expected_active_generation))
            .await
    }

    pub(crate) async fn cancel_generation(
        &self,
        build_id: &VectorGenerationBuildIdV1,
    ) -> Result<bool, VectorGenerationStoreErrorV1> {
        self.mutate_state(|state| Ok(state.cancel_generation(build_id)))
            .await
    }

    pub(crate) async fn active_generation_id(
        &self,
    ) -> Result<Option<VectorGenerationIdV1>, VectorGenerationStoreErrorV1> {
        let (_, state, _) = self.load_state().await?;
        Ok(state.active_generation_id().cloned())
    }

    pub(crate) async fn active_generation_for(
        &self,
        embedding_key: &AdmittedEmbeddingProjectionKeyV1,
        source_generation: &CodeGenerationId,
        source_manifest_digest: &ManifestDigest,
    ) -> Result<Option<PublishedVectorGenerationV1>, VectorGenerationStoreErrorV1> {
        let (_, state, _) = self.load_state().await?;
        Ok(state
            .active_generation_for(embedding_key, source_generation, source_manifest_digest)
            .cloned())
    }

    pub(crate) async fn close(self) -> Result<(), VectorGenerationStoreErrorV1> {
        let transaction = self
            .database
            .begin_write_transaction(VECTOR_GENERATION_STATE_OPERATION)
            .await
            .map_err(storage_error)?;
        let deleted = transaction
            .execute_engine(
                "DELETE FROM semantic_vector_evaluation_state_v1
                 WHERE evaluation_id = ?1",
                params![self.evaluation_id],
            )
            .await
            .map_err(storage_error)?;
        if deleted != 1 {
            transaction.rollback().await.map_err(storage_error)?;
            return Err(VectorGenerationStoreErrorV1::ConcurrentMutation);
        }
        // The measured run owned every evaluation payload only while some
        // evaluation state referenced it. Once the last one is gone the whole
        // lane is unreachable, so it is released with the row that named it.
        transaction
            .execute_engine(
                "DELETE FROM semantic_vector_evaluation_payload_v1
                 WHERE NOT EXISTS (SELECT 1 FROM semantic_vector_evaluation_state_v1)",
                (),
            )
            .await
            .map_err(storage_error)?;
        transaction
            .execute_engine(
                "DELETE FROM semantic_vector_evaluation_state_slice_v1
                 WHERE NOT EXISTS (SELECT 1 FROM semantic_vector_evaluation_state_v1)",
                (),
            )
            .await
            .map_err(storage_error)?;
        transaction.commit().await.map_err(storage_error)?;
        Ok(())
    }

    async fn mutate_state<ResultValue>(
        &self,
        mut mutation: impl FnMut(
            &mut FakeVectorGenerationStoreV1,
        ) -> Result<ResultValue, VectorGenerationStoreErrorV1>,
    ) -> Result<ResultValue, VectorGenerationStoreErrorV1> {
        for _ in 0..MAX_STATE_CAS_RETRIES {
            let (revision, mut state, load) = self.load_state().await?;
            let result = mutation(&mut state)?;
            let pending_slices = seal_external_state(&mut state, &load.durable_slices)?;
            let state_json = serde_json::to_string(&state).map_err(storage_error)?;
            let transaction = self
                .database
                .begin_write_transaction(VECTOR_GENERATION_STATE_OPERATION)
                .await
                .map_err(storage_error)?;
            write_vector_payloads(
                &transaction,
                VECTOR_EVALUATION_PAYLOAD_TABLE_V1,
                &state,
                &load.durable,
            )
            .await?;
            write_state_slices(
                &transaction,
                VECTOR_EVALUATION_STATE_SLICE_TABLE_V1,
                &pending_slices,
            )
            .await?;
            let changed = transaction
                .execute_engine(
                    "UPDATE semantic_vector_evaluation_state_v1
                     SET revision = revision + 1, state_json = ?1
                     WHERE evaluation_id = ?2 AND revision = ?3",
                    params![state_json, self.evaluation_id.clone(), revision],
                )
                .await
                .map_err(storage_error)?;
            if changed == 1 {
                transaction.commit().await.map_err(storage_error)?;
                return Ok(result);
            }
            transaction.rollback().await.map_err(storage_error)?;
        }
        Err(VectorGenerationStoreErrorV1::ConcurrentMutation)
    }

    async fn load_state(
        &self,
    ) -> Result<(i64, FakeVectorGenerationStoreV1, VectorPayloadLoadV1), VectorGenerationStoreErrorV1>
    {
        let mut rows = self
            .database
            .engine_conn()
            .query(
                "SELECT revision, state_json
                 FROM semantic_vector_evaluation_state_v1
                 WHERE evaluation_id = ?1",
                params![self.evaluation_id.clone()],
            )
            .await
            .map_err(storage_error)?;
        let row = rows.next().await.map_err(storage_error)?.ok_or_else(|| {
            VectorGenerationStoreErrorV1::Storage(
                "semantic evaluation state row is missing".to_owned(),
            )
        })?;
        let revision = row.get::<i64>(0).map_err(storage_error)?;
        let state_json = row.get::<String>(1).map_err(storage_error)?;
        drop(rows);
        let mut state: FakeVectorGenerationStoreV1 =
            serde_json::from_str(&state_json).map_err(storage_error)?;
        drop(state_json);
        let (durable_slices, inline_collections) = hydrate_external_state(
            self.database,
            VECTOR_EVALUATION_STATE_SLICE_TABLE_V1,
            &mut state,
        )
        .await?;
        let mut load = hydrate_vector_payloads(
            self.database,
            VECTOR_EVALUATION_PAYLOAD_TABLE_V1,
            &mut state,
        )
        .await?;
        load.durable_slices = durable_slices;
        load.migrated_inline_collections = inline_collections;
        state.ensure_physical_reuse_index()?;
        validate_loaded_state(&state)?;
        Ok((revision, state, load))
    }
}
