//! The daemon stores an owner-served operation needs beyond the project
//! graph. Project open publishes a core owner first and replaces it with the
//! full owner once the project's session stores mount, so admission reads
//! these declarations to decide which owner a call must reach.

use crate::ApplicationSurfaceOperation;

/// What a project's owner must have mounted to answer a request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OwnerStoresV1 {
    /// The project graph and the daemon authorities the core owner holds.
    ProjectGraph,
    /// The project's session stores and session sync owner, which only the
    /// full owner mounts.
    ProjectSessions,
}

/// The stores an operation declares it needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OwnerStoreDeclarationV1 {
    /// Every request of the operation needs these stores.
    Every(OwnerStoresV1),
    /// The operation multiplexes actions; each action of its typed request
    /// declares its own stores.
    PerAction,
}

impl ApplicationSurfaceOperation {
    pub const fn owner_stores(self) -> OwnerStoreDeclarationV1 {
        match self {
            // A managed test run is recorded in the project session store,
            // and a PR context page continues through a cursor that store
            // authenticates.
            Self::RunAffectedTests | Self::PrContext => {
                OwnerStoreDeclarationV1::Every(OwnerStoresV1::ProjectSessions)
            }
            Self::AdminCli | Self::HookRuntime => OwnerStoreDeclarationV1::PerAction,
            _ => OwnerStoreDeclarationV1::Every(OwnerStoresV1::ProjectGraph),
        }
    }
}
