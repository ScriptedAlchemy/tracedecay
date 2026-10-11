//! Engine-portability lint over every registered store shape, and the
//! differential proof that the tables converted from `WITHOUT ROWID` keep their
//! key semantics.

use std::path::Path;

use tempfile::TempDir;
use tracedecay_domain::ProjectId;
use tracedecay_runtime_core::db::TestDatabaseRuntimeScope;
use tracedecay_runtime_core::db::engine::TestConnection;
use tracedecay_sql::types::Value;
use tracedecay_sql::{Connection, ErrorExt, ErrorKind, OpenFlags, params_from_iter};

use crate::tests::harness::open_registered_test_database_fixture;

/// FTS5 tables still installed on the `SQLite` engine. Each has a native
/// `USING fts` replacement; this list empties when the native engine becomes
/// the only engine.
const FTS5_ALLOWLIST: [&str; 4] = [
    "memory_v2_assertion_payloads_fts",
    "lcm_raw_messages_fts",
    "session_occurrences_fts",
    "session_summary_nodes_fts",
];

/// Tables that were `WITHOUT ROWID` before the native cutover.
const CONVERTED_TABLES: [&str; 11] = [
    "td_runtime_writer_checkpoint_v1",
    "td_runtime_writer_idempotency_v2",
    "td_runtime_writer_outbox_v1",
    "td_runtime_writer_inbox_v1",
    "external_source_commit_receipts_v2",
    "external_source_retained_receipts_v1",
    "external_source_objects_v2",
    "external_source_projected_objects_v2",
    "git_evidence_commit",
    "git_history_session_publication",
    "observation_source_presence",
];

fn open_read_only(path: &Path) -> Connection {
    Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap()
}

async fn installed_sessions_store(
    directory: &TempDir,
    scope: TestDatabaseRuntimeScope,
) -> Connection {
    crate::register_registered_schema_installer();
    let path = directory.path().join("sessions.db");
    drop(
        open_registered_test_database_fixture(&path, scope)
            .await
            .unwrap(),
    );
    open_read_only(&path)
}

/// Returns every construct the native engine cannot carry with identical
/// semantics.
fn portability_violations(connection: &Connection) -> Vec<String> {
    let mut violations = Vec::new();
    let mut tables = connection
        .prepare("SELECT name, type, wr FROM pragma_table_list WHERE schema = 'main' ORDER BY name")
        .unwrap();
    let tables: Vec<(String, String, bool)> = tables
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    for (name, kind, without_rowid) in tables {
        if name.starts_with("sqlite_") {
            continue;
        }
        match kind.as_str() {
            "virtual" => {
                if !FTS5_ALLOWLIST.contains(&name.as_str()) {
                    violations.push(format!("{name}: virtual table outside the FTS5 allowlist"));
                }
            }
            "table" => {
                if without_rowid {
                    violations.push(format!("{name}: WITHOUT ROWID"));
                }
                violations.extend(
                    nullable_primary_key_columns(connection, &name)
                        .into_iter()
                        .map(|column| format!("{name}.{column}: nullable PRIMARY KEY column")),
                );
            }
            _ => {}
        }
    }
    violations
}

/// A lone `INTEGER PRIMARY KEY` is the rowid itself and can never hold NULL.
/// Any other primary-key column of a rowid table accepts NULL unless declared
/// `NOT NULL`.
fn nullable_primary_key_columns(connection: &Connection, table: &str) -> Vec<String> {
    let mut statement = connection
        .prepare("SELECT name, type, \"notnull\", pk FROM pragma_table_info(?1) WHERE pk > 0")
        .unwrap();
    let key: Vec<(String, String, bool)> = statement
        .query_map([table], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    if let [(_, declared_type, _)] = key.as_slice()
        && declared_type.eq_ignore_ascii_case("INTEGER")
    {
        return Vec::new();
    }
    key.into_iter()
        .filter(|(_, _, not_null)| !not_null)
        .map(|(name, _, _)| name)
        .collect()
}

#[tokio::test]
async fn every_registered_store_installs_only_engine_portable_shapes() {
    let mut violations = Vec::new();
    for (label, scope) in [
        (
            "profile sessions",
            TestDatabaseRuntimeScope::ProfileSessions,
        ),
        (
            "project sessions",
            TestDatabaseRuntimeScope::ProjectSessions {
                project_id: ProjectId::new("project.schema-portability").unwrap(),
            },
        ),
    ] {
        let directory = TempDir::new().unwrap();
        let connection = installed_sessions_store(&directory, scope).await;
        violations.extend(
            portability_violations(&connection)
                .into_iter()
                .map(|violation| format!("{label}: {violation}")),
        );
    }

    let directory = TempDir::new().unwrap();
    let path = directory.path().join("project.db");
    let installer = TestConnection::open(&path);
    tracedecay_runtime_core::db::migrations::create_schema_connection(&installer)
        .await
        .unwrap();
    let mut secure_delete = installer.query("PRAGMA secure_delete", ()).await.unwrap();
    let secure_delete = secure_delete
        .next()
        .await
        .unwrap()
        .unwrap()
        .get::<i64>(0)
        .unwrap();
    if secure_delete != 0 {
        violations.push("project store: schema install enables secure_delete".to_owned());
    }
    drop(installer);
    violations.extend(
        portability_violations(&open_read_only(&path))
            .into_iter()
            .map(|violation| format!("project store: {violation}")),
    );

    assert!(
        violations.is_empty(),
        "registered schema must be engine-portable:\n{}",
        violations.join("\n")
    );
}

#[derive(Debug, PartialEq)]
enum Outcome {
    Changed(usize),
    /// The constraint failure message, which names the refusing columns.
    Refused(String),
}

struct KeyedTable {
    name: String,
    columns: Vec<String>,
    integer_columns: Vec<String>,
    key: Vec<String>,
}

impl KeyedTable {
    fn read(connection: &Connection, name: &str) -> Self {
        let mut statement = connection
            .prepare("SELECT name, type, pk FROM pragma_table_info(?1) ORDER BY cid")
            .unwrap();
        let rows: Vec<(String, String, i64)> = statement
            .query_map([name], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        let mut key: Vec<(i64, String)> = rows
            .iter()
            .filter(|(_, _, ordinal)| *ordinal > 0)
            .map(|(column, _, ordinal)| (*ordinal, column.clone()))
            .collect();
        key.sort();
        Self {
            name: name.to_owned(),
            integer_columns: rows
                .iter()
                .filter(|(_, declared_type, _)| declared_type.eq_ignore_ascii_case("INTEGER"))
                .map(|(column, _, _)| column.clone())
                .collect(),
            columns: rows.into_iter().map(|(column, _, _)| column).collect(),
            key: key.into_iter().map(|(_, column)| column).collect(),
        }
    }

    fn position(&self, column: &str) -> usize {
        self.columns
            .iter()
            .position(|candidate| candidate == column)
            .unwrap()
    }

    fn values(&self) -> Vec<&String> {
        self.columns
            .iter()
            .filter(|column| !self.key.contains(column))
            .collect()
    }

    /// A row whose every column differs from any other seed's row and
    /// satisfies the converted tables' CHECK constraints.
    fn row(&self, seed: i64) -> Vec<Value> {
        self.columns
            .iter()
            .map(|column| {
                if column == "state" {
                    Value::Text("pending".to_owned())
                } else if self.integer_columns.contains(column) {
                    Value::Integer(seed)
                } else {
                    Value::Text(format!("{column}-{seed}"))
                }
            })
            .collect()
    }

    fn key_of(&self, row: &[Value]) -> Vec<Value> {
        self.key
            .iter()
            .map(|key| row[self.position(key)].clone())
            .collect()
    }

    fn key_predicate(&self) -> String {
        self.key
            .iter()
            .enumerate()
            .map(|(index, column)| format!("{column} = ?{}", index + 1))
            .collect::<Vec<_>>()
            .join(" AND ")
    }

    fn insert_sql(&self, verb: &str) -> String {
        let placeholders = (1..=self.columns.len())
            .map(|index| format!("?{index}"))
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "{verb} INTO {} ({}) VALUES ({placeholders})",
            self.name,
            self.columns.join(", ")
        )
    }
}

fn run(connection: &Connection, sql: &str, values: &[Value]) -> Outcome {
    match connection.execute(sql, params_from_iter(values)) {
        Ok(changed) => Outcome::Changed(changed),
        Err(error) if error.kind() == ErrorKind::Constraint => Outcome::Refused(error.to_string()),
        Err(error) => panic!("{sql}: {error}"),
    }
}

/// Runs the key-semantics script against one engine shape and returns every
/// step's outcome plus the final rows in key order.
fn key_semantics_script(
    connection: &Connection,
    table: &KeyedTable,
) -> (Vec<Outcome>, Vec<Vec<Value>>) {
    let insert = table.insert_sql("INSERT");
    let first = table.row(1);
    let mut outcomes = Vec::new();

    let inserted = run(connection, &insert, &first);
    assert_eq!(
        inserted,
        Outcome::Changed(1),
        "{}: first insert",
        table.name
    );
    outcomes.push(inserted);

    let duplicate = run(connection, &insert, &first);
    assert!(
        matches!(
            &duplicate,
            Outcome::Refused(message) if message.starts_with("UNIQUE constraint failed")
        ),
        "{}: duplicate key must be refused, got {duplicate:?}",
        table.name
    );
    outcomes.push(duplicate);

    for key in &table.key {
        let mut row = table.row(2);
        row[table.position(key)] = Value::Null;
        let null_key = run(connection, &insert, &row);
        assert!(
            matches!(
                &null_key,
                Outcome::Refused(message) if message.starts_with("NOT NULL constraint failed")
            ),
            "{}.{key}: NULL primary-key column must be refused, not stored or auto-assigned",
            table.name
        );
        outcomes.push(null_key);
    }

    // One key column moved at a time: secondary UNIQUE constraints that span
    // the unchanged columns must refuse exactly as before.
    for key in &table.key {
        let position = table.position(key);
        let mut row = first.clone();
        row[position] = table.row(3)[position].clone();
        outcomes.push(run(connection, &insert, &row));
    }

    let assignments = table
        .values()
        .iter()
        .map(|column| format!("{column} = excluded.{column}"))
        .collect::<Vec<_>>()
        .join(", ");
    let mut upsert_row = table.row(4);
    for (position, column) in table.columns.iter().enumerate() {
        if table.key.contains(column) {
            upsert_row[position] = first[position].clone();
        }
    }
    let upsert = run(
        connection,
        &format!(
            "{} ON CONFLICT({}) DO UPDATE SET {assignments}",
            table.insert_sql("INSERT"),
            table.key.join(", ")
        ),
        &upsert_row,
    );
    assert_eq!(upsert, Outcome::Changed(1), "{}: upsert", table.name);
    outcomes.push(upsert);

    let ignored = run(connection, &table.insert_sql("INSERT OR IGNORE"), &first);
    assert_eq!(
        ignored,
        Outcome::Changed(0),
        "{}: insert-or-ignore",
        table.name
    );
    outcomes.push(ignored);

    let first_value = table.values()[0].clone();
    let mut update_values = table.key_of(&first);
    update_values.push(table.row(5)[table.position(&first_value)].clone());
    let update = run(
        connection,
        &format!(
            "UPDATE {} SET {first_value} = ?{} WHERE {}",
            table.name,
            table.key.len() + 1,
            table.key_predicate()
        ),
        &update_values,
    );
    assert_eq!(update, Outcome::Changed(1), "{}: update by key", table.name);
    outcomes.push(update);

    for seed in [7, 8, 9] {
        outcomes.push(run(connection, &insert, &table.row(seed)));
    }
    let delete = run(
        connection,
        &format!("DELETE FROM {} WHERE {}", table.name, table.key_predicate()),
        &table.key_of(&table.row(7)),
    );
    assert_eq!(delete, Outcome::Changed(1), "{}: delete by key", table.name);
    outcomes.push(delete);

    // The ledger's bounded prune shape: a row-value IN over a key-ordered,
    // limited candidate scan.
    let key_list = table.key.join(", ");
    outcomes.push(run(
        connection,
        &format!(
            "DELETE FROM {name} WHERE ({key_list}) IN (
                SELECT {key_list} FROM {name} ORDER BY {key_list} LIMIT 2
             )",
            name = table.name
        ),
        &[],
    ));

    let mut rows = connection
        .prepare(&format!("SELECT * FROM {} ORDER BY {key_list}", table.name))
        .unwrap();
    let width = table.columns.len();
    let rows = rows
        .query_map([], |row| {
            (0..width).map(|index| row.get::<_, Value>(index)).collect()
        })
        .unwrap()
        .map(Result::unwrap)
        .collect();
    (outcomes, rows)
}

fn install_table(installed: &Connection, table: &str, layout_suffix: &str) -> Connection {
    let connection = Connection::open_in_memory().unwrap();
    let mut objects = installed
        .prepare(
            "SELECT type, sql FROM sqlite_master
             WHERE tbl_name = ?1 AND sql IS NOT NULL
             ORDER BY type = 'index', name",
        )
        .unwrap();
    let objects: Vec<(String, String)> = objects
        .query_map([table], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert!(
        objects.iter().any(|(kind, _)| kind == "table"),
        "{table} must be installed by the registered schema"
    );
    for (kind, sql) in objects {
        let sql = if kind == "table" {
            format!("{sql}{layout_suffix}")
        } else {
            sql
        };
        connection.execute_batch(&sql).unwrap();
    }
    connection
}

#[tokio::test]
async fn converted_tables_keep_without_rowid_key_semantics() {
    let directory = TempDir::new().unwrap();
    let installed =
        installed_sessions_store(&directory, TestDatabaseRuntimeScope::ProfileSessions).await;
    for name in CONVERTED_TABLES {
        let rowid = install_table(&installed, name, "");
        let without_rowid = install_table(&installed, name, " WITHOUT ROWID");
        let table = KeyedTable::read(&rowid, name);
        assert!(table.key.len() > 1, "{name}: composite key expected");
        assert_eq!(
            key_semantics_script(&rowid, &table),
            key_semantics_script(&without_rowid, &table),
            "{name}: ROWID layout must keep the WITHOUT ROWID key semantics"
        );
    }
}

#[tokio::test]
async fn hot_ledger_lookups_search_the_primary_key_index() {
    let directory = TempDir::new().unwrap();
    let installed =
        installed_sessions_store(&directory, TestDatabaseRuntimeScope::ProfileSessions).await;
    for (table, lookup) in [
        (
            "td_runtime_writer_idempotency_v2",
            "SELECT request_digest FROM td_runtime_writer_idempotency_v2
             WHERE shard_json = ?1 AND incarnation = ?2 AND authority_epoch = ?3
               AND idempotency_key = ?4",
        ),
        (
            "td_runtime_writer_checkpoint_v1",
            "SELECT commit_sequence FROM td_runtime_writer_checkpoint_v1
             WHERE shard_json = ?1 AND incarnation = ?2",
        ),
        (
            "td_runtime_writer_idempotency_v2",
            "DELETE FROM td_runtime_writer_idempotency_v2
             WHERE (shard_json, incarnation, authority_epoch, idempotency_key) IN (
                 SELECT candidate.shard_json, candidate.incarnation,
                        candidate.authority_epoch, candidate.idempotency_key
                 FROM td_runtime_writer_idempotency_v2 AS candidate
                 WHERE candidate.shard_json = ?1 AND candidate.incarnation = ?2
                   AND candidate.authority_epoch < ?3
                 ORDER BY candidate.authority_epoch, candidate.idempotency_key
                 LIMIT ?4
             )",
        ),
    ] {
        let connection = install_table(&installed, table, "");
        let mut plan = connection
            .prepare(&format!("EXPLAIN QUERY PLAN {lookup}"))
            .unwrap();
        let parameters = plan.parameter_count();
        let plan: Vec<String> = plan
            .query_map(params_from_iter(vec![Value::Null; parameters]), |row| {
                row.get(3)
            })
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert!(
            plan.iter().all(|step| !step.starts_with("SCAN"))
                && plan.iter().any(|step| step.contains("sqlite_autoindex_")),
            "{lookup}\nmust search the primary-key index, plan: {plan:#?}"
        );
    }
}
