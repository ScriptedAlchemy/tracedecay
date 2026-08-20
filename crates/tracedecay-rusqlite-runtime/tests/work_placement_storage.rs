//! Durable Work placement storage contract: compare-and-swap publication,
//! database-enforced exclusivity of a managed target root, authority
//! isolation, and restart durability over the registered exact-SQL channel.
//!
//! The exclusivity assertion below is the point of this suite. Plan 32
//! (`docs/plans/tracedecay-v2/32-dynamic-workflow-runtime-and-sdk.md`,
//! "Placement, topology, and safe Git effects") calls linked and isolated
//! placements "canonical, exclusive, fenced"; the application service produces
//! the typed refusal, but only the partial unique index makes the rule survive
//! a crash between the service's read and its write, so the rule is tested
//! where it is enforced.

mod common;
mod work_registered_store;

use std::collections::BTreeSet;

use tracedecay_application::{WorkPlacementStorageError, WorkPlacementStoragePort};
use tracedecay_domain::{
    ActorId, ManifestDigest, ProjectId, RepositoryId, RunId, TaskId, UtcMicros, WorkAuthority,
    WorkPlacementBlockerV1, WorkPlacementIdentityV1, WorkPlacementKindV1,
    WorkPlacementObservationV1, WorkPlacementPreflightV1, WorkPlacementStateV1,
    WorkPlacementTargetV1, WorkPlacementV1, WorktreeId,
};

use common::fixture_abs_root;
use work_registered_store::RegisteredWorkStore;

static ROOT: std::sync::LazyLock<String> =
    std::sync::LazyLock::new(|| fixture_abs_root("/workspace/placement-storage"));

fn id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    T::Error: std::fmt::Debug,
{
    T::try_from(value.to_owned()).unwrap()
}

fn digest(byte: char) -> ManifestDigest {
    ManifestDigest::new(format!("sha256:{}", byte.to_string().repeat(64))).unwrap()
}

fn authority(actor: &str) -> WorkAuthority {
    authority_in_worktree_with_policy(actor, "worktree.placement.storage", 'a')
}

fn authority_in_worktree_with_policy(actor: &str, worktree: &str, policy: char) -> WorkAuthority {
    WorkAuthority::new(
        id::<ProjectId>("project.placement.storage"),
        id::<RepositoryId>("repository.placement.storage"),
        id::<WorktreeId>(worktree),
        id::<ActorId>(actor),
        digest(policy),
    )
    .unwrap()
}

#[test]
fn authority_in_worktree_a_targeting_root_b_blocks_cleanup_of_b_across_lineage() {
    let store = RegisteredWorkStore::start("placement-cleanup-holder-scope");
    let old_policy = authority_in_worktree_with_policy(
        "actor.placement.current",
        "worktree.placement.old-policy",
        '9',
    );
    let other_actor = authority_in_worktree_with_policy(
        "actor.placement.delegated",
        "worktree.placement.other-actor",
        'a',
    );
    let old_root = "/workspace/placement-target-b";
    let other_root = "/workspace/placement-other-actor";
    store
        .storage()
        .publish_placement(&old_policy, None, &admitted("run.old-policy", old_root))
        .unwrap();
    store
        .storage()
        .publish_placement(&other_actor, None, &admitted("run.other-actor", other_root))
        .unwrap();

    for (authority, root) in [(&old_policy, old_root), (&other_actor, other_root)] {
        assert!(
            store
                .storage()
                .has_target_holder_in_exact_repository_root(
                    authority.project_id(),
                    authority.repository_id(),
                    root,
                )
                .unwrap(),
            "cleanup must see placements outside its current actor/policy lineage"
        );
    }
    assert!(
        !store
            .storage()
            .has_target_holder_in_exact_repository_root(
                old_policy.project_id(),
                old_policy.repository_id(),
                "/workspace/placement-unrelated",
            )
            .unwrap()
    );
}

fn identity(run: &str) -> WorkPlacementIdentityV1 {
    WorkPlacementIdentityV1::new(id::<TaskId>("task.placement.storage"), id::<RunId>(run))
}

fn target(root: &str) -> WorkPlacementTargetV1 {
    WorkPlacementTargetV1::new(
        WorkPlacementKindV1::LinkedWorktree,
        Some(root.to_owned()),
        false,
        true,
    )
    .unwrap()
}

fn clean() -> WorkPlacementObservationV1 {
    WorkPlacementObservationV1 {
        dirty_tracked_paths: 0,
        untracked_paths: 0,
        unique_commits: Some(0),
        readable: true,
        active_holder: false,
        network_required: false,
        observed_at: UtcMicros(100),
    }
}

fn admitted(run: &str, root: &str) -> WorkPlacementV1 {
    let preflight = WorkPlacementPreflightV1::evaluate(identity(run), target(root), clean());
    WorkPlacementV1::admit(&preflight, Some(UtcMicros(50_000)), UtcMicros(200)).unwrap()
}

#[test]
fn an_unplaced_run_has_no_row_and_no_holder() {
    let store = RegisteredWorkStore::start("placement-absent");
    let authority = authority("actor.placement.absent");
    assert_eq!(
        store
            .storage()
            .load_placement(&authority, &identity("run.a"))
            .unwrap(),
        None
    );
    assert_eq!(
        store.storage().target_holder(&authority, &ROOT).unwrap(),
        None
    );
}

#[test]
fn the_first_admission_inserts_and_a_racing_first_admission_conflicts() {
    let store = RegisteredWorkStore::start("placement-first");
    let authority = authority("actor.placement.first");
    let placement = admitted("run.a", &ROOT);
    store
        .storage()
        .publish_placement(&authority, None, &placement)
        .unwrap();
    assert_eq!(
        store
            .storage()
            .load_placement(&authority, &identity("run.a"))
            .unwrap(),
        Some(placement.clone())
    );
    assert_eq!(
        store.storage().target_holder(&authority, &ROOT).unwrap(),
        Some(identity("run.a"))
    );
    assert_eq!(
        store
            .storage()
            .publish_placement(&authority, None, &placement)
            .expect_err("a racing first admission conflicts"),
        WorkPlacementStorageError::AuthorityConflict
    );
}

#[test]
fn the_database_refuses_a_second_holder_of_the_same_managed_root() {
    let store = RegisteredWorkStore::start("placement-exclusive");
    let authority = authority("actor.placement.exclusive");
    store
        .storage()
        .publish_placement(&authority, None, &admitted("run.a", &ROOT))
        .unwrap();

    // A different run naming the same root is refused by the exclusivity index
    // even though its own row does not exist yet.
    assert_eq!(
        store
            .storage()
            .publish_placement(&authority, None, &admitted("run.b", &ROOT))
            .expect_err("a held root is exclusive"),
        WorkPlacementStorageError::AuthorityConflict
    );
    assert_eq!(store.count("work_placements_v1"), 1);

    // A different root is unaffected.
    store
        .storage()
        .publish_placement(
            &authority,
            None,
            &admitted("run.c", "/workspace/placement-storage-other"),
        )
        .unwrap();
    assert_eq!(store.count("work_placements_v1"), 2);
}

#[test]
fn a_released_placement_frees_its_root_and_a_quarantined_one_does_not() {
    let store = RegisteredWorkStore::start("placement-release");
    let authority = authority("actor.placement.release");
    let placement = admitted("run.a", &ROOT);
    store
        .storage()
        .publish_placement(&authority, None, &placement)
        .unwrap();

    let quarantined = placement
        .release(
            BTreeSet::from([WorkPlacementBlockerV1::UniqueCommits]),
            UtcMicros(400),
        )
        .unwrap();
    store
        .storage()
        .publish_placement(
            &authority,
            Some(placement.authority_version()),
            &quarantined,
        )
        .unwrap();
    // Quarantine retains the bytes, so the root is still held.
    assert_eq!(
        store.storage().target_holder(&authority, &ROOT).unwrap(),
        Some(identity("run.a"))
    );
    assert_eq!(
        store
            .storage()
            .publish_placement(&authority, None, &admitted("run.b", &ROOT))
            .expect_err("a quarantined root is still held"),
        WorkPlacementStorageError::AuthorityConflict
    );

    let released = quarantined
        .release(BTreeSet::new(), UtcMicros(600))
        .unwrap();
    store
        .storage()
        .publish_placement(&authority, Some(quarantined.authority_version()), &released)
        .unwrap();
    assert_eq!(released.state(), WorkPlacementStateV1::Released);
    assert_eq!(
        store.storage().target_holder(&authority, &ROOT).unwrap(),
        None
    );
    // Only now can another run take it.
    store
        .storage()
        .publish_placement(&authority, None, &admitted("run.b", &ROOT))
        .unwrap();
}

#[test]
fn a_stale_version_conflicts_and_rows_survive_a_restart_per_authority() {
    let store = RegisteredWorkStore::start("placement-isolation");
    let mine = authority("actor.placement.mine");
    let peer = authority("actor.placement.peer");
    let placement = admitted("run.a", &ROOT);
    store
        .storage()
        .publish_placement(&mine, None, &placement)
        .unwrap();
    let released = placement.release(BTreeSet::new(), UtcMicros(400)).unwrap();
    assert_eq!(
        store
            .storage()
            .publish_placement(&mine, Some(placement.authority_version() + 9), &released)
            .expect_err("stale authority version"),
        WorkPlacementStorageError::AuthorityConflict
    );

    // Another actor holds nothing here, and the same root is free for it.
    assert_eq!(store.storage().target_holder(&peer, &ROOT).unwrap(), None);
    store
        .storage()
        .publish_placement(&peer, None, &admitted("run.a", &ROOT))
        .unwrap();

    let restarted = store.restart("placement-isolation");
    assert_eq!(
        restarted
            .storage()
            .load_placement(&mine, &identity("run.a"))
            .unwrap(),
        Some(placement)
    );
}
