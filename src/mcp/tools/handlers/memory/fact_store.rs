use std::path::Path;

use serde_json::{Value, json};

use crate::application::memory::{MemoryApplication, V1UpdateFactOutcome};
use crate::errors::Result;
use crate::global_db::RegisteredGlobalDb;
use crate::mcp::tools::{ToolResult, render, renderers};
use crate::memory::types::{AddFactRequest, MemoryCategory, SearchFactsRequest, UpdateFactRequest};
use crate::store::DatabaseFactStore;
use crate::tracedecay::TraceDecay;

use super::super::support::{project_selector_present, string_array_values};
use super::super::text_tool_result;
use super::actions::FactStoreAction;
use super::args::{
    MAX_FACT_LIMIT, fact_id, limit, metadata_with_tags, optional_category, optional_f64,
    request_entities, required_str, update_trust,
};
use super::status::feedback_history_repair_payload;
use super::{
    TargetMemoryDb, config_error, memory_application, memory_application_error,
    memory_operation_context, open_target_memory_db, refresh_target_memory_digest,
};

fn rendered_fact_store(project_root: Option<&Path>, args: &Value, value: &Value) -> ToolResult {
    let text = render::finalize(project_root, args, value, || {
        renderers::fact_store_md(args, value)
    });
    text_tool_result(&text)
}

fn results_envelope(action: &str, results: &Value, count: usize) -> Value {
    json!({
        "action": action,
        "results": results,
        "facts": results,
        "count": count,
    })
}

pub(in crate::mcp::tools::handlers) async fn handle_fact_store(
    cg: &TraceDecay,
    args: Value,
    global_db: Option<&RegisteredGlobalDb>,
) -> Result<ToolResult> {
    let action = required_str(&args, "action")?;
    let cross_project_selector = project_selector_present(&args, &["project_path"]);
    if FactStoreAction::parse(action).is_some_and(FactStoreAction::writes) && cross_project_selector
    {
        return Err(config_error(
            "cross-project fact_store writes are not supported; omit project_selector to write the active project",
        ));
    }
    // The store-touching work (open + dispatch, including the add-path
    // holographic encode, the serialized write, and any digest refresh) is
    // bounded once, centrally, by the retained memory dispatch off the
    // admission-carried client deadline (dispatch_groups::dispatch_memory_operation).
    let target_memory = open_target_memory_db(cg, &args, global_db).await?;
    handle_fact_store_for_target(args, cross_project_selector, target_memory).await
}

pub(super) async fn handle_fact_store_for_target(
    args: Value,
    cross_project_selector: bool,
    target_memory: TargetMemoryDb<'_>,
) -> Result<ToolResult> {
    let action = required_str(&args, "action")?;
    let action_kind = FactStoreAction::parse(action)
        .ok_or_else(|| config_error(format!("unknown fact_store action: {action}")))?;
    let memory = memory_application(&target_memory)?;
    let mut refresh_digest = false;
    let out = match action_kind {
        FactStoreAction::Add => {
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
        FactStoreAction::Search
        | FactStoreAction::Probe
        | FactStoreAction::Related
        | FactStoreAction::Reason
        | FactStoreAction::List => {
            read_facts_envelope(
                action_kind,
                action,
                &args,
                &memory,
                &target_memory,
                cross_project_selector,
            )
            .await?
        }
        FactStoreAction::Contradict => {
            let threshold = optional_f64(&args, "threshold").unwrap_or(0.3);
            let limit = limit(&args);
            let facts = memory
                .contradict_facts_v1(optional_category(&args)?, threshold, limit)
                .await
                .map_err(memory_application_error)?;
            let count = facts.len();
            results_envelope(action, &json!(facts), count)
        }
        FactStoreAction::Get => {
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
        FactStoreAction::Update => {
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
        FactStoreAction::Remove => {
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

/// Tracked/untracked read dispatch over the [`FactStoreAction`] table: a
/// cross-project selector runs the untracked variant (retrieval accounting
/// stays local to the owning project), otherwise the read carries a
/// daemon-issued operation context. (design item P2.9)
async fn read_facts_envelope(
    action_kind: FactStoreAction,
    action: &str,
    args: &Value,
    memory: &MemoryApplication<DatabaseFactStore<'_>>,
    target_memory: &TargetMemoryDb<'_>,
    cross_project_selector: bool,
) -> Result<Value> {
    let (count, results) = match action_kind {
        FactStoreAction::Search | FactStoreAction::Probe | FactStoreAction::Related => {
            let query = required_str(
                args,
                if action_kind == FactStoreAction::Search {
                    "query"
                } else {
                    "entity"
                },
            )?;
            let request = SearchFactsRequest {
                query: query.to_owned(),
                category: optional_category(args)?,
                limit: Some(limit(args)),
                min_trust: optional_f64(args, "min_trust"),
                include_why: true,
            };
            let facts = if cross_project_selector {
                match action_kind {
                    FactStoreAction::Search => memory.search_facts_untracked_v1(request).await,
                    FactStoreAction::Probe => memory.probe_facts_untracked_v1(request).await,
                    _ => memory.related_facts_untracked_v1(request).await,
                }
            } else {
                let context = memory_operation_context(args, target_memory, action)?;
                match action_kind {
                    FactStoreAction::Search => memory.search_facts_v1(request, context).await,
                    FactStoreAction::Probe => memory.probe_facts_v1(request, context).await,
                    _ => memory.related_facts_v1(request, context).await,
                }
            }
            .map_err(memory_application_error)?;
            (facts.len(), json!(facts))
        }
        FactStoreAction::Reason => {
            let entities = request_entities(args);
            if entities.is_empty() {
                return Err(config_error(
                    "missing required parameter: entities — `fact_store --action reason` \
                     requires at least one `--entities`/`--entity` value to reason over",
                ));
            }
            let category = optional_category(args)?;
            let min_trust = optional_f64(args, "min_trust");
            let limit = limit(args);
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
                        memory_operation_context(args, target_memory, action)?,
                    )
                    .await
            }
            .map_err(memory_application_error)?;
            (facts.len(), json!(facts))
        }
        FactStoreAction::List => {
            let category = optional_category(args)?;
            let min_trust = optional_f64(args, "min_trust");
            let limit = limit(args);
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
                        memory_operation_context(args, target_memory, action)?,
                    )
                    .await
            }
            .map_err(memory_application_error)?;
            (facts.len(), json!(facts))
        }
        _ => unreachable!("read_facts_envelope dispatches read actions only"),
    };
    Ok(results_envelope(action, &results, count))
}
