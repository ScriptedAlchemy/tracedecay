use std::fs;
use std::path::Path;
use std::process::Command;

use tempfile::TempDir;
use tokensave::tokensave::TokenSave;

fn git(project: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(project)
        .output()
        .unwrap_or_else(|e| panic!("failed to run git {args:?}: {e}"));
    assert!(
        output.status.success(),
        "git {args:?} failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn commit_all(project: &Path, message: &str) {
    git(project, &["add", "."]);
    git(
        project,
        &[
            "-c",
            "user.name=TokenSave Test",
            "-c",
            "user.email=tokensave-test@example.com",
            "commit",
            "-m",
            message,
        ],
    );
}

#[tokio::test]
async fn sync_refuses_to_write_when_opened_on_fallback_branch() {
    let dir = TempDir::new().unwrap();
    let project = dir.path();

    git(project, &["init", "-b", "main"]);
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn indexed_on_main() {}\n").unwrap();
    commit_all(project, "initial commit");

    let main = TokenSave::init(project).await.unwrap();
    main.index_all().await.unwrap();
    drop(main);

    git(project, &["checkout", "-b", "feature/untracked"]);
    fs::write(
        project.join("src/untracked_only.rs"),
        "pub fn untracked_only() {}\n",
    )
    .unwrap();

    let fallback = TokenSave::open(project).await.unwrap();
    assert_eq!(fallback.active_branch(), Some("feature/untracked"));
    assert_eq!(fallback.serving_branch(), Some("main"));
    assert!(fallback.is_fallback());

    let err = fallback.sync().await.unwrap_err();
    let message = err.to_string();
    assert!(
        message.contains("fallback") && message.contains("tokensave branch add"),
        "unexpected error: {message}"
    );

    drop(fallback);
    git(project, &["checkout", "main"]);
    let main = TokenSave::open(project).await.unwrap();
    let results = main.search("untracked_only", 10).await.unwrap();
    assert!(
        results.is_empty(),
        "fallback sync must not index untracked branch files into main DB"
    );
}

#[tokio::test]
async fn full_index_refuses_to_write_when_opened_on_fallback_branch() {
    let dir = TempDir::new().unwrap();
    let project = dir.path();

    git(project, &["init", "-b", "main"]);
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn indexed_on_main() {}\n").unwrap();
    commit_all(project, "initial commit");

    let main = TokenSave::init(project).await.unwrap();
    main.index_all().await.unwrap();
    drop(main);

    git(project, &["checkout", "-b", "feature/untracked"]);
    fs::write(
        project.join("src/untracked_only.rs"),
        "pub fn untracked_only() {}\n",
    )
    .unwrap();

    let fallback = TokenSave::open(project).await.unwrap();
    assert_eq!(fallback.active_branch(), Some("feature/untracked"));
    assert_eq!(fallback.serving_branch(), Some("main"));
    assert!(fallback.is_fallback());

    let err = match fallback.index_all().await {
        Ok(_) => panic!("full index should refuse fallback writes"),
        Err(err) => err,
    };
    let message = err.to_string();
    assert!(
        message.contains("fallback") && message.contains("tokensave branch add"),
        "unexpected error: {message}"
    );

    drop(fallback);
    git(project, &["checkout", "main"]);
    let main = TokenSave::open(project).await.unwrap();
    let results = main.search("untracked_only", 10).await.unwrap();
    assert!(
        results.is_empty(),
        "fallback full index must not index untracked branch files into main DB"
    );
}
