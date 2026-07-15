use std::collections::HashMap;

use libsql::{Connection, params};

use super::{global_db_operation_error, global_db_operation_message};

#[derive(Clone, Copy)]
struct Column {
    name: &'static str,
    declared_type: &'static str,
    not_null: bool,
    default_value: Option<&'static str>,
    primary_key_ordinal: i64,
}

const fn column(
    name: &'static str,
    declared_type: &'static str,
    not_null: bool,
    default_value: Option<&'static str>,
    primary_key_ordinal: i64,
) -> Column {
    Column {
        name,
        declared_type,
        not_null,
        default_value,
        primary_key_ordinal,
    }
}

#[derive(Clone, Copy)]
struct ForeignKey {
    from: &'static str,
    target_table: &'static str,
    target_column: &'static str,
    on_delete: &'static str,
}

const fn foreign_key(
    from: &'static str,
    target_table: &'static str,
    target_column: &'static str,
    on_delete: &'static str,
) -> ForeignKey {
    ForeignKey {
        from,
        target_table,
        target_column,
        on_delete,
    }
}

#[derive(Clone, Copy)]
struct Table {
    name: &'static str,
    columns: &'static [Column],
    foreign_keys: &'static [ForeignKey],
}

macro_rules! table {
    ($name:literal, [$($column:expr),* $(,)?], [$($foreign_key:expr),* $(,)?]) => {
        Table {
            name: $name,
            columns: &[$($column),*],
            foreign_keys: &[$($foreign_key),*],
        }
    };
}

const TABLES: &[Table] = &[
    table!(
        "projects",
        [
            column("path", "TEXT", false, None, 1),
            column("tokens_saved", "INTEGER", true, Some("0"), 0),
        ],
        []
    ),
    table!(
        "code_projects",
        [
            column("project_id", "TEXT", false, None, 1),
            column("canonical_root", "TEXT", true, None, 0),
            column("display_root", "TEXT", true, None, 0),
            column("primary_root_platform", "TEXT", false, None, 0),
            column("primary_root_bytes", "BLOB", false, None, 0),
            column("primary_root_last_seen_at", "INTEGER", false, None, 0),
            column("git_common_dir", "TEXT", false, None, 0),
            column("git_remote_url", "TEXT", false, None, 0),
            column("default_branch", "TEXT", false, None, 0),
            column("created_at", "INTEGER", true, None, 0),
            column("last_seen_at", "INTEGER", true, None, 0),
        ],
        []
    ),
    table!(
        "project_aliases",
        [
            column("alias_path", "TEXT", false, None, 1),
            column("project_id", "TEXT", true, None, 0),
            column("last_seen_at", "INTEGER", true, None, 0),
        ],
        [foreign_key(
            "project_id",
            "code_projects",
            "project_id",
            "CASCADE"
        )]
    ),
    table!(
        "store_instances",
        [
            column("store_id", "TEXT", false, None, 1),
            column("project_id", "TEXT", true, None, 0),
            column("store_kind", "TEXT", true, None, 0),
            column("storage_mode", "TEXT", true, None, 0),
            column("store_relpath", "TEXT", true, None, 0),
            column("manifest_relpath", "TEXT", false, None, 0),
            column("created_at", "INTEGER", true, None, 0),
            column("last_verified_at", "INTEGER", false, None, 0),
            column("last_write_at", "INTEGER", false, None, 0),
        ],
        [foreign_key(
            "project_id",
            "code_projects",
            "project_id",
            "CASCADE"
        )]
    ),
    table!(
        "graph_scopes",
        [
            column("graph_scope_id", "TEXT", false, None, 1),
            column("project_id", "TEXT", true, None, 0),
            column("store_id", "TEXT", true, None, 0),
            column("branch_name", "TEXT", true, None, 0),
            column("db_relpath", "TEXT", true, None, 0),
            column("parent_scope_id", "TEXT", false, None, 0),
            column("last_synced_at", "INTEGER", false, None, 0),
            column("writable", "INTEGER", true, Some("1"), 0),
        ],
        [
            foreign_key("project_id", "code_projects", "project_id", "CASCADE"),
            foreign_key("store_id", "store_instances", "store_id", "CASCADE"),
        ]
    ),
    table!(
        "store_artifacts",
        [
            column("store_id", "TEXT", true, None, 1),
            column("artifact_kind", "TEXT", true, None, 2),
            column("relpath", "TEXT", true, None, 3),
            column("size_bytes", "INTEGER", false, None, 0),
            column("schema_version", "TEXT", false, None, 0),
            column("updated_at", "INTEGER", false, None, 0),
        ],
        [foreign_key(
            "store_id",
            "store_instances",
            "store_id",
            "CASCADE"
        )]
    ),
    table!(
        "sanitization_receipts",
        [
            column("receipt_id", "TEXT", false, None, 1),
            column("sanitizer_version", "TEXT", true, None, 0),
            column("payload_digest", "TEXT", true, None, 0),
            column("receipt_json", "TEXT", true, None, 0),
        ],
        []
    ),
    table!(
        "observations",
        [
            column("sequence", "INTEGER", false, None, 1),
            column("observation_id", "TEXT", true, None, 0),
            column("payload_digest", "TEXT", true, None, 0),
            column("receipt_id", "TEXT", true, None, 0),
            column("observation_json", "TEXT", true, None, 0),
            column("committed_cursor_json", "TEXT", true, None, 0),
        ],
        [foreign_key(
            "receipt_id",
            "sanitization_receipts",
            "receipt_id",
            "NO ACTION"
        )]
    ),
    table!(
        "source_cursors",
        [
            column("source_json", "TEXT", true, None, 1),
            column("scope_json", "TEXT", true, None, 2),
            column("cursor_json", "TEXT", true, None, 0),
        ],
        []
    ),
    table!(
        "projection_queue",
        [
            column("observation_id", "TEXT", false, None, 1),
            column("observation_sequence", "INTEGER", true, None, 0),
        ],
        [foreign_key(
            "observation_id",
            "observations",
            "observation_id",
            "NO ACTION"
        )]
    ),
    table!(
        "observation_projection_provenance",
        [
            column("projector_version", "TEXT", true, None, 1),
            column("observation_id", "TEXT", true, None, 2),
            column("receipt_id", "TEXT", true, None, 0),
            column("output_provider", "TEXT", true, None, 0),
            column("output_message_id", "TEXT", true, None, 0),
            column("output_digest", "TEXT", true, None, 0),
            column("message_created", "INTEGER", true, None, 0),
        ],
        [
            foreign_key(
                "observation_id",
                "observations",
                "observation_id",
                "NO ACTION"
            ),
            foreign_key(
                "receipt_id",
                "sanitization_receipts",
                "receipt_id",
                "NO ACTION"
            ),
        ]
    ),
    table!(
        "observation_projection_checkpoints",
        [
            column("projector_version", "TEXT", false, None, 1),
            column("last_sequence", "INTEGER", true, None, 0),
        ],
        []
    ),
    table!(
        "observation_projection_aliases",
        [
            column("projector_version", "TEXT", true, None, 1),
            column("observation_id", "TEXT", true, None, 2),
            column("output_provider", "TEXT", true, None, 0),
            column("output_message_id", "TEXT", true, None, 0),
        ],
        [foreign_key(
            "observation_id",
            "observations",
            "observation_id",
            "NO ACTION"
        )]
    ),
    table!(
        "observation_projection_dispositions",
        [
            column("projector_version", "TEXT", true, None, 1),
            column("observation_id", "TEXT", true, None, 2),
            column("receipt_id", "TEXT", true, None, 0),
            column("reason", "TEXT", true, None, 0),
        ],
        [
            foreign_key(
                "observation_id",
                "observations",
                "observation_id",
                "NO ACTION"
            ),
            foreign_key(
                "receipt_id",
                "sanitization_receipts",
                "receipt_id",
                "NO ACTION"
            ),
        ]
    ),
];

#[derive(Clone, Copy)]
struct Index {
    table: &'static str,
    name: Option<&'static str>,
    unique: bool,
    columns: &'static [&'static str],
}

const INDEXES: &[Index] = &[
    Index {
        table: "project_aliases",
        name: Some("idx_project_aliases_project_id"),
        unique: false,
        columns: &["project_id"],
    },
    Index {
        table: "store_instances",
        name: Some("idx_store_instances_project_id"),
        unique: false,
        columns: &["project_id"],
    },
    Index {
        table: "graph_scopes",
        name: Some("idx_graph_scopes_project_store"),
        unique: false,
        columns: &["project_id", "store_id"],
    },
    Index {
        table: "observations",
        name: None,
        unique: true,
        columns: &["observation_id"],
    },
    Index {
        table: "projection_queue",
        name: None,
        unique: true,
        columns: &["observation_sequence"],
    },
];

const TRIGGERS: &[(&str, &str)] = &[
    ("observations_immutable_update", "observations"),
    ("observations_immutable_delete", "observations"),
    ("graph_scopes_store_project_insert_v1", "graph_scopes"),
    ("graph_scopes_store_project_update_v1", "graph_scopes"),
    ("projection_queue_identity_insert_v1", "projection_queue"),
    ("projection_queue_identity_update_v1", "projection_queue"),
    (
        "projection_provenance_receipt_insert_v1",
        "observation_projection_provenance",
    ),
    (
        "projection_provenance_receipt_update_v1",
        "observation_projection_provenance",
    ),
    (
        "projection_disposition_receipt_insert_v1",
        "observation_projection_dispositions",
    ),
    (
        "projection_disposition_receipt_update_v1",
        "observation_projection_dispositions",
    ),
    (
        "projection_provenance_message_created_insert_v1",
        "observation_projection_provenance",
    ),
    (
        "projection_provenance_message_created_update_v1",
        "observation_projection_provenance",
    ),
    (
        "projection_checkpoint_sequence_insert_v1",
        "observation_projection_checkpoints",
    ),
    (
        "projection_checkpoint_sequence_update_v1",
        "observation_projection_checkpoints",
    ),
];

fn normalize_default(value: Option<&str>) -> Option<String> {
    value.map(|value| {
        let mut value = value.trim();
        while value.starts_with('(') && value.ends_with(')') && value.len() > 1 {
            value = value[1..value.len() - 1].trim();
        }
        value.to_ascii_lowercase()
    })
}

async fn validate_table(conn: &Connection, contract: &Table) -> crate::errors::Result<()> {
    const OPERATION: &str = "validate global database schema";
    let mut rows = conn
        .query(
            "SELECT name, type, \"notnull\", dflt_value, pk FROM pragma_table_info(?1)",
            params![contract.name],
        )
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    let mut actual = HashMap::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?
    {
        let name = row
            .get::<String>(0)
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        actual.insert(
            name.to_ascii_lowercase(),
            (
                row.get::<String>(1)
                    .map_err(|error| global_db_operation_error(OPERATION, error))?,
                row.get::<i64>(2)
                    .map_err(|error| global_db_operation_error(OPERATION, error))?
                    != 0,
                row.get::<Option<String>>(3)
                    .map_err(|error| global_db_operation_error(OPERATION, error))?,
                row.get::<i64>(4)
                    .map_err(|error| global_db_operation_error(OPERATION, error))?,
            ),
        );
    }
    if actual.len() != contract.columns.len() {
        return Err(global_db_operation_message(
            OPERATION,
            format!(
                "table '{}' has an incompatible number of columns",
                contract.name
            ),
        ));
    }
    for column in contract.columns {
        let Some((declared_type, not_null, default_value, primary_key_ordinal)) =
            actual.get(&column.name.to_ascii_lowercase())
        else {
            return Err(global_db_operation_message(
                OPERATION,
                format!(
                    "table '{}' is missing column '{}'",
                    contract.name, column.name
                ),
            ));
        };
        if !declared_type.eq_ignore_ascii_case(column.declared_type)
            || *not_null != column.not_null
            || normalize_default(default_value.as_deref())
                != normalize_default(column.default_value)
            || *primary_key_ordinal != column.primary_key_ordinal
        {
            return Err(global_db_operation_message(
                OPERATION,
                format!(
                    "table '{}' column '{}' has incompatible type/null/default/primary-key metadata",
                    contract.name, column.name
                ),
            ));
        }
    }

    let mut rows = conn
        .query(
            "SELECT \"from\", \"table\", \"to\", on_update, on_delete, \"match\"
             FROM pragma_foreign_key_list(?1)",
            params![contract.name],
        )
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    let mut foreign_keys = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?
    {
        foreign_keys.push((
            row.get::<String>(0)
                .map_err(|error| global_db_operation_error(OPERATION, error))?,
            row.get::<String>(1)
                .map_err(|error| global_db_operation_error(OPERATION, error))?,
            row.get::<String>(2)
                .map_err(|error| global_db_operation_error(OPERATION, error))?,
            row.get::<String>(3)
                .map_err(|error| global_db_operation_error(OPERATION, error))?,
            row.get::<String>(4)
                .map_err(|error| global_db_operation_error(OPERATION, error))?,
            row.get::<String>(5)
                .map_err(|error| global_db_operation_error(OPERATION, error))?,
        ));
    }
    if foreign_keys.len() != contract.foreign_keys.len()
        || contract.foreign_keys.iter().any(|expected| {
            !foreign_keys.iter().any(|actual| {
                actual.0.eq_ignore_ascii_case(expected.from)
                    && actual.1.eq_ignore_ascii_case(expected.target_table)
                    && actual.2.eq_ignore_ascii_case(expected.target_column)
                    && actual.3.eq_ignore_ascii_case("NO ACTION")
                    && actual.4.eq_ignore_ascii_case(expected.on_delete)
                    && actual.5.eq_ignore_ascii_case("NONE")
            })
        })
    {
        return Err(global_db_operation_message(
            OPERATION,
            format!("table '{}' has incompatible foreign keys", contract.name),
        ));
    }
    Ok(())
}

async fn index_columns(conn: &Connection, name: &str) -> crate::errors::Result<Vec<String>> {
    const OPERATION: &str = "validate global database schema";
    let mut rows = conn
        .query(
            "SELECT name FROM pragma_index_info(?1) ORDER BY seqno",
            params![name],
        )
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    let mut columns = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?
    {
        columns.push(
            row.get::<String>(0)
                .map_err(|error| global_db_operation_error(OPERATION, error))?,
        );
    }
    Ok(columns)
}

fn same_columns(actual: &[String], expected: &[&str]) -> bool {
    actual.len() == expected.len()
        && actual
            .iter()
            .zip(expected)
            .all(|(actual, expected)| actual.eq_ignore_ascii_case(expected))
}

async fn validate_index(conn: &Connection, contract: &Index) -> crate::errors::Result<()> {
    const OPERATION: &str = "validate global database schema";
    let mut rows = conn
        .query(
            "SELECT name, \"unique\" FROM pragma_index_list(?1)",
            params![contract.table],
        )
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    let mut candidates = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?
    {
        candidates.push((
            row.get::<String>(0)
                .map_err(|error| global_db_operation_error(OPERATION, error))?,
            row.get::<i64>(1)
                .map_err(|error| global_db_operation_error(OPERATION, error))?
                != 0,
        ));
    }
    for (name, unique) in candidates {
        if unique == contract.unique
            && contract
                .name
                .is_none_or(|expected| name.eq_ignore_ascii_case(expected))
            && same_columns(&index_columns(conn, &name).await?, contract.columns)
        {
            return Ok(());
        }
    }
    Err(global_db_operation_message(
        OPERATION,
        format!(
            "table '{}' is missing required {}index on ({})",
            contract.table,
            if contract.unique { "unique " } else { "" },
            contract.columns.join(", ")
        ),
    ))
}

fn unique_keys(table: &str) -> &'static [&'static [&'static str]] {
    const OBSERVATIONS: &[&[&str]] = &[&["observation_id"]];
    const PROJECTION_QUEUE: &[&[&str]] = &[&["observation_sequence"]];
    match table {
        "observations" => OBSERVATIONS,
        "projection_queue" => PROJECTION_QUEUE,
        _ => &[],
    }
}

async fn validate_unique_keys(conn: &Connection, table: &str) -> crate::errors::Result<()> {
    const OPERATION: &str = "validate global database schema";
    let mut rows = conn
        .query(
            "SELECT name FROM pragma_index_list(?1)
             WHERE \"unique\" = 1 AND origin != 'pk'",
            params![table],
        )
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    let mut names = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?
    {
        names.push(
            row.get::<String>(0)
                .map_err(|error| global_db_operation_error(OPERATION, error))?,
        );
    }
    let mut actual = Vec::new();
    for name in names {
        actual.push(index_columns(conn, &name).await?);
    }
    let expected = unique_keys(table);
    if actual.len() == expected.len()
        && expected
            .iter()
            .all(|expected| actual.iter().any(|actual| same_columns(actual, expected)))
    {
        Ok(())
    } else {
        Err(global_db_operation_message(
            OPERATION,
            format!("table '{table}' has incompatible unique-key indexes"),
        ))
    }
}

async fn validate_trigger(conn: &Connection, name: &str, table: &str) -> crate::errors::Result<()> {
    const OPERATION: &str = "validate global database schema";
    let mut rows = conn
        .query(
            "SELECT tbl_name FROM sqlite_master
             WHERE type = 'trigger' AND name = ?1 COLLATE NOCASE",
            params![name],
        )
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    let actual = rows
        .next()
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?
        .map(|row| row.get::<String>(0))
        .transpose()
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    if actual
        .as_deref()
        .is_some_and(|actual| actual.eq_ignore_ascii_case(table))
    {
        Ok(())
    } else {
        Err(global_db_operation_message(
            OPERATION,
            format!("required trigger '{name}' on table '{table}' is missing"),
        ))
    }
}

pub(super) async fn ensure_cross_identity_triggers(conn: &Connection) -> Result<(), libsql::Error> {
    conn.execute_batch(
        "CREATE TRIGGER IF NOT EXISTS graph_scopes_store_project_insert_v1
             BEFORE INSERT ON graph_scopes WHEN NOT EXISTS (
                 SELECT 1 FROM store_instances WHERE store_id = NEW.store_id AND project_id = NEW.project_id
             ) BEGIN SELECT RAISE(ABORT, 'graph scope store/project mismatch'); END;
         CREATE TRIGGER IF NOT EXISTS graph_scopes_store_project_update_v1
             BEFORE UPDATE OF store_id, project_id ON graph_scopes WHEN NOT EXISTS (
                 SELECT 1 FROM store_instances WHERE store_id = NEW.store_id AND project_id = NEW.project_id
             ) BEGIN SELECT RAISE(ABORT, 'graph scope store/project mismatch'); END;
         CREATE TRIGGER IF NOT EXISTS projection_queue_identity_insert_v1
             BEFORE INSERT ON projection_queue WHEN NOT EXISTS (
                 SELECT 1 FROM observations WHERE observation_id = NEW.observation_id AND sequence = NEW.observation_sequence
             ) BEGIN SELECT RAISE(ABORT, 'projection queue observation identity mismatch'); END;
         CREATE TRIGGER IF NOT EXISTS projection_queue_identity_update_v1
             BEFORE UPDATE OF observation_id, observation_sequence ON projection_queue WHEN NOT EXISTS (
                 SELECT 1 FROM observations WHERE observation_id = NEW.observation_id AND sequence = NEW.observation_sequence
             ) BEGIN SELECT RAISE(ABORT, 'projection queue observation identity mismatch'); END;
         CREATE TRIGGER IF NOT EXISTS projection_provenance_receipt_insert_v1
             BEFORE INSERT ON observation_projection_provenance WHEN NOT EXISTS (
                 SELECT 1 FROM observations WHERE observation_id = NEW.observation_id AND receipt_id = NEW.receipt_id
             ) BEGIN SELECT RAISE(ABORT, 'projection provenance receipt mismatch'); END;
         CREATE TRIGGER IF NOT EXISTS projection_provenance_receipt_update_v1
             BEFORE UPDATE OF observation_id, receipt_id ON observation_projection_provenance WHEN NOT EXISTS (
                 SELECT 1 FROM observations WHERE observation_id = NEW.observation_id AND receipt_id = NEW.receipt_id
             ) BEGIN SELECT RAISE(ABORT, 'projection provenance receipt mismatch'); END;
         CREATE TRIGGER IF NOT EXISTS projection_disposition_receipt_insert_v1
             BEFORE INSERT ON observation_projection_dispositions WHEN NOT EXISTS (
                 SELECT 1 FROM observations WHERE observation_id = NEW.observation_id AND receipt_id = NEW.receipt_id
             ) BEGIN SELECT RAISE(ABORT, 'projection disposition receipt mismatch'); END;
         CREATE TRIGGER IF NOT EXISTS projection_disposition_receipt_update_v1
             BEFORE UPDATE OF observation_id, receipt_id ON observation_projection_dispositions WHEN NOT EXISTS (
                 SELECT 1 FROM observations WHERE observation_id = NEW.observation_id AND receipt_id = NEW.receipt_id
             ) BEGIN SELECT RAISE(ABORT, 'projection disposition receipt mismatch'); END;
         CREATE TRIGGER IF NOT EXISTS projection_provenance_message_created_insert_v1
             BEFORE INSERT ON observation_projection_provenance WHEN NEW.message_created NOT IN (0, 1)
             BEGIN SELECT RAISE(ABORT, 'invalid projection message_created'); END;
         CREATE TRIGGER IF NOT EXISTS projection_provenance_message_created_update_v1
             BEFORE UPDATE OF message_created ON observation_projection_provenance WHEN NEW.message_created NOT IN (0, 1)
             BEGIN SELECT RAISE(ABORT, 'invalid projection message_created'); END;
         CREATE TRIGGER IF NOT EXISTS projection_checkpoint_sequence_insert_v1
             BEFORE INSERT ON observation_projection_checkpoints WHEN NEW.last_sequence < 0
             BEGIN SELECT RAISE(ABORT, 'invalid projection checkpoint sequence'); END;
         CREATE TRIGGER IF NOT EXISTS projection_checkpoint_sequence_update_v1
             BEFORE UPDATE OF last_sequence ON observation_projection_checkpoints WHEN NEW.last_sequence < 0
             BEGIN SELECT RAISE(ABORT, 'invalid projection checkpoint sequence'); END;",
    )
    .await
    .map(|_| ())
}

async fn validate_query_has_no_rows(
    conn: &Connection,
    query: &str,
    violation: &'static str,
) -> crate::errors::Result<()> {
    const OPERATION: &str = "validate global database schema";
    let mut rows = conn
        .query(query, ())
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    if rows
        .next()
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?
        .is_some()
    {
        return Err(global_db_operation_message(OPERATION, violation));
    }
    Ok(())
}

async fn validate_cross_identity_rows(conn: &Connection) -> crate::errors::Result<()> {
    for (query, violation) in [
        (
            "SELECT 1 FROM graph_scopes AS scope LEFT JOIN store_instances AS store
             ON store.store_id = scope.store_id AND store.project_id = scope.project_id
             WHERE store.store_id IS NULL LIMIT 1",
            "graph_scopes contains a store/project identity mismatch",
        ),
        (
            "SELECT 1 FROM projection_queue AS queue LEFT JOIN observations AS observation
             ON observation.observation_id = queue.observation_id
             AND observation.sequence = queue.observation_sequence
             WHERE observation.observation_id IS NULL LIMIT 1",
            "projection_queue contains an observation identity mismatch",
        ),
        (
            "SELECT 1 FROM observation_projection_provenance AS provenance
             LEFT JOIN observations AS observation
             ON observation.observation_id = provenance.observation_id
             AND observation.receipt_id = provenance.receipt_id
             WHERE observation.observation_id IS NULL LIMIT 1",
            "observation projection provenance contains a receipt mismatch",
        ),
        (
            "SELECT 1 FROM observation_projection_dispositions AS disposition
             LEFT JOIN observations AS observation
             ON observation.observation_id = disposition.observation_id
             AND observation.receipt_id = disposition.receipt_id
             WHERE observation.observation_id IS NULL LIMIT 1",
            "observation projection disposition contains a receipt mismatch",
        ),
        (
            "SELECT 1 FROM observation_projection_provenance WHERE message_created NOT IN (0, 1) LIMIT 1",
            "observation projection provenance contains invalid message_created",
        ),
        (
            "SELECT 1 FROM observation_projection_checkpoints WHERE last_sequence < 0 LIMIT 1",
            "observation projection checkpoints contains a negative sequence",
        ),
        (
            "PRAGMA foreign_key_check",
            "global database contains a foreign-key violation",
        ),
    ] {
        validate_query_has_no_rows(conn, query, violation).await?;
    }
    Ok(())
}

pub(super) async fn validate_global_schema_contract(
    conn: &Connection,
) -> crate::errors::Result<()> {
    for contract in TABLES {
        validate_table(conn, contract).await?;
        validate_unique_keys(conn, contract.name).await?;
    }
    for contract in INDEXES {
        validate_index(conn, contract).await?;
    }
    for (name, table) in TRIGGERS {
        validate_trigger(conn, name, table).await?;
    }
    validate_cross_identity_rows(conn).await
}
