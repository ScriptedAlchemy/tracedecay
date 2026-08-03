#![cfg(feature = "test-transport")]

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tracedecay::application::host_admission::HostAdmissionTestRuntimeV1;
use tracedecay::mcp::McpServer;
use tracedecay::tracedecay::{
    GraphRebuildAvailabilityV1, GraphRebuildStatusV1, TraceDecay, TraceDecayOpenOptions,
};
use tracedecay_store::ProjectId;

const FILE_COUNT: usize = 4_001;
const CHECKPOINT_BATCH_SIZE: usize = 1_024;

fn git(project: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(project)
        .status()
        .expect("run git");
    assert!(status.success(), "git {args:?} failed");
}

async fn wait_for_status(
    graph: &TraceDecay,
    predicate: impl Fn(&GraphRebuildStatusV1) -> bool,
) -> GraphRebuildStatusV1 {
    tokio::time::timeout(Duration::from_secs(90), async {
        loop {
            let status = graph
                .graph_rebuild_status()
                .await
                .expect("read graph rebuild status");
            if predicate(&status) {
                return status;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("graph rebuild reaches expected state")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn graph_rebuild_is_nonblocking_resumable_and_honestly_reported() {
    std::fs::create_dir_all("target").expect("repo-local target directory");
    let root = tempfile::Builder::new()
        .prefix("graph-rebuild-")
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
    std::fs::write(project.join("src/lib.rs"), "pub fn before_rebuild() {}\n")
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
    git(&project, &["branch", "feature"]);

    let options = TraceDecayOpenOptions {
        profile_root: Some(profile.clone()),
        global_db_path: Some(profile.join("global.db")),
    };
    let runtime = HostAdmissionTestRuntimeV1::project(
        &profile,
        &project,
        ProjectId::new("project.graph-rebuild").expect("project id"),
    )
    .await
    .expect("registered project runtime");
    TraceDecay::configure_graph_rebuild_for_test(0, 0, false);
    let initialized = runtime
        .initialize_project_graph_for_test(&project, options.clone())
        .await
        .expect("initialize fixture");
    let full_index = initialized.index_all().await.expect("seed old generation");
    assert_eq!(full_index.file_count, FILE_COUNT);
    git(&project, &["checkout", "-q", "feature"]);
    let feature_seed = runtime
        .open_project_graph_for_test(&project, options.clone())
        .await
        .expect("auto-track peer branch through retained runtime");
    assert_eq!(feature_seed.serving_branch(), Some("feature"));
    feature_seed.close();
    git(&project, &["checkout", "-q", "main"]);

    initialized.close();
    TraceDecay::configure_graph_rebuild_for_test(0, 0, false);
    let compatible = runtime
        .open_project_graph_for_test(&project, options.clone())
        .await
        .expect("reopen the freshly indexed graph");
    assert!(matches!(
        compatible
            .graph_rebuild_status()
            .await
            .expect("read current generation status"),
        GraphRebuildStatusV1::Current { .. }
    ));
    assert_eq!(
        TraceDecay::graph_rebuild_extractions_for_test(),
        0,
        "a current graph generation must not trigger extraction"
    );

    std::fs::write(project.join("src/lib.rs"), "pub fn after_rebuild() {}\n")
        .expect("updated source");
    compatible
        .db()
        .set_metadata("graph_generation_schema_version", "16")
        .await
        .expect("stamp old graph generation");
    compatible
        .db()
        .execute_write_batch(
            "prepare stale graph generation fixture",
            "DELETE FROM metadata WHERE key = 'graph_rebuild_state_v1';",
        )
        .await
        .expect("clear the rebuild marker");
    compatible.close();

    TraceDecay::configure_graph_rebuild_for_test(1, 750, false);
    let admission_started = Instant::now();
    let opened = runtime
        .open_project_graph_for_test(&project, options.clone())
        .await
        .expect("open the stale-generation project");
    let admission_elapsed = admission_started.elapsed();
    assert!(
        admission_elapsed.as_millis().saturating_mul(4) < u128::from(full_index.duration_ms),
        "admission took {admission_elapsed:?}, not a small fraction of the measured full index ({:?})",
        Duration::from_millis(full_index.duration_ms)
    );
    assert!(matches!(
        opened
            .graph_rebuild_status()
            .await
            .expect("read pending rebuild status"),
        GraphRebuildStatusV1::Indexing {
            availability: GraphRebuildAvailabilityV1::Stale,
            ..
        }
    ));

    tokio::time::timeout(Duration::from_secs(5), async {
        while !opened.store_layout().dirty_path.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("background rebuild publishes its live store marker");
    let peer = tokio::time::timeout(
        Duration::from_secs(2),
        runtime.open_project_branch_for_test(&project, "feature", options.clone()),
    )
    .await
    .expect("live rebuild marker must not block a peer branch")
    .expect("peer branch opens while rebuild owns the store marker");
    peer.close();

    assert_eq!(
        opened
            .get_nodes_by_name("before_rebuild")
            .await
            .expect("read old generation")
            .len(),
        1
    );
    assert!(
        opened
            .get_nodes_by_name("after_rebuild")
            .await
            .expect("new generation remains hidden")
            .is_empty()
    );

    let failed = wait_for_status(&opened, |status| {
        matches!(status, GraphRebuildStatusV1::Failed { .. })
    })
    .await;
    assert!(matches!(
        failed,
        GraphRebuildStatusV1::Failed {
            retryable: true,
            ..
        }
    ));
    assert_eq!(
        TraceDecay::graph_rebuild_extractions_for_test(),
        CHECKPOINT_BATCH_SIZE
    );
    let mut active_sync_lock_name = opened
        .db_path()
        .file_name()
        .expect("active graph database filename")
        .to_os_string();
    active_sync_lock_name.push(".sync.lock");
    let checkpoint_root = opened
        .db_path()
        .with_file_name(active_sync_lock_name)
        .with_extension("graph-rebuild-checkpoint-v1");
    assert!(
        std::fs::read_dir(&checkpoint_root)
            .expect("checkpoint directory")
            .filter_map(std::result::Result::ok)
            .any(|entry| entry.file_name().to_string_lossy().starts_with("batch-")),
        "the interrupted worker must leave a durable checkpoint batch"
    );

    TraceDecay::configure_graph_rebuild_for_test(0, 0, true);
    opened
        .db()
        .execute_write_batch(
            "simulate a lost rebuild marker",
            "DELETE FROM metadata WHERE key = 'graph_rebuild_state_v1'",
        )
        .await
        .expect("delete the rebuild state");
    opened.close();
    let markerless = runtime
        .open_project_graph_for_test(&project, options.clone())
        .await
        .expect("reopen markerless migrated generation");
    assert!(matches!(
        markerless
            .graph_rebuild_status()
            .await
            .expect("read markerless status"),
        GraphRebuildStatusV1::Indexing {
            completed_files: 0,
            availability: GraphRebuildAvailabilityV1::Stale,
            ..
        }
    ));
    markerless.close();

    TraceDecay::configure_graph_rebuild_for_test(0, 0, false);
    let resumed = runtime
        .open_project_graph_for_test(&project, options.clone())
        .await
        .expect("resume the interrupted rebuild");
    wait_for_status(&resumed, |status| {
        matches!(status, GraphRebuildStatusV1::Current { .. })
    })
    .await;
    assert_eq!(
        TraceDecay::graph_rebuild_extractions_for_test(),
        FILE_COUNT - CHECKPOINT_BATCH_SIZE,
        "resume must extract only files absent from the durable checkpoint"
    );
    assert!(
        resumed
            .get_nodes_by_name("before_rebuild")
            .await
            .expect("old generation replaced")
            .is_empty()
    );
    assert_eq!(
        resumed
            .get_nodes_by_name("after_rebuild")
            .await
            .expect("new generation published")
            .len(),
        1
    );

    TraceDecay::configure_graph_rebuild_for_test(0, 0, true);
    resumed
        .db()
        .execute_write_batch(
            "prepare an unavailable graph generation",
            "DELETE FROM edges;
             DELETE FROM unresolved_refs;
             DELETE FROM nodes;
             DELETE FROM files;
             DELETE FROM metadata WHERE key = 'graph_rebuild_state_v1';
             UPDATE metadata SET value = '16'
             WHERE key = 'graph_generation_schema_version';",
        )
        .await
        .expect("clear old graph generation");
    resumed.close();
    let unavailable = runtime
        .open_project_graph_for_test(&project, options)
        .await
        .expect("open the unavailable graph generation");
    assert!(matches!(
        unavailable
            .graph_rebuild_status()
            .await
            .expect("read unavailable status"),
        GraphRebuildStatusV1::Indexing {
            availability: GraphRebuildAvailabilityV1::Unavailable,
            ..
        }
    ));

    let server = McpServer::new(unavailable, None).await;
    let status = server
        .call_tool_for_test(
            "tracedecay_status",
            json!({
                "format": "json",
                "include_branch_diagnostics": false,
                "include_storage_health": false,
                "include_session_ingest": false,
                "include_staleness": false,
            }),
        )
        .await
        .expect("query typed unavailable status");
    let payload: Value = serde_json::from_str(
        status.value["content"][0]["text"]
            .as_str()
            .expect("status JSON text"),
    )
    .expect("parse status JSON");
    assert_eq!(payload["graph_rebuild"]["state"], "indexing");
    assert_eq!(payload["graph_rebuild"]["availability"], "unavailable");
    assert!(
        payload["graph_rebuild_warning"]
            .as_str()
            .is_some_and(|warning| warning.contains("counts are not authoritative"))
    );

    TraceDecay::configure_graph_rebuild_for_test(0, 0, false);
}
