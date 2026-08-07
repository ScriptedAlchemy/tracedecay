//! Persistence layer for memory facts, entities, vectors, and feedback.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fmt;
use std::future::Future;
use std::pin::Pin;

use super::encoding::HolographicEncoder;
use super::entities::{extract_entities, normalize_entity};
use super::types::{
    FactRecord, FactRelationKind, FactRelationRecord, FeedbackAction, MemoryCategory,
};
use crate::db::MemoryConnection;
use crate::db::engine::{IntoParams, Row, Rows, TransactionBehavior, Value, params};
use crate::errors::{Result, TraceDecayError};
use crate::tracedecay::current_timestamp;

mod facts;
mod feedback;
mod grooming;
mod queries;
mod relations;
mod vectors;

const DEFAULT_LIMIT: usize = 50;
const ENTITY_BATCH_SIZE: usize = 500;
const MEMORY_SOURCE_DEFAULT: &str = "manual";
const HRR_ALGEBRA: &str = "amari_fhrr";

pub struct MemoryStore<'a> {
    conn: MemoryConnection<'a>,
    encoder: HolographicEncoder,
}

impl<'a> MemoryStore<'a> {
    pub const fn new_runtime(conn: &'a crate::db::engine::Connection) -> Self {
        Self {
            conn: MemoryConnection::runtime(conn),
            encoder: HolographicEncoder::new(),
        }
    }

    pub const fn new_engine_transaction(transaction: &'a crate::db::engine::Transaction) -> Self {
        Self {
            conn: MemoryConnection::runtime_transaction(transaction),
            encoder: HolographicEncoder::new(),
        }
    }

    pub const fn new_database_transaction(
        transaction: &'a crate::db::DatabaseMemoryTransaction<'a>,
    ) -> Self {
        Self {
            conn: MemoryConnection::database_transaction(transaction),
            encoder: HolographicEncoder::new(),
        }
    }

    /// Runs `work` through the transaction executor, committing on success and
    /// rolling back on error or cancellation. Caller-owned transactions remain
    /// caller-owned and provide the surrounding atomic boundary.
    async fn with_immediate_tx<T, F>(&self, operation: &str, work: F) -> Result<T>
    where
        T: Send,
        F: Send
            + for<'tx> FnOnce(
                &'tx MemoryStore<'tx>,
            ) -> Pin<Box<dyn Future<Output = Result<T>> + Send + 'tx>>,
    {
        if matches!(
            self.conn,
            MemoryConnection::RuntimeTransaction(_)
                | MemoryConnection::Transaction(_)
                | MemoryConnection::DatabaseTransaction(_)
        ) {
            return work(self).await;
        }
        let transaction = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .map_err(|e| db_error(operation, e))?;
        let transactional_store = MemoryStore {
            conn: MemoryConnection::transaction(&transaction),
            encoder: self.encoder.clone(),
        };
        match work(&transactional_store).await {
            Ok(value) => {
                transaction
                    .commit()
                    .await
                    .map_err(|error| db_error(operation, error))?;
                Ok(value)
            }
            Err(error) => {
                let _ = transaction.rollback().await;
                Err(error)
            }
        }
    }

    /// Appends one row to `memory_oplog`. `detail` must never carry fact
    /// content beyond what the operation needs — deletes record a
    /// `content_hash`, not the content (hard-delete stance preserved).
    async fn log_oplog(
        &self,
        op: &str,
        fact_id: Option<i64>,
        detail: &serde_json::Value,
    ) -> Result<()> {
        let detail_json = to_json_string(detail, "log_oplog")?;
        self.conn
            .execute(
                "INSERT INTO memory_oplog (ts, op, fact_id, detail_json)
                 VALUES (?1, ?2, ?3, ?4)",
                params![current_timestamp(), op, fact_id, detail_json],
            )
            .await
            .map_err(|e| db_error("log_oplog", e))?;
        Ok(())
    }

    async fn get_fact_relation(
        &self,
        source_fact_id: i64,
        target_fact_id: i64,
        relation: FactRelationKind,
    ) -> Result<Option<FactRelationRecord>> {
        let mut rows = self
            .conn
            .query(
                "SELECT source_fact_id, target_fact_id, relation, confidence, source,
                        metadata, created_at, updated_at
                 FROM memory_fact_relations
                 WHERE source_fact_id = ?1 AND target_fact_id = ?2 AND relation = ?3",
                params![source_fact_id, target_fact_id, relation.as_str()],
            )
            .await
            .map_err(|e| db_error("get_fact_relation", e))?;
        rows.next()
            .await
            .map_err(|e| db_error("get_fact_relation", e))?
            .map(|row| relation_from_row(&row, "get_fact_relation"))
            .transpose()
    }

    async fn rewire_fact_relations_inner(&self, winner_id: i64, loser_ids: &[i64]) -> Result<()> {
        if loser_ids.is_empty() {
            return Ok(());
        }
        let loser_set: BTreeSet<i64> = loser_ids.iter().copied().collect();
        let relations = self.load_fact_relations_for_rewire(loser_ids).await?;
        let mut relation_delete_params = Vec::with_capacity(loser_ids.len() * 2);
        relation_delete_params.extend(loser_ids.iter().copied().map(Value::Integer));
        relation_delete_params.extend(loser_ids.iter().copied().map(Value::Integer));
        self.conn
            .execute(
                &format!(
                    "DELETE FROM memory_fact_relations
                     WHERE source_fact_id IN ({0}) OR target_fact_id IN ({0})",
                    std::iter::repeat_n("?", loser_ids.len())
                        .collect::<Vec<_>>()
                        .join(",")
                ),
                relation_delete_params,
            )
            .await
            .map_err(|e| db_error("merge_facts", e))?;
        for relation in relations.into_iter().filter(|relation| {
            loser_set.contains(&relation.source_fact_id)
                || loser_set.contains(&relation.target_fact_id)
        }) {
            let source_fact_id = if loser_set.contains(&relation.source_fact_id) {
                winner_id
            } else {
                relation.source_fact_id
            };
            let target_fact_id = if loser_set.contains(&relation.target_fact_id) {
                winner_id
            } else {
                relation.target_fact_id
            };
            if source_fact_id != target_fact_id {
                self.upsert_fact_relation_inner(
                    source_fact_id,
                    target_fact_id,
                    relation.relation,
                    relation.confidence,
                    &relation.source,
                    relation.metadata,
                )
                .await?;
            }
        }
        Ok(())
    }

    async fn load_fact_relations_for_rewire(
        &self,
        fact_ids: &[i64],
    ) -> Result<Vec<FactRelationRecord>> {
        const PAGE_SIZE: i64 = 512;

        let placeholders = std::iter::repeat_n("?", fact_ids.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT source_fact_id, target_fact_id, relation, confidence, source,
                    metadata, created_at, updated_at
             FROM memory_fact_relations
             WHERE (
                 source_fact_id IN ({placeholders})
                 OR target_fact_id IN ({placeholders})
             )
               AND (
                   ? IS NULL
                   OR source_fact_id > ?
                   OR (source_fact_id = ? AND target_fact_id > ?)
                   OR (
                       source_fact_id = ?
                       AND target_fact_id = ?
                       AND relation > ?
                   )
               )
             ORDER BY source_fact_id, target_fact_id, relation
             LIMIT ?"
        );
        let mut relations = Vec::new();
        let mut source_cursor: Option<i64> = None;
        let mut target_cursor: Option<i64> = None;
        let mut relation_cursor: Option<String> = None;
        loop {
            let mut values = Vec::with_capacity(fact_ids.len() * 2 + 8);
            values.extend(fact_ids.iter().copied().map(Value::Integer));
            values.extend(fact_ids.iter().copied().map(Value::Integer));
            values.push(source_cursor.map_or(Value::Null, Value::Integer));
            values.push(source_cursor.map_or(Value::Null, Value::Integer));
            values.push(source_cursor.map_or(Value::Null, Value::Integer));
            values.push(target_cursor.map_or(Value::Null, Value::Integer));
            values.push(source_cursor.map_or(Value::Null, Value::Integer));
            values.push(target_cursor.map_or(Value::Null, Value::Integer));
            values.push(
                relation_cursor
                    .as_ref()
                    .map_or(Value::Null, |value| Value::Text(value.clone())),
            );
            values.push(Value::Integer(PAGE_SIZE));
            let mut rows = self
                .conn
                .query(sql.as_str(), values)
                .await
                .map_err(|error| db_error("merge_facts", error))?;
            let mut page_count = 0;
            while let Some(row) = rows
                .next()
                .await
                .map_err(|error| db_error("merge_facts", error))?
            {
                source_cursor = Some(
                    row.get::<i64>(0)
                        .map_err(|error| db_error("merge_facts", error))?,
                );
                target_cursor = Some(
                    row.get::<i64>(1)
                        .map_err(|error| db_error("merge_facts", error))?,
                );
                relation_cursor = Some(
                    row.get::<String>(2)
                        .map_err(|error| db_error("merge_facts", error))?,
                );
                relations.push(relation_from_row(&row, "merge_facts")?);
                page_count += 1;
            }
            if page_count < PAGE_SIZE {
                break;
            }
        }
        Ok(relations)
    }

    pub async fn query<P>(&self, operation: &str, sql: &str, params: P) -> Result<Rows>
    where
        P: IntoParams,
    {
        self.conn
            .query(sql, params)
            .await
            .map_err(|error| db_error(operation, error))
    }

    async fn get_fact_by_content(&self, content: &str) -> Result<Option<FactRecord>> {
        let mut rows = self
            .conn
            .query(
                "SELECT fact_id FROM memory_facts WHERE content = ?1",
                params![content],
            )
            .await
            .map_err(|e| db_error("get_fact_by_content", e))?;
        let Some(row) = rows
            .next()
            .await
            .map_err(|e| db_error("get_fact_by_content", e))?
        else {
            return Ok(None);
        };
        let fact_id = row
            .get::<i64>(0)
            .map_err(|e| db_error("get_fact_by_content", e))?;
        self.get_fact(fact_id).await
    }

    async fn row_to_fact(&self, row: &Row, operation: &str) -> Result<FactRecord> {
        let fact_id = row.get::<i64>(0).map_err(|e| db_error(operation, e))?;
        let entities = self.load_fact_entities(fact_id).await?;
        fact_from_row(row, operation, entities)
    }

    async fn load_entities_for_facts(&self, fact_ids: &[i64]) -> Result<HashMap<i64, Vec<String>>> {
        const PAGE_SIZE: i64 = 512;

        let mut entities: HashMap<i64, Vec<String>> = HashMap::new();
        for chunk in fact_ids.chunks(ENTITY_BATCH_SIZE) {
            let Some(id_list) = sql_i64_list(chunk) else {
                continue;
            };
            let sql = format!(
                "SELECT fe.fact_id, e.name, e.entity_id
                 FROM memory_fact_entities fe
                 JOIN memory_entities e ON e.entity_id = fe.entity_id
                 WHERE fe.fact_id IN ({id_list})
                   AND (
                       ?1 IS NULL
                       OR fe.fact_id > ?1
                       OR (
                           fe.fact_id = ?1
                           AND (
                               e.name > ?2
                               OR (e.name = ?2 AND e.entity_id > ?3)
                           )
                       )
                   )
                 ORDER BY fe.fact_id, e.name, e.entity_id
                 LIMIT ?4"
            );
            let mut fact_cursor: Option<i64> = None;
            let mut name_cursor: Option<String> = None;
            let mut entity_cursor: Option<i64> = None;
            loop {
                let mut rows = self
                    .conn
                    .query(
                        sql.as_str(),
                        params![fact_cursor, name_cursor.as_ref(), entity_cursor, PAGE_SIZE],
                    )
                    .await
                    .map_err(|e| db_error("load_entities_for_facts", e))?;
                let mut page_count = 0;
                while let Some(row) = rows
                    .next()
                    .await
                    .map_err(|e| db_error("load_entities_for_facts", e))?
                {
                    let fact_id = row
                        .get::<i64>(0)
                        .map_err(|e| db_error("load_entities_for_facts", e))?;
                    let entity = row
                        .get::<String>(1)
                        .map_err(|e| db_error("load_entities_for_facts", e))?;
                    fact_cursor = Some(fact_id);
                    name_cursor = Some(entity.clone());
                    entity_cursor = Some(
                        row.get::<i64>(2)
                            .map_err(|e| db_error("load_entities_for_facts", e))?,
                    );
                    entities.entry(fact_id).or_default().push(entity);
                    page_count += 1;
                }
                if page_count < PAGE_SIZE {
                    break;
                }
            }
        }
        Ok(entities)
    }

    async fn replace_fact_entities(&self, fact_id: i64, entities: &[String]) -> Result<()> {
        self.conn
            .execute(
                "DELETE FROM memory_fact_entities WHERE fact_id = ?1",
                params![fact_id],
            )
            .await
            .map_err(|e| db_error("replace_fact_entities", e))?;

        for entity in entities {
            let entity_id = self.resolve_entity(entity).await?;
            self.conn
                .execute(
                    "INSERT OR IGNORE INTO memory_fact_entities (fact_id, entity_id)
                     VALUES (?1, ?2)",
                    params![fact_id, entity_id],
                )
                .await
                .map_err(|e| db_error("replace_fact_entities", e))?;
        }
        Ok(())
    }

    async fn resolve_entity(&self, entity: &str) -> Result<i64> {
        let name = normalize_entity(entity);
        let normalized = name.to_ascii_lowercase();
        let mut rows = self
            .conn
            .query(
                "SELECT entity_id FROM memory_entities WHERE normalized_name = ?1",
                params![normalized.as_str()],
            )
            .await
            .map_err(|e| db_error("resolve_entity", e))?;
        if let Some(row) = rows
            .next()
            .await
            .map_err(|e| db_error("resolve_entity", e))?
        {
            let entity_id = row
                .get::<i64>(0)
                .map_err(|e| db_error("resolve_entity", e))?;
            return Ok(entity_id);
        }

        self.conn
            .execute(
                "INSERT OR IGNORE INTO memory_entities (
                    name, normalized_name, entity_type, aliases, created_at, updated_at
                 )
                 VALUES (?1, ?2, 'unknown', '[]', ?3, ?3)",
                params![name, normalized.as_str(), current_timestamp(),],
            )
            .await
            .map_err(|e| db_error("resolve_entity", e))?;
        let mut rows = self
            .conn
            .query(
                "SELECT entity_id FROM memory_entities WHERE normalized_name = ?1",
                params![normalized.as_str()],
            )
            .await
            .map_err(|e| db_error("resolve_entity", e))?;
        let row = rows
            .next()
            .await
            .map_err(|e| db_error("resolve_entity", e))?
            .ok_or_else(|| db_message("resolve_entity", "entity insert/read returned no row"))?;
        row.get::<i64>(0).map_err(|e| db_error("resolve_entity", e))
    }

    async fn load_fact_entities(&self, fact_id: i64) -> Result<Vec<String>> {
        Ok(self
            .load_entities_for_facts(&[fact_id])
            .await?
            .remove(&fact_id)
            .unwrap_or_default())
    }

    async fn last_insert_rowid(&self, operation: &str) -> Result<i64> {
        let mut rows = self
            .conn
            .query("SELECT last_insert_rowid()", ())
            .await
            .map_err(|e| db_error(operation, e))?;
        let row = rows
            .next()
            .await
            .map_err(|e| db_error(operation, e))?
            .ok_or_else(|| db_message(operation, "last_insert_rowid returned no rows"))?;
        row.get::<i64>(0).map_err(|e| db_error(operation, e))
    }

    fn encode_vector(
        &self,
        content: &str,
        entities: &[String],
        operation: &str,
    ) -> Result<Vec<u8>> {
        let vector = self.encoder.encode_fact(content, entities);
        serialize_vector(&vector, operation)
    }

    async fn update_fact_vector(
        &self,
        fact_id: i64,
        content: &str,
        entities: &[String],
        operation: &str,
    ) -> Result<()> {
        let vector = self.encode_vector(content, entities, operation)?;
        self.conn
            .execute(
                "UPDATE memory_facts
                 SET hrr_vector = ?1,
                     hrr_algebra = ?2,
                     hrr_dim = ?3,
                     hrr_precision = ?4,
                     updated_at = ?5
                 WHERE fact_id = ?6",
                params![
                    vector,
                    HRR_ALGEBRA,
                    HolographicEncoder::DIMENSIONS as i64,
                    HolographicEncoder::HRR_PRECISION,
                    current_timestamp(),
                    fact_id,
                ],
            )
            .await
            .map_err(|e| db_error(operation, e))?;
        Ok(())
    }

}

fn merge_entities(content: &str, explicit: &[String]) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut entities = Vec::new();
    for entity in explicit.iter().cloned().chain(extract_entities(content)) {
        let normalized = normalize_entity(&entity);
        if normalized.is_empty() {
            continue;
        }
        if seen.insert(normalized.to_ascii_lowercase()) {
            entities.push(normalized);
        }
    }
    entities
}

fn merge_metadata_object(existing: &mut serde_json::Value, incoming: &serde_json::Value) -> bool {
    let Some(incoming) = incoming.as_object() else {
        return false;
    };
    if incoming.is_empty() {
        return false;
    }

    if existing.is_null() {
        *existing = serde_json::Value::Object(incoming.clone());
        return true;
    }
    let Some(existing) = existing.as_object_mut() else {
        return false;
    };

    let mut changed = false;
    for (key, value) in incoming {
        if existing.get(key) != Some(value) {
            existing.insert(key.clone(), value.clone());
            changed = true;
        }
    }
    changed
}

fn to_json_string<T: serde::Serialize>(value: &T, operation: &str) -> Result<String> {
    serde_json::to_string(value)
        .map_err(|e| db_message(operation, format!("failed to serialize JSON: {e}")))
}

fn parse_json_array(value: &str, operation: &str) -> Result<Vec<String>> {
    serde_json::from_str(value)
        .map_err(|e| db_message(operation, format!("failed to parse JSON array: {e}")))
}

fn parse_category(value: &str, operation: &str) -> Result<MemoryCategory> {
    value
        .parse()
        .map_err(|e| db_message(operation, format!("failed to parse category: {e}")))
}

fn fact_from_row(row: &Row, operation: &str, entities: Vec<String>) -> Result<FactRecord> {
    let category = parse_category(
        &row.get::<String>(2).map_err(|e| db_error(operation, e))?,
        operation,
    )?;
    let tags = parse_json_array(
        &row.get::<String>(3).map_err(|e| db_error(operation, e))?,
        operation,
    )?;
    let metadata =
        serde_json::from_str(&row.get::<String>(13).map_err(|e| db_error(operation, e))?)
            .map_err(|e| db_message(operation, format!("failed to parse metadata: {e}")))?;

    Ok(FactRecord {
        fact_id: row.get::<i64>(0).map_err(|e| db_error(operation, e))?,
        content: row.get::<String>(1).map_err(|e| db_error(operation, e))?,
        category,
        tags,
        entities,
        trust_score: row.get::<f64>(4).map_err(|e| db_error(operation, e))?,
        source: Some(row.get::<String>(5).map_err(|e| db_error(operation, e))?),
        retrieval_count: row.get::<i64>(6).map_err(|e| db_error(operation, e))?,
        access_count: row.get::<i64>(14).map_err(|e| db_error(operation, e))?,
        helpful_count: row.get::<i64>(7).map_err(|e| db_error(operation, e))?,
        unhelpful_count: row.get::<i64>(8).map_err(|e| db_error(operation, e))?,
        created_at: row.get::<i64>(9).map_err(|e| db_error(operation, e))?,
        updated_at: row.get::<i64>(10).map_err(|e| db_error(operation, e))?,
        last_retrieved_at: row
            .get::<Option<i64>>(11)
            .map_err(|e| db_error(operation, e))?,
        last_recalled_at: row
            .get::<Option<i64>>(15)
            .map_err(|e| db_error(operation, e))?,
        last_feedback_at: row
            .get::<Option<i64>>(12)
            .map_err(|e| db_error(operation, e))?,
        metadata,
    })
}

fn relation_from_row(row: &Row, operation: &str) -> Result<FactRelationRecord> {
    let relation = row
        .get::<String>(2)
        .map_err(|e| db_error(operation, e))?
        .parse::<FactRelationKind>()
        .map_err(|e| db_message(operation, e))?;
    let metadata = serde_json::from_str(&row.get::<String>(5).map_err(|e| db_error(operation, e))?)
        .map_err(|e| db_message(operation, format!("failed to parse relation metadata: {e}")))?;
    Ok(FactRelationRecord {
        source_fact_id: row.get::<i64>(0).map_err(|e| db_error(operation, e))?,
        target_fact_id: row.get::<i64>(1).map_err(|e| db_error(operation, e))?,
        relation,
        confidence: row.get::<f64>(3).map_err(|e| db_error(operation, e))?,
        source: row.get::<String>(4).map_err(|e| db_error(operation, e))?,
        metadata,
        created_at: row.get::<i64>(6).map_err(|e| db_error(operation, e))?,
        updated_at: row.get::<i64>(7).map_err(|e| db_error(operation, e))?,
    })
}

fn relations_conflict(left: FactRelationKind, right: FactRelationKind) -> bool {
    matches!(
        (left, right),
        (FactRelationKind::Supports, FactRelationKind::Contradicts)
            | (FactRelationKind::Contradicts, FactRelationKind::Supports)
    )
}

fn serialize_vector(vector: &[f64], operation: &str) -> Result<Vec<u8>> {
    HolographicEncoder::serialize(vector)
        .map_err(|e| db_message(operation, format!("failed to serialize vector: {e}")))
}

fn deserialize_vector_value(value: Value, operation: &str) -> Result<Option<Vec<f64>>> {
    match value {
        Value::Blob(bytes) => HolographicEncoder::deserialize(&bytes)
            .map(Some)
            .map_err(|e| db_message(operation, format!("failed to decode vector: {e}"))),
        Value::Null => Ok(None),
        _ => Err(db_message(
            operation,
            "hrr_vector contained a non-blob value",
        )),
    }
}

fn sql_i64_list(ids: &[i64]) -> Option<String> {
    if ids.is_empty() {
        None
    } else {
        Some(
            ids.iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", "),
        )
    }
}

fn feedback_action_str(action: FeedbackAction) -> &'static str {
    match action {
        FeedbackAction::Helpful => "helpful",
        FeedbackAction::Unhelpful => "unhelpful",
    }
}

fn parse_feedback_action(value: &str, operation: &str) -> Result<FeedbackAction> {
    match value {
        "helpful" => Ok(FeedbackAction::Helpful),
        "unhelpful" => Ok(FeedbackAction::Unhelpful),
        other => Err(db_message(
            operation,
            format!("failed to parse feedback action: {other}"),
        )),
    }
}

fn normalized_limit(limit: usize) -> usize {
    if limit == 0 {
        DEFAULT_LIMIT
    } else {
        limit.min(i64::MAX as usize)
    }
}

fn db_error(operation: &str, error: impl fmt::Display) -> TraceDecayError {
    TraceDecayError::Database {
        message: error.to_string(),
        operation: operation.to_string(),
    }
}

fn db_message(operation: &str, message: impl Into<String>) -> TraceDecayError {
    TraceDecayError::Database {
        message: message.into(),
        operation: operation.to_string(),
    }
}

#[cfg(test)]
mod cancellation_tests {
    use std::future::pending;
    use std::sync::Arc;

    use super::*;
    use crate::memory::trust::DEFAULT_TRUST;
    use crate::memory::types::AddFactRequest;

    #[tokio::test]
    async fn cancelled_immediate_transaction_rolls_back() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("memory.db");
        let runtime = Arc::new(crate::db::engine::TestConnection::open(&path));
        runtime
            .execute_batch("CREATE TABLE cancellation_probe(value INTEGER NOT NULL)")
            .await
            .unwrap();
        let retained = Arc::clone(&runtime);
        let (started, started_rx) = tokio::sync::oneshot::channel();

        let task = tokio::spawn(async move {
            MemoryStore::new_runtime(runtime.as_ref())
                .with_immediate_tx("cancelled memory write", move |transactional| {
                    Box::pin(async move {
                        transactional
                            .conn
                            .execute(
                                "INSERT INTO cancellation_probe(value) VALUES(?1)",
                                params![1_i64],
                            )
                            .await
                            .unwrap();
                        let _ = started.send(());
                        pending::<Result<()>>().await
                    })
                })
                .await
        });
        started_rx.await.unwrap();
        task.abort();
        let _ = task.await;

        let mut rows = retained
            .query("SELECT COUNT(*) FROM cancellation_probe", ())
            .await
            .unwrap();
        assert_eq!(
            rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
            0
        );
    }

    #[tokio::test]
    async fn runtime_memory_store_executes_writes_inside_its_transaction() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("runtime-memory.db");
        let authority =
            crate::db::DatabaseAuthority::acquire_test(&path, "runtime memory transaction parity")
                .unwrap();
        let (database, _) = crate::db::Database::publish_test_runtime(
            &path,
            &authority,
            crate::db::TestDatabaseRuntimeMode::Initialize,
        )
        .await
        .unwrap();
        drop(database);
        drop(authority);

        let runtime = crate::db::engine::TestConnection::open(&path);
        let store = MemoryStore::new_runtime(&runtime);
        let outcome = store
            .add_fact(
                AddFactRequest {
                    content: "runtime memory transaction fact".to_owned(),
                    category: MemoryCategory::Project,
                    tags: vec!["runtime".to_owned()],
                    entities: vec!["TraceDecay".to_owned()],
                    trust: Some(DEFAULT_TRUST),
                    source: Some("runtime-test".to_owned()),
                    metadata: serde_json::json!({"engine": "rusqlite"}),
                },
                DEFAULT_TRUST,
            )
            .await
            .unwrap();

        let fact = outcome.fact.expect("runtime fact");
        assert_eq!(
            store.get_fact(fact.fact_id).await.unwrap().unwrap().content,
            "runtime memory transaction fact"
        );
    }

    #[tokio::test]
    async fn runtime_memory_store_rolls_back_failed_transaction_work() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("runtime-memory-rollback.db");
        let runtime = crate::db::engine::TestConnection::open(&path);
        runtime
            .execute_batch("CREATE TABLE transaction_probe(value INTEGER NOT NULL)")
            .await
            .unwrap();
        let store = MemoryStore::new_runtime(&runtime);

        let result: Result<()> = store
            .with_immediate_tx("runtime rollback parity", |transactional| {
                Box::pin(async move {
                    transactional
                        .conn
                        .execute(
                            "INSERT INTO transaction_probe(value) VALUES(?1)",
                            params![42_i64],
                        )
                        .await
                        .map_err(|error| db_error("runtime rollback parity", error))?;
                    Err(db_message("runtime rollback parity", "deliberate failure"))
                })
            })
            .await;
        assert!(result.is_err());

        let mut rows = runtime
            .query("SELECT COUNT(*) FROM transaction_probe", ())
            .await
            .unwrap();
        assert_eq!(
            rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
            0
        );
    }

    #[tokio::test]
    async fn database_transaction_memory_store_uses_ambient_transaction() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("database-memory-transaction.db");
        let authority =
            crate::db::DatabaseAuthority::acquire_test(&path, "database memory transaction")
                .unwrap();
        let (database, _) = crate::db::Database::publish_test_runtime(
            &path,
            &authority,
            crate::db::TestDatabaseRuntimeMode::Initialize,
        )
        .await
        .unwrap();
        let transaction = database
            .begin_memory_write_transaction("database memory transaction")
            .await
            .unwrap();
        let store = MemoryStore::new_database_transaction(&transaction);

        let outcome = store
            .add_fact(
                AddFactRequest {
                    content: "ambient database transaction fact".to_owned(),
                    category: MemoryCategory::Project,
                    tags: Vec::new(),
                    entities: Vec::new(),
                    trust: Some(DEFAULT_TRUST),
                    source: Some("database-transaction-test".to_owned()),
                    metadata: serde_json::Value::Null,
                },
                DEFAULT_TRUST,
            )
            .await
            .unwrap();
        assert!(outcome.fact.is_some());

        transaction.rollback().await.unwrap();
        let read = database
            .begin_memory_read_transaction("verify database memory rollback")
            .await
            .unwrap();
        let mut rows = read
            .query(
                "SELECT COUNT(*) FROM memory_facts WHERE content = ?1",
                params!["ambient database transaction fact"],
            )
            .await
            .unwrap();
        assert_eq!(
            rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
            0
        );
        read.commit().await.unwrap();
    }
}
