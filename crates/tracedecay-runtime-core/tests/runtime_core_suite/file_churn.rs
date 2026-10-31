use std::fs;

use tempfile::TempDir;
use tracedecay_runtime_core::git::churn::file_churn;

#[tokio::test]
async fn test_file_churn() {
    let dir = TempDir::new().expect("failed to create temp dir");
    let project = dir.path();

    for args in [
        &["init"][..],
        &["config", "user.email", "test@test.com"][..],
        &["config", "user.name", "Test"][..],
    ] {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(project)
            .output()
            .expect("git setup command failed");
        assert!(output.status.success(), "git {args:?} failed");
    }

    fs::write(project.join("file.rs"), "fn foo() {}").expect("write failed");
    for args in [&["add", "."][..], &["commit", "-m", "first"][..]] {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(project)
            .output()
            .expect("first commit command failed");
        assert!(output.status.success(), "git {args:?} failed");
    }

    fs::write(project.join("file.rs"), "fn foo() {} fn bar() {}").expect("write failed");
    for args in [&["add", "."][..], &["commit", "-m", "second"][..]] {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(project)
            .output()
            .expect("second commit command failed");
        assert!(output.status.success(), "git {args:?} failed");
    }

    let churn = file_churn(project, 90).await.expect("file_churn failed");
    let count = churn.get("file.rs").copied().unwrap_or(0);
    assert!(count >= 2, "file.rs should have churn >= 2, got {count}");
}

#[tokio::test]
async fn test_file_churn_nonexistent_dir() {
    let missing = std::env::temp_dir().join(format!(
        "tracedecay-missing-churn-root-{}",
        std::process::id()
    ));
    let churn = file_churn(&missing, 90)
        .await
        .expect("file_churn should not error for nonexistent dir");
    assert!(churn.is_empty());
}
