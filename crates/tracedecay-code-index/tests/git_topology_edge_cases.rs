//! Git topology projection edge cases: octopus merges, detached HEAD, deleted
//! branches, linked-worktree identity, real cancellation, and deterministic
//! traversal order. Every case reads through the verified snapshot store so a
//! silent partial answer cannot pass as a truthful one.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use tracedecay_code_index::git_projection::{
    GIT_TOPOLOGY_PROJECTOR_REVISION_V1, GitTopologyProjectionError, GitTopologyProjectionStore,
    GitTopologyProjectionV1, GitTopologyRefV1, build_git_topology_manifest_checked,
    git_topology_generation_id, git_topology_idempotency_key, git_topology_namespace,
    git_topology_projection_identity, git_topology_ref_watermark,
};
use tracedecay_domain::{
    GitCommitIdentityV1, GitCommitMetadataV1, GitCoverageV1, GitHeadStateV1, GitHistoryV1,
    GitOidV1, ManifestDigest, RefId, RepositoryId, UtcMicros,
};
use tracedecay_graph_db::{
    GraphCancellation, GraphGenerationManifest, GraphProjectionIdentity, GraphProjectorRevision,
    NeverCancelled, VerifiedGraphSnapshot,
};

/// Cancellation that is already cancelled before the first traversal step.
#[derive(Debug)]
struct AlwaysCancelled;

impl GraphCancellation for AlwaysCancelled {
    fn is_cancelled(&self) -> bool {
        true
    }
}

/// Cancellation that fires after a bounded number of observations, so a
/// traversal is cancelled while it is walking rather than before it starts.
#[derive(Debug)]
struct CancelAfter {
    remaining: AtomicUsize,
}

impl CancelAfter {
    fn new(observations: usize) -> Self {
        Self {
            remaining: AtomicUsize::new(observations),
        }
    }
}

impl GraphCancellation for CancelAfter {
    fn is_cancelled(&self) -> bool {
        self.remaining
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                remaining.checked_sub(1)
            })
            .is_err()
    }
}

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
    GitCommitMetadataV1 {
        commit: oid(label),
        tree: oid('9'),
        parents: parents.iter().copied().map(oid).collect(),
        author: identity.clone(),
        committer: identity,
        subject: format!("commit {label}"),
        message_digest: digest(label),
    }
}

fn reference(name: &str, target: char) -> GitTopologyRefV1 {
    GitTopologyRefV1 {
        reference: RefId::new(name).expect("ref"),
        target: Some(oid(target)),
    }
}

fn repository(id: &str) -> RepositoryId {
    RepositoryId::new(id).expect("repository")
}

fn projection(
    repository: RepositoryId,
    head: GitHeadStateV1,
    refs: Vec<GitTopologyRefV1>,
    commits: Vec<GitCommitMetadataV1>,
) -> GitTopologyProjectionV1 {
    let ref_watermark =
        git_topology_ref_watermark(&repository, &head, &refs).expect("ref watermark");
    GitTopologyProjectionV1 {
        repository: repository.clone(),
        head,
        refs,
        history: GitHistoryV1 {
            repository,
            commits,
            truncated: false,
            coverage: GitCoverageV1::complete(),
        },
        ref_watermark,
    }
}

fn revision() -> GraphProjectorRevision {
    GraphProjectorRevision::try_from(GIT_TOPOLOGY_PROJECTOR_REVISION_V1.to_owned())
        .expect("revision")
}

/// Production keys the topology projection by repository identity, never by a
/// checkout path (see `git_topology_namespace`).
fn identity(repository: &RepositoryId) -> GraphProjectionIdentity {
    git_topology_projection_identity(git_topology_namespace(repository).expect("namespace"))
        .expect("projection identity")
}

fn manifest(projection: &GitTopologyProjectionV1) -> GraphGenerationManifest {
    build_git_topology_manifest_checked(
        identity(&projection.repository),
        projection,
        &revision(),
        &|| Ok(()),
    )
    .expect("manifest")
}

fn store(projection: &GitTopologyProjectionV1) -> GitTopologyProjectionStore {
    let snapshot = VerifiedGraphSnapshot::memory(manifest(projection), Arc::new(NeverCancelled))
        .expect("verified snapshot");
    GitTopologyProjectionStore::from_verified_snapshot(snapshot, projection).expect("store")
}

fn set(oids: Vec<GitOidV1>) -> BTreeSet<GitOidV1> {
    oids.into_iter().collect()
}

/// `0 <- a, 0 <- b, 0 <- c`, then an octopus merge `f` over all three.
fn octopus_projection() -> GitTopologyProjectionV1 {
    projection(
        repository("repository.git-topology-edge"),
        GitHeadStateV1::Attached {
            branch: "main".to_owned(),
            commit: oid('f'),
        },
        vec![reference("refs/heads/main", 'f')],
        vec![
            commit('f', &['a', 'b', 'c']),
            commit('c', &['0']),
            commit('b', &['0']),
            commit('a', &['0']),
            commit('0', &[]),
        ],
    )
}

/// Linear history `0 <- a <- b <- c` where every depth holds exactly one
/// commit, so traversal order is fully determined by the contract.
fn chain_commits() -> Vec<GitCommitMetadataV1> {
    vec![
        commit('c', &['b']),
        commit('b', &['a']),
        commit('a', &['0']),
        commit('0', &[]),
    ]
}

fn chain_projection(head: GitHeadStateV1) -> GitTopologyProjectionV1 {
    projection(
        repository("repository.git-topology-edge"),
        head,
        vec![reference("refs/heads/main", 'c')],
        chain_commits(),
    )
}

#[test]
fn octopus_merge_exposes_every_parent_and_a_truthful_merge_base() {
    let input = octopus_projection();
    let store = store(&input);
    let cancellation = Arc::new(NeverCancelled);

    let parents = store
        .ancestors(&oid('f'), 1, 16, cancellation.clone())
        .expect("octopus parents");
    assert_eq!(parents.len(), 3, "octopus merge must expose three parents");
    assert_eq!(set(parents), set(vec![oid('a'), oid('b'), oid('c')]));

    assert_eq!(
        set(store
            .ancestors(&oid('f'), 8, 16, cancellation.clone())
            .expect("octopus ancestors")),
        set(vec![oid('a'), oid('b'), oid('c'), oid('0')])
    );
    assert_eq!(
        set(store
            .descendants(&oid('0'), 8, 16, cancellation.clone())
            .expect("octopus descendants")),
        set(vec![oid('a'), oid('b'), oid('c'), oid('f')])
    );

    // Sibling parents of the octopus merge meet at the shared root.
    assert_eq!(
        store
            .merge_base(&oid('a'), &oid('b'), 8, 16, cancellation.clone())
            .expect("sibling merge base"),
        Some(oid('0'))
    );
    assert_eq!(
        store
            .merge_base(&oid('b'), &oid('c'), 8, 16, cancellation.clone())
            .expect("sibling merge base"),
        Some(oid('0'))
    );
    // Every parent is itself the merge base against the octopus merge.
    for parent in ['a', 'b', 'c'] {
        assert_eq!(
            store
                .merge_base(&oid('f'), &oid(parent), 8, 16, cancellation.clone())
                .expect("octopus parent merge base"),
            Some(oid(parent)),
            "octopus merge base against parent {parent}"
        );
    }
    assert_eq!(
        store
            .merge_base(&oid('f'), &oid('0'), 8, 16, cancellation)
            .expect("octopus root merge base"),
        Some(oid('0'))
    );
}

#[test]
fn detached_head_projection_traverses_and_verifies_its_watermark() {
    let detached = chain_projection(GitHeadStateV1::Detached { commit: oid('b') });
    assert_eq!(detached.head.branch(), None, "detached HEAD has no branch");
    let store = store(&detached);
    let cancellation = Arc::new(NeverCancelled);

    store
        .verify_ref_watermark(&detached.ref_watermark)
        .expect("detached watermark verifies against its own generation");
    assert_eq!(
        store.ref_watermark(),
        &detached.ref_watermark,
        "store must publish the detached ref watermark it was built from"
    );

    assert_eq!(
        set(store
            .ancestors(&oid('b'), 8, 16, cancellation.clone())
            .expect("detached ancestors")),
        set(vec![oid('a'), oid('0')])
    );
    assert_eq!(
        set(store
            .descendants(&oid('b'), 8, 16, cancellation.clone())
            .expect("detached descendants")),
        set(vec![oid('c')])
    );
    assert_eq!(
        store
            .merge_base(&oid('b'), &oid('c'), 8, 16, cancellation)
            .expect("detached merge base"),
        Some(oid('b'))
    );

    // The same refs under an attached HEAD are a different observation, so the
    // watermark and the generation must both diverge and be provably stale.
    let attached = chain_projection(GitHeadStateV1::Attached {
        branch: "main".to_owned(),
        commit: oid('c'),
    });
    assert_ne!(detached.ref_watermark, attached.ref_watermark);
    assert_ne!(
        git_topology_generation_id(&detached, &revision()).expect("detached generation"),
        git_topology_generation_id(&attached, &revision()).expect("attached generation")
    );
    assert_eq!(
        store.verify_ref_watermark(&attached.ref_watermark),
        Err(GitTopologyProjectionError::Stale {
            projected: detached.ref_watermark,
            current: attached.ref_watermark,
        })
    );
}

#[test]
fn deleted_branch_generation_proves_watermark_staleness_and_drops_its_commits() {
    // W1: `main` at c plus a `side` branch whose commit d is exclusive to it.
    let with_branch = projection(
        repository("repository.git-topology-edge"),
        GitHeadStateV1::Attached {
            branch: "main".to_owned(),
            commit: oid('c'),
        },
        vec![
            reference("refs/heads/main", 'c'),
            reference("refs/heads/side", 'd'),
        ],
        {
            let mut commits = chain_commits();
            commits.push(commit('d', &['a']));
            commits
        },
    );
    // W2: `side` is deleted, and its exclusive commit is no longer captured.
    let without_branch = chain_projection(GitHeadStateV1::Attached {
        branch: "main".to_owned(),
        commit: oid('c'),
    });
    assert_ne!(with_branch.ref_watermark, without_branch.ref_watermark);

    let before = store(&with_branch);
    let after = store(&without_branch);
    let cancellation = Arc::new(NeverCancelled);

    // The W1 generation can prove it is stale against the W2 watermark.
    assert_eq!(
        before.verify_ref_watermark(&without_branch.ref_watermark),
        Err(GitTopologyProjectionError::Stale {
            projected: with_branch.ref_watermark.clone(),
            current: without_branch.ref_watermark.clone(),
        })
    );
    // And the reverse: the W2 generation rejects the retired W1 watermark.
    assert_eq!(
        after.verify_ref_watermark(&with_branch.ref_watermark),
        Err(GitTopologyProjectionError::Stale {
            projected: without_branch.ref_watermark.clone(),
            current: with_branch.ref_watermark.clone(),
        })
    );
    after
        .verify_ref_watermark(&without_branch.ref_watermark)
        .expect("current watermark verifies against the new generation");
    assert_ne!(
        git_topology_generation_id(&with_branch, &revision()).expect("W1 generation"),
        git_topology_generation_id(&without_branch, &revision()).expect("W2 generation")
    );

    // The retired branch's exclusive commit is reachable in W1 only.
    assert_eq!(
        set(before
            .descendants(&oid('a'), 8, 16, cancellation.clone())
            .expect("W1 descendants")),
        set(vec![oid('b'), oid('c'), oid('d')])
    );
    assert_eq!(
        set(after
            .descendants(&oid('a'), 8, 16, cancellation.clone())
            .expect("W2 descendants")),
        set(vec![oid('b'), oid('c')]),
        "the deleted branch's exclusive commit must not survive into W2"
    );
    // Shared history is untouched by the deletion.
    assert_eq!(
        set(after
            .ancestors(&oid('c'), 8, 16, cancellation.clone())
            .expect("W2 ancestors")),
        set(vec![oid('b'), oid('a'), oid('0')])
    );
    // Starting from the removed commit fails truthfully instead of empty.
    assert!(matches!(
        after.ancestors(&oid('d'), 8, 16, cancellation),
        Err(GitTopologyProjectionError::Contract(_))
    ));
}

/// Linked worktrees resolve to one repository identity root, so both checkouts
/// key the same Git topology projection.
fn repository_for_worktree(worktree_path: &str) -> RepositoryId {
    match worktree_path {
        "/repos/primary" | "/repos/primary-linked-feature" => {
            repository("repository.git-topology-shared")
        }
        _ => repository("repository.git-topology-other"),
    }
}

fn worktree_projection(worktree_path: &str) -> GitTopologyProjectionV1 {
    chain_projection_for(repository_for_worktree(worktree_path))
}

fn chain_projection_for(repository: RepositoryId) -> GitTopologyProjectionV1 {
    projection(
        repository,
        GitHeadStateV1::Attached {
            branch: "main".to_owned(),
            commit: oid('c'),
        },
        vec![reference("refs/heads/main", 'c')],
        chain_commits(),
    )
}

#[test]
fn linked_worktrees_of_one_repository_share_one_topology_generation() {
    let primary = worktree_projection("/repos/primary");
    let linked = worktree_projection("/repos/primary-linked-feature");
    let unrelated = worktree_projection("/repos/unrelated");

    // Same repository identity => same projection, namespace, and generation.
    assert_eq!(primary, linked);
    assert_eq!(
        identity(&primary.repository),
        identity(&linked.repository),
        "linked worktrees must not fork the projection namespace"
    );
    assert_eq!(
        git_topology_generation_id(&primary, &revision()).expect("primary generation"),
        git_topology_generation_id(&linked, &revision()).expect("linked generation")
    );
    assert_eq!(
        git_topology_idempotency_key(&primary, &revision()).expect("primary idempotency"),
        git_topology_idempotency_key(&linked, &revision()).expect("linked idempotency"),
        "a second worktree must replay the same publication, not a new one"
    );

    // The generation published from one worktree is consumed by the other.
    let snapshot = VerifiedGraphSnapshot::memory(manifest(&primary), Arc::new(NeverCancelled))
        .expect("verified snapshot");
    let shared = GitTopologyProjectionStore::from_verified_snapshot(snapshot, &linked)
        .expect("linked worktree consumes the primary worktree's generation");
    assert_eq!(shared.repository(), &linked.repository);
    assert_eq!(shared.ref_watermark(), &primary.ref_watermark);
    assert_eq!(
        set(shared
            .ancestors(&oid('c'), 8, 16, Arc::new(NeverCancelled))
            .expect("shared ancestors")),
        set(vec![oid('b'), oid('a'), oid('0')])
    );

    // A different repository is a different identity, and cannot be consumed.
    assert_ne!(
        identity(&unrelated.repository),
        identity(&primary.repository)
    );
    assert_ne!(
        git_topology_generation_id(&unrelated, &revision()).expect("unrelated generation"),
        git_topology_generation_id(&primary, &revision()).expect("primary generation")
    );
    let foreign = VerifiedGraphSnapshot::memory(manifest(&primary), Arc::new(NeverCancelled))
        .expect("verified snapshot");
    assert!(matches!(
        GitTopologyProjectionStore::from_verified_snapshot(foreign, &unrelated),
        Err(GitTopologyProjectionError::GenerationMismatch)
    ));
}

#[test]
fn cancelled_traversal_fails_typed_instead_of_returning_partial_results() {
    let input = octopus_projection();
    let store = store(&input);

    // A pre-cancelled token stops every traversal entry point.
    let cancelled: Arc<dyn GraphCancellation> = Arc::new(AlwaysCancelled);
    assert_eq!(
        store.ancestors(&oid('f'), 8, 16, Arc::clone(&cancelled)),
        Err(GitTopologyProjectionError::Cancelled)
    );
    assert_eq!(
        store.descendants(&oid('0'), 8, 16, Arc::clone(&cancelled)),
        Err(GitTopologyProjectionError::Cancelled)
    );
    assert_eq!(
        store.merge_base(&oid('a'), &oid('b'), 8, 16, Arc::clone(&cancelled)),
        Err(GitTopologyProjectionError::Cancelled)
    );

    // Cancelling mid-walk is also typed, never a silently shortened answer.
    let complete = store
        .ancestors(&oid('f'), 8, 16, Arc::new(NeverCancelled))
        .expect("uncancelled ancestors");
    assert_eq!(
        complete.len(),
        4,
        "uncancelled traversal sees four ancestors"
    );
    let mid_walk = store.ancestors(&oid('f'), 8, 16, Arc::new(CancelAfter::new(3)));
    assert_eq!(
        mid_walk,
        Err(GitTopologyProjectionError::Cancelled),
        "a traversal cancelled while walking must not return a partial vector"
    );
}

#[test]
fn traversal_order_is_deterministic_without_caller_sorting() {
    let input = chain_projection(GitHeadStateV1::Attached {
        branch: "main".to_owned(),
        commit: oid('c'),
    });
    let published = store(&input);
    let cancellation = Arc::new(NeverCancelled);

    // Ancestors are returned nearest-first, in ascending depth order.
    let ancestors = published
        .ancestors(&oid('c'), 8, 16, cancellation.clone())
        .expect("ordered ancestors");
    assert_eq!(ancestors, vec![oid('b'), oid('a'), oid('0')]);

    // Descendants are returned nearest-first from the root.
    let descendants = published
        .descendants(&oid('0'), 8, 16, cancellation.clone())
        .expect("ordered descendants");
    assert_eq!(descendants, vec![oid('a'), oid('b'), oid('c')]);

    // The order is stable across repeated reads of one generation.
    assert_eq!(
        ancestors,
        published
            .ancestors(&oid('c'), 8, 16, cancellation.clone())
            .expect("replayed ancestors")
    );
    assert_eq!(
        descendants,
        published
            .descendants(&oid('0'), 8, 16, cancellation.clone())
            .expect("replayed descendants")
    );

    // A second store over the same projection reproduces the same order.
    let replayed = store(&input);
    assert_eq!(
        ancestors,
        replayed
            .ancestors(&oid('c'), 8, 16, cancellation.clone())
            .expect("republished ancestors")
    );
    assert_eq!(
        descendants,
        replayed
            .descendants(&oid('0'), 8, 16, cancellation)
            .expect("republished descendants")
    );
}
