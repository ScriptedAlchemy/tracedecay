//! Cross-session and holographic memory handlers.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::db::Database;
use crate::errors::{Result, TraceDecayError};
use crate::memory::retrieval::FactRetriever;
use crate::memory::store::MemoryStore;
use crate::memory::trust::DEFAULT_TRUST;
use crate::memory::types::{
    AddFactRequest, FactRecord, FactSearchResult, FeedbackAction, FeedbackRequest, MemoryCategory,
    SearchFactsRequest, UpdateFactRequest,
};
use crate::tracedecay::TraceDecay;

use super::super::render;
use super::super::ToolResult;
use super::{
    global_db_profile_root, project_registry_context, project_selector_present,
    safe_profile_relpath, truncated_json_envelope_with_handle,
};

const DEFAULT_FACT_LIMIT: usize = 20;
const MAX_FACT_LIMIT: usize = 200;

fn tool_json(project_root: Option<&Path>, value: &Value) -> ToolResult {
    let formatted = serde_json::to_string(value).unwrap_or_default();
    ToolResult {
        value: json!({ "content": [{ "type": "text", "text": truncated_json_envelope_with_handle(project_root, &formatted) }] }),
        touched_files: vec![],
    }
}

async fn open_target_memory_db(cg: &TraceDecay, args: &Value) -> Result<(Database, PathBuf)> {
    let Some(context) = project_registry_context(args, &["project_path"]).await? else {
        return Ok((
            cg.open_project_store_db().await?,
            cg.project_root().to_path_buf(),
        ));
    };
    let profile_root = global_db_profile_root()?;
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
    let (db, _) = Database::open(&db_path).await?;
    Ok((db, PathBuf::from(context.project.display_root)))
}

fn config_error(message: impl Into<String>) -> TraceDecayError {
    TraceDecayError::Config {
        message: message.into(),
    }
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

fn string_array(args: &Value, key: &str) -> Vec<String> {
    args.get(key)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(ToOwned::to_owned))
                .collect()
        })
        .unwrap_or_default()
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
    let tags = string_array(args, "tags");
    if !tags.is_empty() {
        if let Some(map) = metadata.as_object_mut() {
            map.insert("tags".to_string(), json!(tags));
        }
    }
    metadata
}

fn request_entities(args: &Value) -> Vec<String> {
    let mut entities = string_array(args, "entities");
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

fn fact_result_ids(results: &[FactSearchResult]) -> Vec<i64> {
    results.iter().map(|result| result.fact.fact_id).collect()
}

fn fact_ids(facts: &[FactRecord]) -> Vec<i64> {
    facts.iter().map(|fact| fact.fact_id).collect()
}

fn update_rejected_secret_like(err: &TraceDecayError) -> Option<String> {
    match err {
        TraceDecayError::Database { message, operation }
            if operation == "update_fact" && message.contains("rejected_secret_like") =>
        {
            Some(message.clone())
        }
        _ => None,
    }
}

fn action_mutates_memory(action: &str) -> bool {
    matches!(action, "add" | "update" | "remove")
}

async fn record_retrieval_counts(
    store: &MemoryStore<'_>,
    cross_project_selector: bool,
    ids: &[i64],
) -> Result<()> {
    if !cross_project_selector {
        store.increment_retrieval_counts(ids).await?;
    }
    Ok(())
}

async fn search_results_envelope(
    store: &MemoryStore<'_>,
    cross_project_selector: bool,
    action: &str,
    facts: Vec<FactSearchResult>,
) -> Result<Value> {
    let ids = fact_result_ids(&facts);
    record_retrieval_counts(store, cross_project_selector, &ids).await?;
    let count = facts.len();
    Ok(results_envelope(action, &json!(facts), count))
}

async fn fact_records_envelope(
    store: &MemoryStore<'_>,
    cross_project_selector: bool,
    action: &str,
    facts: Vec<FactRecord>,
) -> Result<Value> {
    let ids = fact_ids(&facts);
    record_retrieval_counts(store, cross_project_selector, &ids).await?;
    let count = facts.len();
    Ok(results_envelope(action, &json!(facts), count))
}

async fn update_trust(args: &Value, store: &MemoryStore<'_>, fact_id: i64) -> Result<Option<f64>> {
    if let Some(trust) = optional_f64(args, "trust") {
        return Ok(Some(trust));
    }
    let Some(delta) = optional_f64(args, "trust_delta") else {
        return Ok(None);
    };
    let existing = store
        .get_fact(fact_id)
        .await?
        .ok_or_else(|| config_error(format!("fact {fact_id} not found")))?;
    Ok(Some((existing.trust_score + delta).clamp(0.0, 1.0)))
}

pub(super) async fn handle_fact_store(cg: &TraceDecay, args: Value) -> Result<ToolResult> {
    let action = required_str(&args, "action")?;
    let cross_project_selector = project_selector_present(&args, &["project_path"]);
    if action_mutates_memory(action) && cross_project_selector {
        return Err(config_error(
            "cross-project fact_store writes are not supported; omit project_selector to write the active project",
        ));
    }
    let (db, target_root) = open_target_memory_db(cg, &args).await?;
    let conn = db.conn();
    let store = MemoryStore::new(conn);
    let out = match action {
        "add" => {
            let outcome = store
                .add_fact(
                    AddFactRequest {
                        content: required_str(&args, "content")?.to_string(),
                        category: optional_category(&args)?.unwrap_or(MemoryCategory::General),
                        source: args
                            .get("source")
                            .and_then(Value::as_str)
                            .map(ToOwned::to_owned),
                        tags: string_array(&args, "tags"),
                        entities: request_entities(&args),
                        trust: optional_f64(&args, "trust"),
                        metadata: metadata_with_tags(&args),
                    },
                    DEFAULT_TRUST,
                )
                .await?;
            // Additive write-time diff report fields, so writers SEE
            // near-duplicates, possible conflicts, and secret rejections.
            let count = usize::from(outcome.fact.is_some());
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
            let facts = FactRetriever::new(conn)
                .search(
                    &request.query,
                    request.category,
                    request.min_trust,
                    request.limit.unwrap_or(DEFAULT_FACT_LIMIT),
                )
                .await?;
            search_results_envelope(&store, cross_project_selector, action, facts).await?
        }
        "probe" => {
            let facts = FactRetriever::new(conn)
                .probe(
                    required_str(&args, "entity")?,
                    optional_category(&args)?,
                    optional_f64(&args, "min_trust"),
                    limit(&args),
                )
                .await?;
            search_results_envelope(&store, cross_project_selector, action, facts).await?
        }
        "related" => {
            let limit = limit(&args);
            let retriever = FactRetriever::new(conn);
            let related_entities = retriever
                .related(required_str(&args, "entity")?, limit)
                .await?;
            let mut seen = std::collections::HashSet::new();
            let mut facts = Vec::new();
            for related in related_entities {
                for result in retriever
                    .probe(
                        &related.name,
                        optional_category(&args)?,
                        optional_f64(&args, "min_trust"),
                        limit.saturating_mul(2),
                    )
                    .await?
                {
                    if seen.insert(result.fact.fact_id) {
                        facts.push(result);
                        if facts.len() >= limit.clamp(1, MAX_FACT_LIMIT) {
                            break;
                        }
                    }
                }
                if facts.len() >= limit.clamp(1, MAX_FACT_LIMIT) {
                    break;
                }
            }
            search_results_envelope(&store, cross_project_selector, action, facts).await?
        }
        "reason" => {
            let entities = request_entities(&args);
            let facts = FactRetriever::new(conn)
                .reason(
                    &entities,
                    optional_category(&args)?,
                    optional_f64(&args, "min_trust"),
                    limit(&args),
                )
                .await?;
            search_results_envelope(&store, cross_project_selector, action, facts).await?
        }
        "contradict" => {
            let threshold = optional_f64(&args, "threshold").unwrap_or(0.3);
            let limit = limit(&args);
            let retriever = FactRetriever::new(conn);
            let facts = if let Some(category) = optional_category(&args)? {
                retriever.contradict(category, threshold, limit).await?
            } else {
                let mut out = Vec::new();
                for category in [
                    MemoryCategory::General,
                    MemoryCategory::UserPref,
                    MemoryCategory::Project,
                    MemoryCategory::Tool,
                    MemoryCategory::Decision,
                    MemoryCategory::CodeArea,
                ] {
                    out.extend(retriever.contradict(category, threshold, limit).await?);
                    if out.len() >= limit.clamp(1, MAX_FACT_LIMIT) {
                        out.truncate(limit.clamp(1, MAX_FACT_LIMIT));
                        break;
                    }
                }
                out
            };
            let count = facts.len();
            results_envelope(action, &json!(facts), count)
        }
        "get" => {
            let id = fact_id(&args)?;
            let fact = store
                .get_fact(id)
                .await?
                .ok_or_else(|| config_error(format!("fact {id} not found")))?;
            let trust_history = store.fact_trust_history(id).await?;
            json!({
                "action": action,
                "fact": fact,
                "trust_history": trust_history,
                "count": 1,
            })
        }
        "update" => {
            let id = fact_id(&args)?;
            let update = UpdateFactRequest {
                fact_id: id,
                content: args
                    .get("content")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned),
                category: optional_category(&args)?,
                tags: args.get("tags").map(|_| string_array(&args, "tags")),
                entities: args.get("entities").map(|_| request_entities(&args)),
                trust: update_trust(&args, &store, id).await?,
                source: args
                    .get("source")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned),
                metadata: args.get("metadata").cloned(),
            };
            match store.update_fact(update).await {
                Ok(fact) => json!({ "action": action, "fact": fact, "count": 1 }),
                Err(err) => {
                    if let Some(reason) = update_rejected_secret_like(&err) {
                        json!({
                            "action": action,
                            "fact": Value::Null,
                            "count": 0,
                            "diff": "rejected_secret_like",
                            "reason": reason,
                            "error": reason,
                        })
                    } else {
                        return Err(err);
                    }
                }
            }
        }
        "remove" => {
            let removed = store.remove_fact(fact_id(&args)?).await?;
            json!({ "action": action, "removed": removed, "count": usize::from(removed) })
        }
        "list" => {
            let facts = store
                .list_facts(
                    optional_category(&args)?,
                    optional_f64(&args, "min_trust"),
                    limit(&args),
                )
                .await?;
            fact_records_envelope(&store, cross_project_selector, action, facts).await?
        }
        other => return Err(config_error(format!("unknown fact_store action: {other}"))),
    };
    Ok(tool_json(Some(&target_root), &out))
}

pub(super) async fn handle_fact_feedback(cg: &TraceDecay, args: Value) -> Result<ToolResult> {
    let note = args
        .get("note")
        .or_else(|| args.get("reason"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let db = cg.open_project_store_db().await?;
    let result = MemoryStore::new(db.conn())
        .record_feedback_event(FeedbackRequest {
            fact_id: fact_id(&args)?,
            action: feedback_action(&args)?,
            source: args
                .get("source")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            note,
        })
        .await?;
    Ok(tool_json(
        Some(cg.project_root()),
        &json!({ "status": "recorded", "feedback": result }),
    ))
}

pub(super) async fn handle_memory_status(cg: &TraceDecay, args: Value) -> Result<ToolResult> {
    let (db, target_root) = open_target_memory_db(cg, &args).await?;
    let status = TraceDecay::memory_status_for_conn(db.conn()).await?;
    let value = json!({ "status": "ok", "memory": status });
    let text = render::finalize(Some(&target_root), &args, &value, || render::generic_md(&value));
    Ok(ToolResult {
        value: json!({ "content": [{ "type": "text", "text": text }] }),
        touched_files: vec![],
    })
}
