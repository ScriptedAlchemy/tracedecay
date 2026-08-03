use super::*;

static SCOPE_TEST_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn canonical_identity_collapses_parent_aliases() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(temp.path().join("nested")).unwrap();
    let direct = DatabaseIdentity::for_path(&temp.path().join("graph.db")).unwrap();
    let aliased = DatabaseIdentity::for_path(&temp.path().join("nested/../graph.db")).unwrap();
    assert_eq!(direct, aliased);
}

#[test]
fn identity_key_preserves_unproven_case_variants() {
    let temp = tempfile::tempdir().unwrap();
    let upper = temp.path().join("MixedCase.DB");
    let lower = temp.path().join("mixedcase.db");

    assert_ne!(platform_identity_key(&upper), platform_identity_key(&lower));
}

#[cfg(target_os = "linux")]
#[test]
fn case_distinct_database_files_have_distinct_identities() {
    let temp = tempfile::tempdir().unwrap();
    let upper = temp.path().join("MixedCase.DB");
    let lower = temp.path().join("mixedcase.db");
    std::fs::write(&upper, []).unwrap();
    std::fs::write(&lower, []).unwrap();

    let upper = DatabaseIdentity::for_path(&upper).unwrap();
    let lower = DatabaseIdentity::for_path(&lower).unwrap();

    assert_ne!(upper.database_key, lower.database_key);
    assert_ne!(upper.writer_lock_path, lower.writer_lock_path);

    let upper_authority = DatabaseAuthority::acquire_test(
        &temp.path().join("MixedCase.DB"),
        "upper case-sensitive database",
    )
    .unwrap();
    let lower_authority = DatabaseAuthority::acquire_test(
        &temp.path().join("mixedcase.db"),
        "lower case-sensitive database",
    )
    .unwrap();
    assert_ne!(upper_authority.token(), lower_authority.token());
}

#[cfg(any(windows, target_os = "macos"))]
#[test]
fn fresh_case_variants_cannot_hold_concurrent_first_create_authorities() {
    let temp = tempfile::tempdir().unwrap();
    let upper = temp.path().join("MixedCase.DB");
    let lower = temp.path().join("mixedcase.db");

    let first = DatabaseAuthority::acquire_test(&upper, "first case variant").unwrap();
    let error = DatabaseAuthority::acquire_test(&lower, "second case variant").unwrap_err();
    assert!(error.to_string().contains("case-variant first-create"));

    std::fs::write(&upper, []).unwrap();
    drop(first);
    let second = DatabaseAuthority::acquire_test(&lower, "second case variant").unwrap();
    if lower.exists() {
        assert_eq!(
            second.canonical_database_path(),
            upper.canonicalize().unwrap()
        );
    } else {
        std::fs::write(&lower, []).unwrap();
        let upper_identity = DatabaseIdentity::for_path(&upper).unwrap();
        let lower_identity = DatabaseIdentity::for_path(&lower).unwrap();
        assert_ne!(upper_identity.database_key, lower_identity.database_key);
    }
}

#[cfg(unix)]
#[test]
fn symlink_aliases_share_one_database_identity() {
    let temp = tempfile::tempdir().unwrap();
    let database = temp.path().join("database.db");
    let alias = temp.path().join("database-alias.db");
    std::fs::write(&database, []).unwrap();
    std::os::unix::fs::symlink(&database, &alias).unwrap();

    let database = DatabaseIdentity::for_path(&database).unwrap();
    let alias = DatabaseIdentity::for_path(&alias).unwrap();

    assert_eq!(database.database_key, alias.database_key);
    assert_eq!(database.writer_lock_path, alias.writer_lock_path);
}

#[test]
fn profile_databases_share_one_exact_profile_scope() {
    let temp = tempfile::tempdir().unwrap();
    let profile = temp.path().join("profile");
    std::fs::create_dir_all(&profile).unwrap();
    let expected_profile = platform_identity_key(&profile.canonicalize().unwrap());
    let expected_lock_parent = expected_profile.join(".tracedecay-database-locks");
    let paths = [
        profile.join("global.db"),
        profile.join("user-memory.db"),
        profile.join("user-sessions.db"),
        profile.join("projects/project/tracedecay.db"),
        profile.join("projects/project/sessions.db"),
        profile.join("projects/project/branches/feature.db"),
    ];

    for path in paths {
        let identity = DatabaseIdentity::for_path(&path).unwrap();
        assert_eq!(
            identity.profile_root,
            expected_profile,
            "{}",
            path.display()
        );
        assert!(
            !identity.allows_ambient_profile_scope,
            "{} must require its exact profile authority",
            path.display()
        );
        assert_eq!(
            identity.access_lock_path.parent(),
            Some(expected_lock_parent.as_path())
        );
    }
}

#[test]
fn projects_directory_in_repository_path_is_not_a_profile_shard() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("projects/repository/.tracedecay");
    let path = data_root.join("tracedecay.db");
    let identity = DatabaseIdentity::for_path(&path).unwrap();

    assert_eq!(
        identity.profile_root,
        platform_identity_key(&data_root.canonicalize().unwrap())
    );
    assert!(identity.allows_ambient_profile_scope);
}

#[test]
fn fs2_contention_is_classified_as_an_active_lease() {
    let temp = tempfile::tempdir().unwrap();
    let lock_path = temp.path().join("authority.lock");
    let first = open_lock_file(&lock_path).unwrap();
    let second = open_lock_file(&lock_path).unwrap();
    fs2::FileExt::try_lock_exclusive(&first).unwrap();

    let error = fs2::FileExt::try_lock_exclusive(&second).unwrap_err();

    assert!(is_lock_contended(&error), "unexpected lock error: {error}");
    fs2::FileExt::unlock(&first).unwrap();
}

#[test]
fn writer_owner_replacement_is_complete_and_leaves_no_temporary_file() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("writer.owner");
    let first = writer_owner("first", "first owner");
    let second = writer_owner("second", "replacement owner");
    write_owner(&path, &first).unwrap();

    write_owner(&path, &second).unwrap();

    assert_eq!(read_owner(&path), Some(second));
    assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 1);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

#[test]
fn atomic_record_publication_preserves_a_colliding_temporary_file() {
    let temp = tempfile::tempdir().unwrap();
    let destination = temp.path().join("authority.record");
    let temporary = temp.path().join("authority.record.tmp");
    std::fs::write(&temporary, b"other publisher").unwrap();

    let error = DatabaseAuthority::publish_record_atomically(
        &temporary,
        &destination,
        b"replacement",
        "test authority record",
    )
    .unwrap_err();

    assert!(error.to_string().contains("create test authority record"));
    assert_eq!(std::fs::read(&temporary).unwrap(), b"other publisher");
    assert!(!destination.exists());
}

#[test]
fn daemon_authority_is_same_process_reentrant() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("graph.db");
    let first = DatabaseAuthority::acquire_test(&path, "first").unwrap();
    let second = DatabaseAuthority::acquire_test(&path, "second").unwrap();
    assert_eq!(first.token(), second.token());
    assert_eq!(
        probe_writer_owner(&path).unwrap(),
        WriterOwnership::Active(
            read_owner(&DatabaseIdentity::for_path(&path).unwrap().writer_owner_path).unwrap()
        )
    );
    drop(first);
    assert!(matches!(
        probe_writer_owner(&path).unwrap(),
        WriterOwnership::Active(_)
    ));
    drop(second);
    assert_eq!(probe_writer_owner(&path).unwrap(), WriterOwnership::Idle);
}

#[test]
fn maintenance_and_daemon_authorities_are_mutually_exclusive() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("graph.db");
    let daemon = DatabaseAuthority::acquire_test(&path, "daemon").unwrap();
    let error = DatabaseAuthority::acquire_maintenance(&path, "replace").unwrap_err();
    assert!(
        error
            .to_string()
            .contains("incompatible database authority")
    );
    drop(daemon);

    let maintenance = DatabaseAuthority::acquire_maintenance(&path, "replace").unwrap();
    let error = DatabaseAuthority::acquire_test(&path, "daemon").unwrap_err();
    assert!(
        error
            .to_string()
            .contains("incompatible database authority")
    );
    drop(maintenance);
}

#[test]
fn deletion_fence_is_ordered_and_same_process_exclusive() {
    let temp = tempfile::tempdir().unwrap();
    let first = temp.path().join("a.db");
    let second = temp.path().join("b.db");
    let ordinary = DatabaseAuthority::acquire_test(&first, "ordinary holder").unwrap();

    let error =
        DatabaseDeletionFence::acquire(&[second.clone(), first.clone()], "delete databases")
            .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("incompatible database authority")
    );
    drop(ordinary);

    let fence = DatabaseDeletionFence::acquire(
        &[second.clone(), first.clone(), second.clone()],
        "delete databases",
    )
    .unwrap();
    let canonical_temp = temp.path().canonicalize().unwrap();
    assert_eq!(
        fence
            .database_paths()
            .map(Path::to_path_buf)
            .collect::<Vec<_>>(),
        vec![canonical_temp.join("a.db"), canonical_temp.join("b.db")]
    );
    assert!(fence.transaction_id().contains(':'));
    assert_eq!(fence.tombstone_paths().count(), 2);

    let error = DatabaseAuthority::acquire_test(&first, "ordinary overlap").unwrap_err();
    assert!(error.to_string().contains("database deletion fence"));
    let error = DatabaseDeletionFence::acquire(&[second], "second deletion").unwrap_err();
    assert!(error.to_string().contains("deletion fence"));

    drop(fence);
    DatabaseAuthority::acquire_test(&first, "ordinary after fence").unwrap();
}

#[test]
fn deleting_tombstones_rollback_only_while_the_fence_is_retained() {
    let temp = tempfile::tempdir().unwrap();
    let first = temp.path().join("a.db");
    let second = temp.path().join("b.db");
    let fence =
        DatabaseDeletionFence::acquire(&[second.clone(), first.clone()], "delete databases")
            .unwrap();

    fence.publish_deleting().unwrap();
    for path in fence.tombstone_paths() {
        let record = read_record_strict(path, "test tombstone").unwrap().unwrap();
        assert!(record.contains("state=deleting"));
        assert!(record.contains(fence.transaction_id()));
    }
    let error = DatabaseAuthority::acquire_test(&first, "open while deleting").unwrap_err();
    assert!(error.to_string().contains("database deletion fence"));

    fence.rollback_deleting().unwrap();
    assert!(fence.tombstone_paths().all(|path| !path.exists()));
    drop(fence);
    DatabaseAuthority::acquire_test(&first, "open after rollback").unwrap();
}

#[test]
fn drop_retains_deleting_tombstone_and_ordinary_authority_fails_closed() {
    let temp = tempfile::tempdir().unwrap();
    let database = temp.path().join("graph.db");
    let fence =
        DatabaseDeletionFence::acquire(std::slice::from_ref(&database), "delete database").unwrap();
    let tombstone = fence.tombstone_paths().next().unwrap().to_path_buf();

    fence.publish_deleting().unwrap();
    drop(fence);

    assert!(tombstone.exists());
    let error = DatabaseAuthority::acquire_test(&database, "open deleted database").unwrap_err();
    assert!(error.to_string().contains("deletion is in progress"));
    remove_record_durably(&tombstone, "test tombstone cleanup").unwrap();
    DatabaseAuthority::acquire_test(&database, "open after cleanup").unwrap();
}

#[test]
fn rollback_never_removes_another_transactions_tombstone() {
    let temp = tempfile::tempdir().unwrap();
    let database = temp.path().join("graph.db");
    let identity = DatabaseIdentity::for_path(&database).unwrap();
    let fence =
        DatabaseDeletionFence::acquire(std::slice::from_ref(&database), "delete database").unwrap();
    fence.publish_deleting().unwrap();

    let foreign = format!(
        "version=1\tstate=deleting\ttransaction_id=foreign\tdatabase_id={:016x}\n",
        identity.database_id
    );
    write_record_atomically(
        &identity.deletion_tombstone_path,
        foreign.as_bytes(),
        "foreign test tombstone",
    )
    .unwrap();
    let error = fence.rollback_deleting().unwrap_err();
    assert!(error.to_string().contains("belongs to transaction foreign"));
    assert!(identity.deletion_tombstone_path.exists());

    let own = format!(
        "version=1\tstate=deleting\ttransaction_id={}\tdatabase_id={:016x}\n",
        fence.transaction_id(),
        identity.database_id
    );
    write_record_atomically(
        &identity.deletion_tombstone_path,
        own.as_bytes(),
        "own test tombstone",
    )
    .unwrap();
    fence.rollback_deleting().unwrap();
}

#[test]
fn same_transaction_deleting_tombstone_can_be_reacquired_with_locks_retained() {
    let temp = tempfile::tempdir().unwrap();
    let database = temp.path().join("graph.db");
    let fence =
        DatabaseDeletionFence::acquire(std::slice::from_ref(&database), "delete database").unwrap();
    fence.publish_deleting().unwrap();
    let transaction_id = fence.transaction_id().to_string();
    drop(fence);

    let (reacquired, states) = DatabaseDeletionFence::reacquire(
        std::slice::from_ref(&database),
        &transaction_id,
        "recover deletion",
    )
    .unwrap();
    assert_eq!(states.missing(), 0);
    assert_eq!(states.deleting(), 1);
    assert_eq!(states.deleted(), 0);
    assert_eq!(reacquired.tombstone_states().unwrap(), states);
    let error = DatabaseAuthority::acquire_test(&database, "overlap recovery").unwrap_err();
    assert!(error.to_string().contains("database deletion fence"));

    reacquired.rollback_deleting().unwrap();
    drop(reacquired);
    DatabaseAuthority::acquire_test(&database, "open after recovery rollback").unwrap();
}

#[test]
fn same_transaction_deleted_tombstone_can_be_reacquired_idempotently() {
    let temp = tempfile::tempdir().unwrap();
    let database = temp.path().join("graph.db");
    let identity = DatabaseIdentity::for_path(&database).unwrap();
    let fence =
        DatabaseDeletionFence::acquire(std::slice::from_ref(&database), "delete database").unwrap();
    fence.publish_deleting().unwrap();
    fence.promote_deleted().unwrap();
    let transaction_id = fence.transaction_id().to_string();
    drop(fence);

    let (reacquired, states) = DatabaseDeletionFence::reacquire(
        std::slice::from_ref(&database),
        &transaction_id,
        "recover committed deletion",
    )
    .unwrap();
    assert_eq!(states.deleted(), 1);
    reacquired.promote_deleted().unwrap();
    assert_eq!(reacquired.tombstone_states().unwrap().deleted(), 1);
    assert!(reacquired.rollback_deleting().is_err());
    drop(reacquired);
    remove_record_durably(&identity.deletion_tombstone_path, "test tombstone cleanup").unwrap();
}

#[test]
fn deletion_reacquire_rejects_foreign_transaction() {
    let temp = tempfile::tempdir().unwrap();
    let database = temp.path().join("graph.db");
    let identity = DatabaseIdentity::for_path(&database).unwrap();
    let fence =
        DatabaseDeletionFence::acquire(std::slice::from_ref(&database), "delete database").unwrap();
    fence.publish_deleting().unwrap();
    drop(fence);

    let error = DatabaseDeletionFence::reacquire(
        std::slice::from_ref(&database),
        "foreign-transaction",
        "recover foreign deletion",
    )
    .unwrap_err();
    assert!(error.to_string().contains("transaction ID is invalid"));
    remove_record_durably(&identity.deletion_tombstone_path, "test tombstone cleanup").unwrap();
}

#[test]
fn deletion_reacquire_rejects_transaction_for_another_path_set() {
    let temp = tempfile::tempdir().unwrap();
    let first = temp.path().join("first.db");
    let second = temp.path().join("second.db");
    let identity = DatabaseIdentity::for_path(&first).unwrap();
    let fence =
        DatabaseDeletionFence::acquire(std::slice::from_ref(&first), "delete database").unwrap();
    fence.publish_deleting().unwrap();
    let transaction_id = fence.transaction_id().to_string();
    drop(fence);

    let error = DatabaseDeletionFence::reacquire(
        std::slice::from_ref(&second),
        &transaction_id,
        "recover wrong database",
    )
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("does not match the database path set")
    );
    remove_record_durably(&identity.deletion_tombstone_path, "test tombstone cleanup").unwrap();
}

#[cfg(any(windows, target_os = "macos"))]
#[test]
fn deletion_fence_collapses_and_locks_missing_case_variants() {
    let temp = tempfile::tempdir().unwrap();
    let upper = temp.path().join("MixedCase.DB");
    let lower = temp.path().join("mixedcase.db");

    let fence =
        DatabaseDeletionFence::acquire(&[upper.clone(), lower.clone()], "delete missing database")
            .unwrap();
    assert_eq!(fence.database_paths().count(), 1);
    let error = DatabaseAuthority::acquire_test(&lower, "create case variant").unwrap_err();
    assert!(error.to_string().contains("case-variant first-create"));

    drop(fence);
    DatabaseAuthority::acquire_test(&lower, "create after deletion fence").unwrap();
}

#[cfg(any(windows, target_os = "macos"))]
#[test]
fn deletion_fence_retains_first_create_lock_after_database_removal() {
    let temp = tempfile::tempdir().unwrap();
    let upper = temp.path().join("MixedCase.DB");
    let lower = temp.path().join("mixedcase.db");
    std::fs::write(&upper, []).unwrap();

    let fence =
        DatabaseDeletionFence::acquire(std::slice::from_ref(&upper), "delete existing database")
            .unwrap();
    std::fs::remove_file(&upper).unwrap();
    let error = DatabaseAuthority::acquire_test(&lower, "recreate case variant").unwrap_err();
    assert!(error.to_string().contains("case-variant first-create"));

    drop(fence);
    DatabaseAuthority::acquire_test(&lower, "recreate after deletion fence").unwrap();
}

#[test]
fn partial_publication_reacquires_missing_and_deleting_for_rollback() {
    let temp = tempfile::tempdir().unwrap();
    let first = temp.path().join("a.db");
    let second = temp.path().join("b.db");
    let fence =
        DatabaseDeletionFence::acquire(&[first.clone(), second.clone()], "delete databases")
            .unwrap();
    fence.publish_deleting().unwrap();
    let transaction_id = fence.transaction_id().to_string();
    let first_tombstone = fence.tombstone_paths().next().unwrap().to_path_buf();
    remove_record_durably(&first_tombstone, "simulate partial publication").unwrap();
    drop(fence);

    let (reacquired, states) = DatabaseDeletionFence::reacquire(
        &[second.clone(), first.clone()],
        &transaction_id,
        "recover partial publication",
    )
    .unwrap();
    assert_eq!(states.missing(), 1);
    assert_eq!(states.deleting(), 1);
    assert!(states.has_missing());
    assert!(states.has_deleting());
    assert!(!states.has_deleted());
    reacquired.rollback_deleting().unwrap();
    drop(reacquired);
    DatabaseAuthority::acquire_test(&first, "open first after rollback").unwrap();
    DatabaseAuthority::acquire_test(&second, "open second after rollback").unwrap();
}

#[test]
fn partial_promotion_reacquires_deleting_and_deleted_for_completion() {
    let temp = tempfile::tempdir().unwrap();
    let first = temp.path().join("a.db");
    let second = temp.path().join("b.db");
    let first_identity = DatabaseIdentity::for_path(&first).unwrap();
    let second_identity = DatabaseIdentity::for_path(&second).unwrap();
    let fence =
        DatabaseDeletionFence::acquire(&[first.clone(), second.clone()], "delete databases")
            .unwrap();
    fence.publish_deleting().unwrap();
    fence.promote_deleted().unwrap();
    let transaction_id = fence.transaction_id().to_string();
    let partial = format!(
        "version=1\tstate=deleting\ttransaction_id={}\tdatabase_id={:016x}\n",
        transaction_id, first_identity.database_id
    );
    write_record_atomically(
        &first_identity.deletion_tombstone_path,
        partial.as_bytes(),
        "simulate partial promotion",
    )
    .unwrap();
    drop(fence);

    let (reacquired, states) = DatabaseDeletionFence::reacquire(
        &[second.clone(), first.clone()],
        &transaction_id,
        "recover partial promotion",
    )
    .unwrap();
    assert_eq!(states.deleting(), 1);
    assert_eq!(states.deleted(), 1);
    assert!(!states.has_missing());
    assert!(states.has_deleting());
    assert!(states.has_deleted());
    reacquired.promote_deleted().unwrap();
    let completed = reacquired.tombstone_states().unwrap();
    assert_eq!(completed.deleting(), 0);
    assert_eq!(completed.deleted(), 2);
    drop(reacquired);
    remove_record_durably(
        &first_identity.deletion_tombstone_path,
        "test tombstone cleanup",
    )
    .unwrap();
    remove_record_durably(
        &second_identity.deletion_tombstone_path,
        "test tombstone cleanup",
    )
    .unwrap();
}

#[test]
fn promoted_tombstone_is_committed_and_cannot_be_rolled_back() {
    let temp = tempfile::tempdir().unwrap();
    let database = temp.path().join("graph.db");
    let fence =
        DatabaseDeletionFence::acquire(std::slice::from_ref(&database), "delete database").unwrap();
    let tombstone = fence.tombstone_paths().next().unwrap().to_path_buf();

    fence.publish_deleting().unwrap();
    fence.promote_deleted().unwrap();
    let error = fence.rollback_deleting().unwrap_err();
    assert!(error.to_string().contains("already deleted"));
    drop(fence);

    assert!(tombstone.exists());
    let error = DatabaseAuthority::acquire_test(&database, "open deleted database").unwrap_err();
    assert!(error.to_string().contains("database was deleted"));
    remove_record_durably(&tombstone, "test tombstone cleanup").unwrap();
}

#[test]
fn retired_path_query_distinguishes_missing_deleting_and_deleted() {
    let temp = tempfile::tempdir().unwrap();
    let database = temp.path().join("graph.db");
    let identity = DatabaseIdentity::for_path(&database).unwrap();
    assert!(!database_path_is_tombstoned(&database).unwrap());

    let fence =
        DatabaseDeletionFence::acquire(std::slice::from_ref(&database), "delete database").unwrap();
    fence.publish_deleting().unwrap();
    assert!(database_path_is_tombstoned(&database).unwrap());
    fence.promote_deleted().unwrap();
    assert!(database_path_is_tombstoned(&database).unwrap());
    drop(fence);

    remove_record_durably(&identity.deletion_tombstone_path, "test tombstone cleanup").unwrap();
    assert!(!database_path_is_tombstoned(&database).unwrap());
}

#[test]
fn corrupt_and_identity_mismatched_tombstones_fail_closed() {
    let temp = tempfile::tempdir().unwrap();
    let database = temp.path().join("graph.db");
    let identity = DatabaseIdentity::for_path(&database).unwrap();

    write_record_atomically(
        &identity.deletion_tombstone_path,
        b"not-a-tombstone\n",
        "test corrupt tombstone",
    )
    .unwrap();
    let error = database_path_is_tombstoned(&database).unwrap_err();
    assert!(error.to_string().contains("tombstone is corrupt"));
    let error = DatabaseAuthority::acquire_test(&database, "open corrupt marker").unwrap_err();
    assert!(error.to_string().contains("tombstone is corrupt"));

    let payload = format!(
        "version=1\tstate=deleting\ttransaction_id=test\tdatabase_id={:016x}\n",
        identity.database_id.wrapping_add(1)
    );
    write_record_atomically(
        &identity.deletion_tombstone_path,
        payload.as_bytes(),
        "test mismatched tombstone",
    )
    .unwrap();
    let error = database_path_is_tombstoned(&database).unwrap_err();
    assert!(error.to_string().contains("identity does not match"));
    let error = DatabaseAuthority::acquire_test(&database, "open mismatched marker").unwrap_err();
    assert!(error.to_string().contains("identity does not match"));

    remove_record_durably(&identity.deletion_tombstone_path, "test tombstone cleanup").unwrap();
    std::fs::create_dir(&identity.deletion_tombstone_path).unwrap();
    let error = database_path_is_tombstoned(&database).unwrap_err();
    assert!(error.to_string().contains("not a regular file"));
    let error = DatabaseAuthority::acquire_test(&database, "open unreadable marker").unwrap_err();
    assert!(error.to_string().contains("not a regular file"));
    std::fs::remove_dir(&identity.deletion_tombstone_path).unwrap();

    DatabaseAuthority::acquire_test(&database, "open after marker cleanup").unwrap();
}

#[cfg(unix)]
#[test]
fn symlinked_tombstone_fails_closed() {
    let temp = tempfile::tempdir().unwrap();
    let database = temp.path().join("graph.db");
    let identity = DatabaseIdentity::for_path(&database).unwrap();
    let target = temp.path().join("other.tombstone");
    std::fs::write(
        &target,
        format!(
            "version=1\tstate=deleting\ttransaction_id=test\tdatabase_id={:016x}\n",
            identity.database_id
        ),
    )
    .unwrap();
    std::os::unix::fs::symlink(&target, &identity.deletion_tombstone_path).unwrap();

    let error = database_path_is_tombstoned(&database).unwrap_err();
    assert!(error.to_string().contains("must not be a symlink"));
    let error = DatabaseAuthority::acquire_test(&database, "open symlink marker").unwrap_err();
    assert!(error.to_string().contains("must not be a symlink"));
    std::fs::remove_file(&identity.deletion_tombstone_path).unwrap();
    DatabaseAuthority::acquire_test(&database, "open after symlink cleanup").unwrap();
}

#[test]
fn stale_owner_metadata_never_establishes_ownership() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("graph.db");
    let identity = DatabaseIdentity::for_path(&path).unwrap();
    std::fs::write(
        &identity.writer_owner_path,
        "token=stale\tpid=1\tstarted_epoch_ms=1\tversion=old\tintent=old\n",
    )
    .unwrap();
    assert_eq!(probe_writer_owner(&path).unwrap(), WriterOwnership::Idle);
    assert!(identity.writer_owner_path.exists());
}

#[test]
fn authority_is_bound_to_one_canonical_database() {
    let temp = tempfile::tempdir().unwrap();
    let first = temp.path().join("first.db");
    let second = temp.path().join("second.db");
    let authority = DatabaseAuthority::acquire_test(&first, "test").unwrap();
    let error = authority.hold_for(&second, "open").unwrap_err();
    assert!(error.to_string().contains("different database"));
}

#[test]
fn daemon_authority_inherits_live_election_scope() {
    let _lock = SCOPE_TEST_LOCK.lock().unwrap();
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("graph.db");
    let scope = enter_daemon_database_scope(temp.path(), 7, "election-token").unwrap();
    let authority = DatabaseAuthority::acquire_daemon(&path, "daemon").unwrap();
    assert_eq!(authority.role(), DatabaseAuthorityRole::Daemon);
    drop(authority);
    drop(scope);
}

#[test]
fn sole_daemon_scope_authorizes_only_legacy_repo_local_database() {
    let _lock = SCOPE_TEST_LOCK.lock().unwrap();
    let profile = tempfile::tempdir().unwrap();
    let repository = tempfile::tempdir().unwrap();
    let scope = enter_daemon_database_scope(profile.path(), 1, "daemon").unwrap();
    let identity =
        DatabaseIdentity::for_path(&repository.path().join(".tracedecay/tracedecay.db")).unwrap();

    assert!(identity.allows_ambient_profile_scope);
    assert_eq!(
        scoped_runtime_role(&identity, "legacy repository database").unwrap(),
        Some(DatabaseAuthorityRole::Daemon)
    );

    drop(scope);
}

#[test]
fn sole_daemon_scope_rejects_standard_databases_from_another_profile() {
    let _lock = SCOPE_TEST_LOCK.lock().unwrap();
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    let scope = enter_daemon_database_scope(first.path(), 1, "first").unwrap();
    let paths = [
        second.path().join("global.db"),
        second.path().join("user-memory.db"),
        second.path().join("user-sessions.db"),
        second.path().join("projects/project/tracedecay.db"),
        second.path().join("projects/project/sessions.db"),
        second.path().join("projects/project/branches/feature.db"),
    ];

    for path in paths {
        let identity = DatabaseIdentity::for_path(&path).unwrap();
        assert_eq!(
            exact_scoped_runtime_role(&identity.profile_root, "other profile").unwrap(),
            None
        );
        assert_eq!(
            scoped_runtime_role(&identity, "other profile").unwrap(),
            None,
            "{} used an unrelated ambient profile scope",
            path.display()
        );
    }

    drop(scope);
}

#[test]
fn maintenance_scope_requires_and_inherits_exclusive_profile_lease() {
    let _lock = SCOPE_TEST_LOCK.lock().unwrap();
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("projects/p1/tracedecay.db");
    let lifecycle =
        crate::lifecycle_lease::acquire_exclusive_for_profile(temp.path(), "maintenance test")
            .unwrap();
    let scope =
        enter_maintenance_database_scope(&lifecycle, temp.path(), "maintenance test").unwrap();
    let authority = DatabaseAuthority::for_runtime(&path, "repair").unwrap();
    assert_eq!(authority.role(), DatabaseAuthorityRole::Maintenance);
    drop(authority);
    drop(scope);
    drop(lifecycle);
}

#[test]
fn daemon_scopes_are_isolated_by_profile() {
    let _lock = SCOPE_TEST_LOCK.lock().unwrap();
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    let first_scope = enter_daemon_database_scope(first.path(), 1, "first").unwrap();
    let second_scope = enter_daemon_database_scope(second.path(), 1, "second").unwrap();

    let first_authority = DatabaseAuthority::for_runtime(
        &first.path().join("projects/one/tracedecay.db"),
        "first profile",
    )
    .unwrap();
    let second_authority = DatabaseAuthority::for_runtime(
        &second.path().join("projects/two/tracedecay.db"),
        "second profile",
    )
    .unwrap();
    assert_eq!(first_authority.role(), DatabaseAuthorityRole::Daemon);
    assert_eq!(second_authority.role(), DatabaseAuthorityRole::Daemon);

    drop((first_authority, second_authority, first_scope, second_scope));
}

#[test]
fn maintenance_scope_is_reentrant_across_nested_intents() {
    let _lock = SCOPE_TEST_LOCK.lock().unwrap();
    let temp = tempfile::tempdir().unwrap();
    let lifecycle =
        crate::lifecycle_lease::acquire_exclusive_for_profile(temp.path(), "outer").unwrap();
    let outer = enter_maintenance_database_scope(&lifecycle, temp.path(), "plan").unwrap();
    let inner = enter_maintenance_database_scope(&lifecycle, temp.path(), "apply").unwrap();
    let authority = DatabaseAuthority::for_runtime(
        &temp.path().join("projects/p1/tracedecay.db"),
        "nested operation",
    )
    .unwrap();
    assert_eq!(authority.role(), DatabaseAuthorityRole::Maintenance);

    drop(authority);
    drop(inner);
    drop(outer);
    drop(lifecycle);
}
