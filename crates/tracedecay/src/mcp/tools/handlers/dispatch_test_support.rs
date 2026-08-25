use std::collections::BTreeSet;
use std::ffi::{OsStr, OsString};
use std::path::Path;
use std::sync::Arc;

use super::*;
use crate::config::USER_DATA_DIR_ENV;

#[derive(Clone)]
struct FixtureCodeGraphProjection {
    scope: tracedecay_application::ResolvedScope,
    store: Arc<tracedecay_code_index::graph_projection::CodeGraphProjectionStore>,
}

#[derive(Clone)]
struct FailingFixtureCodeGraphProjection {
    error: tracedecay_usecases::graph::CodeGraphReadError,
}

impl tracedecay_usecases::graph::CodeGraphProjectionReadPort for FailingFixtureCodeGraphProjection {
    fn open<'a>(
        &'a self,
        _request: tracedecay_usecases::graph::CodeGraphReadRequest<'a>,
    ) -> tracedecay_usecases::graph::CodeGraphReadFuture<'a> {
        let error = self.error.clone();
        Box::pin(async move { Err(error) })
    }
}

impl tracedecay_usecases::graph::CodeGraphProjectionReadPort for FixtureCodeGraphProjection {
    fn open<'a>(
        &'a self,
        request: tracedecay_usecases::graph::CodeGraphReadRequest<'a>,
    ) -> tracedecay_usecases::graph::CodeGraphReadFuture<'a> {
        Box::pin(async move {
            if request.cancellation.is_cancelled() {
                return Err(tracedecay_usecases::graph::CodeGraphReadError::Cancelled);
            }
            if request.context.scope() != &self.scope {
                return Err(tracedecay_usecases::graph::CodeGraphReadError::Denied);
            }
            tracedecay_usecases::graph::VerifiedCodeGraphRead::new(
                self.scope.clone(),
                Arc::clone(&self.store),
            )
        })
    }
}

#[derive(Clone)]
struct FixtureCodeGraphAdmission {
    scope: tracedecay_application::ResolvedScope,
}

impl tracedecay_usecases::graph::CodeGraphReadAdmissionPort for FixtureCodeGraphAdmission {
    fn admit<'a>(
        &'a self,
        request: tracedecay_usecases::graph::CodeGraphReadAdmissionRequest<'a>,
    ) -> tracedecay_usecases::graph::CodeGraphReadAdmissionFuture<'a> {
        Box::pin(async move {
            if request.cancellation.is_cancelled() {
                return Err(tracedecay_usecases::graph::CodeGraphReadError::Cancelled);
            }
            if request.deadline.is_elapsed_at(request.observed_at) {
                return Err(tracedecay_usecases::graph::CodeGraphReadError::TimedOut);
            }
            let actor = tracedecay_domain::ActorId::new("actor.mcp-verified-graph-fixture")
                .expect("graph fixture actor");
            let grant = tracedecay_application::CapabilityGrantSnapshot::new(
                tracedecay_application::CapabilityGrantId::new("grant.mcp-verified-graph-fixture")
                    .expect("graph fixture grant identity"),
                1,
                tracedecay_domain::ManifestDigest::new(format!("sha256:{}", "a".repeat(64)))
                    .expect("graph fixture grant digest"),
                actor.clone(),
                request.observed_at,
                request.deadline.expires_at,
                self.scope.clone(),
                BTreeSet::from([request.operation.capability_id().clone()]),
                BTreeSet::from([request.operation.use_case_id().clone()]),
                tracedecay_application::DisclosureClass::Evidence,
            )
            .map_err(|error| {
                tracedecay_usecases::graph::CodeGraphReadError::InvalidRequest {
                    detail: error.to_string(),
                }
            })?;
            tracedecay_application::RequestContext::new(
                actor,
                self.scope.clone(),
                grant,
                request.request_id,
                request.deadline,
                request.cancellation.context(),
            )
            .map_err(|error| {
                tracedecay_usecases::graph::CodeGraphReadError::InvalidRequest {
                    detail: error.to_string(),
                }
            })
        })
    }
}

pub(super) fn verified_graph_options<'a>(
    cg: &TraceDecay,
    mut options: ToolCallRegistryOptions<'a>,
) -> ToolCallRegistryOptions<'a> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("graph fixture clock")
        .as_micros() as i64;
    if options.application_request_id.is_none() {
        options.application_request_id = Some(
            tracedecay_application::RequestId::new("request.mcp-verified-graph-fixture")
                .expect("graph fixture request identity"),
        );
    }
    if options.application_deadline.is_none() {
        options.application_deadline = Some(
            tracedecay_application::Deadline::new(tracedecay_domain::UtcMicros(
                now.saturating_add(30_000_000),
            ))
            .expect("graph fixture deadline"),
        );
    }
    if options.application_cancellation.is_none() {
        options.application_cancellation = Some(
            tracedecay_application::CancellationSignal::active("cancel.mcp-verified-graph-request")
                .expect("graph fixture request cancellation"),
        );
    }
    let project_id = cg
        .store_layout()
        .identity
        .project_id
        .as_deref()
        .and_then(|value| tracedecay_domain::ProjectId::new(value.to_owned()).ok())
        .expect("registered graph fixture project identity");
    let scope = crate::daemon::project_open_owners::resolved_scope_for_project(
        cg.project_root(),
        &project_id,
    )
    .expect("registered graph fixture scope");
    let cancellation =
        tracedecay_application::CancellationSignal::active("cancel.mcp-verified-graph-fixture")
            .expect("graph fixture cancellation");
    let projection =
        tracedecay_code_index::graph_projection::HermeticCodeGraphProjectionStore::memory(
            &cancellation,
        )
        .expect("graph fixture projection");
    let generation =
        tracedecay_domain::CodeGenerationId::new("generation.mcp-verified-graph-fixture.1")
            .expect("graph fixture generation");
    projection
        .publish_with_cancellation(
            &generation,
            &[],
            &[],
            Arc::new(tracedecay_graph_db::NeverCancelled),
        )
        .expect("publish graph fixture generation");
    let store = Arc::new(
        projection
            .verified_store(&generation)
            .expect("open graph fixture generation"),
    );
    options.code_graph_projection_read_port = Some(Arc::new(FixtureCodeGraphProjection {
        scope: scope.clone(),
        store,
    }));
    options.code_graph_read_admission_port = Some(Arc::new(FixtureCodeGraphAdmission { scope }));
    options
}

pub(super) fn verified_graph_error_options<'a>(
    cg: &TraceDecay,
    options: ToolCallRegistryOptions<'a>,
    error: tracedecay_usecases::graph::CodeGraphReadError,
) -> ToolCallRegistryOptions<'a> {
    let mut options = verified_graph_options(cg, options);
    options.code_graph_projection_read_port =
        Some(Arc::new(FailingFixtureCodeGraphProjection { error }));
    options
}

/// A second registered project fixture mounted through the caller's existing
/// test runtime. The profile session-relation graph has exactly one writer,
/// so multi-project tests must mount sibling projects through the first
/// runtime's daemon session registry instead of constructing another runtime
/// on the same profile.
pub(super) async fn init_sibling_registered_fixture(
    runtime: &crate::host_admission::HostAdmissionTestRuntimeV1,
    project_root: &Path,
    project_id: &str,
) -> (
    TraceDecay,
    Arc<crate::host_admission::HostAdmissionTestRuntimeV1>,
) {
    let profile_root = crate::storage::default_profile_root().expect("sibling profile root");
    let project_id =
        tracedecay_domain::ProjectId::new(project_id).expect("typed sibling project identity");
    let sibling = Arc::new(
        runtime
            .sibling_project(project_root, project_id)
            .await
            .expect("sibling registered runtime"),
    );
    let graph = sibling
        .initialize_project_graph_for_test(
            project_root,
            crate::tracedecay::TraceDecayOpenOptions {
                profile_root: Some(profile_root),
                global_db_path: None,
            },
        )
        .await
        .expect("sibling project graph");
    (graph, sibling)
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
        McpToolDispatchGroup::ApplicationSurface
        | McpToolDispatchGroup::RetainedApplication
        | McpToolDispatchGroup::Work
        | McpToolDispatchGroup::Workflow => false,
        McpToolDispatchGroup::MultiRoot => {
            owned(handle_multi_root(tool_name, invalid_args, None, None, None, None).await)
        }
        McpToolDispatchGroup::Graph => {
            owned(dispatch_graph_tools(tool_name, cg, invalid_args, None, options).await)
        }
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
