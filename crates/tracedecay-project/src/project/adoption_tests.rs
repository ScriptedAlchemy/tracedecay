use tempfile::TempDir;

/// Enrolls a non-git project store on disk: profile shard + manifest + graph
/// bytes, and no `.git/` marker. Identity lives in the registry and the
/// store's recorded root.
#[cfg(unix)]
fn enroll_nongit_project_on_disk(
    project_root: &std::path::Path,
    profile_root: &std::path::Path,
    project_id: &str,
) -> tracedecay_runtime_core::storage::StoreLayout {
    let layout = tracedecay_runtime_core::storage::profile_sharded_layout(
        project_root,
        profile_root,
        project_id,
    )
    .expect("nongit layout");
    std::fs::create_dir_all(&layout.data_root).expect("profile store root");
    tracedecay_runtime_core::storage::write_store_manifest(&layout).expect("store manifest");
    let sessions = rusqlite::Connection::open(&layout.sessions_db_path).expect("sessions database");
    sessions
        .execute_batch("PRAGMA user_version = 1;")
        .expect("initialize sessions database");
    std::fs::write(
        &layout.graph_db_path,
        format!("nongit-graph-{project_id}").as_bytes(),
    )
    .expect("graph store");
    layout
}

#[cfg(unix)]
fn moved_nongit_open_options(profile_root: &std::path::Path) -> super::TraceDecayOpenOptions {
    super::TraceDecayOpenOptions {
        profile_root: Some(profile_root.to_path_buf()),
        global_db_path: None,
    }
}

/// After the working-tree enrollment file was removed, a moved non-git
/// project cannot be found by path-derived id. Rebinding its registry row is
/// an operator decision: ambient first-touch mints fresh, explicit init
/// without flags refuses with the candidate, and `--yes` (`AdoptUnique`)
/// completes the remap with store data intact.
#[cfg(unix)]
#[asupersync::test]
async fn moved_nongit_project_is_readopted_only_when_confirmed() {
    let home = TempDir::new().expect("isolated home");
    let root = home.path().canonicalize().expect("canonical home");
    let profile_root = root.join(".tracedecay");
    let original = root.join("nongit-original");
    std::fs::create_dir_all(&original).expect("create nongit project");
    let project_id = "proj_nongit_moved";
    let layout = enroll_nongit_project_on_disk(&original, &profile_root, project_id);
    let graph_bytes_before = std::fs::read(&layout.graph_db_path).expect("graph bytes");

    let _database_scope =
        enter_test_daemon_database_scope(&profile_root, "moved nongit unique re-adoption");
    let identity = tracedecay_daemon_identity::profile_identity::load_or_create(&profile_root)
        .expect("profile identity");
    let runtime = tracedecay_store_runtime::DaemonSessionRuntimeRegistryV1::open(identity)
        .await
        .expect("profile runtime");
    let registry = runtime.profile_database().await.expect("profile registry");
    registry
        .upsert_code_project(project_id, &original, None, None, None)
        .await
        .expect("register nongit project");

    let moved = root.join("nongit-moved");
    std::fs::rename(&original, &moved).expect("move nongit project");
    let moved_canonical = moved.canonicalize().expect("canonical moved root");

    // Ambient first-touch (`Never`) mints a fresh path-derived identity and
    // must not touch the moved project's registration.
    let ambient = super::TraceDecay::resolve_first_touch_configuration_layout_with_adoption(
        &moved,
        &moved_nongit_open_options(&profile_root),
        registry.as_ref(),
        &super::MovedStoreAdoption::Never,
    )
    .await
    .expect("ambient first-touch mints fresh");
    assert_eq!(
        ambient.identity.project_id.as_deref(),
        Some(
            tracedecay_runtime_core::storage::default_profile_project_id(&moved_canonical).as_str()
        ),
        "ambient first-touch must mint the path-derived identity, never adopt"
    );

    // Explicit init without adoption flags refuses with the candidate and
    // the explicit choices instead of silently remapping or silently
    // splitting identity.
    let offer = super::TraceDecay::resolve_first_touch_configuration_layout_with_adoption(
        &moved,
        &moved_nongit_open_options(&profile_root),
        registry.as_ref(),
        &super::MovedStoreAdoption::OfferCandidates,
    )
    .await
    .expect_err("explicit init without flags must refuse when a candidate exists");
    let message = offer.to_string();
    assert!(
        message.contains(project_id)
            && message.contains("--adopt-project")
            && message.contains("--fresh"),
        "refusal must name the candidate and both explicit choices, got {message}"
    );
    let unmoved = registry
        .project_registry_context_by_id(project_id)
        .await
        .expect("registry lookup")
        .expect("refusal must not touch the registration");
    assert_eq!(
        std::path::Path::new(&unmoved.project.canonical_root),
        original.as_path(),
        "a refusal must leave the registry row untouched"
    );

    // `init --yes` confirms adopting the unique candidate.
    let store_layout = super::TraceDecay::resolve_first_touch_configuration_layout_with_adoption(
        &moved,
        &moved_nongit_open_options(&profile_root),
        registry.as_ref(),
        &super::MovedStoreAdoption::AdoptUnique,
    )
    .await
    .expect("confirmed unique moved nongit project must be adopted");
    assert_eq!(
        store_layout.identity.project_id.as_deref(),
        Some(project_id),
        "adoption must keep the registered identity, never mint a path-derived alias"
    );
    assert_eq!(
        store_layout.graph_db_path, layout.graph_db_path,
        "adoption must keep the existing profile shard"
    );
    let remapped = registry
        .project_registry_context_by_id(project_id)
        .await
        .expect("registry lookup")
        .expect("adopted project remains registered");
    assert_eq!(
        std::path::Path::new(&remapped.project.canonical_root),
        moved_canonical.as_path(),
        "registry canonical root must follow the move"
    );
    let graph_bytes_after = std::fs::read(&layout.graph_db_path).expect("graph bytes after");
    assert_eq!(
        graph_bytes_before, graph_bytes_after,
        "adoption must leave store data intact"
    );
    assert!(
        !moved.join(".tracedecay").exists(),
        "adoption must not create working-tree state"
    );
}

/// One stale non-git registry row must never hijack an unrelated fresh
/// directory: ambient first-touch (any handshake with `allow_init`, e.g.
/// agent tools touching a scratch directory) always mints the path-derived
/// identity and leaves the stale registration alone.
#[cfg(unix)]
#[asupersync::test]
async fn ambient_first_touch_never_adopts_a_moved_nongit_store() {
    let home = TempDir::new().expect("isolated home");
    let root = home.path().canonicalize().expect("canonical home");
    let profile_root = root.join(".tracedecay");
    let original = root.join("nongit-original");
    std::fs::create_dir_all(&original).expect("create nongit project");
    let stale_id = "proj_nongit_stale";
    enroll_nongit_project_on_disk(&original, &profile_root, stale_id);

    let _database_scope =
        enter_test_daemon_database_scope(&profile_root, "ambient first-touch never adopts");
    let identity = tracedecay_daemon_identity::profile_identity::load_or_create(&profile_root)
        .expect("profile identity");
    let runtime = tracedecay_store_runtime::DaemonSessionRuntimeRegistryV1::open(identity)
        .await
        .expect("profile runtime");
    let registry = runtime.profile_database().await.expect("profile registry");
    registry
        .upsert_code_project(stale_id, &original, None, None, None)
        .await
        .expect("register nongit project");
    std::fs::remove_dir_all(&original).expect("delete the project without wiping the store");

    let scratch = root.join("unrelated-scratch");
    std::fs::create_dir_all(&scratch).expect("create unrelated directory");
    let scratch_canonical = scratch.canonicalize().expect("canonical scratch root");

    let layout = super::TraceDecay::resolve_first_touch_configuration_layout_with_adoption(
        &scratch,
        &moved_nongit_open_options(&profile_root),
        registry.as_ref(),
        &super::MovedStoreAdoption::Never,
    )
    .await
    .expect("ambient first-touch on a fresh directory mints a fresh identity");
    assert_eq!(
        layout.identity.project_id.as_deref(),
        Some(
            tracedecay_runtime_core::storage::default_profile_project_id(&scratch_canonical)
                .as_str()
        ),
        "ambient first-touch must never inherit a stale project identity"
    );
    let stale = registry
        .project_registry_context_by_id(stale_id)
        .await
        .expect("registry lookup")
        .expect("stale registration survives");
    assert_eq!(
        std::path::Path::new(&stale.project.canonical_root),
        original.as_path(),
        "ambient first-touch must not rewrite the stale registry row"
    );
}

/// Two moved non-git stores can claim a brand-new root. A confirmed adoption
/// (`--yes`) must refuse instead of picking a winner, and the stale stores
/// must not brick a fresh init: opting out of adoption mints a new identity.
#[cfg(unix)]
#[asupersync::test]
async fn moved_nongit_adoption_is_refused_when_ambiguous() {
    let home = TempDir::new().expect("isolated home");
    let root = home.path().canonicalize().expect("canonical home");
    let profile_root = root.join(".tracedecay");
    let original_a = root.join("nongit-a");
    let original_b = root.join("nongit-b");
    std::fs::create_dir_all(&original_a).expect("create project a");
    std::fs::create_dir_all(&original_b).expect("create project b");
    enroll_nongit_project_on_disk(&original_a, &profile_root, "proj_nongit_ambig_a");
    enroll_nongit_project_on_disk(&original_b, &profile_root, "proj_nongit_ambig_b");

    let _database_scope =
        enter_test_daemon_database_scope(&profile_root, "moved nongit ambiguous adoption");
    let identity = tracedecay_daemon_identity::profile_identity::load_or_create(&profile_root)
        .expect("profile identity");
    let runtime = tracedecay_store_runtime::DaemonSessionRuntimeRegistryV1::open(identity)
        .await
        .expect("profile runtime");
    let registry = runtime.profile_database().await.expect("profile registry");
    registry
        .upsert_code_project("proj_nongit_ambig_a", &original_a, None, None, None)
        .await
        .expect("register a");
    registry
        .upsert_code_project("proj_nongit_ambig_b", &original_b, None, None, None)
        .await
        .expect("register b");

    std::fs::rename(&original_a, root.join("nongit-a-moved")).expect("move a");
    std::fs::rename(&original_b, root.join("nongit-b-moved")).expect("move b");
    let target = root.join("nongit-new");
    std::fs::create_dir_all(&target).expect("create adoption target");

    let error = super::TraceDecay::resolve_first_touch_configuration_layout_with_adoption(
        &target,
        &moved_nongit_open_options(&profile_root),
        registry.as_ref(),
        &super::MovedStoreAdoption::AdoptUnique,
    )
    .await
    .expect_err("ambiguous moved nongit adoption must refuse");
    let message = error.to_string();
    assert!(
        message.contains("ambiguous") && message.contains("--adopt-project"),
        "refusal must name the adopt flag, got {message}"
    );
    assert!(
        message.contains("proj_nongit_ambig_a") && message.contains("proj_nongit_ambig_b"),
        "refusal must name both candidates, got {message}"
    );

    // The stale stores must not brick a genuinely new project: opting out of
    // adoption (`--fresh`, and every ambient first-touch) mints fresh.
    let fresh = super::TraceDecay::resolve_first_touch_configuration_layout_with_adoption(
        &target,
        &moved_nongit_open_options(&profile_root),
        registry.as_ref(),
        &super::MovedStoreAdoption::Never,
    )
    .await
    .expect("fresh init must stay possible with stale moved stores present");
    let target_canonical = target.canonicalize().expect("canonical target");
    assert_eq!(
        fresh.identity.project_id.as_deref(),
        Some(
            tracedecay_runtime_core::storage::default_profile_project_id(&target_canonical)
                .as_str()
        ),
        "opting out of adoption must mint the path-derived identity"
    );
}

/// `--adopt-project` selects exactly one moved non-git store when first-touch
/// would otherwise be ambiguous.
#[cfg(unix)]
#[asupersync::test]
async fn moved_nongit_adoption_honors_explicit_project_id() {
    let home = TempDir::new().expect("isolated home");
    let root = home.path().canonicalize().expect("canonical home");
    let profile_root = root.join(".tracedecay");
    let original_a = root.join("nongit-a");
    let original_b = root.join("nongit-b");
    std::fs::create_dir_all(&original_a).expect("create project a");
    std::fs::create_dir_all(&original_b).expect("create project b");
    let layout_a = enroll_nongit_project_on_disk(&original_a, &profile_root, "proj_nongit_flag_a");
    enroll_nongit_project_on_disk(&original_b, &profile_root, "proj_nongit_flag_b");
    let graph_bytes_before = std::fs::read(&layout_a.graph_db_path).expect("graph bytes");

    let _database_scope =
        enter_test_daemon_database_scope(&profile_root, "moved nongit flagged adoption");
    let identity = tracedecay_daemon_identity::profile_identity::load_or_create(&profile_root)
        .expect("profile identity");
    let runtime = tracedecay_store_runtime::DaemonSessionRuntimeRegistryV1::open(identity)
        .await
        .expect("profile runtime");
    let registry = runtime.profile_database().await.expect("profile registry");
    registry
        .upsert_code_project("proj_nongit_flag_a", &original_a, None, None, None)
        .await
        .expect("register a");
    registry
        .upsert_code_project("proj_nongit_flag_b", &original_b, None, None, None)
        .await
        .expect("register b");

    std::fs::rename(&original_a, root.join("nongit-a-moved")).expect("move a");
    std::fs::rename(&original_b, root.join("nongit-b-moved")).expect("move b");
    let target = root.join("nongit-new");
    std::fs::create_dir_all(&target).expect("create adoption target");

    let store_layout = super::TraceDecay::resolve_first_touch_configuration_layout_with_adoption(
        &target,
        &moved_nongit_open_options(&profile_root),
        registry.as_ref(),
        &super::MovedStoreAdoption::AdoptNamed("proj_nongit_flag_a".to_owned()),
    )
    .await
    .expect("flagged adoption must select the named project");
    assert_eq!(
        store_layout.identity.project_id.as_deref(),
        Some("proj_nongit_flag_a")
    );
    let remapped = registry
        .project_registry_context_by_id("proj_nongit_flag_a")
        .await
        .expect("registry lookup")
        .expect("adopted project remains registered");
    assert_eq!(
        std::path::Path::new(&remapped.project.canonical_root),
        target.canonicalize().expect("canonical target").as_path()
    );
    assert_eq!(
        std::fs::read(&layout_a.graph_db_path).expect("graph bytes after"),
        graph_bytes_before
    );
}

/// A new root that already resolves to a different registered project cannot
/// be aliased onto a moved store.
#[cfg(unix)]
#[asupersync::test]
async fn moved_nongit_adoption_refuses_conflicting_registered_root() {
    let home = TempDir::new().expect("isolated home");
    let root = home.path().canonicalize().expect("canonical home");
    let profile_root = root.join(".tracedecay");
    let occupant = root.join("occupant");
    let original = root.join("nongit-original");
    std::fs::create_dir_all(&occupant).expect("create occupant");
    std::fs::create_dir_all(&original).expect("create moved project");
    enroll_nongit_project_on_disk(&occupant, &profile_root, "proj_nongit_occupant");
    enroll_nongit_project_on_disk(&original, &profile_root, "proj_nongit_conflict");

    let _database_scope =
        enter_test_daemon_database_scope(&profile_root, "moved nongit conflicting adoption");
    let identity = tracedecay_daemon_identity::profile_identity::load_or_create(&profile_root)
        .expect("profile identity");
    let runtime = tracedecay_store_runtime::DaemonSessionRuntimeRegistryV1::open(identity)
        .await
        .expect("profile runtime");
    let registry = runtime.profile_database().await.expect("profile registry");
    registry
        .upsert_code_project("proj_nongit_occupant", &occupant, None, None, None)
        .await
        .expect("register occupant");
    registry
        .upsert_code_project("proj_nongit_conflict", &original, None, None, None)
        .await
        .expect("register moved project");
    std::fs::rename(&original, root.join("nongit-moved")).expect("move conflicting project");

    let error = super::TraceDecay::resolve_first_touch_configuration_layout_with_adoption(
        &occupant,
        &moved_nongit_open_options(&profile_root),
        registry.as_ref(),
        &super::MovedStoreAdoption::AdoptNamed("proj_nongit_conflict".to_owned()),
    )
    .await
    .expect_err("adoption onto another project's root must refuse");
    let message = error.to_string();
    assert!(
        message.contains("proj_nongit_conflict") && message.contains("proj_nongit_occupant"),
        "conflict refusal must name both identities, got {message}"
    );
}

/// A remap interrupted between the store-side evidence write and the registry
/// commit leaves the shard manifest naming the new root while the registry
/// still names the gone previous root. That manifest is the journal record:
/// the next explicit init resumes the remap without flags, positive linkage
/// written under the earlier explicit adoption, and commits the registry.
#[cfg(unix)]
#[asupersync::test]
async fn interrupted_moved_nongit_remap_resumes_on_next_explicit_init() {
    let home = TempDir::new().expect("isolated home");
    let root = home.path().canonicalize().expect("canonical home");
    let profile_root = root.join(".tracedecay");
    let original = root.join("nongit-original");
    std::fs::create_dir_all(&original).expect("create nongit project");
    let project_id = "proj_nongit_torn";
    let layout = enroll_nongit_project_on_disk(&original, &profile_root, project_id);
    let graph_bytes_before = std::fs::read(&layout.graph_db_path).expect("graph bytes");

    let _database_scope =
        enter_test_daemon_database_scope(&profile_root, "interrupted nongit remap resume");
    let identity = tracedecay_daemon_identity::profile_identity::load_or_create(&profile_root)
        .expect("profile identity");
    let runtime = tracedecay_store_runtime::DaemonSessionRuntimeRegistryV1::open(identity)
        .await
        .expect("profile runtime");
    let registry = runtime.profile_database().await.expect("profile registry");
    registry
        .upsert_code_project(project_id, &original, None, None, None)
        .await
        .expect("register nongit project");

    let moved = root.join("nongit-moved");
    std::fs::rename(&original, &moved).expect("move nongit project");
    // Simulate the interruption: the remap wrote the shard manifest for the
    // new root but crashed before the registry upsert.
    let torn_layout =
        tracedecay_runtime_core::storage::profile_sharded_layout(&moved, &profile_root, project_id)
            .expect("layout for the interrupted remap");
    tracedecay_runtime_core::storage::write_store_manifest(&torn_layout)
        .expect("journal manifest write");

    let resumed = super::TraceDecay::resolve_first_touch_configuration_layout_with_adoption(
        &moved,
        &moved_nongit_open_options(&profile_root),
        registry.as_ref(),
        &super::MovedStoreAdoption::OfferCandidates,
    )
    .await
    .expect("a torn remap must resume from its manifest journal record");
    assert_eq!(
        resumed.identity.project_id.as_deref(),
        Some(project_id),
        "resume must restore the registered identity, never mint an alias"
    );
    let remapped = registry
        .project_registry_context_by_id(project_id)
        .await
        .expect("registry lookup")
        .expect("resumed project remains registered");
    let moved_canonical = moved.canonicalize().expect("canonical moved root");
    assert_eq!(
        std::path::Path::new(&remapped.project.canonical_root),
        moved_canonical.as_path(),
        "resume must commit the registry to the new root"
    );
    assert_eq!(
        std::fs::read(&layout.graph_db_path).expect("graph bytes after"),
        graph_bytes_before,
        "resume must leave store data intact"
    );
}

/// A present-but-unreadable store manifest must fail candidate evaluation
/// with a typed error, not silently drop the candidate: a corrupt manifest
/// could otherwise remove the true candidate and make a wrong one unique.
#[cfg(unix)]
#[asupersync::test]
async fn unreadable_moved_store_evidence_is_a_typed_refusal() {
    let home = TempDir::new().expect("isolated home");
    let root = home.path().canonicalize().expect("canonical home");
    let profile_root = root.join(".tracedecay");
    let original = root.join("nongit-original");
    std::fs::create_dir_all(&original).expect("create nongit project");
    let project_id = "proj_nongit_corrupt";
    let layout = enroll_nongit_project_on_disk(&original, &profile_root, project_id);

    let _database_scope =
        enter_test_daemon_database_scope(&profile_root, "unreadable adoption evidence");
    let identity = tracedecay_daemon_identity::profile_identity::load_or_create(&profile_root)
        .expect("profile identity");
    let runtime = tracedecay_store_runtime::DaemonSessionRuntimeRegistryV1::open(identity)
        .await
        .expect("profile runtime");
    let registry = runtime.profile_database().await.expect("profile registry");
    registry
        .upsert_code_project(project_id, &original, None, None, None)
        .await
        .expect("register nongit project");

    let moved = root.join("nongit-moved");
    std::fs::rename(&original, &moved).expect("move nongit project");
    let manifest_path = layout
        .manifest_path
        .as_deref()
        .expect("profile-sharded layout carries a manifest path");
    std::fs::write(manifest_path, b"not a manifest").expect("corrupt the manifest");

    let error = super::TraceDecay::resolve_first_touch_configuration_layout_with_adoption(
        &moved,
        &moved_nongit_open_options(&profile_root),
        registry.as_ref(),
        &super::MovedStoreAdoption::AdoptUnique,
    )
    .await
    .expect_err("unreadable evidence must be a typed error, not a silent non-match");
    let message = error.to_string();
    assert!(
        message.contains("moved-store adoption evidence") && message.contains("--fresh"),
        "the refusal must name the evidence failure and the fresh-mint escape, got {message}"
    );
}

fn enter_test_daemon_database_scope(
    profile_root: &std::path::Path,
    label: &str,
) -> tracedecay_runtime_core::db::DaemonDatabaseScope {
    tracedecay_runtime_core::storage::PrivateStoreIo::create_dir_all(profile_root)
        .expect("create private profile root");
    tracedecay_runtime_core::db::enter_daemon_database_scope(profile_root, 1, "test-daemon")
        .unwrap_or_else(|error| panic!("enter test daemon database scope ({label}): {error}"))
}
