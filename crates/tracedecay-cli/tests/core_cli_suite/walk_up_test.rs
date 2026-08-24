//! Walk-up project-root discovery.

use std::fs;
use tempfile::TempDir;

#[tokio::test]
async fn test_discover_project_root_from_subdir() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join(".tracedecay")).unwrap();
    fs::write(root.join(".tracedecay/tracedecay.db"), b"fake").unwrap();
    let subdir = root.join("src/mcp/tools");
    fs::create_dir_all(&subdir).unwrap();

    let found = tracedecay::config::discover_project_root(&subdir);
    assert_eq!(found.unwrap(), root);
}
