//! Runtime authorities supplied by the root composition layer.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;

use tracedecay_domain::{BrainId, FactOwnerV1, ProjectId, UserProfileId};
use tracedecay_global_db::RegisteredGlobalDb;
use tracedecay_runtime_core::db::Database;
use tracedecay_runtime_core::errors::Result;
use tracedecay_runtime_core::storage::StoreLayout;

pub type RuntimeFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>;

/// Project runtime needed by automation.
pub trait ProjectRuntime: Send + Sync {
    fn project_root(&self) -> &Path;
    fn db(&self) -> &Database;
    fn store_layout(&self) -> &StoreLayout;
    fn project_memory_owner(&self) -> Result<FactOwnerV1>;
    fn profile_id(&self) -> &UserProfileId;
    fn profile_database(&self) -> &Arc<RegisteredGlobalDb>;
    fn project_sessions<'a>(
        &'a self,
        project_id: ProjectId,
        roots: Vec<PathBuf>,
    ) -> RuntimeFuture<'a, Arc<RegisteredGlobalDb>>;
    fn open_project_store_db(&self) -> RuntimeFuture<'_, Database>;
}

pub type TraceDecay = dyn ProjectRuntime;

/// Profile runtime needed by projectless automation.
pub trait ProfileRuntime: Send + Sync {
    fn profile_id(&self) -> &UserProfileId;
    fn profile_sessions(&self) -> RuntimeFuture<'_, Arc<RegisteredGlobalDb>>;
    fn open_user_memory_db(&self) -> RuntimeFuture<'_, Database>;
}

/// Stable profile identity values used to bind session evidence.
pub trait ProfileIdentity: Send + Sync {
    fn brain_id(&self) -> &BrainId;
    fn profile_id(&self) -> &UserProfileId;
}
