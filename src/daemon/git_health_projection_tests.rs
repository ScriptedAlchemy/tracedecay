use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use tempfile::TempDir;
use tracedecay_application::{
    GitHealthProjectionAvailabilityV1, GitHealthProjectionBindingV1, GitHealthProjectionReadPortV1,
    GitHealthProjectionReadServiceV1, ResolvedScope,
};
use tracedecay_domain::{ProjectId, SourceStoreId, UserProfileId};

use super::{GitHealthProjectionMountErrorV1, GitHealthProjectionRegistryV1};
use crate::application::context::CancellationToken;

fn git(root: &Path, args: &[&str]) {
    let committed_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system time")
        .as_secs()
        .saturating_sub(24 * 60 * 60);
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(root)
        .env("GIT_AUTHOR_NAME", "TraceDecay Test")
        .env("GIT_AUTHOR_EMAIL", "test@tracedecay.invalid")
        .env("GIT_COMMITTER_NAME", "TraceDecay Test")
        .env("GIT_COMMITTER_EMAIL", "test@tracedecay.invalid")
        .env("GIT_AUTHOR_DATE", format!("@{committed_at} +0000"))
        .env("GIT_COMMITTER_DATE", format!("@{committed_at} +0000"))
        .output()
        .expect("git command should start");
    assert!(
        output.status.success(),
        "git {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn repository() -> TempDir {
    let repository = TempDir::new().expect("temporary repository");
    git(repository.path(), &["init", "--quiet", "-b", "main"]);
    fs::write(repository.path().join("history.rs"), "revision 0\n").expect("write fixture");
    git(repository.path(), &["add", "history.rs"]);
    git(repository.path(), &["commit", "--quiet", "-m", "fixture"]);
    repository
}

fn binding(root: &Path) -> GitHealthProjectionBindingV1 {
    let identity = crate::daemon::code_index_scheduler::identity::IndexingIdentityV1::resolve(root)
        .expect("resolve fixture identity");
    let scope = ResolvedScope::new(
        ProjectId::new("project.daemon-git-health").expect("project id"),
        identity.repository_id().clone(),
        identity.worktree_id().clone(),
        identity.head_ref().cloned(),
    )
    .expect("scope");
    GitHealthProjectionBindingV1::new(
        scope,
        UserProfileId::new("profile.daemon-git-health").expect("profile"),
        SourceStoreId::new("store.daemon-git-health").expect("store"),
    )
    .expect("binding")
}

#[tokio::test]
async fn cancelled_and_failed_opens_never_consume_owner_capacity() {
    let repository = repository();
    let store_dir = TempDir::new().expect("store root");
    let registry = GitHealthProjectionRegistryV1::new(1);
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    assert!(matches!(
        registry
            .mount(
                repository.path(),
                store_dir.path().join("cancelled.grafeo"),
                binding(repository.path()),
                &cancellation,
            )
            .await,
        Err(GitHealthProjectionMountErrorV1::Cancelled)
    ));
    assert_eq!(registry.owner_count(), 0);

    let blocked_parent = store_dir.path().join("not-a-directory");
    fs::write(&blocked_parent, "fixture").expect("blocking file");
    assert!(matches!(
        registry
            .mount(
                repository.path(),
                blocked_parent.join("store.grafeo"),
                binding(repository.path()),
                &CancellationToken::new(),
            )
            .await,
        Err(GitHealthProjectionMountErrorV1::Store(_))
    ));
    assert_eq!(registry.owner_count(), 0);

    let lease = registry
        .mount(
            repository.path(),
            store_dir.path().join("healthy.grafeo"),
            binding(repository.path()),
            &CancellationToken::new(),
        )
        .await
        .expect("capacity remains available");
    assert_eq!(registry.owner_count(), 1);
    drop(lease);
    registry.shutdown().await;
}

#[tokio::test]
async fn dropping_the_last_reader_deregisters_before_a_reopen() {
    let first = repository();
    let second = repository();
    let stores = TempDir::new().expect("store root");
    let registry = GitHealthProjectionRegistryV1::new(1);
    let first_binding = binding(first.path());
    let first_lease = registry
        .mount(
            first.path(),
            stores.path().join("first.grafeo"),
            first_binding.clone(),
            &CancellationToken::new(),
        )
        .await
        .expect("first mount");
    assert_eq!(registry.owner_count(), 1);
    drop(first_lease);
    assert_eq!(registry.owner_count(), 0);

    let second_lease = registry
        .mount(
            second.path(),
            stores.path().join("second.grafeo"),
            binding(second.path()),
            &CancellationToken::new(),
        )
        .await
        .expect("retirement is joined before reopen");
    assert_eq!(registry.owner_count(), 1);
    drop(second_lease);
    registry.shutdown().await;
}

#[tokio::test]
async fn project_server_candidate_can_trigger_idle_owner_eviction_at_capacity() {
    let first = repository();
    let second = repository();
    let stores = TempDir::new().expect("store root");
    let registry = GitHealthProjectionRegistryV1::new(1);
    let first_lease = registry
        .mount(
            first.path(),
            stores.path().join("first.grafeo"),
            binding(first.path()),
            &CancellationToken::new(),
        )
        .await
        .expect("first owner");

    let second_lease = registry
        .mount_candidate(
            second.path(),
            stores.path().join("second.grafeo"),
            binding(second.path()),
            &CancellationToken::new(),
        )
        .await
        .expect("candidate mounts before idle server eviction");
    assert_eq!(registry.owner_count(), 2);

    drop(first_lease);
    assert_eq!(
        registry.owner_count(),
        1,
        "evicting the idle server releases its projection lease"
    );
    drop(second_lease);
    registry.shutdown().await;
}

#[tokio::test]
async fn distinct_worktrees_share_one_project_graph_database() {
    let first = repository();
    let second = repository();
    let stores = TempDir::new().expect("store root");
    let store_path = stores.path().join("shared-project.grafeo");
    let registry = GitHealthProjectionRegistryV1::new(2);
    let first_lease = registry
        .mount(
            first.path(),
            store_path.clone(),
            binding(first.path()),
            &CancellationToken::new(),
        )
        .await
        .expect("first worktree");
    let second_lease = registry
        .mount(
            second.path(),
            store_path,
            binding(second.path()),
            &CancellationToken::new(),
        )
        .await
        .expect("second worktree shares the open graph database");
    assert_eq!(registry.owner_count(), 2);
    drop(first_lease);
    drop(second_lease);
    registry.shutdown().await;
}

#[tokio::test]
async fn ref_rekey_retires_the_old_reader_and_keeps_one_owner() {
    let repository = repository();
    let stores = TempDir::new().expect("store root");
    let store_path = stores.path().join("shared.grafeo");
    let registry = GitHealthProjectionRegistryV1::new(1);
    let old_binding = binding(repository.path());
    let old_lease = registry
        .mount(
            repository.path(),
            store_path.clone(),
            old_binding.clone(),
            &CancellationToken::new(),
        )
        .await
        .expect("old ref mount");
    let old_port: Arc<dyn GitHealthProjectionReadPortV1> = Arc::new(old_lease);
    let reader = GitHealthProjectionReadServiceV1::new(old_binding.clone(), old_port)
        .expect("binding-pinned reader");
    git(repository.path(), &["switch", "--quiet", "-c", "other"]);
    let new_binding = binding(repository.path());
    let new_lease = registry
        .mount(
            repository.path(),
            store_path,
            new_binding.clone(),
            &CancellationToken::new(),
        )
        .await
        .expect("rekeyed mount");
    let new_port: Arc<dyn GitHealthProjectionReadPortV1> = Arc::new(new_lease);
    reader
        .rebind(new_binding.clone(), new_port)
        .expect("publish rekeyed lease");
    assert_eq!(registry.owner_count(), 1);
    let snapshot = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let GitHealthProjectionAvailabilityV1::Ready { snapshot } = reader.read() {
                break snapshot;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("rekeyed projection should rebuild");
    assert_eq!(snapshot.source.binding, new_binding);
    assert_eq!(reader.binding().expect("reader binding"), new_binding);
    drop(reader);
    registry.shutdown().await;
}

#[tokio::test]
async fn daemon_projection_is_read_through_the_binding_pinned_service() {
    let repository = repository();
    let stores = TempDir::new().expect("store root");
    let binding = binding(repository.path());
    let registry = GitHealthProjectionRegistryV1::new(1);
    let lease = registry
        .mount(
            repository.path(),
            stores.path().join("projection.grafeo"),
            binding.clone(),
            &CancellationToken::new(),
        )
        .await
        .expect("mount");
    let port: Arc<dyn GitHealthProjectionReadPortV1> = Arc::new(lease);
    let reader =
        GitHealthProjectionReadServiceV1::new(binding.clone(), port).expect("application reader");
    let snapshot = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let GitHealthProjectionAvailabilityV1::Ready { snapshot } = reader.read() {
                break snapshot;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("projection should become ready");
    assert_eq!(snapshot.source.binding, binding);
    assert_eq!(snapshot.churn_entries, 1);
    let mut cursor = None;
    let entry = loop {
        let page = reader
            .read_churn_page(cursor.as_deref(), 1)
            .expect("bounded churn page");
        if let Some(entry) = page.entries.into_iter().next() {
            break entry;
        }
        cursor = page.next_cursor;
        assert!(
            cursor.is_some(),
            "authenticated churn entry must be reachable"
        );
    };
    assert_eq!(entry.path, "history.rs");
    assert_eq!(entry.churn, 1);
    assert!(
        serde_json::to_value(&snapshot)
            .expect("snapshot JSON")
            .get("file_churn")
            .is_none(),
        "the MCP-facing snapshot must not clone the complete churn map"
    );
    drop(reader);
    registry.shutdown().await;
}
