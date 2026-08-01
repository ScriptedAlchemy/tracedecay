use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::path::Path;
use std::sync::Arc;

use super::*;
use crate::config::USER_DATA_DIR_ENV;
use crate::daemon::store_runtime::session_registry::DaemonSessionRuntimeRegistryV1;

pub(super) struct SelectorRegistry {
    database: Arc<RegisteredGlobalDb>,
    _registry: DaemonSessionRuntimeRegistryV1,
    _scope: crate::db::DaemonDatabaseScope,
}

impl SelectorRegistry {
    pub(super) async fn open() -> Self {
        let profile_root = crate::config::user_data_dir().expect("selector profile root");
        let identity = crate::daemon::profile_identity::load_or_create(&profile_root)
            .expect("selector profile identity");
        let scope = crate::db::enter_daemon_database_scope(
            identity.profile_root(),
            1,
            "host-admission-test-runtime",
        )
        .expect("selector daemon database scope");
        let registry = DaemonSessionRuntimeRegistryV1::open(identity)
            .await
            .expect("selector session runtime registry");
        let database = registry
            .profile_database()
            .await
            .expect("selector registered profile database");
        Self {
            database,
            _registry: registry,
            _scope: scope,
        }
    }

    pub(super) fn database(&self) -> &Arc<RegisteredGlobalDb> {
        &self.database
    }
}

pub(super) fn selector_options(
    registry: &SelectorRegistry,
    graphs: Vec<Arc<TraceDecay>>,
) -> ToolCallRegistryOptions<'_> {
    let graphs = Arc::new(
        graphs
            .into_iter()
            .map(|graph| (graph.project_root().to_path_buf(), graph))
            .collect::<BTreeMap<_, _>>(),
    );
    let resolver: crate::mcp::server::RetainedProjectGraphResolver = Arc::new(move |request| {
        let graph = graphs.get(&request.registered_root).cloned();
        Box::pin(async move { Ok(graph) })
    });
    ToolCallRegistryOptions {
        global_db: Some(registry.database()),
        retained_project_graph_resolver: Some(resolver),
        ..Default::default()
    }
}

struct EnvVarGuard {
    key: &'static str,
    previous: Option<OsString>,
}

impl EnvVarGuard {
    fn set(key: &'static str, value: impl AsRef<OsStr>) -> Self {
        let previous = std::env::var_os(key);
        unsafe {
            std::env::set_var(key, value);
        }
        Self { key, previous }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        unsafe {
            if let Some(previous) = self.previous.take() {
                std::env::set_var(self.key, previous);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }
}

pub(super) struct SelectorEnv {
    _home: EnvVarGuard,
    _userprofile: EnvVarGuard,
    _data_dir: EnvVarGuard,
    _global_db: EnvVarGuard,
}

impl SelectorEnv {
    pub(super) fn new(root: &Path) -> Self {
        let home = root.join("home");
        let profile_root = home.join(".tracedecay");
        crate::storage::PrivateStoreIo::create_dir_all(&profile_root).unwrap();
        let home = home.canonicalize().unwrap();
        let profile_root = home.join(".tracedecay");
        let global_db_path = profile_root.join("global.db");
        Self {
            _home: EnvVarGuard::set("HOME", &home),
            _userprofile: EnvVarGuard::set("USERPROFILE", &home),
            _data_dir: EnvVarGuard::set(USER_DATA_DIR_ENV, &profile_root),
            _global_db: EnvVarGuard::set("TRACEDECAY_GLOBAL_DB", &global_db_path),
        }
    }
}

pub(super) async fn concrete_dispatch_group_accepts(
    group: McpToolDispatchGroup,
    tool_name: &str,
    cg: &TraceDecay,
    options: ToolCallRegistryOptions<'_>,
) -> bool {
    let invalid_args = Value::String("dispatch-metadata-probe".to_owned());
    // The probe args are deliberately invalid, so an accepted tool still fails —
    // just not with the sentinel every group returns for a name it does not own.
    let owned = |result: Result<ToolResult>| {
        !matches!(
            &result,
            Err(TraceDecayError::Config { message })
                if message == &format!("unknown tool: {tool_name}")
        )
    };
    match group {
        McpToolDispatchGroup::ApplicationSurface | McpToolDispatchGroup::RetainedApplication => {
            false
        }
        McpToolDispatchGroup::Graph => owned(
            dispatch_graph_tools(tool_name, cg, invalid_args, None, None, None, None, None).await,
        ),
        McpToolDispatchGroup::Info => owned(
            dispatch_info_tools(tool_name, cg, invalid_args, None, None, None, None, options).await,
        ),
        McpToolDispatchGroup::Admin => {
            owned(dispatch_admin_tools(tool_name, cg, invalid_args, options).await)
        }
        McpToolDispatchGroup::Analysis => {
            owned(dispatch_analysis_tools(tool_name, cg, invalid_args, None, None, options).await)
        }
        McpToolDispatchGroup::Git => {
            owned(dispatch_git_tools(tool_name, cg, invalid_args, options).await)
        }
        McpToolDispatchGroup::Edit => {
            owned(dispatch_edit_tools(tool_name, cg, invalid_args, options).await)
        }
        McpToolDispatchGroup::Health => {
            owned(dispatch_health_tools(tool_name, cg, invalid_args, None, None, options).await)
        }
        McpToolDispatchGroup::Memory => {
            owned(dispatch_memory_tools(tool_name, cg, invalid_args, options).await)
        }
        McpToolDispatchGroup::SessionWorkflow => {
            owned(dispatch_session_workflow_tools(tool_name, cg, invalid_args, options).await)
        }
    }
}
