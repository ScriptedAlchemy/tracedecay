impl DatabaseVectorGenerationStoreV1<'_> {
    /// Reclaim at most `row_budget` owner or physical rows.
    ///
    /// Cancellation only queues a build. This bounded worker walks indexed
    /// owner rows, then deletes unowned payload/slice rows one at a time.
    /// `true` means more queued work remains.
    pub(crate) async fn reclaim_retired_generation_page(
        &self,
        row_budget: usize,
    ) -> Result<bool, VectorGenerationStoreErrorV1> {
        if row_budget == 0 {
            return Err(VectorGenerationStoreErrorV1::InvalidPlan(
                "vector generation reclamation row budget must be positive".to_owned(),
            ));
        }
        let transaction = self
            .database
            .begin_write_transaction("reclaim retired semantic vector generation")
            .await
            .map_err(storage_error)?;
        let mut remaining = row_budget;

        let mut retired_rows = transaction
            .query_engine(
                "SELECT build_id
                 FROM semantic_vector_generation_retired_v1
                 ORDER BY build_id
                 LIMIT 1",
                (),
            )
            .await
            .map_err(storage_error)?;
        let retired_build = retired_rows
            .next()
            .await
            .map_err(storage_error)?
            .map(|row| row.get::<String>(0).map_err(storage_error))
            .transpose()?;
        drop(retired_rows);
        if let Some(build_id) = retired_build.as_deref() {
            while remaining > 0 {
                let owner = next_generation_owner(&transaction, build_id).await?;
                let Some((kind, address)) = owner else {
                    break;
                };
                let (owner_table, owner_column) = match kind {
                    VectorResourceKindV1::Payload => {
                        ("semantic_vector_payload_owner_v1", "output_digest")
                    }
                    VectorResourceKindV1::StateSlice => (
                        "semantic_vector_state_slice_owner_v1",
                        "collection_digest",
                    ),
                };
                transaction
                    .execute_engine(
                        &format!(
                            "DELETE FROM {owner_table}
                             WHERE build_id = ?1
                               AND {owner_column} = ?2"
                        ),
                        params![build_id, &address],
                    )
                    .await
                    .map_err(storage_error)?;
                if !resource_has_owner(&transaction, kind, &address).await? {
                    transaction
                        .execute_engine(
                            "INSERT OR IGNORE INTO semantic_vector_orphan_resource_v1 (
                                kind, address
                             ) VALUES (?1, ?2)",
                            params![kind.as_str(), address],
                        )
                        .await
                        .map_err(storage_error)?;
                }
                remaining -= 1;
            }
            if !retired_build_has_owners(&transaction, build_id).await? {
                transaction
                    .execute_engine(
                        "DELETE FROM semantic_vector_generation_retired_v1
                         WHERE build_id = ?1",
                        params![build_id],
                    )
                    .await
                    .map_err(storage_error)?;
            }
        }

        while remaining > 0 {
            let Some((kind, address)) = next_orphan_resource(&transaction).await? else {
                break;
            };
            if resource_has_owner(&transaction, kind, &address).await? {
                delete_orphan_resource(&transaction, kind, &address).await?;
                continue;
            }
            match kind {
                VectorResourceKindV1::Payload => {
                    transaction
                        .execute_engine(
                            "DELETE FROM semantic_vector_payload_v1
                             WHERE output_digest = ?1",
                            params![&address],
                        )
                        .await
                        .map_err(storage_error)?;
                    delete_orphan_resource(&transaction, kind, &address).await?;
                }
                VectorResourceKindV1::StateSlice => {
                    transaction
                        .execute_engine(
                            "DELETE FROM semantic_vector_state_slice_v1
                             WHERE rowid IN (
                                 SELECT rowid
                                 FROM semantic_vector_state_slice_v1
                                 WHERE collection_digest = ?1
                                 ORDER BY ordinal
                                 LIMIT 1
                             )",
                            params![&address],
                        )
                        .await
                        .map_err(storage_error)?;
                    if !state_slice_address_exists(&transaction, &address).await? {
                        delete_orphan_resource(&transaction, kind, &address).await?;
                    }
                }
            }
            remaining -= 1;
        }

        let has_more = vector_gc_has_work(&transaction).await?;
        transaction.commit().await.map_err(storage_error)?;
        Ok(has_more)
    }
}

#[derive(Clone, Copy)]
enum VectorResourceKindV1 {
    Payload,
    StateSlice,
}

impl VectorResourceKindV1 {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Payload => "payload",
            Self::StateSlice => "state_slice",
        }
    }
}

async fn next_generation_owner(
    transaction: &tracedecay_runtime_core::db::DatabaseWriteTransaction<'_>,
    build_id: &str,
) -> Result<Option<(VectorResourceKindV1, String)>, VectorGenerationStoreErrorV1> {
    let mut rows = transaction
        .query_engine(
            "SELECT kind, address
             FROM (
                 SELECT 'payload' AS kind, output_digest AS address
                 FROM semantic_vector_payload_owner_v1
                 WHERE build_id = ?1
                 UNION ALL
                 SELECT 'state_slice' AS kind, collection_digest AS address
                 FROM semantic_vector_state_slice_owner_v1
                 WHERE build_id = ?1
             )
             ORDER BY kind, address
             LIMIT 1",
            params![build_id],
        )
        .await
        .map_err(storage_error)?;
    let owner = rows
        .next()
        .await
        .map_err(storage_error)?
        .map(|row| {
            let kind = match row.get::<String>(0).map_err(storage_error)?.as_str() {
                "payload" => VectorResourceKindV1::Payload,
                "state_slice" => VectorResourceKindV1::StateSlice,
                _ => {
                    return Err(VectorGenerationStoreErrorV1::Storage(
                        "unknown vector resource owner kind".to_owned(),
                    ));
                }
            };
            Ok((kind, row.get::<String>(1).map_err(storage_error)?))
        })
        .transpose()?;
    drop(rows);
    Ok(owner)
}

async fn resource_has_owner(
    transaction: &tracedecay_runtime_core::db::DatabaseWriteTransaction<'_>,
    kind: VectorResourceKindV1,
    address: &str,
) -> Result<bool, VectorGenerationStoreErrorV1> {
    let (table, column) = match kind {
        VectorResourceKindV1::Payload => {
            ("semantic_vector_payload_owner_v1", "output_digest")
        }
        VectorResourceKindV1::StateSlice => (
            "semantic_vector_state_slice_owner_v1",
            "collection_digest",
        ),
    };
    let mut rows = transaction
        .query_engine(
            &format!(
                "SELECT 1 FROM {table}
                 WHERE {column} = ?1
                 LIMIT 1"
            ),
            params![address],
        )
        .await
        .map_err(storage_error)?;
    let exists = rows.next().await.map_err(storage_error)?.is_some();
    drop(rows);
    Ok(exists)
}

async fn retired_build_has_owners(
    transaction: &tracedecay_runtime_core::db::DatabaseWriteTransaction<'_>,
    build_id: &str,
) -> Result<bool, VectorGenerationStoreErrorV1> {
    Ok(next_generation_owner(transaction, build_id).await?.is_some())
}

async fn next_orphan_resource(
    transaction: &tracedecay_runtime_core::db::DatabaseWriteTransaction<'_>,
) -> Result<Option<(VectorResourceKindV1, String)>, VectorGenerationStoreErrorV1> {
    let mut rows = transaction
        .query_engine(
            "SELECT kind, address
             FROM semantic_vector_orphan_resource_v1
             ORDER BY kind, address
             LIMIT 1",
            (),
        )
        .await
        .map_err(storage_error)?;
    let orphan = rows
        .next()
        .await
        .map_err(storage_error)?
        .map(|row| {
            let kind = match row.get::<String>(0).map_err(storage_error)?.as_str() {
                "payload" => VectorResourceKindV1::Payload,
                "state_slice" => VectorResourceKindV1::StateSlice,
                _ => {
                    return Err(VectorGenerationStoreErrorV1::Storage(
                        "unknown orphan vector resource kind".to_owned(),
                    ));
                }
            };
            Ok((kind, row.get::<String>(1).map_err(storage_error)?))
        })
        .transpose()?;
    drop(rows);
    Ok(orphan)
}

async fn delete_orphan_resource(
    transaction: &tracedecay_runtime_core::db::DatabaseWriteTransaction<'_>,
    kind: VectorResourceKindV1,
    address: &str,
) -> Result<(), VectorGenerationStoreErrorV1> {
    transaction
        .execute_engine(
            "DELETE FROM semantic_vector_orphan_resource_v1
             WHERE kind = ?1
               AND address = ?2",
            params![kind.as_str(), address],
        )
        .await
        .map_err(storage_error)?;
    Ok(())
}

async fn state_slice_address_exists(
    transaction: &tracedecay_runtime_core::db::DatabaseWriteTransaction<'_>,
    address: &str,
) -> Result<bool, VectorGenerationStoreErrorV1> {
    let mut rows = transaction
        .query_engine(
            "SELECT 1
             FROM semantic_vector_state_slice_v1
             WHERE collection_digest = ?1
             LIMIT 1",
            params![address],
        )
        .await
        .map_err(storage_error)?;
    let exists = rows.next().await.map_err(storage_error)?.is_some();
    drop(rows);
    Ok(exists)
}

async fn vector_gc_has_work(
    transaction: &tracedecay_runtime_core::db::DatabaseWriteTransaction<'_>,
) -> Result<bool, VectorGenerationStoreErrorV1> {
    let mut rows = transaction
        .query_engine(
            "SELECT 1 FROM semantic_vector_generation_retired_v1
             UNION ALL
             SELECT 1 FROM semantic_vector_orphan_resource_v1
             LIMIT 1",
            (),
        )
        .await
        .map_err(storage_error)?;
    let has_work = rows.next().await.map_err(storage_error)?.is_some();
    drop(rows);
    Ok(has_work)
}
