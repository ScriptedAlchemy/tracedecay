//! Daemon-owned project-store/session runtime, as the aggregate consumes it.
//!
//! `DaemonSessionRuntimeRegistryV1` implements this in the root crate. This
//! crate must not depend on that type.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;

use tracedecay_domain::errors::Result;
use tracedecay_domain::{ProjectId, UserProfileId};
use tracedecay_global_db::RegisteredGlobalDbLeaseV1;
use tracedecay_runtime_core::db::{Database, DatabaseAccessMode, DatabaseAuthority};

pub type RuntimeFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>;

/// Project-store and session mounts the TraceDecay aggregate actually uses.
///
/// Dyn-safe (boxed futures) so the aggregate can consume `&dyn ProjectStoreRuntimeV1`.
pub trait ProjectStoreRuntimeV1: Send + Sync {
    fn profile_id(&self) -> &UserProfileId;
    fn profile_database(&self) -> RuntimeFuture<'_, RegisteredGlobalDbLeaseV1>;
    fn project_sessions(
        &self,
        project_id: ProjectId,
        roots: Vec<PathBuf>,
    ) -> RuntimeFuture<'_, RegisteredGlobalDbLeaseV1>;
    fn project_graph(
        &self,
        project_root: &Path,
        project_id: ProjectId,
        db_path: PathBuf,
        authority: DatabaseAuthority,
        access: DatabaseAccessMode,
    ) -> RuntimeFuture<'_, Database>;
    fn project_graph_registered(
        &self,
        project_id: ProjectId,
        db_path: PathBuf,
        access: DatabaseAccessMode,
    ) -> RuntimeFuture<'_, Database>;
}
