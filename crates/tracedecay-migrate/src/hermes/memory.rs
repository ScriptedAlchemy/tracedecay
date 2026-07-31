//! Legacy memory-store merge: facts, entities, associations, and feedback.

use std::collections::HashMap;

use sha2::{Digest, Sha256};

use super::copy::{
    MIGRATION_QUERY_PAGE_ROWS, compatible_columns, count_exact_rows, ensure_materialized_row_room,
    insert_row_or_skip_exact, quote_identifier, table_columns,
};
use super::fingerprint::hash_sqlite_value;
use super::pipeline::verify_source;
use crate::root_seam::db::Database;
use crate::root_seam::db::engine::{Executor, QueryExecutor, Value, params, params_from_iter};
use crate::root_seam::memory::store::MemoryStore;

pub(crate) fn source_integer(columns: &[String], values: &[Value], name: &str) -> Option<i64> {
    let value = values.get(columns.iter().position(|column| column == name)?)?;
    match value {
        Value::Integer(value) => Some(*value),
        _ => None,
    }
}

fn source_real(columns: &[String], values: &[Value], name: &str) -> Option<f64> {
    let value = values.get(columns.iter().position(|column| column == name)?)?;
    match value {
        Value::Real(value) => Some(*value),
        Value::Integer(value) => Some(*value as f64),
        _ => None,
    }
}

fn source_text<'a>(columns: &[String], values: &'a [Value], name: &str) -> Option<&'a str> {
    let value = values.get(columns.iter().position(|column| column == name)?)?;
    match value {
        Value::Text(value) => Some(value),
        _ => None,
    }
}

fn min_nonzero(left: i64, right: i64) -> i64 {
    match (left, right) {
        (0, value) | (value, 0) => value,
        _ => left.min(right),
    }
}

fn max_optional(left: Option<i64>, right: Option<i64>) -> Option<i64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.max(right)),
        (left, right) => left.or(right),
    }
}

fn merge_json_string_arrays(target: &str, source: &str) -> String {
    let mut merged = serde_json::from_str::<Vec<String>>(target).unwrap_or_default();
    for value in serde_json::from_str::<Vec<String>>(source).unwrap_or_default() {
        if !merged.iter().any(|existing| existing == &value) {
            merged.push(value);
        }
    }
    serde_json::to_string(&merged).unwrap_or_else(|_| "[]".to_string())
}

fn sqlite_row_fingerprint(columns: &[String], values: &[Value]) -> String {
    let mut hash = Sha256::new();
    hash.update(b"tracedecay-hermes-memory-row-v1\0");
    for (column, value) in columns.iter().zip(values) {
        hash.update(column.as_bytes());
        hash.update([0]);
        hash_sqlite_value(&mut hash, value.clone());
    }
    hex::encode(hash.finalize())
}

async fn memory_fact_id_by_content<Q>(target: &Q, content: &str) -> Result<Option<i64>, String>
where
    Q: QueryExecutor + ?Sized,
{
    let mut rows = target
        .query(
            "SELECT fact_id FROM memory_facts WHERE content = ?1",
            params![content],
        )
        .await
        .map_err(|error| format!("could not resolve migrated memory fact: {error}"))?;
    rows.next()
        .await
        .map_err(|error| format!("could not read migrated memory fact: {error}"))?
        .map(|row| {
            row.get(0)
                .map_err(|error| format!("invalid migrated memory fact id: {error}"))
        })
        .transpose()
}

fn merged_fact_metadata(
    target_raw: &str,
    source_raw: &str,
    fingerprint: &str,
    columns: &[String],
    values: &[Value],
) -> (String, bool) {
    let mut target = serde_json::from_str::<serde_json::Value>(target_raw)
        .unwrap_or_else(|_| serde_json::json!({}));
    if !target.is_object() {
        target = serde_json::json!({"legacy_target_metadata": target});
    }
    let serde_json::Value::Object(target_object) = &mut target else {
        return ("{}".to_string(), false);
    };
    if let Ok(serde_json::Value::Object(source)) =
        serde_json::from_str::<serde_json::Value>(source_raw)
    {
        for (key, value) in source {
            target_object.entry(key).or_insert(value);
        }
    }
    let merges = target_object
        .entry(LEGACY_FACT_MERGES_KEY)
        .or_insert_with(|| serde_json::json!({}));
    if !merges.is_object() {
        *merges = serde_json::json!({});
    }
    let serde_json::Value::Object(merges) = merges else {
        return (target.to_string(), false);
    };
    if merges.contains_key(fingerprint) {
        return (target.to_string(), true);
    }
    merges.insert(
        fingerprint.to_string(),
        serde_json::json!({
            "category": source_text(columns, values, "category"),
            "source": source_text(columns, values, "source"),
            "trust_score": source_real(columns, values, "trust_score"),
        }),
    );
    (target.to_string(), false)
}

async fn record_memory_fact_merge_marker<E>(
    target: &E,
    target_id: i64,
    columns: &[String],
    values: &[Value],
    fingerprint: &str,
) -> Result<(), String>
where
    E: Executor + ?Sized,
{
    let mut rows = target
        .query(
            "SELECT metadata FROM memory_facts WHERE fact_id = ?1",
            params![target_id],
        )
        .await
        .map_err(|error| format!("could not read migrated memory metadata: {error}"))?;
    let metadata: String = rows
        .next()
        .await
        .map_err(|error| format!("could not read migrated memory metadata: {error}"))?
        .ok_or_else(|| format!("migrated memory fact {target_id} disappeared"))?
        .get(0)
        .map_err(|error| format!("invalid migrated memory metadata: {error}"))?;
    let (metadata, _) = merged_fact_metadata(&metadata, "{}", fingerprint, columns, values);
    target
        .execute(
            "UPDATE memory_facts SET metadata = ?1 WHERE fact_id = ?2",
            params![metadata, target_id],
        )
        .await
        .map_err(|error| format!("could not record migrated memory fact source: {error}"))?;
    Ok(())
}

async fn merge_memory_fact_collision<E>(
    target: &E,
    target_id: i64,
    columns: &[String],
    values: &[Value],
    fingerprint: &str,
) -> Result<u64, String>
where
    E: Executor + ?Sized,
{
    let mut rows = target
        .query(
            "SELECT category, tags, trust_score, retrieval_count, access_count,
                    helpful_count, unhelpful_count, created_at, updated_at,
                    last_retrieved_at, last_recalled_at, last_feedback_at,
                    source, metadata
             FROM memory_facts WHERE fact_id = ?1",
            params![target_id],
        )
        .await
        .map_err(|error| format!("could not read colliding memory fact: {error}"))?;
    let row = rows
        .next()
        .await
        .map_err(|error| format!("could not read colliding memory fact: {error}"))?
        .ok_or_else(|| format!("colliding memory fact {target_id} disappeared"))?;
    let target_category: String = row.get(0).map_err(|error| error.to_string())?;
    let target_tags: String = row.get(1).map_err(|error| error.to_string())?;
    let target_trust: f64 = row.get(2).map_err(|error| error.to_string())?;
    let target_retrieval: i64 = row.get(3).map_err(|error| error.to_string())?;
    let target_access: i64 = row.get(4).map_err(|error| error.to_string())?;
    let target_helpful: i64 = row.get(5).map_err(|error| error.to_string())?;
    let target_unhelpful: i64 = row.get(6).map_err(|error| error.to_string())?;
    let target_created: i64 = row.get(7).map_err(|error| error.to_string())?;
    let target_updated: i64 = row.get(8).map_err(|error| error.to_string())?;
    let target_last_retrieved: Option<i64> = row.get(9).map_err(|error| error.to_string())?;
    let target_last_recalled: Option<i64> = row.get(10).map_err(|error| error.to_string())?;
    let target_last_feedback: Option<i64> = row.get(11).map_err(|error| error.to_string())?;
    let target_source: String = row.get(12).map_err(|error| error.to_string())?;
    let target_metadata: String = row.get(13).map_err(|error| error.to_string())?;

    let (metadata, already_merged) = merged_fact_metadata(
        &target_metadata,
        source_text(columns, values, "metadata").unwrap_or("{}"),
        fingerprint,
        columns,
        values,
    );
    if already_merged {
        return Ok(0);
    }

    let source_helpful = source_integer(columns, values, "helpful_count").unwrap_or(0);
    let source_unhelpful = source_integer(columns, values, "unhelpful_count").unwrap_or(0);
    let target_weight = 1_i64.saturating_add(target_helpful.saturating_add(target_unhelpful));
    let source_weight = 1_i64.saturating_add(source_helpful.saturating_add(source_unhelpful));
    let source_trust = source_real(columns, values, "trust_score").unwrap_or(0.5);
    let trust = ((target_trust * target_weight as f64) + (source_trust * source_weight as f64))
        / target_weight.saturating_add(source_weight) as f64;
    let source_category = source_text(columns, values, "category").unwrap_or("general");
    let category = if target_category == "general" && source_category != "general" {
        source_category
    } else {
        &target_category
    };
    let source_label = source_text(columns, values, "source").unwrap_or("manual");
    let source_label = if target_source == "manual" && source_label != "manual" {
        source_label
    } else {
        &target_source
    };
    let tags = merge_json_string_arrays(
        &target_tags,
        source_text(columns, values, "tags").unwrap_or("[]"),
    );
    target
        .execute(
            "UPDATE memory_facts
             SET category = ?1, tags = ?2, trust_score = ?3,
                 retrieval_count = ?4, access_count = ?5,
                 helpful_count = ?6, unhelpful_count = ?7,
                 created_at = ?8, updated_at = ?9,
                 last_retrieved_at = ?10, last_recalled_at = ?11,
                 last_feedback_at = ?12, source = ?13, metadata = ?14
             WHERE fact_id = ?15",
            params![
                category,
                tags,
                trust.clamp(0.0, 1.0),
                target_retrieval.saturating_add(
                    source_integer(columns, values, "retrieval_count").unwrap_or(0)
                ),
                target_access
                    .saturating_add(source_integer(columns, values, "access_count").unwrap_or(0)),
                target_helpful.saturating_add(source_helpful),
                target_unhelpful.saturating_add(source_unhelpful),
                min_nonzero(
                    target_created,
                    source_integer(columns, values, "created_at").unwrap_or(0),
                ),
                target_updated.max(source_integer(columns, values, "updated_at").unwrap_or(0)),
                max_optional(
                    target_last_retrieved,
                    source_integer(columns, values, "last_retrieved_at"),
                ),
                max_optional(
                    target_last_recalled,
                    source_integer(columns, values, "last_recalled_at"),
                ),
                max_optional(
                    target_last_feedback,
                    source_integer(columns, values, "last_feedback_at"),
                ),
                source_label,
                metadata,
                target_id,
            ],
        )
        .await
        .map_err(|error| format!("could not merge colliding memory fact: {error}"))?;
    Ok(1)
}

async fn copy_memory_facts<S, T>(source: &S, target: &T) -> Result<(u64, HashMap<i64, i64>), String>
where
    S: QueryExecutor + ?Sized,
    T: Executor + ?Sized,
{
    let source_columns = table_columns(source, "memory_facts").await?;
    let target_columns = table_columns(target, "memory_facts").await?;
    if target_columns.is_empty() {
        return Err("target is missing required table memory_facts".to_string());
    }
    let columns = compatible_columns(
        source_columns,
        &target_columns,
        &["fact_id"],
        "memory_facts",
    )?;
    let content_index = columns
        .iter()
        .position(|column| column == "content")
        .ok_or_else(|| "legacy memory facts have no content column".to_string())?;
    let quoted = columns
        .iter()
        .map(|column| quote_identifier(column))
        .collect::<Vec<_>>()
        .join(", ");
    let mut copied = 0;
    let mut fact_ids = HashMap::new();
    let mut last_fact_id = i64::MIN;
    let mut first_page = true;
    loop {
        let mut rows = source
            .query(
                &format!(
                    "SELECT fact_id, {quoted} FROM memory_facts
                     WHERE fact_id > ?1 OR (?3 = 1 AND fact_id = ?1)
                     ORDER BY fact_id LIMIT ?2"
                ),
                params![
                    last_fact_id,
                    MIGRATION_QUERY_PAGE_ROWS,
                    i64::from(first_page)
                ],
            )
            .await
            .map_err(|error| format!("could not read legacy memory facts: {error}"))?;
        let mut page_rows = 0_i64;
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| format!("could not read legacy memory fact: {error}"))?
        {
            let source_id = row
                .get::<i64>(0)
                .map_err(|error| format!("invalid legacy memory fact id: {error}"))?;
            if source_id < last_fact_id
                || (source_id == last_fact_id && (!first_page || page_rows > 0))
            {
                return Err("legacy memory facts returned an unstable fact_id order".to_string());
            }
            last_fact_id = source_id;
            page_rows += 1;
            let mut values = Vec::with_capacity(columns.len());
            for index in 0..columns.len() {
                values
                    .push(row.get::<Value>((index + 1) as i32).map_err(|error| {
                        format!("could not decode legacy memory fact: {error}")
                    })?);
            }
            let content = match &values[content_index] {
                Value::Text(content) => content.clone(),
                _ => return Err("legacy memory fact content is not text".to_string()),
            };
            let fingerprint = sqlite_row_fingerprint(&columns, &values);
            let target_id = memory_fact_id_by_content(target, &content).await?;
            let target_id = if let Some(target_id) = target_id {
                copied +=
                    merge_memory_fact_collision(target, target_id, &columns, &values, &fingerprint)
                        .await?;
                target_id
            } else {
                copied +=
                    insert_row_or_skip_exact(target, "memory_facts", &columns, &values).await?;
                let target_id = memory_fact_id_by_content(target, &content)
                    .await?
                    .ok_or_else(|| "migrated memory fact is absent from target".to_string())?;
                record_memory_fact_merge_marker(target, target_id, &columns, &values, &fingerprint)
                    .await?;
                target_id
            };
            ensure_materialized_row_room(fact_ids.len(), "memory fact identity map")?;
            fact_ids.insert(source_id, target_id);
        }
        if page_rows < MIGRATION_QUERY_PAGE_ROWS {
            break;
        }
        first_page = false;
    }
    Ok((copied, fact_ids))
}

async fn copy_memory_entities<S, T>(
    source: &S,
    target: &T,
) -> Result<(u64, HashMap<i64, i64>), String>
where
    S: QueryExecutor + ?Sized,
    T: Executor + ?Sized,
{
    let source_columns = table_columns(source, "memory_entities").await?;
    if source_columns.is_empty() {
        return Ok((0, HashMap::new()));
    }
    let target_columns = table_columns(target, "memory_entities").await?;
    if target_columns.is_empty() {
        return Err("target is missing required table memory_entities".to_string());
    }
    let columns = compatible_columns(
        source_columns,
        &target_columns,
        &["entity_id"],
        "memory_entities",
    )?;
    let normalized_index = columns
        .iter()
        .position(|column| column == "normalized_name")
        .ok_or_else(|| "legacy memory entities have no normalized_name column".to_string())?;
    let quoted = columns
        .iter()
        .map(|column| quote_identifier(column))
        .collect::<Vec<_>>()
        .join(", ");
    let mut inserted = 0;
    let mut entity_ids = HashMap::new();
    let mut last_entity_id = i64::MIN;
    let mut first_page = true;
    loop {
        let mut rows = source
            .query(
                &format!(
                    "SELECT entity_id, {quoted} FROM memory_entities
                     WHERE entity_id > ?1 OR (?3 = 1 AND entity_id = ?1)
                     ORDER BY entity_id LIMIT ?2"
                ),
                params![
                    last_entity_id,
                    MIGRATION_QUERY_PAGE_ROWS,
                    i64::from(first_page)
                ],
            )
            .await
            .map_err(|error| format!("could not read legacy memory entities: {error}"))?;
        let mut page_rows = 0_i64;
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| format!("could not read legacy memory entity: {error}"))?
        {
            let source_id = row
                .get::<i64>(0)
                .map_err(|error| format!("invalid legacy memory entity id: {error}"))?;
            if source_id < last_entity_id
                || (source_id == last_entity_id && (!first_page || page_rows > 0))
            {
                return Err(
                    "legacy memory entities returned an unstable entity_id order".to_string(),
                );
            }
            last_entity_id = source_id;
            page_rows += 1;
            let mut values = Vec::with_capacity(columns.len());
            for index in 0..columns.len() {
                values.push(
                    row.get::<Value>((index + 1) as i32).map_err(|error| {
                        format!("could not decode legacy memory entity: {error}")
                    })?,
                );
            }
            let normalized_name = match &values[normalized_index] {
                Value::Text(value) => value.clone(),
                _ => return Err("legacy normalized entity name is not text".to_string()),
            };
            inserted +=
                insert_row_or_skip_exact(target, "memory_entities", &columns, &values).await?;
            let mut target_rows = target
                .query(
                    "SELECT entity_id FROM memory_entities WHERE normalized_name = ?1",
                    params![normalized_name],
                )
                .await
                .map_err(|error| format!("could not resolve migrated memory entity: {error}"))?;
            let target_id = target_rows
                .next()
                .await
                .map_err(|error| format!("could not read migrated memory entity: {error}"))?
                .ok_or_else(|| "migrated memory entity is absent from target".to_string())?
                .get(0)
                .map_err(|error| format!("invalid migrated memory entity id: {error}"))?;
            ensure_materialized_row_room(entity_ids.len(), "memory entity identity map")?;
            entity_ids.insert(source_id, target_id);
        }
        if page_rows < MIGRATION_QUERY_PAGE_ROWS {
            break;
        }
        first_page = false;
    }
    Ok((inserted, entity_ids))
}

async fn copy_memory_fact_entities<S, T>(
    source: &S,
    target: &T,
    fact_ids: &HashMap<i64, i64>,
    entity_ids: &HashMap<i64, i64>,
) -> Result<u64, String>
where
    S: QueryExecutor + ?Sized,
    T: Executor + ?Sized,
{
    let source_columns = table_columns(source, "memory_fact_entities").await?;
    if source_columns.is_empty() {
        return Ok(0);
    }
    let expected_columns = ["fact_id", "entity_id"];
    let unsupported = source_columns
        .iter()
        .filter(|column| !expected_columns.contains(&column.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    if !unsupported.is_empty() {
        return Err(format!(
            "source table memory_fact_entities has unsupported columns that would be dropped: {}",
            unsupported.join(", ")
        ));
    }
    if !expected_columns
        .iter()
        .all(|required| source_columns.iter().any(|column| column == required))
    {
        return Err("source memory_fact_entities is missing required columns".to_string());
    }
    let mut inserted = 0;
    let mut last_fact_id = i64::MIN;
    let mut last_entity_id = i64::MIN;
    let mut first_page = true;
    loop {
        let mut rows = source
            .query(
                "SELECT fact_id, entity_id FROM memory_fact_entities
                 WHERE fact_id > ?1 OR (fact_id = ?1 AND entity_id > ?2)
                    OR (?4 = 1 AND fact_id = ?1 AND entity_id = ?2)
                 ORDER BY fact_id, entity_id LIMIT ?3",
                params![
                    last_fact_id,
                    last_entity_id,
                    MIGRATION_QUERY_PAGE_ROWS,
                    i64::from(first_page)
                ],
            )
            .await
            .map_err(|error| format!("could not read legacy memory associations: {error}"))?;
        let mut page_rows = 0_i64;
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| format!("could not read legacy memory association: {error}"))?
        {
            let source_fact_id = row
                .get::<i64>(0)
                .map_err(|error| format!("invalid legacy fact association: {error}"))?;
            let source_entity_id = row
                .get::<i64>(1)
                .map_err(|error| format!("invalid legacy entity association: {error}"))?;
            if (source_fact_id, source_entity_id) < (last_fact_id, last_entity_id)
                || !first_page
                    && (source_fact_id, source_entity_id) == (last_fact_id, last_entity_id)
                || (source_fact_id, source_entity_id) == (last_fact_id, last_entity_id)
                    && page_rows > 0
            {
                return Err("legacy memory associations returned an unstable order".to_string());
            }
            last_fact_id = source_fact_id;
            last_entity_id = source_entity_id;
            page_rows += 1;
            let target_fact_id = fact_ids.get(&source_fact_id).ok_or_else(|| {
                format!("legacy association references missing fact {source_fact_id}")
            })?;
            let target_entity_id = entity_ids.get(&source_entity_id).ok_or_else(|| {
                format!("legacy association references missing entity {source_entity_id}")
            })?;
            inserted += insert_row_or_skip_exact(
                target,
                "memory_fact_entities",
                &["fact_id".to_string(), "entity_id".to_string()],
                &[
                    Value::Integer(*target_fact_id),
                    Value::Integer(*target_entity_id),
                ],
            )
            .await?;
        }
        if page_rows < MIGRATION_QUERY_PAGE_ROWS {
            break;
        }
        first_page = false;
    }
    Ok(inserted)
}

async fn copy_memory_feedback<S, T>(
    source: &S,
    target: &T,
    fact_ids: &HashMap<i64, i64>,
) -> Result<u64, String>
where
    S: QueryExecutor + ?Sized,
    T: Executor + ?Sized,
{
    let source_columns = table_columns(source, "memory_feedback_events").await?;
    if source_columns.is_empty() {
        return Ok(0);
    }
    let target_columns = table_columns(target, "memory_feedback_events").await?;
    if target_columns.is_empty() {
        return Err("target is missing required table memory_feedback_events".to_string());
    }
    let columns = compatible_columns(
        source_columns,
        &target_columns,
        &["event_id", "fact_id"],
        "memory_feedback_events",
    )?;
    let quoted = columns
        .iter()
        .map(|column| quote_identifier(column))
        .collect::<Vec<_>>()
        .join(", ");
    let select_sql = format!(
        "SELECT event_id, fact_id, {quoted} FROM memory_feedback_events
         WHERE event_id > ?1 OR (?3 = 1 AND event_id = ?1)
         ORDER BY event_id LIMIT ?2"
    );
    let mut target_columns_with_fact = vec!["fact_id".to_string()];
    target_columns_with_fact.extend(columns.iter().cloned());
    let target_quoted = target_columns_with_fact
        .iter()
        .map(|column| quote_identifier(column))
        .collect::<Vec<_>>()
        .join(", ");
    let placeholders = (1..=target_columns_with_fact.len())
        .map(|index| format!("?{index}"))
        .collect::<Vec<_>>()
        .join(", ");
    let insert_sql =
        format!("INSERT INTO memory_feedback_events ({target_quoted}) VALUES ({placeholders})");
    let mut inserted = 0;
    let mut source_occurrences: HashMap<String, u64> = HashMap::new();
    let mut last_event_id = i64::MIN;
    let mut first_page = true;
    loop {
        let mut rows = source
            .query(
                &select_sql,
                params![
                    last_event_id,
                    MIGRATION_QUERY_PAGE_ROWS,
                    i64::from(first_page)
                ],
            )
            .await
            .map_err(|error| format!("could not read legacy memory feedback: {error}"))?;
        let mut page_rows = 0_i64;
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| format!("could not read legacy memory feedback row: {error}"))?
        {
            let event_id = row
                .get::<i64>(0)
                .map_err(|error| format!("invalid legacy feedback event id: {error}"))?;
            if event_id < last_event_id
                || (event_id == last_event_id && (!first_page || page_rows > 0))
            {
                return Err(
                    "legacy memory feedback returned an unstable event_id order".to_string()
                );
            }
            last_event_id = event_id;
            page_rows += 1;
            let source_fact_id = row
                .get::<i64>(1)
                .map_err(|error| format!("invalid legacy feedback fact id: {error}"))?;
            let target_fact_id = fact_ids.get(&source_fact_id).ok_or_else(|| {
                format!("legacy feedback references missing fact {source_fact_id}")
            })?;
            let mut values = Vec::with_capacity(columns.len() + 1);
            values.push(Value::Integer(*target_fact_id));
            for index in 0..columns.len() {
                values.push(row.get::<Value>((index + 2) as i32).map_err(|error| {
                    format!("could not decode legacy memory feedback: {error}")
                })?);
            }
            let signature = sqlite_row_fingerprint(&target_columns_with_fact, &values);
            if !source_occurrences.contains_key(&signature) {
                ensure_materialized_row_room(
                    source_occurrences.len(),
                    "memory feedback occurrence map",
                )?;
            }
            let occurrence = source_occurrences.entry(signature).or_default();
            *occurrence = occurrence.saturating_add(1);
            if count_exact_rows(
                target,
                "memory_feedback_events",
                &target_columns_with_fact,
                &values,
            )
            .await?
                >= *occurrence
            {
                continue;
            }
            inserted += target
                .execute(&insert_sql, params_from_iter(values.iter().cloned()))
                .await
                .map_err(|error| format!("could not copy legacy memory feedback: {error}"))?;
        }
        if page_rows < MIGRATION_QUERY_PAGE_ROWS {
            break;
        }
        first_page = false;
    }
    Ok(inserted)
}

async fn copy_memory_tables<S, T>(source: &S, target: &T) -> Result<u64, String>
where
    S: QueryExecutor + ?Sized,
    T: Executor + ?Sized,
{
    let (fact_rows, fact_ids) = copy_memory_facts(source, target).await?;
    let (entity_rows, entity_ids) = copy_memory_entities(source, target).await?;
    let association_rows =
        copy_memory_fact_entities(source, target, &fact_ids, &entity_ids).await?;
    let feedback_rows = copy_memory_feedback(source, target, &fact_ids).await?;
    Ok(fact_rows + entity_rows + association_rows + feedback_rows)
}

#[cfg(test)]
pub(super) async fn merge_memory_snapshot_for_test<S>(
    source: &S,
    target: &crate::root_seam::db::engine::Connection,
) -> Result<u64, String>
where
    S: QueryExecutor + ?Sized,
{
    if table_columns(source, "memory_facts").await?.is_empty() {
        return Ok(0);
    }
    verify_source(source).await?;
    let transaction = target
        .transaction_with_behavior(crate::root_seam::db::engine::TransactionBehavior::Immediate)
        .await
        .map_err(|error| format!("could not begin target memory migration: {error}"))?;
    let rows_copied = copy_memory_tables(source, &transaction).await?;
    MemoryStore::new_engine_transaction(&transaction)
        .rebuild_all_banks()
        .await
        .map_err(|error| format!("could not rebuild migrated memory banks: {error}"))?;
    transaction
        .commit()
        .await
        .map_err(|error| format!("could not commit target memory migration: {error}"))?;
    Ok(rows_copied)
}

pub(crate) async fn merge_memory_snapshot<S>(source: &S, target: &Database) -> Result<u64, String>
where
    S: QueryExecutor + ?Sized,
{
    if table_columns(source, "memory_facts").await?.is_empty() {
        return Ok(0);
    }
    verify_source(source).await?;
    let transaction = target
        .begin_memory_write_transaction("merge memory migration snapshot")
        .await
        .map_err(|error| format!("could not begin target memory migration: {error}"))?;
    let rows_copied = copy_memory_tables(source, &transaction).await?;
    MemoryStore::new_database_transaction(&transaction)
        .rebuild_all_banks()
        .await
        .map_err(|error| format!("could not rebuild migrated memory banks: {error}"))?;
    transaction
        .commit()
        .await
        .map_err(|error| format!("could not commit target memory migration: {error}"))?;
    Ok(rows_copied)
}

const LEGACY_FACT_MERGES_KEY: &str = "_tracedecay_legacy_hermes_merges";
