//! Daemon ownership for the one embedded graph store in each exact project shard.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use thiserror::Error;
use tracedecay_domain::ProjectId;
use tracedecay_graph_db::{
    GraphDb, GraphDbError, GraphDbLocation, GraphDbOpenOptions, GraphDurability,
    GraphFormatVersion, NeverCancelled,
};

const GRAPH_DIRECTORY: &str = "graph";
const GRAPH_FILE: &str = "graph.grafeo";
const GRAPH_FORMAT_VERSION: u32 = 2;

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub(in crate::daemon) enum EmbeddedGraphRuntimeError {
    #[error("embedded graph owner identity is invalid: {0}")]
    InvalidIdentity(String),
    #[error("embedded graph owner identity conflicts with an existing mount")]
    IdentityConflict,
    #[error("embedded graph store requires reset: {0}")]
    ResetRequired(String),
    #[error("embedded graph store is corrupt: {0}")]
    Corrupt(String),
    #[error("embedded graph store is unavailable: {0}")]
    Unavailable(String),
    #[error("embedded graph store durability is uncertain: {0}")]
    DurabilityUncertain(String),
    #[error("embedded graph store is closed")]
    Closed,
}

impl From<GraphDbError> for EmbeddedGraphRuntimeError {
    fn from(error: GraphDbError) -> Self {
        match error {
            GraphDbError::ResetRequired { message } => Self::ResetRequired(message),
            GraphDbError::Corrupt { message } => Self::Corrupt(message),
            GraphDbError::Unavailable { message } => Self::Unavailable(message),
            GraphDbError::DurabilityUncertain { message } => Self::DurabilityUncertain(message),
            GraphDbError::Closed => Self::Closed,
            GraphDbError::Cancelled => {
                Self::Unavailable("embedded graph open was cancelled".to_owned())
            }
            GraphDbError::InvalidRequest { message } => Self::InvalidIdentity(message),
            GraphDbError::Conflict => Self::IdentityConflict,
            GraphDbError::BudgetExhausted => {
                Self::Unavailable("embedded graph open budget was exhausted".to_owned())
            }
        }
    }
}

#[derive(Clone)]
struct MountedProjectGraph {
    owner_root: PathBuf,
    graph_path: PathBuf,
    database: Arc<GraphDb>,
    git_convergence: Option<Arc<crate::graph::git::GitTopologyConvergenceOwner>>,
}

#[derive(Clone, Default)]
pub(in crate::daemon) struct EmbeddedGraphRuntimeRegistry {
    mounted: Arc<Mutex<BTreeMap<ProjectId, MountedProjectGraph>>>,
}

impl std::fmt::Debug for EmbeddedGraphRuntimeRegistry {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.mounted.lock() {
            Ok(mounted) => formatter
                .debug_struct("EmbeddedGraphRuntimeRegistry")
                .field("mounted_projects", &mounted.len())
                .finish_non_exhaustive(),
            Err(_) => formatter
                .debug_struct("EmbeddedGraphRuntimeRegistry")
                .field("state", &"unavailable")
                .finish_non_exhaustive(),
        }
    }
}

impl EmbeddedGraphRuntimeRegistry {
    /// Resolve an already-mounted exact project handle without opening a store.
    /// Maintenance and Doctor use this read-only lookup so they cannot create a
    /// second runtime or turn observation into lifecycle mutation.
    pub(in crate::daemon) fn mounted_project(
        &self,
        project_id: &ProjectId,
        project_store_root: &Path,
    ) -> Result<Option<Arc<GraphDb>>, EmbeddedGraphRuntimeError> {
        project_id
            .validate()
            .map_err(|error| EmbeddedGraphRuntimeError::InvalidIdentity(error.to_string()))?;
        let owner_root = project_store_root.canonicalize().map_err(|error| {
            EmbeddedGraphRuntimeError::Unavailable(format!(
                "canonical project store {} is unavailable: {error}",
                project_store_root.display()
            ))
        })?;
        let graph_path = owner_root.join(GRAPH_DIRECTORY).join(GRAPH_FILE);
        let mounted = self.mounted.lock().map_err(|_| {
            EmbeddedGraphRuntimeError::Unavailable(
                "embedded graph registry lock is poisoned".to_owned(),
            )
        })?;
        let Some(existing) = mounted.get(project_id) else {
            if mounted
                .values()
                .any(|existing| existing.graph_path == graph_path)
            {
                return Err(EmbeddedGraphRuntimeError::IdentityConflict);
            }
            return Ok(None);
        };
        if existing.owner_root != owner_root || existing.graph_path != graph_path {
            return Err(EmbeddedGraphRuntimeError::IdentityConflict);
        }
        Ok(Some(Arc::clone(&existing.database)))
    }

    pub(in crate::daemon) fn resolve_project(
        &self,
        project_id: &ProjectId,
        project_store_root: &Path,
    ) -> Result<Arc<GraphDb>, EmbeddedGraphRuntimeError> {
        project_id
            .validate()
            .map_err(|error| EmbeddedGraphRuntimeError::InvalidIdentity(error.to_string()))?;
        let owner_root = project_store_root.canonicalize().map_err(|error| {
            EmbeddedGraphRuntimeError::Unavailable(format!(
                "canonical project store {} is unavailable: {error}",
                project_store_root.display()
            ))
        })?;
        let graph_directory = owner_root.join(GRAPH_DIRECTORY);
        std::fs::create_dir_all(&graph_directory).map_err(|error| {
            EmbeddedGraphRuntimeError::Unavailable(format!(
                "embedded graph directory {} is unavailable: {error}",
                graph_directory.display()
            ))
        })?;
        let graph_directory = graph_directory.canonicalize().map_err(|error| {
            EmbeddedGraphRuntimeError::Unavailable(format!(
                "embedded graph directory {} has no canonical identity: {error}",
                graph_directory.display()
            ))
        })?;
        let graph_path = graph_directory.join(GRAPH_FILE);

        let mut mounted = self.mounted.lock().map_err(|_| {
            EmbeddedGraphRuntimeError::Unavailable(
                "embedded graph registry lock is poisoned".to_owned(),
            )
        })?;
        if let Some(existing) = mounted.get(project_id) {
            return if existing.owner_root == owner_root && existing.graph_path == graph_path {
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
            project_id.clone(),
            MountedProjectGraph {
                owner_root,
                graph_path,
                database: Arc::clone(&database),
                git_convergence: None,
            },
        );
        Ok(database)
    }

    pub(in crate::daemon) async fn enqueue_git_convergence(
        &self,
        project_id: &ProjectId,
        project_store_root: &Path,
        project_root: &Path,
        database: Arc<GraphDb>,
    ) -> Result<(), EmbeddedGraphRuntimeError> {
        let owner_root = project_store_root.canonicalize().map_err(|error| {
            EmbeddedGraphRuntimeError::Unavailable(format!(
                "canonical project store {} is unavailable: {error}",
                project_store_root.display()
            ))
        })?;
        let project_root = project_root.canonicalize().map_err(|error| {
            EmbeddedGraphRuntimeError::Unavailable(format!(
                "canonical project root {} is unavailable: {error}",
                project_root.display()
            ))
        })?;
        let owner = {
            let mut mounted = self.mounted.lock().map_err(|_| {
                EmbeddedGraphRuntimeError::Unavailable(
                    "embedded graph registry lock is poisoned".to_owned(),
                )
            })?;
            let entry = mounted
                .get_mut(project_id)
                .ok_or(EmbeddedGraphRuntimeError::IdentityConflict)?;
            if entry.owner_root != owner_root || !Arc::ptr_eq(&entry.database, &database) {
                return Err(EmbeddedGraphRuntimeError::IdentityConflict);
            }
            Arc::clone(entry.git_convergence.get_or_insert_with(|| {
                crate::graph::git::GitTopologyConvergenceOwner::start(project_id.clone(), database)
            }))
        };
        owner
            .wake(&project_root)
            .await
            .map_err(|error| EmbeddedGraphRuntimeError::Unavailable(error.to_string()))?;
        Ok(())
    }

    pub(in crate::daemon) async fn enqueue_git_evidence(
        &self,
        project_id: &ProjectId,
        project_store_root: &Path,
        project_root: &Path,
        database: Arc<GraphDb>,
        intent: tracedecay_domain::GitGraphEvidenceIntent,
        sink: Arc<dyn crate::graph::git::GitEvidenceReceiptSink>,
    ) -> Result<(), EmbeddedGraphRuntimeError> {
        let owner_root = project_store_root.canonicalize().map_err(|error| {
            EmbeddedGraphRuntimeError::Unavailable(format!(
                "canonical project store {} is unavailable: {error}",
                project_store_root.display()
            ))
        })?;
        let project_root = project_root.canonicalize().map_err(|error| {
            EmbeddedGraphRuntimeError::Unavailable(format!(
                "canonical project root {} is unavailable: {error}",
                project_root.display()
            ))
        })?;
        let owner = {
            let mut mounted = self.mounted.lock().map_err(|_| {
                EmbeddedGraphRuntimeError::Unavailable(
                    "embedded graph registry lock is poisoned".to_owned(),
                )
            })?;
            let entry = mounted
                .get_mut(project_id)
                .ok_or(EmbeddedGraphRuntimeError::IdentityConflict)?;
            if entry.owner_root != owner_root || !Arc::ptr_eq(&entry.database, &database) {
                return Err(EmbeddedGraphRuntimeError::IdentityConflict);
            }
            Arc::clone(entry.git_convergence.get_or_insert_with(|| {
                crate::graph::git::GitTopologyConvergenceOwner::start(project_id.clone(), database)
            }))
        };
        owner
            .enqueue_evidence(&project_root, intent, sink)
            .await
            .map_err(|error| EmbeddedGraphRuntimeError::Unavailable(error.to_string()))
    }

    pub(in crate::daemon) async fn close_all(&self) -> Vec<EmbeddedGraphRuntimeError> {
        let mounted = match self.mounted.lock() {
            Ok(mut mounted) => std::mem::take(&mut *mounted),
            Err(_) => {
                return vec![EmbeddedGraphRuntimeError::Unavailable(
                    "embedded graph registry lock is poisoned".to_owned(),
                )];
            }
        };
        let mut errors = Vec::new();
        for entry in mounted.into_values() {
            if let Some(owner) = entry.git_convergence
                && let Err(error) = owner.shutdown().await
            {
                errors.push(EmbeddedGraphRuntimeError::Unavailable(format!(
                    "Git topology convergence did not drain: {error}"
                )));
            }
            if let Err(error) = entry.database.close() {
                errors.push(error.into());
            }
        }
        errors
    }

    #[cfg(test)]
    fn graph_path_for(&self, project_id: &ProjectId) -> Option<PathBuf> {
        self.mounted
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(project_id)
            .map(|entry| entry.graph_path.clone())
    }

    #[cfg(test)]
    fn git_owner_for(
        &self,
        project_id: &ProjectId,
    ) -> Option<Arc<crate::graph::git::GitTopologyConvergenceOwner>> {
        self.mounted
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(project_id)
            .and_then(|entry| entry.git_convergence.as_ref().map(Arc::clone))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn exact_project_owner_reuses_one_neutral_durable_handle() {
        let store = tempfile::tempdir().expect("project store");
        let project_id = ProjectId::new("project.graph-runtime").expect("project id");
        let registry = EmbeddedGraphRuntimeRegistry::default();

        let first = registry
            .resolve_project(&project_id, store.path())
            .expect("first graph mount");
        let second = registry
            .resolve_project(&project_id, store.path())
            .expect("second graph mount");

        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(
            registry.graph_path_for(&project_id),
            Some(store.path().join("graph/graph.grafeo"))
        );
        assert!(store.path().join("graph/graph.grafeo").is_file());
        assert!(registry.close_all().await.is_empty());
    }

    #[tokio::test]
    async fn one_project_identity_cannot_alias_another_owner_shard() {
        let first_store = tempfile::tempdir().expect("first project store");
        let second_store = tempfile::tempdir().expect("second project store");
        let project_id = ProjectId::new("project.graph-runtime").expect("project id");
        let registry = EmbeddedGraphRuntimeRegistry::default();
        registry
            .resolve_project(&project_id, first_store.path())
            .expect("first graph mount");

        assert_eq!(
            registry
                .resolve_project(&project_id, second_store.path())
                .unwrap_err(),
            EmbeddedGraphRuntimeError::IdentityConflict
        );
        assert!(registry.close_all().await.is_empty());
    }

    #[tokio::test]
    async fn mounted_lookup_never_opens_and_preserves_exact_owner_identity() {
        let store = tempfile::tempdir().expect("project store");
        let other = tempfile::tempdir().expect("other project store");
        let project_id = ProjectId::new("project.graph-runtime").expect("project id");
        let registry = EmbeddedGraphRuntimeRegistry::default();

        assert!(
            registry
                .mounted_project(&project_id, store.path())
                .expect("unmounted lookup")
                .is_none()
        );
        assert!(!store.path().join("graph").exists());

        let opened = registry
            .resolve_project(&project_id, store.path())
            .expect("graph mount");
        let observed = registry
            .mounted_project(&project_id, store.path())
            .expect("mounted lookup")
            .expect("mounted graph");
        assert!(Arc::ptr_eq(&opened, &observed));
        assert_eq!(
            registry
                .mounted_project(&project_id, other.path())
                .unwrap_err(),
            EmbeddedGraphRuntimeError::IdentityConflict
        );
        assert!(registry.close_all().await.is_empty());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_enqueues_install_one_project_convergence_owner() {
        let store = tempfile::tempdir().expect("project store");
        let root = tempfile::tempdir().expect("worktree root");
        let project_id = ProjectId::new("project.graph-runtime.concurrent").expect("project id");
        let registry = EmbeddedGraphRuntimeRegistry::default();

        let mut joins = tokio::task::JoinSet::new();
        for _ in 0..100 {
            let registry = registry.clone();
            let project_id = project_id.clone();
            let store_root = store.path().to_path_buf();
            let worktree_root = root.path().to_path_buf();
            joins.spawn(async move {
                let database = registry
                    .resolve_project(&project_id, &store_root)
                    .expect("shared graph");
                registry
                    .enqueue_git_convergence(&project_id, &store_root, &worktree_root, database)
                    .await
                    .expect("coalesced enqueue");
            });
        }
        while let Some(result) = joins.join_next().await {
            result.expect("enqueue task");
        }

        let first = registry
            .git_owner_for(&project_id)
            .expect("installed owner");
        let second = registry.git_owner_for(&project_id).expect("same owner");
        assert!(Arc::ptr_eq(&first, &second));
        assert!(registry.close_all().await.is_empty());
    }
}
