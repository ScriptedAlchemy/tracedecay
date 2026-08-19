use tracedecay_domain::git::repository_state::{
    RepositoryIndexSnapshotV1, RepositoryIndexStateV1, RepositoryStateSnapshotV1,
    RepositoryWorkingTreeSnapshotV1, RepositoryWorkingTreeStateV1,
};
use tracedecay_domain::{
    GitCoverageV1, GitHeadStateV1, GitObjectFormatV1, GitOidV1, GitOperationStateV1,
    ManifestDigest, ProjectId, RepositoryId, UtcMicros, WorktreeId,
};

fn id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    <T as TryFrom<String>>::Error: std::fmt::Debug,
{
    T::try_from(value.to_owned()).expect("fixture id is canonical")
}

fn oid(byte: char) -> GitOidV1 {
    GitOidV1::new(byte.to_string().repeat(40)).expect("fixture oid is canonical")
}

fn digest(byte: char) -> ManifestDigest {
    ManifestDigest::new(format!("sha256:{}", byte.to_string().repeat(64)))
        .expect("fixture digest is canonical")
}

fn snapshot(head: GitOidV1) -> RepositoryStateSnapshotV1 {
    RepositoryStateSnapshotV1::new(
        id::<ProjectId>("project.fixture"),
        id::<RepositoryId>("repository.fixture"),
        Some(id::<WorktreeId>("worktree.fixture")),
        1,
        GitObjectFormatV1::Sha1,
        GitHeadStateV1::Attached {
            branch: "refs/heads/main".to_owned(),
            commit: head,
        },
        RepositoryIndexSnapshotV1 {
            checksum: digest('a'),
            tree_id: Some(oid('b')),
            state: RepositoryIndexStateV1::Clean,
            unmerged_stage_digest: None,
        },
        RepositoryWorkingTreeSnapshotV1 {
            state: RepositoryWorkingTreeStateV1::Clean,
            tracked_digest: digest('c'),
            untracked_name_digest: None,
            ignored_collision_digest: None,
        },
        GitOperationStateV1::None,
        None,
        None,
        None,
        None,
        None,
        UtcMicros(42),
        GitCoverageV1::complete(),
    )
    .unwrap()
}

#[test]
fn repository_state_snapshot_is_content_addressed_and_exact() {
    let first = snapshot(oid('d'));
    let repeated = snapshot(oid('d'));
    let changed_head = snapshot(oid('e'));

    first.validate().unwrap();
    assert_eq!(first.snapshot_id(), repeated.snapshot_id());
    assert_ne!(first.snapshot_id(), changed_head.snapshot_id());
}

#[test]
fn repository_state_snapshot_rejects_tampered_identity() {
    let value = serde_json::to_value(snapshot(oid('d'))).unwrap();
    let mut tampered = value;
    tampered["snapshot_id"] = serde_json::json!("repository.state.v1.invalid");

    assert!(serde_json::from_value::<RepositoryStateSnapshotV1>(tampered).is_err());
}

#[test]
fn mutation_ineligible_states_remain_explicit() {
    let mut state = snapshot(oid('d'));
    state.index.state = RepositoryIndexStateV1::Unmerged;
    state.index.unmerged_stage_digest = Some(digest('f'));

    assert!(!state.is_mutation_eligible());
    assert!(
        state.validate().is_err(),
        "a changed index state requires a freshly captured snapshot identity"
    );
}
