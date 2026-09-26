//! A hard kill of the shipped daemon on either side of a code-generation seal.
//!
//! The seal batches its durability: every new segment is written under a
//! temporary name, flushed once, and only then named; the manifest and the
//! active pointer follow. A kill after the pointer is durable must restart on
//! the exact sealed bytes. A kill before it must leave the prior generation
//! published and no partial generation or temporary behind once the restarted
//! daemon rebuilds.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde_json::json;
use sha2::{Digest, Sha256};
use tracedecay_code_index_retention::code_index_generations::{
    code_generation_segments_root, code_text_artifacts_root, scoped_code_index_store_root,
};

use crate::code_index_journey::{
    RECEIPT_TIMEOUT, commit_all, exact_identity, git, initialize_tracedecay,
    stop_daemon_gracefully, tool, wait_for_terminal_generation,
};
use crate::common::{IsolatedEnv, daemon_socket_path, spawn_tracedecay_daemon_with};
use tracedecay_runtime_core::path_safety::canonical_existing_identity;

const PUBLICATION_TEMPORARY_PREFIXES: [&str; 2] =
    [".segment-publication.", ".evidence-pack-publication."];

fn scope_root(home: &Path, project: &Path) -> PathBuf {
    let layout =
        tracedecay_runtime_core::storage::resolve_layout(project, &home.join(".tracedecay"))
            .expect("profile-sharded project layout");
    scoped_code_index_store_root(&layout.data_root.join("code-index-v1"), project)
}

fn active_pointer(scope: &Path) -> Vec<u8> {
    fs::read(scope.join("active-code-generation-v1.json")).expect("active code generation pointer")
}

/// Content digest of every file in the sealed generation's durable family:
/// the active pointer, the generation manifests, the file segments, and the
/// text artifacts.
fn sealed_family(scope: &Path) -> BTreeMap<String, String> {
    let mut family = BTreeMap::new();
    family.insert(
        "active-code-generation-v1.json".to_owned(),
        hex::encode(Sha256::digest(active_pointer(scope))),
    );
    for (directory, prefix) in [
        (scope.join("code-generations-v1"), "generation-"),
        (code_generation_segments_root(scope), "segment-"),
        (code_text_artifacts_root(scope), "text-artifact-"),
    ] {
        for entry in fs::read_dir(&directory).expect("sealed family directory") {
            let entry = entry.expect("sealed family entry");
            let path = entry.path();
            if entry.file_name().to_string_lossy().starts_with(prefix) {
                family.insert(
                    path.strip_prefix(scope.parent().expect("scope parent"))
                        .expect("family path under the project code index")
                        .display()
                        .to_string(),
                    hex::encode(Sha256::digest(fs::read(&path).expect("sealed family file"))),
                );
            }
        }
    }
    family
}

fn publication_temporaries(scope: &Path) -> Vec<String> {
    fs::read_dir(code_generation_segments_root(scope))
        .expect("segments directory")
        .map(|entry| {
            entry
                .expect("segments entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .filter(|name| {
            PUBLICATION_TEMPORARY_PREFIXES
                .iter()
                .any(|prefix| name.starts_with(prefix))
        })
        .collect()
}

fn segments_entry_count(scope: &Path) -> usize {
    fs::read_dir(code_generation_segments_root(scope))
        .expect("segments directory")
        .count()
}

fn write_batch(project: &Path, files: u32, symbols: u32) {
    let batch = project.join("src/batch");
    fs::create_dir_all(&batch).expect("batch directory");
    for file_index in 0..files {
        let mut source = String::new();
        for symbol_index in 0..symbols {
            writeln!(
                source,
                "pub fn crash_probe_{file_index:04}_{symbol_index:03}(input: u32) -> u32 {{ input + {symbol_index} }}"
            )
            .expect("format batch source");
        }
        fs::write(batch.join(format!("file_{file_index:04}.rs")), source)
            .expect("write batch source");
    }
}

fn initialize_repository(project: &Path) -> String {
    fs::create_dir_all(project.join("src")).expect("fixture source directory");
    fs::write(
        project.join("src/lib.rs"),
        "pub fn crash_anchor_symbol() -> &'static str { \"anchor\" }\n",
    )
    .expect("fixture anchor source");
    write_batch(project, 8, 16);
    git(project, &["init", "--quiet", "--initial-branch=main"]);
    commit_all(project, "sealed crash fixture")
}

#[tokio::test]
async fn sigkill_after_the_seal_restarts_on_the_identical_sealed_generation() {
    let (environment, project) = IsolatedEnv::acquire().await;
    let project = canonical_existing_identity(&project).expect("canonical fixture project");
    let revision = initialize_repository(&project);
    let socket = daemon_socket_path(environment.home());
    let mut daemon = spawn_tracedecay_daemon_with(environment.home(), |_| {});
    let identity = exact_identity(
        &project,
        initialize_tracedecay(environment.home(), &project),
    );
    tracedecay_project::product_runtime::register_fixture_product_runtime();
    let handshake =
        tracedecay::daemon::handshake_for_current_client(Some(project.clone()), None, false, false)
            .expect("production daemon handshake");

    let sealed = wait_for_terminal_generation(
        &socket,
        &handshake,
        &project,
        &identity,
        "refs/heads/main",
        Some(&revision),
        None,
        "crash_anchor_symbol",
        Some("src/lib.rs"),
    )
    .await;
    let scope = scope_root(environment.home(), &project);
    let before_kill = sealed_family(&scope);

    let killed = daemon.kill_and_wait().expect("SIGKILL the sealed daemon");
    assert!(
        !killed.success(),
        "SIGKILL must not look like a graceful exit"
    );
    assert_eq!(
        sealed_family(&scope),
        before_kill,
        "the kill changed the sealed generation's durable bytes"
    );

    daemon = spawn_tracedecay_daemon_with(environment.home(), |_| {});
    let restarted = wait_for_terminal_generation(
        &socket,
        &handshake,
        &project,
        &identity,
        "refs/heads/main",
        Some(&revision),
        None,
        "crash_probe_0007_015",
        Some("src/batch/file_0007.rs"),
    )
    .await;
    assert_eq!(
        restarted.generation_id, sealed.generation_id,
        "the restarted daemon must serve the generation sealed before the kill"
    );
    assert_eq!(
        sealed_family(&scope),
        before_kill,
        "the restarted daemon rebuilt or rewrote the sealed generation instead of serving it"
    );
    stop_daemon_gracefully(&mut daemon);
}

#[tokio::test]
async fn sigkill_during_the_seal_keeps_the_prior_generation_published() {
    let (environment, project) = IsolatedEnv::acquire().await;
    let project = canonical_existing_identity(&project).expect("canonical fixture project");
    let revision = initialize_repository(&project);
    let socket = daemon_socket_path(environment.home());
    let mut daemon = spawn_tracedecay_daemon_with(environment.home(), |_| {});
    let identity = exact_identity(
        &project,
        initialize_tracedecay(environment.home(), &project),
    );
    tracedecay_project::product_runtime::register_fixture_product_runtime();
    let handshake =
        tracedecay::daemon::handshake_for_current_client(Some(project.clone()), None, false, false)
            .expect("production daemon handshake");
    let prior = wait_for_terminal_generation(
        &socket,
        &handshake,
        &project,
        &identity,
        "refs/heads/main",
        Some(&revision),
        None,
        "crash_anchor_symbol",
        Some("src/lib.rs"),
    )
    .await;
    let scope = scope_root(environment.home(), &project);
    let prior_pointer = active_pointer(&scope);
    let prior_family = sealed_family(&scope);
    let prior_entries = segments_entry_count(&scope);

    write_batch(&project, 128, 16);
    let batch_revision = commit_all(&project, "install the batch the killed seal was writing");
    let receipt = tool(
        &socket,
        &handshake,
        "tracedecay_admin_sync",
        json!({ "format": "json" }),
    )
    .await;
    assert_eq!(receipt["status"], "queued", "refresh receipt: {receipt}");
    // The seal's first segment write is the first new entry beside the prior
    // generation's segments; kill the daemon while that seal is in flight.
    let deadline = Instant::now() + RECEIPT_TIMEOUT;
    while segments_entry_count(&scope) < prior_entries + 2 {
        assert!(
            Instant::now() < deadline,
            "the refresh never started sealing its segments"
        );
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    let killed = daemon.kill_and_wait().expect("SIGKILL the sealing daemon");
    assert!(
        !killed.success(),
        "SIGKILL must not look like a graceful exit"
    );
    assert_eq!(
        active_pointer(&scope),
        prior_pointer,
        "a kill inside the seal must leave the prior generation's pointer in place"
    );
    let after_kill = sealed_family(&scope);
    for (path, digest) in &prior_family {
        assert_eq!(
            after_kill.get(path),
            Some(digest),
            "the kill damaged the prior generation's durable file {path}"
        );
    }

    daemon = spawn_tracedecay_daemon_with(environment.home(), |_| {});
    let rebuilt = wait_for_terminal_generation(
        &socket,
        &handshake,
        &project,
        &identity,
        "refs/heads/main",
        Some(&batch_revision),
        Some(&prior.generation_id),
        "crash_probe_0127_015",
        Some("src/batch/file_0127.rs"),
    )
    .await;
    assert_ne!(rebuilt.generation_id, prior.generation_id);
    assert_eq!(
        publication_temporaries(&scope),
        Vec::<String>::new(),
        "the restarted daemon must collect the killed seal's temporaries"
    );
    stop_daemon_gracefully(&mut daemon);
}
