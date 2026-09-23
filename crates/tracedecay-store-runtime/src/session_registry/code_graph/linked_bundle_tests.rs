//! Linked worktrees of one project seal their read bundles into the project's
//! shared artifact root: identical trees store one catalog artifact, a
//! divergent tree never serves its catalog to a sibling, and retiring one
//! worktree keeps the artifact the other still names.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use sha2::{Digest, Sha256};
use tracedecay_code_index_retention::code_index_generations::{
    CodeGenerationRetentionModeV1, DurablePublicationPointerV1, code_generation_segments_root,
    run_code_generation_retention,
};
use tracedecay_code_index_runtime::CodeGraphReplayBindingV1;
use tracedecay_code_index_runtime::code_index_scheduler::{
    CodeIndexWorktreeSchedulerV1, SharedCodeIndexBytePoolV1, scoped_code_index_store_root,
};
use tracedecay_daemon_identity::profile_identity;
use tracedecay_domain::{ProjectId, UtcMicros, sha256_hex_suffix};
use tracedecay_graph_db::{
    SealedGraphStateDigest, SealedReadBundleArtifactStateV1, retire_sealed_read_bundle,
    sealed_read_bundle_artifact_file_digest, sealed_read_bundle_manifest_artifact_digests,
};

use super::super::DaemonSessionRuntimeRegistryV1;

const SHARED_SOURCE: &str = "pub fn shared_alpha() -> usize { 1 }\n\
     pub fn shared_beta() -> usize { shared_alpha() + 1 }\n";

fn git(root: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .expect("run git fixture command");
    assert!(
        output.status.success(),
        "git fixture command failed: {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

struct LinkedBundleScopeV1 {
    scope: PathBuf,
    sealed: SealedGraphStateDigest,
    /// The artifact digests this scope's own bundle manifest names.
    artifacts: Vec<String>,
    /// The catalog bytes this scope's runtime loads through its manifest.
    catalog: Vec<u8>,
}

struct LinkedBundlesV1 {
    _temporary: tempfile::TempDir,
    first: LinkedBundleScopeV1,
    linked: LinkedBundleScopeV1,
}

/// Seals and publishes the primary checkout and one linked worktree of the
/// same project, each into its own scope of one `code-index-v1/`. The linked
/// worktree optionally commits a file the primary checkout does not have.
async fn publish_linked_worktree_bundles(
    label: &str,
    linked_only_source: Option<&str>,
) -> LinkedBundlesV1 {
    let temporary = tempfile::tempdir().expect("temporary fixture parent");
    let root = temporary
        .path()
        .canonicalize()
        .expect("canonical fixture root");
    let profile_root = root.join("profile");
    let project_root = root.join("project");
    std::fs::create_dir_all(project_root.join("src")).expect("project source directory");
    git(&project_root, &["init", "-q", "-b", "main"]);
    git(&project_root, &["config", "user.name", "TraceDecay Test"]);
    git(
        &project_root,
        &["config", "user.email", "tracedecay@example.invalid"],
    );
    std::fs::write(project_root.join("src/lib.rs"), SHARED_SOURCE).expect("project source");
    git(&project_root, &["add", "."]);
    git(&project_root, &["commit", "-qm", "linked bundle fixture"]);
    let project_id = ProjectId::new(format!("project.linked-bundle-{label}")).expect("project id");
    tracedecay_runtime_core::storage::pin_fixture_repository_identity(
        &project_root,
        project_id.as_str(),
    )
    .expect("project enrollment");
    let linked_root = root.join("linked");
    git(
        &project_root,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "linked",
            linked_root.to_str().expect("UTF-8 linked root"),
            "main",
        ],
    );
    if let Some(source) = linked_only_source {
        std::fs::write(linked_root.join("src/only_linked.rs"), source).expect("linked source");
        git(&linked_root, &["add", "src/only_linked.rs"]);
        git(&linked_root, &["commit", "-qm", "linked-only file"]);
    }
    let project_root = project_root.canonicalize().expect("canonical project root");
    let linked_root = linked_root.canonicalize().expect("canonical linked root");
    tracedecay_runtime_core::storage::pin_fixture_repository_identity(
        &linked_root,
        project_id.as_str(),
    )
    .expect("linked worktree enrollment");

    let code_index_root = root.join("code-index-store");
    let identity = profile_identity::load_or_create(&profile_root).expect("profile identity");
    let _database_scope = tracedecay_runtime_core::db::enter_daemon_database_scope(
        &profile_root,
        44,
        "linked worktree read bundles",
    )
    .expect("daemon database scope");
    let registry = DaemonSessionRuntimeRegistryV1::open(identity)
        .await
        .expect("session runtime registry");
    let project_database = registry
        .project_memory(
            project_id.clone(),
            [project_root.clone(), linked_root.clone()],
        )
        .await
        .expect("project graph database");

    let mut scopes = Vec::with_capacity(2);
    for worktree_root in [&project_root, &linked_root] {
        let scope = scoped_code_index_store_root(&code_index_root, worktree_root);
        let mut scheduler = CodeIndexWorktreeSchedulerV1::open(
            project_id.clone(),
            worktree_root,
            scope.clone(),
            Arc::new(SharedCodeIndexBytePoolV1::default()),
        )
        .expect("open worktree scheduler");
        scheduler.reconcile_now().expect("seal the generation");
        let latest = scheduler.latest_complete().expect("complete generation");
        let worktree_id = scheduler.identity().worktree_id().clone();
        drop(scheduler);
        let snapshot = latest.generation().snapshot();
        let pointer: DurablePublicationPointerV1 = serde_json::from_slice(
            &std::fs::read(scope.join("active-code-generation-v1.json"))
                .expect("active generation pointer"),
        )
        .expect("decode active generation pointer");
        let generations_root = scope.join("code-generations-v1");
        let sealed = SealedGraphStateDigest::try_from(pointer.state_digest.clone())
            .expect("sealed state digest");
        let runtime = registry
            .retain_code_graph_runtime(
                project_id.clone(),
                snapshot.repository.clone(),
                worktree_id,
                snapshot.reference.clone(),
                latest.generation().manifest().generation_id.clone(),
                Arc::clone(&project_database),
                CodeGraphReplayBindingV1 {
                    generations_root: generations_root.clone(),
                    sealed_state_digest: sealed.clone(),
                },
                None,
            )
            .await
            .expect("retain code graph runtime");
        runtime
            .publish_verified_snapshot(latest.generation(), Arc::new(AtomicBool::new(false)))
            .expect("seal the code graph");
        let manifest = generations_root.join(format!(
            "read-bundle-{}.json",
            sha256_hex_suffix(&pointer.state_digest).expect("sha256 state digest")
        ));
        let artifacts = sealed_read_bundle_manifest_artifact_digests(&manifest)
            .expect("read bundle manifest")
            .expect("the scope sealed a read bundle");
        let loaded = runtime
            .load_sealed_read_bundle_catalog(&Arc::new(AtomicBool::new(false)))
            .expect("load the bundle catalog");
        let SealedReadBundleArtifactStateV1::Loaded { bytes, .. } = loaded else {
            panic!("a freshly sealed bundle must load, got {loaded:?}");
        };
        scopes.push(LinkedBundleScopeV1 {
            scope,
            sealed,
            artifacts,
            catalog: bytes,
        });
    }
    let linked = scopes.pop().expect("linked scope");
    let first = scopes.pop().expect("primary scope");
    LinkedBundlesV1 {
        _temporary: temporary,
        first,
        linked,
    }
}

/// Every catalog artifact in the shared root of `scope`'s project, by digest.
fn shared_artifacts(scope: &Path) -> BTreeSet<String> {
    let Ok(entries) = std::fs::read_dir(code_generation_segments_root(scope)) else {
        return BTreeSet::new();
    };
    entries
        .map(|entry| entry.expect("shared artifact entry"))
        .filter_map(|entry| sealed_read_bundle_artifact_file_digest(entry.file_name().to_str()?))
        .collect()
}

fn contains(bytes: &[u8], needle: &[u8]) -> bool {
    bytes.windows(needle.len()).any(|window| window == needle)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn linked_worktrees_with_identical_trees_share_one_read_bundle_artifact() {
    let bundles = publish_linked_worktree_bundles("identical", None).await;
    assert_ne!(
        bundles.first.sealed, bundles.linked.sealed,
        "each worktree seals its own generation"
    );
    assert_eq!(bundles.first.artifacts, bundles.linked.artifacts);
    assert_eq!(bundles.first.catalog, bundles.linked.catalog);
    assert_eq!(
        shared_artifacts(&bundles.first.scope),
        bundles.first.artifacts.iter().cloned().collect(),
        "the project stores the catalog both worktrees name exactly once"
    );
    for scope in [&bundles.first.scope, &bundles.linked.scope] {
        let local = std::fs::read_dir(scope.join("code-generations-v1"))
            .expect("scope generations")
            .map(|entry| entry.expect("generation entry").file_name())
            .filter(|name| name.to_string_lossy().ends_with(".bin"))
            .collect::<Vec<_>>();
        assert!(
            local.is_empty(),
            "a worktree scope keeps no artifact bytes: {local:?}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_linked_worktree_with_a_divergent_file_never_serves_its_catalog_to_the_other() {
    let marker = "only_in_linked_worktree";
    let bundles = publish_linked_worktree_bundles(
        "divergent",
        Some(&format!("pub fn {marker}() -> usize {{ 7 }}\n")),
    )
    .await;
    assert_ne!(bundles.first.artifacts, bundles.linked.artifacts);
    assert!(
        contains(&bundles.linked.catalog, marker.as_bytes()),
        "the linked worktree serves its own divergent symbol"
    );
    assert!(
        !contains(&bundles.first.catalog, marker.as_bytes()),
        "the primary worktree must never be answered with the linked worktree's catalog"
    );
    let mut both = bundles
        .first
        .artifacts
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    both.extend(bundles.linked.artifacts.iter().cloned());
    assert_eq!(shared_artifacts(&bundles.first.scope), both);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retiring_one_linked_worktree_keeps_the_read_bundle_artifact_its_sibling_names() {
    let bundles = publish_linked_worktree_bundles("retire", None).await;
    let shared = bundles
        .first
        .artifacts
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    let sweep = |scope: &Path| {
        run_code_generation_retention(
            scope,
            &BTreeSet::new(),
            CodeGenerationRetentionModeV1::Apply,
            UtcMicros(1),
            None,
        )
        .expect("project segment sweep")
    };

    // The linked worktree goes away: its bundle retires with its generation,
    // then its whole scope is removed.
    retire_sealed_read_bundle(
        &bundles.linked.scope.join("code-generations-v1"),
        &bundles.linked.sealed,
    )
    .expect("retire the linked bundle");
    std::fs::remove_dir_all(&bundles.linked.scope).expect("remove the linked scope");
    sweep(&bundles.first.scope);
    assert_eq!(
        shared_artifacts(&bundles.first.scope),
        shared,
        "the primary worktree still names the shared catalog"
    );
    let digest = shared.first().expect("one shared artifact");
    let path = code_generation_segments_root(&bundles.first.scope).join(format!(
        "read-bundle-artifact-{}.bin",
        sha256_hex_suffix(digest).expect("sha256 artifact digest")
    ));
    let bytes = std::fs::read(&path).expect("shared catalog artifact");
    assert_eq!(bytes, bundles.first.catalog);
    assert_eq!(
        format!("sha256:{}", hex::encode(Sha256::digest(&bytes))),
        *digest
    );

    // Once no manifest names it, the sweep collects it.
    retire_sealed_read_bundle(
        &bundles.first.scope.join("code-generations-v1"),
        &bundles.first.sealed,
    )
    .expect("retire the primary bundle");
    sweep(&bundles.first.scope);
    assert!(shared_artifacts(&bundles.first.scope).is_empty());
}
