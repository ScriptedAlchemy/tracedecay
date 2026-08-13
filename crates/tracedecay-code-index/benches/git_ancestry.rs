use std::sync::Arc;
use std::time::Duration;

use criterion::{BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};
use tracedecay_code_index::git_projection::{
    GIT_TOPOLOGY_PROJECTOR_REVISION_V1, GitTopologyProjectionStore, GitTopologyProjectionV1,
    GitTopologyRefV1, build_git_topology_manifest_checked, git_topology_namespace,
    git_topology_projection_identity, git_topology_ref_watermark,
};
use tracedecay_domain::{
    GitCommitIdentityV1, GitCommitMetadataV1, GitCoverageV1, GitHeadStateV1, GitHistoryV1,
    GitOidV1, ManifestDigest, RefId, RepositoryId, UtcMicros,
};
use tracedecay_graph_db::{
    GraphGenerationManifest, GraphProjectorRevision, NeverCancelled, VerifiedGraphSnapshot,
};

#[path = "support/git.rs"]
mod support;

use support::PersistentGitGraph;

const MAIN_DEPTH: usize = 4_096;
const DIVERGENCE_DEPTH: usize = MAIN_DEPTH / 2;
const SIDE_DEPTH: usize = 1_024;
const SIDE_OID_BASE: usize = 1_000_000;
const MERGE_OID: usize = 2_000_000;
const TOTAL_COMMITS: usize = MAIN_DEPTH + SIDE_DEPTH + 2;
const MAX_DEPTH: usize = MAIN_DEPTH + 1;

struct GitFixture {
    projection: GitTopologyProjectionV1,
    manifest: GraphGenerationManifest,
    root: GitOidV1,
    main_tip: GitOidV1,
    side_tip: GitOidV1,
    merge: GitOidV1,
    merge_base: GitOidV1,
}

impl GitFixture {
    fn new() -> Self {
        let repository = RepositoryId::new("repository.git-ancestry-benchmark")
            .expect("benchmark repository identity is valid");
        let root = oid(0);
        let mut commits = Vec::with_capacity(TOTAL_COMMITS);
        commits.push(commit(0, Vec::new()));
        for depth in 1..=MAIN_DEPTH {
            commits.push(commit(depth, vec![oid(depth - 1)]));
        }
        for depth in 1..=SIDE_DEPTH {
            let parent = if depth == 1 {
                oid(DIVERGENCE_DEPTH)
            } else {
                oid(SIDE_OID_BASE + depth - 1)
            };
            commits.push(commit(SIDE_OID_BASE + depth, vec![parent]));
        }
        let main_tip = oid(MAIN_DEPTH);
        let side_tip = oid(SIDE_OID_BASE + SIDE_DEPTH);
        let merge = oid(MERGE_OID);
        commits.push(commit(MERGE_OID, vec![main_tip.clone(), side_tip.clone()]));

        let refs = vec![
            GitTopologyRefV1 {
                reference: RefId::new("refs/heads/main").expect("benchmark main ref is valid"),
                target: Some(merge.clone()),
            },
            GitTopologyRefV1 {
                reference: RefId::new("refs/heads/side").expect("benchmark side ref is valid"),
                target: Some(side_tip.clone()),
            },
        ];
        let head = GitHeadStateV1::Attached {
            branch: "main".to_owned(),
            commit: merge.clone(),
        };
        let ref_watermark = git_topology_ref_watermark(&repository, &head, &refs)
            .expect("benchmark ref watermark is valid");
        let projection = GitTopologyProjectionV1 {
            repository: repository.clone(),
            head,
            refs,
            history: GitHistoryV1 {
                repository: repository.clone(),
                commits,
                truncated: false,
                coverage: GitCoverageV1::complete(),
            },
            ref_watermark,
            branch_stacks: Vec::new(),
            worktree_occupancies: Vec::new(),
        };
        let identity = git_topology_projection_identity(
            git_topology_namespace(&repository).expect("benchmark namespace is valid"),
        )
        .expect("benchmark projection identity is valid");
        let manifest = build_git_topology_manifest_checked(
            identity,
            &projection,
            &projector_revision(),
            &|| Ok(()),
        )
        .expect("benchmark Git topology manifest builds");
        Self {
            projection,
            manifest,
            root,
            main_tip,
            side_tip,
            merge,
            merge_base: oid(DIVERGENCE_DEPTH),
        }
    }

    fn expected_ancestors(&self) -> Vec<GitOidV1> {
        let mut expected = (0..=MAIN_DEPTH)
            .map(oid)
            .chain((1..=SIDE_DEPTH).map(|depth| oid(SIDE_OID_BASE + depth)))
            .collect::<Vec<_>>();
        expected.sort();
        expected
    }

    fn expected_descendants(&self) -> Vec<GitOidV1> {
        let mut expected = (1..=MAIN_DEPTH)
            .map(oid)
            .chain((1..=SIDE_DEPTH).map(|depth| oid(SIDE_OID_BASE + depth)))
            .chain([self.merge.clone()])
            .collect::<Vec<_>>();
        expected.sort();
        expected
    }
}

fn oid(value: usize) -> GitOidV1 {
    GitOidV1::new(format!("{value:040x}")).expect("benchmark object identity is valid")
}

fn commit(value: usize, parents: Vec<GitOidV1>) -> GitCommitMetadataV1 {
    let identity = GitCommitIdentityV1 {
        name: "TraceDecay benchmark".to_owned(),
        email: "benchmark@tracedecay.dev".to_owned(),
        at: UtcMicros(value as i64),
    };
    GitCommitMetadataV1 {
        commit: oid(value),
        tree: oid(value + 3_000_000),
        parents,
        author: identity.clone(),
        committer: identity,
        subject: format!("deterministic commit {value}"),
        message_digest: ManifestDigest::new(format!("sha256:{value:064x}"))
            .expect("benchmark message digest is valid"),
    }
}

fn projector_revision() -> GraphProjectorRevision {
    GraphProjectorRevision::try_from(GIT_TOPOLOGY_PROJECTOR_REVISION_V1.to_owned())
        .expect("benchmark projector revision is valid")
}

fn store(
    snapshot: VerifiedGraphSnapshot,
    projection: &GitTopologyProjectionV1,
) -> GitTopologyProjectionStore {
    GitTopologyProjectionStore::from_verified_snapshot(snapshot, projection)
        .expect("benchmark verified Git topology store opens")
}

fn preflight(store: &GitTopologyProjectionStore, fixture: &GitFixture) {
    let mut ancestors = store
        .ancestors(
            &fixture.merge,
            MAX_DEPTH,
            TOTAL_COMMITS,
            Arc::new(NeverCancelled),
        )
        .expect("benchmark ancestors preflight succeeds");
    ancestors.sort();
    assert_eq!(ancestors, fixture.expected_ancestors());

    let mut descendants = store
        .descendants(
            &fixture.root,
            MAX_DEPTH,
            TOTAL_COMMITS,
            Arc::new(NeverCancelled),
        )
        .expect("benchmark descendants preflight succeeds");
    descendants.sort();
    assert_eq!(descendants, fixture.expected_descendants());

    let merge_base = store
        .merge_base(
            &fixture.main_tip,
            &fixture.side_tip,
            MAX_DEPTH,
            TOTAL_COMMITS,
            Arc::new(NeverCancelled),
        )
        .expect("benchmark merge-base preflight succeeds");
    assert_eq!(merge_base.as_ref(), Some(&fixture.merge_base));
}

fn git_ancestry(criterion: &mut Criterion) {
    let fixture = GitFixture::new();
    let expected_digest = fixture
        .manifest
        .expected_recovered_digest(&|| Ok(()))
        .expect("benchmark recovered digest is deterministic");
    let expected_generation = fixture.manifest.generation.clone();
    let mut persistent = PersistentGitGraph::new();
    let snapshot = persistent.publish(fixture.manifest.clone());
    assert_eq!(snapshot.generation(), &expected_generation);
    assert_eq!(snapshot.verified_head().recovered_digest, expected_digest);
    let published = store(snapshot, &fixture.projection);
    preflight(&published, &fixture);

    let mut group = criterion.benchmark_group("git_ancestry/warm_verified_snapshot");
    group.throughput(Throughput::Elements((TOTAL_COMMITS - 1) as u64));
    group.bench_function(BenchmarkId::new("ancestors", TOTAL_COMMITS), |bencher| {
        bencher.iter(|| {
            black_box(
                published
                    .ancestors(
                        black_box(&fixture.merge),
                        MAX_DEPTH,
                        TOTAL_COMMITS,
                        Arc::new(NeverCancelled),
                    )
                    .expect("benchmark ancestors traversal succeeds"),
            )
        });
    });
    group.bench_function(BenchmarkId::new("descendants", TOTAL_COMMITS), |bencher| {
        bencher.iter(|| {
            black_box(
                published
                    .descendants(
                        black_box(&fixture.root),
                        MAX_DEPTH,
                        TOTAL_COMMITS,
                        Arc::new(NeverCancelled),
                    )
                    .expect("benchmark descendants traversal succeeds"),
            )
        });
    });
    group.throughput(Throughput::Elements(2));
    group.bench_function(BenchmarkId::new("merge_base", TOTAL_COMMITS), |bencher| {
        bencher.iter(|| {
            black_box(
                published
                    .merge_base(
                        black_box(&fixture.main_tip),
                        black_box(&fixture.side_tip),
                        MAX_DEPTH,
                        TOTAL_COMMITS,
                        Arc::new(NeverCancelled),
                    )
                    .expect("benchmark merge-base traversal succeeds"),
            )
        });
    });
    group.finish();

    let mut digest_group = criterion.benchmark_group("git_ancestry/digest_identity");
    digest_group.throughput(Throughput::Elements(TOTAL_COMMITS as u64));
    digest_group.bench_function("full_recovered_state", |bencher| {
        bencher.iter(|| {
            black_box(
                fixture
                    .manifest
                    .expected_recovered_digest(&|| Ok(()))
                    .expect("benchmark recovered digest recomputes"),
            )
        });
    });
    digest_group.finish();

    drop(published);
    let recovered = persistent.recover_snapshot();
    assert_eq!(recovered.generation(), &expected_generation);
    assert_eq!(recovered.verified_head().recovered_digest, expected_digest);
    let recovered_store = store(recovered, &fixture.projection);
    preflight(&recovered_store, &fixture);
    drop(recovered_store);

    let mut reopen_group = criterion.benchmark_group("git_ancestry/reopen_verified_snapshot");
    reopen_group.throughput(Throughput::Elements(TOTAL_COMMITS as u64));
    reopen_group.bench_function("close_reopen_verify_digest", |bencher| {
        bencher.iter(|| {
            let recovered = persistent.recover_snapshot();
            black_box(store(recovered, &fixture.projection))
        });
    });
    reopen_group.finish();
}

criterion_group! {
    name = benches;
    config = Criterion::default()
        .sample_size(100)
        .measurement_time(Duration::from_secs(20));
    targets = git_ancestry
}
criterion_main!(benches);
