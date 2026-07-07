use std::{ffi::OsString, path::Path, time::Duration};
use tempfile::tempdir;
use tracedecay::mcp::McpServer;
use tracedecay::tracedecay::{TraceDecay, TraceDecayOpenOptions};

use crate::common::canonical_existing_path;

struct TranscriptHomeEnvGuard {
    previous_home: Option<OsString>,
    previous_userprofile: Option<OsString>,
    previous_data_dir: Option<OsString>,
    previous_hermes_home: Option<OsString>,
}

impl TranscriptHomeEnvGuard {
    fn set(home: &Path) -> Self {
        let previous_home = std::env::var_os("HOME");
        let previous_userprofile = std::env::var_os("USERPROFILE");
        let previous_data_dir = std::env::var_os(tracedecay::config::USER_DATA_DIR_ENV);
        let previous_hermes_home = std::env::var_os("HERMES_HOME");
        unsafe {
            std::env::set_var("HOME", home);
            std::env::set_var("USERPROFILE", home);
            std::env::set_var(
                tracedecay::config::USER_DATA_DIR_ENV,
                home.join(tracedecay::config::TRACEDECAY_DIR),
            );
            std::env::set_var("HERMES_HOME", home.join(".hermes"));
        }
        Self {
            previous_home,
            previous_userprofile,
            previous_data_dir,
            previous_hermes_home,
        }
    }
}

impl Drop for TranscriptHomeEnvGuard {
    fn drop(&mut self) {
        unsafe {
            match self.previous_home.take() {
                Some(value) => std::env::set_var("HOME", value),
                None => std::env::remove_var("HOME"),
            }
            match self.previous_userprofile.take() {
                Some(value) => std::env::set_var("USERPROFILE", value),
                None => std::env::remove_var("USERPROFILE"),
            }
            match self.previous_data_dir.take() {
                Some(value) => std::env::set_var(tracedecay::config::USER_DATA_DIR_ENV, value),
                None => std::env::remove_var(tracedecay::config::USER_DATA_DIR_ENV),
            }
            match self.previous_hermes_home.take() {
                Some(value) => std::env::set_var("HERMES_HOME", value),
                None => std::env::remove_var("HERMES_HOME"),
            }
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_mcps_on_same_project_coordinate_via_sync_lock() {
    let home = tempdir().unwrap();
    let tmp = tempdir().unwrap();
    let project = tmp.path().to_path_buf();
    let profile_root = canonical_existing_path(home.path()).join(".tracedecay");
    let _transcript_home_env = TranscriptHomeEnvGuard::set(home.path());
    let open_options = TraceDecayOpenOptions {
        profile_root: Some(profile_root.clone()),
        global_db_path: Some(profile_root.join("global.db")),
    };
    std::fs::write(project.join("a.rs"), "fn a() {}").unwrap();

    // Initial sync so both MCPs start with the same DB state.
    let cg_init =
        crate::fixture::init_project_from_template_with_options(&project, open_options.clone())
            .await
            .unwrap();
    cg_init.sync().await.unwrap();
    drop(cg_init);

    // Spin up two MCP servers on the same project.
    let cg1 = TraceDecay::open_with_options(&project, open_options.clone())
        .await
        .unwrap();
    let cg2 = TraceDecay::open_with_options(&project, open_options)
        .await
        .unwrap();
    let server1 = McpServer::new(cg1, None).await;
    let server2 = McpServer::new(cg2, None).await;

    // `McpServer::new` returns immediately and the #414 catch-up sync runs
    // on a detached tokio task. Wait for both servers' catch-up tasks to
    // finish before snapshotting so we don't race them against the
    // manual staleness pipeline below — otherwise the file_token_map
    // value we capture as `count_before` is non-deterministic.
    for (label, server) in [("server1", &server1), ("server2", &server2)] {
        assert!(
            server
                .wait_for_startup_catch_up(Duration::from_secs(30))
                .await,
            "{label}: startup catch-up should finish within 30s"
        );
    }

    let count_before = server1.file_token_map_snapshot().len();

    // Trigger a change. With the notify watcher removed (#80), each MCP
    // now picks changes up via the lazy staleness check. Drive that
    // pipeline directly on both servers so the test doesn't have to
    // wait through the 30 s cooldown gate in `maybe_sync_if_stale`. The
    // sync lock inside `sync_if_stale_silent` still serializes the two
    // peers — which is the property under test.
    std::fs::write(project.join("b.rs"), "fn b() {}").unwrap();

    // server1 syncs first and writes the new file into the shared DB.
    // server2 then walks and finds nothing stale (DB is already fresh),
    // but still refreshes its in-memory map from the DB — mirroring the
    // production path in `maybe_sync_if_stale`, which always refreshes
    // after passing the cooldown gate so quiet peers don't drift.
    for server in [&server1, &server2] {
        let cg = server.cg().await;
        let stale = cg.find_stale_files().await;
        if !stale.is_empty() {
            cg.sync_if_stale_silent(&stale).await.unwrap();
        }
        server.refresh_file_token_map().await;
    }

    let count_after_1 = server1.file_token_map_snapshot().len();
    let count_after_2 = server2.file_token_map_snapshot().len();

    assert!(count_after_1 > count_before, "server1 saw new file");
    assert!(count_after_2 > count_before, "server2 saw new file");

    // Both maps should converge to the same state — the sync lock ensures
    // exactly one MCP wrote the DB, and both refresh from the same DB.
    assert_eq!(count_after_1, count_after_2);

    server1.shutdown().await;
    server2.shutdown().await;
}
