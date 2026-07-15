use libsql::{Connection, params};

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
    conn: &Connection,
) -> Result<(), libsql::Error> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS observation_projection_provenance (
            projector_version TEXT NOT NULL,
            observation_id TEXT NOT NULL,
            receipt_id TEXT NOT NULL,
            output_provider TEXT NOT NULL,
            output_message_id TEXT NOT NULL,
            output_digest TEXT NOT NULL,
            message_created INTEGER NOT NULL CHECK(message_created IN (0, 1)),
            PRIMARY KEY(projector_version, observation_id),
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
        );",
    )
    .await?;
    migrate_legacy_projection_output_uniqueness(conn).await?;
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_observation_projection_provenance_output
         ON observation_projection_provenance
            (projector_version, output_provider, output_message_id);
         CREATE INDEX IF NOT EXISTS idx_observation_projection_provenance_global_output
         ON observation_projection_provenance
            (output_provider, output_message_id, projector_version);",
    )
    .await?;
    Ok(())
}

async fn has_legacy_projection_output_uniqueness(conn: &Connection) -> Result<bool, libsql::Error> {
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
    conn: &Connection,
) -> Result<Vec<String>, libsql::Error> {
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

fn unsupported_legacy_projection_schema() -> libsql::Error {
    libsql::Error::Misuse("unsupported observation_projection_provenance legacy schema".to_string())
}

async fn legacy_projection_columns_match(conn: &Connection) -> Result<bool, libsql::Error> {
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

async fn legacy_projection_foreign_keys_match(conn: &Connection) -> Result<bool, libsql::Error> {
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

async fn legacy_projection_indexes_match(conn: &Connection) -> Result<bool, libsql::Error> {
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
    conn: &Connection,
    index_name: &str,
    expected_columns: &[&str],
) -> Result<bool, libsql::Error> {
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

async fn legacy_projection_table_sql_matches(conn: &Connection) -> Result<bool, libsql::Error> {
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
        .ok_or_else(|| libsql::Error::Misuse("legacy projection table is missing".to_string()))?
        .get::<String>(0)?;
    Ok(normalize_schema_sql(&sql) == normalize_schema_sql(LEGACY_PROJECTION_PROVENANCE_TABLE_SQL))
}

async fn read_supported_legacy_projection_triggers(
    conn: &Connection,
) -> Result<Vec<String>, libsql::Error> {
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
            return Err(libsql::Error::Misuse(format!(
                "unsupported observation_projection_provenance trigger {name}"
            )));
        };
        if normalize_schema_sql(&sql) != normalize_schema_sql(expected_sql) {
            return Err(libsql::Error::Misuse(format!(
                "unsupported definition for observation_projection_provenance trigger {name}"
            )));
        }
        triggers.push(sql);
    }
    Ok(triggers)
}

async fn migrate_legacy_projection_output_uniqueness(
    conn: &Connection,
) -> Result<(), libsql::Error> {
    if !has_legacy_projection_output_uniqueness(conn).await? {
        return Ok(());
    }
    let triggers = validate_legacy_projection_provenance_schema(conn).await?;

    conn.execute_batch(
        "DROP TABLE IF EXISTS observation_projection_provenance_without_output_unique;
             CREATE TABLE observation_projection_provenance_without_output_unique (
                projector_version TEXT NOT NULL,
                observation_id TEXT NOT NULL,
                receipt_id TEXT NOT NULL,
                output_provider TEXT NOT NULL,
                output_message_id TEXT NOT NULL,
                output_digest TEXT NOT NULL,
                message_created INTEGER NOT NULL CHECK(message_created IN (0, 1)),
                PRIMARY KEY(projector_version, observation_id),
                FOREIGN KEY(observation_id) REFERENCES observations(observation_id),
                FOREIGN KEY(receipt_id) REFERENCES sanitization_receipts(receipt_id)
             );
             INSERT INTO observation_projection_provenance_without_output_unique
                (projector_version, observation_id, receipt_id, output_provider,
                 output_message_id, output_digest, message_created)
             SELECT projector_version, observation_id, receipt_id, output_provider,
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
    Ok(())
}
