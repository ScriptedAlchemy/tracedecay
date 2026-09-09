//! Runtime authorities supplied by the root composition layer.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;

use tracedecay_domain::errors::Result;
use tracedecay_domain::{ProjectId, UserProfileId};
use tracedecay_global_db::RegisteredGlobalDbLeaseV1;
use tracedecay_runtime_core::db::Database;

use crate::automation::host_io::HostIo;

pub type RuntimeFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>;

/// Immutable project values captured by the composition root for one
/// automation run.
pub struct AutomationProjectContext {
    pub project_root: PathBuf,
    pub dashboard_root: PathBuf,
    pub host_io: HostIo,
    pub project_id: ProjectId,
    pub profile_id: UserProfileId,
    pub profile_database: RegisteredGlobalDbLeaseV1,
    pub project_sessions: RegisteredGlobalDbLeaseV1,
    pub project_memory_database: Database,
}

impl AutomationProjectContext {
    #[must_use]
    pub fn project_id(&self) -> &ProjectId {
        &self.project_id
    }

    #[must_use]
    pub fn project_root(&self) -> &Path {
        &self.project_root
    }
}

/// Profile runtime needed by projectless automation.
pub trait ProfileRuntime: Send + Sync {
    fn profile_id(&self) -> &UserProfileId;
    fn profile_sessions(&self) -> RuntimeFuture<'_, RegisteredGlobalDbLeaseV1>;
    fn open_user_memory_db(&self) -> RuntimeFuture<'_, Database>;
}
