use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

use super::*;

#[derive(Clone)]
struct FixtureCodeGraphProjection {
    scope: tracedecay_contracts::ResolvedScope,
    store: Arc<tracedecay_code_index::graph_projection::CodeGraphProjectionStore>,
    freshness: tracedecay_graph_query::CodeGraphReadFreshnessV1,
}

#[derive(Clone)]
struct FailingFixtureCodeGraphProjection {
    error: tracedecay_graph_query::CodeGraphReadError,
}

impl tracedecay_graph_query::CodeGraphProjectionReadPort for FailingFixtureCodeGraphProjection {
    fn open<'a>(
        &'a self,
        _request: tracedecay_graph_query::CodeGraphReadRequest<'a>,
    ) -> tracedecay_graph_query::CodeGraphReadFuture<'a> {
        let error = self.error.clone();
        Box::pin(async move { Err(error) })
    }
}

impl tracedecay_graph_query::CodeGraphProjectionReadPort for FixtureCodeGraphProjection {
    fn open<'a>(
        &'a self,
        request: tracedecay_graph_query::CodeGraphReadRequest<'a>,
    ) -> tracedecay_graph_query::CodeGraphReadFuture<'a> {
        Box::pin(async move {
            if request.cancellation.is_cancelled() {
                return Err(tracedecay_graph_query::CodeGraphReadError::Cancelled);
            }
            if request.context.scope() != &self.scope {
                return Err(tracedecay_graph_query::CodeGraphReadError::Denied);
            }
            tracedecay_graph_query::VerifiedCodeGraphRead::new(
                self.scope.clone(),
                Arc::clone(&self.store),
                self.freshness,
            )
        })
    }
}

#[derive(Clone)]
struct FixtureCodeGraphAdmission {
    scope: tracedecay_contracts::ResolvedScope,
}

impl tracedecay_graph_query::CodeGraphReadAdmissionPort for FixtureCodeGraphAdmission {
    fn admit<'a>(
        &'a self,
        request: tracedecay_graph_query::CodeGraphReadAdmissionRequest<'a>,
    ) -> tracedecay_graph_query::CodeGraphReadAdmissionFuture<'a> {
        Box::pin(async move {
            if request.cancellation.is_cancelled() {
                return Err(tracedecay_graph_query::CodeGraphReadError::Cancelled);
            }
            if request.deadline.is_elapsed_at(request.observed_at) {
                return Err(tracedecay_graph_query::CodeGraphReadError::TimedOut);
            }
            let actor = tracedecay_domain::ActorId::new("actor.mcp-verified-graph-fixture")
                .expect("graph fixture actor");
            let grant = tracedecay_contracts::CapabilityGrantSnapshot::new(
                tracedecay_contracts::CapabilityGrantId::new("grant.mcp-verified-graph-fixture")
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
                tracedecay_contracts::DisclosureClass::Evidence,
            )
            .map_err(|error| {
                tracedecay_graph_query::CodeGraphReadError::InvalidRequest {
                    detail: error.to_string(),
                }
            })?;
            tracedecay_contracts::RequestContext::new(
                actor,
                self.scope.clone(),
                grant,
                request.request_id,
                request.deadline,
                request.cancellation.context(),
            )
            .map_err(|error| {
                tracedecay_graph_query::CodeGraphReadError::InvalidRequest {
                    detail: error.to_string(),
                }
            })
        })
    }
}

pub(super) fn verified_graph_options<'a>(
    cg: &TraceDecay,
    options: ToolCallRegistryOptions<'a>,
) -> ToolCallRegistryOptions<'a> {
    verified_graph_options_with_freshness(
        cg,
        options,
        tracedecay_graph_query::CodeGraphReadFreshnessV1::Current,
    )
}

/// [`verified_graph_options`] whose projection open reports the last complete
/// generation serving through a rebuild window (a reconcile pass in flight),
/// for pinning the typed freshness trailer at the dispatch boundary.
pub(super) fn verified_graph_stale_options<'a>(
    cg: &TraceDecay,
    options: ToolCallRegistryOptions<'a>,
) -> ToolCallRegistryOptions<'a> {
    verified_graph_options_with_freshness(
        cg,
        options,
        tracedecay_graph_query::CodeGraphReadFreshnessV1::LastCompleteStale {
            sealed_at: fixture_sealed_at(),
            rebuild_in_flight: true,
        },
    )
}

/// [`verified_graph_stale_options`] with no source-moving rebuild proven. The
/// seat remains stale while source currency is unverified.
pub(super) fn verified_graph_wedged_options<'a>(
    cg: &TraceDecay,
    options: ToolCallRegistryOptions<'a>,
) -> ToolCallRegistryOptions<'a> {
    verified_graph_options_with_freshness(
        cg,
        options,
        tracedecay_graph_query::CodeGraphReadFreshnessV1::LastCompleteStale {
            sealed_at: fixture_sealed_at(),
            rebuild_in_flight: false,
        },
    )
}

fn fixture_sealed_at() -> tracedecay_domain::UtcMicros {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("graph fixture clock")
        .as_micros() as i64;
    tracedecay_domain::UtcMicros(now.saturating_sub(90_000_000))
}

fn verified_graph_options_with_freshness<'a>(
    cg: &TraceDecay,
    mut options: ToolCallRegistryOptions<'a>,
    freshness: tracedecay_graph_query::CodeGraphReadFreshnessV1,
) -> ToolCallRegistryOptions<'a> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("graph fixture clock")
        .as_micros() as i64;
    if options.application_request_id.is_none() {
        options.application_request_id = Some(
            tracedecay_contracts::RequestId::new("request.mcp-verified-graph-fixture")
                .expect("graph fixture request identity"),
        );
    }
    if options.application_deadline.is_none() {
        options.application_deadline = Some(
            tracedecay_contracts::Deadline::new(tracedecay_domain::UtcMicros(
                now.saturating_add(30_000_000),
            ))
            .expect("graph fixture deadline"),
        );
    }
    if options.application_cancellation.is_none() {
        options.application_cancellation = Some(
            tracedecay_contracts::CancellationSignal::active("cancel.mcp-verified-graph-request")
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
    let scope =
        tracedecay_code_index_runtime::resolved_scope_for_project(cg.project_root(), &project_id)
            .expect("registered graph fixture scope");
    let cancellation =
        tracedecay_contracts::CancellationSignal::active("cancel.mcp-verified-graph-fixture")
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
        freshness,
    }));
    options.admitted_project_scope = Some(scope.clone());
    options.code_graph_read_admission_port = Some(Arc::new(FixtureCodeGraphAdmission { scope }));
    options.verified_graph_query_port = Some(
        tracedecay_graph_query::admitted_verified_graph_query_port_with_source(
            options
                .code_graph_read_admission_port
                .clone()
                .expect("graph fixture admission"),
            options
                .code_graph_projection_read_port
                .clone()
                .expect("graph fixture projection"),
            Some(tracedecay_graph_query::SourceReadContext::new(
                cg.project_root().to_path_buf(),
                cg.db().clone(),
                cg.is_read_only(),
                project_id.as_str().to_owned(),
            )),
        ),
    );
    options
}

/// Graph-tool operations execute on the project's graph-tool owner, which
/// computes them under the owning server's admitted authorities and renders
/// the typed result; every other tool still dispatches through the MCP
/// handler table.
pub(super) async fn dispatch_on_graph_authority(
    cg: &TraceDecay,
    tool_name: &str,
    args: Value,
    options: ToolCallRegistryOptions<'_>,
) -> Result<ToolResult> {
    match ApplicationSurfaceOperation::from_tool_name(tool_name)
        .filter(|operation| operation.is_graph_tool())
    {
        Some(operation) => {
            let completion =
                super::compute_graph_tool_for_owner(cg, operation, args.clone(), None, options)
                    .await?;
            tracedecay_mcp::handlers::graph_tool::render_graph_tool(
                Some(&cg.store_layout().response_handle_root),
                &args,
                completion,
            )
        }
        None => {
            handle_tool_call_with_registry_options(cg, tool_name, args, None, None, options).await
        }
    }
}

pub(super) fn verified_graph_error_options<'a>(
    cg: &TraceDecay,
    options: ToolCallRegistryOptions<'a>,
    error: tracedecay_graph_query::CodeGraphReadError,
) -> ToolCallRegistryOptions<'a> {
    let mut options = verified_graph_options(cg, options);
    options.code_graph_projection_read_port =
        Some(Arc::new(FailingFixtureCodeGraphProjection { error }));
    options.verified_graph_query_port = Some(
        tracedecay_graph_query::admitted_verified_graph_query_port_with_source(
            options
                .code_graph_read_admission_port
                .clone()
                .expect("graph fixture admission"),
            options
                .code_graph_projection_read_port
                .clone()
                .expect("graph fixture projection"),
            None,
        ),
    );
    options
}

/// A second registered project fixture mounted through the caller's existing
/// test runtime. The profile session-relation graph has exactly one writer,
/// so multi-project tests must mount sibling projects through the first
/// runtime's daemon session registry instead of constructing another runtime
/// on the same profile.
pub(super) async fn init_sibling_registered_fixture(
    runtime: &tracedecay_project::test_support::host_admission::HostAdmissionTestRuntimeV1,
    project_root: &Path,
    project_id: &str,
) -> (
    TraceDecay,
    Arc<tracedecay_project::test_support::host_admission::HostAdmissionTestRuntimeV1>,
) {
    let profile_root = runtime.profile_root_for_test().to_path_buf();
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
            tracedecay_project::project::TraceDecayOpenOptions {
                profile_root: Some(profile_root),
                global_db_path: None,
            },
        )
        .await
        .expect("sibling project graph");
    (graph, sibling)
}

/// The isolated profile `<root>/home/.tracedecay` a dispatch fixture hands to
/// every API it drives.
pub(super) struct SelectorProfile {
    profile: tracedecay_runtime_core::config::ProfileRoot,
}

impl SelectorProfile {
    pub(super) fn new(root: &Path) -> Self {
        let home = root.join("home");
        tracedecay_runtime_core::storage::PrivateStoreIo::create_dir_all(
            &home.join(tracedecay_runtime_core::config::TRACEDECAY_DIR),
        )
        .unwrap();
        Self {
            profile: tracedecay_runtime_core::config::ProfileRoot::under_home(
                home.canonicalize().unwrap(),
            ),
        }
    }

    pub(super) fn profile(&self) -> &tracedecay_runtime_core::config::ProfileRoot {
        &self.profile
    }

    pub(super) fn data_dir(&self) -> &Path {
        self.profile.data_dir()
    }
}

pub(super) async fn concrete_dispatch_group_accepts(
    group: McpToolDispatchGroup,
    tool_name: &str,
    cg: &TraceDecay,
    options: ToolCallRegistryOptions<'_>,
) -> bool {
    let invalid_args = Value::String("dispatch-metadata-probe".to_owned());
    // The probe args are deliberately invalid, so an accepted tool still fails,
    // just not with the sentinel every group returns for a name it does not own.
    let owned = |result: Result<ToolResult>| {
        !matches!(
            &result,
            Err(TraceDecayError::Config { message })
                if message == &format!("unknown tool: {tool_name}")
        )
    };
    match group {
        // The daemon serves the internal branch-add tool, the git group's
        // only member, before MCP dispatch.
        McpToolDispatchGroup::ApplicationSurface
        | McpToolDispatchGroup::Git
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
        McpToolDispatchGroup::Health => {
            owned(dispatch_health_tools(tool_name, cg, invalid_args, options).await)
        }
        McpToolDispatchGroup::Memory => {
            owned(dispatch_memory_tools(tool_name, cg, invalid_args, options).await)
        }
        McpToolDispatchGroup::SessionWorkflow => {
            owned(dispatch_session_workflow_tools(tool_name, cg, invalid_args, options).await)
        }
    }
}
