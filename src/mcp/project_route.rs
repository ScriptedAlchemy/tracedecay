use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use super::hook_events;
use super::tools::tool_dispatches_registered_project_reader;

const MAX_HOOK_ROUTE_CACHE_ENTRIES: usize = 256;

#[derive(Clone, Default)]
pub(crate) struct HookProjectRouteCache {
    project_path: Option<String>,
    paths_by_session: HashMap<String, String>,
    paths_by_thread: HashMap<String, String>,
    threads_by_session: HashMap<String, String>,
    session_by_thread: HashMap<String, String>,
    session_order: VecDeque<String>,
    thread_order: VecDeque<String>,
}

impl HookProjectRouteCache {
    pub(crate) fn route_cwd(event: &hook_events::HookEvent) -> Option<&std::path::Path> {
        event
            .route
            .as_ref()
            .and_then(|route| route.cwd.as_deref())
            .or(event.cwd.as_deref())
    }

    pub(crate) fn observe_hook_event(
        &mut self,
        event: &hook_events::HookEvent,
        project_path: Option<String>,
    ) {
        self.project_path.clone_from(&project_path);
        let Some(project_path) = project_path else {
            return;
        };
        if let Some(route) = event.route.as_ref() {
            if let Some(session_id) = route.session_id.as_deref().filter(|id| !id.is_empty()) {
                self.insert_session_route(session_id.to_string(), project_path.clone());
                if let Some(thread_id) = route.thread_id.as_deref().filter(|id| !id.is_empty())
                    && let Some(old_thread_id) = self
                        .threads_by_session
                        .insert(session_id.to_string(), thread_id.to_string())
                    && old_thread_id != thread_id
                {
                    self.remove_thread_route(&old_thread_id);
                }
            }
            if let Some(thread_id) = route.thread_id.as_deref().filter(|id| !id.is_empty()) {
                let session_id = route.session_id.as_deref().filter(|id| !id.is_empty());
                self.insert_thread_route(thread_id.to_string(), project_path, session_id);
            }
        }
    }

    pub(crate) fn apply_to_tool_arguments(&self, tool_name: &str, mut arguments: Value) -> Value {
        if !tool_dispatches_registered_project_reader(tool_name)
            || arguments_have_project_selector(&arguments)
        {
            return arguments;
        }
        let Some(project_path) = self.project_path_for_arguments(&arguments) else {
            return arguments;
        };
        if let Some(map) = arguments.as_object_mut() {
            map.insert(
                "project_selector".to_string(),
                json!({ "path": project_path }),
            );
        }
        arguments
    }

    fn project_path_for_arguments(&self, arguments: &Value) -> Option<&str> {
        if let Some(thread_id) = mcp_route_thread_id(arguments)
            && let Some(project_path) = self.paths_by_thread.get(&thread_id)
        {
            return Some(project_path.as_str());
        }
        if let Some(session_id) = mcp_analytics_session_id(arguments)
            && let Some(project_path) = self.paths_by_session.get(&session_id)
        {
            return Some(project_path.as_str());
        }
        self.project_path.as_deref()
    }

    /// Overwrite every field except `project_path` from an already-cloned
    /// shared snapshot, taking ownership so no second deep clone is needed.
    fn refresh_from_owned(&mut self, mut shared: HookProjectRouteCache) {
        shared.project_path = self.project_path.take();
        *self = shared;
    }

    fn insert_session_route(&mut self, session_id: String, project_path: String) {
        if !self.paths_by_session.contains_key(&session_id) {
            self.session_order.push_back(session_id.clone());
        }
        self.paths_by_session.insert(session_id, project_path);
        self.evict_old_session_routes();
    }

    fn insert_thread_route(
        &mut self,
        thread_id: String,
        project_path: String,
        session_id: Option<&str>,
    ) {
        if !self.paths_by_thread.contains_key(&thread_id) {
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
        self.paths_by_thread.insert(thread_id, project_path);
        self.evict_old_thread_routes();
    }

    fn remove_thread_route(&mut self, thread_id: &str) {
        self.paths_by_thread.remove(thread_id);
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
        while self.paths_by_session.len() > MAX_HOOK_ROUTE_CACHE_ENTRIES {
            let Some(session_id) = self.session_order.pop_front() else {
                break;
            };
            if self.paths_by_session.remove(&session_id).is_some()
                && let Some(thread_id) = self.threads_by_session.remove(&session_id)
            {
                self.remove_thread_route(&thread_id);
            }
        }
    }

    fn evict_old_thread_routes(&mut self) {
        while self.paths_by_thread.len() > MAX_HOOK_ROUTE_CACHE_ENTRIES {
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
    pub(crate) fn snapshot(&self) -> HookProjectRouteCache {
        self.inner
            .lock()
            .map(|cache| cache.clone())
            .unwrap_or_default()
    }

    pub(crate) fn store(&self, cache: &HookProjectRouteCache) {
        if let Ok(mut shared) = self.inner.lock() {
            let mut cache = cache.clone();
            cache.project_path = None;
            shared.clone_from(&cache);
        }
    }

    /// Refresh `target` from the shared cache with a single deep clone taken
    /// under the lock, preserving `target`'s local `project_path`.
    pub(crate) fn refresh_into(&self, target: &mut HookProjectRouteCache) {
        let cloned = self
            .inner
            .lock()
            .map(|cache| cache.clone())
            .unwrap_or_default();
        target.refresh_from_owned(cloned);
    }
}

pub(crate) fn mcp_analytics_session_id(arguments: &Value) -> Option<String> {
    route_identity_from_arguments(arguments, &["session_id", "sessionId"])
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

fn arguments_have_project_selector(arguments: &Value) -> bool {
    arguments.get("project_selector").is_some()
        || arguments.get("project_id").is_some()
        || arguments.get("project_path").is_some()
        || arguments.get("project_root").is_some()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use serde_json::json;

    use super::{HookProjectRouteCache, MAX_HOOK_ROUTE_CACHE_ENTRIES, SharedHookProjectRouteCache};
    use crate::daemon::{HookAgent, HookRouteMetadata};
    use crate::mcp::hook_events::{HookEvent, HookEventKind};

    #[test]
    fn route_prefers_thread_then_session_then_last_hook_path() {
        let mut cache = HookProjectRouteCache {
            project_path: Some("/repo/default".to_string()),
            ..HookProjectRouteCache::default()
        };
        cache
            .paths_by_session
            .insert("session-a".to_string(), "/repo/session-a".to_string());
        cache
            .paths_by_thread
            .insert("thread-a".to_string(), "/repo/thread-a".to_string());

        assert_eq!(
            cache.project_path_for_arguments(
                &json!({"session_id": "session-a", "thread_id": "thread-a"})
            ),
            Some("/repo/thread-a")
        );
        assert_eq!(
            cache.project_path_for_arguments(&json!({"session_id": "session-a"})),
            Some("/repo/session-a")
        );
        assert_eq!(
            cache.project_path_for_arguments(&json!({"session_id": "unknown"})),
            Some("/repo/default")
        );
    }

    #[test]
    fn route_reads_thread_and_session_ids_from_meta() {
        let mut cache = HookProjectRouteCache::default();
        cache
            .paths_by_session
            .insert("session-meta".to_string(), "/repo/session-meta".to_string());
        cache
            .paths_by_thread
            .insert("thread-meta".to_string(), "/repo/thread-meta".to_string());

        assert_eq!(
            cache.project_path_for_arguments(
                &json!({"_meta": {"sessionId": "session-meta", "threadId": "thread-meta"}})
            ),
            Some("/repo/thread-meta")
        );
    }

    #[test]
    fn route_injects_selector_without_overriding_explicit_selector() {
        let mut cache = HookProjectRouteCache::default();
        cache
            .paths_by_session
            .insert("session-a".to_string(), "/repo/session-a".to_string());

        let routed = cache.apply_to_tool_arguments(
            "tracedecay_context",
            json!({"task": "inspect routing", "session_id": "session-a"}),
        );
        assert_eq!(routed["project_selector"]["path"], "/repo/session-a");

        let explicit = cache.apply_to_tool_arguments(
            "tracedecay_context",
            json!({
                "task": "inspect routing",
                "session_id": "session-a",
                "project_selector": {"path": "/repo/explicit"},
            }),
        );
        assert_eq!(explicit["project_selector"]["path"], "/repo/explicit");
    }

    #[test]
    fn shared_route_survives_fresh_request_cache_and_invalidates_old_ids() {
        let shared = SharedHookProjectRouteCache::default();
        let first_event = hook_event("session-a", "thread-a", "/work/project-a");
        let mut hook_connection_cache = shared.snapshot();
        hook_connection_cache.observe_hook_event(&first_event, Some("/repo/a".to_string()));
        shared.store(&hook_connection_cache);

        let tool_connection_cache = shared.snapshot();
        let routed = tool_connection_cache.apply_to_tool_arguments(
            "tracedecay_context",
            json!({"task": "inspect", "session_id": "session-a", "thread_id": "thread-a"}),
        );
        assert_eq!(routed["project_selector"]["path"], "/repo/a");

        let second_event = hook_event("session-a", "thread-b", "/work/project-b");
        let mut later_hook_connection_cache = shared.snapshot();
        later_hook_connection_cache.observe_hook_event(&second_event, Some("/repo/b".to_string()));
        shared.store(&later_hook_connection_cache);

        let fresh_tool_connection_cache = shared.snapshot();
        let rerouted = fresh_tool_connection_cache.apply_to_tool_arguments(
            "tracedecay_context",
            json!({"task": "inspect", "session_id": "session-a", "thread_id": "thread-b"}),
        );
        assert_eq!(rerouted["project_selector"]["path"], "/repo/b");

        let stale_thread = fresh_tool_connection_cache.apply_to_tool_arguments(
            "tracedecay_context",
            json!({"task": "inspect", "session_id": "session-a", "thread_id": "thread-a"}),
        );
        assert_eq!(stale_thread["project_selector"]["path"], "/repo/b");
    }

    #[test]
    fn shared_route_does_not_publish_last_hook_fallback() {
        let shared = SharedHookProjectRouteCache::default();
        let event = hook_event("session-a", "thread-a", "/work/project-a");
        let mut hook_connection_cache = shared.snapshot();
        hook_connection_cache.observe_hook_event(&event, Some("/repo/a".to_string()));
        shared.store(&hook_connection_cache);

        let fresh_tool_connection_cache = shared.snapshot();
        let unrouted = fresh_tool_connection_cache
            .apply_to_tool_arguments("tracedecay_context", json!({"task": "inspect"}));
        assert!(
            unrouted.get("project_selector").is_none(),
            "daemon-wide shared state must not route identity-free calls by last hook"
        );
    }

    #[test]
    fn shared_route_evicts_oldest_session_and_thread_routes() {
        let shared = SharedHookProjectRouteCache::default();
        let mut hook_connection_cache = shared.snapshot();

        for index in 0..=MAX_HOOK_ROUTE_CACHE_ENTRIES {
            let event = hook_event(
                &format!("session-{index}"),
                &format!("thread-{index}"),
                &format!("/work/project-{index}"),
            );
            hook_connection_cache.observe_hook_event(&event, Some(format!("/repo/{index}")));
        }
        shared.store(&hook_connection_cache);

        let fresh_tool_connection_cache = shared.snapshot();
        let evicted = fresh_tool_connection_cache.apply_to_tool_arguments(
            "tracedecay_context",
            json!({"task": "inspect", "session_id": "session-0", "thread_id": "thread-0"}),
        );
        assert!(
            evicted.get("project_selector").is_none(),
            "oldest identity routes should be evicted once the shared cache is full"
        );

        let retained = fresh_tool_connection_cache.apply_to_tool_arguments(
            "tracedecay_context",
            json!({
                "task": "inspect",
                "session_id": format!("session-{MAX_HOOK_ROUTE_CACHE_ENTRIES}"),
                "thread_id": format!("thread-{MAX_HOOK_ROUTE_CACHE_ENTRIES}")
            }),
        );
        assert_eq!(
            retained["project_selector"]["path"],
            format!("/repo/{MAX_HOOK_ROUTE_CACHE_ENTRIES}")
        );
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
