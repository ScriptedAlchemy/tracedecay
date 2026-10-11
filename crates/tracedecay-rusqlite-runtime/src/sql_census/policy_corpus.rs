//! Generates `crates/tracedecay-sql/tests/testdata/policy.tsv`: the SQLite
//! authorizers' prepare-time verdict for a statement corpus, per capability.
//! The test writes `policy.tsv` and `policy-fixture.sql` to
//! `$TEST_UNDECLARED_OUTPUTS_DIR`; `scripts/storage/sql-census.sh` copies them.

use rusqlite::{Connection, ErrorCode};

use super::{CensusCapability, escape};
use crate::{
    connection::{authorize_maintenance, authorize_reader, authorize_writer},
    exact_sql::{ExactSqlError, with_exact_sql_guard},
};

/// Objects every corpus statement may name. Temp objects let the corpus
/// address temp drops and temp indexes.
const FIXTURE: &[&str] = &[
    "CREATE TABLE t(id INTEGER PRIMARY KEY, a TEXT, b INTEGER)",
    "CREATE INDEX t_a ON t(a)",
    "CREATE TABLE u(id INTEGER PRIMARY KEY, t_id INTEGER REFERENCES t(id))",
    "CREATE VIEW v AS SELECT id, a FROM t",
    "CREATE TRIGGER t_ai AFTER INSERT ON t BEGIN UPDATE t SET b = 0 WHERE id = new.id AND b IS NULL; END",
    "CREATE VIRTUAL TABLE f USING fts5(body)",
    "CREATE TEMP TABLE tf(x)",
    "CREATE INDEX temp.tf_x ON tf(x)",
    "CREATE TEMP VIEW tf_v AS SELECT x FROM tf",
    "CREATE TEMP TRIGGER tf_tr AFTER INSERT ON tf BEGIN SELECT 1; END",
];

const CORPUS: &[&str] = &[
    // Queries, CTEs, explain, table-valued functions.
    "SELECT a FROM t WHERE id = ?1",
    "SELECT * FROM v",
    "SELECT count(*) FROM sqlite_schema",
    "SELECT name FROM sqlite_temp_schema",
    "SELECT rowid FROM f WHERE f MATCH 'x'",
    "SELECT value FROM json_each('[1,2]')",
    "SELECT key FROM json_tree('{\"a\":1}')",
    "SELECT name FROM pragma_table_info('t')",
    "SELECT name FROM pragma_index_list('t')",
    "WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM c WHERE x < 3) SELECT x FROM c",
    "EXPLAIN SELECT 1",
    "EXPLAIN QUERY PLAN SELECT a FROM t WHERE a = 'x'",
    "SELECT sqlite_version()",
    "SELECT randomblob(4)",
    "SELECT a FROM t WHERE a LIKE 'x%' ESCAPE '\\'",
    // Functions and virtual tables outside the policy.
    "SELECT load_extension('x')",
    "SELECT load_extension('x', 'entry')",
    "SELECT * FROM sqlite_dbpage('main')",
    "SELECT * FROM dbstat",
    "SELECT * FROM btree_dump",
    "SELECT * FROM turso_types",
    // DML.
    "INSERT INTO t(a, b) VALUES ('x', 1)",
    "INSERT INTO t(a, b) VALUES ('x', 1) ON CONFLICT(id) DO UPDATE SET b = excluded.b",
    "INSERT OR REPLACE INTO t(id, a) VALUES (1, 'y')",
    "INSERT INTO t(a) VALUES ('x') RETURNING id",
    "INSERT INTO t(a) SELECT a FROM t",
    "REPLACE INTO u(id, t_id) VALUES (1, 1)",
    "UPDATE t SET a = 'z' WHERE id = 1",
    "UPDATE t SET a = 'z' WHERE id = 1 RETURNING a",
    "DELETE FROM t WHERE id = 1",
    "DELETE FROM u",
    "INSERT INTO f(body) VALUES ('hello')",
    "INSERT INTO f(f) VALUES ('optimize')",
    "DELETE FROM f WHERE rowid = 1",
    "INSERT INTO tf(x) VALUES (1)",
    "UPDATE sqlite_schema SET sql = '' WHERE name = 't'",
    // Durable DDL.
    "CREATE TABLE n(x)",
    "CREATE TABLE IF NOT EXISTS t(id INTEGER PRIMARY KEY)",
    "CREATE TABLE n2 AS SELECT * FROM t",
    "CREATE TABLE n3(x NOT NULL PRIMARY KEY) WITHOUT ROWID",
    "CREATE TABLE n4(x INTEGER PRIMARY KEY, y TEXT NOT NULL, UNIQUE(y)) STRICT",
    "CREATE INDEX n_b ON t(b)",
    "CREATE UNIQUE INDEX IF NOT EXISTS t_a_unique ON t(a)",
    "CREATE INDEX n_partial ON t(b) WHERE b IS NOT NULL",
    "CREATE TRIGGER n_tr AFTER DELETE ON t BEGIN DELETE FROM u WHERE t_id = old.id; END",
    "CREATE TRIGGER IF NOT EXISTS n_guard BEFORE UPDATE ON t BEGIN SELECT RAISE(ABORT, 'immutable'); END",
    "CREATE TRIGGER main.n_main AFTER INSERT ON t BEGIN SELECT 1; END",
    "CREATE VIEW n_v AS SELECT a FROM t",
    "CREATE VIEW IF NOT EXISTS main.n_v2 AS SELECT a FROM t",
    "CREATE VIRTUAL TABLE n_f USING fts5(body)",
    "CREATE VIRTUAL TABLE n_f2 USING fts5(body, content='t', content_rowid='id')",
    "ALTER TABLE t ADD COLUMN c TEXT",
    "ALTER TABLE t RENAME TO t_renamed",
    "ALTER TABLE t RENAME COLUMN a TO a2",
    "ALTER TABLE u DROP COLUMN t_id",
    "DROP TABLE u",
    "DROP TABLE IF EXISTS missing",
    "DROP INDEX t_a",
    "DROP INDEX IF EXISTS missing_index",
    "DROP TRIGGER t_ai",
    "DROP TRIGGER main.t_ai",
    "DROP VIEW v",
    "DROP VIEW main.v",
    "DROP TABLE f",
    "ANALYZE",
    "ANALYZE t",
    "REINDEX t",
    "VACUUM",
    "VACUUM INTO 'policy-corpus-never-written.db'",
    // Temp DDL.
    "CREATE TEMP TABLE n_tt(x)",
    "CREATE TEMPORARY TABLE n_tt2 AS SELECT * FROM t",
    "CREATE TABLE temp.n_tt3(x)",
    "CREATE TEMP TABLE IF NOT EXISTS tf(x)",
    "CREATE INDEX temp.n_tt_x ON tf(x)",
    "CREATE TEMP TRIGGER n_ttr AFTER INSERT ON t BEGIN SELECT 1; END",
    "CREATE TRIGGER temp.n_ttr2 AFTER INSERT ON tf BEGIN SELECT 1; END",
    "CREATE TEMP VIEW n_tv AS SELECT 1",
    "CREATE VIEW temp.n_tv2 AS SELECT 1",
    "DROP TABLE temp.tf",
    "DROP TABLE tf",
    "DROP INDEX temp.tf_x",
    "DROP TRIGGER temp.tf_tr",
    "DROP TRIGGER tf_tr",
    "DROP VIEW temp.tf_v",
    "DROP VIEW tf_v",
    // Attachment.
    "ATTACH DATABASE ':memory:' AS aux",
    "ATTACH DATABASE ?1 AS aux",
    "ATTACH 'file:policy-corpus?mode=memory' AS aux",
    "DETACH DATABASE aux",
    "DETACH aux",
    // Transaction control.
    "BEGIN",
    "BEGIN DEFERRED",
    "BEGIN IMMEDIATE",
    "BEGIN EXCLUSIVE",
    "COMMIT",
    "END",
    "ROLLBACK",
    "SAVEPOINT sp",
    "RELEASE sp",
    "RELEASE SAVEPOINT sp",
    "ROLLBACK TO sp",
    "ROLLBACK TO SAVEPOINT sp",
    // Pragmas in use, plus protected ones.
    "PRAGMA application_id",
    "PRAGMA application_id = 1413762609",
    "PRAGMA auto_vacuum",
    "PRAGMA auto_vacuum = INCREMENTAL",
    "PRAGMA auto_vacuum = 2",
    "PRAGMA auto_vacuum = NONE",
    "PRAGMA busy_timeout",
    "PRAGMA busy_timeout = 5000",
    "PRAGMA cache_flush",
    "PRAGMA cache_size",
    "PRAGMA cache_size = -2000",
    "PRAGMA case_sensitive_like = ON",
    "PRAGMA cell_size_check = ON",
    "PRAGMA collation_list",
    "PRAGMA compile_options",
    "PRAGMA data_version",
    "PRAGMA database_list",
    "PRAGMA defer_foreign_keys",
    "PRAGMA defer_foreign_keys = ON",
    "PRAGMA defer_foreign_keys = OFF",
    "PRAGMA foreign_key_check",
    "PRAGMA foreign_key_check(u)",
    "PRAGMA foreign_key_list(u)",
    "PRAGMA foreign_keys",
    "PRAGMA foreign_keys = ON",
    "PRAGMA foreign_keys = OFF",
    "PRAGMA freelist_count",
    "PRAGMA function_list",
    "PRAGMA hard_heap_limit = 0",
    "PRAGMA ignore_check_constraints = ON",
    "PRAGMA incremental_vacuum",
    "PRAGMA incremental_vacuum(10)",
    "PRAGMA index_info(t_a)",
    "PRAGMA index_list(t)",
    "PRAGMA index_xinfo(t_a)",
    "PRAGMA integrity_check",
    "PRAGMA integrity_check(1)",
    "PRAGMA journal_mode",
    "PRAGMA journal_mode = WAL",
    "PRAGMA journal_mode = DELETE",
    "PRAGMA journal_size_limit",
    "PRAGMA journal_size_limit = 0",
    "PRAGMA locking_mode = EXCLUSIVE",
    "PRAGMA main.user_version",
    "PRAGMA mmap_size",
    "PRAGMA mmap_size = 0",
    "PRAGMA module_list",
    "PRAGMA optimize",
    "PRAGMA page_count",
    "PRAGMA page_size",
    "PRAGMA page_size = 4096",
    "PRAGMA pragma_list",
    "PRAGMA query_only",
    "PRAGMA query_only = ON",
    "PRAGMA query_only = OFF",
    "PRAGMA quick_check",
    "PRAGMA quick_check(1)",
    "PRAGMA recursive_triggers = ON",
    "PRAGMA schema_version",
    "PRAGMA schema_version = 1",
    "PRAGMA secure_delete",
    "PRAGMA secure_delete = ON",
    "PRAGMA secure_delete = OFF",
    "PRAGMA secure_delete = FAST",
    "PRAGMA shrink_memory",
    "PRAGMA soft_heap_limit = 0",
    "PRAGMA synchronous",
    "PRAGMA synchronous = NORMAL",
    "PRAGMA synchronous = OFF",
    "PRAGMA table_info(t)",
    "PRAGMA table_list",
    "PRAGMA table_xinfo(t)",
    "PRAGMA temp.table_info(tf)",
    "PRAGMA temp_store",
    "PRAGMA temp_store = MEMORY",
    "PRAGMA threads",
    "PRAGMA threads = 4",
    "PRAGMA trusted_schema",
    "PRAGMA trusted_schema = OFF",
    "PRAGMA user_version",
    "PRAGMA user_version = 7",
    "PRAGMA wal_autocheckpoint",
    "PRAGMA wal_autocheckpoint = 1000",
    "PRAGMA wal_checkpoint",
    "PRAGMA wal_checkpoint(PASSIVE)",
    "PRAGMA wal_checkpoint(FULL)",
    "PRAGMA wal_checkpoint(RESTART)",
    "PRAGMA wal_checkpoint(TRUNCATE)",
    "PRAGMA writable_schema",
    "PRAGMA writable_schema = ON",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Verdict {
    Allow,
    Deny,
    /// SQLite refused the statement for a reason other than the authorizer,
    /// for example an unresolvable module or a statement it cannot prepare.
    Error,
}

impl Verdict {
    fn name(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Deny => "deny",
            Self::Error => "error",
        }
    }

    fn of(result: rusqlite::Result<()>) -> Self {
        match result {
            Ok(()) => Self::Allow,
            // A denied function reports SQLITE_ERROR "not authorized to use
            // function", not SQLITE_AUTH.
            Err(error)
                if error.sqlite_error_code()
                    == Some(ErrorCode::AuthorizationForStatementDenied)
                    || error.to_string().contains("not authorized") =>
            {
                Self::Deny
            }
            Err(_) => Self::Error,
        }
    }
}

fn fixture_connection() -> Connection {
    let connection = Connection::open_in_memory().unwrap();
    for statement in FIXTURE {
        connection.execute_batch(statement).unwrap();
    }
    connection
}

fn prepare(connection: &Connection, sql: &str) -> rusqlite::Result<()> {
    connection.prepare(sql).map(drop)
}

/// The verdict of the production authorizer stack for `capability`: the
/// connection-mode authorizer, or the exact-SQL guard as the exact-SQL writer
/// lane installs it.
fn verdict(connection: &Connection, capability: CensusCapability, sql: &str) -> Verdict {
    let canonical = match capability {
        CensusCapability::Reader => authorize_reader,
        CensusCapability::Writer => authorize_writer,
        CensusCapability::Maintenance => authorize_maintenance,
        CensusCapability::ExactSql | CensusCapability::ExactSqlSavepoints => {
            let guarded = with_exact_sql_guard(
                connection,
                capability == CensusCapability::ExactSqlSavepoints,
                false,
                None,
                None,
                false,
                None,
                authorize_writer,
                true,
                || Ok(prepare(connection, sql)),
            );
            return match guarded {
                Ok(result) => Verdict::of(result),
                Err(ExactSqlError::TransactionControlDenied) => Verdict::Deny,
                Err(error) => panic!("exact SQL guard failed for {sql:?}: {error:?}"),
            };
        }
    };
    connection.authorizer(Some(canonical)).unwrap();
    let verdict = Verdict::of(prepare(connection, sql));
    connection
        .authorizer(None::<fn(rusqlite::hooks::AuthContext<'_>) -> rusqlite::hooks::Authorization>)
        .unwrap();
    verdict
}

fn render() -> (String, Vec<[Verdict; 5]>) {
    let connections = CensusCapability::ALL.map(|_| fixture_connection());
    let mut tsv = CensusCapability::ALL
        .iter()
        .map(|capability| capability.name())
        .chain(["sql"])
        .collect::<Vec<_>>()
        .join("\t");
    tsv.push('\n');
    let mut verdicts = Vec::with_capacity(CORPUS.len());
    for sql in CORPUS {
        let row = std::array::from_fn(|index| {
            verdict(&connections[index], CensusCapability::ALL[index], sql)
        });
        for verdict in row {
            tsv.push_str(verdict.name());
            tsv.push('\t');
        }
        tsv.push_str(&escape(sql));
        tsv.push('\n');
        verdicts.push(row);
    }
    (tsv, verdicts)
}

fn verdicts_for<'a>(verdicts: &'a [[Verdict; 5]], sql: &str) -> &'a [Verdict; 5] {
    let index = CORPUS
        .iter()
        .position(|candidate| *candidate == sql)
        .unwrap();
    &verdicts[index]
}

#[test]
fn sql_policy_corpus_records_the_sqlite_authorizer_verdicts() {
    let (tsv, verdicts) = render();
    use Verdict::{Allow, Deny};

    // Spot checks pin the generator to the authorizers it claims to record:
    // a generator that never installed them would report allow everywhere.
    assert_eq!(
        verdicts_for(&verdicts, "ATTACH DATABASE ':memory:' AS aux"),
        &[Deny; 5]
    );
    assert_eq!(
        verdicts_for(&verdicts, "INSERT INTO t(a, b) VALUES ('x', 1)"),
        &[Deny, Allow, Allow, Allow, Allow]
    );
    assert_eq!(
        verdicts_for(
            &verdicts,
            "CREATE TRIGGER n_tr AFTER DELETE ON t BEGIN DELETE FROM u WHERE t_id = old.id; END"
        ),
        &[Deny, Deny, Allow, Allow, Allow]
    );
    assert_eq!(
        verdicts_for(
            &verdicts,
            "CREATE TEMP TRIGGER n_ttr AFTER INSERT ON t BEGIN SELECT 1; END"
        ),
        &[Deny, Deny, Deny, Deny, Allow]
    );
    assert_eq!(
        verdicts_for(&verdicts, "SAVEPOINT sp"),
        &[Allow, Allow, Deny, Allow, Allow]
    );
    assert_eq!(
        verdicts_for(&verdicts, "SELECT load_extension('x')"),
        &[Deny, Deny, Deny, Deny, Allow]
    );

    if let Some(dir) = std::env::var_os("TEST_UNDECLARED_OUTPUTS_DIR") {
        let dir = std::path::PathBuf::from(dir);
        std::fs::write(dir.join("policy.tsv"), tsv).unwrap();
        let mut fixture = FIXTURE.join(";\n");
        fixture.push_str(";\n");
        std::fs::write(dir.join("policy-fixture.sql"), fixture).unwrap();
    }
}
