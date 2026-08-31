//! Admission-bound capture of the exact project source authority.
//!
//! The source runtime enters a verified graph query exactly once, at admitted
//! open, through [`CodeGraphSourceAuthorityPort`]. The bind wait is raced
//! against the canonical deadline/cancellation pair by the opener, and the
//! returned runtime is immediately frozen into [`AdmittedSourceAuthority`]:
//! root, database authority, read-only posture, and project identity are
//! copied once and used exclusively thereafter, so a runtime facade cannot
//! change its answers after admission and no later API accepts a substitute.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;

use tracedecay_application::RequestContext;
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_domain::{ProjectId, UtcMicros};
use tracedecay_runtime_core::db::Database;

use super::CodeGraphReadError;
use crate::SourceReadRuntimePort;

/// Inputs handed to the source authority when one admitted graph query binds
/// its exact project source. The context is the admitted context returned by
/// the canonical admission port, so implementations resolve the source for
/// exactly that scope and never for a caller-chosen root or database.
pub struct CodeGraphSourceBindRequest<'a> {
    pub context: &'a RequestContext,
    pub observed_at: UtcMicros,
}

pub type CodeGraphSourceBindFuture<'a> = Pin<
    Box<
        dyn Future<Output = std::result::Result<Arc<dyn SourceReadRuntimePort>, CodeGraphReadError>>
            + Send
            + 'a,
    >,
>;

/// Lower open-time port supplying the exact project source runtime for an
/// admitted graph query. It is wired once at composition; handlers never hold
/// it, and the opener validates the returned runtime against the admitted
/// scope before freezing it.
pub trait CodeGraphSourceAuthorityPort: Send + Sync {
    fn bind<'a>(&'a self, request: CodeGraphSourceBindRequest<'a>)
    -> CodeGraphSourceBindFuture<'a>;
}

impl<T> CodeGraphSourceAuthorityPort for Arc<T>
where
    T: CodeGraphSourceAuthorityPort + ?Sized,
{
    fn bind<'a>(
        &'a self,
        request: CodeGraphSourceBindRequest<'a>,
    ) -> CodeGraphSourceBindFuture<'a> {
        (**self).bind(request)
    }
}

/// Exact source authority frozen at admitted open.
///
/// Construction is crate-private: nothing outside this crate can build or
/// inject one, so the only way a source reaches a [`super::VerifiedGraphQuery`]
/// is the admission-raced bind inside [`super::open_verified_graph_query`].
pub(crate) struct AdmittedSourceAuthority {
    project_root: PathBuf,
    db: Database,
    read_only: bool,
    project_id: ProjectId,
}

impl AdmittedSourceAuthority {
    /// Freezes the runtime's answers after validating its claimed identity
    /// against the admitted scope. Identity is denied before any other
    /// runtime surface is consulted.
    pub(crate) fn capture(
        context: &RequestContext,
        runtime: &dyn SourceReadRuntimePort,
    ) -> Result<Self> {
        if runtime.project_id() != context.scope().project_id.as_str() {
            return Err(graph_source_scope_mismatch());
        }
        Ok(Self {
            project_root: runtime.project_root().to_path_buf(),
            db: runtime.db().clone(),
            read_only: runtime.is_read_only(),
            project_id: context.scope().project_id.clone(),
        })
    }

    pub(crate) fn project_root(&self) -> &Path {
        &self.project_root
    }

    pub(crate) fn db(&self) -> &Database {
        &self.db
    }

    pub(crate) fn read_only(&self) -> bool {
        self.read_only
    }

    pub(crate) fn project_id(&self) -> &str {
        self.project_id.as_str()
    }
}

pub(crate) fn graph_source_unbound() -> TraceDecayError {
    TraceDecayError::project_route(
        "code-graph-denied",
        false,
        "the admitted graph query has no bound project source authority",
    )
}

pub(crate) fn graph_source_scope_mismatch() -> TraceDecayError {
    TraceDecayError::project_route(
        "code-graph-denied",
        false,
        "the source read is outside the admitted graph query project scope",
    )
}
