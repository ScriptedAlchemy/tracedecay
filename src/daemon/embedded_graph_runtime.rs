//! Daemon ownership for the one native Grafeo store mounted per project shard.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use thiserror::Error;
use tracedecay_global_db::session_temporal::relations::SessionRelationScope;
use tracedecay_graph_db::{
    GraphDb, GraphDbError, GraphDbLocation, GraphDbOpenOptions, GraphDurability,
    GraphFormatVersion, NeverCancelled,
};

const GRAPH_DIRECTORY: &str = "graph";
const GRAPH_FILE: &str = "graph.grafeo";
const GRAPH_FORMAT_VERSION: u32 = 2;

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub(super) enum EmbeddedGraphRuntimeError {
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
pub(super) struct EmbeddedGraphRuntimeRegistry {
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
    pub(super) fn resolve_scope(
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
        let graph_directory = store_root.join(GRAPH_DIRECTORY);
        std::fs::create_dir_all(&graph_directory).map_err(|error| {
            EmbeddedGraphRuntimeError::Unavailable(format!(
                "embedded graph directory {} is unavailable: {error}",
                graph_directory.display()
            ))
        })?;
        let graph_path = graph_directory.join(GRAPH_FILE);
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
        let database = Arc::new(GraphDb::open(GraphDbOpenOptions {
            location: GraphDbLocation::Persistent(graph_path.clone()),
            expected_format: GraphFormatVersion::new(GRAPH_FORMAT_VERSION)?,
            durability: GraphDurability::Sync,
            cancellation: Arc::new(NeverCancelled),
        })?);
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
