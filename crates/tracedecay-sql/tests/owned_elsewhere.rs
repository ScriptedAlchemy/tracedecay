//! A store held open by one process is unavailable to every other process
//! under the native engine: the second opener sees `ErrorKind::OwnedElsewhere`,
//! not a retryable `Busy`. SQLite shares its WAL across processes, so the
//! default engine opens it.

use std::process::Command;

use tracedecay_sql::{Connection, ENGINE, Engine, ErrorExt};

const CHILD_STORE: &str = "TRACEDECAY_SQL_OWNED_ELSEWHERE_STORE";
const TEST_NAME: &str = "second_process_open_reports_owned_elsewhere";

fn open_and_write(path: &str) -> tracedecay_sql::Result<Connection> {
    let connection = Connection::open(path)?;
    connection.execute(
        "CREATE TABLE IF NOT EXISTS held(id INTEGER PRIMARY KEY)",
        [],
    )?;
    Ok(connection)
}

#[test]
fn second_process_open_reports_owned_elsewhere() {
    if let Ok(path) = std::env::var(CHILD_STORE) {
        match open_and_write(&path) {
            Ok(_) => println!("OUTCOME=opened"),
            Err(error) => println!("OUTCOME={:?} {error}", error.kind()),
        }
        return;
    }

    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("held.tdb");
    let path = path.to_str().unwrap();
    let _holder = open_and_write(path).unwrap();

    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", TEST_NAME, "--nocapture", "--test-threads=1"])
        .env(CHILD_STORE, path)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "child failed: {stdout}");
    let expected = match ENGINE {
        Engine::Native => "OUTCOME=OwnedElsewhere",
        Engine::Sqlite => "OUTCOME=opened",
    };
    assert!(
        stdout.contains(expected),
        "expected {expected}, child printed: {stdout}"
    );
}
