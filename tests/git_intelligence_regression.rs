use std::process::Command;

use tempfile::TempDir;
use tracedecay::git_intelligence::NativeGitIntelligence;
use tracedecay_domain::git::GitOperationStateV1;
use tracedecay_domain::research::{RepositoryId, WorktreeId};

#[test]
fn status_reports_sequencer_directory() {
    let repository = TempDir::new().unwrap();
    let status = Command::new("git")
        .args(["init", "-b", "main"])
        .current_dir(repository.path())
        .status()
        .unwrap();
    assert!(status.success());
    std::fs::create_dir(repository.path().join(".git/sequencer")).unwrap();
    let adapter = NativeGitIntelligence::new(
        repository.path(),
        RepositoryId::new("repository.fixture").unwrap(),
        WorktreeId::new("worktree.fixture").unwrap(),
    );

    let status = adapter.status().unwrap();

    assert_eq!(status.operation, GitOperationStateV1::Sequencer);
}
