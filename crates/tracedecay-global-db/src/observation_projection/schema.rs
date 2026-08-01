use tracedecay_store::SESSION_MESSAGE_PROJECTOR_VERSION_V4;

use crate::db::engine::{Connection, Error, Executor, QueryExecutor, params};

const PROJECTION_ANCHOR_BACKFILL_PAGE_ROWS: i64 = 256;
const LEGACY_PROJECTION_PROVENANCE_TABLE_SQL: &str =
    "CREATE TABLE observation_projection_provenance (
        projector_version TEXT NOT NULL,
        observation_id TEXT NOT NULL,
        receipt_id TEXT NOT NULL,
        output_provider TEXT NOT NULL,
        output_message_id TEXT NOT NULL,
        output_digest TEXT NOT NULL,
        message_created INTEGER NOT NULL CHECK(message_created IN (0, 1)),
        PRIMARY KEY(projector_version, observation_id),
        UNIQUE(projector_version, output_provider, output_message_id),
        FOREIGN KEY(observation_id) REFERENCES observations(observation_id),
        FOREIGN KEY(receipt_id) REFERENCES sanitization_receipts(receipt_id)
    )";
const SUPPORTED_LEGACY_PROJECTION_TRIGGERS: &[(&str, &str)] = &[
    (
        "projection_provenance_receipt_insert_v1",
        "CREATE TRIGGER projection_provenance_receipt_insert_v1
         BEFORE INSERT ON observation_projection_provenance WHEN NOT EXISTS (
            SELECT 1 FROM observations
            WHERE observation_id = NEW.observation_id AND receipt_id = NEW.receipt_id
         ) BEGIN SELECT RAISE(ABORT, 'projection provenance receipt mismatch'); END",
    ),
    (
        "projection_provenance_receipt_update_v1",
        "CREATE TRIGGER projection_provenance_receipt_update_v1
         BEFORE UPDATE OF observation_id, receipt_id
         ON observation_projection_provenance WHEN NOT EXISTS (
            SELECT 1 FROM observations
            WHERE observation_id = NEW.observation_id AND receipt_id = NEW.receipt_id
         ) BEGIN SELECT RAISE(ABORT, 'projection provenance receipt mismatch'); END",
    ),
    (
        "projection_provenance_message_created_insert_v1",
        "CREATE TRIGGER projection_provenance_message_created_insert_v1
         BEFORE INSERT ON observation_projection_provenance
         WHEN NEW.message_created NOT IN (0, 1)
         BEGIN SELECT RAISE(ABORT, 'invalid projection message_created'); END",
    ),
    (
        "projection_provenance_message_created_update_v1",
        "CREATE TRIGGER projection_provenance_message_created_update_v1
         BEFORE UPDATE OF message_created ON observation_projection_provenance
         WHEN NEW.message_created NOT IN (0, 1)
         BEGIN SELECT RAISE(ABORT, 'invalid projection message_created'); END",
    ),
    (
        "projection_provenance_audit_invalidate_update_v1",
        "CREATE TRIGGER projection_provenance_audit_invalidate_update_v1
         AFTER UPDATE ON observation_projection_provenance BEGIN
            DELETE FROM authority_audit_checkpoints
            WHERE audit_name = 'observation-authority';
         END",
    ),
    (
        "projection_provenance_audit_invalidate_delete_v1",
        "CREATE TRIGGER projection_provenance_audit_invalidate_delete_v1
         AFTER DELETE ON observation_projection_provenance BEGIN
            DELETE FROM authority_audit_checkpoints
            WHERE audit_name = 'observation-authority';
         END",
    ),
];

fn normalize_schema_sql(sql: &str) -> String {
    sql.chars()
        .filter(|character| !character.is_whitespace() && *character != '"' && *character != '`')
        .flat_map(char::to_lowercase)
        .collect()
}

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
        CREATE TABLE IF NOT EXISTS observation_projection_migrations (
            source_projector_version TEXT NOT NULL,
            target_projector_version TEXT NOT NULL,
            source_frontier INTEGER NOT NULL CHECK(source_frontier >= 0),
            migrated_through INTEGER NOT NULL CHECK(
                migrated_through >= 0 AND migrated_through <= source_frontier
            ),
            completed INTEGER NOT NULL CHECK(completed IN (0, 1)),
            PRIMARY KEY(source_projector_version, target_projector_version),
            CHECK(completed = 0 OR migrated_through = source_frontier)
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
    migrate_projection_rebuild_schema(conn).await?;
    migrate_legacy_projection_output_uniqueness(conn).await?;
    migrate_projection_multi_output_primary_key(conn).await?;
    ensure_projection_anchor_columns(conn).await?;
    ensure_v4_projection_binding_triggers(conn).await
}

pub(in super::super) async fn ensure_observation_projection_performance_indexes(
    conn: &Connection,
) -> Result<(), Error> {
    // Install each historical-data index as its own durable schema step. These
    // cannot share the lease-bounded all-schema transaction: an interrupted
    // later build would otherwise roll back every earlier completed build. The
    // explicit schema-step API keeps shutdown cancellation while allowing one
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
        "CREATE INDEX IF NOT EXISTS idx_observation_projection_provenance_pending_anchor
         ON observation_projection_provenance (projector_version, observation_id)
            WHERE retrieval_anchor_id IS NULL;",
        "CREATE INDEX IF NOT EXISTS idx_observation_workflow_facts_pending_anchor
         ON observation_workflow_facts (projector_version, observation_id)
            WHERE retrieval_anchor_id IS NULL;",
        "CREATE INDEX IF NOT EXISTS idx_observations_identity_receipt
         ON observations (observation_id, receipt_id);",
        "CREATE INDEX IF NOT EXISTS idx_projection_dispositions_observation_receipt
         ON observation_projection_dispositions (observation_id, receipt_id);",
    ] {
        let transaction = conn.schema_migration_transaction().await?;
        transaction.execute_schema_batch_step(sql).await?;
        transaction.commit().await?;
    }
    Ok(())
}

/// Binds the canonical retrieval anchor onto v4 projection rows that predate the
/// `retrieval_anchor_id` column. A store upgraded from an earlier v4 schema kept
/// its projected provenance and workflow rows but gained a NULL anchor when
/// [`ensure_projection_anchor_columns`] added the column; the v4 authority
/// invariants require every v4 output to resolve to an owner-bound anchor, so
/// fill each NULL anchor from the observation's already-backfilled binding. The
/// v4 update triggers validate that the anchor resolves to the same observation
/// and receipt, so a mismatch fails closed rather than persisting a bad binding.
///
/// This must re-run on every open rather than be skipped forever after a
/// first success: [`ensure_projection_anchor_columns`] can reintroduce NULL
/// anchors (e.g. after a legacy schema reinstall re-adds the column), and a
/// permanent completion marker would then leave those rows unbound. Instead,
/// the `idx_observation_projection_provenance_pending_anchor` and
/// `idx_observation_workflow_facts_pending_anchor` partial indexes created
/// above — covering only rows with `retrieval_anchor_id IS NULL` — keep the
/// `WHERE retrieval_anchor_id IS NULL` scan below cheap in the steady state
/// where that partial index is empty, instead of running the two `UPDATE`s as
/// unindexed full-table scans.
pub(in super::super) async fn converge_v4_projection_anchor_bindings(
    conn: &impl Executor,
) -> Result<(), Error> {
    for table in [
        "observation_projection_provenance",
        "observation_workflow_facts",
    ] {
        loop {
            let updated = conn
                .execute(
                    &format!(
                        "UPDATE {table}
                         SET retrieval_anchor_id = (
                             SELECT anchor.anchor_id
                             FROM observation_retrieval_anchors AS anchor
                             WHERE anchor.observation_id = {table}.observation_id
                         )
                         WHERE rowid IN (
                             SELECT candidate.rowid
                             FROM {table} AS candidate
                             WHERE candidate.projector_version = ?1
                               AND candidate.retrieval_anchor_id IS NULL
                               AND EXISTS (
                                   SELECT 1
                                   FROM observation_retrieval_anchors AS anchor
                                   WHERE anchor.observation_id = candidate.observation_id
                               )
                             LIMIT ?2
                         )"
                    ),
                    params![
                        SESSION_MESSAGE_PROJECTOR_VERSION_V4,
                        PROJECTION_ANCHOR_BACKFILL_PAGE_ROWS
                    ],
                )
                .await?;
            if updated == 0 {
                break;
            }
        }
    }
    Ok(())
}

async fn ensure_projection_anchor_columns(conn: &impl Executor) -> Result<(), Error> {
    for table in [
        "observation_projection_provenance",
        "observation_workflow_facts",
        "observation_projection_rebuild_provenance",
        "observation_projection_rebuild_workflow_facts",
    ] {
        let ddl = format!(
            "ALTER TABLE {table} ADD COLUMN retrieval_anchor_id TEXT \
             REFERENCES retrieval_anchors(anchor_id)"
        );
        ensure_projection_table_columns(conn, table, &[("retrieval_anchor_id", ddl.as_str())])
            .await?;
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

async fn add_projection_table_column_after_missing_check(
    conn: &impl Executor,
    table: &str,
    column: &str,
    ddl: &str,
) -> Result<bool, Error> {
    match conn.execute(ddl, ()).await {
        Ok(_) => Ok(true),
        Err(error) => {
            if projection_table_column_exists(conn, table, column).await? {
                Ok(false)
            } else {
                Err(error)
            }
        }
    }
}

async fn ensure_projection_table_columns(
    conn: &impl Executor,
    table: &str,
    columns: &[(&str, &str)],
) -> Result<(), Error> {
    for &(column, ddl) in columns {
        if !projection_table_column_exists(conn, table, column).await? {
            add_projection_table_column_after_missing_check(conn, table, column, ddl).await?;
        }
    }
    Ok(())
}

async fn ensure_v4_projection_binding_triggers(conn: &impl Executor) -> Result<(), Error> {
    conn.execute_batch(include_str!("projection_v4_binding_triggers.sql"))
        .await
}

const LEGACY_REBUILD_COLUMNS: &[&str] = &[
    "projector_version",
    "generation",
    "frontier_sequence",
    "staged_through",
    "projected_rows",
    "skipped_observations",
    "state",
];
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
const LEGACY_REBUILD_MESSAGE_COLUMNS: &[&str] = &[
    "projector_version",
    "generation",
    "output_provider",
    "output_message_id",
    "message_json",
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

async fn migrate_projection_rebuild_schema(conn: &impl Executor) -> Result<(), Error> {
    let rebuild_columns =
        projection_rebuild_column_names(conn, "observation_projection_rebuilds").await?;
    let message_columns =
        projection_rebuild_column_names(conn, "observation_projection_rebuild_messages").await?;
    let rebuild_is_legacy = columns_match(&rebuild_columns, LEGACY_REBUILD_COLUMNS);
    let rebuild_columns_are_current = columns_match(&rebuild_columns, CURRENT_REBUILD_COLUMNS);
    let messages_are_legacy = columns_match(&message_columns, LEGACY_REBUILD_MESSAGE_COLUMNS);
    let messages_are_current = columns_match(&message_columns, CURRENT_REBUILD_MESSAGE_COLUMNS);
    if (!rebuild_is_legacy && !rebuild_columns_are_current)
        || (!messages_are_legacy && !messages_are_current)
    {
        return Err(unsupported_projection_rebuild_schema());
    }
    let rebuild_supports_aliasing = projection_rebuild_supports_aliasing(conn).await?;
    let rebuild_requires_replacement = rebuild_is_legacy || !rebuild_supports_aliasing;
    if !rebuild_requires_replacement && messages_are_current {
        return Ok(());
    }

    if messages_are_legacy {
        // The legacy message staging rows lack the hashes and rendered text
        // needed for safe activation. Rebuild staging is derived from durable
        // observations, so discard only this restartable job state.
        conn.execute_batch(
            "DELETE FROM observation_projection_rebuild_aliases;
             DELETE FROM observation_projection_rebuild_sessions;
             DELETE FROM observation_projection_rebuild_messages;
             DELETE FROM observation_projection_rebuild_provenance;
             DELETE FROM observation_projection_rebuild_dispositions;
             DELETE FROM observation_projection_rebuild_workflow_facts;
             DELETE FROM observation_projection_rebuilds;
             DROP TABLE observation_projection_rebuild_messages;",
        )
        .await?;
        if rebuild_requires_replacement {
            replace_empty_projection_rebuild_root(conn).await?;
        }
        create_current_projection_rebuild_messages(conn).await?;
        return Ok(());
    }

    let aliases_staged_through = if rebuild_is_legacy {
        "0"
    } else {
        "aliases_staged_through"
    };
    conn.execute_batch(&format!(
        "DROP TABLE IF EXISTS temp.projection_rebuilds_upgrade;
         DROP TABLE IF EXISTS temp.projection_rebuild_aliases_upgrade;
         DROP TABLE IF EXISTS temp.projection_rebuild_sessions_upgrade;
         DROP TABLE IF EXISTS temp.projection_rebuild_messages_upgrade;
         DROP TABLE IF EXISTS temp.projection_rebuild_provenance_upgrade;
         DROP TABLE IF EXISTS temp.projection_rebuild_dispositions_upgrade;
         DROP TABLE IF EXISTS temp.projection_rebuild_workflow_facts_upgrade;
         CREATE TEMP TABLE projection_rebuilds_upgrade AS
         SELECT projector_version, generation, frontier_sequence,
                {aliases_staged_through} AS aliases_staged_through,
                staged_through, projected_rows, skipped_observations, state
         FROM observation_projection_rebuilds;
         CREATE TEMP TABLE projection_rebuild_aliases_upgrade AS
         SELECT * FROM observation_projection_rebuild_aliases;
         CREATE TEMP TABLE projection_rebuild_sessions_upgrade AS
         SELECT * FROM observation_projection_rebuild_sessions;
         CREATE TEMP TABLE projection_rebuild_messages_upgrade AS
         SELECT * FROM observation_projection_rebuild_messages;
         CREATE TEMP TABLE projection_rebuild_provenance_upgrade AS
         SELECT * FROM observation_projection_rebuild_provenance;
         CREATE TEMP TABLE projection_rebuild_dispositions_upgrade AS
         SELECT * FROM observation_projection_rebuild_dispositions;
         CREATE TEMP TABLE projection_rebuild_workflow_facts_upgrade AS
         SELECT * FROM observation_projection_rebuild_workflow_facts;
         DELETE FROM observation_projection_rebuild_aliases;
         DELETE FROM observation_projection_rebuild_sessions;
         DELETE FROM observation_projection_rebuild_messages;
         DELETE FROM observation_projection_rebuild_provenance;
         DELETE FROM observation_projection_rebuild_dispositions;
         DELETE FROM observation_projection_rebuild_workflow_facts;
         DELETE FROM observation_projection_rebuilds;"
    ))
    .await?;
    replace_empty_projection_rebuild_root(conn).await?;
    conn.execute_batch(
        "INSERT INTO observation_projection_rebuilds
         SELECT * FROM projection_rebuilds_upgrade;
         INSERT INTO observation_projection_rebuild_aliases
         SELECT * FROM projection_rebuild_aliases_upgrade;
         INSERT INTO observation_projection_rebuild_sessions
         SELECT * FROM projection_rebuild_sessions_upgrade;
         INSERT INTO observation_projection_rebuild_messages
         SELECT * FROM projection_rebuild_messages_upgrade;
         INSERT INTO observation_projection_rebuild_provenance
         SELECT * FROM projection_rebuild_provenance_upgrade;
         INSERT INTO observation_projection_rebuild_dispositions
         SELECT * FROM projection_rebuild_dispositions_upgrade;
         INSERT INTO observation_projection_rebuild_workflow_facts
         SELECT * FROM projection_rebuild_workflow_facts_upgrade;
         DROP TABLE projection_rebuilds_upgrade;
         DROP TABLE projection_rebuild_aliases_upgrade;
         DROP TABLE projection_rebuild_sessions_upgrade;
         DROP TABLE projection_rebuild_messages_upgrade;
         DROP TABLE projection_rebuild_provenance_upgrade;
         DROP TABLE projection_rebuild_dispositions_upgrade;
         DROP TABLE projection_rebuild_workflow_facts_upgrade;",
    )
    .await?;
    Ok(())
}

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

async fn projection_rebuild_supports_aliasing(conn: &impl QueryExecutor) -> Result<bool, Error> {
    let mut rows = conn
        .query(
            "SELECT sql FROM sqlite_schema
             WHERE type = 'table' AND name = 'observation_projection_rebuilds'",
            (),
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Err(unsupported_projection_rebuild_schema());
    };
    Ok(row.get::<String>(0)?.contains("'aliasing'"))
}

async fn replace_empty_projection_rebuild_root(conn: &impl Executor) -> Result<(), Error> {
    conn.execute_batch(
        "DROP TABLE observation_projection_rebuilds;
         CREATE TABLE observation_projection_rebuilds (
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
         );",
    )
    .await?;
    Ok(())
}

async fn create_current_projection_rebuild_messages(conn: &impl Executor) -> Result<(), Error> {
    conn.execute_batch(
        "CREATE TABLE observation_projection_rebuild_messages (
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
         );",
    )
    .await?;
    Ok(())
}

fn unsupported_projection_rebuild_schema() -> Error {
    Error::invalid_operation("unsupported observation projection rebuild schema")
}

async fn has_legacy_projection_output_uniqueness(conn: &impl QueryExecutor) -> Result<bool, Error> {
    let mut rows = conn
        .query("PRAGMA index_list(observation_projection_provenance)", ())
        .await?;
    let mut unique_indexes = Vec::new();
    while let Some(row) = rows.next().await? {
        if row.get::<i64>(2)? != 0 {
            unique_indexes.push(row.get::<String>(1)?);
        }
    }
    drop(rows);

    for index_name in unique_indexes {
        let mut columns = conn
            .query(
                "SELECT name FROM pragma_index_info(?1) ORDER BY seqno",
                params![index_name],
            )
            .await?;
        let mut names = Vec::new();
        while let Some(row) = columns.next().await? {
            names.push(row.get::<String>(0)?);
        }
        if names == ["projector_version", "output_provider", "output_message_id"] {
            return Ok(true);
        }
    }
    Ok(false)
}

async fn validate_legacy_projection_provenance_schema(
    conn: &impl QueryExecutor,
) -> Result<Vec<String>, Error> {
    require_projection_provenance_table(conn).await?;
    let columns_match = legacy_projection_columns_match(conn).await?;
    let foreign_keys_match = legacy_projection_foreign_keys_match(conn).await?;
    let indexes_match = legacy_projection_indexes_match(conn).await?;
    let table_sql_matches = legacy_projection_table_sql_matches(conn).await?;
    let triggers = read_supported_legacy_projection_triggers(conn).await?;

    if columns_match && foreign_keys_match && indexes_match && table_sql_matches {
        Ok(triggers)
    } else {
        Err(unsupported_legacy_projection_schema())
    }
}

async fn require_projection_provenance_table(conn: &impl QueryExecutor) -> Result<(), Error> {
    let mut objects = conn
        .query(
            "SELECT type FROM sqlite_schema
             WHERE name = 'observation_projection_provenance'",
            (),
        )
        .await?;
    let object_type = objects
        .next()
        .await?
        .ok_or_else(unsupported_legacy_projection_schema)?
        .get::<String>(0)?;
    drop(objects);
    if object_type != "table" {
        return Err(Error::invalid_operation(format!(
            "unsupported observation_projection_provenance {object_type}"
        )));
    }
    Ok(())
}

fn unsupported_legacy_projection_schema() -> Error {
    Error::invalid_operation("unsupported observation_projection_provenance legacy schema")
}

async fn legacy_projection_columns_match(conn: &impl QueryExecutor) -> Result<bool, Error> {
    const EXPECTED: &[(&str, &str, i64)] = &[
        ("projector_version", "TEXT", 1),
        ("observation_id", "TEXT", 2),
        ("receipt_id", "TEXT", 0),
        ("output_provider", "TEXT", 0),
        ("output_message_id", "TEXT", 0),
        ("output_digest", "TEXT", 0),
        ("message_created", "INTEGER", 0),
    ];

    let mut rows = conn
        .query(
            "SELECT cid, name, type, \"notnull\", dflt_value, pk, hidden
             FROM pragma_table_xinfo('observation_projection_provenance') ORDER BY cid",
            (),
        )
        .await?;
    for (cid, &(expected_name, expected_type, expected_pk)) in EXPECTED.iter().enumerate() {
        let Some(row) = rows.next().await? else {
            return Ok(false);
        };
        let Ok(expected_cid) = i64::try_from(cid) else {
            return Ok(false);
        };
        if row.get::<i64>(0)? != expected_cid
            || row.get::<String>(1)? != expected_name
            || row.get::<String>(2)?.to_ascii_uppercase() != expected_type
            || row.get::<i64>(3)? != 1
            || row.get::<Option<String>>(4)?.is_some()
            || row.get::<i64>(5)? != expected_pk
            || row.get::<i64>(6)? != 0
        {
            return Ok(false);
        }
    }
    Ok(rows.next().await?.is_none())
}

async fn legacy_projection_foreign_keys_match(conn: &impl QueryExecutor) -> Result<bool, Error> {
    const EXPECTED: &[(&str, &str, &str)] = &[
        ("observation_id", "observations", "observation_id"),
        ("receipt_id", "sanitization_receipts", "receipt_id"),
    ];
    let mut rows = conn
        .query(
            "SELECT \"from\", \"table\", \"to\", on_update, on_delete, \"match\"
             FROM pragma_foreign_key_list('observation_projection_provenance')
             ORDER BY \"from\", \"table\", \"to\", on_update, on_delete, \"match\"",
            (),
        )
        .await?;
    for &(expected_from, expected_table, expected_to) in EXPECTED {
        let Some(row) = rows.next().await? else {
            return Ok(false);
        };
        if row.get::<String>(0)? != expected_from
            || row.get::<String>(1)? != expected_table
            || row.get::<String>(2)? != expected_to
            || row.get::<String>(3)? != "NO ACTION"
            || row.get::<String>(4)? != "NO ACTION"
            || row.get::<String>(5)? != "NONE"
        {
            return Ok(false);
        }
    }
    Ok(rows.next().await?.is_none())
}

async fn legacy_projection_indexes_match(conn: &impl QueryExecutor) -> Result<bool, Error> {
    const EXPECTED: &[(&str, &[&str])] = &[
        ("pk", &["projector_version", "observation_id"]),
        (
            "u",
            &["projector_version", "output_provider", "output_message_id"],
        ),
    ];
    let mut rows = conn
        .query(
            "SELECT name, \"unique\", origin, partial
             FROM pragma_index_list('observation_projection_provenance')
             ORDER BY origin",
            (),
        )
        .await?;
    let mut index_headers = Vec::new();
    while let Some(row) = rows.next().await? {
        index_headers.push((
            row.get::<String>(0)?,
            row.get::<i64>(1)?,
            row.get::<String>(2)?,
            row.get::<i64>(3)?,
        ));
    }
    if index_headers.len() != EXPECTED.len() {
        return Ok(false);
    }
    for ((name, unique, origin, partial), &(expected_origin, expected_columns)) in
        index_headers.into_iter().zip(EXPECTED)
    {
        if unique != 1
            || origin != expected_origin
            || partial != 0
            || !legacy_projection_index_columns_match(conn, &name, expected_columns).await?
        {
            return Ok(false);
        }
    }
    Ok(true)
}

async fn legacy_projection_index_columns_match(
    conn: &impl QueryExecutor,
    index_name: &str,
    expected_columns: &[&str],
) -> Result<bool, Error> {
    let mut rows = conn
        .query(
            "SELECT name, desc, coll FROM pragma_index_xinfo(?1)
             WHERE key = 1 ORDER BY seqno",
            params![index_name],
        )
        .await?;
    for &expected_name in expected_columns {
        let Some(row) = rows.next().await? else {
            return Ok(false);
        };
        if row.get::<String>(0)? != expected_name
            || row.get::<i64>(1)? != 0
            || row.get::<String>(2)? != "BINARY"
        {
            return Ok(false);
        }
    }
    Ok(rows.next().await?.is_none())
}

async fn legacy_projection_table_sql_matches(conn: &impl QueryExecutor) -> Result<bool, Error> {
    let mut sql_rows = conn
        .query(
            "SELECT sql FROM sqlite_schema
             WHERE type = 'table' AND name = 'observation_projection_provenance'",
            (),
        )
        .await?;
    let sql = sql_rows
        .next()
        .await?
        .ok_or_else(|| Error::invalid_operation("legacy projection table is missing"))?
        .get::<String>(0)?;
    Ok(normalize_schema_sql(&sql) == normalize_schema_sql(LEGACY_PROJECTION_PROVENANCE_TABLE_SQL))
}

async fn read_supported_legacy_projection_triggers(
    conn: &impl QueryExecutor,
) -> Result<Vec<String>, Error> {
    let mut trigger_rows = conn
        .query(
            "SELECT name, sql FROM sqlite_schema
             WHERE type = 'trigger' AND tbl_name = 'observation_projection_provenance'
             ORDER BY name",
            (),
        )
        .await?;
    let mut triggers = Vec::new();
    while let Some(row) = trigger_rows.next().await? {
        let name = row.get::<String>(0)?;
        let sql = row.get::<String>(1)?;
        let Some((_, expected_sql)) = SUPPORTED_LEGACY_PROJECTION_TRIGGERS
            .iter()
            .find(|(expected_name, _)| *expected_name == name)
        else {
            return Err(Error::invalid_operation(format!(
                "unsupported observation_projection_provenance trigger {name}"
            )));
        };
        if normalize_schema_sql(&sql) != normalize_schema_sql(expected_sql) {
            return Err(Error::invalid_operation(format!(
                "unsupported definition for observation_projection_provenance trigger {name}"
            )));
        }
        triggers.push(sql);
    }
    Ok(triggers)
}

async fn migrate_legacy_projection_output_uniqueness(conn: &impl Executor) -> Result<(), Error> {
    if !has_legacy_projection_output_uniqueness(conn).await? {
        return Ok(());
    }
    let triggers = validate_legacy_projection_provenance_schema(conn).await?;

    conn.execute_batch(
        "DROP TRIGGER IF EXISTS projection_provenance_receipt_insert_v1;
             DROP TRIGGER IF EXISTS projection_provenance_receipt_update_v1;
             DROP TRIGGER IF EXISTS projection_provenance_message_created_insert_v1;
             DROP TRIGGER IF EXISTS projection_provenance_message_created_update_v1;
             DROP TRIGGER IF EXISTS projection_provenance_audit_invalidate_update_v1;
             DROP TRIGGER IF EXISTS projection_provenance_audit_invalidate_delete_v1;
             DROP TRIGGER IF EXISTS projection_output_audit_invalidate_update_v1;
             DROP TRIGGER IF EXISTS projection_output_audit_invalidate_delete_v1;
             DROP TABLE IF EXISTS observation_projection_provenance_without_output_unique;
             CREATE TABLE observation_projection_provenance_without_output_unique (
                projector_version TEXT NOT NULL,
                observation_id TEXT NOT NULL,
                output_ordinal INTEGER NOT NULL DEFAULT 0 CHECK(output_ordinal >= 0),
                receipt_id TEXT NOT NULL,
                output_provider TEXT NOT NULL,
                output_message_id TEXT NOT NULL,
                output_digest TEXT NOT NULL,
                message_created INTEGER NOT NULL CHECK(message_created IN (0, 1)),
                PRIMARY KEY(projector_version, observation_id, output_ordinal),
                FOREIGN KEY(observation_id) REFERENCES observations(observation_id),
                FOREIGN KEY(receipt_id) REFERENCES sanitization_receipts(receipt_id)
             );
             INSERT INTO observation_projection_provenance_without_output_unique
                (projector_version, observation_id, output_ordinal, receipt_id, output_provider,
                 output_message_id, output_digest, message_created)
             SELECT projector_version, observation_id, 0, receipt_id, output_provider,
                    output_message_id, output_digest, message_created
             FROM observation_projection_provenance;
             DROP TABLE observation_projection_provenance;
             ALTER TABLE observation_projection_provenance_without_output_unique
                RENAME TO observation_projection_provenance;",
    )
    .await?;
    for trigger in triggers {
        conn.execute_batch(&trigger).await?;
    }
    restore_projection_output_audit_triggers(conn).await?;
    Ok(())
}

async fn migrate_projection_multi_output_primary_key(conn: &impl Executor) -> Result<(), Error> {
    require_projection_provenance_table(conn).await?;
    if has_output_ordinal(conn).await? {
        return Ok(());
    }
    if !legacy_projection_columns_match(conn).await?
        || !legacy_projection_foreign_keys_match(conn).await?
    {
        return Err(unsupported_legacy_projection_schema());
    }
    let triggers = read_supported_legacy_projection_triggers(conn).await?;
    conn.execute_batch(
        "DROP TRIGGER IF EXISTS projection_provenance_receipt_insert_v1;
         DROP TRIGGER IF EXISTS projection_provenance_receipt_update_v1;
         DROP TRIGGER IF EXISTS projection_provenance_message_created_insert_v1;
         DROP TRIGGER IF EXISTS projection_provenance_message_created_update_v1;
         DROP TRIGGER IF EXISTS projection_provenance_audit_invalidate_update_v1;
         DROP TRIGGER IF EXISTS projection_provenance_audit_invalidate_delete_v1;
         DROP TRIGGER IF EXISTS projection_output_audit_invalidate_update_v1;
         DROP TRIGGER IF EXISTS projection_output_audit_invalidate_delete_v1;
         DROP TABLE IF EXISTS observation_projection_provenance_multi_output;
         CREATE TABLE observation_projection_provenance_multi_output (
            projector_version TEXT NOT NULL,
            observation_id TEXT NOT NULL,
            output_ordinal INTEGER NOT NULL DEFAULT 0 CHECK(output_ordinal >= 0),
            receipt_id TEXT NOT NULL,
            output_provider TEXT NOT NULL,
            output_message_id TEXT NOT NULL,
            output_digest TEXT NOT NULL,
            message_created INTEGER NOT NULL CHECK(message_created IN (0, 1)),
            PRIMARY KEY(projector_version, observation_id, output_ordinal),
            FOREIGN KEY(observation_id) REFERENCES observations(observation_id),
            FOREIGN KEY(receipt_id) REFERENCES sanitization_receipts(receipt_id)
         );
         INSERT INTO observation_projection_provenance_multi_output
            (projector_version, observation_id, output_ordinal, receipt_id, output_provider,
             output_message_id, output_digest, message_created)
         SELECT projector_version, observation_id, 0, receipt_id, output_provider,
                output_message_id, output_digest, message_created
         FROM observation_projection_provenance;
         DROP TABLE observation_projection_provenance;
         ALTER TABLE observation_projection_provenance_multi_output
            RENAME TO observation_projection_provenance;",
    )
    .await?;
    for trigger in triggers {
        conn.execute_batch(&trigger).await?;
    }
    restore_projection_output_audit_triggers(conn).await?;
    Ok(())
}

async fn restore_projection_output_audit_triggers(conn: &impl Executor) -> Result<(), Error> {
    conn.execute_batch(
        "CREATE TRIGGER IF NOT EXISTS projection_output_audit_invalidate_update_v1
         AFTER UPDATE ON session_messages
         WHEN EXISTS (
            SELECT 1 FROM observation_projection_provenance
            WHERE projector_version = 'claude-session-message-v4'
              AND output_provider = OLD.provider
              AND output_message_id = OLD.message_id
         ) BEGIN
            DELETE FROM authority_audit_checkpoints
            WHERE audit_name = 'observation-authority';
         END;
         CREATE TRIGGER IF NOT EXISTS projection_output_audit_invalidate_delete_v1
         AFTER DELETE ON session_messages
         WHEN EXISTS (
            SELECT 1 FROM observation_projection_provenance
            WHERE projector_version = 'claude-session-message-v4'
              AND output_provider = OLD.provider
              AND output_message_id = OLD.message_id
         ) BEGIN
            DELETE FROM authority_audit_checkpoints
            WHERE audit_name = 'observation-authority';
         END;",
    )
    .await?;
    Ok(())
}

async fn has_output_ordinal(conn: &impl QueryExecutor) -> Result<bool, Error> {
    let mut rows = conn
        .query(
            "SELECT 1 FROM pragma_table_xinfo('observation_projection_provenance')
             WHERE name = 'output_ordinal'",
            (),
        )
        .await?;
    Ok(rows.next().await?.is_some())
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use crate::db::engine::TestConnection;
    use crate::ensure_registered_schema;

    async fn open_registered_schema(
        path: &std::path::Path,
    ) -> crate::errors::Result<TestConnection> {
        let conn = TestConnection::open(path);
        ensure_registered_schema(&conn).await?;
        Ok(conn)
    }

    async fn seed_legacy_rebuild_schema(path: &std::path::Path, legacy_messages: bool) {
        let conn = TestConnection::open(path);
        conn.execute_batch(
            "PRAGMA foreign_keys = ON;
             CREATE TABLE observation_projection_rebuilds (
                projector_version TEXT PRIMARY KEY,
                generation TEXT NOT NULL,
                frontier_sequence INTEGER NOT NULL CHECK(frontier_sequence >= 0),
                staged_through INTEGER NOT NULL DEFAULT 0 CHECK(staged_through >= 0),
                projected_rows INTEGER NOT NULL DEFAULT 0 CHECK(projected_rows >= 0),
                skipped_observations INTEGER NOT NULL DEFAULT 0 CHECK(skipped_observations >= 0),
                state TEXT NOT NULL CHECK(state IN ('building', 'ready')),
                UNIQUE(projector_version, generation)
             );
             INSERT INTO observation_projection_rebuilds (
                projector_version, generation, frontier_sequence, staged_through,
                projected_rows, skipped_observations, state
             ) VALUES ('projector-v1', 'generation-v1', 7, 3, 2, 1, 'building');",
        )
        .await
        .unwrap();
        if legacy_messages {
            conn.execute_batch(
                "CREATE TABLE observation_projection_rebuild_messages (
                    projector_version TEXT NOT NULL,
                    generation TEXT NOT NULL,
                    output_provider TEXT NOT NULL,
                    output_message_id TEXT NOT NULL,
                    message_json TEXT NOT NULL CHECK(json_valid(message_json)),
                    PRIMARY KEY(
                        projector_version, generation, output_provider, output_message_id
                    ),
                    FOREIGN KEY(projector_version, generation)
                        REFERENCES observation_projection_rebuilds(projector_version, generation)
                        ON DELETE CASCADE
                 );
                 INSERT INTO observation_projection_rebuild_messages VALUES (
                    'projector-v1', 'generation-v1', 'claude', 'message-v1', '{}'
                 );",
            )
            .await
            .unwrap();
        }
    }

    #[tokio::test]
    async fn legacy_rebuild_root_preserves_a_structurally_upgradeable_job() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("global.db");
        seed_legacy_rebuild_schema(&path, false).await;

        let conn = open_registered_schema(&path).await.unwrap();
        let mut rows = conn
            .query(
                "SELECT aliases_staged_through, staged_through, state
                 FROM observation_projection_rebuilds
                 WHERE projector_version = 'projector-v1'",
                (),
            )
            .await
            .unwrap();
        let row = rows.next().await.unwrap().unwrap();
        assert_eq!(row.get::<i64>(0).unwrap(), 0);
        assert_eq!(row.get::<i64>(1).unwrap(), 3);
        assert_eq!(row.get::<String>(2).unwrap(), "building");
    }

    #[tokio::test]
    async fn legacy_unrendered_messages_restart_only_projection_rebuild_staging() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("global.db");
        seed_legacy_rebuild_schema(&path, true).await;

        let conn = open_registered_schema(&path).await.unwrap();
        for table in [
            "observation_projection_rebuilds",
            "observation_projection_rebuild_messages",
        ] {
            let count = conn
                .query(&format!("SELECT COUNT(*) FROM {table}"), ())
                .await
                .unwrap()
                .next()
                .await
                .unwrap()
                .unwrap()
                .get::<i64>(0)
                .unwrap();
            assert_eq!(count, 0);
        }
        conn.execute(
            "INSERT INTO observation_projection_rebuilds (
                    projector_version, generation, frontier_sequence, state
                 ) VALUES ('projector-v2', 'generation-v2', 0, 'aliasing')",
            (),
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn later_schema_failure_rolls_back_legacy_rebuild_upgrade() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("global.db");
        seed_legacy_rebuild_schema(&path, false).await;
        let conn = TestConnection::open(&path);
        conn.execute(
            "CREATE TABLE observation_projection_provenance (unsupported TEXT)",
            (),
        )
        .await
        .unwrap();
        drop(conn);

        assert!(open_registered_schema(&path).await.is_err());

        let conn = TestConnection::open(&path);
        let mut columns = conn
            .query(
                "SELECT name FROM pragma_table_xinfo('observation_projection_rebuilds')
                 WHERE name = 'aliases_staged_through'",
                (),
            )
            .await
            .unwrap();
        assert!(columns.next().await.unwrap().is_none());
        drop(columns);
        let mut rows = conn
            .query(
                "SELECT staged_through, state FROM observation_projection_rebuilds
                 WHERE projector_version = 'projector-v1'",
                (),
            )
            .await
            .unwrap();
        let row = rows.next().await.unwrap().unwrap();
        assert_eq!(row.get::<i64>(0).unwrap(), 3);
        assert_eq!(row.get::<String>(1).unwrap(), "building");
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
             DROP INDEX idx_observation_projection_provenance_pending_anchor;
             DROP INDEX idx_observation_workflow_facts_pending_anchor;
             DROP INDEX idx_observations_identity_receipt;
             DROP INDEX idx_projection_dispositions_observation_receipt;",
        )
        .await
        .unwrap();

        let transaction = conn
            .transaction_with_behavior(crate::db::engine::TransactionBehavior::Immediate)
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
                    'idx_observation_projection_provenance_pending_anchor',
                    'idx_observation_workflow_facts_pending_anchor',
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

    #[tokio::test]
    async fn projection_anchor_backfill_converges_in_bounded_pages() {
        let temp = TempDir::new().unwrap();
        let conn = TestConnection::open(&temp.path().join("global.db"));
        conn.execute_batch(
            "CREATE TABLE observation_retrieval_anchors (
                observation_id TEXT PRIMARY KEY,
                anchor_id TEXT NOT NULL
             );
             CREATE TABLE observation_projection_provenance (
                projector_version TEXT NOT NULL,
                observation_id TEXT NOT NULL,
                retrieval_anchor_id TEXT
             );
             CREATE TABLE observation_workflow_facts (
                projector_version TEXT NOT NULL,
                observation_id TEXT NOT NULL,
                retrieval_anchor_id TEXT
             );
             WITH RECURSIVE sequence(value) AS (
                VALUES(1) UNION ALL SELECT value + 1 FROM sequence WHERE value < 300
             )
             INSERT INTO observation_retrieval_anchors
             SELECT 'observation-' || value, 'anchor-' || value FROM sequence;
             WITH RECURSIVE sequence(value) AS (
                VALUES(1) UNION ALL SELECT value + 1 FROM sequence WHERE value < 300
             )
             INSERT INTO observation_projection_provenance
             SELECT 'claude-session-message-v4', 'observation-' || value, NULL FROM sequence;
             WITH RECURSIVE sequence(value) AS (
                VALUES(1) UNION ALL SELECT value + 1 FROM sequence WHERE value < 300
             )
             INSERT INTO observation_workflow_facts
             SELECT 'claude-session-message-v4', 'observation-' || value, NULL FROM sequence;",
        )
        .await
        .unwrap();

        super::converge_v4_projection_anchor_bindings(&conn)
            .await
            .unwrap();

        for table in [
            "observation_projection_provenance",
            "observation_workflow_facts",
        ] {
            let mut rows = conn
                .query(
                    &format!("SELECT COUNT(*) FROM {table} WHERE retrieval_anchor_id IS NULL"),
                    (),
                )
                .await
                .unwrap();
            assert_eq!(
                rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
                0
            );
        }
    }
}
