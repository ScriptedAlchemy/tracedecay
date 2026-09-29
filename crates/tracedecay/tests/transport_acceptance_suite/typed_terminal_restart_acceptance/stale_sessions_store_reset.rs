//! Session stores whose observation rows predate the unified observation
//! identity are refused as typed reset states without taking code
//! intelligence down with them, and their scoped reset deletes exactly those
//! stores.
//!
//! A physically spawned `tracedecay daemon run` first writes a real profile
//! and project session store. Both are then given the shape a released binary
//! left behind: observation rows written before the unified identity, with no
//! unified-identity marker. Over that profile the project must still open, the
//! MCP host must initialize and list tools, code search must answer, session
//! reads must return the typed `reset_required` refusal naming
//! `tracedecay wipe --stale --yes`, and `tracedecay doctor` must name that
//! command instead of `tracedecay install`. The scoped reset then deletes both
//! session stores and nothing else, and the restarted daemon serves sessions
//! from empty stores.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::common::{
    TestChildProcess, canonical_existing_path, spawn_tracedecay_daemon_with,
    tracedecay_command_with_home,
};

const STALE_STORE_RESET: &str = "tracedecay wipe --stale --yes";
const OBSERVATIONS_RESET_REASON: &str = "observations persisted shape requires reset: \
     observation rows predate the unified observation identity and cannot be read; reset the \
     profile so ingestion can rebuild them from host transcripts";
const PRE_UNIFIED_OBSERVATION_ID: &str =
    "sha256:efd99c7fd87f4ad156b40f16d982d18511ebfb708afc140f9f67e63e0c73f5ba";
const PRE_UNIFIED_RECEIPT_ID: &str =
    "privacy.claude.v1.0000000000000000000000000000000000000000000000000000000000000000";
/// A Claude observation as binaries before the unified identity wrote it:
/// its id derives from `tracedecay.claude.observation.v1` and its JSON still
/// carries `idempotency_key`.
const PRE_UNIFIED_OBSERVATION_JSON: &str = concat!(
    r#"{"observation_id":""#,
    "sha256:efd99c7fd87f4ad156b40f16d982d18511ebfb708afc140f9f67e63e0c73f5ba",
    r#"","idempotency_key":""#,
    "sha256:efd99c7fd87f4ad156b40f16d982d18511ebfb708afc140f9f67e63e0c73f5ba",
    r#"","identity":{"source":{"provider":"claude","session_id":"session.fixture"}},"#,
    r#""receipt":{"receipt_id":""#,
    "privacy.claude.v1.0000000000000000000000000000000000000000000000000000000000000000",
    r#""},"retention_class":"transcript.fixture","payload":{"message":"safe"}}"#
);
/// Bound on the daemon recording both refused stores after it restarts over
/// them: the project store refuses on project open, the profile store on the
/// full-route upgrade that follows.
const RESET_CENSUS_TIMEOUT: Duration = Duration::from_secs(60);
const SERVE_TIMEOUT: Duration = Duration::from_secs(90);

/// Gives an existing session store the persisted shape a released binary
/// wrote before the unified observation identity: one observation row and no
/// unified-identity marker.
fn seed_pre_unified_observation_rows(db_path: &Path) {
    let connection = rusqlite::Connection::open(db_path).expect("open the session store");
    let unmarked = connection
        .execute(
            "DELETE FROM global_schema_migrations \
             WHERE migration = 'observations-unified-identity-v1'",
            [],
        )
        .expect("remove the unified-identity marker");
    assert_eq!(unmarked, 1, "{} records the marker once", db_path.display());
    connection
        .execute(
            "INSERT INTO sanitization_receipts \
             (receipt_id, sanitizer_version, payload_digest, receipt_json) \
             VALUES (?1, 'privacy.claude-record.v1', ?2, '{}')",
            [PRE_UNIFIED_RECEIPT_ID, PRE_UNIFIED_OBSERVATION_ID],
        )
        .expect("seed the pre-unified receipt");
    connection
        .execute(
            "INSERT INTO observations \
             (observation_id, payload_digest, receipt_id, observation_json, \
              committed_cursor_json) VALUES (?1, ?1, ?2, ?3, '{}')",
            [
                PRE_UNIFIED_OBSERVATION_ID,
                PRE_UNIFIED_RECEIPT_ID,
                PRE_UNIFIED_OBSERVATION_JSON,
            ],
        )
        .expect("seed the pre-unified observation");
}

/// SHA-256 of every regular file under `root`, keyed by relative path.
fn file_digests(root: &Path) -> BTreeMap<PathBuf, String> {
    fn walk(root: &Path, directory: &Path, digests: &mut BTreeMap<PathBuf, String>) {
        for entry in std::fs::read_dir(directory).expect("read profile directory") {
            let path = entry.expect("profile directory entry").path();
            let file_type = std::fs::symlink_metadata(&path)
                .expect("inspect profile entry")
                .file_type();
            if file_type.is_dir() {
                walk(root, &path, digests);
            } else if file_type.is_file() {
                let bytes = std::fs::read(&path).expect("read profile file");
                digests.insert(
                    path.strip_prefix(root)
                        .expect("relative path")
                        .to_path_buf(),
                    hex::encode(Sha256::digest(&bytes)),
                );
            }
        }
    }
    let mut digests = BTreeMap::new();
    walk(root, root, &mut digests);
    digests
}

/// Whether `relative` belongs to one of the two refused session stores.
fn is_session_store_member(relative: &Path, project_store: &Path) -> bool {
    let name = relative.to_string_lossy();
    let in_project_sessions = relative.starts_with(project_store)
        && relative
            .strip_prefix(project_store)
            .ok()
            .and_then(|rest| rest.components().next())
            .is_some_and(|first| {
                let first = first.as_os_str().to_string_lossy();
                first.starts_with("sessions.") || first == ".sessions.db.host-admission"
            });
    in_project_sessions
        || name.starts_with("user-sessions.")
        || name.starts_with(".user-sessions.db.host-admission")
}

fn status(home: &Path, project: &Path) -> Value {
    let status = super::tool_call(
        home,
        project,
        "tracedecay_status",
        &json!({ "format": "json" }),
    );
    super::typed_envelope(&status)
}

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

fn reset_required_stores(home: &Path, project: &Path) -> Vec<Value> {
    let status = status(home, project);
    let mut stores = find_key(&status, "reset_required_stores")
        .and_then(|stores| stores.as_array().cloned())
        .unwrap_or_else(|| panic!("tracedecay_status omitted reset_required_stores: {status}"));
    stores.sort_by(|left, right| left["store"].as_str().cmp(&right["store"].as_str()));
    stores
}

fn wait_for_reset_required_stores(home: &Path, project: &Path, expected: &[Value]) {
    let started = Instant::now();
    loop {
        let stores = reset_required_stores(home, project);
        if stores == expected {
            return;
        }
        assert!(
            started.elapsed() < RESET_CENSUS_TIMEOUT,
            "the daemon never recorded exactly the refused session stores within \
             {RESET_CENSUS_TIMEOUT:?}\nexpected: {expected:#?}\nobserved: {stores:#?}"
        );
        std::thread::sleep(Duration::from_millis(500));
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
            started.elapsed() < Duration::from_secs(120),
            "code search never answered `{symbol}`: {result}"
        );
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// `initialize` then `tools/list` through a real `tracedecay serve` host.
fn mcp_initialize_and_list_tools(home: &Path, project: &Path) -> (Value, Value) {
    let mut command = tracedecay_command_with_home(home);
    command
        .arg("serve")
        .arg("--path")
        .arg(project)
        .current_dir(project)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = TestChildProcess::new(command.spawn().expect("spawn tracedecay serve"));
    {
        let stdin = child.stdin_mut().expect("MCP host stdin is piped");
        for request in [
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2024-11-05",
                    "capabilities": {},
                    "clientInfo": { "name": "stale-sessions-store-reset", "version": "0.0.0" }
                }
            }),
            json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
            json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }),
        ] {
            writeln!(stdin, "{request}").expect("write MCP request");
        }
    }
    let output = child
        .wait_with_output(SERVE_TIMEOUT)
        .expect("the MCP host exits after stdin closes");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let response = |id: i64| {
        stdout
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .find(|message| message.get("id") == Some(&json!(id)))
            .unwrap_or_else(|| {
                panic!(
                    "the MCP host returned no response {id}\nstdout:\n{stdout}\nstderr:\n{}",
                    String::from_utf8_lossy(&output.stderr)
                )
            })
    };
    (response(1), response(2))
}

/// The outcome kind a code-read primitive answered, or the problem it
/// refused with.
fn code_read_outcome(home: &Path, project: &Path, tool: &str, args: &Value) -> Value {
    let envelope = super::typed_envelope(&super::tool_call(home, project, tool, args));
    if envelope["problem"].is_object() {
        return json!({ "problem": envelope["problem"]["kind"] });
    }
    envelope["outcome"]["outcome"].clone()
}

fn probe_symbol_id(home: &Path, project: &Path) -> String {
    let found = super::typed_envelope(&super::tool_call(
        home,
        project,
        "tracedecay_find_exact_symbol",
        &json!({ "name": "probe", "format": "json" }),
    ));
    found["matches"][0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("find_exact_symbol did not resolve `probe`: {found}"))
        .to_owned()
}

fn session_status(home: &Path, project: &Path, storage_scope: &str) -> Value {
    super::tool_call(
        home,
        project,
        "tracedecay_lcm_status",
        &json!({ "storage_scope": storage_scope, "format": "json" }),
    )
}

#[test]
fn stale_session_stores_refuse_sessions_only_until_their_scoped_reset() {
    let home = tempfile::TempDir::new().expect("isolated home");
    let home_path = canonical_existing_path(home.path());
    let project = tempfile::TempDir::new().expect("project");
    let project_path = canonical_existing_path(project.path());
    let profile_root = home_path.join(".tracedecay");
    let project_id = tracedecay_runtime_core::storage::default_profile_project_id(&project_path);
    let project_store = PathBuf::from("projects").join(&project_id);

    let mut daemon = spawn_tracedecay_daemon_with(&home_path, |_| {});
    super::initialize_project(&home_path, &project_path, "stale-sessions-store-reset");
    wait_for_code_index_hit(&home_path, &project_path, "probe");
    for scope in ["project", "user"] {
        let served = session_status(&home_path, &project_path, scope);
        assert!(
            find_key(&served, "problem").is_none_or(|problem| problem.is_null()),
            "the fresh {scope} session store serves before it is aged: {served}"
        );
    }
    daemon
        .kill_and_wait()
        .expect("stop the daemon that wrote the profile");

    seed_pre_unified_observation_rows(&profile_root.join("user-sessions.db"));
    seed_pre_unified_observation_rows(&profile_root.join(&project_store).join("sessions.db"));

    let mut daemon = spawn_tracedecay_daemon_with(&home_path, |_| {});

    let (initialize, tools) = mcp_initialize_and_list_tools(&home_path, &project_path);
    assert_eq!(
        initialize.get("error"),
        None,
        "MCP initialize must serve over stale session stores: {initialize}"
    );
    let tool_names: Vec<&str> = tools["result"]["tools"]
        .as_array()
        .unwrap_or_else(|| panic!("tools/list returned no catalog: {tools}"))
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect();
    for code_tool in [
        "tracedecay_search",
        "tracedecay_callers",
        "tracedecay_status",
    ] {
        assert!(
            tool_names.contains(&code_tool),
            "tools/list omitted {code_tool}: {tools}"
        );
    }

    wait_for_code_index_hit(&home_path, &project_path, "probe");
    let probe_id = probe_symbol_id(&home_path, &project_path);
    assert_eq!(
        code_read_outcome(
            &home_path,
            &project_path,
            "tracedecay_callers",
            &json!({ "node_id": probe_id, "format": "json" }),
        ),
        json!("evidence"),
        "callers must serve over stale session stores"
    );
    assert_eq!(
        code_read_outcome(
            &home_path,
            &project_path,
            "tracedecay_file_dependents",
            &json!({ "file": "src/lib.rs", "format": "json" }),
        ),
        json!("evidence"),
        "file dependents must serve over stale session stores"
    );
    let expected_stale = vec![
        json!({
            "store": "profile sessions",
            "authority": "observations",
            "found_version": null,
            "required_version": null,
            "reason": OBSERVATIONS_RESET_REASON,
            "remedy": STALE_STORE_RESET,
        }),
        json!({
            "store": format!("project sessions {project_id}"),
            "authority": "observations",
            "found_version": null,
            "required_version": null,
            "reason": OBSERVATIONS_RESET_REASON,
            "remedy": STALE_STORE_RESET,
        }),
    ];
    wait_for_reset_required_stores(&home_path, &project_path, &expected_stale);
    let project_open = find_key(&status(&home_path, &project_path), "project_open");
    assert!(
        project_open
            .as_ref()
            .is_none_or(|open| open.is_null() || open["state"] == "completed"),
        "project open must not stall on a session-store verdict: {project_open:?}"
    );

    for scope in ["project", "user"] {
        let refused = super::cli_problem_envelope(
            &session_status(&home_path, &project_path, scope),
            &format!("{scope} session read over a stale store"),
        );
        super::assert_reset_required(&refused, &format!("{scope} session read"));
        assert_eq!(
            refused["problem"]["detail"]["authority"], "observations",
            "{scope} session read names the refused authority: {refused}"
        );
        assert_eq!(
            refused["problem"]["detail"]["remedy"], STALE_STORE_RESET,
            "{scope} session read names the scoped reset: {refused}"
        );
    }

    let doctor = tracedecay_command_with_home(&home_path)
        .arg("doctor")
        .current_dir(&project_path)
        .stdin(Stdio::null())
        .output()
        .expect("run doctor");
    let doctor_text = format!(
        "{}{}",
        String::from_utf8_lossy(&doctor.stdout),
        String::from_utf8_lossy(&doctor.stderr)
    );
    for store in [
        "profile sessions".to_owned(),
        format!("project sessions {project_id}"),
    ] {
        let line = format!(
            "Store {store} requires reset ({OBSERVATIONS_RESET_REASON}). Pending operator \
             action: run `{STALE_STORE_RESET}`"
        );
        assert!(
            doctor_text.contains(&line),
            "doctor omitted `{line}`:\n{doctor_text}"
        );
    }
    assert!(
        doctor_text.contains("pending operator action(s)") && doctor_text.contains("no issues."),
        "stale session stores are pending operator actions, not issues:\n{doctor_text}"
    );
    assert!(
        !doctor_text.contains("to fix most issues"),
        "doctor must not send a stale session store to `tracedecay install`:\n{doctor_text}"
    );
    assert!(
        !doctor_text.contains("Stalled"),
        "doctor must not report project open stalled on a session store:\n{doctor_text}"
    );

    let mut before_reset = BTreeMap::new();
    let (reset_status, reset_output) =
        super::run_scoped_reset(&home_path, &project_path, &mut daemon, || {
            before_reset = file_digests(&profile_root);
        });
    assert!(
        reset_status.success(),
        "the scoped reset failed:\n{reset_output}"
    );
    let after_reset = file_digests(&profile_root);
    let (stale_members, kept): (BTreeMap<_, _>, BTreeMap<_, _>) = before_reset
        .into_iter()
        .partition(|(relative, _)| is_session_store_member(relative, &project_store));
    assert!(
        stale_members.contains_key(Path::new("user-sessions.db"))
            && stale_members.contains_key(&project_store.join("sessions.db")),
        "both refused stores existed before the reset: {stale_members:#?}"
    );
    for relative in stale_members.keys() {
        assert!(
            !after_reset.contains_key(relative),
            "the scoped reset left refused store member {} behind",
            relative.display()
        );
    }
    // The lifecycle lock records whichever command holds the profile lease;
    // it is coordination, not stored data.
    let changed: Vec<_> = kept
        .iter()
        .filter(|(relative, _)| relative.as_path() != Path::new("lifecycle.lock"))
        .filter(|(relative, digest)| after_reset.get(*relative) != Some(digest))
        .map(|(relative, _)| relative.clone())
        .collect();
    assert_eq!(
        changed,
        Vec::<PathBuf>::new(),
        "the scoped reset touched files outside the refused stores:\n{reset_output}"
    );
    for store in [
        "profile sessions".to_owned(),
        format!("project sessions {project_id}"),
    ] {
        assert!(
            reset_output.contains(&format!("reset {store}")),
            "the scoped reset did not report resetting {store}:\n{reset_output}"
        );
    }

    let mut daemon = spawn_tracedecay_daemon_with(&home_path, |_| {});
    wait_for_code_index_hit(&home_path, &project_path, "probe");
    for scope in ["project", "user"] {
        let served = session_status(&home_path, &project_path, scope);
        assert!(
            find_key(&served, "problem").is_none_or(|problem| problem.is_null()),
            "the {scope} session store must serve after the scoped reset: {served}"
        );
    }
    assert_eq!(
        reset_required_stores(&home_path, &project_path),
        Vec::<Value>::new(),
        "no store stays refused after the scoped reset"
    );

    let _ = daemon.kill_and_wait();
}
