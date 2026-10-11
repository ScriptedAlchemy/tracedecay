//! Guard parity between the SQLite authorizers and the native engine (#3402).
//!
//! - `policy.tsv` holds the SQLite arm's prepare-time verdict per capability
//!   for a statement corpus, over the objects in `policy-fixture.sql`. The
//!   fixture holds only objects both engines can create: a temp trigger, which
//!   native refuses, would make SQLite deny a cascading `DROP TABLE` for a
//!   reason native cannot reproduce.
//! - `census.tsv` holds every distinct statement the owned-store suites ran on
//!   SQLite, with the capability it ran under. Its `sqlite_replay` column says
//!   how far SQLite gets with the statement on the census's own replayed DDL:
//!   `runs`, `prepares` (stepping fails), or `missing` (it names objects that
//!   union lost, so it is not a native gap).
//!
//! `scripts/storage/sql-census.sh` regenerates the TSVs. Each test that
//! compares against a file writes the observed version to
//! `$TEST_UNDECLARED_OUTPUTS_DIR` under the same name.

use std::{
    collections::BTreeSet,
    time::{Duration, Instant},
};

use tracedecay_sql::{
    Capability, Connection, ConnectionControl, Error, Statement, params_from_iter, types::Value,
};

const CAPABILITIES: [&str; 6] = [
    "reader",
    "exact_sql_read",
    "writer",
    "exact_sql",
    "exact_sql_savepoints",
    "maintenance",
];
const POLICY: &str = include_str!("testdata/policy.tsv");
const POLICY_FIXTURE: &str = include_str!("testdata/policy-fixture.sql");
const CENSUS: &str = include_str!("testdata/census.tsv");
const CENSUS_HEADER: [&str; 3] = ["capability", "sql", "sqlite_replay"];
const STATEMENT_DEADLINE: Duration = Duration::from_secs(10);

/// How far a statement gets on a store.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Reach {
    Missing,
    Prepares,
    Runs,
}

impl Reach {
    fn name(self) -> &'static str {
        match self {
            Self::Missing => "missing",
            Self::Prepares => "prepares",
            Self::Runs => "runs",
        }
    }
}

struct CensusRow {
    capability: String,
    sql: String,
    sqlite: Reach,
}

fn capability(name: &str) -> Capability {
    match name {
        "reader" => Capability::Reader,
        "exact_sql_read" => Capability::ExactSqlRead,
        "writer" => Capability::Writer,
        "exact_sql" => Capability::ExactSql { savepoints: false },
        "exact_sql_savepoints" => Capability::ExactSql { savepoints: true },
        "maintenance" => Capability::Maintenance,
        other => panic!("unknown capability {other:?}"),
    }
}

fn unescape(field: &str) -> String {
    let mut unescaped = String::with_capacity(field.len());
    let mut characters = field.chars();
    while let Some(character) = characters.next() {
        if character != '\\' {
            unescaped.push(character);
            continue;
        }
        match characters.next() {
            Some('\\') => unescaped.push('\\'),
            Some('t') => unescaped.push('\t'),
            Some('n') => unescaped.push('\n'),
            Some('r') => unescaped.push('\r'),
            other => panic!("invalid escape \\{other:?} in {field:?}"),
        }
    }
    unescaped
}

fn escape(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace('\t', "\\t")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
}

/// Rows of `tsv` under exactly `header`, every field unescaped.
fn rows(name: &str, tsv: &str, header: &[&str]) -> Vec<Vec<String>> {
    let mut lines = tsv.lines();
    assert_eq!(
        lines.next(),
        Some(header.join("\t").as_str()),
        "{name}: header"
    );
    lines
        .map(|line| {
            let fields: Vec<String> = line.split('\t').map(unescape).collect();
            assert_eq!(fields.len(), header.len(), "{name}: row {line:?}");
            fields
        })
        .collect()
}

fn policy() -> Vec<(Vec<String>, String)> {
    let header: Vec<&str> = CAPABILITIES.iter().copied().chain(["sql"]).collect();
    rows("policy.tsv", POLICY, &header)
        .into_iter()
        .map(|mut row| {
            let sql = row.pop().unwrap();
            (row, sql)
        })
        .collect()
}

fn census() -> Vec<CensusRow> {
    rows("census.tsv", CENSUS, &CENSUS_HEADER)
        .into_iter()
        .map(|row| {
            let [capability, sql, replay]: [String; 3] = row.try_into().unwrap();
            let sqlite = [Reach::Missing, Reach::Prepares, Reach::Runs]
                .into_iter()
                .find(|reach| reach.name() == replay)
                .unwrap_or_else(|| panic!("census.tsv sqlite_replay {replay:?} for {sql:?}"));
            CensusRow {
                capability,
                sql,
                sqlite,
            }
        })
        .collect()
}

#[cfg(not(td_engine = "native"))]
fn write_observed(name: &str, contents: &str) {
    if let Some(dir) = std::env::var_os("TEST_UNDECLARED_OUTPUTS_DIR") {
        std::fs::write(std::path::Path::new(&dir).join(name), contents).unwrap();
    }
}

/// `sql` from its first keyword, past leading whitespace and comments.
fn statement_text(sql: &str) -> &str {
    let mut rest = sql.trim_start();
    loop {
        let after = if let Some(comment) = rest.strip_prefix("--") {
            comment.split_once('\n').map_or("", |(_, after)| after)
        } else if let Some(comment) = rest.strip_prefix("/*") {
            comment.split_once("*/").map_or("", |(_, after)| after)
        } else {
            return rest;
        };
        rest = after.trim_start();
    }
}

fn is_schema(sql: &str) -> bool {
    let upper = statement_text(sql).to_ascii_uppercase();
    (upper.starts_with("CREATE ")
        && !upper.starts_with("CREATE TEMP ")
        && !upper.starts_with("CREATE TEMPORARY "))
        || (upper.starts_with("ALTER TABLE ") && upper.contains(" ADD "))
}

/// Transaction control cannot nest, and the writer checkpoints only outside a
/// transaction, so both run bare rather than inside the isolating one.
fn runs_bare(sql: &str) -> bool {
    let upper = statement_text(sql).to_ascii_uppercase();
    let keyword = upper
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .trim_end_matches(';');
    ["BEGIN", "COMMIT", "END", "ROLLBACK", "SAVEPOINT", "RELEASE"].contains(&keyword)
        || upper.starts_with("PRAGMA WAL_CHECKPOINT")
}

/// A fresh in-memory store with `ddl` installed until no statement makes
/// progress, so the order of the suites' stage installs does not matter.
fn replayed_store(ddl: &[&str]) -> Connection {
    let connection = Connection::open_in_memory().unwrap();
    connection.set_capability(Capability::Maintenance).unwrap();
    let mut pending = ddl.to_vec();
    loop {
        let before = pending.len();
        pending.retain(|sql| connection.execute_batch(sql).is_err());
        if pending.len() == before {
            return connection;
        }
    }
}

fn policy_fixture() -> Connection {
    let fixture: Vec<&str> = POLICY_FIXTURE
        .split(";\n")
        .filter(|sql| !sql.trim().is_empty())
        .collect();
    replayed_store(&fixture)
}

/// The census's schema statements, longest first. Suites create one table at
/// several schema stages; the longest definition is the latest stage, so it
/// wins and later statements on its columns prepare.
fn census_ddl(census: &[CensusRow]) -> Vec<&str> {
    let mut ddl: Vec<&str> = census
        .iter()
        .map(|row| row.sql.as_str())
        .filter(|sql| is_schema(sql))
        .collect();
    ddl.sort_by_key(|sql| std::cmp::Reverse(sql.len()));
    ddl
}

fn drain(
    statement: &mut Statement<'_>,
    parameters: usize,
    value: Value,
) -> tracedecay_sql::Result<()> {
    let mut rows = statement.query(params_from_iter(std::iter::repeat_n(value, parameters)))?;
    while rows.next()?.is_some() {}
    Ok(())
}

/// Prepares `sql` and steps it to completion under whatever capability
/// `connection` holds, with NULL and then 1 bound to every parameter. A
/// statement that outlives `STATEMENT_DEADLINE` is interrupted and counts as
/// prepared.
fn attempt(connection: &Connection, sql: &str) -> (Reach, Option<String>) {
    let mut statement = match connection.prepare(sql) {
        Ok(statement) => statement,
        Err(error) => return (Reach::Missing, Some(error.to_string())),
    };
    let deadline = Instant::now() + STATEMENT_DEADLINE;
    let ran = connection
        .guard_scope(
            move || Instant::now() > deadline,
            |_| {
                let parameters = match drain(&mut statement, 0, Value::Null) {
                    Err(Error::InvalidParameterCount(_, expected)) => expected,
                    ran => return ran,
                };
                drain(&mut statement, parameters, Value::Null)
                    .or_else(|_| drain(&mut statement, parameters, Value::Integer(1)))
            },
        )
        .and_then(|ran| ran);
    match ran {
        Ok(()) => (Reach::Runs, None),
        Err(error) => (Reach::Prepares, Some(error.to_string())),
    }
}

/// `attempt` under `capability`, then rolled back so the next statement sees
/// the replayed store, unless the statement [`runs_bare`].
fn isolated(connection: &Connection, capability: Capability, sql: &str) -> (Reach, Option<String>) {
    if !runs_bare(sql)
        && let Err(error) = connection.execute_batch("BEGIN")
    {
        return (Reach::Missing, Some(format!("isolating BEGIN: {error}")));
    }
    connection.set_capability(capability).unwrap();
    let outcome = attempt(connection, sql);
    connection.set_capability(Capability::Maintenance).unwrap();
    if !connection.is_autocommit()
        && let Err(error) = connection.execute_batch("ROLLBACK")
    {
        return (Reach::Missing, Some(format!("isolating ROLLBACK: {error}")));
    }
    outcome
}

#[test]
fn policy_corpus_is_well_formed() {
    let policy = policy();
    assert!(!policy.is_empty(), "policy.tsv has no statements");
    let mut statements = BTreeSet::new();
    for (verdicts, sql) in &policy {
        assert!(!sql.trim().is_empty(), "policy.tsv has an empty statement");
        assert!(statements.insert(sql), "policy.tsv repeats {sql:?}");
        for verdict in verdicts {
            assert!(
                ["allow", "deny", "error"].contains(&verdict.as_str()),
                "policy.tsv verdict {verdict:?} for {sql:?}"
            );
        }
    }
    for (index, capability) in CAPABILITIES.iter().enumerate() {
        for verdict in ["allow", "deny"] {
            assert!(
                policy
                    .iter()
                    .any(|(verdicts, _)| verdicts[index] == verdict),
                "policy.tsv never records {verdict} for {capability}"
            );
        }
    }
    assert!(
        POLICY_FIXTURE.contains("CREATE TABLE"),
        "policy-fixture.sql installs no tables"
    );
}

#[test]
fn census_is_well_formed() {
    let census = census();
    assert!(!census.is_empty(), "census.tsv has no statements");
    for row in &census {
        assert!(
            CAPABILITIES.contains(&row.capability.as_str()),
            "census.tsv capability {:?}",
            row.capability
        );
        assert!(
            !row.sql.trim().is_empty(),
            "census.tsv has an empty statement"
        );
    }
    let keys: Vec<&str> = CENSUS
        .lines()
        .skip(1)
        .map(|line| line.rsplit_once('\t').unwrap().0)
        .collect();
    assert!(
        keys.windows(2).all(|pair| pair[0] < pair[1]),
        "census.tsv must be sorted and unique by capability and statement"
    );
    for name in ["reader", "writer", "exact_sql", "maintenance"] {
        assert!(
            census
                .iter()
                .any(|row| row.capability == name && row.sqlite == Reach::Runs),
            "no {name} census statement runs on its replayed schema"
        );
    }
}

/// `policy.tsv` is a function of its statement column and the SQLite arm's
/// authorizers, recomputed here. An authorizer change without regenerating
/// the record fails.
#[cfg(not(td_engine = "native"))]
#[test]
fn policy_tsv_records_the_sqlite_verdicts() {
    use tracedecay_sql::{ErrorExt, ErrorKind};

    let store = policy_fixture();
    let mut observed = CAPABILITIES
        .iter()
        .copied()
        .chain(["sql"])
        .collect::<Vec<_>>()
        .join("\t");
    observed.push('\n');
    for line in POLICY.lines().skip(1) {
        let sql = unescape(line.rsplit('\t').next().unwrap());
        for name in CAPABILITIES {
            store.set_capability(capability(name)).unwrap();
            observed.push_str(match store.prepare(&sql) {
                Ok(_) => "allow",
                // A denied function reports "not authorized to use function"
                // as a plain SQLITE_ERROR.
                Err(error)
                    if matches!(
                        error.kind(),
                        ErrorKind::Denied | ErrorKind::TransactionControlDenied
                    ) || error.to_string().contains("not authorized") =>
                {
                    "deny"
                }
                Err(_) => "error",
            });
            observed.push('\t');
        }
        store.set_capability(Capability::Maintenance).unwrap();
        observed.push_str(&escape(&sql));
        observed.push('\n');
    }
    write_observed("policy.tsv", &observed);
    let stale: Vec<&str> = observed
        .lines()
        .zip(POLICY.lines())
        .filter(|(observed, recorded)| observed != recorded)
        .map(|(observed, _)| observed)
        .collect();
    assert!(
        stale.is_empty(),
        "policy.tsv is stale for {} rows; the SQLite arm now says:\n{}",
        stale.len(),
        stale.join("\n")
    );
}

/// The `sqlite_replay` column is a function of `census.tsv` alone, recomputed
/// here on SQLite under the default access. A census edited without
/// recomputing it fails.
#[cfg(not(td_engine = "native"))]
#[test]
fn census_sqlite_replay_column_is_current() {
    let census = census();
    let store = replayed_store(&census_ddl(&census));
    let mut observed = CENSUS_HEADER.join("\t");
    observed.push('\n');
    let mut stale = Vec::new();
    for row in &census {
        let (reach, _) = isolated(&store, Capability::Maintenance, &row.sql);
        if reach != row.sqlite {
            stale.push(format!(
                "{}\t{}\t{} -> {}",
                row.capability,
                row.sql,
                row.sqlite.name(),
                reach.name()
            ));
        }
        observed.push_str(&format!(
            "{}\t{}\t{}\n",
            row.capability,
            escape(&row.sql),
            reach.name()
        ));
    }
    write_observed("census.tsv", &observed);
    assert!(
        stale.is_empty(),
        "census.tsv sqlite_replay is stale for {} rows:\n{}",
        stale.len(),
        stale.join("\n")
    );
}

#[cfg(td_engine = "native")]
mod native {
    use std::collections::BTreeSet;

    use tracedecay_sql::{Capability, Connection, ConnectionControl, types::Value};

    use super::{
        CAPABILITIES, CensusRow, Reach, attempt, capability, census, census_ddl, escape, isolated,
        policy, policy_fixture, replayed_store, statement_text,
    };

    /// The identifiers `sql` names, lowercased, whether quoted or not.
    fn identifiers(sql: &str) -> impl Iterator<Item = String> + '_ {
        sql.split(|character: char| !(character.is_ascii_alphanumeric() || character == '_'))
            .filter(|word| !word.is_empty())
            .map(str::to_ascii_lowercase)
    }

    /// An FTS5 table of the SQLite search shape. A native store has a
    /// `USING fts` index of the same name on the external-content table.
    struct Fts5Table {
        name: String,
        content: Option<String>,
        columns: Vec<String>,
    }

    impl Fts5Table {
        fn parse(sql: &str) -> Option<Self> {
            let text = statement_text(sql);
            let upper = text.to_ascii_uppercase();
            if !upper.starts_with("CREATE VIRTUAL TABLE") {
                return None;
            }
            let using = upper.find("USING FTS5")?;
            let open = using + upper[using..].find('(')?;
            let close = text.rfind(')')?;
            let mut table = Self {
                name: identifiers(&text[..using]).last()?,
                content: None,
                columns: Vec::new(),
            };
            for argument in text[open + 1..close].split(',') {
                match argument.split_once('=') {
                    Some((option, value)) if option.trim().eq_ignore_ascii_case("content") => {
                        let value = value.trim().trim_matches(['\'', '"']);
                        table.content = (!value.is_empty()).then(|| value.to_owned());
                    }
                    Some(_) => {}
                    None if argument.to_ascii_uppercase().contains("UNINDEXED") => {}
                    None => table
                        .columns
                        .extend(argument.split_whitespace().next().map(str::to_owned)),
                }
            }
            Some(table)
        }

        /// The table and its FTS5 shadow tables.
        fn objects(&self) -> impl Iterator<Item = String> + '_ {
            ["", "_data", "_idx", "_content", "_docsize", "_config"]
                .into_iter()
                .map(|suffix| format!("{}{suffix}", self.name))
        }

        /// The native index that replaces an external-content table. A
        /// contentful FTS5 table has no native counterpart.
        fn native_index(&self) -> Option<String> {
            let content = self.content.as_ref()?;
            Some(format!(
                "CREATE INDEX IF NOT EXISTS {} ON {content} USING fts ({})",
                self.name,
                self.columns.join(", ")
            ))
        }
    }

    /// The census's SQLite search shape.
    struct SearchShape {
        tables: Vec<Fts5Table>,
        /// The FTS5 tables, their shadow tables, and the triggers and views
        /// over them.
        objects: BTreeSet<String>,
    }

    impl SearchShape {
        fn of(census: &[CensusRow]) -> Self {
            let tables: Vec<Fts5Table> = census
                .iter()
                .filter_map(|row| Fts5Table::parse(&row.sql))
                .collect();
            let mut shape = Self {
                objects: tables.iter().flat_map(Fts5Table::objects).collect(),
                tables,
            };
            let dependents: Vec<String> = census
                .iter()
                .filter_map(|row| shape.dependent(&row.sql))
                .collect();
            shape.objects.extend(dependents);
            shape
        }

        fn names_fts5(&self, names: &[String]) -> bool {
            names
                .iter()
                .any(|name| name == "fts5" || self.objects.contains(name))
        }

        /// The trigger or view `sql` creates over an FTS5 object.
        fn dependent(&self, sql: &str) -> Option<String> {
            let names: Vec<String> = identifiers(statement_text(sql)).collect();
            let mut words = names.iter().map(String::as_str);
            if words.next()? != "create" {
                return None;
            }
            let mut kind = words.next()?;
            if matches!(kind, "temp" | "temporary") {
                kind = words.next()?;
            }
            if !matches!(kind, "trigger" | "view") {
                return None;
            }
            let mut name = words.next()?;
            if name == "if" {
                name = words.nth(2)?;
            }
            self.names_fts5(&names).then(|| name.to_owned())
        }

        /// `ddl` as a native store installs it: each external-content FTS5
        /// table becomes its native index, and the sync triggers and views
        /// over FTS5 objects are not installed.
        fn native_ddl(&self, ddl: Vec<&str>) -> Vec<String> {
            ddl.into_iter()
                .filter_map(|sql| match Fts5Table::parse(sql) {
                    Some(table) => table.native_index(),
                    None if self.dependent(sql).is_some() => None,
                    None => Some(sql.to_owned()),
                })
                .collect()
        }
    }

    /// SQLite-only statement shapes the cutover drops on purpose (#3402
    /// approved deltas). The native arms never issue them.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
    enum ApprovedDelta {
        /// FTS5 tables, their shadow tables and sync triggers.
        FtsShape,
        /// `dbstat`, and the NOOP checkpoint the native arm answers from the
        /// engine's WAL frame counts.
        SqliteIntrospectionRemoved,
        /// `secure_delete`: native deletes logically, without page scrubbing.
        SecureDelete,
    }

    impl ApprovedDelta {
        const ALL: [Self; 3] = [
            Self::FtsShape,
            Self::SqliteIntrospectionRemoved,
            Self::SecureDelete,
        ];

        fn of(sql: &str, search: &SearchShape) -> Option<Self> {
            let names: Vec<String> = identifiers(statement_text(sql)).collect();
            let named = |wanted: &str| names.iter().any(|name| name == wanted);
            let pragma = match names.as_slice() {
                [keyword, pragma, ..] if keyword == "pragma" => pragma.as_str(),
                _ => "",
            };
            if search.names_fts5(&names) {
                Some(Self::FtsShape)
            } else if named("dbstat") || (pragma == "wal_checkpoint" && named("noop")) {
                Some(Self::SqliteIntrospectionRemoved)
            } else if pragma == "secure_delete" {
                Some(Self::SecureDelete)
            } else {
                None
            }
        }
    }

    /// Everything a statement on the policy fixture could change: schema, rows
    /// of every fixture table, attached databases and the settable pragmas.
    const STATE: [(&str, usize); 31] = [
        (
            "SELECT type, name, tbl_name, sql FROM sqlite_schema ORDER BY type, name",
            4,
        ),
        (
            "SELECT type, name, tbl_name, sql FROM sqlite_temp_schema ORDER BY type, name",
            4,
        ),
        ("SELECT id, a, b FROM t ORDER BY id", 3),
        ("SELECT id, t_id FROM u ORDER BY id", 2),
        ("SELECT rowid, body FROM f ORDER BY rowid", 2),
        ("SELECT x FROM tf ORDER BY x", 1),
        ("PRAGMA database_list", 3),
        ("PRAGMA application_id", 1),
        ("PRAGMA auto_vacuum", 1),
        ("PRAGMA busy_timeout", 1),
        ("PRAGMA cache_size", 1),
        ("PRAGMA cell_size_check", 1),
        ("PRAGMA defer_foreign_keys", 1),
        ("PRAGMA foreign_keys", 1),
        ("PRAGMA ignore_check_constraints", 1),
        ("PRAGMA journal_mode", 1),
        ("PRAGMA journal_size_limit", 1),
        ("PRAGMA locking_mode", 1),
        ("PRAGMA mmap_size", 1),
        ("PRAGMA page_size", 1),
        ("PRAGMA query_only", 1),
        ("PRAGMA recursive_triggers", 1),
        ("PRAGMA schema_version", 1),
        ("PRAGMA secure_delete", 1),
        ("PRAGMA synchronous", 1),
        ("PRAGMA temp_store", 1),
        ("PRAGMA threads", 1),
        ("PRAGMA trusted_schema", 1),
        ("PRAGMA user_version", 1),
        ("PRAGMA wal_autocheckpoint", 1),
        ("PRAGMA writable_schema", 1),
    ];

    fn state(connection: &Connection) -> Vec<String> {
        STATE
            .iter()
            .map(|&(sql, columns)| {
                let read = || -> tracedecay_sql::Result<String> {
                    let mut statement = connection.prepare(sql)?;
                    let mut rows = statement.query([])?;
                    let mut rendered = String::new();
                    while let Some(row) = rows.next()? {
                        for column in 0..columns {
                            rendered.push_str(&format!("{:?} ", row.get::<_, Value>(column)?));
                        }
                        rendered.push('\n');
                    }
                    Ok(rendered)
                };
                format!("{sql}: {:?}", read().map_err(|error| error.to_string()))
            })
            .collect()
    }

    /// Zero tolerance: a statement SQLite refused under a capability is
    /// refused natively under it and leaves the store as it was.
    #[test]
    fn every_sqlite_denial_stays_denied_and_has_no_effect() {
        let untouched = state(&policy_fixture());
        let mut violations = Vec::new();
        for (verdicts, sql) in policy() {
            for (name, verdict) in CAPABILITIES.iter().zip(&verdicts) {
                if verdict == "allow" {
                    continue;
                }
                let store = policy_fixture();
                store.set_capability(capability(name)).unwrap();
                let (reach, _) = attempt(&store, &sql);
                store.set_capability(Capability::Maintenance).unwrap();
                if reach == Reach::Runs {
                    violations.push(format!("{name}: SQLite {verdict}, native runs: {sql}"));
                }
                let after = state(&store);
                for (before, after) in untouched.iter().zip(&after) {
                    if before != after {
                        violations.push(format!(
                            "{name}: SQLite {verdict}, native changed the store: {sql}\n  before {before}\n  after  {after}"
                        ));
                    }
                }
            }
        }
        assert!(
            violations.is_empty(),
            "native lets through statements SQLite refused:\n{}",
            violations.join("\n")
        );
    }

    /// Zero tolerance: every census statement gets as far natively, under its
    /// capability, as it did on SQLite, over the census schema as a native
    /// store installs it. An approved delta is the only exception, and only
    /// when native refuses it at prepare.
    #[test]
    fn every_census_statement_runs_under_its_capability() {
        let census = census();
        let search = SearchShape::of(&census);
        let ddl = search.native_ddl(census_ddl(&census));
        let store = replayed_store(&ddl.iter().map(String::as_str).collect::<Vec<_>>());
        for table in search.tables.iter().filter(|table| table.content.is_some()) {
            assert_eq!(
                store
                    .query_row(
                        "SELECT count(*) FROM sqlite_schema WHERE type = 'index' AND name = ?1",
                        [table.name.as_str()],
                        |row| row.get::<_, i64>(0),
                    )
                    .unwrap(),
                1,
                "the replay lacks the native index for {}: {:?}",
                table.name,
                table.native_index()
            );
        }

        let mut excused = BTreeSet::new();
        let mut failing = Vec::new();
        for row in census.iter().filter(|row| row.sqlite > Reach::Missing) {
            let (reach, error) = isolated(&store, capability(&row.capability), &row.sql);
            if reach >= row.sqlite {
                continue;
            }
            match ApprovedDelta::of(&row.sql, &search) {
                Some(delta) if reach == Reach::Missing => {
                    excused.insert(delta);
                }
                _ => failing.push(format!(
                    "{}\t{}\tSQLite {}, native {}: {}",
                    row.capability,
                    escape(&row.sql),
                    row.sqlite.name(),
                    reach.name(),
                    escape(
                        &error
                            .unwrap_or_default()
                            .chars()
                            .take(200)
                            .collect::<String>()
                    )
                )),
            }
        }
        assert!(
            failing.is_empty(),
            "{} census statements get less far natively than on SQLite:\n{}",
            failing.len(),
            failing.join("\n")
        );
        let unused: Vec<ApprovedDelta> = ApprovedDelta::ALL
            .into_iter()
            .filter(|delta| !excused.contains(delta))
            .collect();
        assert!(
            unused.is_empty(),
            "approved deltas that excuse no census statement, delete them: {unused:?}"
        );
    }
}
