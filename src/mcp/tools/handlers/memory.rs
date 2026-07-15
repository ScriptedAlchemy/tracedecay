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
use crate::memory::user::open_user_memory_db;
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
}

fn requests_user_memory(args: &Value) -> bool {
    args.get("memory_scope").and_then(Value::as_str) == Some("user")
}

async fn open_user_memory_target(profile_root: &Path) -> Result<TargetMemoryDb<'static>> {
    Ok(TargetMemoryDb {
        db: TargetMemoryDbHandle::Owned(Box::new(open_user_memory_db(profile_root).await?)),
        project_root: profile_root.to_path_buf(),
        user_scope: true,
    })
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
    db: &Database,
    cross_project_selector: bool,
    ids: &[i64],
    recall: bool,
) -> Result<()> {
    if !cross_project_selector && !ids.is_empty() {
        let writer = db.writer_connection("record memory retrieval").await?;
        let store = writer.memory_store();
        if recall {
            let _ = store.record_fact_recalls(ids).await;
        }
        store.increment_retrieval_counts(ids).await?;
    }
    Ok(())
}

async fn search_results_envelope(
    db: &Database,
    cross_project_selector: bool,
    action: &str,
    facts: Vec<FactSearchResult>,
) -> Result<Value> {
    let ids = fact_result_ids(&facts);
    record_retrieval_counts(db, cross_project_selector, &ids, action == "search").await?;
    let count = facts.len();
    Ok(results_envelope(action, &json!(facts), count))
}

async fn fact_records_envelope(
    db: &Database,
    cross_project_selector: bool,
    action: &str,
    facts: Vec<FactRecord>,
) -> Result<Value> {
    let ids = fact_ids(&facts);
    record_retrieval_counts(db, cross_project_selector, &ids, false).await?;
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
    handle_fact_store_for_target(args, cross_project_selector, target_memory).await
}

async fn handle_fact_store_for_target(
    args: Value,
    cross_project_selector: bool,
    target_memory: TargetMemoryDb<'_>,
) -> Result<ToolResult> {
    let action = required_str(&args, "action")?;
    let db = target_memory.db();
    let reader = if action_mutates_memory(action) {
        None
    } else {
        Some(
            db.begin_isolated_read_snapshot("read memory tool facts")
                .await?,
        )
    };
    let conn = reader.as_ref().map_or(db.conn(), |reader| reader);
    let store = MemoryStore::new(conn);
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
            let writer = db.writer_connection("add memory tool fact").await?;
            let outcome = writer
                .memory_store()
                .add_fact(request, DEFAULT_TRUST)
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
                .search_untracked(
                    &request.query,
                    request.category,
                    request.min_trust,
                    request.limit.unwrap_or(DEFAULT_FACT_LIMIT),
                )
                .await?;
            search_results_envelope(db, cross_project_selector, action, facts).await?
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
            search_results_envelope(db, cross_project_selector, action, facts).await?
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
            search_results_envelope(db, cross_project_selector, action, facts).await?
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
            search_results_envelope(db, cross_project_selector, action, facts).await?
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
            let result = {
                let writer = db.writer_connection("update memory tool fact").await?;
                let store = writer.memory_store();
                let update = UpdateFactRequest {
                    fact_id: id,
                    content,
                    category,
                    tags,
                    entities,
                    trust: update_trust(&args, &store, id).await?,
                    source,
                    metadata,
                };
                store.update_fact(update).await
            };
            match result {
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
            let id = fact_id(&args)?;
            let writer = db.writer_connection("remove memory tool fact").await?;
            let removed = writer.memory_store().remove_fact(id).await?;
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
            fact_records_envelope(db, cross_project_selector, action, facts).await?
        }
        other => return Err(config_error(format!("unknown fact_store action: {other}"))),
    };
    if refresh_digest && !target_memory.user_scope {
        refresh_target_memory_digest(&target_memory).await;
    }
    Ok(rendered_fact_store(
        (!target_memory.user_scope).then_some(target_memory.project_root.as_path()),
        &args,
        &out,
    ))
}

async fn refresh_target_memory_digest(target_memory: &TargetMemoryDb<'_>) {
    match target_memory
        .db()
        .begin_isolated_read_snapshot("refresh memory tool digest")
        .await
    {
        Ok(reader) => {
            refresh_memory_digest_after_memory_change(&reader, &target_memory.project_root).await;
        }
        Err(error) => eprintln!("warning: memory digest refresh failed: {error}"),
    }
}

pub(super) async fn handle_fact_feedback(
    cg: &TraceDecay,
    args: Value,
    global_db: Option<&GlobalDb>,
    allow_default_registry_fallback: bool,
) -> Result<ToolResult> {
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
    let writer = target_memory
        .db()
        .writer_connection("record memory tool feedback")
        .await?;
    let result = writer.memory_store().record_feedback_event(request).await?;
    if !target_memory.user_scope {
        refresh_target_memory_digest(&target_memory).await;
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
    let status = TraceDecay::memory_status_for_db(target_memory.db()).await?;
    let value = json!({ "status": "ok", "memory": status });
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
            let writer = target_memory
                .db()
                .writer_connection("record user memory feedback")
                .await?;
            let result = writer.memory_store().record_feedback_event(request).await?;
            Ok(rendered_tool_json(
                None,
                &args,
                &json!({ "status": "recorded", "feedback": result }),
            ))
        }
        "tracedecay_memory_status" => {
            let status = TraceDecay::memory_status_for_db(target_memory.db()).await?;
            Ok(rendered_tool_json(
                None,
                &args,
                &json!({ "status": "ok", "memory": status }),
            ))
        }
        other => Err(config_error(format!("{other} is not a user-memory tool"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn seeded_memory() -> (tempfile::TempDir, TraceDecay, i64) {
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
        let fact_id = {
            let writer = cg
                .db()
                .writer_connection("seed memory tool test")
                .await
                .unwrap();
            writer
                .memory_store()
                .add_fact(
                    AddFactRequest {
                        content: "existing fact".to_string(),
                        category: MemoryCategory::General,
                        source: None,
                        tags: Vec::new(),
                        entities: Vec::new(),
                        trust: None,
                        metadata: json!({}),
                    },
                    DEFAULT_TRUST,
                )
                .await
                .unwrap()
                .fact
                .unwrap()
                .fact_id
        };
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
            MemoryStore::new(cg.db().conn())
                .list_facts(None, None, 10)
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
        let fact_ids = [fact_id];
        let mut record = Box::pin(record_retrieval_counts(cg.db(), false, &fact_ids, true));

        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(25), &mut record)
                .await
                .is_err()
        );
        drop(writer);
        record.await.unwrap();
        assert_eq!(
            MemoryStore::new(cg.db().conn())
                .get_fact(fact_id)
                .await
                .unwrap()
                .unwrap()
                .retrieval_count,
            1
        );
        assert_eq!(
            MemoryStore::new(cg.db().conn())
                .get_fact(fact_id)
                .await
                .unwrap()
                .unwrap()
                .access_count,
            1
        );
    }
}
