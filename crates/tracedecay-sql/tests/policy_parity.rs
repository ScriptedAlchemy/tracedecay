//! Guard parity between the SQLite authorizers and the native engine (#3402).
//!
//! - `policy.tsv` holds the SQLite authorizers' prepare-time verdict per
//!   capability for a statement corpus, over the objects in
//!   `policy-fixture.sql`.
//! - `census.tsv` holds every distinct statement the owned-store suites ran on
//!   SQLite, with the capability it ran under. Its `sqlite_replay` column says
//!   how far SQLite gets with the statement on the census's own replayed DDL,
//!   NULL bound to every parameter: `runs`, `prepares` (stepping fails), or
//!   `missing` (it names objects that union lost, so it is not a native gap).
//! - `known_native_gaps.tsv` lists the native gate's current failures by
//!   capability and statement (`*` names the capability itself). U12 drives it
//!   to empty.
//!
//! `scripts/storage/sql-census.sh` regenerates all four. Each test that
//! compares against a file writes the observed version to
//! `$TEST_UNDECLARED_OUTPUTS_DIR` under the same name.

use std::{
    collections::BTreeSet,
    time::{Duration, Instant},
};

use tracedecay_sql::{
    Capability, Connection, ConnectionControl, Error, Statement, params_from_iter, types::Value,
};

const CAPABILITIES: [&str; 5] = [
    "reader",
    "writer",
    "exact_sql",
    "exact_sql_savepoints",
    "maintenance",
];
const POLICY: &str = include_str!("testdata/policy.tsv");
const POLICY_FIXTURE: &str = include_str!("testdata/policy-fixture.sql");
const CENSUS: &str = include_str!("testdata/census.tsv");
const CENSUS_HEADER: [&str; 3] = ["capability", "sql", "sqlite_replay"];
const KNOWN_NATIVE_GAPS: &str = include_str!("testdata/known_native_gaps.tsv");
const CAPABILITY_ITSELF: &str = "*";
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

fn known_native_gaps() -> BTreeSet<(String, String)> {
    let mut gaps = BTreeSet::new();
    for row in rows(
        "known_native_gaps.tsv",
        KNOWN_NATIVE_GAPS,
        &["capability", "sql", "reason"],
    ) {
        let [capability, sql, _reason]: [String; 3] = row.try_into().unwrap();
        assert!(
            gaps.insert((capability.clone(), sql.clone())),
            "known_native_gaps.tsv repeats {capability} {sql:?}"
        );
    }
    gaps
}

fn write_observed(name: &str, contents: &str) {
    if let Some(dir) = std::env::var_os("TEST_UNDECLARED_OUTPUTS_DIR") {
        std::fs::write(std::path::Path::new(&dir).join(name), contents).unwrap();
    }
}

fn is_schema(sql: &str) -> bool {
    let upper = sql.trim_start().to_ascii_uppercase();
    (upper.starts_with("CREATE ")
        && !upper.starts_with("CREATE TEMP ")
        && !upper.starts_with("CREATE TEMPORARY "))
        || (upper.starts_with("ALTER TABLE ") && upper.contains(" ADD "))
}

fn is_transaction_control(sql: &str) -> bool {
    let keyword = sql
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .trim_end_matches(';')
        .to_ascii_uppercase();
    ["BEGIN", "COMMIT", "END", "ROLLBACK", "SAVEPOINT", "RELEASE"].contains(&keyword.as_str())
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
/// the replayed store. Transaction control runs bare because it cannot nest.
fn isolated(
    connection: &Connection,
    capability: Option<Capability>,
    sql: &str,
) -> (Reach, Option<String>) {
    if !is_transaction_control(sql)
        && let Err(error) = connection.execute_batch("BEGIN")
    {
        return (Reach::Missing, Some(format!("isolating BEGIN: {error}")));
    }
    if let Some(capability) = capability {
        connection.set_capability(capability).unwrap();
    }
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
    for capability in CAPABILITIES {
        assert!(
            census
                .iter()
                .any(|row| row.capability == capability && row.sqlite == Reach::Runs),
            "no {capability} census statement runs on its replayed schema"
        );
    }
}

#[test]
fn known_native_gaps_name_census_rows() {
    let census: BTreeSet<(String, String)> = census()
        .into_iter()
        .map(|row| (row.capability, row.sql))
        .collect();
    for (capability, sql) in known_native_gaps() {
        assert!(
            CAPABILITIES.contains(&capability.as_str()),
            "known_native_gaps.tsv capability {capability:?}"
        );
        assert!(
            sql == CAPABILITY_ITSELF || census.contains(&(capability.clone(), sql.clone())),
            "known_native_gaps.tsv names {capability} {sql:?}, which is not in census.tsv"
        );
    }
}

/// The `sqlite_replay` column is a function of `census.tsv` alone, recomputed
/// here on SQLite without an authorizer. A census edited without recomputing
/// it fails.
#[cfg(not(td_engine = "native"))]
#[test]
fn census_sqlite_replay_column_is_current() {
    let census = census();
    let store = replayed_store(&census_ddl(&census));
    let mut observed = CENSUS_HEADER.join("\t");
    observed.push('\n');
    let mut stale = Vec::new();
    for row in &census {
        let (reach, _) = isolated(&store, None, &row.sql);
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
    use std::collections::{BTreeMap, BTreeSet};

    use tracedecay_sql::{Capability, Connection, ConnectionControl, types::Value};

    use super::{
        CAPABILITIES, CAPABILITY_ITSELF, POLICY_FIXTURE, Reach, attempt, census, census_ddl,
        escape, isolated, known_native_gaps, policy, replayed_store, write_observed,
    };

    /// The native capability a census or policy capability enters, if any.
    fn capability(name: &str) -> Option<Capability> {
        match name {
            "reader" => Some(Capability::Reader),
            "writer" => Some(Capability::Writer),
            "maintenance" => Some(Capability::Maintenance),
            _ => None,
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

    fn fixture() -> Connection {
        let fixture: Vec<&str> = POLICY_FIXTURE
            .split(";\n")
            .filter(|sql| !sql.trim().is_empty())
            .collect();
        replayed_store(&fixture)
    }

    /// Zero tolerance: a statement SQLite refused under a capability is
    /// refused natively under it and leaves the store as it was.
    #[test]
    fn every_sqlite_denial_stays_denied_and_has_no_effect() {
        let untouched = state(&fixture());
        let mut violations = Vec::new();
        for (verdicts, sql) in policy() {
            for (name, verdict) in CAPABILITIES.iter().zip(&verdicts) {
                // A capability the native arm cannot enter is a listed gap.
                let Some(capability) = capability(name).filter(|_| verdict != "allow") else {
                    continue;
                };
                let store = fixture();
                store.set_capability(capability).unwrap();
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

    /// Every census statement gets as far natively, under its capability, as
    /// it did on SQLite: one that ran there runs here.
    #[test]
    fn every_census_statement_runs_under_its_capability() {
        let census = census();
        let store = replayed_store(&census_ddl(&census));

        let mut failing: BTreeMap<(String, String), String> = BTreeMap::new();
        for name in CAPABILITIES {
            if capability(name).is_none() {
                failing.insert(
                    (name.to_owned(), CAPABILITY_ITSELF.to_owned()),
                    format!("no native capability for {name}"),
                );
            }
        }
        for row in census.iter().filter(|row| row.sqlite > Reach::Missing) {
            // Without its capability the statement still runs on the default
            // access, so SQL gaps surface before the capability lands.
            let (reach, error) = isolated(&store, capability(&row.capability), &row.sql);
            if reach < row.sqlite {
                let why = error
                    .unwrap_or_default()
                    .chars()
                    .take(200)
                    .collect::<String>();
                failing.insert(
                    (row.capability.clone(), row.sql.clone()),
                    escape(&format!(
                        "SQLite {}, native {}: {why}",
                        row.sqlite.name(),
                        reach.name()
                    )),
                );
            }
        }

        let mut observed = String::from("capability\tsql\treason\n");
        for ((name, sql), why) in &failing {
            observed.push_str(&format!("{name}\t{}\t{why}\n", escape(sql)));
        }
        write_observed("known_native_gaps.tsv", &observed);

        let known = known_native_gaps();
        let observed: BTreeSet<(String, String)> = failing.keys().cloned().collect();
        let new: Vec<String> = observed
            .difference(&known)
            .map(|key| format!("{}\t{}\t{}", key.0, key.1, failing[key]))
            .collect();
        let fixed: Vec<String> = known
            .difference(&observed)
            .map(|(name, sql)| format!("{name}\t{sql}"))
            .collect();
        assert!(
            new.is_empty() && fixed.is_empty(),
            "native gate drifted from known_native_gaps.tsv\nnew failures ({}):\n{}\nfixed, remove from the list ({}):\n{}",
            new.len(),
            new.join("\n"),
            fixed.len(),
            fixed.join("\n"),
        );
    }
}
