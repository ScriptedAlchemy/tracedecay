//! Golden receipt trace for owned stores (#3402).
//!
//! Fixed request streams run through the production writer: the ledger, every
//! repository payload variant, and the work, workflow and remote stores. Each
//! stream yields its receipt trace and the logical dump of its store. The
//! SQLite engine generates the checked-in goldens; every engine must
//! reproduce them byte for byte.
//!
//! Defect caught: any engine or driver change that alters a receipt, a ledger
//! disposition, an executor refusal, or a persisted value (row, column, type
//! or order) fails here with the first differing line.

mod golden_receipt_trace {
    pub mod dump;
    pub mod fixtures;
    pub mod harness;
    pub mod streams;
}

use std::{collections::BTreeMap, fs, path::Path};

const REGEN: &str = "bazel run //tools/storage/parity:regen";
const GOLDEN_DIR: &str = "fixtures/golden";
/// Set by `//tools/storage/parity:regen` to the source golden directory.
const REGEN_DIR: &str = "TRACEDECAY_PARITY_REGEN_DIR";
/// Divergent lines listed per golden and direction (`-` golden only,
/// `+` produced only).
const MAX_LISTED_LINES: usize = 20;

fn render() -> BTreeMap<String, String> {
    tracedecay_global_db::register_registered_schema_installer();
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("parity runtime")
        .block_on(golden_receipt_trace::streams::render_all())
}

fn checked_in() -> BTreeMap<String, String> {
    let directory = Path::new(GOLDEN_DIR);
    fs::read_dir(directory)
        .unwrap_or_else(|error| panic!("read {}: {error}; run `{REGEN}`", directory.display()))
        .map(|entry| {
            let path = entry.expect("golden entry").path();
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            (name, fs::read_to_string(&path).expect("read golden"))
        })
        .collect()
}

fn regenerate(directory: &Path, produced: &BTreeMap<String, String>) {
    assert_eq!(
        tracedecay_sql::ENGINE,
        tracedecay_sql::Engine::Sqlite,
        "goldens are generated on the SQLite engine only"
    );
    if directory.exists() {
        fs::remove_dir_all(directory).expect("clear previous goldens");
    }
    fs::create_dir_all(directory).expect("create golden directory");
    for (name, content) in produced {
        fs::write(directory.join(name), content).expect("write golden");
    }
    eprintln!(
        "wrote {} goldens to {}",
        produced.len(),
        directory.display()
    );
}

#[test]
fn engine_reproduces_golden_receipt_trace_and_logical_dump() {
    let produced = render();
    if let Some(directory) = std::env::var_os(REGEN_DIR) {
        regenerate(Path::new(&directory), &produced);
        return;
    }
    let golden = checked_in();
    assert_eq!(
        golden.keys().collect::<Vec<_>>(),
        produced.keys().collect::<Vec<_>>(),
        "golden stream set is stale; run `{REGEN}`"
    );
    let mismatched = golden
        .iter()
        .filter(|(name, expected)| produced[*name] != **expected)
        .map(|(name, expected)| {
            let actual = &produced[name];
            let mut report = format!("{name}:");
            for (sign, from, other) in [("-", expected, actual), ("+", actual, expected)] {
                let other = other.lines().collect::<std::collections::HashSet<_>>();
                let missing = from.lines().filter(|line| !other.contains(line));
                for line in missing.take(MAX_LISTED_LINES) {
                    let line = line.chars().take(300).collect::<String>();
                    report.push_str(&format!("\n  {sign} {line}"));
                }
            }
            report
        })
        .collect::<Vec<_>>();
    assert!(
        mismatched.is_empty(),
        "engine {:?} diverges from the golden (regenerate only on SQLite with `{REGEN}`); \
         - lines are golden only, + lines produced only:\n{}",
        tracedecay_sql::ENGINE,
        mismatched.join("\n")
    );
}
