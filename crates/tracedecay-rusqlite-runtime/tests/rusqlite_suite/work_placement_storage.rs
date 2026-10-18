//! Durable Work placement storage contract: compare-and-swap publication,
//! database-enforced exclusivity of a managed target root, authority
//! isolation, and restart durability over the registered exact-SQL channel.
//!
//! Linked and isolated placements are exclusive and fenced. The application
//! service produces the typed refusal, but only the partial unique index makes
//! the rule survive a crash between the service's read and its write, so the
//! rule is tested where it is enforced.

use crate::common;
use crate::work_registered_store;

use std::collections::BTreeSet;

use tracedecay_contracts::{
    WorkPlacementPublicationPort, WorkPlacementStorageError, WorkPlacementStoragePort,
};
use tracedecay_domain::{
    ActorId, ProjectId, RepositoryId, RunId, TaskId, UtcMicros, WorkAuthority,
    WorkPlacementBlockerV1, WorkPlacementIdentityV1, WorkPlacementKindV1,
    WorkPlacementObservationV1, WorkPlacementPreflightV1, WorkPlacementStateV1,
    WorkPlacementTargetV1, WorkPlacementV1, WorktreeId,
};

use common::fixture_abs_root;
use tracedecay_rusqlite_runtime::work::install_work_schema;
use work_registered_store::RegisteredWorkStore;

static ROOT: std::sync::LazyLock<String> =
    std::sync::LazyLock::new(|| fixture_abs_root("/workspace/placement-storage"));

use tracedecay_domain::test_fixtures::id;

use tracedecay_domain::test_fixtures::digest;

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

/// Publishes `next` in one writer job.
fn publish(
    store: &RegisteredWorkStore,
    authority: &WorkAuthority,
    expected: Option<u64>,
    next: &WorkPlacementV1,
) -> Result<(), WorkPlacementStorageError> {
    let (authority, next) = (authority.clone(), next.clone());
    tracedecay_rusqlite_runtime::native_runtime::block_on(
        store
            .storage()
            .transact(move |storage| storage.publish_placement(&authority, expected, &next)),
    )
    .expect("native runtime")
    .expect("the writer runs the job")
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
    let old_root = fixture_abs_root("/workspace/placement-target-b");
    let other_root = fixture_abs_root("/workspace/placement-other-actor");
    publish(
        &store,
        &old_policy,
        None,
        &admitted("run.old-policy", &old_root),
    )
    .unwrap();
    publish(
        &store,
        &other_actor,
        None,
        &admitted("run.other-actor", &other_root),
    )
    .unwrap();

    for (authority, root) in [
        (&old_policy, old_root.as_str()),
        (&other_actor, other_root.as_str()),
    ] {
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
                &fixture_abs_root("/workspace/placement-unrelated"),
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
    assert!(
        !store
            .storage()
            .target_held_by_other(&authority, &identity("run.a"), &ROOT)
            .unwrap()
    );
}

#[test]
fn the_first_admission_inserts_and_a_racing_first_admission_conflicts() {
    let store = RegisteredWorkStore::start("placement-first");
    let authority = authority("actor.placement.first");
    let placement = admitted("run.a", &ROOT);
    publish(&store, &authority, None, &placement).unwrap();
    assert_eq!(
        store
            .storage()
            .load_placement(&authority, &identity("run.a"))
            .unwrap(),
        Some(placement.clone())
    );
    assert!(held_by_other(&store, &authority, "run.b"));
    assert!(
        !held_by_other(&store, &authority, "run.a"),
        "a run is not its own blocker"
    );
    assert_eq!(
        publish(&store, &authority, None, &placement)
            .expect_err("a racing first admission conflicts"),
        WorkPlacementStorageError::AuthorityConflict
    );
}

#[test]
fn the_database_refuses_a_second_holder_of_the_same_managed_root() {
    let store = RegisteredWorkStore::start("placement-exclusive");
    let authority = authority("actor.placement.exclusive");
    publish(&store, &authority, None, &admitted("run.a", &ROOT)).unwrap();

    // A different run naming the same root is refused by the exclusivity index
    // even though its own row does not exist yet.
    assert_eq!(
        publish(&store, &authority, None, &admitted("run.b", &ROOT))
            .expect_err("a held root is exclusive"),
        WorkPlacementStorageError::AuthorityConflict
    );
    assert_eq!(store.count("work_placements_v1"), 1);

    // A different root is unaffected.
    publish(
        &store,
        &authority,
        None,
        &admitted(
            "run.c",
            &fixture_abs_root("/workspace/placement-storage-other"),
        ),
    )
    .unwrap();
    assert_eq!(store.count("work_placements_v1"), 2);
}

#[test]
fn a_released_placement_frees_its_root_and_a_quarantined_one_does_not() {
    let store = RegisteredWorkStore::start("placement-release");
    let authority = authority("actor.placement.release");
    let placement = admitted("run.a", &ROOT);
    publish(&store, &authority, None, &placement).unwrap();

    let quarantined = placement
        .release(
            BTreeSet::from([WorkPlacementBlockerV1::UniqueCommits]),
            UtcMicros(400),
        )
        .unwrap();
    publish(
        &store,
        &authority,
        Some(placement.authority_version()),
        &quarantined,
    )
    .unwrap();
    // Quarantine retains the bytes, so the root is still held.
    assert!(held_by_other(&store, &authority, "run.b"));
    assert_eq!(
        publish(&store, &authority, None, &admitted("run.b", &ROOT))
            .expect_err("a quarantined root is still held"),
        WorkPlacementStorageError::AuthorityConflict
    );

    let released = quarantined
        .release(BTreeSet::new(), UtcMicros(600))
        .unwrap();
    publish(
        &store,
        &authority,
        Some(quarantined.authority_version()),
        &released,
    )
    .unwrap();
    assert_eq!(released.state(), WorkPlacementStateV1::Released);
    assert!(!held_by_other(&store, &authority, "run.b"));
    // Only now can another run take it.
    publish(&store, &authority, None, &admitted("run.b", &ROOT)).unwrap();
}

#[test]
fn a_stale_version_conflicts_and_rows_survive_a_restart_per_authority() {
    let store = RegisteredWorkStore::start("placement-isolation");
    let mine = authority("actor.placement.mine");
    let placement = admitted("run.a", &ROOT);
    publish(&store, &mine, None, &placement).unwrap();
    let released = placement.release(BTreeSet::new(), UtcMicros(400)).unwrap();
    assert_eq!(
        publish(
            &store,
            &mine,
            Some(placement.authority_version() + 9),
            &released
        )
        .expect_err("stale authority version"),
        WorkPlacementStorageError::AuthorityConflict
    );

    let restarted = store.restart("placement-isolation");
    assert_eq!(
        restarted
            .storage()
            .load_placement(&mine, &identity("run.a"))
            .unwrap(),
        Some(placement)
    );
}

/// Whether `authority` sees `ROOT` held by a placement other than `run`.
fn held_by_other(store: &RegisteredWorkStore, authority: &WorkAuthority, run: &str) -> bool {
    store
        .storage()
        .target_held_by_other(authority, &identity(run), &ROOT)
        .unwrap()
}

#[test]
fn another_actor_policy_or_worktree_cannot_hold_a_root_while_it_is_held() {
    let store = RegisteredWorkStore::start("placement-cross-authority");
    let holder = authority("actor.placement.holder");
    let held = admitted("run.a", &ROOT);
    publish(&store, &holder, None, &held).unwrap();

    let rivals = [
        ("another actor", authority("actor.placement.peer")),
        (
            "another policy digest",
            authority_in_worktree_with_policy(
                "actor.placement.holder",
                "worktree.placement.storage",
                'b',
            ),
        ),
        (
            "another worktree route",
            authority_in_worktree_with_policy(
                "actor.placement.holder",
                "worktree.placement.elsewhere",
                'a',
            ),
        ),
    ];
    for (label, rival) in &rivals {
        // The rival even reuses the holder's task and run ids.
        assert!(
            held_by_other(&store, rival, "run.a"),
            "{label} must see the root as occupied"
        );
        assert_eq!(
            publish(&store, rival, None, &admitted("run.a", &ROOT))
                .expect_err("the database refuses a second holder"),
            WorkPlacementStorageError::AuthorityConflict,
            "{label}"
        );
    }
    assert_eq!(store.count("work_placements_v1"), 1);

    // Exclusion survives a restart.
    let store = store.restart("placement-cross-authority");
    let peer = &rivals[0].1;
    assert!(held_by_other(&store, peer, "run.a"));
    assert_eq!(
        publish(&store, peer, None, &admitted("run.a", &ROOT)).expect_err("still held"),
        WorkPlacementStorageError::AuthorityConflict
    );

    // A stale release by the rival cannot clear the holder's reservation.
    let rival_release = held.release(BTreeSet::new(), UtcMicros(400)).unwrap();
    assert_eq!(
        publish(&store, peer, Some(held.authority_version()), &rival_release)
            .expect_err("the rival has no such row"),
        WorkPlacementStorageError::AuthorityConflict
    );
    assert!(held_by_other(&store, peer, "run.a"));

    // A valid release by the holder lets the other actor in.
    publish(
        &store,
        &holder,
        Some(held.authority_version()),
        &rival_release,
    )
    .unwrap();
    assert!(!held_by_other(&store, peer, "run.a"));
    publish(&store, peer, None, &admitted("run.a", &ROOT)).unwrap();
}

/// Rows written under the pre-exclusivity index, which keyed the root on the
/// actor and policy digest, so two authorities could hold one root.
fn seed_pre_exclusivity_duplicates(connection: &rusqlite::Connection) {
    install_work_schema(connection).unwrap();
    connection
        .execute_batch(
            "DROP INDEX work_placements_v1_exclusive_target;
             CREATE UNIQUE INDEX work_placements_v1_exclusive_root
                 ON work_placements_v1 (
                     project_id, repository_id, worktree_id, actor_id, policy_digest, target_root
                 )
                 WHERE target_root IS NOT NULL AND state IN ('admitted', 'quarantined');",
        )
        .unwrap();
    let insert = |actor: &str, placement: &WorkPlacementV1| {
        let authority = authority(actor);
        connection
            .execute(
                "INSERT INTO work_placements_v1 (
                     project_id, repository_id, worktree_id, actor_id, policy_digest,
                     task_id, run_id, kind, target_root, state, authority_version,
                     placement_payload
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'linked_worktree', ?8, ?9, ?10, ?11)",
                rusqlite::params![
                    authority.project_id().as_str(),
                    authority.repository_id().as_str(),
                    authority.worktree_id().as_str(),
                    authority.actor_id().as_str(),
                    authority.policy_digest().as_str(),
                    placement.identity().task_id().as_str(),
                    placement.identity().run_id().as_str(),
                    placement.target().root(),
                    match placement.state() {
                        WorkPlacementStateV1::Admitted => "admitted",
                        WorkPlacementStateV1::Released => "released",
                        WorkPlacementStateV1::Quarantined => "quarantined",
                    },
                    placement.authority_version() as i64,
                    serde_json::to_string(placement).unwrap(),
                ],
            )
            .unwrap();
    };
    let first = admitted("run.first", &ROOT);
    let quarantined_first = first
        .release(
            BTreeSet::from([WorkPlacementBlockerV1::UniqueCommits]),
            UtcMicros(300),
        )
        .unwrap();
    // The oldest holder is itself quarantined: it keeps the root.
    insert("actor.placement.first", &quarantined_first);
    insert("actor.placement.second", &admitted("run.second", &ROOT));
    let third = admitted("run.third", &ROOT)
        .release(
            BTreeSet::from([WorkPlacementBlockerV1::DirtyTrackedFiles]),
            UtcMicros(300),
        )
        .unwrap();
    insert("actor.placement.third", &third);
    // A different root and a released history row are untouched.
    let other = fixture_abs_root("/workspace/placement-storage-other");
    insert("actor.placement.second", &admitted("run.other", &other));
    let released = admitted("run.released", &ROOT)
        .release(BTreeSet::new(), UtcMicros(300))
        .unwrap();
    insert("actor.placement.fourth", &released);
}

#[test]
fn migration_keeps_the_oldest_holder_and_quarantines_later_duplicates() {
    let store =
        RegisteredWorkStore::start_seeded("placement-migration", seed_pre_exclusivity_duplicates);
    let load = |actor: &str, run: &str| {
        store
            .storage()
            .load_placement(&authority(actor), &identity(run))
            .unwrap()
            .expect("the row survives the migration")
    };

    let first = load("actor.placement.first", "run.first");
    assert_eq!(first.state(), WorkPlacementStateV1::Quarantined);
    assert_eq!(
        first.blockers(),
        &BTreeSet::from([WorkPlacementBlockerV1::UniqueCommits])
    );

    let second = load("actor.placement.second", "run.second");
    assert_eq!(second.state(), WorkPlacementStateV1::Quarantined);
    assert_eq!(second.authority_version(), 2, "the demotion is a revision");
    assert_eq!(
        second.blockers(),
        &BTreeSet::from([WorkPlacementBlockerV1::ActiveHolder])
    );
    assert_eq!(second.target().root(), Some(ROOT.as_str()));

    let third = load("actor.placement.third", "run.third");
    assert_eq!(third.state(), WorkPlacementStateV1::Quarantined);
    assert_eq!(
        third.authority_version(),
        2,
        "a quarantined duplicate keeps its own revision and blockers"
    );
    assert_eq!(
        third.blockers(),
        &BTreeSet::from([WorkPlacementBlockerV1::DirtyTrackedFiles])
    );

    assert_eq!(
        load("actor.placement.fourth", "run.released").state(),
        WorkPlacementStateV1::Released
    );
    assert_eq!(
        store.count("work_placements_v1"),
        5,
        "the migration deletes nothing"
    );

    // Only the oldest holder holds the root afterwards.
    let observer = authority("actor.placement.observer");
    assert!(held_by_other(&store, &observer, "run.second"));
    assert_eq!(
        publish(&store, &observer, None, &admitted("run.new", &ROOT))
            .expect_err("the kept holder still excludes newcomers"),
        WorkPlacementStorageError::AuthorityConflict
    );
    let holders: i64 = store.inspect(|connection| {
        connection
            .query_row(
                "SELECT COUNT(*) FROM work_placements_v1
                 WHERE target_root = ?1 AND state IN ('admitted', 'quarantined')",
                [ROOT.as_str()],
                |row| row.get(0),
            )
            .unwrap()
    });
    assert_eq!(holders, 1);

    // Installing the schema again on the migrated store changes nothing.
    store.inspect(|connection| install_work_schema(connection).unwrap());
    assert_eq!(
        store
            .storage()
            .load_placement(
                &authority("actor.placement.second"),
                &identity("run.second")
            )
            .unwrap(),
        Some(second)
    );
}
