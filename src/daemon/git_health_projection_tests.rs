use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use tempfile::TempDir;
use tracedecay_application::{
    GitHealthProjectionAvailabilityV1, GitHealthProjectionReadPortV1,
    GitHealthProjectionReadServiceV1, GitHealthProjectionUnavailableReasonV1, ResolvedScope,
};
use tracedecay_domain::{ProjectId, RefId};

use super::GitHealthProjectionRegistryV1;

fn git(root: &Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(root)
        .env("GIT_AUTHOR_NAME", "TraceDecay Test")
        .env("GIT_AUTHOR_EMAIL", "test@tracedecay.invalid")
        .env("GIT_COMMITTER_NAME", "TraceDecay Test")
        .env("GIT_COMMITTER_EMAIL", "test@tracedecay.invalid")
        .output()
        .expect("git command should start");
    assert!(
        output.status.success(),
        "git {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn commit(root: &Path, ordinal: usize) {
    fs::write(root.join("history.rs"), format!("revision {ordinal}\n")).expect("write fixture");
    git(root, &["add", "history.rs"]);
    git(
        root,
        &["commit", "--quiet", "-m", &format!("commit {ordinal}")],
    );
}

fn scope(root: &Path) -> ResolvedScope {
    let identity = crate::daemon::code_index_scheduler::identity::IndexingIdentityV1::resolve(root)
        .expect("resolve fixture identity");
    ResolvedScope::new(
        ProjectId::new("project.daemon-git-health").expect("project id"),
        identity.repository_id().clone(),
        identity.worktree_id().clone(),
        identity.head_ref().cloned(),
    )
    .expect("resolved scope")
}

#[tokio::test]
async fn daemon_projection_is_read_through_the_scope_pinned_application_service() {
    let repository = TempDir::new().expect("temporary repository");
    git(repository.path(), &["init", "--quiet", "-b", "main"]);
    commit(repository.path(), 0);
    commit(repository.path(), 1);
    let scope = scope(repository.path());
    let store_dir = TempDir::new().expect("temporary projection store");
    let registry = GitHealthProjectionRegistryV1::new(1);
    assert!(registry.mount(
        repository.path(),
        store_dir.path().join("git-health.grafeo"),
        scope.clone(),
    ));
    let port: Arc<dyn GitHealthProjectionReadPortV1> = Arc::new(registry.clone());
    let reader =
        GitHealthProjectionReadServiceV1::new(scope.clone(), port).expect("application reader");

    let snapshot = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let GitHealthProjectionAvailabilityV1::Ready { snapshot } = reader.read() {
                break snapshot;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("background projection should become ready");
    assert_eq!(snapshot.source.scope, scope);
    assert_eq!(snapshot.file_churn.get("history.rs"), Some(&2));
    assert_eq!(snapshot.commits_projected, 2);

    git(repository.path(), &["switch", "--quiet", "-c", "other"]);
    let stale_snapshot = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let GitHealthProjectionAvailabilityV1::Stale {
                snapshot,
                reason: GitHealthProjectionUnavailableReasonV1::ScopeDrift,
            } = reader.read()
            {
                break snapshot;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("ref drift should mark the prior snapshot stale");
    assert_eq!(stale_snapshot, snapshot);

    let drifted_scope = ResolvedScope::new(
        scope.project_id.clone(),
        scope.repository_id.clone(),
        scope.worktree_id.clone(),
        Some(RefId::new("refs/heads/other").expect("ref")),
    )
    .expect("drifted scope");
    let drifted_port: Arc<dyn GitHealthProjectionReadPortV1> = Arc::new(registry.clone());
    let drifted =
        GitHealthProjectionReadServiceV1::new(drifted_scope, drifted_port).expect("reader");
    assert_eq!(
        drifted.read(),
        GitHealthProjectionAvailabilityV1::Unavailable {
            reason: GitHealthProjectionUnavailableReasonV1::ScopeDrift,
        }
    );

    registry.shutdown().await;
}
