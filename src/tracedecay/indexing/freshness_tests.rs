use crate::config::PinnedUserDataDir;
use crate::tracedecay::TraceDecay;
use std::path::Path;
use std::process::Command;
use tempfile::TempDir;

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new(crate::git::git_program())
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .status()
        .expect("git command runs");
    assert!(status.success(), "git {args:?} failed");
}

fn head_oid(dir: &Path) -> String {
    let out = Command::new(crate::git::git_program())
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", "HEAD"])
        .output()
        .expect("git rev-parse runs");
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

async fn init_repo_with_commit() -> (TraceDecay, TempDir, PinnedUserDataDir) {
    let pin = PinnedUserDataDir::new();
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    git(root, &["init", "-q", "-b", "main"]);
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/a.rs"), "pub fn a() {}\n").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-q", "-m", "initial"]);

    let cg = TraceDecay::init(root).await.expect("init");
    cg.index_all().await.expect("index");
    (cg, dir, pin)
}

#[tokio::test]
async fn last_synced_commit_stamped_after_index() {
    let (cg, dir, _pin) = init_repo_with_commit().await;
    let stamped = cg.last_synced_commit().await;
    assert_eq!(stamped.as_deref(), Some(head_oid(dir.path()).as_str()));
}

#[tokio::test]
async fn stale_files_since_commit_reports_changed_file() {
    let (cg, dir, _pin) = init_repo_with_commit().await;
    let root = dir.path();
    let base = head_oid(root);

    std::fs::write(root.join("src/b.rs"), "pub fn b() {}\n").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-q", "-m", "add b"]);

    let changed = cg
        .stale_files_since_commit(&base, 500)
        .expect("diff succeeds");
    assert!(
        changed.contains(&"src/b.rs".to_string()),
        "expected src/b.rs in {changed:?}"
    );
}

#[tokio::test]
async fn stale_files_since_commit_none_when_base_missing() {
    let (cg, _dir, _pin) = init_repo_with_commit().await;
    // A syntactically valid but unreachable commit id.
    let bogus = "0".repeat(40);
    assert!(cg.stale_files_since_commit(&bogus, 500).is_none());
}

#[tokio::test]
async fn stale_files_since_commit_none_when_over_escalation_limit() {
    let (cg, dir, _pin) = init_repo_with_commit().await;
    let root = dir.path();
    let base = head_oid(root);

    for i in 0..5 {
        std::fs::write(root.join(format!("src/f{i}.rs")), "pub fn f() {}\n").unwrap();
    }
    git(root, &["add", "."]);
    git(root, &["commit", "-q", "-m", "many"]);

    // escalation_limit below the number of changed files → None.
    assert!(cg.stale_files_since_commit(&base, 2).is_none());
}
