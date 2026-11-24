//! Superseded sealed code-graph generations are retired by the daemon's own
//! scheduled maintenance, not only by `projects forget` and a fresh `init`.
//!
//! A publication supersedes the prior generation's sealed artifact under
//! `tracedecay.sealed/`. The code-index retention pass collects the superseded
//! generation, hard-links it into the graph replay pool, and queues a durable
//! release event; the daemon's graph reconciler consumes that event and
//! retires the sealed directory. This journey proves the whole path through
//! the shipped daemon, using isolated fixtures only.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use tracedecay_configuration::RetentionConfig;
use tracedecay_maintenance::tick::MaintenanceCadence;
use tracedecay_runtime_core::path_safety::canonical_existing_identity;

use crate::code_index_journey::{
    RECEIPT_TIMEOUT, deliver_save, exact_identity, git, initialize_tracedecay,
    stop_daemon_gracefully, wait_for_terminal_generation,
};
use crate::common::{IsolatedHome, daemon_socket_path, spawn_tracedecay_daemon_with};

/// How many superseded sealed generations to build before expecting
/// collection. More than three proves collection is not a single-rollover
/// accident.
const SUPERSEDED_GENERATIONS: usize = 5;

fn project_store_root(home: &Path, project: &Path) -> PathBuf {
    tracedecay_runtime_core::storage::resolve_layout(project, &home.join(".tracedecay"))
        .expect("profile-sharded project layout")
        .data_root
}

/// The sealed generation directory names the graph store currently holds.
fn sealed_generation_names(home: &Path, project: &Path) -> BTreeSet<String> {
    let root = project_store_root(home, project).join("tracedecay.sealed");
    match fs::read_dir(&root) {
        Ok(entries) => entries
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
            .filter_map(|entry| {
                let name = entry.file_name();
                let name = name.to_str()?;
                (name.len() == 64 && name.bytes().all(|byte| byte.is_ascii_hexdigit()))
                    .then(|| name.to_owned())
            })
            .collect(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => BTreeSet::new(),
        Err(error) => panic!("read sealed store '{}': {error}", root.display()),
    }
}

fn commit_fixture(project: &Path, message: &str) -> String {
    git(project, &["add", "-A"]);
    let staged = git(project, &["diff", "--cached", "--name-only"]);
    assert!(
        !staged.is_empty(),
        "fixture staged nothing before commit {message:?}"
    );
    git(
        project,
        &[
            "-c",
            "user.name=TraceDecay Test",
            "-c",
            "user.email=tracedecay-test@example.invalid",
            "commit",
            "--quiet",
            "-m",
            message,
        ],
    );
    git(project, &["rev-parse", "HEAD"])
}

fn initialize_repository(project: &Path) -> String {
    fs::create_dir_all(project.join("src")).expect("fixture source directory");
    fs::write(
        project.join("src/lib.rs"),
        "pub fn sealed_retention_anchor_0() -> &'static str { \"anchor\" }\n",
    )
    .expect("fixture anchor source");
    git(project, &["init", "--quiet", "--initial-branch=main"]);
    commit_fixture(project, "sealed retention fixture")
}

#[tokio::test]
async fn superseded_sealed_generations_collect_to_live_plus_one() {
    let (environment, project) = IsolatedHome::new();
    let project = canonical_existing_identity(&project).expect("canonical fixture project");
    let mut revision = initialize_repository(&project);
    let socket = daemon_socket_path(environment.home());
    let mut daemon = spawn_tracedecay_daemon_with(environment.home(), |_| {});
    let identity = exact_identity(
        &project,
        initialize_tracedecay(environment.home(), &project),
    );
    tracedecay_project::product_runtime::register_fixture_product_runtime();
    let handshake = crate::common::handshake_for_current_client(
        environment.profile(),
        Some(project.clone()),
        None,
        false,
        false,
    )
    .expect("production daemon handshake");

    let mut prior = None;
    let mut first_sealed = BTreeSet::new();
    for index in 0..SUPERSEDED_GENERATIONS {
        if index > 0 {
            fs::write(
                project.join("src/lib.rs"),
                format!(
                    "pub fn sealed_retention_anchor_{index}() -> &'static str {{ \"anchor\" }}\n"
                ),
            )
            .expect("write successor source");
            revision = commit_fixture(&project, &format!("sealed retention generation {index}"));
            deliver_save(environment.profile(), &project, &["src/lib.rs"]).await;
        }
        let receipt = wait_for_terminal_generation(
            &socket,
            &handshake,
            &project,
            &identity,
            "refs/heads/main",
            Some(&revision),
            prior.as_deref(),
            &format!("sealed_retention_anchor_{index}"),
            Some("src/lib.rs"),
        )
        .await;
        if index == 0 {
            first_sealed = sealed_generation_names(environment.home(), &project);
        }
        prior = Some(receipt.generation_id);
    }
    assert!(
        !first_sealed.is_empty(),
        "the first publication never sealed a code-graph generation, so this \
         fixture cannot prove retirement; daemon_log={}",
        crate::code_index_journey::daemon_log_for_failure()
    );

    // Publication wakes maintenance, and its first admitted tick is at most one
    // retry delay away under the default policy. Observe only the sealed store:
    // a live successor may still be preparing its own work.
    let cadence = MaintenanceCadence::new(Duration::from_hours(
        RetentionConfig::default().interval_hours.max(1),
    ));
    let deadline = Instant::now() + cadence.retry_delay() + RECEIPT_TIMEOUT;
    while sealed_generation_names(environment.home(), &project).len() > 2
        && Instant::now() < deadline
    {
        std::thread::sleep(Duration::from_millis(50));
    }
    let retained = sealed_generation_names(environment.home(), &project);
    assert!(
        retained.len() <= 2,
        "scheduled maintenance did not retire superseded sealed generations: \
         {retained:?} remain under tracedecay.sealed; daemon_log={}",
        crate::code_index_journey::daemon_log_for_failure()
    );
    assert!(
        first_sealed.is_disjoint(&retained),
        "the first publication's sealed generation was never retired: \
         first={first_sealed:?} retained={retained:?}; daemon_log={}",
        crate::code_index_journey::daemon_log_for_failure()
    );
    stop_daemon_gracefully(&mut daemon);
}
