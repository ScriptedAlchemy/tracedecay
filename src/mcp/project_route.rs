use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};

use serde_json::Value;

use super::hook_events;
use crate::global_db::ProjectRegistryContext;

const MAX_HOOK_ROUTE_CACHE_ENTRIES: usize = 256;

#[derive(Clone)]
pub(crate) struct ResolvedProjectRoute {
    server: Weak<crate::mcp::server::McpServer>,
    pub(crate) owner: ProjectRegistryContext,
    pub(crate) profile_id: tracedecay_domain::UserProfileId,
    pub(crate) requested_root: PathBuf,
    /// Git identity observed when the route was resolved. Retained on the
    /// route value for scope/diagnostic cutover; dispatch today keys only on
    /// `requested_root` + mounted server.
    #[allow(dead_code)]
    pub(crate) requested_git_common_dir: Option<PathBuf>,
    #[allow(dead_code)]
    pub(crate) requested_branch: Option<String>,
    /// The exact application scope resolved ONCE at the entry point for this
    /// route (plan: `docs/superpowers/plans/v2/01-domain-request-context.md`).
    /// Query-facing handlers consume the routed server; the scope names the
    /// exact project/repository/worktree that server answers for.
    pub(crate) scope: tracedecay_application::ResolvedScope,
}

impl ResolvedProjectRoute {
    pub(crate) fn retained_server(
        &self,
    ) -> crate::errors::Result<Arc<crate::mcp::server::McpServer>> {
        self.server.upgrade().ok_or_else(|| {
            crate::errors::TraceDecayError::project_route(
                "project_route_unavailable",
                true,
                format!(
                    "registered project '{}' is no longer mounted",
                    self.owner.project.project_id
                ),
            )
        })
    }
}

impl std::fmt::Debug for ResolvedProjectRoute {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `server` is a live authority bundle with no useful Debug form; the
        // scope naming what it answers for is the diagnostic payload.
        formatter
            .debug_struct("ResolvedProjectRoute")
            .field("owner", &self.owner)
            .field("profile_id", &self.profile_id)
            .field("requested_root", &self.requested_root)
            .field("requested_git_common_dir", &self.requested_git_common_dir)
            .field("requested_branch", &self.requested_branch)
            .field("scope", &self.scope)
            .finish_non_exhaustive()
    }
}

pub(crate) async fn resolve_registered_project_route(
    context: ProjectRegistryContext,
    requested_path: &Path,
    global_db: &crate::global_db::RegisteredGlobalDb,
    resolver: Option<crate::mcp::server::RetainedProjectServerResolver>,
) -> crate::errors::Result<ResolvedProjectRoute> {
    let Some(resolver) = resolver else {
        return Err(crate::errors::TraceDecayError::project_route(
            "project_route_unavailable",
            true,
            "registered project server resolver is unavailable",
        ));
    };
    let requested_path = requested_path.to_path_buf();
    let request = crate::mcp::server::RetainedProjectGraphRequest::for_registered_project(
        context.clone(),
        requested_path.clone(),
    );
    let server = resolver(request.clone()).await?.ok_or_else(|| {
        crate::errors::TraceDecayError::project_route(
            "project_route_unavailable",
            true,
            format!(
                "registered project '{}' is not mounted for workspace {}",
                context.project.project_id,
                requested_path.display()
            ),
        )
    })?;
    let scope = crate::mcp::scope::resolve_query_scope(&context, &requested_path)
        .map_err(|error| error.into_route_failure().into_error())?;
    Ok(ResolvedProjectRoute {
        server: Arc::downgrade(&server),
        owner: context,
        profile_id: global_db.binding().shard_id.profile_id.clone(),
        requested_root: requested_path,
        requested_git_common_dir: request.requested_git_common_dir,
        requested_branch: request.requested_branch,
        scope,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProjectRouteFailureKind {
    NotFound,
    NotAuthorized,
    Ambiguous,
    Unavailable,
}

impl ProjectRouteFailureKind {
    pub(crate) const fn reason_code(self) -> &'static str {
        match self {
            Self::NotFound => "project_route_not_found",
            Self::NotAuthorized => "project_route_not_authorized",
            Self::Ambiguous => "project_route_ambiguous",
            Self::Unavailable => "project_route_unavailable",
        }
    }

    pub(crate) const fn retryable(self) -> bool {
        matches!(self, Self::Unavailable)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProjectRouteFailure {
    pub(crate) kind: ProjectRouteFailureKind,
    pub(crate) detail: String,
}

impl ProjectRouteFailure {
    pub(crate) fn into_error(self) -> crate::errors::TraceDecayError {
        crate::errors::TraceDecayError::project_route(
            self.kind.reason_code(),
            self.kind.retryable(),
            self.detail,
        )
    }

    pub(crate) fn from_selection_error(error: &crate::errors::TraceDecayError) -> Self {
        let detail = error.to_string();
        let kind = match error {
            crate::errors::TraceDecayError::ProjectRoute { reason_code, .. } => {
                match reason_code.as_str() {
                    "project_route_not_found" => ProjectRouteFailureKind::NotFound,
                    "project_route_not_authorized" => ProjectRouteFailureKind::NotAuthorized,
                    "project_route_ambiguous" => ProjectRouteFailureKind::Ambiguous,
                    _ => ProjectRouteFailureKind::Unavailable,
                }
            }
            crate::errors::TraceDecayError::Config { message }
                if message.contains("not found for selector") =>
            {
                ProjectRouteFailureKind::NotFound
            }
            crate::errors::TraceDecayError::Config { message }
                if message.contains("ambiguous") || message.contains("multiple stores") =>
            {
                ProjectRouteFailureKind::Ambiguous
            }
            crate::errors::TraceDecayError::Config { message }
                if message.contains("registry is unavailable")
                    || message.contains("profile identity") =>
            {
                ProjectRouteFailureKind::NotAuthorized
            }
            _ => ProjectRouteFailureKind::Unavailable,
        };
        Self { kind, detail }
    }
}

#[derive(Clone)]
pub(crate) enum WorkspaceProjectRoute {
    Resolved(Box<ResolvedProjectRoute>),
    Failed(ProjectRouteFailure),
}

#[derive(Clone, Default)]
pub(crate) struct HookProjectRouteCache {
    /// Route observed on this exact transport connection. It is never copied
    /// into [`SharedHookProjectRouteCache`]; only structural session/thread
    /// routes may cross from a hook socket to an agent socket.
    connection_route: Option<WorkspaceProjectRoute>,
    routes_by_session: HashMap<String, WorkspaceProjectRoute>,
    routes_by_thread: HashMap<String, WorkspaceProjectRoute>,
    threads_by_session: HashMap<String, String>,
    session_by_thread: HashMap<String, String>,
    session_order: VecDeque<String>,
    thread_order: VecDeque<String>,
}

impl HookProjectRouteCache {
    /// Evicts cached routes for a tombstoned project. The cached server lease
    /// is weak, but retaining the identity would make every later request fail
    /// against a retired project instead of allowing a newly resolved route.
    pub(crate) fn forget_project(
        &mut self,
        profile_id: &tracedecay_domain::UserProfileId,
        project_id: &str,
    ) {
        let belongs_to = |route: &WorkspaceProjectRoute| {
            matches!(route, WorkspaceProjectRoute::Resolved(resolved)
            if project_route_identity_matches(
                &resolved.profile_id,
                &resolved.owner.project.project_id,
                profile_id,
                project_id,
            ))
        };
        if self
            .connection_route
            .as_ref()
            .is_some_and(|route| belongs_to(route))
        {
            self.connection_route = None;
        }
        let sessions = self
            .routes_by_session
            .iter()
            .filter_map(|(session_id, route)| belongs_to(route).then_some(session_id.clone()))
            .collect::<Vec<_>>();
        for session_id in sessions {
            self.routes_by_session.remove(&session_id);
            if let Some(thread_id) = self.threads_by_session.remove(&session_id) {
                self.remove_thread_route(&thread_id);
            }
        }
        let threads = self
            .routes_by_thread
            .iter()
            .filter_map(|(thread_id, route)| belongs_to(route).then_some(thread_id.clone()))
            .collect::<Vec<_>>();
        for thread_id in threads {
            self.remove_thread_route(&thread_id);
        }
    }

    pub(crate) fn route_cwd(event: &hook_events::HookEvent) -> Option<&std::path::Path> {
        event
            .route
            .as_ref()
            .and_then(|route| route.worktree.as_deref().or(route.cwd.as_deref()))
            .or(event.cwd.as_deref())
    }

    pub(crate) fn observe_workspace_route(
        &mut self,
        event: &hook_events::HookEvent,
        route: WorkspaceProjectRoute,
    ) {
        self.connection_route = Some(route.clone());
        let Some(metadata) = event.route.as_ref() else {
            return;
        };
        let session_id = metadata
            .session_id
            .as_deref()
            .filter(|identity| !identity.is_empty())
            .and_then(|identity| crate::privacy::protect_sensitive_structural_id(identity).ok());
        let thread_id = metadata
            .thread_id
            .as_deref()
            .filter(|identity| !identity.is_empty())
            .and_then(|identity| crate::privacy::protect_sensitive_structural_id(identity).ok());
        if let Some(session_id) = session_id.as_deref() {
            self.insert_session_workspace_route(session_id.to_owned(), route.clone());
            if let Some(thread_id) = thread_id.as_deref()
                && let Some(old_thread_id) = self
                    .threads_by_session
                    .insert(session_id.to_owned(), thread_id.to_owned())
                && old_thread_id != thread_id
            {
                self.remove_thread_route(&old_thread_id);
            }
        }
        if let Some(thread_id) = thread_id {
            self.insert_thread_workspace_route(thread_id, route, session_id.as_deref());
        }
    }

    pub(crate) fn workspace_route_for_arguments(
        &self,
        arguments: &Value,
    ) -> Option<&WorkspaceProjectRoute> {
        let thread_id = mcp_route_thread_id(arguments);
        if let Some(thread_id) = thread_id.as_ref()
            && let Some(route) = self.routes_by_thread.get(thread_id)
        {
            return Some(route);
        }
        let session_id = mcp_analytics_session_id(arguments);
        if let Some(session_id) = session_id.as_ref()
            && let Some(route) = self.routes_by_session.get(session_id)
        {
            return Some(route);
        }
        // A structural identity is an explicit routing claim. If it does not
        // match the connection/shared cache, fail closed instead of silently
        // inheriting the last workspace observed on this socket. A stale
        // thread may still converge through its matching current session
        // above; only identity-free calls use the connection-local route.
        if thread_id.is_some() || session_id.is_some() {
            return None;
        }
        self.connection_route.as_ref()
    }

    fn insert_session_workspace_route(&mut self, session_id: String, route: WorkspaceProjectRoute) {
        if !self.routes_by_session.contains_key(&session_id) {
            self.session_order.push_back(session_id.clone());
        }
        self.routes_by_session.insert(session_id, route);
        self.evict_old_session_routes();
    }

    fn insert_thread_workspace_route(
        &mut self,
        thread_id: String,
        route: WorkspaceProjectRoute,
        session_id: Option<&str>,
    ) {
        if !self.routes_by_thread.contains_key(&thread_id) {
            self.thread_order.push_back(thread_id.clone());
        }
        if let Some(session_id) = session_id
            && let Some(old_session_id) = self
                .session_by_thread
                .insert(thread_id.clone(), session_id.to_string())
            && old_session_id != session_id
            && self
                .threads_by_session
                .get(&old_session_id)
                .is_some_and(|old_thread_id| old_thread_id == &thread_id)
        {
            self.threads_by_session.remove(&old_session_id);
        }
        self.routes_by_thread.insert(thread_id, route);
        self.evict_old_thread_routes();
    }

    fn remove_thread_route(&mut self, thread_id: &str) {
        self.routes_by_thread.remove(thread_id);
        if let Some(session_id) = self.session_by_thread.remove(thread_id)
            && self
                .threads_by_session
                .get(&session_id)
                .is_some_and(|old_thread_id| old_thread_id == thread_id)
        {
            self.threads_by_session.remove(&session_id);
        }
    }

    fn evict_old_session_routes(&mut self) {
        while self.routes_by_session.len() > MAX_HOOK_ROUTE_CACHE_ENTRIES {
            let Some(session_id) = self.session_order.pop_front() else {
                break;
            };
            let removed_route = self.routes_by_session.remove(&session_id).is_some();
            if removed_route && let Some(thread_id) = self.threads_by_session.remove(&session_id) {
                self.remove_thread_route(&thread_id);
            }
        }
    }

    fn evict_old_thread_routes(&mut self) {
        while self.routes_by_thread.len() > MAX_HOOK_ROUTE_CACHE_ENTRIES {
            let Some(thread_id) = self.thread_order.pop_front() else {
                break;
            };
            self.remove_thread_route(&thread_id);
        }
    }
}

#[derive(Clone, Default)]
pub(crate) struct SharedHookProjectRouteCache {
    inner: Arc<Mutex<HookProjectRouteCache>>,
}

impl SharedHookProjectRouteCache {
    fn unavailable(operation: &str) -> crate::errors::TraceDecayError {
        crate::errors::TraceDecayError::project_route(
            "project_route_unavailable",
            true,
            format!("project route cache is unavailable during {operation}"),
        )
    }

    pub(crate) fn snapshot(&self) -> crate::errors::Result<HookProjectRouteCache> {
        self.inner
            .lock()
            .map(|cache| cache.clone())
            .map_err(|_| Self::unavailable("snapshot"))
    }

    pub(crate) fn store(&self, cache: &HookProjectRouteCache) -> crate::errors::Result<()> {
        let mut shared = self.inner.lock().map_err(|_| Self::unavailable("update"))?;
        let mut cache = cache.clone();
        cache.connection_route = None;
        shared.clone_from(&cache);
        Ok(())
    }

    /// Refresh `target` from the shared cache with one clone under the lock.
    pub(crate) fn refresh_into(
        &self,
        target: &mut HookProjectRouteCache,
    ) -> crate::errors::Result<()> {
        let connection_route = target.connection_route.take();
        *target = self.snapshot()?;
        target.connection_route = connection_route;
        Ok(())
    }

    pub(crate) fn forget_project(
        &self,
        profile_id: &tracedecay_domain::UserProfileId,
        project_id: &str,
    ) -> Result<(), crate::errors::TraceDecayError> {
        let mut cache = self
            .inner
            .lock()
            .map_err(|_| Self::unavailable("project retirement"))?;
        cache.forget_project(profile_id, project_id);
        Ok(())
    }
}

pub(crate) fn mcp_analytics_session_id(arguments: &Value) -> Option<String> {
    route_identity_from_arguments(arguments, &["session_id", "sessionId"])
}

pub(crate) fn arguments_have_structural_route_identity(arguments: &Value) -> bool {
    mcp_route_thread_id(arguments).is_some() || mcp_analytics_session_id(arguments).is_some()
}

pub(crate) fn protect_tool_structural_ids(arguments: &mut Value) -> Result<(), ()> {
    const STRUCTURAL_ID_KEYS: &[&str] = &[
        "session_id",
        "sessionId",
        "thread_id",
        "threadId",
        "message_id",
        "messageId",
        "parent_session_id",
        "parentSessionId",
        "agent_id",
        "agentId",
        "parent_tool_use_id",
        "parentToolUseId",
        "turn_id",
        "turnId",
        "tool_call_id",
        "toolCallId",
        "conversation_id",
        "conversationId",
        "transcript_watermark",
        "transcriptWatermark",
        "source_id",
        "sourceId",
        "observation_id",
        "observationId",
    ];

    fn protect_fields(value: &mut Value, keys: &[&str]) -> Result<(), ()> {
        let Some(map) = value.as_object_mut() else {
            return Ok(());
        };
        for key in keys {
            let Some(raw) = map.get(*key).and_then(Value::as_str) else {
                continue;
            };
            let protected = crate::privacy::protect_sensitive_structural_id(raw).map_err(|_| ())?;
            map.insert((*key).to_string(), Value::String(protected));
        }
        Ok(())
    }

    protect_fields(arguments, STRUCTURAL_ID_KEYS)?;
    if let Some(meta) = arguments.get_mut("_meta") {
        protect_fields(meta, STRUCTURAL_ID_KEYS)?;
    }
    Ok(())
}

fn mcp_route_thread_id(arguments: &Value) -> Option<String> {
    route_identity_from_arguments(arguments, &["thread_id", "threadId"])
}

fn route_identity_from_arguments(arguments: &Value, keys: &[&str]) -> Option<String> {
    fn string_field(value: &Value, key: &str) -> Option<String> {
        let value = value.get(key).and_then(Value::as_str)?;
        if value.is_empty() {
            return None;
        }
        crate::privacy::protect_sensitive_structural_id(value).ok()
    }

    [Some(arguments), arguments.get("_meta")]
        .into_iter()
        .flatten()
        .find_map(|value| keys.iter().find_map(|key| string_field(value, key)))
}

pub(crate) fn arguments_have_project_selector(arguments: &Value) -> bool {
    arguments.get("project_selector").is_some()
        || arguments.get("project_id").is_some()
        || arguments.get("project_path").is_some()
        || arguments.get("project_root").is_some()
        || arguments.get("root").is_some()
}

fn project_route_identity_matches(
    route_profile_id: &tracedecay_domain::UserProfileId,
    route_project_id: &str,
    target_profile_id: &tracedecay_domain::UserProfileId,
    target_project_id: &str,
) -> bool {
    route_profile_id == target_profile_id && route_project_id == target_project_id
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::process::Command;
    use std::sync::Arc;

    use serde_json::json;
    use tempfile::TempDir;

    use super::{
        HookProjectRouteCache, MAX_HOOK_ROUTE_CACHE_ENTRIES, SharedHookProjectRouteCache,
        project_route_identity_matches,
    };
    use crate::daemon::{HookAgent, HookRouteMetadata, ProductionProjectCompositionHarnessV1};
    use crate::mcp::hook_events::{HookEvent, HookEventKind};
    use crate::mcp::server::McpServer;

    struct ResolvedRouteFixture {
        _isolation: TempDir,
        _harness: ProductionProjectCompositionHarnessV1,
        server: Arc<McpServer>,
        event: HookEvent,
    }

    async fn resolved_route_fixture(session_id: &str, thread_id: &str) -> ResolvedRouteFixture {
        let isolation = TempDir::new().expect("route isolation");
        let project = isolation.path().join("route-project");
        fs::create_dir_all(project.join("src")).expect("route source directory");
        fs::write(
            project.join("Cargo.toml"),
            "[package]\nname = \"route_fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        )
        .expect("route manifest");
        fs::write(project.join("src/lib.rs"), "pub fn route_marker() {}\n").expect("route source");
        for args in [
            &["init", "--quiet"][..],
            &["add", "."][..],
            &[
                "-c",
                "user.name=TraceDecay Tests",
                "-c",
                "user.email=tests@tracedecay.invalid",
                "commit",
                "--quiet",
                "-m",
                "fixture",
            ][..],
        ] {
            assert!(
                Command::new(crate::git::git_program())
                    .args(args)
                    .current_dir(&project)
                    .status()
                    .expect("git route fixture")
                    .success()
            );
        }
        let harness =
            ProductionProjectCompositionHarnessV1::open(isolation.path(), [project.clone()])
                .await
                .expect("route composition");
        let server = harness.server(&project).expect("registered route server");
        let route_root = server
            .cg()
            .await
            .project_root()
            .to_string_lossy()
            .into_owned();
        let event = hook_event(session_id, thread_id, &route_root);
        ResolvedRouteFixture {
            _isolation: isolation,
            _harness: harness,
            server,
            event,
        }
    }

    #[test]
    fn project_route_retirement_keeps_same_project_id_in_another_profile() {
        let profile_a = tracedecay_domain::UserProfileId::new("profile.a").expect("profile A");
        let profile_b = tracedecay_domain::UserProfileId::new("profile.b").expect("profile B");

        assert!(project_route_identity_matches(
            &profile_a,
            "proj-shared",
            &profile_a,
            "proj-shared"
        ));
        assert!(!project_route_identity_matches(
            &profile_b,
            "proj-shared",
            &profile_a,
            "proj-shared"
        ));
    }

    #[tokio::test]
    async fn cached_resolved_route_is_out_of_band_and_preserves_arguments() {
        let fixture = resolved_route_fixture("session.route-cache", "thread.route-cache").await;
        let expected_graph = fixture.server.cg().await;
        let expected_project_id = expected_graph
            .store_layout()
            .identity
            .project_id
            .as_deref()
            .expect("route fixture project identity");
        let mut cache = HookProjectRouteCache::default();
        fixture
            .server
            .update_hook_workspace_route(&fixture.event, &mut cache)
            .await
            .expect("hook route resolves");
        let arguments = json!({
            "layout": "flat",
            "session_id": "session.route-cache",
            "thread_id": "thread.route-cache",
            "_meta": {"client": "fixture"}
        });
        let original_bytes = serde_json::to_vec(&arguments).expect("serialize original arguments");

        let selected = resolved_route_for_arguments(&cache, &arguments);
        let routed = arguments;
        let selected = selected.expect("cached route travels out of band");

        assert_eq!(
            serde_json::to_vec(&routed).expect("serialize routed arguments"),
            original_bytes,
            "route selection must not rewrite caller arguments"
        );
        assert!(
            routed.get("project_selector").is_none()
                && routed.get("project_path").is_none()
                && routed.get("project_root").is_none(),
            "cached routes must not inject public selector aliases: {routed}"
        );
        assert!(Arc::ptr_eq(
            &selected
                .retained_server()
                .expect("cached server remains live"),
            &fixture.server
        ));
        assert_eq!(
            selected.owner.project.project_id.as_str(),
            expected_project_id
        );
        assert_eq!(
            selected.owner.project.canonical_root,
            expected_graph.project_root().to_string_lossy()
        );
        assert_eq!(selected.scope.project_id.as_str(), expected_project_id);
        selected
            .scope
            .validate()
            .expect("cached route scope validates");
    }

    #[tokio::test]
    async fn shared_host_retry_retains_exact_route_without_json_mutation() {
        let fixture = resolved_route_fixture("session.host-retry", "thread.host-retry").await;
        let shared = SharedHookProjectRouteCache::default();
        let mut hook_connection = shared.snapshot().expect("shared route snapshot");
        fixture
            .server
            .update_hook_workspace_route(&fixture.event, &mut hook_connection)
            .await
            .expect("host hook route resolves");
        let expected = resolved_route_for_arguments(
            &hook_connection,
            &json!({
                "layout": "flat",
                "session_id": "session.host-retry",
                "thread_id": "thread.host-retry"
            }),
        );
        let expected = expected.expect("initial host route is out of band");
        shared
            .store(&hook_connection)
            .expect("store shared hook route");

        let retry_connection = shared.snapshot().expect("shared retry snapshot");
        let arguments = json!({
            "layout": "flat",
            "session_id": "session.host-retry",
            "thread_id": "thread.host-retry"
        });
        let original_bytes = serde_json::to_vec(&arguments).expect("serialize retry arguments");
        let selected = resolved_route_for_arguments(&retry_connection, &arguments);
        let routed = arguments;
        let selected = selected.expect("host retry carries a resolved route");

        assert_eq!(
            serde_json::to_vec(&routed).expect("serialize routed retry arguments"),
            original_bytes,
            "a host retry must preserve its caller JSON"
        );
        let expected_server = expected
            .retained_server()
            .expect("expected route server remains live");
        assert!(Arc::ptr_eq(
            &selected
                .retained_server()
                .expect("selected route server remains live"),
            &expected_server
        ));
        assert_eq!(selected.owner, expected.owner);
        assert_eq!(selected.scope, expected.scope);
        let next_event = hook_event(
            "session.host-retry",
            "thread.host-retry.next",
            expected_server
                .cg_snapshot()
                .await
                .project_root()
                .to_string_lossy()
                .as_ref(),
        );
        let mut refreshed = shared.snapshot().expect("shared refreshed snapshot");
        fixture
            .server
            .update_hook_workspace_route(&next_event, &mut refreshed)
            .await
            .expect("updated host route resolves");
        assert!(
            !refreshed.routes_by_thread.contains_key("thread.host-retry")
                && refreshed
                    .routes_by_thread
                    .contains_key("thread.host-retry.next"),
            "a new thread for the session must invalidate its stale thread route"
        );
        shared.store(&refreshed).expect("store refreshed route");

        let retry_connection = shared.snapshot().expect("shared session snapshot");
        let stale_thread_arguments = json!({
            "layout": "flat",
            "session_id": "session.host-retry",
            "thread_id": "thread.host-retry"
        });
        let stale_thread_selected =
            resolved_route_for_arguments(&retry_connection, &stale_thread_arguments);
        let stale_thread_routed = stale_thread_arguments.clone();
        assert_eq!(
            stale_thread_routed, stale_thread_arguments,
            "session fallback must not rewrite stale-thread arguments"
        );
        let stale_thread_selected = stale_thread_selected.expect("current session route exists");
        assert!(Arc::ptr_eq(
            &stale_thread_selected
                .retained_server()
                .expect("session route server remains live"),
            &expected_server
        ));

        let identity_free = json!({"layout": "flat"});
        let identity_free_selected =
            resolved_route_for_arguments(&retry_connection, &identity_free);
        let identity_free_routed = identity_free.clone();
        assert_eq!(identity_free_routed, identity_free);
        assert!(
            identity_free_selected.is_none(),
            "daemon-wide shared state must not route identity-free calls by the last hook"
        );
    }

    #[tokio::test]
    async fn unknown_explicit_identity_never_inherits_last_connection_project() {
        let project_a = resolved_route_fixture("session.project-a", "thread.project-a").await;
        let project_b = resolved_route_fixture("session.project-b", "thread.project-b").await;
        let mut cache = HookProjectRouteCache::default();
        project_a
            .server
            .update_hook_workspace_route(&project_a.event, &mut cache)
            .await
            .expect("project A route resolves");
        project_b
            .server
            .update_hook_workspace_route(&project_b.event, &mut cache)
            .await
            .expect("project B route resolves");

        for arguments in [
            json!({"session_id": "session.unknown"}),
            json!({"thread_id": "thread.unknown"}),
            json!({
                "session_id": "session.unknown",
                "thread_id": "thread.unknown"
            }),
        ] {
            assert!(
                cache.workspace_route_for_arguments(&arguments).is_none(),
                "unknown explicit identity must not inherit the last connection route: {arguments}"
            );
        }

        let identity_free = cache
            .workspace_route_for_arguments(&json!({"layout": "flat"}))
            .expect("identity-free request keeps the exact connection-local route");
        let WorkspaceProjectRoute::Resolved(identity_free) = identity_free else {
            panic!("project B connection route must remain resolved");
        };
        assert!(Arc::ptr_eq(
            &identity_free
                .retained_server()
                .expect("project B route remains live"),
            &project_b.server
        ));
    }

    #[tokio::test]
    async fn shared_resolved_routes_evict_oldest_session_and_thread() {
        let fixture = resolved_route_fixture("session.seed", "thread.seed").await;
        let mut seed = HookProjectRouteCache::default();
        fixture
            .server
            .update_hook_workspace_route(&fixture.event, &mut seed)
            .await
            .expect("seed route resolves");
        let route = seed
            .routes_by_session
            .get("session.seed")
            .expect("seed session route")
            .clone();
        let mut bounded = HookProjectRouteCache::default();
        for index in 0..=MAX_HOOK_ROUTE_CACHE_ENTRIES {
            bounded.observe_workspace_route(
                &hook_event(
                    &format!("session-{index}"),
                    &format!("thread-{index}"),
                    route_root(&route).as_ref(),
                ),
                route.clone(),
            );
        }
        let shared = SharedHookProjectRouteCache::default();
        shared.store(&bounded).expect("store bounded routes");
        let fresh = shared.snapshot().expect("bounded route snapshot");

        let evicted = json!({
            "layout": "flat",
            "session_id": "session-0",
            "thread_id": "thread-0"
        });
        let evicted_route = resolved_route_for_arguments(&fresh, &evicted);
        let evicted_routed = evicted.clone();
        assert_eq!(evicted_routed, evicted);
        assert!(evicted_route.is_none(), "oldest route must be evicted");

        let newest = json!({
            "layout": "flat",
            "_meta": {
                "sessionId": format!("session-{MAX_HOOK_ROUTE_CACHE_ENTRIES}"),
                "threadId": format!("thread-{MAX_HOOK_ROUTE_CACHE_ENTRIES}")
            }
        });
        let newest_route = resolved_route_for_arguments(&fresh, &newest);
        let newest_routed = newest.clone();
        assert_eq!(newest_routed, newest);
        assert!(newest_route.is_some(), "newest route must remain cached");
    }

    fn route_root(route: &super::WorkspaceProjectRoute) -> std::borrow::Cow<'_, str> {
        match route {
            super::WorkspaceProjectRoute::Resolved(route) => route.requested_root.to_string_lossy(),
            super::WorkspaceProjectRoute::Failed(_) => unreachable!("seed route resolved"),
        }
    }

    fn resolved_route_for_arguments(
        cache: &HookProjectRouteCache,
        arguments: &serde_json::Value,
    ) -> Option<super::ResolvedProjectRoute> {
        match cache.workspace_route_for_arguments(arguments) {
            Some(super::WorkspaceProjectRoute::Resolved(route)) => Some(route.as_ref().clone()),
            Some(super::WorkspaceProjectRoute::Failed(failure)) => {
                panic!("unexpected failed route: {}", failure.detail)
            }
            None => None,
        }
    }

    fn hook_event(session_id: &str, thread_id: &str, cwd: &str) -> HookEvent {
        HookEvent {
            agent: HookAgent::Claude,
            kind: HookEventKind::FileEdit,
            rel_paths: Vec::new(),
            had_command: false,
            cwd: Some(PathBuf::from(cwd)),
            route: Some(HookRouteMetadata {
                session_id: Some(session_id.to_string()),
                thread_id: Some(thread_id.to_string()),
                cwd: Some(PathBuf::from(cwd)),
                worktree: None,
                branch: None,
            }),
            receipt: None,
        }
    }
}
