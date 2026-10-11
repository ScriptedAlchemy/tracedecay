//! Guard parity between the SQLite authorizers and the native engine (#3402).
//!
//! - `policy.tsv` holds the SQLite authorizers' prepare-time verdict per
//!   capability for a statement corpus, over the objects in
//!   `policy-fixture.sql`.
//! - `census.tsv` holds every distinct statement the owned-store suites ran on
//!   SQLite, with the capability it ran under. Its `sqlite_replay` column says
//!   whether SQLite prepares the statement on the census's own replayed DDL;
//!   `missing` rows name objects that union lost, so they are not native gaps.
//! - `known_native_gaps.tsv` lists the native gate's current failures by
//!   capability and statement (`*` names the capability itself). U12 drives it
//!   to empty.
//!
//! `scripts/storage/sql-census.sh` regenerates all four. Each test that
//! compares against a file writes the observed version to
//! `$TEST_UNDECLARED_OUTPUTS_DIR` under the same name.

use std::collections::BTreeSet;

use tracedecay_sql::{Capability, Connection, ConnectionControl};

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

struct CensusRow {
    capability: String,
    sql: String,
    replays: bool,
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
            let replays = match replay.as_str() {
                "ok" => true,
                "missing" => false,
                other => panic!("census.tsv sqlite_replay {other:?} for {sql:?}"),
            };
            CensusRow {
                capability,
                sql,
                replays,
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

/// A fresh in-memory store with `ddl` installed until no statement makes
/// progress, so the order of the suites' stage installs does not matter.
fn replayed_store(ddl: &[&str]) -> Connection {
    let connection = Connection::open_in_memory().unwrap();
    // The native arm cannot install capabilities yet; its capability gap rows
    // report that, and the connection's default access installs the schema.
    let _ = connection.set_capability(Capability::Maintenance);
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
    assert!(
        census.iter().any(|row| row.replays),
        "no census statement prepares on its replayed schema"
    );
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
/// here on SQLite. A census edited without recomputing it fails.
#[cfg(not(td_engine = "native"))]
#[test]
fn census_sqlite_replay_column_is_current() {
    let census = census();
    let store = replayed_store(&census_ddl(&census));
    let mut observed = CENSUS_HEADER.join("\t");
    observed.push('\n');
    let mut stale = Vec::new();
    for row in &census {
        let replays = store.prepare(&row.sql).is_ok();
        if replays != row.replays {
            stale.push(format!("{}\t{}", row.capability, row.sql));
        }
        observed.push_str(&format!(
            "{}\t{}\t{}\n",
            row.capability,
            escape(&row.sql),
            if replays { "ok" } else { "missing" }
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

    use tracedecay_sql::{Capability, Connection, ConnectionControl};

    use super::{
        CAPABILITIES, CAPABILITY_ITSELF, POLICY_FIXTURE, census, census_ddl, escape,
        known_native_gaps, policy, replayed_store, write_observed,
    };

    fn capability(name: &str) -> Option<Capability> {
        match name {
            "reader" => Some(Capability::Reader),
            "writer" => Some(Capability::Writer),
            "maintenance" => Some(Capability::Maintenance),
            _ => None,
        }
    }

    fn enter(connection: &Connection, name: &str) -> Result<(), String> {
        let capability =
            capability(name).ok_or_else(|| format!("no native capability for {name}"))?;
        connection
            .set_capability(capability)
            .map_err(|error| error.to_string())
    }

    fn reason(error: impl ToString) -> String {
        escape(&error.to_string().chars().take(200).collect::<String>())
    }

    /// Zero tolerance: no statement SQLite refused under a capability may
    /// prepare natively under it.
    #[test]
    fn every_sqlite_denial_stays_denied() {
        let fixture: Vec<&str> = POLICY_FIXTURE
            .split(";\n")
            .filter(|sql| !sql.trim().is_empty())
            .collect();
        let connection = replayed_store(&fixture);
        let mut violations = Vec::new();
        for (verdicts, sql) in policy() {
            for (name, verdict) in CAPABILITIES.iter().zip(&verdicts) {
                // A capability the native arm cannot enter is a listed gap.
                if verdict == "allow" || enter(&connection, name).is_err() {
                    continue;
                }
                if connection.prepare(&sql).is_ok() {
                    violations.push(format!("{name}: SQLite {verdict}, native allow: {sql}"));
                }
            }
        }
        assert!(
            violations.is_empty(),
            "native allows statements SQLite refused:\n{}",
            violations.join("\n")
        );
    }

    #[test]
    fn every_census_statement_prepares_under_its_capability() {
        let census = census();
        let ddl = census_ddl(&census);
        let guarded = replayed_store(&ddl);
        let fallback = replayed_store(&ddl);

        let mut failing: BTreeMap<(String, String), String> = BTreeMap::new();
        let mut unsupported = BTreeSet::new();
        for name in CAPABILITIES {
            if let Err(why) = enter(&guarded, name) {
                unsupported.insert(name);
                failing.insert((name.to_owned(), CAPABILITY_ITSELF.to_owned()), reason(why));
            }
        }
        for row in census.iter().filter(|row| row.replays) {
            // Without its capability the statement still runs on the default
            // access, so SQL gaps surface before the capability lands.
            let prepared = if unsupported.contains(row.capability.as_str()) {
                fallback.prepare(&row.sql).map(drop)
            } else {
                enter(&guarded, &row.capability).unwrap();
                guarded.prepare(&row.sql).map(drop)
            };
            if let Err(error) = prepared {
                failing.insert((row.capability.clone(), row.sql.clone()), reason(error));
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
