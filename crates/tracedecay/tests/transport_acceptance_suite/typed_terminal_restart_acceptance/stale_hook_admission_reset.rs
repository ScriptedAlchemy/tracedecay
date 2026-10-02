//! A Hook V2 admission ledger in the shape a pre-log binary wrote is never
//! imported. A physically spawned daemon refuses it, reports the refusal in
//! its reset census without taking code intelligence down, and
//! `tracedecay wipe --stale --yes` deletes exactly that project's hook
//! admission state.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use super::stale_sessions_store_reset::{
    file_digests, reset_required_stores, wait_for_code_index_hit, wait_for_reset_required_stores,
};
use crate::common::{canonical_existing_path, spawn_tracedecay_daemon_with};

#[test]
fn a_pre_log_hook_admission_ledger_is_refused_until_wipe_stale_resets_it() {
    let home = tempfile::TempDir::new().expect("isolated home");
    let home_path = canonical_existing_path(home.path());
    let project = tempfile::TempDir::new().expect("project");
    let project_path = canonical_existing_path(project.path());
    let profile_root = home_path.join(".tracedecay");
    let project_id = tracedecay_runtime_core::storage::default_profile_project_id(&project_path);
    let hook_admissions = PathBuf::from("projects")
        .join(&project_id)
        .join("hook-v2-admissions");
    let pre_log_records = hook_admissions.join("claude").join("admissions.v1.bin");

    let mut daemon = spawn_tracedecay_daemon_with(&home_path, |_| {});
    super::initialize_project(&home_path, &project_path, "pre-log-hook-ledger");
    wait_for_code_index_hit(&home_path, &project_path, "probe");
    daemon
        .kill_and_wait()
        .expect("stop the daemon that wrote the profile");

    // A TDL1 header and one record body, as the pre-log ledger wrote them.
    let mut pre_log = b"TDL1\x01\x00".to_vec();
    pre_log.extend_from_slice(&[9u8; 64]);
    let pre_log_path = profile_root.join(&pre_log_records);
    std::fs::create_dir_all(pre_log_path.parent().expect("ledger root"))
        .expect("create the pre-log ledger root");
    std::fs::write(&pre_log_path, &pre_log).expect("write the pre-log ledger");

    let mut daemon = spawn_tracedecay_daemon_with(&home_path, |_| {});
    wait_for_code_index_hit(&home_path, &project_path, "probe");
    wait_for_reset_required_stores(
        &home_path,
        &project_path,
        &[json!({
            "store": format!("project hook admissions {project_id}"),
            "authority": "hook admission ledger",
            "found_version": null,
            "required_version": null,
            "reason": "hook admission ledger holds a pre-log shape this binary does not open",
            "remedy": "tracedecay wipe --stale --yes",
        })],
    );
    assert_eq!(
        std::fs::read(&pre_log_path).expect("the refused ledger stays"),
        pre_log,
        "a refused ledger is neither imported nor rewritten"
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
    assert!(
        reset_output.contains(&format!("reset project hook admissions {project_id}")),
        "the scoped reset did not report the hook admission reset:\n{reset_output}"
    );
    let after_reset = file_digests(&profile_root);
    let removed: Vec<&PathBuf> = before_reset
        .keys()
        .filter(|relative| !after_reset.contains_key(*relative))
        .collect();
    assert!(
        removed.contains(&&pre_log_records)
            && removed
                .iter()
                .all(|relative| relative.starts_with(&hook_admissions)),
        "the reset removes exactly the project's hook admission state: {removed:#?}"
    );
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
        "the scoped reset touched files outside the refused hook admission state"
    );

    let mut daemon = spawn_tracedecay_daemon_with(&home_path, |_| {});
    wait_for_code_index_hit(&home_path, &project_path, "probe");
    assert_eq!(
        reset_required_stores(&home_path, &project_path),
        Vec::<Value>::new(),
        "no store stays refused after the scoped reset"
    );
    let _ = daemon.kill_and_wait();
}
