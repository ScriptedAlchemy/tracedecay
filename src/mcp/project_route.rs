use std::collections::HashMap;

use serde_json::{json, Value};

use super::hook_events;
use super::tools::tool_dispatches_registered_project_reader;

#[derive(Default)]
pub(crate) struct HookProjectRouteCache {
    project_path: Option<String>,
    paths_by_session: HashMap<String, String>,
    paths_by_thread: HashMap<String, String>,
}

impl HookProjectRouteCache {
    pub(crate) fn route_cwd<'a>(event: &'a hook_events::HookEvent) -> Option<&'a std::path::Path> {
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
                self.paths_by_session
                    .insert(session_id.to_string(), project_path.clone());
            }
            if let Some(thread_id) = route.thread_id.as_deref().filter(|id| !id.is_empty()) {
                self.paths_by_thread
                    .insert(thread_id.to_string(), project_path);
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
        if let Some(thread_id) = mcp_route_thread_id(arguments) {
            if let Some(project_path) = self.paths_by_thread.get(&thread_id) {
                return Some(project_path.as_str());
            }
        }
        if let Some(session_id) = mcp_analytics_session_id(arguments) {
            if let Some(project_path) = self.paths_by_session.get(&session_id) {
                return Some(project_path.as_str());
            }
        }
        self.project_path.as_deref()
    }
}

pub(crate) fn mcp_analytics_session_id(arguments: &Value) -> Option<String> {
    route_identity_from_arguments(arguments, &["session_id", "sessionId"])
}

fn mcp_route_thread_id(arguments: &Value) -> Option<String> {
    route_identity_from_arguments(arguments, &["thread_id", "threadId"])
}

fn route_identity_from_arguments(arguments: &Value, keys: &[&str]) -> Option<String> {
    fn string_field(value: &Value, key: &str) -> Option<String> {
        value
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(ToOwned::to_owned)
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
    use serde_json::json;

    use super::HookProjectRouteCache;

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
}
