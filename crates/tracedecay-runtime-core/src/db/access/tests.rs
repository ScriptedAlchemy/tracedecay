use super::*;

fn sqlite_fixture(path: &Path, marker: &str) -> u64 {
    let connection = rusqlite::Connection::open(path).unwrap();
    connection
        .execute_batch(&format!(
            "PRAGMA journal_mode=DELETE;
             CREATE TABLE marker (value TEXT NOT NULL);
             INSERT INTO marker VALUES ('{marker}');"
        ))
        .unwrap();
    drop(connection);
    crate::db::sqlite_generation_identity(path).unwrap()
}

fn sqlite_marker(path: &Path) -> String {
    rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .unwrap()
        .query_row("SELECT value FROM marker", (), |row| row.get(0))
        .unwrap()
}

#[test]
fn restored_sqlite_publication_retains_exact_rollback() {
    let temp = tempfile::tempdir().unwrap();
    let destination = temp.path().join("project.db");
    let staging = temp.path().join("restore.staging");
    let rollback = temp.path().join("restore.rollback");
    let destination_identity = sqlite_fixture(&destination, "old");
    let staging_identity = sqlite_fixture(&staging, "new");

    DatabaseAuthority::replace_sqlite_with_rollback_atomically(
        &staging,
        &destination,
        &rollback,
        destination_identity,
        staging_identity,
    )
    .unwrap();

    assert_eq!(sqlite_marker(&destination), "new");
    assert_eq!(sqlite_marker(&rollback), "old");
    assert_eq!(
        crate::db::sqlite_generation_identity(&destination).unwrap(),
        staging_identity
    );
    assert_eq!(
        crate::db::sqlite_generation_identity(&rollback).unwrap(),
        destination_identity
    );
    assert!(!staging.exists());
}

#[test]
fn restored_sqlite_publication_rejects_changed_destination_identity() {
    let temp = tempfile::tempdir().unwrap();
    let destination = temp.path().join("project.db");
    let staging = temp.path().join("restore.staging");
    let rollback = temp.path().join("restore.rollback");
    let stale_destination_identity = sqlite_fixture(&destination, "old");
    let staging_identity = sqlite_fixture(&staging, "new");
    let changed_destination = temp.path().join("changed.db");
    sqlite_fixture(&changed_destination, "changed");
    std::fs::rename(&changed_destination, &destination).unwrap();

    let error = DatabaseAuthority::replace_sqlite_with_rollback_atomically(
        &staging,
        &destination,
        &rollback,
        stale_destination_identity,
        staging_identity,
    )
    .unwrap_err();

    assert!(error.to_string().contains("identity changed"));
    assert_eq!(sqlite_marker(&destination), "changed");
    assert_eq!(sqlite_marker(&staging), "new");
    assert!(!rollback.exists());
}

#[test]
fn restored_sqlite_publication_never_overwrites_existing_rollback() {
    let temp = tempfile::tempdir().unwrap();
    let destination = temp.path().join("project.db");
    let staging = temp.path().join("restore.staging");
    let rollback = temp.path().join("restore.rollback");
    let destination_identity = sqlite_fixture(&destination, "old");
    let staging_identity = sqlite_fixture(&staging, "new");
    sqlite_fixture(&rollback, "retained");

    let error = DatabaseAuthority::replace_sqlite_with_rollback_atomically(
        &staging,
        &destination,
        &rollback,
        destination_identity,
        staging_identity,
    )
    .unwrap_err();

    assert!(error.to_string().contains("already exists"));
    assert_eq!(sqlite_marker(&destination), "old");
    assert_eq!(sqlite_marker(&staging), "new");
    assert_eq!(sqlite_marker(&rollback), "retained");
}

#[test]
fn symlink_aliases_share_one_database_identity() {
    let temp = tempfile::tempdir().unwrap();
    let database = temp.path().join("database.db");
    let alias = temp.path().join("database-alias.db");
    std::fs::write(&database, []).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&database, &alias).unwrap();
    #[cfg(windows)]
    std::os::windows::fs::symlink_file(&database, &alias).unwrap();
    #[cfg(not(any(unix, windows)))]
    std::fs::copy(&database, &alias).unwrap();

    let database = DatabaseIdentity::for_path(&database).unwrap();
    let alias = DatabaseIdentity::for_path(&alias).unwrap();
    #[cfg(any(unix, windows))]
    assert_eq!(database.database_key, alias.database_key);
}

#[test]
fn profile_project_databases_share_the_profile_scope() {
    let temp = tempfile::tempdir().unwrap();
    let profile = temp.path().join("profile");
    let first = profile.join("projects/first/graph.db");
    let second = profile.join("projects/second/branches/main.db");
    std::fs::create_dir_all(first.parent().unwrap()).unwrap();
    std::fs::create_dir_all(second.parent().unwrap()).unwrap();

    let first = DatabaseIdentity::for_path(&first).unwrap();
    let second = DatabaseIdentity::for_path(&second).unwrap();
    assert_eq!(
        first.profile_root,
        profile.canonicalize().unwrap(),
        "project stores must inherit the profile lifecycle fence"
    );
    assert_eq!(second.profile_root, first.profile_root);
}

#[test]
fn legacy_profile_store_databases_share_the_profile_scope() {
    let temp = tempfile::tempdir().unwrap();
    let profile = temp.path().join("profile");
    let first = profile.join("stores/first/graph.db");
    let second = profile.join("stores/second/branches/main.db");
    std::fs::create_dir_all(first.parent().unwrap()).unwrap();
    std::fs::create_dir_all(second.parent().unwrap()).unwrap();

    let first = DatabaseIdentity::for_path(&first).unwrap();
    let second = DatabaseIdentity::for_path(&second).unwrap();
    assert_eq!(
        first.profile_root,
        profile.canonicalize().unwrap(),
        "legacy profile stores must inherit the profile lifecycle fence"
    );
    assert_eq!(second.profile_root, first.profile_root);
}

#[test]
fn remote_node_databases_inherit_the_profile_scope() {
    let temp = tempfile::tempdir().unwrap();
    let profile = temp.path().join("profile");
    let remote = profile.join(format!("remote/nodes/{}/remote.db", "a".repeat(64)));
    std::fs::create_dir_all(remote.parent().unwrap()).unwrap();

    let identity = DatabaseIdentity::for_path(&remote).unwrap();
    assert_eq!(
        identity.profile_root,
        profile.canonicalize().unwrap(),
        "registered remote-node stores must inherit the profile lifecycle fence"
    );
}

#[test]
fn authority_clones_share_one_token_without_database_sidecars() {
    let temp = tempfile::tempdir().unwrap();
    let database = temp.path().join("database.db");
    let authority = DatabaseAuthority::acquire_test(&database, "sidecar-free authority").unwrap();
    let joined = DatabaseAuthority::acquire_test(&database, "joined authority").unwrap();

    assert_eq!(authority.token(), joined.token());
    assert!(matches!(
        probe_writer_owner(&database).unwrap(),
        WriterOwnership::Active(owner) if owner.token == authority.token()
    ));
    assert!(
        !temp.path().join(".tracedecay-database-locks").exists(),
        "database authority must not create per-database sidecars"
    );
    drop(joined);
    drop(authority);
    assert_eq!(
        probe_writer_owner(&database).unwrap(),
        WriterOwnership::Idle
    );
}

#[test]
fn concurrent_authority_clients_share_one_token_without_busy_or_locked() {
    for clients in [1_usize, 8, 32, 64] {
        let temp = tempfile::tempdir().unwrap();
        let database = temp.path().join("database.db");
        let barrier = Arc::new(std::sync::Barrier::new(clients + 1));
        let mut threads = Vec::with_capacity(clients);
        for index in 0..clients {
            let database = database.clone();
            let barrier = Arc::clone(&barrier);
            threads.push(std::thread::spawn(move || {
                barrier.wait();
                DatabaseAuthority::acquire_test(
                    &database,
                    &format!("concurrent authority client {index}"),
                )
            }));
        }
        barrier.wait();
        let authorities = threads
            .into_iter()
            .map(|thread| thread.join().unwrap().unwrap())
            .collect::<Vec<_>>();
        let token = authorities[0].token();
        assert!(
            authorities
                .iter()
                .all(|authority| authority.token() == token)
        );
        assert!(
            !temp.path().join(".tracedecay-database-locks").exists(),
            "database authority must not create per-database sidecars"
        );
        eprintln!(
            "writer_authority clients={clients} runtimes=1 tokens=1 busy=0 locked=0 sidecars=0"
        );
    }
}

#[test]
fn incompatible_process_authority_roles_fail_closed() {
    let temp = tempfile::tempdir().unwrap();
    let database = temp.path().join("database.db");
    let _test = DatabaseAuthority::acquire_test(&database, "test authority").unwrap();
    let error =
        DatabaseAuthority::acquire_maintenance(&database, "maintenance authority").unwrap_err();
    assert!(
        error
            .to_string()
            .contains("incompatible database authority")
    );
}

#[test]
fn writer_owner_intent_is_bounded_and_single_line() {
    let intent = format!("{}\n{}", "x".repeat(300), "y".repeat(300));
    let owner = writer_owner("token", &intent);
    assert!(!owner.intent.contains('\n'));
    assert!(owner.intent.len() <= 256);
}

#[test]
fn fallback_scope_is_unambiguous_only_with_one_profile_owner() {
    assert_eq!(fallback_scoped_runtime_role(0, 0).unwrap(), None);
    assert_eq!(
        fallback_scoped_runtime_role(1, 0).unwrap(),
        Some(DatabaseAuthorityRole::Maintenance)
    );
    assert_eq!(
        fallback_scoped_runtime_role(0, 1).unwrap(),
        Some(DatabaseAuthorityRole::Daemon)
    );
    assert!(fallback_scoped_runtime_role(1, 1).is_err());
    assert!(fallback_scoped_runtime_role(2, 0).is_err());
}

/// Reproduces the macOS/Windows shape of the isolated-test-path check on any
/// host: the fixture path is spelled through a symlinked root while the root
/// itself resolves elsewhere.
///
/// macOS `temp_dir()` reports `/var/folders/.../T` and resolves to
/// `/private/var/folders/.../T`; Windows reports the `RUNNER~1` short name and
/// resolves to the `\\?\` verbatim long form. Resolving only the root compared
/// the two sides in different spellings and reported a fixture inside the
/// temporary directory as being outside it.
#[cfg(unix)]
#[test]
fn a_root_reached_through_a_symlink_still_contains_its_fixtures() {
    let temporary = tempfile::tempdir().unwrap();
    let resolved = temporary.path().join("resolved");
    std::fs::create_dir(&resolved).unwrap();
    let reported = temporary.path().join("reported");
    std::os::unix::fs::symlink(&resolved, &reported).unwrap();

    // The root as `temp_dir()` reports it, and the fixture as `tempfile`
    // hands it back: both in the unresolved spelling.
    assert!(
        under_isolated_root(&reported.join("fixture.db"), reported.clone()),
        "a fixture under the reported root must be recognized as isolated"
    );
    // The mixed spellings the two APIs actually produce, in both directions.
    assert!(under_isolated_root(
        &reported.join("fixture.db"),
        resolved.clone()
    ));
    assert!(under_isolated_root(&resolved.join("fixture.db"), reported));

    // A path genuinely outside the root is still refused.
    let outside = tempfile::tempdir().unwrap();
    assert!(!under_isolated_root(
        &outside.path().join("fixture.db"),
        resolved
    ));
}
