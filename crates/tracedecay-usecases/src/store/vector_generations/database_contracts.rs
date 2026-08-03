/// Persistent adapter over the already-open project database.
///
/// Each build/generation owns one revisioned row. Publication validates and
/// hashes outside SQLite, then swaps that row and the shard-bound active
/// pointer in one constant-size transaction.
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

/// Union code-generation pins from immutable vector rows across every graph
/// database in a project store.
pub fn retained_vector_source_generations_from_read_only_project_store(
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
    let mut retained_sources = BTreeSet::new();
    let mut store_count = 0usize;
    for database_path in database_paths {
        if !database_path.is_file() {
            continue;
        }
        if let Some(sources) = retained_sources_from_optional_read_only_database(&database_path)? {
            store_count += 1;
            retained_sources.extend(sources);
        }
    }
    if store_count == 0 {
        return Err(VectorGenerationStoreErrorV1::Storage(format!(
            "no vector generation row store exists under '{}'",
            data_root.display()
        )));
    }
    Ok(retained_sources)
}

fn retained_sources_from_optional_read_only_database(
    database_path: &Path,
) -> Result<Option<BTreeSet<CodeGenerationId>>, VectorGenerationStoreErrorV1> {
    let connection =
        open_read_only_probe(database_path, BOUNDED_PROBE_BUSY_TIMEOUT).map_err(storage_error)?;
    let has_store = connection
        .query_row(
            "SELECT EXISTS(
                SELECT 1
                FROM sqlite_schema
                WHERE type = 'table'
                  AND name = 'semantic_vector_generation_v1'
             )",
            [],
            |row| row.get::<_, bool>(0),
        )
        .map_err(storage_error)?;
    if !has_store {
        return Ok(None);
    }
    let mut statement = connection
        .prepare(
            "SELECT CAST(json_extract(record_json, '$.source_generation') AS TEXT)
             FROM semantic_vector_generation_v1
             WHERE lifecycle = 'published'
             ORDER BY generation_id",
        )
        .map_err(storage_error)?;
    let mut rows = statement.query([]).map_err(storage_error)?;
    let mut retained_sources = BTreeSet::new();
    while let Some(row) = rows.next().map_err(storage_error)? {
        retained_sources.insert(
            CodeGenerationId::try_from(row.get::<_, String>(0).map_err(storage_error)?)
                .map_err(storage_error)?,
        );
    }
    Ok(Some(retained_sources))
}
