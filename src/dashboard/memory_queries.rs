use std::hash::{Hash, Hasher};

use serde_json::Value;

use super::util::{like_pattern, query_rows};
use super::{CuratePreviewFingerprint, DashboardState};
use crate::memory::encoding::HolographicEncoder;

pub(crate) type VectorStateFingerprint = (i64, i64, i64, u64);

pub(crate) fn normalize_fact_metadata(mut row: Value) -> Value {
    if let Some(obj) = row.as_object_mut() {
        if let Some(raw) = obj.get("metadata").and_then(Value::as_str) {
            let parsed = serde_json::from_str::<Value>(raw).unwrap_or(Value::Null);
            obj.insert("metadata".to_string(), parsed);
        }
    }
    row
}

pub(crate) async fn fact_rows(
    state: &DashboardState,
    query: &str,
    limit: i64,
) -> Result<Vec<Value>, String> {
    let q = query.trim();
    let rows = if q.is_empty() {
        query_rows(
            &state.mem_conn,
            "SELECT fact_id, content, category, tags, trust_score,
                    retrieval_count, access_count, last_recalled_at,
                    helpful_count, metadata, created_at, updated_at,
                    hrr_vector IS NOT NULL AS has_hrr
             FROM memory_facts
             ORDER BY trust_score DESC, updated_at DESC
             LIMIT ?1",
            libsql::params![limit],
        )
        .await
    } else {
        let like = like_pattern(q);
        query_rows(
            &state.mem_conn,
            "SELECT fact_id, content, category, tags, trust_score,
                    retrieval_count, access_count, last_recalled_at,
                    helpful_count, metadata, created_at, updated_at,
                    hrr_vector IS NOT NULL AS has_hrr
             FROM memory_facts
             WHERE content LIKE ?1 ESCAPE '\\' OR tags LIKE ?1 ESCAPE '\\'
             ORDER BY trust_score DESC, updated_at DESC
             LIMIT ?2",
            libsql::params![like, limit],
        )
        .await
    }?;
    Ok(rows.into_iter().map(normalize_fact_metadata).collect())
}

pub(crate) async fn entity_rows(state: &DashboardState, limit: i64) -> Result<Vec<Value>, String> {
    query_rows(
        &state.mem_conn,
        "SELECT e.entity_id, e.name, e.entity_type, e.aliases, e.created_at,
                COUNT(fe.fact_id) AS fact_count
         FROM memory_entities e
         LEFT JOIN memory_fact_entities fe ON fe.entity_id = e.entity_id
         GROUP BY e.entity_id
         ORDER BY fact_count DESC, e.name ASC
         LIMIT ?1",
        libsql::params![limit],
    )
    .await
}

pub(crate) async fn trust_histogram_rows(state: &DashboardState) -> Result<Vec<Value>, String> {
    query_rows(
        &state.mem_conn,
        "SELECT MIN(CAST(MAX(MIN(trust_score, 1.0), 0.0) * 10.0 AS INTEGER), 9) AS bucket,
                COUNT(*) AS count
         FROM memory_facts
         WHERE trust_score IS NOT NULL
         GROUP BY bucket",
        (),
    )
    .await
}

pub(crate) async fn overview_categories(state: &DashboardState) -> Result<Vec<Value>, String> {
    query_rows(
        &state.mem_conn,
        "SELECT category, COUNT(*) AS count, AVG(trust_score) AS avg_trust
         FROM memory_facts
         GROUP BY category
         ORDER BY count DESC, category ASC",
        (),
    )
    .await
}

pub(crate) async fn overview_category_rows(state: &DashboardState) -> Result<Vec<Value>, String> {
    query_rows(
        &state.mem_conn,
        "SELECT category,
                COUNT(*) AS facts,
                SUM(CASE WHEN hrr_vector IS NOT NULL THEN 1 ELSE 0 END) AS hrr_vectors
         FROM memory_facts
         GROUP BY category
         ORDER BY facts DESC, category ASC",
        (),
    )
    .await
}

pub(crate) async fn overview_bank_rows(state: &DashboardState) -> Result<Vec<Value>, String> {
    query_rows(
        &state.mem_conn,
        "SELECT bank_name, fact_count, hrr_dim AS dim, updated_at FROM memory_banks",
        (),
    )
    .await
}

pub(crate) async fn overview_entity_types(state: &DashboardState) -> Result<Vec<Value>, String> {
    query_rows(
        &state.mem_conn,
        "SELECT e.entity_type, COUNT(DISTINCT e.entity_id) AS count
         FROM memory_entities e
         JOIN memory_fact_entities fe ON fe.entity_id = e.entity_id
         GROUP BY e.entity_type
         ORDER BY count DESC, e.entity_type ASC",
        (),
    )
    .await
}

pub(crate) async fn live_memory_banks(state: &DashboardState) -> Result<Vec<Value>, String> {
    query_rows(
        &state.mem_conn,
        "SELECT b.bank_id, b.bank_name, b.hrr_dim AS dim,
                CASE WHEN b.bank_name = 'all'
                     THEN (SELECT COUNT(*) FROM memory_facts)
                     ELSE (SELECT COUNT(*) FROM memory_facts f WHERE f.category = b.bank_name)
                END AS fact_count,
                b.fact_count AS bundled_fact_count,
                b.updated_at
         FROM memory_banks b
         ORDER BY b.updated_at DESC
         LIMIT 50",
        (),
    )
    .await
}

pub(crate) async fn growth_rows(state: &DashboardState) -> Result<Vec<Value>, String> {
    query_rows(
        &state.mem_conn,
        "WITH bounds AS (
             SELECT date(MAX(created_at), 'unixepoch') AS max_date
             FROM memory_facts
             WHERE created_at > 0
         ),
         daily AS (
             SELECT date(f.created_at, 'unixepoch') AS date, COUNT(*) AS facts
             FROM memory_facts f, bounds
             WHERE f.created_at > 0
               AND bounds.max_date IS NOT NULL
               AND date(f.created_at, 'unixepoch') >= date(bounds.max_date, '-180 days')
             GROUP BY date
         ),
         prior AS (
             SELECT COUNT(*) AS facts
             FROM memory_facts f, bounds
             WHERE f.created_at > 0
               AND bounds.max_date IS NOT NULL
               AND date(f.created_at, 'unixepoch') < date(bounds.max_date, '-180 days')
         )
         SELECT daily.date,
                daily.facts,
                prior.facts + SUM(daily.facts) OVER (ORDER BY daily.date ASC)
                    AS cumulative_facts
         FROM daily, prior
         ORDER BY daily.date ASC",
        (),
    )
    .await
}

pub(crate) async fn graph_entity_rows(
    state: &DashboardState,
    fact_ids: &[i64],
) -> Result<Vec<Value>, String> {
    if fact_ids.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = fact_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let sql = format!(
        "SELECT fe.fact_id, e.entity_id, e.name, e.entity_type
         FROM memory_fact_entities fe
         JOIN memory_entities e ON e.entity_id = fe.entity_id
         WHERE fe.fact_id IN ({placeholders})
         ORDER BY e.name ASC"
    );
    let params: Vec<libsql::Value> = fact_ids
        .iter()
        .map(|id| libsql::Value::Integer(*id))
        .collect();
    query_rows(&state.mem_conn, &sql, params).await
}

pub(crate) async fn graph_bank_rows(state: &DashboardState) -> Result<Vec<Value>, String> {
    query_rows(
        &state.mem_conn,
        "SELECT bank_name, hrr_dim AS dim, fact_count, updated_at
         FROM memory_banks
         ORDER BY fact_count DESC, bank_name ASC
         LIMIT 50",
        (),
    )
    .await
}

pub(crate) async fn fact_detail_row(
    state: &DashboardState,
    fact_id: i64,
) -> Result<Option<Value>, String> {
    let rows = query_rows(
        &state.mem_conn,
        "SELECT fact_id, content, category, tags, trust_score,
                retrieval_count, access_count, last_recalled_at,
                helpful_count, metadata, created_at, updated_at,
                hrr_vector IS NOT NULL AS has_hrr
         FROM memory_facts
         WHERE fact_id = ?1
         LIMIT 1",
        libsql::params![fact_id],
    )
    .await?;
    Ok(rows.into_iter().next().map(normalize_fact_metadata))
}

pub(crate) async fn fact_entities(
    state: &DashboardState,
    fact_id: i64,
) -> Result<Vec<Value>, String> {
    query_rows(
        &state.mem_conn,
        "SELECT e.entity_id, e.name, e.entity_type
         FROM memory_fact_entities fe
         JOIN memory_entities e ON e.entity_id = fe.entity_id
         WHERE fe.fact_id = ?1
         ORDER BY e.name ASC",
        libsql::params![fact_id],
    )
    .await
}

pub(crate) async fn vector_facts(
    state: &DashboardState,
    query: &str,
    limit: i64,
) -> Result<Vec<(Value, Vec<f64>)>, String> {
    let q = query.trim();
    let (sql, params): (String, Vec<libsql::Value>) = if q.is_empty() {
        (
            "SELECT f.fact_id, f.content, f.category, f.trust_score, f.retrieval_count,
                    f.hrr_vector, b.bank_id, b.bank_name, COUNT(fe.entity_id) AS entity_count,
                    f.access_count, f.last_recalled_at, f.created_at, f.updated_at, f.metadata
             FROM memory_facts f
             LEFT JOIN memory_banks b ON b.bank_name = f.category
             LEFT JOIN memory_fact_entities fe ON fe.fact_id = f.fact_id
             WHERE f.hrr_vector IS NOT NULL
             GROUP BY f.fact_id
             ORDER BY f.trust_score DESC, f.updated_at DESC
             LIMIT ?1"
                .to_string(),
            vec![libsql::Value::Integer(limit)],
        )
    } else {
        (
            "SELECT f.fact_id, f.content, f.category, f.trust_score, f.retrieval_count,
                    f.hrr_vector, b.bank_id, b.bank_name, COUNT(fe.entity_id) AS entity_count,
                    f.access_count, f.last_recalled_at, f.created_at, f.updated_at, f.metadata
             FROM memory_facts f
             LEFT JOIN memory_banks b ON b.bank_name = f.category
             LEFT JOIN memory_fact_entities fe ON fe.fact_id = f.fact_id
             WHERE f.hrr_vector IS NOT NULL
               AND (f.content LIKE ?1 ESCAPE '\\' OR f.tags LIKE ?1 ESCAPE '\\')
             GROUP BY f.fact_id
             ORDER BY f.trust_score DESC, f.updated_at DESC
             LIMIT ?2"
                .to_string(),
            vec![
                libsql::Value::Text(like_pattern(q)),
                libsql::Value::Integer(limit),
            ],
        )
    };

    let mut rows = state
        .mem_conn
        .query(&sql, params)
        .await
        .map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    while let Some(row) = rows.next().await.map_err(|e| e.to_string())? {
        let fact_id: i64 = row.get(0).unwrap_or(0);
        let content: String = row.get(1).unwrap_or_default();
        let category: String = row.get(2).unwrap_or_else(|_| "general".to_string());
        let trust_score: f64 = row.get(3).unwrap_or(0.0);
        let retrieval_count: i64 = row.get(4).unwrap_or(0);
        let Ok(libsql::Value::Blob(bytes)) = row.get_value(5) else {
            continue;
        };
        let Ok(vector) = HolographicEncoder::deserialize(&bytes) else {
            continue;
        };
        if vector.is_empty() || vector.iter().any(|v| !v.is_finite()) {
            continue;
        }
        let bank_id = match row.get_value(6) {
            Ok(libsql::Value::Integer(id)) => serde_json::json!(id),
            _ => Value::Null,
        };
        let bank_name = match row.get_value(7) {
            Ok(libsql::Value::Text(name)) => serde_json::json!(name),
            _ => Value::Null,
        };
        let entity_count: i64 = row.get(8).unwrap_or(0);
        let access_count: i64 = row.get(9).unwrap_or(0);
        let last_recalled_at: Option<i64> = row.get(10).unwrap_or(None);
        let created_at: i64 = row.get(11).unwrap_or(0);
        let updated_at: i64 = row.get(12).unwrap_or(0);
        let metadata = row
            .get::<String>(13)
            .ok()
            .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
            .unwrap_or(Value::Null);
        out.push((
            serde_json::json!({
                "fact_id": fact_id,
                "content": content,
                "category": category,
                "trust_score": trust_score,
                "retrieval_count": retrieval_count,
                "access_count": access_count,
                "last_recalled_at": last_recalled_at,
                "created_at": created_at,
                "updated_at": updated_at,
                "metadata": metadata,
                "bank_id": bank_id,
                "bank_name": bank_name,
                "entity_count": entity_count,
                "connection_count": entity_count,
            }),
            vector,
        ));
    }
    Ok(out)
}

pub(crate) async fn vector_state_fingerprint(
    state: &DashboardState,
) -> Result<VectorStateFingerprint, String> {
    let mut rows = state
        .mem_conn
        .query(
            "SELECT fact_id, COALESCE(updated_at, 0), hrr_algebra, hrr_dim,
                    COALESCE(access_count, 0), COALESCE(last_recalled_at, 0)
             FROM memory_facts
             WHERE hrr_vector IS NOT NULL
             ORDER BY fact_id ASC",
            (),
        )
        .await
        .map_err(|e| e.to_string())?;
    let mut count = 0_i64;
    let mut max_updated_at = 0_i64;
    let mut sum_fact_id = 0_i64;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    while let Some(row) = rows.next().await.map_err(|e| e.to_string())? {
        let fact_id: i64 = row.get(0).unwrap_or(0);
        let updated_at: i64 = row.get(1).unwrap_or(0);
        let hrr_algebra: String = row.get(2).unwrap_or_default();
        let hrr_dim: i64 = row.get(3).unwrap_or(0);
        let access_count: i64 = row.get(4).unwrap_or(0);
        let last_recalled_at: i64 = row.get(5).unwrap_or(0);
        count += 1;
        max_updated_at = max_updated_at.max(updated_at);
        sum_fact_id += fact_id;
        fact_id.hash(&mut hasher);
        updated_at.hash(&mut hasher);
        hrr_algebra.hash(&mut hasher);
        hrr_dim.hash(&mut hasher);
        access_count.hash(&mut hasher);
        last_recalled_at.hash(&mut hasher);
    }
    Ok((count, max_updated_at, sum_fact_id, hasher.finish()))
}

pub(crate) async fn curation_preview_fingerprint(
    state: &DashboardState,
) -> Result<CuratePreviewFingerprint, String> {
    let mut rows = state
        .mem_conn
        .query(
            "SELECT COUNT(*),
                    COALESCE(MAX(updated_at), 0),
                    COALESCE(SUM(fact_id), 0),
                    COALESCE(SUM(updated_at), 0)
             FROM memory_facts",
            (),
        )
        .await
        .map_err(|e| e.to_string())?;
    let Some(row) = rows.next().await.map_err(|e| e.to_string())? else {
        return Ok((0, 0, 0, 0));
    };
    Ok((
        row.get::<i64>(0).unwrap_or(0),
        row.get::<i64>(1).unwrap_or(0),
        row.get::<i64>(2).unwrap_or(0),
        row.get::<i64>(3).unwrap_or(0),
    ))
}

pub(crate) async fn oplog_rows(state: &DashboardState, limit: i64) -> Result<Vec<Value>, String> {
    query_rows(
        &state.mem_conn,
        "SELECT id, ts, op, fact_id, detail_json
         FROM memory_oplog
         ORDER BY id DESC
         LIMIT ?1",
        libsql::params![limit],
    )
    .await
}
