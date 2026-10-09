use std::time::Instant;

use rusqlite::{Connection, StatementStatus};

use super::*;
use crate::runtime_ledger::RUNTIME_LEDGER_SCHEMA;

const QUERY_SQL: &str = "SELECT id, payload FROM hot_path_rows WHERE id = ?1";
const ITERATIONS: usize = 256;

struct PrepareShare {
    prepare_ns: u128,
    execute_ns: u128,
    reprepares: i32,
}

impl PrepareShare {
    fn parse_prepare_share(&self) -> f64 {
        let total = self.prepare_ns.saturating_add(self.execute_ns);
        if total == 0 {
            0.0
        } else {
            self.prepare_ns as f64 / total as f64
        }
    }
}

fn schema_version(connection: &Connection) -> i64 {
    connection
        .query_row("PRAGMA schema_version", [], |row| row.get(0))
        .unwrap()
}

fn seed_rows(connection: &Connection) {
    connection
        .execute_batch(
            "CREATE TABLE hot_path_rows (
                id INTEGER PRIMARY KEY,
                payload TEXT NOT NULL
             );
             INSERT INTO hot_path_rows(id, payload) VALUES (1, 'row');",
        )
        .unwrap();
}

fn measure_before(connection: &Connection) -> PrepareShare {
    let mut prepare_ns = 0;
    let mut execute_ns = 0;
    let mut reprepares = 0;
    for _ in 0..ITERATIONS {
        connection.execute_batch(RUNTIME_LEDGER_SCHEMA).unwrap();
        let started = Instant::now();
        let mut statement = connection.prepare(QUERY_SQL).unwrap();
        prepare_ns += started.elapsed().as_nanos();
        let started = Instant::now();
        let payload: String = statement.query_row([1_i64], |row| row.get(1)).unwrap();
        execute_ns += started.elapsed().as_nanos();
        assert_eq!(payload, "row");
        reprepares += statement.reset_status(StatementStatus::RePrepare);
    }
    PrepareShare {
        prepare_ns,
        execute_ns,
        reprepares,
    }
}

fn measure_after(connection: &Connection) -> PrepareShare {
    let mut prepare_ns = 0;
    let mut execute_ns = 0;
    let mut reprepares = 0;
    for _ in 0..ITERATIONS {
        let started = Instant::now();
        let mut statement = prepare_read_statement(connection, QUERY_SQL).unwrap();
        prepare_ns += started.elapsed().as_nanos();
        let started = Instant::now();
        let payload: String = statement.query_row([1_i64], |row| row.get(1)).unwrap();
        execute_ns += started.elapsed().as_nanos();
        assert_eq!(payload, "row");
        reprepares += statement.reset_status(StatementStatus::RePrepare);
    }
    PrepareShare {
        prepare_ns,
        execute_ns,
        reprepares,
    }
}

fn report(label: &str, share: &PrepareShare) {
    println!(
        "{}",
        serde_json::json!({
            "path": label,
            "iterations": ITERATIONS,
            "prepare_ns": share.prepare_ns,
            "execute_ns": share.execute_ns,
            "parse_prepare_share": share.parse_prepare_share(),
            "reprepares": share.reprepares,
        })
    );
}

#[test]
fn cached_prepare_without_hot_path_ddl_does_not_reprepare() {
    let before_connection = Connection::open_in_memory().unwrap();
    seed_rows(&before_connection);
    let before = measure_before(&before_connection);

    let after_connection = Connection::open_in_memory().unwrap();
    seed_rows(&after_connection);
    after_connection
        .execute_batch(RUNTIME_LEDGER_SCHEMA)
        .unwrap();
    let version_before = schema_version(&after_connection);
    let after = measure_after(&after_connection);
    let version_after = schema_version(&after_connection);

    report("before_uncached_prepare_plus_hot_ddl", &before);
    report("after_prepare_cached_schema_once", &after);

    assert_eq!(
        version_before, version_after,
        "steady-state queries must not change the schema cookie"
    );
    assert_eq!(
        after.reprepares, 0,
        "prepare_cached hits must not reprepare when DDL stays off the query path"
    );
}

#[test]
fn intervening_ddl_reprepares_a_cached_query() {
    let connection = Connection::open_in_memory().unwrap();
    seed_rows(&connection);
    let mut statement = prepare_read_statement(&connection, QUERY_SQL).unwrap();
    let payload: String = statement.query_row([1_i64], |row| row.get(1)).unwrap();
    assert_eq!(payload, "row");
    drop(statement);

    connection
        .execute_batch("CREATE TABLE ddl_invalidates_cache(value INTEGER NOT NULL)")
        .unwrap();

    let mut statement = prepare_read_statement(&connection, QUERY_SQL).unwrap();
    let payload: String = statement.query_row([1_i64], |row| row.get(1)).unwrap();
    assert_eq!(payload, "row");
    assert!(
        statement.reset_status(StatementStatus::RePrepare) > 0,
        "CREATE TABLE on the same connection must reprepare the cached query"
    );
}

#[test]
fn execute_query_unchecked_reuses_the_cached_statement() {
    let connection = Connection::open_in_memory().unwrap();
    seed_rows(&connection);
    let version = schema_version(&connection);
    for _ in 0..ITERATIONS {
        let rows = execute_query_unchecked(
            &connection,
            ExactSqlStatement::new(QUERY_SQL.to_owned(), vec![ExactSqlValue::Integer(1)]).unwrap(),
        )
        .unwrap();
        assert_eq!(rows.rows.len(), 1);
        assert_eq!(
            rows.rows[0].values,
            vec![
                ExactSqlValue::Integer(1),
                ExactSqlValue::Text("row".to_owned()),
            ]
        );
    }
    assert_eq!(schema_version(&connection), version);
}
