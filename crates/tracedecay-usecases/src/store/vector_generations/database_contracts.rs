/// Persistent adapter over the already-open project database.
///
/// The complete generation state is one canonical JSON value guarded by a
/// monotonically increasing revision. Every mutation is a single conditional
/// update, so a reader observes either the complete old state or the complete
/// new state. In particular, an immutable generation record cannot become
/// visible separately from its active-generation pointer.
pub struct DatabaseVectorGenerationStoreV1<'database> {
    database: &'database Database,
}

/// SQLite-backed, non-authoritative state used by the native semantic evaluator.
///
/// It executes the same generation state machine and writer path as
/// production, but uses an isolated row that is removed after the measured
/// run. It can therefore exercise publication/activation without changing the
/// project's active semantic generation.
pub(crate) struct DatabaseVectorEvaluationStoreV1<'database> {
    database: &'database Database,
    evaluation_id: String,
}

#[derive(Clone, Debug)]
pub(crate) struct ActiveVectorGenerationSnapshotV1 {
    revision: i64,
    generation: PublishedVectorGenerationV1,
}

impl ActiveVectorGenerationSnapshotV1 {
    pub(crate) const fn revision(&self) -> i64 {
        self.revision
    }

    pub(crate) fn generation(&self) -> &PublishedVectorGenerationV1 {
        &self.generation
    }

    pub(crate) fn into_generation(self) -> PublishedVectorGenerationV1 {
        self.generation
    }
}

/// Identity-only snapshot of the legacy state. The SQL adapter never returns
/// legacy vector payloads to Rust.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DatabaseLegacyVectorInventoryV1 {
    revision: i64,
    inventory: LegacyVectorInventoryV1,
}

impl LegacyVectorInventoryPortV1 for DatabaseLegacyVectorInventoryV1 {
    fn read_only_inventory(
        &self,
    ) -> Result<
        LegacyVectorInventoryV1,
        tracedecay_semantic::legacy_migration::LegacyVectorMigrationErrorV1,
    > {
        Ok(self.inventory.clone())
    }
}

/// Read only the code-generation identities named by structurally readable
/// vector generations, without opening a daemon runtime or deserializing vector
/// payloads. This is the offline equivalent of
/// [`DatabaseVectorGenerationStoreV1::read_legacy_inventory`] followed by
/// [`LegacyVectorInventoryV1::retained_readable_sources`].
#[cfg(test)]
pub(crate) fn retained_readable_sources_from_read_only_database(
    database_path: &Path,
) -> Result<BTreeSet<CodeGenerationId>, VectorGenerationStoreErrorV1> {
    retained_readable_sources_from_optional_read_only_database(database_path)?.ok_or_else(|| {
        VectorGenerationStoreErrorV1::Storage(format!(
            "vector generation state table is missing from '{}'",
            database_path.display()
        ))
    })
}

/// Union readable code-generation sources across every graph database in a
/// project store. Code-index files are project-scoped while vector inventories
/// may reside in the root graph database or a branch graph database, so an
/// offline sweep must conservatively mark sources from all inventories.
pub fn retained_readable_sources_from_read_only_project_store(
    data_root: &Path,
) -> Result<BTreeSet<CodeGenerationId>, VectorGenerationStoreErrorV1> {
    let mut database_paths = vec![data_root.join(tracedecay_runtime_core::config::DB_FILENAME)];
    let branches_root = data_root.join("branches");
    if let Ok(entries) = std::fs::read_dir(&branches_root) {
        for entry in entries {
            let entry = entry.map_err(storage_error)?;
            let path = entry.path();
            if path.extension().and_then(|extension| extension.to_str()) == Some("db") {
                database_paths.push(path);
            }
        }
    }
    database_paths.sort();
    let mut readable_sources = BTreeSet::new();
    let mut inventory_count = 0usize;
    for database_path in database_paths {
        if !database_path.is_file() {
            continue;
        }
        if let Some(sources) =
            retained_readable_sources_from_optional_read_only_database(&database_path)?
        {
            inventory_count += 1;
            readable_sources.extend(sources);
        }
    }
    if inventory_count == 0 {
        return Err(VectorGenerationStoreErrorV1::Storage(format!(
            "no vector generation inventory exists under '{}'",
            data_root.display()
        )));
    }
    Ok(readable_sources)
}

fn retained_readable_sources_from_optional_read_only_database(
    database_path: &Path,
) -> Result<Option<BTreeSet<CodeGenerationId>>, VectorGenerationStoreErrorV1> {
    let connection =
        open_read_only_probe(database_path, BOUNDED_PROBE_BUSY_TIMEOUT).map_err(storage_error)?;
    let has_inventory = connection
        .query_row(
            "SELECT EXISTS(
                SELECT 1
                FROM sqlite_schema
                WHERE type = 'table'
                  AND name = 'semantic_vector_generation_state_v1'
             )",
            [],
            |row| row.get::<_, bool>(0),
        )
        .map_err(storage_error)?;
    if !has_inventory {
        return Ok(None);
    }
    let (generations_type, active_type, active_raw) = connection
        .query_row(
            "SELECT json_type(state_json, '$.published.generations'),
                    json_type(state_json, '$.published.active_generation'),
                    CAST(json_extract(
                        state_json,
                        '$.published.active_generation'
                    ) AS TEXT)
             FROM semantic_vector_generation_state_v1
             WHERE singleton = 1",
            [],
            |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            },
        )
        .map_err(storage_error)?;
    if generations_type.as_deref() != Some("object") {
        return Err(VectorGenerationStoreErrorV1::LegacyMigration(
            "legacy generation inventory is not a JSON object".to_owned(),
        ));
    }
    match (active_type.as_deref(), active_raw.as_deref()) {
        (None | Some("null"), None) => {}
        (Some("text"), Some(raw)) => {
            parse_vector_generation_id(raw)?;
        }
        _ => {
            return Err(VectorGenerationStoreErrorV1::LegacyMigration(
                "legacy active generation identity is unreadable".to_owned(),
            ));
        }
    }
    let mut statement = connection
        .prepare(
            "SELECT entry.key,
                    entry.type,
                    CASE WHEN entry.type = 'object'
                         THEN CAST(json_extract(entry.value, '$.generation_id') AS TEXT)
                    END,
                    CASE WHEN entry.type = 'object'
                         THEN CAST(json_extract(entry.value, '$.source_generation') AS TEXT)
                    END
             FROM semantic_vector_generation_state_v1 AS state
             JOIN json_each(state.state_json, '$.published.generations') AS entry
             WHERE state.singleton = 1
             ORDER BY entry.key",
        )
        .map_err(storage_error)?;
    let mut rows = statement.query([]).map_err(storage_error)?;
    let mut readable_sources = BTreeSet::new();
    while let Some(row) = rows.next().map_err(storage_error)? {
        let map_key = row.get::<_, String>(0).map_err(storage_error)?;
        let value_type = row.get::<_, Option<String>>(1).map_err(storage_error)?;
        let embedded_generation = row.get::<_, Option<String>>(2).map_err(storage_error)?;
        let source_generation = row.get::<_, Option<String>>(3).map_err(storage_error)?;
        let legacy_generation = parse_vector_generation_id(&map_key)?;
        let embedded_matches = embedded_generation
            .as_deref()
            .and_then(|raw| parse_vector_generation_id(raw).ok())
            .as_ref()
            == Some(&legacy_generation);
        let source_generation =
            source_generation.and_then(|raw| CodeGenerationId::try_from(raw).ok());
        if value_type.as_deref() == Some("object")
            && embedded_matches
            && let Some(source_generation) = source_generation
        {
            readable_sources.insert(source_generation);
        }
    }
    Ok(Some(readable_sources))
}
