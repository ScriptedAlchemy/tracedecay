#![allow(dead_code)] // shared test support: each contract target uses a subset

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[cfg(any(feature = "test-helpers", feature = "eval-helpers"))]
use tracedecay_graph_db::GraphDbLeaseV1;
use tracedecay_graph_db::{
    GraphCancellation, GraphDbError, GraphDbOwnerRegistrationV1, GraphDbRegistration,
    GraphDbRegistry, GraphDbRegistryConfig,
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
        })
    }

    #[cfg(any(feature = "test-helpers", feature = "eval-helpers"))]
    pub fn open_lease(root: &Path) -> Result<(Self, GraphDbLeaseV1), GraphDbError> {
        let registered = Self::new(root)?;
        let registration = registration(registered.binding.clone(), root);
        let owner_attachment = registered
            .registry
            .resolve_owner_attachment(owner_registration(registration.clone()))?;
        let database = registered.registry.resolve(registration)?;
        drop(owner_attachment);
        Ok((registered, database))
    }

    pub fn close(&self) -> Result<bool, GraphDbError> {
        self.registry
            .close(&registration(self.binding.clone(), &self.root))
    }

    #[cfg(any(feature = "test-helpers", feature = "eval-helpers"))]
    pub fn reopen_lease(&self) -> Result<GraphDbLeaseV1, GraphDbError> {
        let registration = registration(self.binding.clone(), &self.root);
        let owner_attachment = self
            .registry
            .resolve_owner_attachment(owner_registration(registration.clone()))?;
        let lease = self.registry.resolve(registration)?;
        drop(owner_attachment);
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
