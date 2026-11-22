//! A registered store whose persisted shape or settings this binary does not open is
//! served as a typed reset-required state instead of a daemon that refuses to
//! start.
//!
//! The profile session store is stamped with the Git correlation schema the
//! previous release wrote and a physically spawned `tracedecay daemon run` is started over it. The daemon
//! must reach readiness, `tracedecay_status` must name the refused store with
//! its exact reset command, a profile session read must return the typed
//! `reset_required` problem, and a code-index read on the same project must
//! still answer. After the named reset the same read serves.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use tracedecay_domain::configuration::{
    ConfigurationCandidateV1, ConfigurationSnapshotV1, ConfigurationValueV1, SettingKey,
    USER_SESSION_COMPACTION_DAYS_SETTING_KEY, USER_SESSION_HISTORY_IMPORT_ENABLED_SETTING_KEY,
    USER_SESSION_RETENTION_DAYS_SETTING_KEY, USER_SESSION_RETENTION_ENABLED_SETTING_KEY,
};
use tracedecay_global_db::configuration::{registry::ConfigurationRegistry, resolver};
use tracedecay_session_temporal_store::SESSION_TEMPORAL_SCHEMA_VERSION;
use tracedecay_sessions::runtime::git_correlation::GIT_CORRELATION_SCHEMA_VERSION;

use super::stale_sessions_store_reset::file_digests;
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
            "UPDATE tracedecay_schema_version SET version = ?1 WHERE component = 'git_correlation'",
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

fn profile_configuration_snapshot(db_path: &Path) -> (String, ConfigurationSnapshotV1) {
    let connection = rusqlite::Connection::open_with_flags(
        db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .expect("read the existing profile settings");
    let (revision, snapshot_id, behavior_digest, provenance_digest) = connection
        .query_row(
            "SELECT revision_id, snapshot_id, effective_behavior_digest,
                    resolution_provenance_digest
             FROM configuration_revisions AS candidate
             WHERE NOT EXISTS (
                 SELECT 1 FROM configuration_revisions AS child
                 WHERE child.parent_revision_id = candidate.revision_id
             )",
            [],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            },
        )
        .expect("read the current profile revision");
    let mut statement = connection
        .prepare("SELECT key, typed_value FROM configuration_entries WHERE revision_id = ?1")
        .expect("read the current profile entries");
    let entries = statement
        .query_map([&revision], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .expect("query profile entries");
    let mut values = BTreeMap::new();
    let mut provenance = BTreeMap::new();
    for entry in entries {
        let (key, encoded) = entry.expect("read a profile setting");
        let key = SettingKey::new(key).expect("registered setting key");
        let entry: Value = serde_json::from_str(&encoded).expect("stored setting payload");
        values.insert(
            key.clone(),
            serde_json::from_value::<ConfigurationValueV1>(entry["value"].clone())
                .expect("typed profile value"),
        );
        provenance.insert(
            key,
            serde_json::from_value::<Vec<ConfigurationCandidateV1>>(entry["provenance"].clone())
                .expect("typed profile provenance"),
        );
    }
    let snapshot =
        ConfigurationSnapshotV1::new(values, provenance).expect("sealed profile snapshot");
    assert_eq!(snapshot.snapshot_id.as_str(), snapshot_id);
    assert_eq!(snapshot.effective_behavior_digest.as_str(), behavior_digest);
    assert_eq!(
        snapshot.resolution_provenance_digest.as_str(),
        provenance_digest
    );
    (revision, snapshot)
}

/// Publishes the settings set an older daemon knew, with valid identity and
/// unchanged physical schema. Appending a revision retains the production
/// immutability triggers; only registry completeness can refuse this store.
fn publish_previous_profile_settings(db_path: &Path) -> (String, ConfigurationSnapshotV1) {
    let (previous_revision, mut snapshot) = profile_configuration_snapshot(db_path);
    let new_keys = [
        USER_SESSION_HISTORY_IMPORT_ENABLED_SETTING_KEY,
        USER_SESSION_RETENTION_ENABLED_SETTING_KEY,
        USER_SESSION_RETENTION_DAYS_SETTING_KEY,
        USER_SESSION_COMPACTION_DAYS_SETTING_KEY,
    ];
    for key in new_keys {
        let key = SettingKey::new(key).unwrap();
        assert!(snapshot.effective_values.remove(&key).is_some());
        assert!(snapshot.provenance.remove(&key).is_some());
    }
    let snapshot = ConfigurationSnapshotV1::new(snapshot.effective_values, snapshot.provenance)
        .expect("the older settings retain a valid snapshot identity");
    let revision = "configuration.profile.previous-settings.v1";
    let mut connection =
        rusqlite::Connection::open(db_path).expect("open the stopped profile store");
    let transaction = connection
        .transaction()
        .expect("publish older profile settings");
    assert_eq!(
        transaction
            .execute(
                "INSERT INTO configuration_revisions (
                     revision_id, parent_revision_id, snapshot_id, effective_behavior_digest,
                     resolution_provenance_digest, actor_id, operation_kind, created_at
                 ) SELECT ?1, revision_id, ?2, ?3, ?4, actor_id, operation_kind, created_at + 1
                   FROM configuration_revisions WHERE revision_id = ?5",
                rusqlite::params![
                    revision,
                    snapshot.snapshot_id.as_str(),
                    snapshot.effective_behavior_digest.as_str(),
                    snapshot.resolution_provenance_digest.as_str(),
                    previous_revision,
                ],
            )
            .expect("append the older settings revision"),
        1
    );
    assert_eq!(
        transaction
            .execute(
                "INSERT INTO configuration_entries (
                     revision_id, key, layer_kind, layer_id, schema_revision, typed_value
                 ) SELECT ?1, key, layer_kind, layer_id, schema_revision, typed_value
                   FROM configuration_entries
                   WHERE revision_id = ?2 AND key NOT IN (?3, ?4, ?5, ?6)",
                rusqlite::params![
                    revision,
                    previous_revision,
                    new_keys[0],
                    new_keys[1],
                    new_keys[2],
                    new_keys[3],
                ],
            )
            .expect("retain the older settings entries"),
        snapshot.effective_values.len()
    );
    transaction.commit().expect("commit the older settings");
    connection
        .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
        .expect("settle the stopped store before byte comparisons");
    profile_configuration_snapshot(db_path)
}

#[test]
fn older_profile_settings_are_resettable_without_blocking_daemon_startup() {
    let home = tempfile::TempDir::new().expect("isolated home");
    let home_path = canonical_existing_path(home.path());
    let project = tempfile::TempDir::new().expect("project");
    let project_path = canonical_existing_path(project.path());
    let profile_root = home_path.join(".tracedecay");
    let profile_sessions = profile_root.join("user-sessions.db");

    let mut daemon = spawn_tracedecay_daemon_with(&home_path, |_| {});
    super::initialize_project(&home_path, &project_path, "older-profile-settings");
    wait_for_code_index_hit(&home_path, &project_path, "probe");
    daemon.kill_and_wait().expect("stop the initialized daemon");
    crate::common::wait_for_stopped_daemon(&home_path);
    let previous = publish_previous_profile_settings(&profile_sessions);
    assert_eq!(
        session_temporal_version(&profile_sessions),
        SESSION_TEMPORAL_SCHEMA_VERSION,
        "the refused settings live in a physically current session store"
    );
    let refused_bytes = std::fs::read(&profile_sessions).expect("read refused store bytes");

    let mut daemon = spawn_tracedecay_daemon_with(&home_path, |_| {});
    let stores = status_reset_required_stores(&home_path, &project_path);
    let stores = stores.as_array().expect("reset census");
    assert_eq!(
        stores.len(),
        1,
        "only profile settings require reset: {stores:?}"
    );
    assert_eq!(stores[0]["store"], json!("profile sessions"));
    assert_eq!(stores[0]["authority"], json!("configuration"));
    assert_eq!(stores[0]["remedy"], json!("tracedecay wipe --stale --yes"));
    assert!(
        stores[0]["reason"]
            .as_str()
            .expect("configuration refusal reason")
            .contains("configuration snapshot does not contain the complete registry"),
        "the refusal must come from the older settings, not a physical schema mismatch: {stores:?}"
    );
    let refused = super::cli_problem_envelope(
        &profile_session_read(&home_path, &project_path),
        "profile read over older settings",
    );
    super::assert_reset_required(&refused, "profile read over older settings");
    assert_eq!(
        refused["problem"]["detail"]["authority"],
        json!("configuration")
    );
    assert_eq!(
        refused["problem"]["detail"]["remedy"],
        json!("tracedecay wipe --stale --yes")
    );
    wait_for_code_index_hit(&home_path, &project_path, "probe");
    assert_eq!(profile_configuration_snapshot(&profile_sessions), previous);

    let mut before_reset = BTreeMap::new();
    let (reset_status, reset_output) =
        super::run_scoped_reset(&home_path, &project_path, &mut daemon, || {
            assert_eq!(profile_configuration_snapshot(&profile_sessions), previous);
            assert_eq!(
                std::fs::read(&profile_sessions).expect("read the refused store before reset"),
                refused_bytes,
                "serving the reset state must leave the refused store byte-identical"
            );
            before_reset = file_digests(&profile_root);
        });
    assert!(
        reset_status.success(),
        "the scoped reset failed:\n{reset_output}"
    );
    assert!(
        reset_output.contains("reset profile sessions"),
        "{reset_output}"
    );
    let after_reset = file_digests(&profile_root);
    assert!(before_reset.contains_key(Path::new("user-sessions.db")));
    for (relative, digest) in &before_reset {
        let name = relative.to_string_lossy();
        if name.starts_with("user-sessions.")
            || name.starts_with(".user-sessions.db.host-admission")
        {
            assert!(
                !after_reset.contains_key(relative),
                "the scoped reset left refused store member {}: {reset_output}",
                relative.display()
            );
        } else if relative.as_path() != Path::new("lifecycle.lock") {
            assert_eq!(
                after_reset.get(relative),
                Some(digest),
                "the profile reset changed an unrelated file {}: {reset_output}",
                relative.display()
            );
        }
    }

    let mut daemon = spawn_tracedecay_daemon_with(&home_path, |_| {});
    let served = profile_session_read(&home_path, &project_path);
    assert!(
        find_key(&served, "problem").is_none_or(|problem| problem.is_null()),
        "the fresh profile must serve after its scoped reset: {served}"
    );
    assert_eq!(
        status_reset_required_stores(&home_path, &project_path),
        json!([])
    );
    let (_, fresh) = profile_configuration_snapshot(&profile_sessions);
    let defaults = resolver::resolve_configuration(&ConfigurationRegistry::profile().unwrap(), &[])
        .expect("current profile defaults")
        .snapshot;
    assert_eq!(fresh.effective_values, defaults.effective_values);
    wait_for_code_index_hit(&home_path, &project_path, "probe");
    let _ = daemon.kill_and_wait();
}

/// Stamps a registered store with a session-temporal schema version.
fn stamp_session_temporal_version(db_path: &Path, version: i64) {
    let stamped = rusqlite::Connection::open(db_path)
        .expect("open the registered store")
        .execute(
            "UPDATE tracedecay_schema_version SET version = ?1
             WHERE component = 'session-temporal'",
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
            "SELECT version FROM tracedecay_schema_version
             WHERE component = 'session-temporal'",
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
