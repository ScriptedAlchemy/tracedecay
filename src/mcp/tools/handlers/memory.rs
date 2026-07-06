//! Cross-session and holographic memory handlers.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::automation::memory_digest::refresh_memory_digest_after_memory_change;
use crate::db::Database;
use crate::errors::{Result, TraceDecayError};
use crate::global_db::GlobalDb;
use crate::memory::retrieval::FactRetriever;
use crate::memory::store::MemoryStore;
use crate::memory::trust::DEFAULT_TRUST;
use crate::memory::types::{
    AddFactRequest, FactRecord, FactSearchResult, FeedbackAction, FeedbackRequest, MemoryCategory,
    SearchFactsRequest, UpdateFactRequest,
};
use crate::tracedecay::TraceDecay;

use super::super::ToolResult;
use super::super::render::{self, Md};
use super::support::{
    profile_root_for_global_db, project_registry_context, project_selector_present,
    safe_profile_relpath, string_array_values,
};

const DEFAULT_FACT_LIMIT: usize = 20;
const MAX_FACT_LIMIT: usize = 200;

struct TargetMemoryDb {
    db: Database,
    project_root: PathBuf,
}

fn text_tool_result(text: &str) -> ToolResult {
    ToolResult::new(
        json!({ "content": [{ "type": "text", "text": text }] }),
        vec![],
    )
}

fn rendered_tool_json(project_root: Option<&Path>, args: &Value, value: &Value) -> ToolResult {
    let text = render::finalize(project_root, args, value, || render::generic_md(value));
    text_tool_result(&text)
}

fn rendered_fact_store(project_root: Option<&Path>, args: &Value, value: &Value) -> ToolResult {
    let text = render::finalize(project_root, args, value, || fact_store_md(args, value));
    text_tool_result(&text)
}

async fn open_target_memory_db(
    cg: &TraceDecay,
    args: &Value,
    global_db: Option<&GlobalDb>,
    allow_default_registry_fallback: bool,
) -> Result<TargetMemoryDb> {
    let Some(context) = project_registry_context(
        args,
        &["project_path"],
        global_db,
        allow_default_registry_fallback,
    )
    .await?
    else {
        return Ok(TargetMemoryDb {
            db: cg.open_project_store_db().await?,
            project_root: cg.project_root().to_path_buf(),
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
    let (db, _) = Database::open(&db_path).await?;
    Ok(TargetMemoryDb {
        db,
        project_root: PathBuf::from(context.project.display_root),
    })
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
    if !tags.is_empty() {
        if let Some(map) = metadata.as_object_mut() {
            map.insert("tags".to_string(), json!(tags));
        }
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

fn fact_store_md(args: &Value, value: &Value) -> String {
    let mut md = Md::new();
    md.heading(2, "Fact Store");
    let action = render::field_str(value, "action");
    if !action.is_empty() {
        md.field("action", action);
    }
    append_fact_store_request(&mut md, args);
    if let Some(count) = value.get("count").and_then(Value::as_i64) {
        md.field("count", &count.to_string());
    }

    if let Some(removed) = value.get("removed").and_then(Value::as_bool) {
        md.field("removed", if removed { "true" } else { "false" });
    }
    append_fact_store_diff(&mut md, value);

    if let Some(fact) = value.get("fact").filter(|fact| fact.is_object()) {
        md.blank().heading(3, "Fact");
        append_fact_md(&mut md, fact, value);
    }

    if let Some(results) = value.get("results").and_then(Value::as_array) {
        md.blank().heading(3, "Facts");
        if results.is_empty() {
            md.empty_note("No matching facts.");
        } else {
            for result in results {
                append_fact_md(&mut md, fact_payload(result), result);
            }
        }
    }

    if let Some(history) = value.get("trust_history").and_then(Value::as_array) {
        md.blank().heading(3, "Trust History");
        if history.is_empty() {
            md.empty_note("No trust feedback recorded.");
        } else {
            for item in history.iter().take(10) {
                md.bullet(&compact_json_summary(item));
            }
            if history.len() > 10 {
                md.bullet(&format!("... {} more", history.len() - 10));
            }
        }
    }

    md.render()
}

fn append_fact_store_request(md: &mut Md, args: &Value) {
    for key in [
        "query",
        "entity",
        "category",
        "min_trust",
        "threshold",
        "limit",
    ] {
        let text = compact_scalar(args.get(key));
        if !text.is_empty() {
            md.field(key, &text);
        }
    }
    let entities = string_list(args.get("entities"));
    if !entities.is_empty() {
        md.field("entities", &entities.join(", "));
    }
}

fn append_fact_store_diff(md: &mut Md, value: &Value) {
    for key in ["diff", "closest_fact_id", "similarity", "reason", "error"] {
        let text = compact_scalar(value.get(key));
        if !text.is_empty() {
            md.field(key, &text);
        }
    }
}

fn fact_payload(value: &Value) -> &Value {
    value
        .get("fact")
        .filter(|fact| fact.is_object())
        .unwrap_or(value)
}

fn append_fact_md(md: &mut Md, fact: &Value, envelope: &Value) {
    let id = fact
        .get("fact_id")
        .and_then(Value::as_i64)
        .map(|id| format!("#{id}"))
        .unwrap_or_else(|| "#?".to_string());
    let category = compact_scalar(fact.get("category"));
    let trust = fact
        .get("trust_score")
        .and_then(Value::as_f64)
        .map(|score| format!("{score:.3}"))
        .unwrap_or_default();
    let content = compact_text(&render::field_str(fact, "content"));
    let mut head = id;
    if !category.is_empty() {
        head.push(' ');
        head.push_str(&category);
    }
    if !trust.is_empty() {
        head.push_str(" trust ");
        head.push_str(&trust);
    }
    if let Some(score) = envelope
        .get("score")
        .and_then(Value::as_f64)
        .map(|score| format!("{score:.3}"))
    {
        head.push_str(" score ");
        head.push_str(&score);
    }
    if !content.is_empty() {
        head.push_str(": ");
        head.push_str(&content);
    }
    md.bullet(&head);

    let detail = fact_detail_line(fact);
    if !detail.is_empty() {
        md.line(&format!("  {detail}"));
    }
    let why = compact_text(&render::field_str(envelope, "why"));
    if !why.is_empty() {
        md.line(&format!("  why: {why}"));
    }
}

fn fact_detail_line(fact: &Value) -> String {
    let mut parts = Vec::new();
    let entities = string_list(fact.get("entities"));
    if !entities.is_empty() {
        parts.push(format!("entities: {}", entities.join(", ")));
    }
    let tags = string_list(fact.get("tags"));
    if !tags.is_empty() {
        parts.push(format!("tags: {}", tags.join(", ")));
    }
    let source = compact_scalar(fact.get("source"));
    if !source.is_empty() {
        parts.push(format!("source: {source}"));
    }
    parts.join("; ")
}

fn string_list(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(compact_text)
                .filter(|text| !text.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

fn compact_scalar(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) => compact_text(text),
        Some(Value::Number(number)) => number.to_string(),
        Some(Value::Bool(true)) => "true".to_string(),
        Some(Value::Bool(false)) => "false".to_string(),
        _ => String::new(),
    }
}

fn compact_text(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn compact_json_summary(value: &Value) -> String {
    match value {
        Value::Object(map) => {
            let mut parts = Vec::new();
            for key in ["created_at", "trust_score", "feedback", "source", "note"] {
                let text = compact_scalar(map.get(key));
                if !text.is_empty() {
                    parts.push(format!("{key}: {text}"));
                }
            }
            if parts.is_empty() {
                serde_json::to_string(value).unwrap_or_default()
            } else {
                parts.join("; ")
            }
        }
        _ => compact_scalar(Some(value)),
    }
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

pub(super) async fn handle_fact_store(
    cg: &TraceDecay,
    args: Value,
    global_db: Option<&GlobalDb>,
    allow_default_registry_fallback: bool,
) -> Result<ToolResult> {
    let action = required_str(&args, "action")?;
    let cross_project_selector = project_selector_present(&args, &["project_path"]);
    if action_mutates_memory(action) && cross_project_selector {
        return Err(config_error(
            "cross-project fact_store writes are not supported; omit project_selector to write the active project",
        ));
    }
    let target_memory =
        open_target_memory_db(cg, &args, global_db, allow_default_registry_fallback).await?;
    let conn = target_memory.db.conn();
    let store = MemoryStore::new(conn);
    let mut refresh_digest = false;
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
                        tags: string_array_values(&args, "tags"),
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
                tags: args.get("tags").map(|_| string_array_values(&args, "tags")),
                entities: args.get("entities").map(|_| request_entities(&args)),
                trust: update_trust(&args, &store, id).await?,
                source: args
                    .get("source")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned),
                metadata: args.get("metadata").cloned(),
            };
            match store.update_fact(update).await {
                Ok(fact) => {
                    refresh_digest = true;
                    json!({ "action": action, "fact": fact, "count": 1 })
                }
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
            refresh_digest = removed;
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
    if refresh_digest {
        refresh_memory_digest_after_memory_change(conn, &target_memory.project_root).await;
    }
    Ok(rendered_fact_store(
        Some(&target_memory.project_root),
        &args,
        &out,
    ))
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
    refresh_memory_digest_after_memory_change(db.conn(), cg.project_root()).await;
    let value = json!({ "status": "recorded", "feedback": result });
    Ok(rendered_tool_json(Some(cg.project_root()), &args, &value))
}

pub(super) async fn handle_memory_status(
    cg: &TraceDecay,
    args: Value,
    global_db: Option<&GlobalDb>,
    allow_default_registry_fallback: bool,
) -> Result<ToolResult> {
    let target_memory =
        open_target_memory_db(cg, &args, global_db, allow_default_registry_fallback).await?;
    let status = TraceDecay::memory_status_for_conn(target_memory.db.conn()).await?;
    let value = json!({ "status": "ok", "memory": status });
    Ok(rendered_tool_json(
        Some(&target_memory.project_root),
        &args,
        &value,
    ))
}
