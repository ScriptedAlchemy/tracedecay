//! Runtime authorities supplied by the root composition layer.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;

use tracedecay_domain::errors::Result;
use tracedecay_domain::{FactOwnerV1, ProjectId, UserProfileId};
use tracedecay_global_db::RegisteredGlobalDbLeaseV1;
use tracedecay_runtime_core::db::Database;
use tracedecay_runtime_core::storage::StoreLayout;

use crate::automation::host_io::HostIo;

pub type RuntimeFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>;

/// Project runtime needed by automation.
pub trait ProjectRuntime: Send + Sync {
    fn project_root(&self) -> &Path;
    /// The host-install I/O the composition root built for managed-skill
    /// deployment; automation never reaches host-owned files another way.
    fn host_io(&self) -> HostIo;
    fn db(&self) -> &Database;
    fn store_layout(&self) -> &StoreLayout;
    fn project_memory_owner(&self) -> Result<FactOwnerV1>;
    fn profile_id(&self) -> &UserProfileId;
    fn profile_database(&self) -> &RegisteredGlobalDbLeaseV1;
    fn project_sessions(
        &self,
        project_id: ProjectId,
        roots: Vec<PathBuf>,
    ) -> RuntimeFuture<'_, RegisteredGlobalDbLeaseV1>;
    fn open_project_store_db(&self) -> RuntimeFuture<'_, Database>;
}

pub type TraceDecay = dyn ProjectRuntime;

/// Profile runtime needed by projectless automation.
pub trait ProfileRuntime: Send + Sync {
    fn profile_id(&self) -> &UserProfileId;
    fn profile_sessions(&self) -> RuntimeFuture<'_, RegisteredGlobalDbLeaseV1>;
    fn open_user_memory_db(&self) -> RuntimeFuture<'_, Database>;
}
