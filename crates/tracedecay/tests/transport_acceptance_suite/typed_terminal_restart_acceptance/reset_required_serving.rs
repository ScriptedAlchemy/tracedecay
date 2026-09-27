//! A registered store whose persisted shape this binary does not open is
//! served as a typed reset-required state instead of a daemon that refuses to
//! start.
//!
//! The profile session store is stamped with the Git correlation schema a
//! released binary wrote (version 5, before per-session Git evidence rows) and
//! a physically spawned `tracedecay daemon run` is started over it. The daemon
//! must reach readiness, `tracedecay_status` must name the refused store with
//! its exact reset command, a profile session read must return the typed
//! `reset_required` problem, and a code-index read on the same project must
//! still answer. After the named reset the same read serves.

use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::common::{
    canonical_existing_path, spawn_tracedecay_daemon_with, tracedecay_command_with_home,
};

/// Bound on waiting for the first sealed code generation of a two-line fixture.
const CODE_INDEX_READY_TIMEOUT: Duration = Duration::from_secs(120);

/// The first value under `key` anywhere in a tool payload, including JSON
/// rendered into MCP text blocks.
fn find_key(value: &Value, key: &str) -> Option<Value> {
    match value {
        Value::Object(map) => map
            .get(key)
            .cloned()
            .or_else(|| map.values().find_map(|child| find_key(child, key))),
        Value::Array(items) => items.iter().find_map(|child| find_key(child, key)),
        Value::String(text) => serde_json::from_str::<Value>(text)
            .ok()
            .and_then(|parsed| find_key(&parsed, key)),
        _ => None,
    }
}

fn names_symbol(value: &Value, name: &str) -> bool {
    match value {
        Value::Object(map) => {
            map.get("name").and_then(Value::as_str) == Some(name)
                || map.values().any(|child| names_symbol(child, name))
        }
        Value::Array(items) => items.iter().any(|child| names_symbol(child, name)),
        Value::String(text) => {
            serde_json::from_str::<Value>(text).is_ok_and(|parsed| names_symbol(&parsed, name))
        }
        _ => false,
    }
}

fn wait_for_code_index_hit(home: &Path, project: &Path, symbol: &str) {
    let started = Instant::now();
    loop {
        let result = super::tool_call(
            home,
            project,
            "tracedecay_search",
            &json!({ "query": symbol, "format": "json" }),
        );
        if names_symbol(&result, symbol) {
            return;
        }
        assert!(
            started.elapsed() < CODE_INDEX_READY_TIMEOUT,
            "code index never answered `{symbol}` within {CODE_INDEX_READY_TIMEOUT:?}: {result}"
        );
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// Stamps the profile session store with the Git correlation schema version
/// a released binary recorded, leaving every other byte of its shape as this
/// binary wrote it.
fn stamp_profile_sessions_git_correlation_version(home: &Path, version: i64) {
    let db_path = home.join(".tracedecay/user-sessions.db");
    assert!(
        db_path.is_file(),
        "the daemon should have created the profile session store at {}",
        db_path.display()
    );
    let stamped = rusqlite::Connection::open(&db_path)
        .expect("open the profile session store")
        .execute(
            "UPDATE session_schema_migrations SET version = ?1 WHERE name = 'git_correlation'",
            [version],
        )
        .expect("stamp the released git correlation schema version");
    assert_eq!(
        stamped, 1,
        "the store records exactly one git correlation schema row"
    );
}

fn profile_session_read(home: &Path, project: &Path) -> Value {
    super::tool_call(
        home,
        project,
        "tracedecay_lcm_status",
        &json!({ "storage_scope": "user", "format": "json" }),
    )
}

fn status_reset_required_stores(home: &Path, project: &Path) -> Value {
    let status = super::tool_call(
        home,
        project,
        "tracedecay_status",
        &json!({ "format": "json" }),
    );
    find_key(&status, "reset_required_stores")
        .unwrap_or_else(|| panic!("tracedecay_status omitted reset_required_stores: {status}"))
}

#[test]
fn reset_required_profile_session_store_is_served_typed_until_its_named_reset() {
    let home = tempfile::TempDir::new().expect("isolated home");
    let home_path = canonical_existing_path(home.path());
    let project = tempfile::TempDir::new().expect("project");
    let project_path = canonical_existing_path(project.path());

    let mut daemon = spawn_tracedecay_daemon_with(&home_path, |_| {});
    super::initialize_project(&home_path, &project_path, "reset-required-serving");
    wait_for_code_index_hit(&home_path, &project_path, "probe");
    daemon
        .kill_and_wait()
        .expect("stop the daemon that wrote the profile");

    stamp_profile_sessions_git_correlation_version(&home_path, 5);
    let mut daemon = spawn_tracedecay_daemon_with(&home_path, |_| {});
    assert!(
        daemon
            .try_wait()
            .expect("inspect the restarted daemon")
            .is_none(),
        "the daemon must keep serving over a reset-required profile session store"
    );

    assert_eq!(
        status_reset_required_stores(&home_path, &project_path),
        json!([{
            "store": "profile sessions",
            "authority": "git correlation",
            "found_version": 5,
            "required_version": 6,
            "reason": "git correlation profile schema 5 is incompatible with required schema 6; \
                       reset the profile",
            "remedy": "tracedecay wipe --all --yes",
        }])
    );
    super::assert_reset_required(
        &super::cli_problem_envelope(
            &profile_session_read(&home_path, &project_path),
            "profile session read over a reset-required store",
        ),
        "profile session read over a reset-required store",
    );
    wait_for_code_index_hit(&home_path, &project_path, "probe");

    // `tracedecay wipe --all --yes` takes the profile offline by stopping the
    // managed service and restarts it afterwards; this unmanaged daemon is
    // stopped and started around the same command.
    daemon
        .kill_and_wait()
        .expect("stop the daemon for the profile reset");
    let wipe = tracedecay_command_with_home(&home_path)
        .args(["wipe", "--all", "--yes"])
        .current_dir(&project_path)
        .stdin(Stdio::null())
        .output()
        .expect("run the named reset");
    assert!(
        wipe.status.success(),
        "tracedecay wipe --all --yes failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&wipe.stdout),
        String::from_utf8_lossy(&wipe.stderr)
    );
    let mut daemon = spawn_tracedecay_daemon_with(&home_path, |_| {});

    let served = profile_session_read(&home_path, &project_path);
    assert!(
        find_key(&served, "problem").is_none_or(|problem| problem.is_null()),
        "the profile session read must serve after the named reset: {served}"
    );
    super::initialize_project(&home_path, &project_path, "reset-required-serving");
    assert_eq!(
        status_reset_required_stores(&home_path, &project_path),
        json!([]),
        "the reset store serves again"
    );

    let _ = daemon.kill_and_wait();
}
