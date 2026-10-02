//! Session stores whose persisted shape this binary refuses are held as typed
//! reset states without taking code intelligence down with them, and their
//! scoped reset deletes exactly those stores.
//!
//! A physically spawned `tracedecay daemon run` first writes a real profile
//! and project session store. Stores are then given a shape a released binary
//! left behind: observation rows written before the unified identity, an LCM
//! schema version, a git correlation schema version, or a workflow schema
//! identity other than the one this binary writes. Over that profile the
//! project must still open, the MCP host must initialize and list tools, code
//! search and callers must answer, session reads against a refused store must
//! return the typed `reset_required` refusal naming
//! `tracedecay wipe --stale --yes`, and `tracedecay doctor` must count it as a
//! pending operator action naming that command. The scoped reset then deletes
//! exactly the refused stores, every other profile file stays byte-identical,
//! and the restarted daemon serves sessions from empty stores.

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
pub(super) fn file_digests(root: &Path) -> BTreeMap<PathBuf, String> {
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

/// Whether `relative` belongs to the project session store, or to the profile
/// session store when `with_profile_store`.
fn is_session_store_member(
    relative: &Path,
    project_store: &Path,
    with_profile_store: bool,
) -> bool {
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
        || (with_profile_store
            && (name.starts_with("user-sessions.")
                || name.starts_with(".user-sessions.db.host-admission")))
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

pub(super) fn reset_required_stores(home: &Path, project: &Path) -> Vec<Value> {
    let status = status(home, project);
    let mut stores = find_key(&status, "reset_required_stores")
        .and_then(|stores| stores.as_array().cloned())
        .unwrap_or_else(|| panic!("tracedecay_status omitted reset_required_stores: {status}"));
    stores.sort_by(|left, right| left["store"].as_str().cmp(&right["store"].as_str()));
    stores
}

pub(super) fn wait_for_reset_required_stores(home: &Path, project: &Path, expected: &[Value]) {
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

pub(super) fn wait_for_code_index_hit(home: &Path, project: &Path, symbol: &str) {
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
pub(super) fn mcp_initialize_and_list_tools(home: &Path, project: &Path) -> (Value, Value) {
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

/// Rewrites exactly one recorded shape row of a stopped session store.
fn execute_once(db_path: &Path, sql: &str) {
    let connection = rusqlite::Connection::open(db_path).expect("open the session store");
    assert_eq!(
        connection.execute(sql, []).expect("age the session store"),
        1,
        "`{sql}` rewrites exactly one recorded shape row of {}",
        db_path.display()
    );
}

/// One way a released binary's session store differs from the shape this
/// binary writes, and the exact census entry the daemon reports for it.
struct SessionStoreRefusal {
    /// Gives one stopped session store the released shape.
    age: fn(&Path),
    /// Also ages the profile session store, not only the project's.
    ages_profile_store: bool,
    authority: &'static str,
    found_version: Value,
    required_version: Value,
    reason: &'static str,
    /// A session tool reading the refused store.
    session_tool: &'static str,
    session_tool_args: fn() -> Value,
}

/// Ages the session stores `refusal` names on a stopped daemon, then proves
/// code intelligence and MCP serve over them, session reads against them
/// refuse typed while every admissible session store keeps serving, doctor
/// counts each as a pending operator action, and `wipe --stale` deletes
/// exactly those stores and leaves every other profile file byte-identical.
fn refused_session_stores_serve_code_until_their_scoped_reset(refusal: &SessionStoreRefusal) {
    let home = tempfile::TempDir::new().expect("isolated home");
    let home_path = canonical_existing_path(home.path());
    let project = tempfile::TempDir::new().expect("project");
    let project_path = canonical_existing_path(project.path());
    let profile_root = home_path.join(".tracedecay");
    let project_id = tracedecay_runtime_core::storage::default_profile_project_id(&project_path);
    let project_store = PathBuf::from("projects").join(&project_id);
    let mut aged = vec![("project", format!("project sessions {project_id}"))];
    if refusal.ages_profile_store {
        aged.insert(0, ("user", "profile sessions".to_owned()));
    }
    let admissible_scopes: Vec<&str> = ["project", "user"]
        .into_iter()
        .filter(|scope| aged.iter().all(|(aged_scope, _)| aged_scope != scope))
        .collect();

    let mut daemon = spawn_tracedecay_daemon_with(&home_path, |_| {});
    super::initialize_project(&home_path, &project_path, "refused-session-stores");
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

    (refusal.age)(&profile_root.join(&project_store).join("sessions.db"));
    if refusal.ages_profile_store {
        (refusal.age)(&profile_root.join("user-sessions.db"));
    }

    let mut daemon = spawn_tracedecay_daemon_with(&home_path, |_| {});
    let (initialize, tools) = mcp_initialize_and_list_tools(&home_path, &project_path);
    assert_eq!(
        initialize.get("error"),
        None,
        "MCP initialize must serve over refused session stores: {initialize}"
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
        "callers must serve over refused session stores"
    );
    assert_eq!(
        code_read_outcome(
            &home_path,
            &project_path,
            "tracedecay_file_dependents",
            &json!({ "file": "src/lib.rs", "format": "json" }),
        ),
        json!("evidence"),
        "file dependents must serve over refused session stores"
    );
    let expected_census: Vec<Value> = aged
        .iter()
        .map(|(_, store)| {
            json!({
                "store": store,
                "authority": refusal.authority,
                "found_version": refusal.found_version,
                "required_version": refusal.required_version,
                "reason": refusal.reason,
                "remedy": STALE_STORE_RESET,
            })
        })
        .collect();
    wait_for_reset_required_stores(&home_path, &project_path, &expected_census);
    let project_open = find_key(&status(&home_path, &project_path), "project_open");
    assert!(
        project_open
            .as_ref()
            .is_none_or(|open| open.is_null() || open["state"] == "completed"),
        "project open must not stall on a session-store verdict: {project_open:?}"
    );

    for (scope, _) in &aged {
        let refused = super::cli_problem_envelope(
            &session_status(&home_path, &project_path, scope),
            &format!("{scope} session read over a refused store"),
        );
        super::assert_reset_required(&refused, &format!("{scope} session read"));
        assert_eq!(
            (
                &refused["problem"]["detail"]["authority"],
                &refused["problem"]["detail"]["remedy"]
            ),
            (&json!(refusal.authority), &json!(STALE_STORE_RESET)),
            "{scope} session read names the refused authority and the scoped reset: {refused}"
        );
    }
    for scope in &admissible_scopes {
        let served = session_status(&home_path, &project_path, scope);
        assert!(
            find_key(&served, "problem").is_none_or(|problem| problem.is_null()),
            "the admissible {scope} session store keeps serving: {served}"
        );
    }
    let session_tool_problem = || {
        find_key(
            &super::tool_call(
                &home_path,
                &project_path,
                refusal.session_tool,
                &(refusal.session_tool_args)(),
            ),
            "problem",
        )
        .filter(|problem| !problem.is_null())
        .map(|problem| problem["kind"].clone())
    };
    assert_eq!(
        session_tool_problem(),
        Some(json!("reset_required")),
        "{} must refuse typed over the refused store",
        refusal.session_tool
    );

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
    for (_, store) in &aged {
        let line = format!(
            "Store {store} requires reset ({}). Pending operator action: run \
             `{STALE_STORE_RESET}`",
            refusal.reason
        );
        assert!(
            doctor_text.contains(&line),
            "doctor omitted `{line}`:\n{doctor_text}"
        );
    }
    assert!(
        doctor_text.contains("pending operator action(s)") && doctor_text.contains("no issues."),
        "refused session stores are pending operator actions, not issues:\n{doctor_text}"
    );
    assert!(
        !doctor_text.contains("to fix most issues"),
        "doctor must not send a refused session store to `tracedecay install`:\n{doctor_text}"
    );
    assert!(
        !doctor_text.contains("Stalled"),
        "doctor must not report project open stalled on a session store:\n{doctor_text}"
    );
    let (exit, report, stderr) = run_doctor_json(&home_path, &project_path);
    assert_eq!(
        (exit, &report["daemon_findings"]["state"], &report["issues"]),
        (Some(75), &json!("observed"), &json!(0)),
        "a reset-required session store leaves the canonical report serving and is \
         only a pending operator action:\n{stderr}"
    );
    let mut pending: Vec<&str> = report["checks"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|check| check["level"] == "pending_operator_action")
        .filter_map(|check| check["message"].as_str())
        .collect();
    pending.sort_unstable();
    let mut expected_pending: Vec<String> = aged
        .iter()
        .map(|(_, store)| {
            format!(
                "Store {store} requires reset ({}). Pending operator action: run \
                 `{STALE_STORE_RESET}`",
                refusal.reason
            )
        })
        .collect();
    expected_pending.sort_unstable();
    assert_eq!(pending, expected_pending, "{stderr}");

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
    let (refused_members, kept): (BTreeMap<_, _>, BTreeMap<_, _>) =
        before_reset.into_iter().partition(|(relative, _)| {
            is_session_store_member(relative, &project_store, refusal.ages_profile_store)
        });
    assert!(
        refused_members.contains_key(&project_store.join("sessions.db"))
            && (refused_members.contains_key(Path::new("user-sessions.db"))
                || kept.contains_key(Path::new("user-sessions.db"))),
        "every session store existed before the reset: {refused_members:#?}"
    );
    for relative in refused_members.keys() {
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
    for (_, store) in &aged {
        assert!(
            reset_output.contains(&format!("reset {store}")),
            "the scoped reset did not report resetting {store}:\n{reset_output}"
        );
    }
    assert!(
        refusal.ages_profile_store || !reset_output.contains("reset profile sessions"),
        "the scoped reset must leave the admissible profile session store:\n{reset_output}"
    );

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
        session_tool_problem(),
        None,
        "{} must serve after the scoped reset",
        refusal.session_tool
    );
    assert_eq!(
        reset_required_stores(&home_path, &project_path),
        Vec::<Value>::new(),
        "no store stays refused after the scoped reset"
    );

    let _ = daemon.kill_and_wait();
}

#[test]
fn stale_session_stores_refuse_sessions_only_until_their_scoped_reset() {
    refused_session_stores_serve_code_until_their_scoped_reset(&SessionStoreRefusal {
        age: seed_pre_unified_observation_rows,
        ages_profile_store: true,
        authority: "observations",
        found_version: Value::Null,
        required_version: Value::Null,
        reason: OBSERVATIONS_RESET_REASON,
        session_tool: "tracedecay_lcm_grep",
        session_tool_args: || json!({ "query": "probe", "format": "json" }),
    });
}

#[test]
fn project_session_store_at_another_lcm_schema_version_refuses_sessions_only() {
    refused_session_stores_serve_code_until_their_scoped_reset(&SessionStoreRefusal {
        age: |db| {
            execute_once(
                db,
                "UPDATE session_schema_migrations SET version = 12 WHERE name = 'lcm'",
            );
        },
        ages_profile_store: false,
        authority: "LCM",
        found_version: json!(12),
        required_version: json!(13),
        reason: "LCM profile schema 12 is incompatible with required schema 13; reset the profile",
        session_tool: "tracedecay_lcm_grep",
        session_tool_args: || json!({ "query": "probe", "format": "json" }),
    });
}

#[test]
fn project_session_store_at_another_git_correlation_version_refuses_sessions_only() {
    refused_session_stores_serve_code_until_their_scoped_reset(&SessionStoreRefusal {
        age: |db| {
            execute_once(
                db,
                "UPDATE session_schema_migrations SET version = 5 WHERE name = 'git_correlation'",
            );
        },
        ages_profile_store: false,
        authority: "git correlation",
        found_version: json!(5),
        required_version: json!(6),
        reason: "git correlation profile schema 5 is incompatible with required schema 6; reset \
                 the profile",
        session_tool: "tracedecay_sessions_for",
        session_tool_args: || json!({ "git_ref": "branch", "value": "main", "format": "json" }),
    });
}

#[test]
fn project_session_store_with_another_workflow_schema_identity_refuses_sessions_only() {
    refused_session_stores_serve_code_until_their_scoped_reset(&SessionStoreRefusal {
        // A workflow schema written from another table contract.
        age: |db| {
            execute_once(
                db,
                "UPDATE workflow_schema SET definition_digest = \
                 'sha256:0000000000000000000000000000000000000000000000000000000000000000'",
            );
        },
        ages_profile_store: false,
        authority: "workflow",
        found_version: Value::Null,
        required_version: Value::Null,
        reason: "workflow persisted shape requires reset: workflow schema identity does not \
                 match the final contract",
        session_tool: "tracedecay_workflow_list_definitions",
        session_tool_args: || json!({}),
    });
}

/// Records one Cursor identity-collision refusal in a stopped session store's
/// cursor-advance ledger, the row the daemon's admission writes when it
/// settles a colliding record past its frontier.
fn seed_cursor_identity_collision_refusal(db_path: &Path, project_id: &str) {
    let scope_json = json!({ "kind": "project", "project_id": project_id }).to_string();
    rusqlite::Connection::open(db_path)
        .expect("open the session store")
        .execute(
            "INSERT INTO source_cursor_advances \
             (source_json, scope_json, coverage_json, reason, receipt_id) VALUES (\
             '{\"provider\":\"cursor\",\"session_id\":\"445777ad-0c9a-4c0e-bb98-7e8f7fb500ce\"}', \
             ?1, \
             '{\"generation\":7,\"ordering_domain\":\"file_bytes\",\
               \"range\":{\"start\":364052,\"end\":364900}}', \
             'observation_identity_collision', NULL)",
            [scope_json],
        )
        .expect("seed the refusal");
}

/// One `tracedecay doctor --json` run: exit code, document, and stderr.
pub(super) fn run_doctor_json(home: &Path, project: &Path) -> (Option<i32>, Value, String) {
    let doctor = tracedecay_command_with_home(home)
        .args(["doctor", "--json"])
        .current_dir(project)
        .stdin(Stdio::null())
        .output()
        .expect("run doctor");
    let stderr = String::from_utf8_lossy(&doctor.stderr).into_owned();
    let report: Value = serde_json::from_slice(&doctor.stdout)
        .unwrap_or_else(|error| panic!("doctor --json printed no document ({error}):\n{stderr}"));
    (doctor.status.code(), report, stderr)
}

/// A restarted daemon has no project open until something asks for one. A
/// lone `doctor` must open the registered project itself and report the
/// canonical findings, not a mount that never starts.
#[test]
fn doctor_alone_on_a_fresh_daemon_opens_the_project_and_reports_findings() {
    let home = tempfile::TempDir::new().expect("isolated home");
    let home_path = canonical_existing_path(home.path());
    let project = tempfile::TempDir::new().expect("project");
    let project_path = canonical_existing_path(project.path());

    let mut daemon = spawn_tracedecay_daemon_with(&home_path, |_| {});
    super::initialize_project(&home_path, &project_path, "cold-doctor");
    daemon
        .kill_and_wait()
        .expect("stop the daemon that registered the project");

    let mut daemon = spawn_tracedecay_daemon_with(&home_path, |_| {});
    let (_, report, stderr) = run_doctor_json(&home_path, &project_path);
    assert_eq!(
        report["daemon_findings"]["state"], "observed",
        "the first doctor on a fresh daemon must reach the canonical report:\n{stderr}"
    );

    let _ = daemon.kill_and_wait();
}

/// `tracedecay doctor --json` exit code and its ingest-coverage finding.
fn doctor_ingest_coverage(home: &Path, project: &Path) -> (Option<i32>, Value, String) {
    let (exit, report, stderr) = run_doctor_json(home, project);
    let finding = report["daemon_findings"]["payload"]["entries"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|entry| &entry["finding"])
        .find(|finding| {
            finding["evidence"][0]["reference"]
                .as_str()
                .is_some_and(|reference| reference.starts_with("observability.ingest-coverage."))
        })
        .cloned()
        .unwrap_or_else(|| panic!("doctor reported no ingest-coverage finding: {report}"));
    (
        exit,
        json!({
            "state": finding["state"],
            "reference": finding["evidence"][0]["reference"],
            "statement": finding["coverage"]["statement"],
        }),
        stderr,
    )
}

/// A durable Cursor refusal is named with its typed cause, session, range,
/// and the fact that nothing needs doing, without making doctor fail; the
/// scoped reset of its store drops it with the store.
#[test]
fn doctor_names_a_live_refusal_informationally_and_drops_it_with_its_reset_store() {
    let home = tempfile::TempDir::new().expect("isolated home");
    let home_path = canonical_existing_path(home.path());
    let project = tempfile::TempDir::new().expect("project");
    let project_path = canonical_existing_path(project.path());
    let project_id = tracedecay_runtime_core::storage::default_profile_project_id(&project_path);
    let project_sessions = home_path
        .join(".tracedecay")
        .join("projects")
        .join(&project_id)
        .join("sessions.db");

    let mut daemon = spawn_tracedecay_daemon_with(&home_path, |_| {});
    super::initialize_project(&home_path, &project_path, "refusal-census");
    wait_for_code_index_hit(&home_path, &project_path, "probe");
    daemon
        .kill_and_wait()
        .expect("stop the daemon that wrote the profile");
    seed_cursor_identity_collision_refusal(&project_sessions, &project_id);

    let mut daemon = spawn_tracedecay_daemon_with(&home_path, |_| {});
    wait_for_code_index_hit(&home_path, &project_path, "probe");
    let (exit, finding, stderr) = doctor_ingest_coverage(&home_path, &project_path);
    assert_eq!(
        finding,
        json!({
            "state": "healthy_complete_coverage",
            "reference": "observability.ingest-coverage.refused-informational",
            "statement": "durable ingest coverage converged past 1 refused source record(s), \
                informational, nothing needs doing: each was skipped by design and re-reading \
                it would refuse it again; cursor session 445777ad-0c9a-4c0e-bb98-7e8f7fb500ce \
                range 364052..364900 observation_identity_collision (a different record with \
                the same identity is already retained)",
        }),
        "{stderr}"
    );
    assert_eq!(
        exit,
        Some(0),
        "an informational refusal is not an issue:\n{stderr}"
    );
    assert!(
        !stderr.contains("config error") && !stderr.contains("to fix most issues"),
        "doctor must not label or route an informational refusal:\n{stderr}"
    );

    daemon
        .kill_and_wait()
        .expect("stop the daemon before aging its store");
    seed_pre_unified_observation_rows(&project_sessions);
    let mut daemon = spawn_tracedecay_daemon_with(&home_path, |_| {});
    wait_for_reset_required_stores(
        &home_path,
        &project_path,
        &[json!({
            "store": format!("project sessions {project_id}"),
            "authority": "observations",
            "found_version": null,
            "required_version": null,
            "reason": OBSERVATIONS_RESET_REASON,
            "remedy": STALE_STORE_RESET,
        })],
    );
    let (reset_status, reset_output) =
        super::run_scoped_reset(&home_path, &project_path, &mut daemon, || {});
    assert!(
        reset_status.success(),
        "the scoped reset failed:\n{reset_output}"
    );
    let mut daemon = spawn_tracedecay_daemon_with(&home_path, |_| {});
    wait_for_code_index_hit(&home_path, &project_path, "probe");
    let (exit, finding, stderr) = doctor_ingest_coverage(&home_path, &project_path);
    assert_eq!(
        (exit, finding),
        (
            Some(0),
            json!({
                "state": "healthy_complete_coverage",
                "reference": "observability.ingest-coverage.converged",
                "statement": "durable ingest coverage records no refused source records",
            })
        ),
        "the reset store's refusal must leave with it:\n{stderr}"
    );

    let _ = daemon.kill_and_wait();
}
