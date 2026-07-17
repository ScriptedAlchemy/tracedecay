//! Cross-session and holographic memory handlers.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use tracedecay_domain::{FactOwnerV1, ProjectId};
use tracedecay_store::CompatibilityFeedbackRepairProgressV1;

use crate::application::memory::{
    MemoryApplication, MemoryApplicationError, MemoryOperationContext, V1UpdateFactOutcome,
};
use crate::automation::memory_digest::refresh_memory_digest_after_memory_change;
use crate::db::Database;
use crate::errors::{Result, TraceDecayError};
use crate::global_db::GlobalDb;
use crate::memory::types::{
    AddFactRequest, FeedbackAction, FeedbackRequest, MemoryCategory, SearchFactsRequest,
    UpdateFactRequest,
};
use crate::memory::user::open_user_memory_db;
use crate::store::DatabaseFactStore;
use crate::tracedecay::TraceDecay;

use super::super::ToolResult;
use super::super::{render, renderers};
use super::support::{
    profile_root_for_global_db, project_registry_context, project_selector_present,
    safe_profile_relpath, string_array_values,
};
use super::{rendered_tool_json, text_tool_result};

const DEFAULT_FACT_LIMIT: usize = 20;
const MAX_FACT_LIMIT: usize = 200;

enum TargetMemoryDbHandle<'a> {
    Active(&'a Database),
    Owned(Box<Database>),
}

pub(super) struct TargetMemoryDb<'a> {
    db: TargetMemoryDbHandle<'a>,
    pub(super) project_root: PathBuf,
    pub(super) user_scope: bool,
    owner: FactOwnerV1,
}

impl TargetMemoryDb<'_> {
    fn db(&self) -> &Database {
        match &self.db {
            TargetMemoryDbHandle::Active(db) => db,
            TargetMemoryDbHandle::Owned(db) => db,
        }
    }

    pub(super) fn conn(&self) -> &libsql::Connection {
        self.db().conn()
    }

    pub(super) fn owner(&self) -> &FactOwnerV1 {
        &self.owner
    }
}

fn requests_user_memory(args: &Value) -> bool {
    args.get("memory_scope").and_then(Value::as_str) == Some("user")
}

async fn open_user_memory_target(profile_root: &Path) -> Result<TargetMemoryDb<'static>> {
    Ok(TargetMemoryDb {
        db: TargetMemoryDbHandle::Owned(Box::new(open_user_memory_db(profile_root).await?)),
        project_root: profile_root.to_path_buf(),
        user_scope: true,
        owner: FactOwnerV1::Profile,
    })
}

fn project_memory_owner(project_id: &str) -> Result<FactOwnerV1> {
    let project_id = ProjectId::new(project_id.to_owned())
        .map_err(|error| config_error(format!("invalid project memory owner: {error}")))?;
    Ok(FactOwnerV1::Project { project_id })
}

fn active_project_memory_owner(cg: &TraceDecay) -> Result<FactOwnerV1> {
    let project_id = cg
        .store_layout()
        .identity
        .project_id
        .as_deref()
        .ok_or_else(|| config_error("active project has no authoritative project_id"))?;
    project_memory_owner(project_id)
}

fn rendered_fact_store(project_root: Option<&Path>, args: &Value, value: &Value) -> ToolResult {
    let text = render::finalize(project_root, args, value, || {
        renderers::fact_store_md(args, value)
    });
    text_tool_result(&text)
}

pub(super) async fn open_target_memory_db<'a>(
    cg: &'a TraceDecay,
    args: &Value,
    global_db: Option<&GlobalDb>,
    allow_default_registry_fallback: bool,
) -> Result<TargetMemoryDb<'a>> {
    if requests_user_memory(args) {
        if project_selector_present(args, &["project_path"]) {
            return Err(config_error(
                "memory_scope=user cannot be combined with a project selector",
            ));
        }
        let profile_root = profile_root_for_global_db(global_db, allow_default_registry_fallback)?;
        return open_user_memory_target(&profile_root).await;
    }
    let Some(context) = project_registry_context(
        args,
        &["project_path"],
        global_db,
        allow_default_registry_fallback,
    )
    .await?
    else {
        let db = if cg.db_path() == cg.store_layout().graph_db_path {
            TargetMemoryDbHandle::Active(cg.db())
        } else {
            TargetMemoryDbHandle::Owned(Box::new(cg.open_project_store_db().await?))
        };
        return Ok(TargetMemoryDb {
            db,
            project_root: cg.project_root().to_path_buf(),
            user_scope: false,
            owner: active_project_memory_owner(cg)?,
        });
    };
    let profile_root = profile_root_for_global_db(global_db, allow_default_registry_fallback)?;
    let graph_relpath = context
        .stores
        .iter()
        .flat_map(|store| store.artifacts.iter())
        .find(|artifact| artifact.artifact_kind == "graph_db")
        .map(|artifact| artifact.relpath.as_str())
        .ok_or_else(|| {
            config_error(format!(
                "project {} has no registered graph_db artifact",
                context.project.project_id
            ))
        })?;
    let db_path = profile_root.join(safe_profile_relpath(graph_relpath)?);
    if !db_path.is_file() {
        return Err(config_error(format!(
            "registered graph_db artifact does not exist: {}",
            db_path.display()
        )));
    }
    let authority = crate::db::DatabaseAuthority::for_runtime(&db_path, "open memory target")?;
    let (db, _) = Database::open(&db_path, &authority).await?;
    Ok(TargetMemoryDb {
        db: TargetMemoryDbHandle::Owned(Box::new(db)),
        project_root: PathBuf::from(context.project.display_root),
        user_scope: false,
        owner: project_memory_owner(&context.project.project_id)?,
    })
}

fn config_error(message: impl Into<String>) -> TraceDecayError {
    TraceDecayError::Config {
        message: message.into(),
    }
}

fn memory_application_error(error: MemoryApplicationError) -> TraceDecayError {
    TraceDecayError::database_operation("memory application", error)
}

fn memory_application<'a>(
    target_memory: &'a TargetMemoryDb<'_>,
) -> Result<MemoryApplication<DatabaseFactStore<'a>>> {
    MemoryApplication::new(
        target_memory.owner().clone(),
        DatabaseFactStore::new(target_memory.db()),
    )
    .map_err(memory_application_error)
}

fn memory_operation_context(
    args: &Value,
    target_memory: &TargetMemoryDb<'_>,
    action: &str,
) -> Result<MemoryOperationContext> {
    // `McpServer` overwrites this private field from the JSON-RPC id for
    // mutations and retrieval-accounting actions. Direct non-retriable calls
    // deliberately receive a fresh opaque operation identity instead.
    match args.get("__mcp_request_id").and_then(Value::as_str) {
        Some(request_id) => MemoryOperationContext::from_trusted_request_id(
            target_memory.owner(),
            action,
            request_id,
            None,
        ),
        None => MemoryOperationContext::generated(target_memory.owner(), action, None),
    }
    .map_err(memory_application_error)
}

fn required_str<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| config_error(format!("missing required parameter: {key}")))
}

fn optional_category(args: &Value) -> Result<Option<MemoryCategory>> {
    args.get("category")
        .and_then(Value::as_str)
        .map(str::parse::<MemoryCategory>)
        .transpose()
        .map_err(|e| config_error(format!("invalid category: {e}")))
}

fn limit(args: &Value) -> usize {
    args.get("limit")
        .and_then(Value::as_u64)
        .map_or(DEFAULT_FACT_LIMIT, |n| {
            (n as usize).clamp(1, MAX_FACT_LIMIT)
        })
}

fn optional_f64(args: &Value, key: &str) -> Option<f64> {
    args.get(key).and_then(Value::as_f64)
}

fn fact_id(args: &Value) -> Result<i64> {
    let value = args
        .get("fact_id")
        .or_else(|| args.get("id"))
        .ok_or_else(|| config_error("missing required parameter: fact_id"))?;
    if let Some(id) = value.as_i64() {
        return Ok(id);
    }
    value
        .as_str()
        .and_then(|s| s.parse::<i64>().ok())
        .ok_or_else(|| config_error("fact_id must be a number or numeric string"))
}

fn metadata_with_tags(args: &Value) -> Value {
    let mut metadata = args
        .get("metadata")
        .cloned()
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}));
    let tags = string_array_values(args, "tags");
    if !tags.is_empty()
        && let Some(map) = metadata.as_object_mut()
    {
        map.insert("tags".to_string(), json!(tags));
    }
    metadata
}

fn request_entities(args: &Value) -> Vec<String> {
    let mut entities = string_array_values(args, "entities");
    if let Some(entity) = args.get("entity").and_then(Value::as_str) {
        entities.push(entity.to_string());
    }
    entities
}

fn feedback_action(args: &Value) -> Result<FeedbackAction> {
    if let Some(action) = args.get("action").and_then(Value::as_str) {
        return match action {
            "helpful" => Ok(FeedbackAction::Helpful),
            "unhelpful" => Ok(FeedbackAction::Unhelpful),
            other => Err(config_error(format!("unknown feedback action: {other}"))),
        };
    }
    match (
        args.get("helpful")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        args.get("unhelpful")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    ) {
        (true, false) => Ok(FeedbackAction::Helpful),
        (false, true) => Ok(FeedbackAction::Unhelpful),
        _ => Err(config_error(
            "missing feedback action: set action, helpful, or unhelpful",
        )),
    }
}

fn results_envelope(action: &str, results: &Value, count: usize) -> Value {
    json!({
        "action": action,
        "results": results,
        "facts": results,
        "count": count,
    })
}

fn feedback_history_repair_payload(progress: CompatibilityFeedbackRepairProgressV1) -> Value {
    let state = match progress {
        CompatibilityFeedbackRepairProgressV1::Unknown => "unknown",
        CompatibilityFeedbackRepairProgressV1::NotRequired => "not_required",
        CompatibilityFeedbackRepairProgressV1::Complete { .. } => "complete",
        CompatibilityFeedbackRepairProgressV1::Incomplete { .. } => "incomplete",
    };
    json!({
        "state": state,
        "processed": progress.processed(),
        "remaining": progress.remaining(),
    })
}

fn action_writes_memory(action: &str) -> bool {
    matches!(action, "add" | "update" | "remove")
}
async fn update_trust(
    args: &Value,
    memory: &MemoryApplication<DatabaseFactStore<'_>>,
    fact_id: i64,
) -> Result<Option<f64>> {
    if let Some(trust) = optional_f64(args, "trust") {
        return Ok(Some(trust));
    }
    let Some(delta) = optional_f64(args, "trust_delta") else {
        return Ok(None);
    };
    let existing = memory
        .get_fact_v1(fact_id)
        .await
        .map_err(memory_application_error)?
        .ok_or_else(|| config_error(format!("fact {fact_id} not found")))?;
    Ok(Some((existing.trust_score + delta).clamp(0.0, 1.0)))
}

pub(super) async fn handle_fact_store(
    cg: &TraceDecay,
    args: Value,
    global_db: Option<&GlobalDb>,
    allow_default_registry_fallback: bool,
) -> Result<ToolResult> {
    let action = required_str(&args, "action")?;
    let cross_project_selector = project_selector_present(&args, &["project_path"]);
    if action_writes_memory(action) && cross_project_selector {
        return Err(config_error(
            "cross-project fact_store writes are not supported; omit project_selector to write the active project",
        ));
    }
    let target_memory =
        open_target_memory_db(cg, &args, global_db, allow_default_registry_fallback).await?;
    handle_fact_store_for_target(args, cross_project_selector, target_memory).await
}

async fn handle_fact_store_for_target(
    args: Value,
    cross_project_selector: bool,
    target_memory: TargetMemoryDb<'_>,
) -> Result<ToolResult> {
    let action = required_str(&args, "action")?;
    let memory = memory_application(&target_memory)?;
    let mut refresh_digest = false;
    let out = match action {
        "add" => {
            let request = AddFactRequest {
                content: required_str(&args, "content")?.to_string(),
                category: optional_category(&args)?.unwrap_or(MemoryCategory::General),
                source: args
                    .get("source")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned),
                tags: string_array_values(&args, "tags"),
                entities: request_entities(&args),
                trust: optional_f64(&args, "trust"),
                metadata: metadata_with_tags(&args),
            };
            let outcome = memory
                .add_fact_v1(
                    request,
                    memory_operation_context(&args, &target_memory, "add")?,
                )
                .await
                .map_err(memory_application_error)?;
            // Additive write-time diff report fields, so writers SEE
            // near-duplicates, possible conflicts, and secret rejections.
            let count = usize::from(outcome.fact.is_some());
            refresh_digest = count > 0;
            json!({
                "action": action,
                "fact": outcome.fact,
                "count": count,
                "diff": outcome.diff.diff.as_str(),
                "closest_fact_id": outcome.diff.closest_fact_id,
                "similarity": outcome.diff.similarity,
                "reason": outcome.diff.reason,
            })
        }
        "search" => {
            let request = SearchFactsRequest {
                query: required_str(&args, "query")?.to_string(),
                category: optional_category(&args)?,
                limit: Some(limit(&args)),
                min_trust: optional_f64(&args, "min_trust"),
                include_why: true,
            };
            let facts = if cross_project_selector {
                memory.search_facts_untracked_v1(request).await
            } else {
                memory
                    .search_facts_v1(
                        request,
                        memory_operation_context(&args, &target_memory, "search")?,
                    )
                    .await
            }
            .map_err(memory_application_error)?;
            let count = facts.len();
            results_envelope(action, &json!(facts), count)
        }
        "probe" => {
            let request = SearchFactsRequest {
                query: required_str(&args, "entity")?.to_owned(),
                category: optional_category(&args)?,
                limit: Some(limit(&args)),
                min_trust: optional_f64(&args, "min_trust"),
                include_why: true,
            };
            let facts = if cross_project_selector {
                memory.probe_facts_untracked_v1(request).await
            } else {
                memory
                    .probe_facts_v1(
                        request,
                        memory_operation_context(&args, &target_memory, "probe")?,
                    )
                    .await
            }
            .map_err(memory_application_error)?;
            let count = facts.len();
            results_envelope(action, &json!(facts), count)
        }
        "related" => {
            let request = SearchFactsRequest {
                query: required_str(&args, "entity")?.to_owned(),
                category: optional_category(&args)?,
                limit: Some(limit(&args)),
                min_trust: optional_f64(&args, "min_trust"),
                include_why: true,
            };
            let facts = if cross_project_selector {
                memory.related_facts_untracked_v1(request).await
            } else {
                memory
                    .related_facts_v1(
                        request,
                        memory_operation_context(&args, &target_memory, "related")?,
                    )
                    .await
            }
            .map_err(memory_application_error)?;
            let count = facts.len();
            results_envelope(action, &json!(facts), count)
        }
        "reason" => {
            let entities = request_entities(&args);
            let category = optional_category(&args)?;
            let min_trust = optional_f64(&args, "min_trust");
            let limit = limit(&args);
            let facts = if cross_project_selector {
                memory
                    .reason_facts_untracked_v1(entities, category, min_trust, limit)
                    .await
            } else {
                memory
                    .reason_facts_v1(
                        entities,
                        category,
                        min_trust,
                        limit,
                        memory_operation_context(&args, &target_memory, "reason")?,
                    )
                    .await
            }
            .map_err(memory_application_error)?;
            let count = facts.len();
            results_envelope(action, &json!(facts), count)
        }
        "contradict" => {
            let threshold = optional_f64(&args, "threshold").unwrap_or(0.3);
            let limit = limit(&args);
            let facts = memory
                .contradict_facts_v1(optional_category(&args)?, threshold, limit)
                .await
                .map_err(memory_application_error)?;
            let count = facts.len();
            results_envelope(action, &json!(facts), count)
        }
        "get" => {
            let id = fact_id(&args)?;
            let fact = memory
                .get_fact_v1(id)
                .await
                .map_err(memory_application_error)?
                .ok_or_else(|| config_error(format!("fact {id} not found")))?;
            let trust_history = memory
                .fact_trust_history_with_progress_v1(id, MAX_FACT_LIMIT)
                .await
                .map_err(memory_application_error)?;
            json!({
                "action": action,
                "fact": fact,
                "trust_history": trust_history.entries,
                "trust_history_availability": feedback_history_repair_payload(trust_history.repair_progress),
                "count": 1,
            })
        }
        "update" => {
            let id = fact_id(&args)?;
            let content = args
                .get("content")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned);
            let category = optional_category(&args)?;
            let tags = args.get("tags").map(|_| string_array_values(&args, "tags"));
            let entities = args.get("entities").map(|_| request_entities(&args));
            let source = args
                .get("source")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned);
            let metadata = args.get("metadata").cloned();
            let update = UpdateFactRequest {
                fact_id: id,
                content,
                category,
                tags,
                entities,
                trust: update_trust(&args, &memory, id).await?,
                source,
                metadata,
            };
            match memory
                .update_fact_v1(
                    update,
                    memory_operation_context(&args, &target_memory, "update")?,
                )
                .await
                .map_err(memory_application_error)?
            {
                V1UpdateFactOutcome::Updated(fact) => {
                    refresh_digest = true;
                    json!({ "action": action, "fact": fact, "count": 1 })
                }
                V1UpdateFactOutcome::RejectedSecretLike { reason } => json!({
                    "action": action,
                    "fact": Value::Null,
                    "count": 0,
                    "diff": "rejected_secret_like",
                    "reason": reason,
                    "error": reason,
                }),
            }
        }
        "remove" => {
            let id = fact_id(&args)?;
            let removed = memory
                .remove_fact_v1(
                    id,
                    memory_operation_context(&args, &target_memory, "remove")?,
                )
                .await
                .map_err(memory_application_error)?;
            refresh_digest = removed;
            json!({ "action": action, "removed": removed, "count": usize::from(removed) })
        }
        "list" => {
            let category = optional_category(&args)?;
            let min_trust = optional_f64(&args, "min_trust");
            let limit = limit(&args);
            let facts = if cross_project_selector {
                memory
                    .list_facts_untracked_v1(category, min_trust, limit)
                    .await
            } else {
                memory
                    .list_facts_v1(
                        category,
                        min_trust,
                        limit,
                        memory_operation_context(&args, &target_memory, "list")?,
                    )
                    .await
            }
            .map_err(memory_application_error)?;
            let count = facts.len();
            results_envelope(action, &json!(facts), count)
        }
        other => return Err(config_error(format!("unknown fact_store action: {other}"))),
    };
    if refresh_digest && !target_memory.user_scope {
        refresh_target_memory_digest(&memory, &target_memory).await;
    }
    Ok(rendered_fact_store(
        (!target_memory.user_scope).then_some(target_memory.project_root.as_path()),
        &args,
        &out,
    ))
}

async fn refresh_target_memory_digest(
    memory: &MemoryApplication<DatabaseFactStore<'_>>,
    target_memory: &TargetMemoryDb<'_>,
) {
    refresh_memory_digest_after_memory_change(memory, &target_memory.project_root).await;
}

pub(super) async fn handle_fact_feedback(
    cg: &TraceDecay,
    args: Value,
    global_db: Option<&GlobalDb>,
    allow_default_registry_fallback: bool,
) -> Result<ToolResult> {
    if project_selector_present(&args, &["project_path"]) {
        return Err(config_error(
            "cross-project fact_feedback writes are not supported; omit project_selector to write the active project",
        ));
    }
    let note = args
        .get("note")
        .or_else(|| args.get("reason"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let target_memory =
        open_target_memory_db(cg, &args, global_db, allow_default_registry_fallback).await?;
    let request = FeedbackRequest {
        fact_id: fact_id(&args)?,
        action: feedback_action(&args)?,
        source: args
            .get("source")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        note,
    };
    let memory = memory_application(&target_memory)?;
    let result = memory
        .record_fact_feedback_v1(
            request,
            memory_operation_context(&args, &target_memory, "feedback")?,
        )
        .await
        .map_err(memory_application_error)?;
    if !target_memory.user_scope {
        refresh_target_memory_digest(&memory, &target_memory).await;
    }
    let value = json!({ "status": "recorded", "feedback": result });
    Ok(rendered_tool_json(
        (!target_memory.user_scope).then_some(target_memory.project_root.as_path()),
        &args,
        &value,
    ))
}

pub(super) async fn handle_memory_status(
    cg: &TraceDecay,
    args: Value,
    global_db: Option<&GlobalDb>,
    allow_default_registry_fallback: bool,
) -> Result<ToolResult> {
    let target_memory =
        open_target_memory_db(cg, &args, global_db, allow_default_registry_fallback).await?;
    let status = memory_application(&target_memory)?
        .memory_status_with_repair_v1()
        .await
        .map_err(memory_application_error)?;
    let value = json!({
        "status": "ok",
        "memory": status.status,
        "feedback_history_repair": feedback_history_repair_payload(status.feedback_history_repair),
    });
    Ok(rendered_tool_json(
        (!target_memory.user_scope).then_some(target_memory.project_root.as_path()),
        &args,
        &value,
    ))
}

pub async fn handle_user_memory_tool(
    tool_name: &str,
    args: Value,
    profile_root: &Path,
) -> Result<ToolResult> {
    if !requests_user_memory(&args) {
        return Err(config_error(
            "projectless memory dispatch requires memory_scope=user",
        ));
    }
    let target_memory = open_user_memory_target(profile_root).await?;
    match tool_name {
        "tracedecay_fact_store" => {
            required_str(&args, "action")?;
            if project_selector_present(&args, &["project_path"]) {
                return Err(config_error(
                    "memory_scope=user cannot be combined with a project selector",
                ));
            }
            handle_fact_store_for_target(args, false, target_memory).await
        }
        "tracedecay_fact_feedback" => {
            let note = args
                .get("note")
                .or_else(|| args.get("reason"))
                .and_then(Value::as_str)
                .map(ToOwned::to_owned);
            let request = FeedbackRequest {
                fact_id: fact_id(&args)?,
                action: feedback_action(&args)?,
                source: args
                    .get("source")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned),
                note,
            };
            let result = memory_application(&target_memory)?
                .record_fact_feedback_v1(
                    request,
                    memory_operation_context(&args, &target_memory, "feedback")?,
                )
                .await
                .map_err(memory_application_error)?;
            Ok(rendered_tool_json(
                None,
                &args,
                &json!({ "status": "recorded", "feedback": result }),
            ))
        }
        "tracedecay_memory_status" => {
            let status = memory_application(&target_memory)?
                .memory_status_with_repair_v1()
                .await
                .map_err(memory_application_error)?;
            Ok(rendered_tool_json(
                None,
                &args,
                &json!({
                    "status": "ok",
                    "memory": status.status,
                    "feedback_history_repair": feedback_history_repair_payload(status.feedback_history_repair),
                }),
            ))
        }
        other => Err(config_error(format!("{other} is not a user-memory tool"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracedecay_store::{
        CompatibilityFactSearchCursorV1, CompatibilityFactSearchFilterV1,
        CompatibilityFactSearchKindV1, CompatibilityFactSearchQuery, FactStoreError,
    };

    fn cursor_fact(content: &str) -> AddFactRequest {
        AddFactRequest {
            content: content.to_owned(),
            category: MemoryCategory::General,
            source: None,
            tags: Vec::new(),
            entities: Vec::new(),
            trust: None,
            metadata: json!({}),
        }
    }

    fn cursor_search_query(
        owner: FactOwnerV1,
        query: &str,
        after: Option<CompatibilityFactSearchCursorV1>,
    ) -> std::result::Result<CompatibilityFactSearchQuery, FactStoreError> {
        CompatibilityFactSearchQuery::with_filter(
            owner,
            CompatibilityFactSearchKindV1::Search,
            Some(query.to_owned()),
            CompatibilityFactSearchFilterV1::new(None, None, None)?,
            after,
            1,
        )
    }

    fn active_memory(cg: &TraceDecay) -> MemoryApplication<DatabaseFactStore<'_>> {
        MemoryApplication::new(
            active_project_memory_owner(cg).unwrap(),
            DatabaseFactStore::new(cg.db()),
        )
        .unwrap()
    }

    async fn empty_memory() -> (tempfile::TempDir, TraceDecay) {
        let tmp = tempfile::tempdir().unwrap();
        let project_root = tmp.path().join("project");
        let profile_root = tmp.path().join("profile");
        std::fs::create_dir_all(&project_root).unwrap();
        let cg = TraceDecay::init_with_options(
            &project_root,
            crate::tracedecay::TraceDecayOpenOptions {
                global_db_path: Some(profile_root.join("global.db")),
                profile_root: Some(profile_root),
            },
        )
        .await
        .unwrap();
        (tmp, cg)
    }

    async fn seeded_memory() -> (tempfile::TempDir, TraceDecay, i64) {
        let (tmp, cg) = empty_memory().await;
        let owner = active_project_memory_owner(&cg).unwrap();
        let fact_id = active_memory(&cg)
            .add_fact_v1(
                AddFactRequest {
                    content: "existing fact".to_string(),
                    category: MemoryCategory::General,
                    source: None,
                    tags: Vec::new(),
                    entities: Vec::new(),
                    trust: None,
                    metadata: json!({}),
                },
                MemoryOperationContext::generated(&owner, "test-seed", None).unwrap(),
            )
            .await
            .unwrap()
            .fact
            .unwrap()
            .fact_id;
        (tmp, cg, fact_id)
    }

    #[tokio::test]
    async fn active_project_memory_uses_the_served_database_handle() {
        let tmp = tempfile::tempdir().unwrap();
        let project_root = tmp.path().join("project");
        let profile_root = tmp.path().join("profile");
        std::fs::create_dir_all(&project_root).unwrap();
        let cg = TraceDecay::init_with_options(
            &project_root,
            crate::tracedecay::TraceDecayOpenOptions {
                global_db_path: Some(profile_root.join("global.db")),
                profile_root: Some(profile_root),
            },
        )
        .await
        .unwrap();

        let target = open_target_memory_db(&cg, &json!({}), None, true)
            .await
            .unwrap();

        assert!(matches!(target.db, TargetMemoryDbHandle::Active(_)));
        assert!(std::ptr::eq(target.conn(), cg.db().conn()));
        assert_eq!(
            target.owner(),
            &project_memory_owner(cg.store_layout().identity.project_id.as_deref().unwrap(),)
                .unwrap()
        );
    }

    #[tokio::test]
    async fn feedback_rejects_cross_project_write_before_opening_a_store() {
        let (_tmp, cg, fact_id) = seeded_memory().await;

        let error = handle_fact_feedback(
            &cg,
            json!({
                "fact_id": fact_id,
                "action": "helpful",
                "project_id": "another_project",
            }),
            None,
            true,
        )
        .await
        .unwrap_err();

        assert!(matches!(
            error,
            TraceDecayError::Config { ref message }
                if message.contains("cross-project fact_feedback writes")
        ));
    }

    #[tokio::test]
    async fn fact_feedback_without_source_keeps_legacy_mcp_history() {
        let (_tmp, cg, fact_id) = seeded_memory().await;

        handle_fact_feedback(
            &cg,
            json!({ "fact_id": fact_id, "action": "helpful" }),
            None,
            true,
        )
        .await
        .unwrap();

        let history = active_memory(&cg)
            .fact_trust_history_v1(fact_id, MAX_FACT_LIMIT)
            .await
            .unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].source, "mcp");
    }

    #[tokio::test]
    async fn trusted_fact_feedback_id_replays_without_duplicate_history() {
        let (_tmp, cg, fact_id) = seeded_memory().await;
        let args = json!({
            "fact_id": fact_id,
            "action": "helpful",
            "__mcp_request_id": "same-feedback-json-rpc-request",
        });

        for _ in 0..2 {
            handle_fact_feedback(&cg, args.clone(), None, true)
                .await
                .unwrap();
        }

        let history = active_memory(&cg)
            .fact_trust_history_v1(fact_id, MAX_FACT_LIMIT)
            .await
            .unwrap();
        assert_eq!(history.len(), 1);
    }

    #[test]
    fn incomplete_feedback_history_repair_is_explicit() {
        assert_eq!(
            feedback_history_repair_payload(CompatibilityFeedbackRepairProgressV1::Incomplete {
                processed: 1,
                remaining: Some(2),
            }),
            json!({
                "state": "incomplete",
                "processed": 1,
                "remaining": 2,
            })
        );
    }

    #[tokio::test]
    async fn pure_fact_reads_do_not_wait_for_the_writer_lane() {
        let (_tmp, cg, fact_id) = seeded_memory().await;
        let transaction = cg
            .db()
            .begin_write_transaction("hold memory tool writer")
            .await
            .unwrap();
        transaction
            .execute(
                "UPDATE memory_facts SET content = 'uncommitted fact' WHERE fact_id = ?1",
                [fact_id],
            )
            .await
            .unwrap();
        let target = TargetMemoryDb {
            db: TargetMemoryDbHandle::Active(cg.db()),
            project_root: cg.project_root().to_path_buf(),
            user_scope: true,
            owner: active_project_memory_owner(&cg).unwrap(),
        };

        let result = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            handle_fact_store_for_target(
                json!({ "action": "get", "fact_id": fact_id }),
                false,
                target,
            ),
        )
        .await
        .expect("pure reads must not wait for writer authority")
        .unwrap();
        let rendered = result.value.to_string();
        assert!(rendered.contains("existing fact"), "{rendered}");
        assert!(!rendered.contains("uncommitted fact"), "{rendered}");
        transaction.rollback().await.unwrap();
    }

    #[tokio::test]
    async fn local_fact_search_records_retrieval_without_snapshot_deadlock() {
        let (_tmp, cg, fact_id) = seeded_memory().await;
        let target = TargetMemoryDb {
            db: TargetMemoryDbHandle::Active(cg.db()),
            project_root: cg.project_root().to_path_buf(),
            user_scope: true,
            owner: active_project_memory_owner(&cg).unwrap(),
        };

        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            handle_fact_store_for_target(
                json!({ "action": "search", "query": "existing fact" }),
                false,
                target,
            ),
        )
        .await
        .expect("local retrieval-counting actions must not hold a read snapshot")
        .unwrap();

        let fact = active_memory(&cg)
            .get_fact_v1(fact_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(fact.retrieval_count, 1);
        assert_eq!(fact.access_count, 1);
    }

    #[tokio::test]
    async fn fact_mutations_wait_for_the_writer_lane_before_starting_a_transaction() {
        let (_tmp, cg, _) = seeded_memory().await;
        let writer = cg
            .db()
            .writer_connection("hold memory mutation writer")
            .await
            .unwrap();
        let target = TargetMemoryDb {
            db: TargetMemoryDbHandle::Active(cg.db()),
            project_root: cg.project_root().to_path_buf(),
            user_scope: true,
            owner: active_project_memory_owner(&cg).unwrap(),
        };
        let mut add = Box::pin(handle_fact_store_for_target(
            json!({ "action": "add", "content": "concurrent fact" }),
            false,
            target,
        ));

        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(25), &mut add)
                .await
                .is_err()
        );
        drop(writer);
        add.await.unwrap();
        assert_eq!(
            active_memory(&cg)
                .list_facts_untracked_v1(None, None, 10)
                .await
                .unwrap()
                .len(),
            2
        );
    }

    #[tokio::test]
    async fn retrieval_counter_writes_wait_for_the_writer_lane() {
        let (_tmp, cg, fact_id) = seeded_memory().await;
        let writer = cg
            .db()
            .writer_connection("hold memory retrieval writer")
            .await
            .unwrap();
        let target = TargetMemoryDb {
            db: TargetMemoryDbHandle::Active(cg.db()),
            project_root: cg.project_root().to_path_buf(),
            user_scope: true,
            owner: active_project_memory_owner(&cg).unwrap(),
        };
        let mut record = Box::pin(handle_fact_store_for_target(
            json!({ "action": "search", "query": "existing fact" }),
            false,
            target,
        ));

        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(25), &mut record)
                .await
                .is_err()
        );
        drop(writer);
        record.await.unwrap();
        assert_eq!(
            active_memory(&cg)
                .get_fact_v1(fact_id)
                .await
                .unwrap()
                .unwrap()
                .retrieval_count,
            1
        );
        assert_eq!(
            active_memory(&cg)
                .get_fact_v1(fact_id)
                .await
                .unwrap()
                .unwrap()
                .access_count,
            1
        );
    }

    #[tokio::test]
    async fn trusted_memory_retrieval_id_replays_without_double_counting() {
        let (_tmp, cg, fact_id) = seeded_memory().await;
        for _ in 0..2 {
            let target = TargetMemoryDb {
                db: TargetMemoryDbHandle::Active(cg.db()),
                project_root: cg.project_root().to_path_buf(),
                user_scope: true,
                owner: active_project_memory_owner(&cg).unwrap(),
            };
            handle_fact_store_for_target(
                json!({
                    "action": "search",
                    "query": "existing fact",
                    "__mcp_request_id": "same-json-rpc-request",
                }),
                false,
                target,
            )
            .await
            .unwrap();
        }
        let fact = active_memory(&cg)
            .get_fact_v1(fact_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(fact.retrieval_count, 1);
        assert_eq!(fact.access_count, 1);
    }

    #[tokio::test]
    async fn cross_project_memory_retrieval_is_untracked() {
        let (_tmp, cg, fact_id) = seeded_memory().await;
        let target = TargetMemoryDb {
            db: TargetMemoryDbHandle::Active(cg.db()),
            project_root: cg.project_root().to_path_buf(),
            user_scope: true,
            owner: active_project_memory_owner(&cg).unwrap(),
        };
        handle_fact_store_for_target(
            json!({ "action": "search", "query": "existing fact" }),
            true,
            target,
        )
        .await
        .unwrap();
        let fact = active_memory(&cg)
            .get_fact_v1(fact_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(fact.retrieval_count, 0);
        assert_eq!(fact.access_count, 0);
    }

    #[tokio::test]
    async fn compatibility_search_cursor_replays_and_rejects_other_owners() {
        let (_tmp, cg) = empty_memory().await;
        let owner = active_project_memory_owner(&cg).unwrap();
        let memory = active_memory(&cg);
        for (operation, content) in [
            (
                "test-project-cursor-one",
                "cursor fixture marigold topology",
            ),
            ("test-project-cursor-two", "cursor fixture basalt workflow"),
        ] {
            assert!(
                memory
                    .add_fact_v1(
                        cursor_fact(content),
                        MemoryOperationContext::generated(&owner, operation, None).unwrap(),
                    )
                    .await
                    .unwrap()
                    .fact
                    .is_some(),
                "{operation} must persist a real fixture fact"
            );
        }

        let first_page = memory
            .search_compatibility_facts(
                cursor_search_query(owner.clone(), "cursor fixture", None).unwrap(),
            )
            .await
            .unwrap();
        let cursor = first_page
            .next_after()
            .cloned()
            .expect("the first finite page must provide its real cursor");
        let second_page = memory
            .search_compatibility_facts(
                cursor_search_query(owner.clone(), "cursor fixture", Some(cursor.clone())).unwrap(),
            )
            .await
            .unwrap();
        let replay_page = memory
            .search_compatibility_facts(
                cursor_search_query(owner.clone(), "cursor fixture", Some(cursor)).unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(first_page.owner(), &owner);
        assert_eq!(first_page.hits().len(), 1);
        assert_eq!(second_page.hits().len(), 1);
        assert!(
            second_page.next_after().is_none(),
            "the two real fixture facts must exhaust the second page"
        );
        assert_ne!(
            first_page.hits()[0].fact().fact_id(),
            second_page.hits()[0].fact().fact_id()
        );
        let first = &first_page.hits()[0];
        let second = &second_page.hits()[0];
        assert!(
            first.score_millionths() > second.score_millionths()
                || (first.score_millionths() == second.score_millionths()
                    && (first.fact().telemetry().updated_at()
                        > second.fact().telemetry().updated_at()
                        || (first.fact().telemetry().updated_at()
                            == second.fact().telemetry().updated_at()
                            && first.fact().fact_id() < second.fact().fact_id()))),
            "search pages must preserve canonical score, timestamp, and fact-id ordering"
        );
        assert_eq!(replay_page, second_page);

        let profile_owner = FactOwnerV1::Profile;
        let profile_memory =
            MemoryApplication::new(profile_owner.clone(), DatabaseFactStore::new(cg.db())).unwrap();
        for (operation, content) in [
            (
                "test-profile-cursor-one",
                "profile cursor fixture violet semantics",
            ),
            (
                "test-profile-cursor-two",
                "profile cursor fixture amber provenance",
            ),
        ] {
            assert!(
                profile_memory
                    .add_fact_v1(
                        cursor_fact(content),
                        MemoryOperationContext::generated(&profile_owner, operation, None).unwrap(),
                    )
                    .await
                    .unwrap()
                    .fact
                    .is_some(),
                "{operation} must persist a real fixture fact"
            );
        }
        let profile_first_page = profile_memory
            .search_compatibility_facts(
                cursor_search_query(profile_owner, "profile cursor fixture", None).unwrap(),
            )
            .await
            .unwrap();
        let foreign_cursor = profile_first_page
            .next_after()
            .cloned()
            .expect("the profile page must provide its real cursor");
        assert!(matches!(
            cursor_search_query(owner, "profile cursor fixture", Some(foreign_cursor)),
            Err(FactStoreError::OwnerMismatch)
        ));
    }
}
