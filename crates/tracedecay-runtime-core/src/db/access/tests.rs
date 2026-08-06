use super::*;

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
