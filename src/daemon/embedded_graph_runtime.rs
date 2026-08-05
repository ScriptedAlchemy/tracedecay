//! Daemon ownership for the one native Grafeo store mounted per project shard.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use thiserror::Error;
use tracedecay_global_db::session_temporal::relations::{
    SessionRelationScope, open_persistent_session_relation_graph,
    persistent_session_relation_graph_path,
};
use tracedecay_graph_db::{GraphDb, GraphDbError};

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub(crate) enum EmbeddedGraphRuntimeError {
    #[error("embedded graph owner identity conflicts with an existing mount")]
    IdentityConflict,
    #[error("embedded graph store requires reset: {0}")]
    ResetRequired(String),
    #[error("embedded graph store is corrupt: {0}")]
    Corrupt(String),
    #[error("embedded graph store is unavailable: {0}")]
    Unavailable(String),
}

impl From<GraphDbError> for EmbeddedGraphRuntimeError {
    fn from(error: GraphDbError) -> Self {
        match error {
            GraphDbError::ResetRequired { message } => Self::ResetRequired(message),
            GraphDbError::Corrupt { message } | GraphDbError::DurabilityUncertain { message } => {
                Self::Corrupt(message)
            }
            GraphDbError::Unavailable { message } => Self::Unavailable(message),
            GraphDbError::Closed => Self::Unavailable("embedded graph is closed".to_owned()),
            GraphDbError::Cancelled => {
                Self::Unavailable("embedded graph open was cancelled".to_owned())
            }
            GraphDbError::InvalidRequest { message } => Self::Unavailable(message),
            GraphDbError::Conflict => Self::IdentityConflict,
            GraphDbError::BudgetExhausted => {
                Self::Unavailable("embedded graph open budget was exhausted".to_owned())
            }
        }
    }
}

struct MountedProjectGraph {
    store_root: PathBuf,
    graph_path: PathBuf,
    database: Arc<GraphDb>,
}

#[derive(Clone, Default)]
pub(crate) struct EmbeddedGraphRuntimeRegistry {
    mounted: Arc<Mutex<BTreeMap<SessionRelationScope, MountedProjectGraph>>>,
}

impl std::fmt::Debug for EmbeddedGraphRuntimeRegistry {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("EmbeddedGraphRuntimeRegistry")
            .finish_non_exhaustive()
    }
}

impl EmbeddedGraphRuntimeRegistry {
    pub(crate) fn resolve_scope(
        &self,
        scope: &SessionRelationScope,
        session_store_root: &Path,
    ) -> Result<Arc<GraphDb>, EmbeddedGraphRuntimeError> {
        let store_root = session_store_root.canonicalize().map_err(|error| {
            EmbeddedGraphRuntimeError::Unavailable(format!(
                "canonical session store {} is unavailable: {error}",
                session_store_root.display()
            ))
        })?;
        let graph_path = persistent_session_relation_graph_path(&store_root);
        let graph_directory = graph_path.parent().ok_or_else(|| {
            EmbeddedGraphRuntimeError::Unavailable(
                "embedded graph path has no storage directory".to_owned(),
            )
        })?;
        std::fs::create_dir_all(&graph_directory).map_err(|error| {
            EmbeddedGraphRuntimeError::Unavailable(format!(
                "embedded graph directory {} is unavailable: {error}",
                graph_directory.display()
            ))
        })?;
        let mut mounted = self.mounted.lock().map_err(|_| {
            EmbeddedGraphRuntimeError::Unavailable(
                "embedded graph registry lock is poisoned".to_owned(),
            )
        })?;
        if let Some(existing) = mounted.get(scope) {
            return if existing.store_root == store_root && existing.graph_path == graph_path {
                Ok(Arc::clone(&existing.database))
            } else {
                Err(EmbeddedGraphRuntimeError::IdentityConflict)
            };
        }
        if mounted
            .values()
            .any(|existing| existing.graph_path == graph_path)
        {
            return Err(EmbeddedGraphRuntimeError::IdentityConflict);
        }
        let database = open_persistent_session_relation_graph(graph_path.clone())?;
        mounted.insert(
            scope.clone(),
            MountedProjectGraph {
                store_root,
                graph_path,
                database: Arc::clone(&database),
            },
        );
        Ok(database)
    }
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;
    use tracedecay_domain::{ProjectId, UserProfileId};

    use super::*;

    #[test]
    fn project_and_profile_mounts_require_distinct_exact_store_roots() {
        let temporary = TempDir::new().expect("temporary mount root");
        let project_root = temporary.path().join("project-sessions");
        let profile_root = temporary.path().join("profile-sessions");
        std::fs::create_dir_all(&project_root).expect("project root");
        std::fs::create_dir_all(&profile_root).expect("profile root");
        let project_scope = SessionRelationScope::project(
            ProjectId::new("project.mount-isolation").expect("project id"),
        );
        let profile_scope = SessionRelationScope::profile(
            UserProfileId::new("profile.mount-isolation").expect("profile id"),
        );
        let registry = EmbeddedGraphRuntimeRegistry::default();

        let project = registry
            .resolve_scope(&project_scope, &project_root)
            .expect("project mount");
        assert!(matches!(
            registry.resolve_scope(&profile_scope, &project_root),
            Err(EmbeddedGraphRuntimeError::IdentityConflict)
        ));
        let profile = registry
            .resolve_scope(&profile_scope, &profile_root)
            .expect("profile mount");
        assert!(!Arc::ptr_eq(&project, &profile));
    }
}
