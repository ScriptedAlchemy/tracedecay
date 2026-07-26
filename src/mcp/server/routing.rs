//! Per-connection project routing: initialize-roots resolution and the
//! connection-scoped route state threaded through request dispatch.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::global_db::RegisteredGlobalDb;
use crate::mcp::project_route::HookProjectRouteCache;

/// Per-connection routing and identity context, constructed once per client
/// connection (or per initialize-replay dispatch) and threaded through
/// [`McpServer::handle_request_for_connection`]. Bundling these values keeps
/// persisted application request correlation and cancellation scoped to the
/// exact client connection.
pub(crate) struct ConnectionRouteState {
    implicit_project_path: Option<PathBuf>,
    /// Connection-scoped prefix (`{mcp_instance_id}-c{seq}`) that widens
    /// client-chosen, connection-local envelope ids into store-unique
    /// application request identities.
    memory_request_scope: String,
    /// Hook workspace routing cache, refreshed per request.
    pub(crate) route_cache: HookProjectRouteCache,
}

impl ConnectionRouteState {
    pub(crate) fn new(memory_request_scope: String, route_cache: HookProjectRouteCache) -> Self {
        Self {
            implicit_project_path: None,
            memory_request_scope,
            route_cache,
        }
    }

    pub(crate) async fn observe_initialize(
        &mut self,
        params: Option<&Value>,
        registry_db: Option<&RegisteredGlobalDb>,
    ) {
        self.implicit_project_path =
            resolve_initialize_roots_project_path(params, registry_db).await;
    }

    pub(crate) fn implicit_project_path(&self) -> Option<&Path> {
        self.implicit_project_path.as_deref()
    }

    pub(crate) fn memory_request_scope(&self) -> &str {
        &self.memory_request_scope
    }
}

pub(crate) async fn resolve_initialize_roots_project_path(
    params: Option<&Value>,
    registry_db: Option<&RegisteredGlobalDb>,
) -> Option<PathBuf> {
    let roots = initialize_root_paths(params);
    if roots.is_empty() {
        return None;
    }
    let registry_db = registry_db?;
    let projects = registry_db.list_code_projects(usize::MAX).await.ok()?;
    for root in roots {
        if let Some(project_path) = match_initialize_root_to_registered_project(&root, &projects) {
            return Some(project_path);
        }
    }
    None
}

pub(crate) fn initialize_root_paths(params: Option<&Value>) -> Vec<PathBuf> {
    params
        .and_then(|p| p.get("roots"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|root| {
            let uri = root.get("uri").and_then(Value::as_str)?;
            crate::serve::local_path_from_mcp_root_uri(uri)
        })
        .collect()
}

fn match_initialize_root_to_registered_project(
    root: &Path,
    projects: &[crate::global_db::CodeProjectRecord],
) -> Option<PathBuf> {
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let mut matches: Vec<_> = projects
        .iter()
        .filter_map(|project| {
            let project_path = PathBuf::from(&project.canonical_root);
            let project_path = project_path
                .canonicalize()
                .unwrap_or_else(|_| project_path.clone());
            (root == project_path || root.starts_with(&project_path))
                .then(|| (project_path.components().count(), project_path))
        })
        .collect();
    matches.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    matches.into_iter().map(|(_, path)| path).next()
}
