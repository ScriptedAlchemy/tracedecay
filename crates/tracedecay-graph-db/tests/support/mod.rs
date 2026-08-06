use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tracedecay_graph_db::{
    GraphDb, GraphDbError, GraphDbRegistration, GraphDbRegistry, GraphDbRegistryConfig,
    NeverCancelled,
};
use tracedecay_store::{
    BrainId, ProjectId, RetainedGraphStoreLeaseV1, StoreAuthorityEpochV1, StoreIncarnationV1,
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
        cancellation: Arc::new(NeverCancelled),
        lifecycle_cancellation: Arc::new(NeverCancelled),
        deadline: Instant::now() + Duration::from_secs(30),
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
