//! A profile authority (`global.db`) whose persisted shape this binary
//! refuses is a scoped-reset store: the daemon names it with
//! `tracedecay wipe --stale --yes`, that reset deletes exactly the profile
//! database and leaves every project store byte-identical, and each project
//! is registered again from its own store manifest.
//!
//! A physically spawned `tracedecay daemon run` registers a project and
//! publishes its code index. Its `global.db` is then given the registry shape
//! v1.0.0-beta.63 wrote: `graph_scopes` with the retired `db_relpath`
//! column. Over that profile the daemon must serve, report only the profile
//! authority as requiring reset, and refuse project reads with the typed
//! reset naming the scoped command: project route admission resolves
//! enrollment, aliases and remote-deletion tombstones through the registry.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use serde_json::{Value, json};

use super::stale_sessions_store_reset::{file_digests, run_doctor_json, wait_for_code_index_hit};
use crate::common::{
    canonical_existing_path, spawn_tracedecay_daemon_with, tracedecay_command_with_home,
};

const STALE_STORE_RESET: &str = "tracedecay wipe --stale --yes";
const REGISTRY_REASON: &str = "database error: table 'graph_scopes' has an incompatible number \
     of columns (operation: validate global database authority schema)";

/// Rewrites `graph_scopes` into the shape v1.0.0-beta.63 and earlier wrote,
/// with each scope's `db_relpath`, keeping its rows, indexes and triggers.
fn give_registry_the_released_graph_scope_shape(db_path: &Path) {
    let connection = rusqlite::Connection::open(db_path).expect("open the profile authority");
    let dependents: Vec<String> = connection
        .prepare(
            "SELECT sql FROM sqlite_schema WHERE tbl_name = 'graph_scopes' \
             AND type IN ('index', 'trigger') AND sql IS NOT NULL",
        )
        .expect("list graph scope dependents")
        .query_map([], |row| row.get(0))
        .expect("read graph scope dependents")
        .collect::<rusqlite::Result<_>>()
        .expect("graph scope dependents");
    connection
        .execute_batch(
            "PRAGMA foreign_keys = OFF;
             BEGIN;
             CREATE TABLE released_graph_scopes (
                 graph_scope_id TEXT PRIMARY KEY,
                 project_id TEXT NOT NULL,
                 store_id TEXT NOT NULL,
                 branch_name TEXT NOT NULL,
                 db_relpath TEXT NOT NULL,
                 parent_scope_id TEXT,
                 last_synced_at INTEGER,
                 writable INTEGER NOT NULL DEFAULT 1,
                 FOREIGN KEY(project_id) REFERENCES code_projects(project_id) ON DELETE CASCADE,
                 FOREIGN KEY(store_id) REFERENCES store_instances(store_id) ON DELETE CASCADE
             );
             INSERT INTO released_graph_scopes
                 SELECT scope.graph_scope_id, scope.project_id, scope.store_id,
                        scope.branch_name, store.store_relpath || '/tracedecay.db',
                        scope.parent_scope_id, scope.last_synced_at, scope.writable
                 FROM graph_scopes AS scope JOIN store_instances AS store USING (store_id);
             DROP TABLE graph_scopes;
             ALTER TABLE released_graph_scopes RENAME TO graph_scopes;
             COMMIT;",
        )
        .expect("give graph_scopes the released shape");
    for sql in dependents {
        connection
            .execute_batch(&sql)
            .expect("restore a graph scope dependent");
    }
    let released: i64 = connection
        .query_row(
            "SELECT count(*) FROM graph_scopes WHERE db_relpath LIKE 'projects/%/tracedecay.db'",
            [],
            |row| row.get(0),
        )
        .expect("count released graph scopes");
    assert_eq!(released, 1, "the registered project keeps its graph scope");
}

/// The stores `tracedecay doctor --json` names as pending resets, read from
/// the daemon's reset census.
fn doctor_store_resets(home: &Path, project: &Path) -> (Option<i32>, Vec<String>) {
    let (exit, report, _) = run_doctor_json(home, project);
    let pending = report["checks"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|check| check["level"] == "pending_operator_action")
        .filter_map(|check| check["message"].as_str())
        .filter(|message| message.starts_with("Store "))
        .map(str::to_owned)
        .collect();
    (exit, pending)
}

fn search_problem(home: &Path, project: &Path) -> Value {
    let envelope = super::cli_problem_envelope(
        &super::tool_call(
            home,
            project,
            "tracedecay_search",
            &json!({ "query": "probe", "format": "json" }),
        ),
        "code search over a reset-required profile authority",
    );
    super::assert_reset_required(
        &envelope,
        "code search over a reset-required profile authority",
    );
    envelope["problem"]["detail"].clone()
}

fn registered_project_ids(home: &Path, project: &Path) -> Vec<String> {
    let listed = super::typed_envelope(&super::tool_call(
        home,
        project,
        "tracedecay_project_list",
        &json!({ "format": "json" }),
    ));
    listed["projects"]
        .as_array()
        .unwrap_or_else(|| panic!("project_list returned no projects: {listed}"))
        .iter()
        .filter_map(|project| project["project_id"].as_str().map(str::to_owned))
        .collect()
}

#[test]
fn released_profile_authority_resets_alone_and_projects_register_again() {
    let home = tempfile::TempDir::new().expect("isolated home");
    let home_path = canonical_existing_path(home.path());
    let project = tempfile::TempDir::new().expect("project");
    let project_path = canonical_existing_path(project.path());
    let profile_root = home_path.join(".tracedecay");
    let project_id = tracedecay_runtime_core::storage::default_profile_project_id(&project_path);

    let mut daemon = spawn_tracedecay_daemon_with(&home_path, |_| {});
    super::initialize_project(&home_path, &project_path, "released-profile-authority");
    wait_for_code_index_hit(&home_path, &project_path, "probe");
    assert_eq!(
        registered_project_ids(&home_path, &project_path),
        std::slice::from_ref(&project_id)
    );
    daemon
        .kill_and_wait()
        .expect("stop the daemon that wrote the profile");
    give_registry_the_released_graph_scope_shape(&profile_root.join("global.db"));

    let mut daemon = spawn_tracedecay_daemon_with(&home_path, |_| {});
    let detail = search_problem(&home_path, &project_path);
    assert_eq!(
        detail,
        json!({
            "kind": "reset_required",
            "authority": "project registry",
            "found_version": null,
            "required_version": null,
            "reason": REGISTRY_REASON,
            "remedy": STALE_STORE_RESET,
        }),
        "a code read names the refused registry and the scoped reset"
    );
    assert_eq!(
        doctor_store_resets(&home_path, &project_path),
        (
            Some(75),
            vec![format!(
                "Store profile authority requires reset (project registry persisted shape \
                 requires reset: {REGISTRY_REASON}). Pending operator action: run \
                 `{STALE_STORE_RESET}`"
            )]
        ),
        "the daemon reports only the profile authority, with its scoped reset"
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
    let removed: Vec<&PathBuf> = before_reset
        .keys()
        .filter(|relative| !after_reset.contains_key(*relative))
        .collect();
    assert_eq!(
        removed,
        [
            &PathBuf::from("global.db"),
            &PathBuf::from("global.db-shm"),
            &PathBuf::from("global.db-wal")
        ],
        "the scoped reset deletes exactly the profile database:\n{reset_output}"
    );
    // The lifecycle lock records whichever command holds the profile lease;
    // it is coordination, not stored data.
    let changed: Vec<&PathBuf> = before_reset
        .iter()
        .filter(|(relative, _)| relative.as_path() != Path::new("lifecycle.lock"))
        .filter(|(relative, digest)| {
            after_reset
                .get(*relative)
                .is_some_and(|after| after != *digest)
        })
        .map(|(relative, _)| relative)
        .collect();
    assert_eq!(
        changed,
        Vec::<&PathBuf>::new(),
        "the scoped reset touched files outside the profile authority:\n{reset_output}"
    );
    let project_store = PathBuf::from("projects").join(&project_id);
    for store in [
        project_store.join("tracedecay.db"),
        project_store.join("sessions.db"),
        PathBuf::from("user-sessions.db"),
    ] {
        assert!(
            after_reset.contains_key(&store),
            "{} must survive the profile authority reset",
            store.display()
        );
    }
    let init_command = format!("tracedecay init {}", project_path.display());
    assert!(
        reset_output.contains(&format!(
            "the project registry is empty; register each project store again with:\n  \
             {init_command}\n"
        )),
        "the scoped reset did not print the registration command:\n{reset_output}"
    );

    let mut daemon = spawn_tracedecay_daemon_with(&home_path, |_| {});
    let init = tracedecay_command_with_home(&home_path)
        .args(["init"])
        .arg(&project_path)
        .current_dir(&home_path)
        .stdin(Stdio::null())
        .output()
        .expect("run the printed registration command");
    assert!(
        init.status.success(),
        "`{init_command}` failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&init.stdout),
        String::from_utf8_lossy(&init.stderr)
    );
    wait_for_code_index_hit(&home_path, &project_path, "probe");
    assert_eq!(
        registered_project_ids(&home_path, &project_path),
        [project_id],
        "the project registers again under the identity its store records"
    );
    assert_eq!(
        doctor_store_resets(&home_path, &project_path).1,
        Vec::<String>::new(),
        "no store requires reset after the profile authority is recreated"
    );

    let _ = daemon.kill_and_wait();
}
