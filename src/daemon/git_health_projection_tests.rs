use std::fs;
use std::path::Path;
use std::time::Duration;

use tempfile::TempDir;
use tracedecay_application::{
    GitHealthProjectionAvailabilityV1, GitHealthProjectionReadServiceV1,
    GitHealthProjectionUnavailableReasonV1, ResolvedScope,
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
    let port = registry
        .mount(
            repository.path(),
            store_dir.path().join("project-graph.grafeo"),
            scope.clone(),
        )
        .await
        .expect("mount projection");
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

    commit(repository.path(), 2);
    let GitHealthProjectionAvailabilityV1::Refreshing {
        snapshot: drifted,
        target,
    } = reader.read()
    else {
        panic!("an immediate HEAD change must invalidate cached Ready");
    };
    assert_eq!(drifted, snapshot);
    assert_ne!(target.commit, snapshot.source.commit);

    git(repository.path(), &["switch", "--quiet", "-c", "other"]);
    let GitHealthProjectionAvailabilityV1::Stale {
        snapshot: stale_snapshot,
        reason: GitHealthProjectionUnavailableReasonV1::ScopeDrift,
    } = reader.read()
    else {
        panic!("ref drift must synchronously mark cached history stale");
    };
    assert_eq!(stale_snapshot, snapshot);

    let switched_scope = ResolvedScope::new(
        scope.project_id.clone(),
        scope.repository_id.clone(),
        scope.worktree_id.clone(),
        Some(RefId::new("refs/heads/other").expect("ref")),
    )
    .expect("switched scope");
    let switched_port = registry
        .mount(
            repository.path(),
            store_dir.path().join("project-graph.grafeo"),
            switched_scope.clone(),
        )
        .await
        .expect("replace branch owner");
    assert_eq!(registry.owner_count(), 1);
    let switched =
        GitHealthProjectionReadServiceV1::new(switched_scope, switched_port).expect("reader");
    assert_eq!(
        reader.read(),
        GitHealthProjectionAvailabilityV1::Unavailable {
            reason: GitHealthProjectionUnavailableReasonV1::NotMounted,
        }
    );
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if matches!(
                switched.read(),
                GitHealthProjectionAvailabilityV1::Ready { .. }
            ) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("normal branch switch should warm a replacement owner");

    drop(switched);
    tokio::task::yield_now().await;
    registry.shutdown().await;
}

#[tokio::test]
async fn retired_owner_releases_capacity_for_another_project() {
    let first = TempDir::new().expect("first repository");
    git(first.path(), &["init", "--quiet", "-b", "main"]);
    commit(first.path(), 0);
    let second = TempDir::new().expect("second repository");
    git(second.path(), &["init", "--quiet", "-b", "main"]);
    commit(second.path(), 0);
    let stores = TempDir::new().expect("project graphs");
    let registry = GitHealthProjectionRegistryV1::new(1);
    let first_port = registry
        .mount(
            first.path(),
            stores.path().join("first-project-graph.grafeo"),
            scope(first.path()),
        )
        .await
        .expect("first owner");
    assert!(
        registry
            .mount(
                second.path(),
                stores.path().join("second-project-graph.grafeo"),
                scope(second.path()),
            )
            .await
            .is_err(),
        "live owner must retain its capacity slot"
    );

    drop(first_port);
    assert_eq!(registry.owner_count(), 0);
    let second_port = registry
        .mount(
            second.path(),
            stores.path().join("second-project-graph.grafeo"),
            scope(second.path()),
        )
        .await
        .expect("retired owner must release its slot");
    assert_eq!(registry.owner_count(), 1);
    drop(second_port);
    registry.shutdown().await;
}

#[tokio::test]
async fn failed_store_open_does_not_consume_owner_capacity() {
    let repository = TempDir::new().expect("temporary repository");
    git(repository.path(), &["init", "--quiet", "-b", "main"]);
    commit(repository.path(), 0);
    let stores = TempDir::new().expect("project graphs");
    let blocked_parent = stores.path().join("not-a-directory");
    fs::write(&blocked_parent, "fixture").expect("blocked store parent");
    let registry = GitHealthProjectionRegistryV1::new(1);

    assert!(
        registry
            .mount(
                repository.path(),
                blocked_parent.join("project-graph.grafeo"),
                scope(repository.path()),
            )
            .await
            .is_err(),
        "an invalid project graph path must fail closed"
    );
    assert_eq!(registry.owner_count(), 0);

    let port = registry
        .mount(
            repository.path(),
            stores.path().join("project-graph.grafeo"),
            scope(repository.path()),
        )
        .await
        .expect("failed open must not consume the owner slot");
    assert_eq!(registry.owner_count(), 1);
    drop(port);
    registry.shutdown().await;
}
