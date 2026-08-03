use tracedecay_runtime_core::db::engine::{Connection, Error, Executor, QueryExecutor, params};

/// Creates the observation projection schema at its final V4 shape, or
/// verifies that an existing store already carries that shape.
///
/// There is no ladder here. Every store is born at the shape below; a store at
/// any other shape is a typed refusal from [`verify_final_projection_shape`],
/// naming the wipe-and-recreate remedy rather than a migrate command.
pub(in super::super) async fn ensure_observation_projection_schema(
    conn: &impl Executor,
) -> Result<(), Error> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS observation_projection_provenance (
            projector_version TEXT NOT NULL,
            observation_id TEXT NOT NULL,
            output_ordinal INTEGER NOT NULL DEFAULT 0 CHECK(output_ordinal >= 0),
            receipt_id TEXT NOT NULL,
            output_provider TEXT NOT NULL,
            output_message_id TEXT NOT NULL,
            output_digest TEXT NOT NULL,
            message_created INTEGER NOT NULL CHECK(message_created IN (0, 1)),
            retrieval_anchor_id TEXT REFERENCES retrieval_anchors(anchor_id),
            PRIMARY KEY(projector_version, observation_id, output_ordinal),
            FOREIGN KEY(observation_id) REFERENCES observations(observation_id),
            FOREIGN KEY(receipt_id) REFERENCES sanitization_receipts(receipt_id)
        );
        CREATE TABLE IF NOT EXISTS observation_projection_checkpoints (
            projector_version TEXT PRIMARY KEY,
            last_sequence INTEGER NOT NULL CHECK(last_sequence >= 0)
        );
        CREATE TABLE IF NOT EXISTS observation_projection_aliases (
            projector_version TEXT NOT NULL,
            observation_id TEXT NOT NULL,
            output_provider TEXT NOT NULL,
            output_message_id TEXT NOT NULL,
            PRIMARY KEY(projector_version, observation_id),
            FOREIGN KEY(observation_id) REFERENCES observations(observation_id)
        );
        CREATE TABLE IF NOT EXISTS observation_projection_dispositions (
            projector_version TEXT NOT NULL,
            observation_id TEXT NOT NULL,
            receipt_id TEXT NOT NULL,
            reason TEXT NOT NULL,
            PRIMARY KEY(projector_version, observation_id),
            FOREIGN KEY(observation_id) REFERENCES observations(observation_id),
            FOREIGN KEY(receipt_id) REFERENCES sanitization_receipts(receipt_id)
        );
        CREATE TABLE IF NOT EXISTS observation_workflow_facts (
            projector_version TEXT NOT NULL,
            observation_id TEXT NOT NULL,
            fact_ordinal INTEGER NOT NULL CHECK(fact_ordinal >= 0),
            receipt_id TEXT NOT NULL,
            observation_sequence INTEGER NOT NULL CHECK(observation_sequence > 0),
            provider TEXT NOT NULL,
            session_id TEXT NOT NULL,
            semantic_kind TEXT NOT NULL CHECK(
                semantic_kind IN ('goal', 'plan', 'todo_list', 'todo_item', 'task')
            ),
            provider_reference TEXT,
            item_id TEXT,
            parent_reference TEXT,
            list_reference TEXT,
            state TEXT,
            status TEXT,
            item_order INTEGER CHECK(item_order IS NULL OR item_order >= 0),
            native_revision TEXT,
            event_sequence INTEGER CHECK(event_sequence IS NULL OR event_sequence >= 0),
            source_sequence INTEGER CHECK(source_sequence IS NULL OR source_sequence >= 0),
            native_timestamp INTEGER,
            ordering_domain TEXT NOT NULL,
            content_json TEXT CHECK(content_json IS NULL OR json_valid(content_json)),
            content_text TEXT NOT NULL,
            output_digest TEXT NOT NULL,
            retrieval_anchor_id TEXT REFERENCES retrieval_anchors(anchor_id),
            PRIMARY KEY(projector_version, observation_id, fact_ordinal),
            FOREIGN KEY(observation_id) REFERENCES observations(observation_id),
            FOREIGN KEY(receipt_id) REFERENCES sanitization_receipts(receipt_id)
        );
        CREATE TABLE IF NOT EXISTS observation_projection_rebuilds (
            projector_version TEXT PRIMARY KEY,
            generation TEXT NOT NULL,
            frontier_sequence INTEGER NOT NULL CHECK(frontier_sequence >= 0),
            aliases_staged_through INTEGER NOT NULL DEFAULT 0
                CHECK(aliases_staged_through >= 0),
            staged_through INTEGER NOT NULL DEFAULT 0 CHECK(staged_through >= 0),
            projected_rows INTEGER NOT NULL DEFAULT 0 CHECK(projected_rows >= 0),
            skipped_observations INTEGER NOT NULL DEFAULT 0 CHECK(skipped_observations >= 0),
            state TEXT NOT NULL CHECK(state IN ('aliasing', 'building', 'ready')),
            UNIQUE(projector_version, generation)
        );
        CREATE TABLE IF NOT EXISTS observation_projection_rebuild_aliases (
            projector_version TEXT NOT NULL,
            generation TEXT NOT NULL,
            observation_id TEXT NOT NULL,
            output_provider TEXT NOT NULL,
            output_message_id TEXT NOT NULL,
            PRIMARY KEY(projector_version, generation, observation_id),
            FOREIGN KEY(projector_version, generation)
                REFERENCES observation_projection_rebuilds(projector_version, generation)
                ON DELETE CASCADE,
            FOREIGN KEY(observation_id) REFERENCES observations(observation_id)
        );
        CREATE TABLE IF NOT EXISTS observation_projection_rebuild_sessions (
            projector_version TEXT NOT NULL,
            generation TEXT NOT NULL,
            provider TEXT NOT NULL,
            session_id TEXT NOT NULL,
            session_json TEXT NOT NULL CHECK(json_valid(session_json)),
            PRIMARY KEY(projector_version, generation, provider, session_id),
            FOREIGN KEY(projector_version, generation)
                REFERENCES observation_projection_rebuilds(projector_version, generation)
                ON DELETE CASCADE
        );
        CREATE TABLE IF NOT EXISTS observation_projection_rebuild_messages (
            projector_version TEXT NOT NULL,
            generation TEXT NOT NULL,
            output_provider TEXT NOT NULL,
            output_message_id TEXT NOT NULL,
            message_json TEXT NOT NULL CHECK(json_valid(message_json)),
            content_hash TEXT NOT NULL,
            snippet_text TEXT NOT NULL,
            index_text TEXT NOT NULL,
            PRIMARY KEY(projector_version, generation, output_provider, output_message_id),
            FOREIGN KEY(projector_version, generation)
                REFERENCES observation_projection_rebuilds(projector_version, generation)
                ON DELETE CASCADE
        );
        CREATE TABLE IF NOT EXISTS observation_projection_rebuild_provenance (
            projector_version TEXT NOT NULL,
            generation TEXT NOT NULL,
            observation_id TEXT NOT NULL,
            output_ordinal INTEGER NOT NULL CHECK(output_ordinal >= 0),
            receipt_id TEXT NOT NULL,
            output_provider TEXT NOT NULL,
            output_message_id TEXT NOT NULL,
            output_digest TEXT NOT NULL,
            message_created INTEGER NOT NULL CHECK(message_created IN (0, 1)),
            retrieval_anchor_id TEXT REFERENCES retrieval_anchors(anchor_id),
            PRIMARY KEY(projector_version, generation, observation_id, output_ordinal),
            FOREIGN KEY(projector_version, generation)
                REFERENCES observation_projection_rebuilds(projector_version, generation)
                ON DELETE CASCADE,
            FOREIGN KEY(observation_id) REFERENCES observations(observation_id),
            FOREIGN KEY(receipt_id) REFERENCES sanitization_receipts(receipt_id)
        );
        CREATE TABLE IF NOT EXISTS observation_projection_rebuild_dispositions (
            projector_version TEXT NOT NULL,
            generation TEXT NOT NULL,
            observation_id TEXT NOT NULL,
            receipt_id TEXT NOT NULL,
            reason TEXT NOT NULL,
            PRIMARY KEY(projector_version, generation, observation_id),
            FOREIGN KEY(projector_version, generation)
                REFERENCES observation_projection_rebuilds(projector_version, generation)
                ON DELETE CASCADE,
            FOREIGN KEY(observation_id) REFERENCES observations(observation_id),
            FOREIGN KEY(receipt_id) REFERENCES sanitization_receipts(receipt_id)
        );
        CREATE TABLE IF NOT EXISTS observation_projection_rebuild_workflow_facts (
            projector_version TEXT NOT NULL,
            generation TEXT NOT NULL,
            observation_id TEXT NOT NULL,
            fact_ordinal INTEGER NOT NULL CHECK(fact_ordinal >= 0),
            receipt_id TEXT NOT NULL,
            observation_sequence INTEGER NOT NULL CHECK(observation_sequence > 0),
            provider TEXT NOT NULL,
            session_id TEXT NOT NULL,
            semantic_kind TEXT NOT NULL CHECK(
                semantic_kind IN ('goal', 'plan', 'todo_list', 'todo_item', 'task')
            ),
            provider_reference TEXT,
            item_id TEXT,
            parent_reference TEXT,
            list_reference TEXT,
            state TEXT,
            status TEXT,
            item_order INTEGER CHECK(item_order IS NULL OR item_order >= 0),
            native_revision TEXT,
            event_sequence INTEGER CHECK(event_sequence IS NULL OR event_sequence >= 0),
            source_sequence INTEGER CHECK(source_sequence IS NULL OR source_sequence >= 0),
            native_timestamp INTEGER,
            ordering_domain TEXT NOT NULL,
            content_json TEXT CHECK(content_json IS NULL OR json_valid(content_json)),
            content_text TEXT NOT NULL,
            output_digest TEXT NOT NULL,
            retrieval_anchor_id TEXT REFERENCES retrieval_anchors(anchor_id),
            PRIMARY KEY(projector_version, generation, observation_id, fact_ordinal),
            FOREIGN KEY(projector_version, generation)
                REFERENCES observation_projection_rebuilds(projector_version, generation)
                ON DELETE CASCADE,
            FOREIGN KEY(observation_id) REFERENCES observations(observation_id),
            FOREIGN KEY(receipt_id) REFERENCES sanitization_receipts(receipt_id)
        );",
    )
    .await?;
    verify_final_projection_shape(conn).await?;
    ensure_v4_projection_binding_triggers(conn).await
}

pub(in super::super) async fn ensure_observation_projection_performance_indexes(
    conn: &Connection,
) -> Result<(), Error> {
    // Install each historical-data index as its own durable authority-revalidated batch. These
    // cannot share the lease-bounded all-schema transaction: an interrupted
    // later build would otherwise roll back every earlier completed build. The
    // explicit revalidated-batch API keeps shutdown cancellation while allowing one
    // real-scale SQLite index build to use the schema transaction's fixed
    // 120-second lease instead of the ordinary 30-second statement deadline.
    for sql in [
        "CREATE INDEX IF NOT EXISTS idx_observation_projection_provenance_output
         ON observation_projection_provenance
            (projector_version, output_provider, output_message_id);",
        "CREATE INDEX IF NOT EXISTS idx_observation_projection_provenance_global_output
         ON observation_projection_provenance
            (output_provider, output_message_id, projector_version);",
        "CREATE INDEX IF NOT EXISTS idx_observation_workflow_facts_query
         ON observation_workflow_facts
            (provider, session_id, semantic_kind, status, observation_sequence);",
        "CREATE INDEX IF NOT EXISTS idx_observation_workflow_facts_item
         ON observation_workflow_facts
            (provider, session_id, semantic_kind, item_id, provider_reference,
             event_sequence, source_sequence, observation_sequence);",
        "CREATE INDEX IF NOT EXISTS idx_projection_rebuild_provenance_output
         ON observation_projection_rebuild_provenance
            (projector_version, generation, output_provider, output_message_id);",
        "CREATE INDEX IF NOT EXISTS idx_projection_rebuild_workflow_goal
         ON observation_projection_rebuild_workflow_facts
            (projector_version, generation, provider, session_id, semantic_kind,
             provider_reference, observation_sequence);",
        "CREATE INDEX IF NOT EXISTS idx_observations_identity_receipt
         ON observations (observation_id, receipt_id);",
        "CREATE INDEX IF NOT EXISTS idx_projection_dispositions_observation_receipt
         ON observation_projection_dispositions (observation_id, receipt_id);",
    ] {
        let transaction = conn.authorized_long_lease_transaction().await?;
        transaction.execute_authority_revalidated_batch(sql).await?;
        transaction.commit().await?;
    }
    Ok(())
}

async fn projection_table_column_exists(
    conn: &impl QueryExecutor,
    table: &str,
    column: &str,
) -> Result<bool, Error> {
    let mut rows = conn
        .query(
            "SELECT 1 FROM pragma_table_info(?1) WHERE name = ?2 COLLATE NOCASE",
            params![table, column],
        )
        .await?;
    Ok(rows.next().await?.is_some())
}

async fn ensure_v4_projection_binding_triggers(conn: &impl Executor) -> Result<(), Error> {
    conn.execute_batch(include_str!("projection_v4_binding_triggers.sql"))
        .await
}

const CURRENT_REBUILD_COLUMNS: &[&str] = &[
    "projector_version",
    "generation",
    "frontier_sequence",
    "aliases_staged_through",
    "staged_through",
    "projected_rows",
    "skipped_observations",
    "state",
];
const CURRENT_REBUILD_MESSAGE_COLUMNS: &[&str] = &[
    "projector_version",
    "generation",
    "output_provider",
    "output_message_id",
    "message_json",
    "content_hash",
    "snippet_text",
    "index_text",
];

async fn projection_rebuild_column_names(
    conn: &impl QueryExecutor,
    table: &str,
) -> Result<Vec<String>, Error> {
    let mut rows = conn
        .query(
            "SELECT name FROM pragma_table_xinfo(?1) ORDER BY cid",
            params![table],
        )
        .await?;
    let mut columns = Vec::new();
    while let Some(row) = rows.next().await? {
        columns.push(row.get::<String>(0)?);
    }
    Ok(columns)
}

fn columns_match(actual: &[String], expected: &[&str]) -> bool {
    actual
        .iter()
        .map(String::as_str)
        .eq(expected.iter().copied())
}

/// Column set every observation-projection table carries at the final V4
/// shape. A store is either created at this shape by
/// [`ensure_observation_projection_schema`] or it is not a shape this binary
/// supports; there is no ladder between shapes.
const CURRENT_PROVENANCE_COLUMNS: &[&str] = &[
    "projector_version",
    "observation_id",
    "output_ordinal",
    "receipt_id",
    "output_provider",
    "output_message_id",
    "output_digest",
    "message_created",
    "retrieval_anchor_id",
];

/// Tables whose V4 shape is asserted by exact column list.
const EXACT_SHAPE_TABLES: &[(&str, &[&str])] = &[
    (
        "observation_projection_provenance",
        CURRENT_PROVENANCE_COLUMNS,
    ),
    ("observation_projection_rebuilds", CURRENT_REBUILD_COLUMNS),
    (
        "observation_projection_rebuild_messages",
        CURRENT_REBUILD_MESSAGE_COLUMNS,
    ),
];

/// Tables whose V4 shape is asserted by the presence of the anchor binding
/// column, which is the only column a pre-V4 store is missing.
const ANCHOR_BOUND_TABLES: &[&str] = &[
    "observation_workflow_facts",
    "observation_projection_rebuild_provenance",
    "observation_projection_rebuild_workflow_facts",
];

/// Verifies the store already carries the final observation-projection shape.
///
/// `CREATE TABLE IF NOT EXISTS` above installs that shape on a fresh store, so
/// this probe is a handful of `pragma_table_xinfo` reads that pass trivially
/// for anything this binary created. A store at any *other* shape is refused:
/// this binary no longer steps an older projection schema forward, and silently
/// writing V4 rows into a pre-V4 table would corrupt authority.
async fn verify_final_projection_shape(conn: &impl QueryExecutor) -> Result<(), Error> {
    for &(table, expected) in EXACT_SHAPE_TABLES {
        let actual = projection_rebuild_column_names(conn, table).await?;
        if !columns_match(&actual, expected) {
            return Err(unsupported_projection_schema(table));
        }
    }
    for &table in ANCHOR_BOUND_TABLES {
        if !projection_table_column_exists(conn, table, "retrieval_anchor_id").await? {
            return Err(unsupported_projection_schema(table));
        }
    }
    Ok(())
}

fn unsupported_projection_schema(table: &str) -> Error {
    Error::invalid_operation(format!(
        "observation projection table `{table}` is not at the schema this binary supports; \
         this store was created by an incompatible binary and cannot be upgraded in place. \
         Remove the store directory and let this binary create a fresh one."
    ))
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use crate::ensure_registered_schema;
    use tracedecay_runtime_core::db::engine::TestConnection;

    async fn open_registered_schema(
        path: &std::path::Path,
    ) -> tracedecay_runtime_core::errors::Result<TestConnection> {
        let conn = TestConnection::open(path);
        ensure_registered_schema(&conn).await?;
        Ok(conn)
    }

    /// A store at a pre-final projection shape is refused with the
    /// fresh-start remedy. This binary creates stores at the final shape and
    /// no longer steps an older one forward.
    #[tokio::test]
    async fn legacy_projection_shape_is_refused_with_a_fresh_start_remedy() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("global.db");
        let conn = TestConnection::open(&path);
        conn.execute_batch(
            "CREATE TABLE observation_projection_rebuilds (
                projector_version TEXT PRIMARY KEY,
                generation TEXT NOT NULL,
                frontier_sequence INTEGER NOT NULL CHECK(frontier_sequence >= 0),
                staged_through INTEGER NOT NULL DEFAULT 0 CHECK(staged_through >= 0),
                projected_rows INTEGER NOT NULL DEFAULT 0 CHECK(projected_rows >= 0),
                skipped_observations INTEGER NOT NULL DEFAULT 0 CHECK(skipped_observations >= 0),
                state TEXT NOT NULL CHECK(state IN ('building', 'ready'))
             );",
        )
        .await
        .unwrap();
        drop(conn);

        let Err(error) = open_registered_schema(&path).await else {
            panic!("a pre-final projection shape must be refused, not migrated");
        };
        let message = error.to_string();
        assert!(
            message.contains("observation_projection_rebuilds"),
            "refusal must name the offending table: {message}"
        );
        assert!(
            message.contains("create a fresh one"),
            "refusal must name the fresh-start remedy, not a migrate command: {message}"
        );
    }

    /// A store this binary created passes the shape probe on every reopen.
    #[tokio::test]
    async fn a_freshly_created_store_reopens_at_the_final_shape() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("global.db");
        drop(open_registered_schema(&path).await.unwrap());
        drop(open_registered_schema(&path).await.unwrap());
    }

    #[tokio::test]
    async fn performance_indexes_install_outside_schema_transaction() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("global.db");
        let conn = open_registered_schema(&path).await.unwrap();
        conn.execute_batch(
            "DROP INDEX idx_observation_projection_provenance_output;
             DROP INDEX idx_observation_projection_provenance_global_output;
             DROP INDEX idx_observation_workflow_facts_query;
             DROP INDEX idx_observation_workflow_facts_item;
             DROP INDEX idx_projection_rebuild_provenance_output;
             DROP INDEX idx_projection_rebuild_workflow_goal;
             DROP INDEX idx_observations_identity_receipt;
             DROP INDEX idx_projection_dispositions_observation_receipt;",
        )
        .await
        .unwrap();

        let transaction = conn
            .transaction_with_behavior(
                tracedecay_runtime_core::db::engine::TransactionBehavior::Immediate,
            )
            .await
            .unwrap();
        super::ensure_observation_projection_schema(&transaction)
            .await
            .unwrap();
        let mut rows = transaction
            .query(
                "SELECT name FROM sqlite_schema
                 WHERE name IN (
                    'idx_observation_projection_provenance_output',
                    'idx_observation_projection_provenance_global_output',
                    'idx_observation_workflow_facts_query',
                    'idx_observation_workflow_facts_item',
                    'idx_projection_rebuild_provenance_output',
                    'idx_projection_rebuild_workflow_goal',
                    'idx_observations_identity_receipt',
                    'idx_projection_dispositions_observation_receipt'
                 )",
                (),
            )
            .await
            .unwrap();
        assert!(
            rows.next().await.unwrap().is_none(),
            "large performance indexes must not be built inside the lease-bounded schema transaction"
        );
    }
}
