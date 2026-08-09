use std::sync::Arc;

use tracedecay_code_index::git_projection::{
    GIT_TOPOLOGY_PROJECTOR_REVISION_V1, GitTopologyProjectionStore, GitTopologyProjectionV1,
    GitTopologyRefV1, build_git_topology_manifest_checked, git_topology_idempotency_key,
    git_topology_projection_identity, git_topology_ref_watermark,
};
use tracedecay_domain::{
    GitCommitIdentityV1, GitCommitMetadataV1, GitCoverageV1, GitHeadStateV1, GitHistoryV1,
    GitOidV1, ManifestDigest, RefId, RepositoryId, UtcMicros,
};
use tracedecay_graph_db::{
    GraphNamespace, GraphProjectorRevision, NeverCancelled, VerifiedGraphSnapshot,
};

fn oid(label: char) -> GitOidV1 {
    GitOidV1::new(label.to_string().repeat(40)).expect("oid")
}

fn digest(label: char) -> ManifestDigest {
    ManifestDigest::new(format!("sha256:{}", label.to_string().repeat(64))).expect("digest")
}

fn commit(label: char, parents: &[char]) -> GitCommitMetadataV1 {
    let identity = GitCommitIdentityV1 {
        name: "Fixture".to_owned(),
        email: "fixture@example.com".to_owned(),
        at: UtcMicros(10),
    };
    let tree_label = match label {
        'a' => 'e',
        'b' => 'f',
        'c' => '1',
        'd' => '2',
        _ => '3',
    };
    GitCommitMetadataV1 {
        commit: oid(label),
        tree: oid(tree_label),
        parents: parents.iter().copied().map(oid).collect(),
        author: identity.clone(),
        committer: identity,
        subject: format!("commit {label}"),
        message_digest: digest(label),
    }
}

fn projection(main_target: char) -> GitTopologyProjectionV1 {
    let repository = RepositoryId::new("repository.git-topology").expect("repository");
    let head = GitHeadStateV1::Attached {
        branch: "main".to_owned(),
        commit: oid(main_target),
    };
    let refs = vec![
        GitTopologyRefV1 {
            reference: RefId::new("refs/heads/main").expect("main ref"),
            target: Some(oid(main_target)),
        },
        GitTopologyRefV1 {
            reference: RefId::new("refs/heads/side").expect("side ref"),
            target: Some(oid('c')),
        },
    ];
    let ref_watermark =
        git_topology_ref_watermark(&repository, &head, &refs).expect("ref watermark");
    GitTopologyProjectionV1 {
        repository: repository.clone(),
        head,
        refs,
        history: GitHistoryV1 {
            repository,
            commits: vec![
                commit('d', &['b', 'c']),
                commit('c', &['a']),
                commit('b', &['a']),
                commit('a', &[]),
            ],
            truncated: false,
            coverage: GitCoverageV1::complete(),
        },
        ref_watermark,
        branch_stacks: Vec::new(),
        worktree_occupancies: Vec::new(),
    }
}

fn store(projection: &GitTopologyProjectionV1) -> GitTopologyProjectionStore {
    let identity =
        git_topology_projection_identity(GraphNamespace::new("git-topology-test").expect("ns"))
            .expect("projection");
    let revision = GraphProjectorRevision::try_from(GIT_TOPOLOGY_PROJECTOR_REVISION_V1.to_owned())
        .expect("revision");
    let manifest = build_git_topology_manifest_checked(identity, projection, &revision, &|| Ok(()))
        .expect("manifest");
    let snapshot = VerifiedGraphSnapshot::memory(manifest, Arc::new(NeverCancelled))
        .expect("verified snapshot");
    GitTopologyProjectionStore::from_verified_snapshot(snapshot, projection).expect("store")
}

#[test]
fn immutable_git_topology_traverses_merges_and_rewrites_refs_by_generation() {
    let original = projection('d');
    let store = store(&original);
    let cancellation = Arc::new(NeverCancelled);

    let mut ancestors = store
        .ancestors(&oid('d'), 8, 16, cancellation.clone())
        .expect("ancestors");
    ancestors.sort();
    assert_eq!(ancestors, vec![oid('a'), oid('b'), oid('c')]);

    let mut descendants = store
        .descendants(&oid('a'), 8, 16, cancellation.clone())
        .expect("descendants");
    descendants.sort();
    assert_eq!(descendants, vec![oid('b'), oid('c'), oid('d')]);
    assert_eq!(
        store
            .merge_base(&oid('b'), &oid('c'), 8, 16, cancellation.clone())
            .expect("merge base"),
        Some(oid('a'))
    );
    assert_eq!(
        store
            .merge_base(&oid('d'), &oid('c'), 8, 16, cancellation)
            .expect("ancestor merge base"),
        Some(oid('c'))
    );

    let rewritten = projection('b');
    let identity =
        git_topology_projection_identity(GraphNamespace::new("git-topology-test").expect("ns"))
            .expect("projection");
    let revision = GraphProjectorRevision::try_from(GIT_TOPOLOGY_PROJECTOR_REVISION_V1.to_owned())
        .expect("revision");
    let original_manifest =
        build_git_topology_manifest_checked(identity.clone(), &original, &revision, &|| Ok(()))
            .expect("original");
    let rewritten_manifest =
        build_git_topology_manifest_checked(identity, &rewritten, &revision, &|| Ok(()))
            .expect("rewritten");
    assert_ne!(original_manifest.generation, rewritten_manifest.generation);
    assert_ne!(original.ref_watermark, rewritten.ref_watermark);
    assert_eq!(
        git_topology_idempotency_key(&original, &revision).expect("original idempotency"),
        git_topology_idempotency_key(&original, &revision).expect("original replay idempotency")
    );
    assert_ne!(
        git_topology_idempotency_key(&original, &revision).expect("original idempotency"),
        git_topology_idempotency_key(&rewritten, &revision).expect("rewritten idempotency")
    );
    assert_ne!(
        original_manifest
            .expected_recovered_digest(&|| Ok(()))
            .expect("original digest"),
        rewritten_manifest
            .expected_recovered_digest(&|| Ok(()))
            .expect("rewritten digest")
    );
}

#[test]
fn truncated_history_keeps_missing_parent_as_truthful_boundary_node() {
    let mut input = projection('d');
    input.history.commits = vec![commit('d', &['b', 'c'])];
    input.history.truncated = true;
    let store = store(&input);

    let mut ancestors = store
        .ancestors(&oid('d'), 2, 4, Arc::new(NeverCancelled))
        .expect("boundary ancestors");
    ancestors.sort();
    assert_eq!(ancestors, vec![oid('b'), oid('c')]);
}

#[test]
fn verified_store_rejects_a_rewritten_ref_watermark_as_stale() {
    let original = projection('d');
    let store = store(&original);
    let rewritten = projection('b');

    assert_eq!(
        store.verify_ref_watermark(&rewritten.ref_watermark),
        Err(
            tracedecay_code_index::git_projection::GitTopologyProjectionError::Stale {
                projected: original.ref_watermark,
                current: rewritten.ref_watermark,
            }
        )
    );
}

#[test]
fn verified_store_rejects_a_stale_generation_binding() {
    let original = projection('d');
    let rewritten = projection('b');
    let identity =
        git_topology_projection_identity(GraphNamespace::new("git-topology-test").expect("ns"))
            .expect("projection");
    let revision = GraphProjectorRevision::try_from(GIT_TOPOLOGY_PROJECTOR_REVISION_V1.to_owned())
        .expect("revision");
    let manifest = build_git_topology_manifest_checked(identity, &original, &revision, &|| Ok(()))
        .expect("manifest");
    let snapshot = VerifiedGraphSnapshot::memory(manifest, Arc::new(NeverCancelled))
        .expect("verified snapshot");

    assert!(matches!(
        GitTopologyProjectionStore::from_verified_snapshot(snapshot, &rewritten),
        Err(tracedecay_code_index::git_projection::GitTopologyProjectionError::GenerationMismatch)
    ));
}
