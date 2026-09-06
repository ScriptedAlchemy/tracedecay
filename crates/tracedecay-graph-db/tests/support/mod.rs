#![allow(dead_code)] // shared test support: each contract target uses a subset

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[cfg(any(feature = "test-helpers", feature = "eval-helpers"))]
use tracedecay_graph_db::GraphDbLeaseV1;
use tracedecay_graph_db::{
    GraphCancellation, GraphDbError, GraphDbOwnerAttachmentV1, GraphDbOwnerRegistrationV1,
    GraphDbRegistration, GraphDbRegistry, GraphDbRegistryConfig, GraphGenerationManifestProvider,
};
use tracedecay_store::{
    BrainId, ProjectId, RetainedGraphStoreLeaseV1, RetainedGraphStoreOwnerAttachmentV1,
    RetainedGraphStoreOwnerOperationLeaseErrorV1, StoreAuthorityEpochV1, StoreIncarnationV1,
    StoreRuntimeBindingV1, StoreShardIdV1, UserProfileId, VerifiedStoreLocatorV1,
    canonical_store_locator_digest,
};

#[derive(Debug)]
struct TestGraphLease {
    binding: StoreRuntimeBindingV1,
    verified_locator: VerifiedStoreLocatorV1,
    canonical_path: PathBuf,
}

impl RetainedGraphStoreLeaseV1 for TestGraphLease {
    fn binding(&self) -> &StoreRuntimeBindingV1 {
        &self.binding
    }

    fn verified_locator(&self) -> &VerifiedStoreLocatorV1 {
        &self.verified_locator
    }

    fn canonical_path(&self) -> &Path {
        &self.canonical_path
    }
}

impl RetainedGraphStoreOwnerAttachmentV1 for TestGraphLease {
    fn binding(&self) -> &StoreRuntimeBindingV1 {
        &self.binding
    }

    fn verified_locator(&self) -> &VerifiedStoreLocatorV1 {
        &self.verified_locator
    }

    fn canonical_path(&self) -> &Path {
        &self.canonical_path
    }

    fn issue_operation_lease(
        &self,
    ) -> Result<Arc<dyn RetainedGraphStoreLeaseV1>, RetainedGraphStoreOwnerOperationLeaseErrorV1>
    {
        Ok(Arc::new(Self {
            binding: self.binding.clone(),
            verified_locator: self.verified_locator.clone(),
            canonical_path: self.canonical_path.clone(),
        }))
    }
}

pub struct RegisteredGraph {
    pub registry: GraphDbRegistry,
    pub binding: StoreRuntimeBindingV1,
    root: PathBuf,
    /// Kept alive only for lazy mounts. The last client lease / snapshot drop
    /// hibernates the staging engine only while this attachment still lives;
    /// dropping it leaves the engine resident because the registry still holds
    /// the owner (neither hibernate nor close).
    lazy_owner: Mutex<Option<GraphDbOwnerAttachmentV1>>,
}

#[derive(Debug)]
pub struct TestCancellation;

impl GraphCancellation for TestCancellation {
    fn is_cancelled(&self) -> bool {
        false
    }
}

impl RegisteredGraph {
    pub fn new(root: &Path) -> Result<Self, GraphDbError> {
        let registry = GraphDbRegistry::new(GraphDbRegistryConfig { max_open: 1 })?;
        let binding = binding();
        Ok(Self {
            registry,
            binding,
            root: root.to_path_buf(),
            lazy_owner: Mutex::new(None),
        })
    }

    pub fn new_mounted(root: &Path) -> Result<Self, GraphDbError> {
        let registered = Self::new(root)?;
        registered.mount()?;
        Ok(registered)
    }

    /// Eager mount whose registry can hydrate a journaled sealed-code-generation
    /// replay. Production remounts always have that provider; the default
    /// test registry is inline-only and cannot recover a sealed-only head.
    pub fn new_mounted_with_manifest_provider(
        root: &Path,
        provider: Arc<dyn GraphGenerationManifestProvider>,
    ) -> Result<Self, GraphDbError> {
        let registered = Self {
            registry: GraphDbRegistry::new_with_manifest_provider(
                GraphDbRegistryConfig { max_open: 1 },
                provider,
            )?,
            binding: binding(),
            root: root.to_path_buf(),
            lazy_owner: Mutex::new(None),
        };
        registered.mount()?;
        Ok(registered)
    }

    /// Lazy counterpart of [`Self::new_mounted`]: registers the owner without
    /// opening the native staging engine.
    ///
    /// After the first publish / snapshot opens the engine, dropping every
    /// [`GraphDbLeaseV1`], verified snapshot, and `VerifiedGraphCommit`
    /// hibernates it. Confirm with `GraphDb::staging_engine_is_open` on a
    /// freshly resolved lease — `resolve` does not reopen a hibernated lazy
    /// engine; `snapshot`, publish, and `staging_generation_row_counts` do.
    pub fn new_mounted_lazy(root: &Path) -> Result<Self, GraphDbError> {
        let registered = Self::new(root)?;
        registered.mount_lazy()?;
        Ok(registered)
    }

    pub fn mount(&self) -> Result<(), GraphDbError> {
        let registration = registration(self.binding.clone(), &self.root);
        let owner_attachment = self
            .registry
            .resolve_owner_attachment(owner_registration(registration))?;
        drop(owner_attachment);
        Ok(())
    }

    /// Same shape as [`Self::mount`], through `resolve_lazy_owner_attachment`.
    ///
    /// Retains the owner attachment so a later last-lease drop hibernates
    /// instead of leaving the engine resident. See [`Self::new_mounted_lazy`].
    pub fn mount_lazy(&self) -> Result<(), GraphDbError> {
        let registration = registration(self.binding.clone(), &self.root);
        let owner_attachment = self
            .registry
            .resolve_lazy_owner_attachment(owner_registration(registration))?;
        let mut lazy_owner = self.lazy_owner.lock().expect("lazy owner attachment lock");
        *lazy_owner = Some(owner_attachment);
        Ok(())
    }

    #[cfg(any(feature = "test-helpers", feature = "eval-helpers"))]
    pub fn open_lease(root: &Path) -> Result<(Self, GraphDbLeaseV1), GraphDbError> {
        let registered = Self::new_mounted(root)?;
        let registration = registration(registered.binding.clone(), root);
        let database = registered.registry.resolve(registration)?;
        Ok((registered, database))
    }

    pub fn close(&self) -> Result<bool, GraphDbError> {
        self.registry
            .close(&registration(self.binding.clone(), &self.root))
    }

    #[cfg(any(feature = "test-helpers", feature = "eval-helpers"))]
    pub fn reopen_lease(&self) -> Result<GraphDbLeaseV1, GraphDbError> {
        let registration = registration(self.binding.clone(), &self.root);
        self.mount()?;
        let lease = self.registry.resolve(registration)?;
        Ok(lease)
    }
}

pub fn graph_path(root: &Path) -> PathBuf {
    root.join("graph.grafeo")
}

pub fn registration(binding: StoreRuntimeBindingV1, root: &Path) -> GraphDbRegistration {
    let canonical_path = graph_path(root);
    let verified_locator = VerifiedStoreLocatorV1::new(
        binding.shard_id.clone(),
        binding.incarnation,
        canonical_store_locator_digest(&canonical_path).unwrap(),
    );
    GraphDbRegistration {
        authority_lease: Arc::new(TestGraphLease {
            binding,
            verified_locator,
            canonical_path,
        }),
        cancellation: Arc::new(TestCancellation),
        lifecycle_cancellation: Arc::new(TestCancellation),
        deadline: Instant::now() + Duration::from_secs(30),
    }
}

pub fn owner_registration(registration: GraphDbRegistration) -> GraphDbOwnerRegistrationV1 {
    let authority_attachment = Box::new(TestGraphLease {
        binding: registration.authority_lease.binding().clone(),
        verified_locator: registration.authority_lease.verified_locator().clone(),
        canonical_path: registration.authority_lease.canonical_path().to_path_buf(),
    });
    GraphDbOwnerRegistrationV1 {
        operation: registration,
        authority_attachment,
    }
}

fn binding() -> StoreRuntimeBindingV1 {
    StoreRuntimeBindingV1::new(
        StoreShardIdV1::project(
            BrainId::try_from("brain.graph-db-test".to_owned()).unwrap(),
            UserProfileId::try_from("profile.graph-db-test".to_owned()).unwrap(),
            ProjectId::try_from("project.graph-db-test".to_owned()).unwrap(),
        ),
        StoreIncarnationV1::new(1).unwrap(),
        StoreAuthorityEpochV1::new(1).unwrap(),
    )
}

/// Windows CI exports this to the whole shard as a speed knob. No production
/// graph or `SQLite` backend currently reads it; durability fixtures still
/// unset it so a green run cannot be mistaken for production durability proof.
pub const SQLITE_UNSAFE_FAST_ENV: &str = "TRACEDECAY_SQLITE_UNSAFE_FAST";
pub const GRAPH_CRASH_CHILD_ROOT_ENV: &str = "TRACEDECAY_GRAPH_CRASH_CHILD_ROOT";
const GRAPH_CRASH_CHILD_READY: &str = "durable-phase.ready";

/// Removes [`SQLITE_UNSAFE_FAST_ENV`] for the fixture's lifetime.
pub struct UnsetSqliteUnsafeFast {
    previous: Option<OsString>,
}

impl UnsetSqliteUnsafeFast {
    pub fn new() -> Self {
        let previous = std::env::var_os(SQLITE_UNSAFE_FAST_ENV);
        unsafe {
            std::env::remove_var(SQLITE_UNSAFE_FAST_ENV);
        }
        assert!(
            std::env::var_os(SQLITE_UNSAFE_FAST_ENV).is_none(),
            "durability fixtures must not credit {SQLITE_UNSAFE_FAST_ENV}"
        );
        Self { previous }
    }
}

impl Drop for UnsetSqliteUnsafeFast {
    fn drop(&mut self) {
        unsafe {
            match self.previous.take() {
                Some(value) => std::env::set_var(SQLITE_UNSAFE_FAST_ENV, value),
                None => std::env::remove_var(SQLITE_UNSAFE_FAST_ENV),
            }
        }
    }
}

pub fn crash_child_root() -> Option<PathBuf> {
    std::env::var_os(GRAPH_CRASH_CHILD_ROOT_ENV).map(PathBuf::from)
}

pub fn mark_durable_phase(root: &Path) {
    std::fs::write(root.join(GRAPH_CRASH_CHILD_READY), b"wal-synced").unwrap_or_else(|error| {
        panic!("write durable-phase marker: {error}");
    });
}

/// Runs this test in a child that exits without closing the store, then copies
/// the leftover container and WAL sidecar. The copy happens only after the
/// child has been joined, so Windows is not asked to `fs::copy` a live store.
pub fn capture_unclean_crash_image(destination: &Path) {
    let source = tempfile::TempDir::new().expect("crash source");
    run_unclean_crash_child(source.path());
    assert!(
        source.path().join(GRAPH_CRASH_CHILD_READY).is_file(),
        "crash child must reach the durable phase before exiting"
    );
    copy_crash_image(source.path(), destination);
}

fn current_test_name() -> String {
    std::env::var("NEXTEST_TEST_NAME")
        .ok()
        .or_else(|| std::thread::current().name().map(str::to_owned))
        .expect("crash child spawn needs the libtest/nextest test name")
}

fn run_unclean_crash_child(root: &Path) {
    let test_name = current_test_name();
    let output = Command::new(std::env::current_exe().expect("test binary"))
        .args(["--exact", &test_name, "--nocapture"])
        .env(GRAPH_CRASH_CHILD_ROOT_ENV, root)
        .env_remove(SQLITE_UNSAFE_FAST_ENV)
        .stdin(Stdio::null())
        .output()
        .unwrap_or_else(|error| panic!("spawn crash child for {test_name}: {error}"));
    assert!(
        output.status.success(),
        "crash child {test_name} failed ({:?})\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Recursively copies a *closed or abandoned* store's container and sidecar.
/// Callers must not use this against a live Windows handle (lock violation 33).
pub fn copy_crash_image(from_root: &Path, to_root: &Path) {
    let from = graph_path(from_root);
    let to = graph_path(to_root);
    std::fs::copy(&from, &to).unwrap_or_else(|error| {
        panic!(
            "copy abandoned crash container {} -> {}: {error}",
            from.display(),
            to.display()
        );
    });
    let from_sidecar = sidecar_wal_path(&from);
    let to_sidecar = sidecar_wal_path(&to);
    std::fs::create_dir_all(&to_sidecar).unwrap();
    for entry in std::fs::read_dir(&from_sidecar).unwrap() {
        let entry = entry.unwrap();
        std::fs::copy(entry.path(), to_sidecar.join(entry.file_name())).unwrap_or_else(|error| {
            panic!(
                "copy abandoned crash WAL {} -> {}: {error}",
                entry.path().display(),
                to_sidecar.display()
            );
        });
    }
}

pub fn sidecar_wal_path(path: &Path) -> PathBuf {
    let mut sidecar = path.as_os_str().to_owned();
    sidecar.push(".wal");
    PathBuf::from(sidecar)
}
