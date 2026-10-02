//! The reset census names every refused registered store before any project
//! opens, so one `tracedecay wipe --stale --yes` resets a profile written by
//! an earlier release.
//!
//! Every registered store carries the project-registry tables, so a profile
//! from v1.0.0-beta.63 has the retired `graph_scopes.db_relpath` shape in
//! `global.db` and in each project's `sessions.db`. Project open stops at the
//! refused profile authority, so the census must inspect each project
//! sessions store from its manifest instead of waiting for its project to
//! open.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use super::stale_profile_authority_reset::{
    REGISTRY_REASON, STALE_STORE_RESET, doctor_store_resets,
    give_registry_the_released_graph_scope_shape, registered_project_ids,
};
use super::stale_sessions_store_reset::{file_digests, wait_for_code_index_hit};
use crate::common::{
    canonical_existing_path, spawn_tracedecay_daemon_with, tracedecay_command_with_home,
};

fn pending_reset(store: &str) -> String {
    format!(
        "Store {store} requires reset (project registry persisted shape requires reset: \
         {REGISTRY_REASON}). Pending operator action: run `{STALE_STORE_RESET}`"
    )
}

fn run_cli(home: &Path, args: &[&str], cwd: &Path) -> std::process::Output {
    tracedecay_command_with_home(home)
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .output()
        .expect("run tracedecay")
}

#[test]
fn one_scoped_reset_clears_the_profile_authority_and_every_project_sessions_store() {
    let home = tempfile::TempDir::new().expect("isolated home");
    let home_path = canonical_existing_path(home.path());
    let profile_root = home_path.join(".tracedecay");
    let projects: Vec<(tempfile::TempDir, PathBuf, String)> = (0..2)
        .map(|_| {
            let project = tempfile::TempDir::new().expect("project");
            let path = canonical_existing_path(project.path());
            let id = tracedecay_runtime_core::storage::default_profile_project_id(&path);
            (project, path, id)
        })
        .collect();
    let mut ids: Vec<&str> = projects.iter().map(|(_, _, id)| id.as_str()).collect();
    ids.sort_unstable();
    let first_project = projects[0].1.as_path();

    let mut daemon = spawn_tracedecay_daemon_with(&home_path, |_| {});
    for (_, path, _) in &projects {
        super::initialize_project(&home_path, path, "released-project-sessions");
        wait_for_code_index_hit(&home_path, path, "probe");
    }
    let mut registered = registered_project_ids(&home_path, first_project);
    registered.sort_unstable();
    assert_eq!(registered, ids);
    daemon
        .kill_and_wait()
        .expect("stop the daemon that wrote the profile");
    assert_eq!(
        give_registry_the_released_graph_scope_shape(&profile_root.join("global.db")),
        2,
        "the profile authority keeps both graph scopes"
    );
    for id in &ids {
        assert_eq!(
            give_registry_the_released_graph_scope_shape(
                &profile_root.join("projects").join(id).join("sessions.db")
            ),
            0,
            "a project sessions store records no graph scope"
        );
    }

    let mut daemon = spawn_tracedecay_daemon_with(&home_path, |_| {});
    let (exit, mut pending) = doctor_store_resets(&home_path, first_project);
    pending.sort_unstable();
    let mut expected = vec![
        pending_reset("profile authority"),
        pending_reset(&format!("project sessions {}", ids[0])),
        pending_reset(&format!("project sessions {}", ids[1])),
    ];
    expected.sort_unstable();
    assert_eq!(
        (exit, pending),
        (Some(75), expected),
        "one census names the profile authority and both project sessions stores"
    );

    let mut before_reset = BTreeMap::new();
    let (reset_status, reset_output) =
        super::run_scoped_reset(&home_path, first_project, &mut daemon, || {
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
    let mut expected_removed = vec![
        PathBuf::from("global.db"),
        PathBuf::from("global.db-shm"),
        PathBuf::from("global.db-wal"),
    ];
    for id in &ids {
        let store = PathBuf::from("projects").join(id);
        expected_removed.extend([
            store.join(".sessions.db.host-admission/meta.json"),
            store.join("sessions.db"),
            store.join("sessions.db-shm"),
            store.join("sessions.db-wal"),
            store.join("sessions.db.delivery-settlement-spool-v1/writer.v1.lock"),
        ]);
    }
    expected_removed.sort();
    assert_eq!(
        removed,
        expected_removed.iter().collect::<Vec<_>>(),
        "one scoped reset deletes exactly the three listed store families:\n{reset_output}"
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
        "the scoped reset touched files outside the listed stores:\n{reset_output}"
    );
    for id in &ids {
        let code_index = PathBuf::from("projects").join(id).join("tracedecay.db");
        assert!(
            after_reset.contains_key(&code_index),
            "{} must survive the reset",
            code_index.display()
        );
    }

    let mut daemon = spawn_tracedecay_daemon_with(&home_path, |_| {});
    let second = run_cli(&home_path, &["wipe", "--stale", "--yes"], first_project);
    assert_eq!(
        (
            second.status.code(),
            String::from_utf8_lossy(&second.stdout).into_owned(),
            String::from_utf8_lossy(&second.stderr).into_owned(),
        ),
        (
            Some(0),
            String::new(),
            "No store requires reset. Nothing was wiped.\n".to_owned()
        ),
        "a second scoped reset finds nothing pending"
    );
    for (_, path, _) in &projects {
        let init = run_cli(&home_path, &["init", &path.to_string_lossy()], &home_path);
        assert!(
            init.status.success(),
            "`tracedecay init {}` failed\nstdout:\n{}\nstderr:\n{}",
            path.display(),
            String::from_utf8_lossy(&init.stdout),
            String::from_utf8_lossy(&init.stderr)
        );
        wait_for_code_index_hit(&home_path, path, "probe");
    }
    let mut registered = registered_project_ids(&home_path, first_project);
    registered.sort_unstable();
    assert_eq!(
        registered, ids,
        "both projects register again after one reset"
    );
    assert_eq!(
        doctor_store_resets(&home_path, first_project).1,
        Vec::<String>::new(),
        "no store requires reset after one scoped reset"
    );

    let _ = daemon.kill_and_wait();
}
