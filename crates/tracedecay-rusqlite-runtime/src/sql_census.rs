//! SQL census for the native Turso cutover's guard-parity gate (#3402).
//!
//! Under `bazel test --test_env=TD_SQL_CENSUS=1`, every distinct statement an
//! owned-store connection runs is appended, with the capability it ran under,
//! to `$TEST_UNDECLARED_OUTPUTS_DIR/sql-census-<pid>.tsv`. Without both
//! variables the hook is never installed. `scripts/storage/sql-census.sh`
//! merges the files into `crates/tracedecay-sql/tests/testdata/census.tsv`.

use std::{
    collections::HashSet,
    fs::{File, OpenOptions},
    io::Write as _,
    path::PathBuf,
    sync::{Mutex, OnceLock, PoisonError},
};

use rusqlite::{
    Connection,
    trace::{TraceEvent, TraceEventCodes},
};

use crate::connection::ConnectionMode;

/// The capability vocabulary shared by `census.tsv`, `policy.tsv` and the
/// `tracedecay-sql` policy-parity gate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CensusCapability {
    Reader,
    Writer,
    ExactSql,
    ExactSqlSavepoints,
    Maintenance,
}

impl CensusCapability {
    pub(crate) const ALL: [Self; 5] = [
        Self::Reader,
        Self::Writer,
        Self::ExactSql,
        Self::ExactSqlSavepoints,
        Self::Maintenance,
    ];

    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Reader => "reader",
            Self::Writer => "writer",
            Self::ExactSql => "exact_sql",
            Self::ExactSqlSavepoints => "exact_sql_savepoints",
            Self::Maintenance => "maintenance",
        }
    }
}

impl From<ConnectionMode> for CensusCapability {
    fn from(mode: ConnectionMode) -> Self {
        match mode {
            ConnectionMode::Reader => Self::Reader,
            ConnectionMode::Writer => Self::Writer,
            ConnectionMode::Maintenance => Self::Maintenance,
        }
    }
}

/// Records later statements on `connection` under `capability`. Call it next
/// to every authorizer change.
pub(crate) fn trace(connection: &Connection, capability: impl Into<CensusCapability>) {
    if output_dir().is_none() {
        return;
    }
    // `trace_v2` takes a plain function pointer, so the capability is baked
    // into one instantiation per variant.
    let record: fn(TraceEvent<'_>) = match capability.into() {
        CensusCapability::Reader => record::<0>,
        CensusCapability::Writer => record::<1>,
        CensusCapability::ExactSql => record::<2>,
        CensusCapability::ExactSqlSavepoints => record::<3>,
        CensusCapability::Maintenance => record::<4>,
    };
    connection.trace_v2(TraceEventCodes::SQLITE_TRACE_STMT, Some(record));
}

fn output_dir() -> Option<&'static PathBuf> {
    static DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
    DIR.get_or_init(|| {
        std::env::var_os("TD_SQL_CENSUS")?;
        std::env::var_os("TEST_UNDECLARED_OUTPUTS_DIR").map(PathBuf::from)
    })
    .as_ref()
}

fn record<const CAPABILITY: usize>(event: TraceEvent<'_>) {
    let TraceEvent::Stmt(statement, _) = event else {
        return;
    };
    let sql = statement.sql();
    let Some(dir) = output_dir().filter(|_| !sql.trim().is_empty()) else {
        return;
    };
    let line = format!(
        "{}\t{}\n",
        CensusCapability::ALL[CAPABILITY].name(),
        escape(&sql)
    );
    static SINK: Mutex<Option<(HashSet<String>, File)>> = Mutex::new(None);
    let mut sink = SINK.lock().unwrap_or_else(PoisonError::into_inner);
    if sink.is_none() {
        let path = dir.join(format!("sql-census-{}.tsv", std::process::id()));
        match OpenOptions::new().create(true).append(true).open(&path) {
            Ok(file) => *sink = Some((HashSet::new(), file)),
            Err(error) => {
                eprintln!("sql census: cannot open {}: {error}", path.display());
                return;
            }
        }
    }
    let Some((seen, file)) = sink.as_mut() else {
        return;
    };
    if !seen.contains(&line) {
        if let Err(error) = file.write_all(line.as_bytes()) {
            eprintln!("sql census: lost a statement: {error}");
        }
        seen.insert(line);
    }
}

/// TSV field escaping shared with the gate's parser: backslash, tab, newline
/// and carriage return become `\\`, `\t`, `\n` and `\r`.
pub(crate) fn escape(sql: &str) -> String {
    let mut escaped = String::with_capacity(sql.len());
    for character in sql.chars() {
        match character {
            '\\' => escaped.push_str("\\\\"),
            '\t' => escaped.push_str("\\t"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            other => escaped.push(other),
        }
    }
    escaped
}

#[cfg(test)]
mod policy_corpus;
