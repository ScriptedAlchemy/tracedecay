use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tracedecay_domain::canonical_sha256;

use super::super::contract::{
    SessionRetrievalProjectSelector, SessionRetrievalServiceOutcome, SessionRetrievalServicePort,
    SessionRetrievalStoreScope,
};
use super::{
    MessageSearchRequest, apply_typed_error, base_message_search_payload, payload_object_mut,
    render_service_outcome, retrieval_command_with_paging,
};
use crate::errors::{Result, TraceDecayError};
use crate::mcp::tools::handlers::project_registry::{
    ProjectRegistryListingCommand, ProjectRegistryListingOutcome, ProjectRegistryListingScope,
    ProjectRegistryReadPort, list_registered_projects,
};
use crate::mcp::tools::handlers::support::argument_error;

const MAX_ALL_REGISTERED_PROJECTS: usize = 25;
const MAX_ALL_REGISTERED_RESULTS: usize = 1_024;
const ALL_REGISTERED_CURSOR_PREFIX: &str = "allr1.";
const MAX_ALL_REGISTERED_CURSOR_BYTES: usize = 4_096;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct AllRegisteredCursor {
    version: u8,
    next_offset: usize,
    query_digest: String,
    project_ids_digest: String,
    root_snapshots_digest: String,
    digest: String,
}

fn unavailable_all_registered_payload(
    request: &MessageSearchRequest<'_>,
    code: &str,
    message: &str,
) -> Result<Value> {
    let mut payload = base_message_search_payload(request)?;
    apply_typed_error(&mut payload, "unavailable", code, message)?;
    let map = payload_object_mut(&mut payload)?;
    map.insert("project_scope".to_string(), json!("all_registered"));
    map.insert("searched_project_count".to_string(), json!(0));
    map.insert("skipped_project_count".to_string(), json!(0));
    map.insert("catch_up_skipped_project_count".to_string(), json!(0));
    Ok(payload)
}

fn compare_all_registered_results(left: &Value, right: &Value) -> std::cmp::Ordering {
    let left_score = left
        .get("score")
        .and_then(Value::as_f64)
        .unwrap_or(f64::NEG_INFINITY);
    let right_score = right
        .get("score")
        .and_then(Value::as_f64)
        .unwrap_or(f64::NEG_INFINITY);
    right_score
        .total_cmp(&left_score)
        .then_with(|| {
            left.get("project_id")
                .and_then(Value::as_str)
                .cmp(&right.get("project_id").and_then(Value::as_str))
        })
        .then_with(|| {
            left.pointer("/session/session_id")
                .and_then(Value::as_str)
                .cmp(&right.pointer("/session/session_id").and_then(Value::as_str))
        })
        .then_with(|| {
            left.pointer("/message/message_id")
                .and_then(Value::as_str)
                .cmp(&right.pointer("/message/message_id").and_then(Value::as_str))
        })
}

fn all_registered_digest<T: Serialize + ?Sized>(domain: &str, value: &T) -> Result<String> {
    canonical_sha256(&(domain, value))
        .map(|digest| digest.as_str().to_owned())
        .map_err(|error| TraceDecayError::Config {
            message: format!("all_registered cursor digest: {error}"),
        })
}

fn all_registered_query_digest(
    request: &MessageSearchRequest<'_>,
    store_scope: SessionRetrievalStoreScope,
) -> Result<String> {
    all_registered_digest(
        "tracedecay.mcp.message-search.all-registered.query.v1",
        &json!({
            "query": request.query,
            "provider": request.requested_provider,
            "project_key": request.project_key,
            "parent_session_id": request.parent_session_id,
            "include_subagents": request.include_subagents,
            "catch_up": request.catch_up,
            "scope": request.scope.as_str(),
            "message_type": request.message_type.as_str(),
            "limit": request.limit,
            "git_filter": request.git_filter,
            "time_range": request.time_range,
            "workflow_scope": request.workflow_scope,
            "goals": request.goals,
            "store_scope": store_scope.as_str(),
        }),
    )
}

fn all_registered_project_ids_digest(project_ids: &[String]) -> Result<String> {
    all_registered_digest(
        "tracedecay.mcp.message-search.all-registered.projects.v1",
        &project_ids,
    )
}

impl AllRegisteredCursor {
    fn new(
        next_offset: usize,
        query_digest: String,
        project_ids_digest: String,
        root_snapshots_digest: String,
    ) -> Result<Self> {
        let digest = all_registered_digest(
            "tracedecay.mcp.message-search.all-registered.cursor.v1",
            &(
                1_u8,
                next_offset,
                &query_digest,
                &project_ids_digest,
                &root_snapshots_digest,
            ),
        )?;
        Ok(Self {
            version: 1,
            next_offset,
            query_digest,
            project_ids_digest,
            root_snapshots_digest,
            digest,
        })
    }

    fn encode(&self) -> Result<String> {
        Ok(format!(
            "{ALL_REGISTERED_CURSOR_PREFIX}{}",
            hex::encode(serde_json::to_vec(self)?)
        ))
    }

    fn decode(
        encoded: &str,
        query_digest: &str,
        project_ids_digest: &str,
        page_size: usize,
    ) -> Result<Self> {
        let encoded = encoded
            .strip_prefix(ALL_REGISTERED_CURSOR_PREFIX)
            .ok_or_else(|| {
                argument_error("all_registered cursor is not a canonical aggregate cursor")
            })?;
        if encoded.len() > MAX_ALL_REGISTERED_CURSOR_BYTES.saturating_mul(2) {
            return Err(argument_error(
                "all_registered cursor exceeds the canonical bound",
            ));
        }
        let bytes = hex::decode(encoded)
            .map_err(|_| argument_error("all_registered cursor is not valid hexadecimal"))?;
        if bytes.len() > MAX_ALL_REGISTERED_CURSOR_BYTES {
            return Err(argument_error(
                "all_registered cursor exceeds the canonical bound",
            ));
        }
        if hex::encode(&bytes) != encoded {
            return Err(argument_error(
                "all_registered cursor is not canonically encoded",
            ));
        }
        let cursor: Self = serde_json::from_slice(&bytes)
            .map_err(|_| argument_error("all_registered cursor is malformed"))?;
        if serde_json::to_vec(&cursor)
            .map_err(|_| argument_error("all_registered cursor cannot be re-encoded"))?
            != bytes
        {
            return Err(argument_error("all_registered cursor is not canonical"));
        }
        let expected = Self::new(
            cursor.next_offset,
            query_digest.to_owned(),
            project_ids_digest.to_owned(),
            cursor.root_snapshots_digest.clone(),
        )?;
        if cursor.version != expected.version
            || cursor.query_digest != expected.query_digest
            || cursor.project_ids_digest != expected.project_ids_digest
            || cursor.digest != expected.digest
        {
            return Err(argument_error(
                "all_registered cursor does not match this query or registered project set",
            ));
        }
        if cursor.next_offset == 0 || !cursor.next_offset.is_multiple_of(page_size) {
            return Err(argument_error(
                "all_registered cursor does not name a canonical page boundary",
            ));
        }
        Ok(cursor)
    }
}

pub(super) async fn all_registered_message_search(
    project_root: Option<&Path>,
    request: &MessageSearchRequest<'_>,
    store_scope: SessionRetrievalStoreScope,
    service: Option<&dyn SessionRetrievalServicePort>,
    registry: Option<&dyn ProjectRegistryReadPort>,
) -> Result<Value> {
    let Some(registry) = registry else {
        return unavailable_all_registered_payload(
            request,
            "project_registry_unavailable",
            "no project registry authority is mounted for this profile",
        );
    };
    let Some(service) = service else {
        return unavailable_all_registered_payload(
            request,
            "session_retrieval_service_not_configured",
            "no session retrieval service is configured for this profile",
        );
    };
    let listing = list_registered_projects(
        Some(registry),
        ProjectRegistryListingCommand {
            active_project_root: project_root.map_or_else(|| PathBuf::from("."), Path::to_path_buf),
            scope: ProjectRegistryListingScope::All,
            limit: MAX_ALL_REGISTERED_PROJECTS,
        },
    )
    .await?;
    let ProjectRegistryListingOutcome::Listing(listing) = listing else {
        return unavailable_all_registered_payload(
            request,
            "project_registry_unavailable",
            "no project registry authority is mounted for this profile",
        );
    };

    let mut registered_projects = listing.projects;
    registered_projects.sort_by(|left, right| left.project_id.cmp(&right.project_id));
    let project_ids = registered_projects
        .iter()
        .map(|project| project.project_id.clone())
        .collect::<Vec<_>>();
    let query_digest = all_registered_query_digest(request, store_scope)?;
    let project_ids_digest = all_registered_project_ids_digest(&project_ids)?;
    let cursor = request
        .cursor
        .map(|cursor| {
            AllRegisteredCursor::decode(cursor, &query_digest, &project_ids_digest, request.limit)
        })
        .transpose()?;
    let offset = cursor.as_ref().map_or(0, |cursor| cursor.next_offset);
    let Some(per_root_limit) = offset
        .checked_add(request.limit)
        .filter(|limit| *limit <= MAX_ALL_REGISTERED_RESULTS)
    else {
        return unavailable_all_registered_payload(
            request,
            "all_registered_cursor_limit_exceeded",
            "the all_registered continuation exceeds the canonical per-project retrieval bound",
        );
    };

    let mut merged = base_message_search_payload(request)?;
    let mut results = Vec::new();
    let mut searched = 0_usize;
    let mut skipped = 0_usize;
    let mut partial_or_stale = false;
    let mut root_cursor_present = false;
    let mut projects = Vec::with_capacity(registered_projects.len());
    let mut root_snapshots = Vec::with_capacity(registered_projects.len());
    for project in &registered_projects {
        let command = retrieval_command_with_paging(
            request,
            store_scope,
            Some(SessionRetrievalProjectSelector {
                project_id: Some(project.project_id.clone()),
                project_path: None,
            }),
            None,
            per_root_limit,
        )?;
        let outcome = service.execute(command).await;
        let searchable = matches!(
            &outcome,
            SessionRetrievalServiceOutcome::Complete { .. }
                | SessionRetrievalServiceOutcome::CompleteZero { .. }
                | SessionRetrievalServiceOutcome::Partial { .. }
                | SessionRetrievalServiceOutcome::Stale { .. }
        );
        partial_or_stale |= matches!(
            &outcome,
            SessionRetrievalServiceOutcome::Partial { .. }
                | SessionRetrievalServiceOutcome::Stale { .. }
        );
        let payload = render_service_outcome(request, outcome)?;
        if searchable {
            searched = searched.saturating_add(1);
            root_cursor_present |= payload
                .pointer("/temporal/cursor")
                .and_then(Value::as_str)
                .is_some()
                && payload
                    .get("count")
                    .and_then(Value::as_u64)
                    .is_some_and(|count| count > 0);
            if let Some(page) = payload.get("results").and_then(Value::as_array) {
                for result in page {
                    let mut result = result.clone();
                    if let Some(map) = result.as_object_mut() {
                        map.insert("project_id".to_string(), json!(project.project_id));
                        map.insert("project_root".to_string(), json!(project.display_root));
                    }
                    results.push(result);
                }
            }
        } else {
            skipped = skipped.saturating_add(1);
        }
        projects.push(json!({
            "project_id": project.project_id,
            "project_root": project.display_root,
            "status": payload.get("status").cloned().unwrap_or(Value::Null),
            "outcome": payload.get("outcome").cloned().unwrap_or(Value::Null),
            "count": payload.get("count").cloned().unwrap_or(json!(0)),
            "error": payload.get("error").cloned().unwrap_or(Value::Null),
            "cursor": payload.pointer("/temporal/cursor").cloned().unwrap_or(Value::Null),
        }));
        root_snapshots.push(json!({
            "project_id": project.project_id,
            "status": payload.get("status").cloned().unwrap_or(Value::Null),
            "outcome": payload.get("outcome").cloned().unwrap_or(Value::Null),
            "watermarks": payload.pointer("/temporal/watermarks").cloned().unwrap_or(Value::Null),
            "error": payload.get("error").cloned().unwrap_or(Value::Null),
        }));
    }
    let root_snapshots_digest = all_registered_digest(
        "tracedecay.mcp.message-search.all-registered.root-snapshots.v1",
        &root_snapshots,
    )?;
    if cursor
        .as_ref()
        .is_some_and(|cursor| cursor.root_snapshots_digest != root_snapshots_digest)
    {
        let mut payload = unavailable_all_registered_payload(
            request,
            "all_registered_cursor_stale",
            "the all_registered continuation no longer matches registered project retrieval state",
        )?;
        let map = payload_object_mut(&mut payload)?;
        map.insert("searched_project_count".to_string(), json!(searched));
        map.insert("skipped_project_count".to_string(), json!(skipped));
        map.insert("projects".to_string(), Value::Array(projects));
        return Ok(payload);
    }
    results.sort_by(compare_all_registered_results);
    let page_end = offset.saturating_add(request.limit).min(results.len());
    let has_more = results.len() > page_end || root_cursor_present;
    let omitted = results.len().saturating_sub(page_end);
    let results = if offset < page_end {
        results.drain(offset..page_end).collect()
    } else {
        Vec::new()
    };
    let complete_zero = results.is_empty();
    {
        let map = payload_object_mut(&mut merged)?;
        map.insert("project_scope".to_string(), json!("all_registered"));
        map.insert("count".to_string(), json!(results.len()));
        map.insert("results".to_string(), Value::Array(results));
        map.insert("searched_project_count".to_string(), json!(searched));
        map.insert("skipped_project_count".to_string(), json!(skipped));
        map.insert("catch_up_skipped_project_count".to_string(), json!(0));
        map.insert("projects".to_string(), Value::Array(projects));
        map.insert(
            "truncated".to_string(),
            json!(listing.truncated || has_more),
        );
        map.insert("omitted".to_string(), json!(omitted));
        if has_more {
            map.insert(
                "continuation".to_string(),
                json!(
                    AllRegisteredCursor::new(
                        page_end,
                        query_digest.clone(),
                        project_ids_digest.clone(),
                        root_snapshots_digest,
                    )?
                    .encode()?
                ),
            );
        }
    }
    if searched == 0 {
        apply_typed_error(
            &mut merged,
            "unavailable",
            "all_registered_search_unavailable",
            "no registered project could answer the session retrieval request",
        )?;
    } else if partial_or_stale || skipped > 0 || listing.truncated {
        let map = payload_object_mut(&mut merged)?;
        map.insert("status".to_string(), json!("partial"));
        map.insert("outcome".to_string(), json!("partial"));
    } else {
        let map = payload_object_mut(&mut merged)?;
        map.insert(
            "outcome".to_string(),
            json!(if complete_zero {
                "complete_zero"
            } else {
                "complete"
            }),
        );
    }
    Ok(merged)
}
