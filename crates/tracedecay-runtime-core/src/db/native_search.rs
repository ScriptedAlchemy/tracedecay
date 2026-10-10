//! Search syntax for the engine attached to an exact registered store.
//!
//! Native indexes live in the same database transaction as their canonical
//! rows. Search callers still own scope, coverage, hydration, and row limits.

use crate::db::engine::BackendKind;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchIndex {
    MemoryPayload,
    RawMessage,
    Occurrence,
    Summary,
}

impl SearchIndex {
    pub const fn table_name(self) -> &'static str {
        match self {
            Self::MemoryPayload => "memory_v2_assertion_payloads",
            Self::RawMessage => "lcm_raw_messages",
            Self::Occurrence => "session_occurrences",
            Self::Summary => "session_summary_nodes",
        }
    }

    pub const fn index_name(self) -> &'static str {
        match self {
            Self::MemoryPayload => "memory_v2_assertion_payloads_fts",
            Self::RawMessage => "lcm_raw_messages_fts",
            Self::Occurrence => "session_occurrences_fts",
            Self::Summary => "session_summary_nodes_fts",
        }
    }

    pub const fn columns(self) -> &'static [&'static str] {
        match self {
            Self::MemoryPayload => &["content"],
            Self::RawMessage => &["index_text", "role", "kind", "model", "tool_names"],
            Self::Occurrence => &["index_text"],
            Self::Summary => &["summary_text"],
        }
    }

    pub const fn create_sql(self) -> &'static str {
        match self {
            Self::MemoryPayload => {
                "CREATE INDEX IF NOT EXISTS memory_v2_assertion_payloads_fts ON memory_v2_assertion_payloads USING fts (content);"
            }
            Self::RawMessage => {
                "CREATE INDEX IF NOT EXISTS lcm_raw_messages_fts ON lcm_raw_messages USING fts (index_text, role, kind, model, tool_names) WITH (weights = 'index_text=10.0,role=2.0,kind=1.0,model=1.0,tool_names=1.0');"
            }
            Self::Occurrence => {
                "CREATE INDEX IF NOT EXISTS session_occurrences_fts ON session_occurrences USING fts (index_text);"
            }
            Self::Summary => {
                "CREATE INDEX IF NOT EXISTS session_summary_nodes_fts ON session_summary_nodes USING fts (summary_text);"
            }
        }
    }
}

/// Arguments are trusted SQL column/parameter fragments supplied by callers;
/// user search text remains a bound value rather than part of the SQL.
pub fn predicate(
    backend: BackendKind,
    sqlite_table: &str,
    native_columns: &[&str],
    query_parameter: &str,
) -> String {
    match backend {
        BackendKind::Sqlite => format!("{sqlite_table} MATCH {query_parameter}"),
        BackendKind::NativeTurso => {
            format!(
                "fts_match({}, {query_parameter})",
                native_columns.join(", ")
            )
        }
    }
}

pub fn score(
    backend: BackendKind,
    sqlite_expression: &str,
    native_columns: &[&str],
    query_parameter: &str,
) -> String {
    match backend {
        BackendKind::Sqlite => sqlite_expression.to_owned(),
        BackendKind::NativeTurso => {
            format!(
                "fts_score({}, {query_parameter})",
                native_columns.join(", ")
            )
        }
    }
}

pub const fn order(backend: BackendKind) -> &'static str {
    match backend {
        BackendKind::Sqlite => "ASC",
        BackendKind::NativeTurso => "DESC",
    }
}

pub fn relevance(backend: BackendKind, raw: f64) -> f64 {
    if !raw.is_finite() {
        return 0.0;
    }
    match backend {
        BackendKind::Sqlite => (-raw).max(0.0),
        BackendKind::NativeTurso => raw.max(0.0),
    }
}

/// Quotes a literal term for the selected search parser, not for SQL.
pub fn quote_term(backend: BackendKind, term: &str) -> String {
    let escaped = match backend {
        BackendKind::Sqlite => term.replace('"', "\"\""),
        BackendKind::NativeTurso => term.replace('\\', "\\\\").replace('"', "\\\""),
    };
    format!("\"{escaped}\"")
}

/// SQLite and Turso persist equivalent DDL with different whitespace/casing.
pub fn normalize_schema_sql(sql: &str) -> String {
    sql.chars()
        .filter(|character| !character.is_whitespace() && *character != ';')
        .flat_map(char::to_lowercase)
        .collect::<String>()
        .replace("ifnotexists", "")
}

#[cfg(test)]
mod tests {
    use crate::db::engine::{BackendKind, NativeTestConnection, TransactionBehavior, params};

    use super::{SearchIndex, normalize_schema_sql, relevance};

    #[test]
    fn invalid_scores_cannot_contaminate_relevance_normalization() {
        for backend in [BackendKind::Sqlite, BackendKind::NativeTurso] {
            for invalid in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
                assert_eq!(relevance(backend, invalid), 0.0);
            }
        }
        assert_eq!(relevance(BackendKind::Sqlite, -2.0), 2.0);
        assert_eq!(relevance(BackendKind::NativeTurso, 2.0), 2.0);
    }

    async fn hits(connection: &NativeTestConnection, index: SearchIndex, query: &str) -> Vec<i64> {
        let sql = format!(
            "SELECT rowid FROM {} WHERE fts_match({}, ?1) ORDER BY rowid",
            index.table_name(),
            index.columns().join(", ")
        );
        let mut rows = connection.query(&sql, params![query]).await.unwrap();
        let mut hits = Vec::new();
        while let Some(row) = rows.next().await.unwrap() {
            hits.push(row.get(0).unwrap());
        }
        hits
    }

    #[tokio::test]
    async fn native_indexes_follow_canonical_transactions_and_reopen() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("search.db");
        let connection = NativeTestConnection::open(&path).unwrap();
        connection.execute_batch(
            "CREATE TABLE memory_v2_assertion_payloads(rowid INTEGER PRIMARY KEY, content TEXT NOT NULL);
             CREATE TABLE lcm_raw_messages(store_id INTEGER PRIMARY KEY, content TEXT NOT NULL,
                 index_text TEXT GENERATED ALWAYS AS (content) VIRTUAL,
                 role TEXT NOT NULL, kind TEXT, model TEXT, tool_names TEXT);
             CREATE TABLE session_occurrences(rowid INTEGER PRIMARY KEY, index_text TEXT NOT NULL);
             CREATE TABLE session_summary_nodes(rowid INTEGER PRIMARY KEY, summary_text TEXT NOT NULL);
             CREATE TRIGGER memory_v2_payloads_no_update BEFORE UPDATE ON memory_v2_assertion_payloads BEGIN
                 SELECT RAISE(ABORT, 'memory_v2 assertion payloads are immutable');
             END;",
        ).await.unwrap();
        let indexes = [
            SearchIndex::MemoryPayload,
            SearchIndex::RawMessage,
            SearchIndex::Occurrence,
            SearchIndex::Summary,
        ];
        for index in indexes {
            connection.execute_batch(index.create_sql()).await.unwrap();
            let mut rows = connection
                .query(
                    "SELECT sql FROM sqlite_master WHERE type = 'index' AND name = ?1",
                    params![index.index_name()],
                )
                .await
                .unwrap();
            let sql: String = rows.next().await.unwrap().unwrap().get(0).unwrap();
            assert_eq!(
                normalize_schema_sql(&sql),
                normalize_schema_sql(index.create_sql())
            );
        }
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .unwrap();
        transaction
            .execute_batch(
                "INSERT INTO memory_v2_assertion_payloads VALUES(1, 'committedword');
             INSERT INTO lcm_raw_messages(store_id,content,role) VALUES(1, 'committedword', 'user');
             INSERT INTO session_occurrences VALUES(1, 'committedword');
             INSERT INTO session_summary_nodes VALUES(1, 'committedword');",
            )
            .await
            .unwrap();
        transaction.commit().await.unwrap();
        for index in indexes {
            assert_eq!(hits(&connection, index, "committedword").await, vec![1]);
        }

        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .unwrap();
        transaction
            .execute_batch(
                "INSERT INTO memory_v2_assertion_payloads VALUES(2, 'rolledbackword');
             UPDATE lcm_raw_messages SET content = 'rolledbackword' WHERE store_id = 1;
             DELETE FROM session_occurrences WHERE rowid = 1;
             INSERT INTO session_summary_nodes VALUES(2, 'rolledbackword');",
            )
            .await
            .unwrap();
        transaction.rollback().await.unwrap();
        for index in indexes {
            assert_eq!(
                hits(&connection, index, "rolledbackword").await,
                [] as [i64; 0]
            );
            assert_eq!(hits(&connection, index, "committedword").await, vec![1]);
        }
        assert!(
            connection
                .execute(
                    "UPDATE memory_v2_assertion_payloads SET content = 'forbiddenword'",
                    ()
                )
                .await
                .is_err()
        );
        assert_eq!(
            hits(&connection, SearchIndex::MemoryPayload, "forbiddenword").await,
            [] as [i64; 0]
        );
        drop(connection);

        let reopened = NativeTestConnection::open(&path).unwrap();
        for index in indexes {
            assert_eq!(hits(&reopened, index, "committedword").await, vec![1]);
            assert_eq!(
                hits(&reopened, index, "rolledbackword").await,
                [] as [i64; 0]
            );
        }
    }

    #[tokio::test]
    async fn native_memory_install_uses_approved_logical_deletion_contract() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("memory.db");
        let connection = NativeTestConnection::open(&path).unwrap();
        crate::db::memory_v2::create_schema(&connection, "native_memory_install")
            .await
            .unwrap();
        let mut rows = connection
            .query(
                "SELECT name FROM sqlite_master WHERE name = 'memory_v2_assertion_payloads'",
                (),
            )
            .await
            .unwrap();
        assert!(rows.next().await.unwrap().is_some());
        assert!(
            connection
                .execute_batch("PRAGMA secure_delete=ON")
                .await
                .is_err()
        );
        drop(connection);
        let reopened = NativeTestConnection::open(&path).unwrap();
        let mut rows = reopened
            .query("SELECT count(*) FROM memory_v2_assertion_payloads", ())
            .await
            .unwrap();
        assert_eq!(
            rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
            0
        );
    }
}
