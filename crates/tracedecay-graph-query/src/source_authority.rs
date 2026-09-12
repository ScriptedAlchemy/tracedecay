//! Admission-bound capture of the exact project source authority.
//!
//! The [`SourceReadContext`] wired at composition enters a verified graph
//! query exactly once, at admitted open, where it is frozen into
//! [`AdmittedSourceAuthority`]: root, database authority, read-only posture,
//! and project identity are copied once and used exclusively thereafter, so
//! no later API accepts a substitute.

use std::path::{Path, PathBuf};

use tracedecay_contracts::RequestContext;
use tracedecay_domain::ProjectId;
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_runtime_core::db::Database;

use crate::SourceReadContext;

/// Exact source authority frozen at admitted open.
///
/// Construction is crate-private: nothing outside this crate can build or
/// inject one, so the only way a source reaches a [`super::VerifiedGraphQuery`]
/// is the admission-validated capture inside [`super::open_verified_graph_query`].
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
    pub(crate) fn capture(context: &RequestContext, source: SourceReadContext) -> Result<Self> {
        if source.project_id() != context.scope().project_id.as_str() {
            return Err(graph_source_scope_mismatch());
        }
        Ok(Self {
            project_root: source.project_root,
            db: source.db,
            read_only: source.read_only,
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
