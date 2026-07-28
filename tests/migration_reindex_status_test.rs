#![cfg(feature = "test-transport")]

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use tracedecay::application::host_admission::HostAdmissionTestRuntimeV1;
use tracedecay::tracedecay::{
    MigrationReindexAvailabilityV1, MigrationReindexStatusV1, TraceDecayOpenOptions,
};
use tracedecay_store::ProjectId;

fn git(project: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(project)
        .status()
        .expect("run git");
    assert!(status.success(), "git {args:?} failed");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn migrated_project_serves_old_generation_until_atomic_background_swap() {
    std::fs::create_dir_all("target").expect("repo-local target directory");
    let root = tempfile::Builder::new()
        .prefix("migration-reindex-")
        .tempdir_in("target")
        .expect("repo-local temporary fixture");
    let project = root.path().join("project");
    let profile = root.path().join("profile");
    std::fs::create_dir_all(project.join("src")).expect("project source directory");
    for index in 0..4_000 {
        std::fs::write(
            project.join("src").join(format!("filler_{index:04}.rs")),
            format!("pub fn filler_{index:04}() -> usize {{ {index} }}\n"),
        )
        .expect("filler source");
    }
    std::fs::write(project.join("src/lib.rs"), "pub fn before_migration() {}\n")
        .expect("initial source");
    git(&project, &["init", "-q", "-b", "main"]);
    git(&project, &["add", "."]);
    git(
        &project,
        &[
            "-c",
            "user.name=TraceDecay Test",
            "-c",
            "user.email=test@tracedecay.local",
            "commit",
            "-qm",
            "fixture",
        ],
    );

    let options = TraceDecayOpenOptions {
        profile_root: Some(profile.clone()),
        global_db_path: Some(profile.join("global.db")),
    };
    let runtime = HostAdmissionTestRuntimeV1::project(
        &profile,
        &project,
        ProjectId::new("project.migration-reindex").expect("project id"),
    )
    .await
    .expect("registered project runtime");
    let initialized = runtime
        .initialize_project_graph_for_test(&project, options.clone())
        .await
        .expect("initialize fixture");
    initialized.index_all().await.expect("seed old generation");
    std::fs::write(project.join("src/lib.rs"), "pub fn after_migration() {}\n")
        .expect("updated source");
    initialized
        .db()
        .execute_write_batch("downgrade migration fixture", "PRAGMA user_version = 24")
        .await
        .expect("downgrade fixture schema");
    initialized.close();

    let opened = tokio::time::timeout(
        Duration::from_secs(2),
        runtime.open_project_graph_for_test(&project, options.clone()),
    )
    .await
    .expect("project admission must not wait for full re-index")
    .expect("open migrated project");
    assert!(matches!(
        opened
            .migration_reindex_status()
            .await
            .expect("read pending migration status"),
        MigrationReindexStatusV1::Indexing { .. }
    ));
    assert_eq!(
        opened
            .get_nodes_by_name("before_migration")
            .await
            .expect("read old generation")
            .len(),
        1
    );
    assert!(
        opened
            .get_nodes_by_name("after_migration")
            .await
            .expect("new generation remains hidden")
            .is_empty()
    );

    tokio::time::timeout(Duration::from_secs(90), async {
        loop {
            if matches!(
                opened
                    .migration_reindex_status()
                    .await
                    .expect("read migration status"),
                MigrationReindexStatusV1::Current { .. }
            ) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("background migration re-index completes");

    assert!(
        opened
            .get_nodes_by_name("before_migration")
            .await
            .expect("old generation replaced")
            .is_empty()
    );
    assert_eq!(
        opened
            .get_nodes_by_name("after_migration")
            .await
            .expect("new generation published")
            .len(),
        1
    );
    opened.close();

    std::fs::write(
        project.join("src/lib.rs"),
        "pub fn after_simulated_restart() {}\n",
    )
    .expect("restart source");
    let interrupted = runtime
        .open_project_graph_for_test(&project, options.clone())
        .await
        .expect("open current generation before simulated restart");
    interrupted
        .db()
        .set_metadata(
            "migration_reindex_state_v1",
            &serde_json::to_string(&MigrationReindexStatusV1::Indexing {
                schema_version: 25,
                completed_files: 1_024,
                total_files: Some(4_001),
                availability: MigrationReindexAvailabilityV1::Stale,
            })
            .expect("encode interrupted state"),
        )
        .await
        .expect("persist interrupted state");
    interrupted.close();

    let resumed = tokio::time::timeout(
        Duration::from_secs(2),
        runtime.open_project_graph_for_test(&project, options),
    )
    .await
    .expect("restart admission must not wait for resumed work")
    .expect("reopen interrupted migration");
    assert!(matches!(
        resumed
            .migration_reindex_status()
            .await
            .expect("read resumed status"),
        MigrationReindexStatusV1::Indexing { .. }
    ));
    tokio::time::timeout(Duration::from_secs(90), async {
        loop {
            if matches!(
                resumed
                    .migration_reindex_status()
                    .await
                    .expect("read resumed migration status"),
                MigrationReindexStatusV1::Current { .. }
            ) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("resumed migration re-index completes");
    assert_eq!(
        resumed
            .get_nodes_by_name("after_simulated_restart")
            .await
            .expect("resumed generation published")
            .len(),
        1
    );
    resumed.close();
}
