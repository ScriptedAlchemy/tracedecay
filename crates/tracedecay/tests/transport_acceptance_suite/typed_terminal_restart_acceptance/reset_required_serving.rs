//! A registered store whose persisted shape this binary does not open is
//! served as a typed reset-required state instead of a daemon that refuses to
//! start.
//!
//! The profile session store is stamped with the Git correlation schema the
//! previous release wrote and a physically spawned `tracedecay daemon run` is started over it. The daemon
//! must reach readiness, `tracedecay_status` must name the refused store with
//! its exact reset command, a profile session read must return the typed
//! `reset_required` problem, and a code-index read on the same project must
//! still answer. After the named reset the same read serves.

use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use tracedecay_session_temporal_store::SESSION_TEMPORAL_SCHEMA_VERSION;
use tracedecay_sessions::runtime::git_correlation::GIT_CORRELATION_SCHEMA_VERSION;

use crate::common::{canonical_existing_path, spawn_tracedecay_daemon_with};

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

    let previous = GIT_CORRELATION_SCHEMA_VERSION - 1;
    stamp_profile_sessions_git_correlation_version(&home_path, previous);
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
            "found_version": previous,
            "required_version": GIT_CORRELATION_SCHEMA_VERSION,
            "reason": format!(
                "git correlation profile schema {previous} is incompatible with required schema \
                 {GIT_CORRELATION_SCHEMA_VERSION}; reset the profile"
            ),
            "remedy": "tracedecay wipe --stale --yes",
        }])
    );
    let refused = super::cli_problem_envelope(
        &profile_session_read(&home_path, &project_path),
        "profile session read over a reset-required store",
    );
    super::assert_reset_required(&refused, "profile session read over a reset-required store");
    assert_eq!(
        refused["problem"]["detail"],
        json!({
            "kind": "reset_required",
            "authority": "git correlation",
            "found_version": previous,
            "required_version": GIT_CORRELATION_SCHEMA_VERSION,
            "reason": format!(
                "git correlation profile schema {previous} is incompatible with required schema \
                 {GIT_CORRELATION_SCHEMA_VERSION}; reset the profile"
            ),
            "remedy": "tracedecay wipe --stale --yes",
        })
    );
    wait_for_code_index_hit(&home_path, &project_path, "probe");

    let (reset_status, reset_output) =
        super::run_scoped_reset(&home_path, &project_path, &mut daemon, || {});
    assert!(
        reset_status.success(),
        "tracedecay wipe --stale --yes failed:\n{reset_output}"
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

/// Stamps a registered store with a session-temporal schema version.
fn stamp_session_temporal_version(db_path: &Path, version: i64) {
    let stamped = rusqlite::Connection::open(db_path)
        .expect("open the registered store")
        .execute(
            "UPDATE session_temporal_schema_migrations SET version = ?1
             WHERE name = 'session-temporal'",
            [version],
        )
        .expect("stamp the released session temporal schema version");
    assert_eq!(
        stamped,
        1,
        "{} records exactly one session temporal schema row",
        db_path.display()
    );
}

fn session_temporal_version(db_path: &Path) -> i64 {
    rusqlite::Connection::open(db_path)
        .expect("open the registered store")
        .query_row(
            "SELECT version FROM session_temporal_schema_migrations
             WHERE name = 'session-temporal'",
            [],
            |row| row.get(0),
        )
        .expect("read the session temporal schema version")
}

/// Gives a store session-temporal state, as a store that projected a session
/// under the released format holds.
fn record_session_generation(db_path: &Path) {
    let inserted = rusqlite::Connection::open(db_path)
        .expect("open the registered store")
        .execute(
            "INSERT INTO session_temporal_generations (
                 session_id, generation, state, frozen_watermarks_json, created_at
             ) VALUES ('released-session', 1, 'building', '{}', 1)",
            [],
        )
        .expect("record a released session generation");
    assert_eq!(inserted, 1);
}

/// Every registered store carries the session-temporal authority, so a
/// profile the previous release wrote has the previous version in the profile
/// authority and each session store. Only a store holding session state needs the scoped reset;
/// an empty copy is replaced with the final contract on open.
#[test]
fn copying_session_temporal_store_is_served_typed_until_its_named_reset() {
    let home = tempfile::TempDir::new().expect("isolated home");
    let home_path = canonical_existing_path(home.path());
    let project = tempfile::TempDir::new().expect("project");
    let project_path = canonical_existing_path(project.path());
    let profile_root = home_path.join(".tracedecay");
    let project_store = profile_root
        .join("projects")
        .join(tracedecay_runtime_core::storage::default_profile_project_id(&project_path))
        .join("sessions.db");

    let mut daemon = spawn_tracedecay_daemon_with(&home_path, |_| {});
    super::initialize_project(&home_path, &project_path, "session-temporal-reset");
    wait_for_code_index_hit(&home_path, &project_path, "probe");
    daemon
        .kill_and_wait()
        .expect("stop the daemon that wrote the profile");

    let previous = SESSION_TEMPORAL_SCHEMA_VERSION - 1;
    record_session_generation(&profile_root.join("user-sessions.db"));
    for store in [
        profile_root.join("global.db"),
        profile_root.join("user-sessions.db"),
        project_store.clone(),
    ] {
        stamp_session_temporal_version(&store, previous);
    }
    let mut daemon = spawn_tracedecay_daemon_with(&home_path, |_| {});
    let temporal_reason = format!(
        "session temporal profile schema {previous} is incompatible with required schema \
         {SESSION_TEMPORAL_SCHEMA_VERSION}; reset the profile"
    );
    let refusal = |store: String| {
        json!({
            "store": store,
            "authority": "session temporal",
            "found_version": previous,
            "required_version": SESSION_TEMPORAL_SCHEMA_VERSION,
            "reason": temporal_reason,
            "remedy": "tracedecay wipe --stale --yes",
        })
    };
    assert_eq!(
        status_reset_required_stores(&home_path, &project_path),
        json!([
            refusal("profile sessions".to_owned()),
            refusal(format!(
                "project sessions {}",
                tracedecay_runtime_core::storage::default_profile_project_id(&project_path)
            )),
        ])
    );
    wait_for_code_index_hit(&home_path, &project_path, "probe");
    for (scope, read) in [
        ("user", profile_session_read(&home_path, &project_path)),
        (
            "project",
            super::tool_call(
                &home_path,
                &project_path,
                "tracedecay_lcm_status",
                &json!({ "storage_scope": "project", "format": "json" }),
            ),
        ),
    ] {
        let context = format!("{scope} session read over a copying session temporal store");
        let refused = super::cli_problem_envelope(&read, &context);
        super::assert_reset_required(&refused, &context);
        assert_eq!(
            refused["problem"]["detail"],
            json!({
                "kind": "reset_required",
                "authority": "session temporal",
                "found_version": previous,
                "required_version": SESSION_TEMPORAL_SCHEMA_VERSION,
                "reason": temporal_reason,
                "remedy": "tracedecay wipe --stale --yes",
            }),
            "{context}"
        );
    }
    assert_eq!(
        session_temporal_version(&profile_root.join("global.db")),
        SESSION_TEMPORAL_SCHEMA_VERSION
    );
    assert_eq!(session_temporal_version(&project_store), previous);
    assert_eq!(
        session_temporal_version(&profile_root.join("user-sessions.db")),
        previous
    );

    let (reset_status, reset_output) =
        super::run_scoped_reset(&home_path, &project_path, &mut daemon, || {});
    assert!(
        reset_status.success(),
        "tracedecay wipe --stale --yes failed:\n{reset_output}"
    );
    let mut daemon = spawn_tracedecay_daemon_with(&home_path, |_| {});
    super::initialize_project(&home_path, &project_path, "session-temporal-reset");
    for scope in ["user", "project"] {
        let served = super::tool_call(
            &home_path,
            &project_path,
            "tracedecay_lcm_status",
            &json!({ "storage_scope": scope, "format": "json" }),
        );
        assert!(
            find_key(&served, "problem").is_none_or(|problem| problem.is_null()),
            "the {scope} session read must serve after the named reset: {served}"
        );
    }
    assert_eq!(
        session_temporal_version(&project_store),
        SESSION_TEMPORAL_SCHEMA_VERSION
    );
    assert_eq!(
        session_temporal_version(&profile_root.join("user-sessions.db")),
        SESSION_TEMPORAL_SCHEMA_VERSION
    );
    assert_eq!(
        status_reset_required_stores(&home_path, &project_path),
        json!([]),
        "the reset store serves again"
    );

    let _ = daemon.kill_and_wait();
}
