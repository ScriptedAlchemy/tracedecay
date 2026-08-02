impl<'database> DatabaseVectorGenerationStoreV1<'database> {
    pub async fn open(database: &'database Database) -> Result<Self, VectorGenerationStoreErrorV1> {
        let store = Self::open_legacy_migration(database).await?;
        store.migrate_inline_vector_payloads().await?;
        Ok(store)
    }

    /// Move a pre-migration state document onto externalized storage.
    ///
    /// Covers both externalizations: a document that still carries floats
    /// inline moves onto row-per-vector payloads, and one that still carries
    /// corpus-sized metadata inline moves onto addressed state slices. Both
    /// are forward-only and crash-safe: the new rows and the rewritten
    /// document commit in one transaction guarded by the same revision the
    /// document was read at, so a crash leaves the original blob intact and
    /// the next open retries. Once migrated the check is a load with nothing
    /// inline to find, and this is a no-op.
    async fn migrate_inline_vector_payloads(&self) -> Result<(), VectorGenerationStoreErrorV1> {
        for _ in 0..MAX_STATE_CAS_RETRIES {
            let (revision, mut state, load) = self.load_state().await?;
            if !load.needs_forward_migration() {
                return Ok(());
            }
            let pending_slices = seal_external_state(&mut state, &load.durable_slices)?;
            let state_json = serde_json::to_string(&state).map_err(storage_error)?;
            let transaction = self
                .database
                .begin_write_transaction(VECTOR_GENERATION_STATE_OPERATION)
                .await
                .map_err(storage_error)?;
            write_vector_payloads(&transaction, VECTOR_PAYLOAD_TABLE_V1, &state, &load.durable)
                .await?;
            write_state_slices(&transaction, VECTOR_STATE_SLICE_TABLE_V1, &pending_slices).await?;
            let changed = transaction
                .execute_engine(
                    "UPDATE semantic_vector_generation_state_v1
                     SET revision = revision + 1, state_json = ?1
                     WHERE singleton = 1 AND revision = ?2",
                    params![state_json, revision],
                )
                .await
                .map_err(storage_error)?;
            if changed == 1 {
                transaction.commit().await.map_err(storage_error)?;
                return Ok(());
            }
            transaction.rollback().await.map_err(storage_error)?;
        }
        Err(VectorGenerationStoreErrorV1::ConcurrentMutation)
    }

    /// Open only the identity/atomic-replacement migration boundary.
    ///
    /// Unlike normal runtime open, this does not deserialize legacy state and
    /// therefore remains callable when old vector payloads are unreadable.
    pub async fn open_legacy_migration(
        database: &'database Database,
    ) -> Result<Self, VectorGenerationStoreErrorV1> {
        database
            .execute_write_batch(
                VECTOR_GENERATION_STATE_OPERATION,
                VECTOR_GENERATION_STATE_SCHEMA_V1,
            )
            .await
            .map_err(storage_error)?;
        database
            .execute_write_batch(VECTOR_GENERATION_STATE_OPERATION, VECTOR_PAYLOAD_SCHEMA_V1)
            .await
            .map_err(storage_error)?;
        database
            .execute_write_batch(
                VECTOR_GENERATION_STATE_OPERATION,
                VECTOR_STATE_SLICE_SCHEMA_V1,
            )
            .await
            .map_err(storage_error)?;
        let initial_state = serde_json::to_string(&FakeVectorGenerationStoreV1::default())
            .map_err(storage_error)?;
        database
            .execute_write_engine(
                VECTOR_GENERATION_STATE_OPERATION,
                "INSERT OR IGNORE INTO semantic_vector_generation_state_v1 (
                    singleton, revision, state_json
                 ) VALUES (1, 0, ?1)",
                params![initial_state],
            )
            .await
            .map_err(storage_error)?;
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
        let mut rows = database
            .engine_conn()
            .query(
                "SELECT state.revision, entry.value
                 FROM semantic_vector_generation_state_v1 AS state
                 JOIN json_each(
                     state.state_json,
                     '$.published.generations'
                 ) AS entry
                   ON entry.key = CAST(json_extract(
                       state.state_json,
                       '$.published.active_generation'
                   ) AS TEXT)
                 WHERE state.singleton = 1
                   AND entry.type = 'object'",
                (),
            )
            .await
            .map_err(storage_error)?;
        let Some(row) = rows.next().await.map_err(storage_error)? else {
            return Ok(None);
        };
        let revision = row.get::<i64>(0).map_err(storage_error)?;
        let generation_json = row.get::<String>(1).map_err(storage_error)?;
        drop(rows);
        let mut generation: PublishedVectorGenerationV1 =
            serde_json::from_str(&generation_json).map_err(storage_error)?;
        drop(generation_json);
        hydrate_generation_slices(database, VECTOR_STATE_SLICE_TABLE_V1, &mut generation).await?;
        hydrate_generation_payloads(database, VECTOR_PAYLOAD_TABLE_V1, &mut generation).await?;
        generation.validate_persisted()?;
        Ok(Some(ActiveVectorGenerationSnapshotV1 {
            revision,
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
        let mut rows = database
            .engine_conn()
            .query(
                "SELECT 1
                 FROM semantic_vector_generation_state_v1
                 WHERE singleton = 1
                   AND revision = ?1
                   AND CAST(json_extract(
                       state_json,
                       '$.published.active_generation'
                   ) AS TEXT) = ?2",
                params![revision, generation_id.as_digest().as_str()],
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
        let mut rows = database
            .engine_conn()
            .query(
                "SELECT entry.value
                 FROM semantic_vector_generation_state_v1 AS state
                 JOIN json_each(
                     state.state_json,
                     '$.published.generations'
                 ) AS entry
                   ON entry.key = ?1
                 WHERE state.singleton = 1
                   AND entry.type = 'object'",
                params![generation_id.as_digest().as_str()],
            )
            .await
            .map_err(storage_error)?;
        let Some(row) = rows.next().await.map_err(storage_error)? else {
            return Ok(None);
        };
        let generation_json = row.get::<String>(0).map_err(storage_error)?;
        drop(rows);
        let mut generation: PublishedVectorGenerationV1 =
            serde_json::from_str(&generation_json).map_err(storage_error)?;
        drop(generation_json);
        hydrate_generation_slices(database, VECTOR_STATE_SLICE_TABLE_V1, &mut generation).await?;
        hydrate_generation_payloads(database, VECTOR_PAYLOAD_TABLE_V1, &mut generation).await?;
        generation.validate_persisted()?;
        (generation.generation_id() == generation_id)
            .then_some(generation)
            .ok_or_else(|| {
                VectorGenerationStoreErrorV1::Storage(
                    "vector generation map key does not match its identity".to_owned(),
                )
            })
            .map(Some)
    }

    pub async fn begin_generation(
        &self,
        plan: VectorGenerationPlanV1,
    ) -> Result<VectorGenerationBuildIdV1, VectorGenerationStoreErrorV1> {
        self.mutate_state(|state| state.begin_generation(plan.clone()))
            .await
    }

    pub async fn rebuild_generation(
        &self,
        plan: VectorGenerationPlanV1,
    ) -> Result<VectorGenerationBuildIdV1, VectorGenerationStoreErrorV1> {
        self.mutate_retiring_state(|state| state.rebuild_generation(plan.clone()))
            .await
    }

    pub async fn cancel_generation(
        &self,
        build_id: &VectorGenerationBuildIdV1,
    ) -> Result<bool, VectorGenerationStoreErrorV1> {
        self.mutate_retiring_state(|state| Ok(state.cancel_generation(build_id)))
            .await
    }

    pub async fn commit_batch(
        &self,
        build_id: &VectorGenerationBuildIdV1,
        expected_checkpoint: Option<&VectorProjectionCheckpointV1>,
        prepared: PreparedVectorGenerationV1,
    ) -> Result<VectorProjectionCheckpointV1, VectorGenerationStoreErrorV1> {
        self.mutate_state(|state| state.commit_batch_ref(build_id, expected_checkpoint, &prepared))
            .await
    }

    pub async fn publish_generation(
        &self,
        build_id: &VectorGenerationBuildIdV1,
        expected_active_generation: Option<&VectorGenerationIdV1>,
    ) -> Result<VectorGenerationPublicationV1, VectorGenerationStoreErrorV1> {
        self.mutate_retiring_state(|state| {
            state.publish_generation(build_id, expected_active_generation)
        })
        .await
    }

    pub async fn activate_generation(
        &self,
        generation_id: &VectorGenerationIdV1,
        expected_active_generation: Option<&VectorGenerationIdV1>,
    ) -> Result<VectorGenerationPublicationV1, VectorGenerationStoreErrorV1> {
        self.mutate_retiring_state(|state| {
            state.activate_generation(generation_id, expected_active_generation)
        })
        .await
    }

    pub async fn deactivate_generation(
        &self,
        expected_active_generation: Option<&VectorGenerationIdV1>,
    ) -> Result<(), VectorGenerationStoreErrorV1> {
        self.mutate_retiring_state(|state| state.deactivate_generation(expected_active_generation))
            .await
    }

    /// Snapshot legacy generation identities without deserializing or
    /// returning any legacy vector payload.
    pub async fn read_legacy_inventory(
        &self,
    ) -> Result<DatabaseLegacyVectorInventoryV1, VectorGenerationStoreErrorV1> {
        let mut rows = self
            .database
            .engine_conn()
            .query(
                "SELECT state.revision,
                        json_type(state.state_json, '$.published.generations'),
                        json_type(state.state_json, '$.published.active_generation'),
                        CAST(json_extract(
                            state.state_json,
                            '$.published.active_generation'
                        ) AS TEXT),
                        entry.key,
                        entry.type,
                        CASE WHEN entry.type = 'object'
                             THEN CAST(json_extract(
                                 entry.value,
                                 '$.generation_id'
                             ) AS TEXT)
                        END,
                        CASE WHEN entry.type = 'object'
                             THEN CAST(json_extract(
                                 entry.value,
                                 '$.source_generation'
                             ) AS TEXT)
                        END
                 FROM semantic_vector_generation_state_v1 AS state
                 LEFT JOIN json_each(
                     state.state_json,
                     '$.published.generations'
                 ) AS entry
                 WHERE state.singleton = 1
                 ORDER BY entry.key",
                (),
            )
            .await
            .map_err(storage_error)?;
        let mut revision = None;
        let mut expected_active_generation = None;
        let mut entries = Vec::new();
        while let Some(row) = rows.next().await.map_err(storage_error)? {
            let row_revision = row.get::<i64>(0).map_err(storage_error)?;
            if revision
                .replace(row_revision)
                .is_some_and(|prior| prior != row_revision)
            {
                return Err(VectorGenerationStoreErrorV1::ConcurrentMutation);
            }
            if row
                .get::<Option<String>>(1)
                .map_err(storage_error)?
                .as_deref()
                != Some("object")
            {
                return Err(VectorGenerationStoreErrorV1::LegacyMigration(
                    "legacy generation inventory is not a JSON object".to_owned(),
                ));
            }
            let active_type = row.get::<Option<String>>(2).map_err(storage_error)?;
            let active_raw = row.get::<Option<String>>(3).map_err(storage_error)?;
            expected_active_generation = match (active_type.as_deref(), active_raw.as_deref()) {
                (None | Some("null"), None) => None,
                (Some("text"), Some(raw)) => Some(parse_vector_generation_id(raw)?),
                _ => {
                    return Err(VectorGenerationStoreErrorV1::LegacyMigration(
                        "legacy active generation identity is unreadable".to_owned(),
                    ));
                }
            };
            let Some(map_key) = row.get::<Option<String>>(4).map_err(storage_error)? else {
                continue;
            };
            let legacy_generation = parse_vector_generation_id(&map_key)?;
            let value_type = row.get::<Option<String>>(5).map_err(storage_error)?;
            let embedded_generation = row.get::<Option<String>>(6).map_err(storage_error)?;
            let source_generation = row.get::<Option<String>>(7).map_err(storage_error)?;
            let readable = value_type.as_deref() == Some("object")
                && embedded_generation
                    .as_deref()
                    .and_then(|raw| parse_vector_generation_id(raw).ok())
                    .as_ref()
                    == Some(&legacy_generation)
                && source_generation
                    .as_deref()
                    .and_then(|raw| CodeGenerationId::try_from(raw.to_owned()).ok())
                    .is_some();
            if readable {
                entries.push(LegacyVectorInventoryEntryV1::Readable {
                    legacy_generation,
                    source_generation: CodeGenerationId::try_from(
                        source_generation.unwrap_or_default(),
                    )
                    .map_err(|error| {
                        VectorGenerationStoreErrorV1::LegacyMigration(error.to_string())
                    })?,
                });
            } else {
                let reason_digest = canonical_sha256(&(
                    LEGACY_VECTOR_UNREADABLE_REASON_DOMAIN_V1,
                    &map_key,
                    &value_type,
                    &embedded_generation,
                    &source_generation,
                ))
                .map_err(storage_error)?;
                entries.push(LegacyVectorInventoryEntryV1::Unreadable {
                    legacy_generation,
                    reason_digest,
                });
            }
        }
        drop(rows);
        Ok(DatabaseLegacyVectorInventoryV1 {
            revision: revision.ok_or_else(|| {
                VectorGenerationStoreErrorV1::Storage(
                    "vector generation state row is missing".to_owned(),
                )
            })?,
            inventory: LegacyVectorInventoryV1 {
                expected_active_generation,
                entries,
            },
        })
    }

    /// Return a durable completed migration receipt, if atomic replacement
    /// already committed. A crash before replacement has no receipt and is
    /// therefore safely retried; a restart after replacement performs no
    /// second rebuild.
    pub(crate) async fn completed_legacy_migration_receipt(
        &self,
    ) -> Result<Option<LegacyVectorMigrationReceiptV1>, VectorGenerationStoreErrorV1> {
        let mut rows = self
            .database
            .engine_conn()
            .query(
                "SELECT entry.key, entry.value
                 FROM semantic_vector_generation_state_v1 AS state
                 JOIN json_each(
                     state.state_json,
                     '$.published.legacy_migration_receipts'
                 ) AS entry
                 WHERE state.singleton = 1
                   AND entry.type = 'object'
                 ORDER BY entry.key",
                (),
            )
            .await
            .map_err(storage_error)?;
        let mut completed = None;
        while let Some(row) = rows.next().await.map_err(storage_error)? {
            let key = row.get::<String>(0).map_err(storage_error)?;
            let receipt_json = row.get::<String>(1).map_err(storage_error)?;
            let receipt: LegacyVectorMigrationReceiptV1 =
                serde_json::from_str(&receipt_json).map_err(storage_error)?;
            receipt.validate().map_err(|error| {
                VectorGenerationStoreErrorV1::LegacyMigration(error.to_string())
            })?;
            if receipt.receipt_digest.as_str() != key {
                return Err(VectorGenerationStoreErrorV1::LegacyMigration(
                    "legacy migration receipt key does not match its digest".to_owned(),
                ));
            }
            completed = Some(receipt);
        }
        Ok(completed)
    }

    /// Replace the complete legacy state with scratch-built canonical
    /// generations in one guarded writer transaction. Unreadable state is
    /// copied into an isolated quarantine table by `SQLite` itself; its bytes
    /// never cross the Rust migration boundary.
    pub(crate) async fn replace_legacy_vectors_atomically(
        &self,
        inventory: &DatabaseLegacyVectorInventoryV1,
        mut replacement: FakeVectorGenerationStoreV1,
        transaction: &LegacyVectorMigrationOwnerTransactionV1,
    ) -> Result<LegacyVectorMigrationReceiptV1, VectorGenerationStoreErrorV1> {
        if inventory.inventory.expected_active_generation
            != transaction.expected_prior_active_generation
        {
            return Err(VectorGenerationStoreErrorV1::StaleActiveGeneration);
        }
        let receipt = replacement.finish_legacy_replacement(transaction)?;
        validate_loaded_state(&replacement)?;
        // The scratch replacement was built entirely in memory, so nothing it
        // references is durable yet: every collection seals and writes here.
        let pending_slices = seal_external_state(&mut replacement, &BTreeSet::new())?;
        let referenced_slices = referenced_state_addresses(&mut replacement)?;
        let state_json = serde_json::to_string(&replacement).map_err(storage_error)?;
        let receipt_json = serde_json::to_string(&receipt).map_err(storage_error)?;
        let quarantined_items = receipt
            .items
            .iter()
            .filter(|item| item.outcome == LegacyVectorMigrationOutcomeKindV1::QuarantineUnreadable)
            .collect::<Vec<_>>();
        let receipt_digest = receipt.receipt_digest.as_str().to_owned();

        let writer = self
            .database
            .begin_write_transaction(VECTOR_GENERATION_STATE_OPERATION)
            .await
            .map_err(storage_error)?;
        let mut current_rows = writer
            .query_engine(
                "SELECT revision,
                        CAST(json_extract(
                            state_json,
                            '$.published.active_generation'
                        ) AS TEXT)
                 FROM semantic_vector_generation_state_v1
                 WHERE singleton = 1",
                (),
            )
            .await
            .map_err(storage_error)?;
        let current = current_rows
            .next()
            .await
            .map_err(storage_error)?
            .ok_or_else(|| {
                VectorGenerationStoreErrorV1::Storage(
                    "vector generation state row is missing".to_owned(),
                )
            })?;
        let current_revision = current.get::<i64>(0).map_err(storage_error)?;
        let current_active = current
            .get::<Option<String>>(1)
            .map_err(storage_error)?
            .as_deref()
            .map(parse_vector_generation_id)
            .transpose()?;
        drop(current_rows);
        if current_revision != inventory.revision
            || current_active != inventory.inventory.expected_active_generation
        {
            writer.rollback().await.map_err(storage_error)?;
            return Err(VectorGenerationStoreErrorV1::ConcurrentMutation);
        }
        if !quarantined_items.is_empty() {
            writer
                .execute_batch_engine(LEGACY_VECTOR_QUARANTINE_SCHEMA_V1)
                .await
                .map_err(storage_error)?;
            for item in quarantined_items {
                let reason = item.quarantine_reason_digest.as_ref().ok_or_else(|| {
                    VectorGenerationStoreErrorV1::LegacyMigration(
                        "quarantine receipt has no reason digest".to_owned(),
                    )
                })?;
                let inserted = writer
                    .execute_engine(
                        "INSERT INTO semantic_legacy_vector_quarantine_v1 (
                        receipt_digest,
                        legacy_generation,
                        reason_digest,
                        generation_json,
                        receipt_json
                     )
                     SELECT ?1,
                            ?2,
                            ?3,
                            CASE entry.type
                                WHEN 'text' THEN json_quote(entry.value)
                                WHEN 'null' THEN 'null'
                                ELSE CAST(entry.value AS TEXT)
                            END,
                            ?4
                     FROM semantic_vector_generation_state_v1 AS state,
                          json_each(
                              state.state_json,
                              '$.published.generations'
                          ) AS entry
                     WHERE state.singleton = 1
                       AND state.revision = ?5
                       AND entry.key = ?2",
                        params![
                            receipt_digest.clone(),
                            item.legacy_generation.as_digest().as_str(),
                            reason.as_str(),
                            receipt_json.clone(),
                            inventory.revision
                        ],
                    )
                    .await
                    .map_err(storage_error)?;
                if inserted != 1 {
                    writer.rollback().await.map_err(storage_error)?;
                    return Err(VectorGenerationStoreErrorV1::ConcurrentMutation);
                }
            }
        }
        // The scratch replacement was built entirely in memory, so none of its
        // payloads are durable yet. They land in the same transaction as the
        // state swap, and the prune releases every payload the replaced legacy
        // state used to reference.
        write_vector_payloads(
            &writer,
            VECTOR_PAYLOAD_TABLE_V1,
            &replacement,
            &BTreeSet::new(),
        )
        .await?;
        prune_unreferenced_vector_payloads(&writer, VECTOR_PAYLOAD_TABLE_V1, &replacement).await?;
        write_state_slices(&writer, VECTOR_STATE_SLICE_TABLE_V1, &pending_slices).await?;
        prune_unreferenced_state_slices(&writer, VECTOR_STATE_SLICE_TABLE_V1, &referenced_slices)
            .await?;
        let changed = writer
            .execute_engine(
                "UPDATE semantic_vector_generation_state_v1
                 SET revision = revision + 1, state_json = ?1
                 WHERE singleton = 1 AND revision = ?2",
                params![state_json, inventory.revision],
            )
            .await
            .map_err(storage_error)?;
        if changed != 1 {
            writer.rollback().await.map_err(storage_error)?;
            return Err(VectorGenerationStoreErrorV1::ConcurrentMutation);
        }
        writer.commit().await.map_err(storage_error)?;
        Ok(receipt)
    }

    pub async fn active_generation_id(
        &self,
    ) -> Result<Option<VectorGenerationIdV1>, VectorGenerationStoreErrorV1> {
        let (_, state, _) = self.load_state().await?;
        Ok(state.active_generation_id().cloned())
    }

    /// The checkpoint of one staged build, or `None` when no build is staged
    /// under that identity yet.
    pub async fn staged_checkpoint(
        &self,
        build_id: &VectorGenerationBuildIdV1,
    ) -> Result<Option<VectorProjectionCheckpointV1>, VectorGenerationStoreErrorV1> {
        let (_, state, _) = self.load_state().await?;
        Ok(state.staged_checkpoint(build_id).cloned())
    }

    pub async fn active_checkpoint(
        &self,
    ) -> Result<Option<VectorProjectionCheckpointV1>, VectorGenerationStoreErrorV1> {
        let (_, state, _) = self.load_state().await?;
        Ok(state.active_checkpoint().cloned())
    }

    pub async fn active_generation(
        &self,
    ) -> Result<Option<PublishedVectorGenerationV1>, VectorGenerationStoreErrorV1> {
        let (_, state, _) = self.load_state().await?;
        Ok(state.active_generation().cloned())
    }

    pub async fn active_generation_for(
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

    pub async fn generation(
        &self,
        generation_id: &VectorGenerationIdV1,
    ) -> Result<Option<PublishedVectorGenerationV1>, VectorGenerationStoreErrorV1> {
        let (_, state, _) = self.load_state().await?;
        Ok(state.generation(generation_id).cloned())
    }

    pub async fn physical_vector_values(
        &self,
        generation_id: &VectorGenerationIdV1,
        chunk_id: &CodeSearchChunkId,
    ) -> Result<Option<Arc<[f32]>>, VectorGenerationStoreErrorV1> {
        let (_, state, _) = self.load_state().await?;
        Ok(state.physical_vector_values(generation_id, chunk_id))
    }

    async fn mutate_state<ResultValue>(
        &self,
        mutation: impl FnMut(
            &mut FakeVectorGenerationStoreV1,
        ) -> Result<ResultValue, VectorGenerationStoreErrorV1>,
    ) -> Result<ResultValue, VectorGenerationStoreErrorV1> {
        self.mutate_state_with_reclamation(false, mutation).await
    }

    /// As [`Self::mutate_state`], but also reclaims payload rows the committed
    /// state no longer references. Used by the mutations that retire staged or
    /// published generations.
    async fn mutate_retiring_state<ResultValue>(
        &self,
        mutation: impl FnMut(
            &mut FakeVectorGenerationStoreV1,
        ) -> Result<ResultValue, VectorGenerationStoreErrorV1>,
    ) -> Result<ResultValue, VectorGenerationStoreErrorV1> {
        self.mutate_state_with_reclamation(true, mutation).await
    }

    async fn mutate_state_with_reclamation<ResultValue>(
        &self,
        reclaim_unreferenced: bool,
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
            write_vector_payloads(&transaction, VECTOR_PAYLOAD_TABLE_V1, &state, &load.durable)
                .await?;
            write_state_slices(&transaction, VECTOR_STATE_SLICE_TABLE_V1, &pending_slices).await?;
            if reclaim_unreferenced {
                prune_unreferenced_vector_payloads(&transaction, VECTOR_PAYLOAD_TABLE_V1, &state)
                    .await?;
                let referenced = referenced_state_addresses(&mut state)?;
                prune_unreferenced_state_slices(
                    &transaction,
                    VECTOR_STATE_SLICE_TABLE_V1,
                    &referenced,
                )
                .await?;
            }
            let changed = transaction
                .execute_engine(
                    "UPDATE semantic_vector_generation_state_v1
                     SET revision = revision + 1, state_json = ?1
                     WHERE singleton = 1 AND revision = ?2",
                    params![state_json, revision],
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
                 FROM semantic_vector_generation_state_v1
                 WHERE singleton = 1",
                (),
            )
            .await
            .map_err(storage_error)?;
        let row = rows.next().await.map_err(storage_error)?.ok_or_else(|| {
            VectorGenerationStoreErrorV1::Storage(
                "vector generation state row is missing".to_string(),
            )
        })?;
        let revision = row.get::<i64>(0).map_err(storage_error)?;
        let state_json = row.get::<String>(1).map_err(storage_error)?;
        drop(rows);
        let mut state: FakeVectorGenerationStoreV1 =
            serde_json::from_str(&state_json).map_err(storage_error)?;
        drop(state_json);
        let (durable_slices, inline_collections) =
            hydrate_external_state(self.database, VECTOR_STATE_SLICE_TABLE_V1, &mut state).await?;
        let mut load =
            hydrate_vector_payloads(self.database, VECTOR_PAYLOAD_TABLE_V1, &mut state).await?;
        load.durable_slices = durable_slices;
        load.migrated_inline_collections = inline_collections;
        state.ensure_physical_reuse_index()?;
        validate_loaded_state(&state)?;
        Ok((revision, state, load))
    }
}
