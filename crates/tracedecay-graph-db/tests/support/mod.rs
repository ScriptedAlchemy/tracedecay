use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tracedecay_graph_db::{
    GraphDb, GraphDbError, GraphDbRegistration, GraphDbRegistry, GraphDbRegistryConfig,
    NeverCancelled,
};
use tracedecay_store::{
    BrainId, GRAPH_STORE_PRIVATE_DIRECTORY, ProjectId, StoreAuthorityEpochV1, StoreIncarnationV1,
    StoreRuntimeBindingV1, StoreShardIdV1, UserProfileId, VerifiedStoreLocatorV1,
    canonical_store_locator_digest,
};

pub struct RegisteredGraph {
    pub registry: GraphDbRegistry,
    pub binding: StoreRuntimeBindingV1,
    root: PathBuf,
}

impl RegisteredGraph {
    pub fn open(root: &Path) -> Result<(Self, Arc<GraphDb>), GraphDbError> {
        let registry = GraphDbRegistry::new(GraphDbRegistryConfig { max_open: 1 })?;
        let binding = binding();
        let database = registry.resolve(registration(binding.clone(), root))?;
        Ok((
            Self {
                registry,
                binding,
                root: root.to_path_buf(),
            },
            database,
        ))
    }

    pub fn close(&self) -> Result<bool, GraphDbError> {
        self.registry
            .close(&registration(self.binding.clone(), &self.root))
    }

    pub fn reopen(&self) -> Result<Arc<GraphDb>, GraphDbError> {
        self.registry
            .reopen(registration(self.binding.clone(), &self.root))
    }
}

pub fn graph_path(root: &Path) -> PathBuf {
    root.join(GRAPH_STORE_PRIVATE_DIRECTORY)
        .join("graph.grafeo")
}

pub fn registration(binding: StoreRuntimeBindingV1, root: &Path) -> GraphDbRegistration {
    create_private_graph_directory(root);
    let canonical_path = graph_path(root);
    let verified_locator = VerifiedStoreLocatorV1::new(
        binding.shard_id.clone(),
        binding.incarnation,
        canonical_store_locator_digest(&canonical_path).unwrap(),
    );
    GraphDbRegistration {
        binding,
        verified_locator,
        canonical_path,
        cancellation: Arc::new(NeverCancelled),
        lifecycle_cancellation: Arc::new(NeverCancelled),
        deadline: Instant::now() + Duration::from_secs(30),
    }
}

#[cfg(unix)]
fn create_private_graph_directory(root: &Path) {
    use std::os::unix::fs::DirBuilderExt;

    let mut builder = std::fs::DirBuilder::new();
    builder.mode(0o700);
    match builder.create(root.join(GRAPH_STORE_PRIVATE_DIRECTORY)) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => panic!("create private graph directory: {error}"),
    }
}

#[cfg(windows)]
fn create_private_graph_directory(root: &Path) {
    match std::fs::create_dir(root.join(GRAPH_STORE_PRIVATE_DIRECTORY)) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => panic!("create private graph directory: {error}"),
    }
}

#[cfg(not(any(unix, windows)))]
fn create_private_graph_directory(_root: &Path) {
    panic!("private graph storage is unsupported on this platform");
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
