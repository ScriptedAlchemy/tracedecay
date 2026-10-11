//! SQL census for the native cutover's guard-parity gate (#3402).
//!
//! Under `bazel test --test_env=TD_SQL_CENSUS=1`, every distinct statement a
//! connection runs after its capability is installed is appended, with that
//! capability, to `$TEST_UNDECLARED_OUTPUTS_DIR/sql-census-<pid>.tsv`. Without
//! both variables no hook is installed. `scripts/storage/sql-census.sh` merges
//! the files into `crates/tracedecay-sql/tests/testdata/census.tsv`.

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

use crate::Capability;

/// Records later statements on `connection` under `capability`.
pub(super) fn trace(connection: &Connection, capability: Capability) {
    if output_dir().is_none() {
        return;
    }
    // `trace_v2` takes a plain function pointer, so the capability is baked
    // into one instantiation per variant.
    let record: fn(TraceEvent<'_>) = match capability {
        Capability::Reader => record::<0>,
        Capability::ExactSqlRead => record::<1>,
        Capability::Writer => record::<2>,
        Capability::ExactSql { savepoints: false } => record::<3>,
        Capability::ExactSql { savepoints: true } => record::<4>,
        Capability::Maintenance => record::<5>,
    };
    connection.trace_v2(TraceEventCodes::SQLITE_TRACE_STMT, Some(record));
}

/// The census and policy column name of each capability, in `record` order.
const NAMES: [&str; 6] = [
    "reader",
    "exact_sql_read",
    "writer",
    "exact_sql",
    "exact_sql_savepoints",
    "maintenance",
];

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
    let line = format!("{}\t{}\n", NAMES[CAPABILITY], escape(&sql));
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
fn escape(sql: &str) -> String {
    sql.replace('\\', "\\\\")
        .replace('\t', "\\t")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
}
