struct LoadedStagedGenerationV1 {
    revision: i64,
    staged: StagedVectorGenerationV1,
    durable_payloads: BTreeSet<ContentDigest>,
    durable_slices: BTreeSet<ContentDigest>,
}

struct ActiveGenerationPointerV1 {
    revision: i64,
    generation_id: Option<VectorGenerationIdV1>,
}

async fn initialize_active_generation_pointer(
    transaction: &tracedecay_runtime_core::db::DatabaseWriteTransaction<'_>,
    database: &Database,
) -> Result<(), VectorGenerationStoreErrorV1> {
    let shard_id_json =
        serde_json::to_string(&database.retained_runtime().binding().shard_id)
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
    let mut rows = transaction
        .query_engine(
            "SELECT shard_id_json
             FROM semantic_vector_active_generation_v1
             WHERE singleton = 1",
            (),
        )
        .await
        .map_err(storage_error)?;
    let actual = rows
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
    drop(rows);
    let actual: tracedecay_store::StoreShardIdV1 =
        serde_json::from_str(&actual)
            .map_err(|_| VectorGenerationStoreErrorV1::ShardIdentityMismatch)?;
    if &actual != &database.retained_runtime().binding().shard_id {
        return Err(VectorGenerationStoreErrorV1::ShardIdentityMismatch);
    }
    Ok(())
}

async fn load_staged_generation_record(
    database: &Database,
    build_id: &VectorGenerationBuildIdV1,
) -> Result<LoadedStagedGenerationV1, VectorGenerationStoreErrorV1> {
    let mut rows = database
        .engine_conn()
        .query(
            "SELECT revision, record_json
             FROM semantic_vector_generation_v1
             WHERE build_id = ?1
               AND lifecycle = 'staged'",
            params![build_id.0.as_str()],
        )
        .await
        .map_err(storage_error)?;
    let Some(row) = rows.next().await.map_err(storage_error)? else {
        return Err(VectorGenerationStoreErrorV1::UnknownBuild);
    };
    let revision = row.get::<i64>(0).map_err(storage_error)?;
    let record_json = row.get::<String>(1).map_err(storage_error)?;
    drop(rows);
    let staged: StagedVectorGenerationV1 =
        serde_json::from_str(&record_json).map_err(storage_error)?;
    let mut state = FakeVectorGenerationStoreV1::default();
    state.staged.insert(build_id.clone(), staged);
    let (durable_slices, inline_collections) =
        hydrate_external_state(database, VECTOR_STATE_SLICE_TABLE_V1, &mut state).await?;
    let payload_load =
        hydrate_vector_payloads(database, VECTOR_PAYLOAD_TABLE_V1, &mut state).await?;
    if inline_collections || payload_load.migrated_inline_payloads {
        return Err(VectorGenerationStoreErrorV1::Storage(
            "normalized vector generation record contains inline corpus state".to_owned(),
        ));
    }
    let staged = state
        .staged
        .remove(build_id)
        .ok_or(VectorGenerationStoreErrorV1::UnknownBuild)?;
    Ok(LoadedStagedGenerationV1 {
        revision,
        staged,
        durable_payloads: payload_load.durable,
        durable_slices,
    })
}

async fn load_published_generation_record(
    database: &Database,
    generation_id: &VectorGenerationIdV1,
) -> Result<Option<PublishedVectorGenerationV1>, VectorGenerationStoreErrorV1> {
    load_published_generation_record_with_control(database, generation_id, &|| false).await
}

async fn load_published_generation_record_with_control(
    database: &Database,
    generation_id: &VectorGenerationIdV1,
    is_cancelled: &(dyn Fn() -> bool + Sync),
) -> Result<Option<PublishedVectorGenerationV1>, VectorGenerationStoreErrorV1> {
    ensure_vector_read_not_cancelled(is_cancelled)?;
    let mut rows = database
        .engine_conn()
        .query(
            "SELECT record_json
             FROM semantic_vector_generation_v1
             WHERE lifecycle = 'published'
               AND generation_id = ?1",
            params![generation_id.as_digest().as_str()],
        )
        .await
        .map_err(storage_error)?;
    let Some(row) = rows.next().await.map_err(storage_error)? else {
        return Ok(None);
    };
    let record_json = row.get::<String>(0).map_err(storage_error)?;
    drop(rows);
    let mut generation: PublishedVectorGenerationV1 =
        serde_json::from_str(&record_json).map_err(storage_error)?;
    hydrate_generation_slices_with_control(
        database,
        VECTOR_STATE_SLICE_TABLE_V1,
        &mut generation,
        is_cancelled,
    )
    .await?;
    hydrate_generation_payloads_with_control(
        database,
        VECTOR_PAYLOAD_TABLE_V1,
        &mut generation,
        is_cancelled,
    )
    .await?;
    ensure_vector_read_not_cancelled(is_cancelled)?;
    generation.validate_persisted()?;
    if generation.generation_id() != generation_id {
        return Err(VectorGenerationStoreErrorV1::Storage(
            "vector generation row identity does not match its record".to_owned(),
        ));
    }
    Ok(Some(generation))
}

async fn active_generation_pointer(
    database: &Database,
) -> Result<ActiveGenerationPointerV1, VectorGenerationStoreErrorV1> {
    let mut rows = database
        .engine_conn()
        .query(
            "SELECT revision, shard_id_json, generation_id
             FROM semantic_vector_active_generation_v1
             WHERE singleton = 1",
            (),
        )
        .await
        .map_err(storage_error)?;
    let Some(row) = rows.next().await.map_err(storage_error)? else {
        return Ok(ActiveGenerationPointerV1 {
            revision: 0,
            generation_id: None,
        });
    };
    let revision = row.get::<i64>(0).map_err(storage_error)?;
    let shard_json = row.get::<String>(1).map_err(storage_error)?;
    let generation_id = row
        .get::<Option<String>>(2)
        .map_err(storage_error)?
        .as_deref()
        .map(parse_vector_generation_id)
        .transpose()?;
    drop(rows);
    let shard: tracedecay_store::StoreShardIdV1 = serde_json::from_str(&shard_json)
        .map_err(|_| VectorGenerationStoreErrorV1::ShardIdentityMismatch)?;
    if &shard != &database.retained_runtime().binding().shard_id {
        return Err(VectorGenerationStoreErrorV1::ShardIdentityMismatch);
    }
    Ok(ActiveGenerationPointerV1 {
        revision,
        generation_id,
    })
}

fn staged_record_state(
    build_id: &VectorGenerationBuildIdV1,
    staged: StagedVectorGenerationV1,
) -> FakeVectorGenerationStoreV1 {
    let mut state = FakeVectorGenerationStoreV1::default();
    state.staged.insert(build_id.clone(), staged);
    state
}

fn staged_record_json(
    state: &FakeVectorGenerationStoreV1,
    build_id: &VectorGenerationBuildIdV1,
) -> Result<String, VectorGenerationStoreErrorV1> {
    serde_json::to_string(
        state
            .staged
            .get(build_id)
            .ok_or(VectorGenerationStoreErrorV1::UnknownBuild)?,
    )
    .map_err(storage_error)
}

fn expected_active_matches(
    actual: Option<&VectorGenerationIdV1>,
    expected: Option<&VectorGenerationIdV1>,
) -> Result<(), VectorGenerationStoreErrorV1> {
    if actual == expected {
        Ok(())
    } else {
        Err(VectorGenerationStoreErrorV1::StaleActiveGeneration)
    }
}

async fn write_generation_resource_owners(
    transaction: &tracedecay_runtime_core::db::DatabaseWriteTransaction<'_>,
    build_id: &VectorGenerationBuildIdV1,
    payloads: &BTreeSet<ContentDigest>,
    state_slices: &BTreeSet<ContentDigest>,
) -> Result<(), VectorGenerationStoreErrorV1> {
    let payload_rows = payloads.iter().collect::<Vec<_>>();
    for group in payload_rows.chunks(VECTOR_PAYLOAD_STATEMENT_ROWS) {
        let tuples = (0..group.len())
            .map(|index| format!("(?{}, ?{})", index * 2 + 1, index * 2 + 2))
            .collect::<Vec<_>>()
            .join(", ");
        let mut values = Vec::with_capacity(group.len() * 2);
        for payload in group {
            values.push(tracedecay_runtime_core::db::engine::Value::Text(
                build_id.0.as_str().to_owned(),
            ));
            values.push(tracedecay_runtime_core::db::engine::Value::Text(
                payload.as_str().to_owned(),
            ));
        }
        transaction
            .execute_engine(
                &format!(
                    "INSERT OR IGNORE INTO semantic_vector_payload_owner_v1 (
                    build_id, output_digest
                 ) VALUES {tuples}"
                ),
                tracedecay_runtime_core::db::engine::params_from_iter(values),
            )
            .await
            .map_err(storage_error)?;
    }
    let state_slice_rows = state_slices.iter().collect::<Vec<_>>();
    for group in state_slice_rows.chunks(VECTOR_STATE_ADDRESS_STATEMENT_ROWS) {
        let tuples = (0..group.len())
            .map(|index| format!("(?{}, ?{})", index * 2 + 1, index * 2 + 2))
            .collect::<Vec<_>>()
            .join(", ");
        let mut values = Vec::with_capacity(group.len() * 2);
        for address in group {
            values.push(tracedecay_runtime_core::db::engine::Value::Text(
                build_id.0.as_str().to_owned(),
            ));
            values.push(tracedecay_runtime_core::db::engine::Value::Text(
                address.as_str().to_owned(),
            ));
        }
        transaction
            .execute_engine(
                &format!(
                    "INSERT OR IGNORE INTO semantic_vector_state_slice_owner_v1 (
                    build_id, collection_digest
                 ) VALUES {tuples}"
                ),
                tracedecay_runtime_core::db::engine::params_from_iter(values),
            )
            .await
            .map_err(storage_error)?;
    }
    Ok(())
}
