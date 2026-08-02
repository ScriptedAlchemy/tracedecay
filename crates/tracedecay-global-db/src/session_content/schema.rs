use tracedecay_runtime_core::db::engine::{Error, Executor, QueryExecutor, params};

/// Fresh-store V2 schema for immutable, deduplicated session content.
///
/// This fragment intentionally has no migration ladder. A database that
/// already has any incompatible shape is refused by the verification below and
/// must be recreated; attempting to repair it in place could turn a digest
/// into an accidental read authority.
pub const SESSION_CONTENT_SCHEMA_DDL: &str = r#"
    CREATE TABLE session_content_objects (
        content_digest TEXT NOT NULL CHECK(
            length(content_digest) = 71
            AND substr(content_digest, 1, 7) = 'sha256:'
            AND substr(content_digest, 8) NOT GLOB '*[^0123456789abcdef]*'
        ),
        inline_bytes BLOB,
        durable_file_locator TEXT,
        byte_count INTEGER NOT NULL CHECK(byte_count >= 0),
        char_count INTEGER NOT NULL CHECK(char_count >= 0),
        CHECK(
            (inline_bytes IS NOT NULL AND durable_file_locator IS NULL)
            OR (inline_bytes IS NULL AND durable_file_locator IS NOT NULL)
        ),
        CHECK(inline_bytes IS NULL OR length(inline_bytes) = byte_count),
        CHECK(durable_file_locator IS NULL OR length(durable_file_locator) > 0),
        PRIMARY KEY(content_digest)
    );

    CREATE TABLE session_content_references (
        owner_kind TEXT NOT NULL CHECK(owner_kind IN ('projection', 'occurrence', 'summary')),
        owner_id TEXT NOT NULL CHECK(length(owner_id) > 0),
        content_kind TEXT NOT NULL CHECK(content_kind IN (
            'observation_json', 'message_text', 'summary_text'
        )),
        content_digest TEXT NOT NULL,
        sanitization_receipt_id TEXT,
        retrieval_anchor_id TEXT,
        PRIMARY KEY(owner_kind, owner_id),
        FOREIGN KEY(content_digest)
            REFERENCES session_content_objects(content_digest)
            ON DELETE RESTRICT,
        CHECK(
            (owner_kind IN ('projection', 'occurrence')
                AND sanitization_receipt_id IS NOT NULL
                AND retrieval_anchor_id IS NULL)
            OR (owner_kind = 'summary'
                AND sanitization_receipt_id IS NULL
                AND retrieval_anchor_id IS NOT NULL)
        )
    );
    CREATE INDEX idx_session_content_references_object
        ON session_content_references(content_digest);

    -- A map retains only object identity and a bounded indexed character count.
    -- The FTS virtual table is contentless, so it never becomes a second source
    -- for the canonical sanitized value.
    CREATE TABLE session_content_fts_rows (
        fts_rowid INTEGER PRIMARY KEY,
        content_digest TEXT NOT NULL,
        indexed_char_count INTEGER NOT NULL CHECK(indexed_char_count BETWEEN 1 AND 65536),
        UNIQUE(content_digest),
        FOREIGN KEY(content_digest)
            REFERENCES session_content_objects(content_digest)
            ON DELETE RESTRICT
    );
    CREATE VIRTUAL TABLE session_content_fts USING fts5(
        index_text,
        content='',
        detail='none',
        columnsize=0
    );

    CREATE TRIGGER session_content_objects_immutable_update
    BEFORE UPDATE ON session_content_objects
    BEGIN
        SELECT RAISE(ABORT, 'session content objects are immutable');
    END;
    CREATE TRIGGER session_content_references_immutable_update
    BEFORE UPDATE ON session_content_references
    BEGIN
        SELECT RAISE(ABORT, 'session content references are immutable');
    END;
"#;

const SESSION_CONTENT_TABLE_COLUMNS: &[(&str, &[&str])] = &[
    (
        "session_content_objects",
        &[
            "content_digest",
            "inline_bytes",
            "durable_file_locator",
            "byte_count",
            "char_count",
        ],
    ),
    (
        "session_content_references",
        &[
            "owner_kind",
            "owner_id",
            "content_kind",
            "content_digest",
            "sanitization_receipt_id",
            "retrieval_anchor_id",
        ],
    ),
    (
        "session_content_fts_rows",
        &["fts_rowid", "content_digest", "indexed_char_count"],
    ),
];

/// Installs the content-object fragment into a closed, empty staging store.
///
/// Callers must never invoke this against an existing profile database.
pub async fn install_session_content_schema(conn: &impl Executor) -> Result<(), Error> {
    conn.execute_batch(SESSION_CONTENT_SCHEMA_DDL).await?;
    validate_session_content_schema(conn).await
}

/// Read-only validation used by the exact final-schema catalog verifier.
pub async fn validate_session_content_schema(conn: &impl QueryExecutor) -> Result<(), Error> {
    for &(table, expected_columns) in SESSION_CONTENT_TABLE_COLUMNS {
        let actual_columns = column_names(conn, table).await?;
        if !columns_match(&actual_columns, expected_columns) {
            return Err(unsupported_session_content_schema(table));
        }
    }

    let fts_sql = schema_sql(conn, "session_content_fts").await?;
    if !normalized(&fts_sql).contains(
        "createvirtualtablesession_content_ftsusingfts5(index_text,content='',detail='none',columnsize=0)",
    ) {
        return Err(unsupported_session_content_schema("session_content_fts"));
    }
    for (trigger, message) in [
        (
            "session_content_objects_immutable_update",
            "raise(abort,'sessioncontentobjectsareimmutable')",
        ),
        (
            "session_content_references_immutable_update",
            "raise(abort,'sessioncontentreferencesareimmutable')",
        ),
    ] {
        let trigger_sql = schema_sql(conn, trigger).await?;
        if !normalized(&trigger_sql).contains(message) {
            return Err(unsupported_session_content_schema(trigger));
        }
    }
    Ok(())
}

async fn column_names(conn: &impl QueryExecutor, table: &str) -> Result<Vec<String>, Error> {
    let mut rows = conn
        .query(
            "SELECT name FROM pragma_table_xinfo(?1) WHERE hidden = 0 ORDER BY cid",
            params![table],
        )
        .await?;
    let mut columns = Vec::new();
    while let Some(row) = rows.next().await? {
        columns.push(row.get::<String>(0)?);
    }
    Ok(columns)
}

async fn schema_sql(conn: &impl QueryExecutor, name: &str) -> Result<String, Error> {
    let mut rows = conn
        .query(
            "SELECT sql FROM sqlite_schema WHERE name = ?1",
            params![name],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Err(unsupported_session_content_schema(name));
    };
    row.get::<String>(0)
}

fn columns_match(actual: &[String], expected: &[&str]) -> bool {
    actual
        .iter()
        .map(String::as_str)
        .eq(expected.iter().copied())
}

fn normalized(sql: &str) -> String {
    sql.chars()
        .filter(|character| !character.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect()
}

fn unsupported_session_content_schema(name: &str) -> Error {
    Error::invalid_operation(format!(
        "session content schema object `{name}` is not at the shape this binary supports; \
         this store was created by an incompatible binary and cannot be upgraded in place. \
         Remove the store directory and let this binary create a fresh one."
    ))
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;
    use tracedecay_runtime_core::db::engine::{Executor, QueryExecutor, TestConnection};

    #[tokio::test]
    async fn installs_contentless_fts_and_exposes_unreferenced_objects_for_gc() {
        let temp = TempDir::new().unwrap();
        let conn = TestConnection::open(&temp.path().join("global.db"));

        super::install_session_content_schema(&conn).await.unwrap();
        conn.execute_batch(
            "INSERT INTO session_content_objects (
                content_digest, inline_bytes, durable_file_locator, byte_count, char_count
             ) VALUES
                ('sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
                 X'73616e6974697a6564', NULL, 9, 9),
                ('sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb',
                 X'6f727068616e', NULL, 6, 6);
             INSERT INTO session_content_references (
                owner_kind, owner_id, content_kind, content_digest,
                sanitization_receipt_id, retrieval_anchor_id
             ) VALUES (
                'occurrence', 'sha256:occurrence', 'message_text',
                'sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
                'receipt.fixture', NULL
             );",
        )
        .await
        .unwrap();

        let mut rows = conn
            .query(
                "SELECT content_digest
                 FROM session_content_objects AS object
                 WHERE NOT EXISTS (
                    SELECT 1
                    FROM session_content_references AS reference
                    WHERE reference.content_digest = object.content_digest
                 )",
                (),
            )
            .await
            .unwrap();
        let orphan = rows
            .next()
            .await
            .unwrap()
            .unwrap()
            .get::<String>(0)
            .unwrap();
        assert_eq!(
            orphan,
            "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
        );
        assert!(rows.next().await.unwrap().is_none());

        let mut rows = conn
            .query(
                "SELECT sql FROM sqlite_schema WHERE name = 'session_content_fts'",
                (),
            )
            .await
            .unwrap();
        let fts_sql = rows
            .next()
            .await
            .unwrap()
            .unwrap()
            .get::<String>(0)
            .unwrap();
        assert!(
            fts_sql.contains("content=''"),
            "expected contentless FTS: {fts_sql}"
        );
    }

    #[tokio::test]
    async fn references_are_immutable_after_authorized_insert() {
        let temp = TempDir::new().unwrap();
        let conn = TestConnection::open(&temp.path().join("global.db"));

        super::install_session_content_schema(&conn).await.unwrap();
        conn.execute_batch(
            "INSERT INTO session_content_objects (
                content_digest, inline_bytes, durable_file_locator, byte_count, char_count
             ) VALUES (
                'sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc',
                X'73616e6974697a6564', NULL, 9, 9
             );
             INSERT INTO session_content_references (
                owner_kind, owner_id, content_kind, content_digest,
                sanitization_receipt_id, retrieval_anchor_id
             ) VALUES (
                'occurrence', 'sha256:immutable', 'message_text',
                'sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc',
                'receipt.immutable', NULL
             );",
        )
        .await
        .unwrap();

        assert!(
            conn.execute(
                "UPDATE session_content_references
                 SET sanitization_receipt_id = 'receipt.replaced'
                 WHERE owner_kind = 'occurrence' AND owner_id = 'sha256:immutable'",
                (),
            )
            .await
            .is_err(),
            "an authorized content reference must not be mutable"
        );
        assert!(
            conn.execute(
                "UPDATE session_content_objects
                 SET inline_bytes = X'7265706c61636564'
                 WHERE content_digest =
                    'sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc'",
                (),
            )
            .await
            .is_err(),
            "canonical content bytes must not be mutable"
        );
    }

    #[tokio::test]
    async fn one_object_can_authorize_distinct_semantic_kinds_and_age_out() {
        let temp = TempDir::new().unwrap();
        let conn = TestConnection::open(&temp.path().join("global.db"));

        super::install_session_content_schema(&conn).await.unwrap();
        conn.execute_batch(
            "INSERT INTO session_content_objects (
                content_digest, inline_bytes, durable_file_locator, byte_count, char_count
             ) VALUES (
                'sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd',
                X'7b22616e73776572223a34327d', NULL, 13, 13
             );
             INSERT INTO session_content_references (
                owner_kind, owner_id, content_kind, content_digest,
                sanitization_receipt_id, retrieval_anchor_id
             ) VALUES
                ('occurrence', 'sha256:occurrence-one', 'message_text',
                 'sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd',
                 'receipt.one', NULL),
                ('summary', 'summary-one', 'summary_text',
                 'sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd',
                 NULL, 'anchor.one');
             DELETE FROM session_content_references
             WHERE owner_kind = 'summary' AND owner_id = 'summary-one';",
        )
        .await
        .unwrap();

        let mut rows = conn
            .query(
                "SELECT COUNT(*) FROM session_content_objects
                 WHERE content_digest =
                    'sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd'",
                (),
            )
            .await
            .unwrap();
        let count = rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap();
        assert_eq!(count, 1);
    }
}
