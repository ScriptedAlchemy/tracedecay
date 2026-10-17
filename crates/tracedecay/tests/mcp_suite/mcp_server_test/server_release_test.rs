use crate::mcp_server_test::support::*;
use serde_json::json;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tempfile::TempDir;
use tracedecay::mcp::McpServer;
use tracedecay_runtime_core::path_safety::canonical_root_identity;

fn open_files_under(roots: &[PathBuf]) -> Vec<PathBuf> {
    fs::read_dir("/proc/self/fd")
        .expect("open file descriptors")
        .flatten()
        .filter_map(|fd| fs::read_link(fd.path()).ok())
        .filter(|target| roots.iter().any(|root| target.starts_with(root)))
        .collect()
}

#[tokio::test]
async fn a_shut_down_direct_server_releases_its_stores() {
    let dir = TempDir::new().unwrap();
    let project = canonical_root_identity(dir.path());
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/main.rs"), "fn main() {}\n").unwrap();
    let cg = Box::pin(tracedecay_project::project::TraceDecay::init(&project))
        .await
        .unwrap();
    let profile_root = cg
        .test_runtime_for_test()
        .expect("direct test graph holds its registered runtime")
        .profile_root_for_test()
        .to_path_buf();
    let roots = [project, canonical_root_identity(&profile_root)];
    let server = Box::pin(McpServer::new(cg, None)).await;
    let released_server = Arc::downgrade(&server);
    assert!(
        !open_files_under(&roots).is_empty(),
        "the serving project holds its stores open"
    );

    run_server_with_messages(server, vec![spec_initialize_request(json!(1))]).await;

    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let open = open_files_under(&roots);
        if released_server.strong_count() == 0 && open.is_empty() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "a shut-down server must release itself and its stores: {} strong handles, open {open:?}",
            released_server.strong_count()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
