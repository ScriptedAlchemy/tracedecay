use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result};
use futures::TryStreamExt;
use lancedb::DistanceType;
use lancedb::arrow::arrow_array::types::Float32Type;
use lancedb::arrow::arrow_array::{
    FixedSizeListArray, Int64Array, RecordBatch, RecordBatchIterator, RecordBatchReader,
};
use lancedb::arrow::arrow_schema::{DataType, Field, Schema, SchemaRef};
use lancedb::index::Index;
use lancedb::index::vector::IvfPqIndexBuilder;
use lancedb::query::{ExecutableQuery, QueryBase, Select};
use lancedb::{Table, connect};
use rusqlite::{Connection, TransactionBehavior, params};

use crate::common::{
    BATCH, CaseMetrics, CrashMetrics, DIM, QUERY_IDS, ROWS, Timing, directory_bytes, peak_rss_kib,
    query_metrics, recall_at_10, synthetic_vector,
};

const TABLE_NAME: &str = "vectors";
const POINTER_NAME: &str = "active.sqlite3";

pub async fn run(root: &Path, executable: &Path) -> Result<CaseMetrics> {
    std::fs::create_dir_all(root)?;
    let uri = root.join("lance");
    let database = connect(path_string(&uri)?).execute().await?;

    let ingest_started = Instant::now();
    let table = database
        .create_table(TABLE_NAME, batch_reader(0, ROWS, 0)?)
        .execute()
        .await?;
    let ingest_ms = ingest_started.elapsed().as_secs_f64() * 1_000.0;

    let index_started = Instant::now();
    table
        .create_index(
            &["vector"],
            Index::IvfPq(
                IvfPqIndexBuilder::default()
                    .distance_type(DistanceType::Cosine)
                    .num_partitions(256)
                    .num_sub_vectors(96),
            ),
        )
        .execute()
        .await?;
    let indexed_version = table.version().await?;
    let index_ms = index_started.elapsed().as_secs_f64() * 1_000.0;
    let pointer_path = root.join(POINTER_NAME);
    create_pointer(&pointer_path)?;
    let initial_publish_started = Instant::now();
    pointer_cas(&pointer_path, 0, 1, indexed_version)?;
    let initial_publish_ms = index_ms + initial_publish_started.elapsed().as_secs_f64() * 1_000.0;

    let mut deltas = Vec::new();
    for (delta_index, rows) in [1_usize, 100, 4_096].into_iter().enumerate() {
        let revision = delta_index as u64 + 2;
        let started = Instant::now();
        merge_delta(&table, revision, rows).await?;
        let version = table.version().await?;
        pointer_cas(&pointer_path, revision - 1, revision, version)?;
        deltas.push(Timing {
            operation: format!("publish_delta_{rows}"),
            rows,
            millis: started.elapsed().as_secs_f64() * 1_000.0,
        });
    }

    let (_, active_version) = pointer(&pointer_path)?;
    let pinned = database
        .open_table(TABLE_NAME)
        .version(active_version)
        .execute()
        .await?;
    let exact = vector_queries(&pinned, true).await?;
    let ann = vector_queries(&pinned, false).await?;
    let ann_recall_at_10 = recall_at_10(&exact.results, &ann.results);
    let crash = crash_test(root, executable).await?;
    let (active_revision, active_native_version) = pointer(&pointer_path)?;

    Ok(CaseMetrics {
        engine: "lancedb-0.31.0".to_owned(),
        rows: ROWS,
        dimensions: DIM,
        ingest_ms,
        initial_publish_ms,
        deltas,
        exact,
        ann: Some(ann),
        ann_recall_at_10: Some(ann_recall_at_10),
        peak_rss_kib: peak_rss_kib(),
        disk_bytes: directory_bytes(root)?,
        crash,
        active_revision,
        active_native_version: Some(active_native_version),
        publication_primitive:
            "Lance optimistic version commit followed by external SQLite pointer CAS; reads pin version"
                .to_owned(),
        native_expected_version_cas: false,
    })
}

pub async fn crash_child(root: &Path, commit_pointer: bool) -> Result<()> {
    let uri = root.join("lance");
    let pointer_path = root.join(POINTER_NAME);
    let (revision, _) = pointer(&pointer_path)?;
    let database = connect(path_string(&uri)?).execute().await?;
    let table = database.open_table(TABLE_NAME).execute().await?;
    merge_delta(&table, 100 + revision, 1).await?;
    let version = table.version().await?;
    if commit_pointer {
        pointer_cas(&pointer_path, revision, revision + 1, version)?;
    }
    unsafe { libc::_exit(86) }
}

fn schema() -> SchemaRef {
    Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, false),
        Field::new("revision", DataType::Int64, false),
        Field::new(
            "vector",
            DataType::FixedSizeList(
                Arc::new(Field::new("item", DataType::Float32, true)),
                DIM as i32,
            ),
            false,
        ),
    ]))
}

fn batch_reader(
    start: usize,
    count: usize,
    revision: u64,
) -> Result<Box<dyn RecordBatchReader + Send>> {
    let schema = schema();
    Ok(Box::new(RecordBatchIterator::new(
        SyntheticBatches {
            schema: schema.clone(),
            next: start,
            end: start + count,
            revision,
        },
        schema.clone(),
    )))
}

struct SyntheticBatches {
    schema: SchemaRef,
    next: usize,
    end: usize,
    revision: u64,
}

impl Iterator for SyntheticBatches {
    type Item = std::result::Result<RecordBatch, lancedb::arrow::arrow_schema::ArrowError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.next == self.end {
            return None;
        }
        let start = self.next;
        let end = (start + BATCH).min(self.end);
        self.next = end;
        let ids = start..end;
        let vectors = ids.clone().map(|id| {
            Some(
                synthetic_vector(id as u64, self.revision)
                    .into_iter()
                    .map(Some)
                    .collect::<Vec<_>>(),
            )
        });
        Some(RecordBatch::try_new(
            self.schema.clone(),
            vec![
                Arc::new(Int64Array::from_iter_values(ids.map(|id| id as i64))),
                Arc::new(Int64Array::from_value(self.revision as i64, end - start)),
                Arc::new(
                    FixedSizeListArray::from_iter_primitive::<Float32Type, _, _>(
                        vectors, DIM as i32,
                    ),
                ),
            ],
        ))
    }
}

async fn merge_delta(table: &Table, revision: u64, count: usize) -> Result<()> {
    let mut merge = table.merge_insert(&["id"]);
    merge
        .when_matched_update_all(None)
        .when_not_matched_insert_all();
    merge.execute(batch_reader(0, count, revision)?).await?;
    Ok(())
}

async fn vector_queries(table: &Table, exact: bool) -> Result<crate::common::QueryMetrics> {
    let mut timings = Vec::new();
    let mut all_results = Vec::new();
    for query_id in QUERY_IDS {
        let query = synthetic_vector(query_id, 0);
        let started = Instant::now();
        let base = table
            .vector_search(query)?
            .distance_type(DistanceType::Cosine)
            .limit(crate::common::TOP_K)
            .select(Select::columns(&["id"]));
        let mut stream = if exact {
            base.bypass_vector_index().execute().await?
        } else {
            base.nprobes(32).refine_factor(2).execute().await?
        };
        let mut ids = Vec::new();
        while let Some(batch) = stream.try_next().await? {
            let column = batch
                .column_by_name("id")
                .context("Lance query did not return id")?
                .as_any()
                .downcast_ref::<Int64Array>()
                .context("Lance id column is not Int64")?;
            ids.extend(column.values().iter().map(|id| *id as u64));
        }
        timings.push(started.elapsed());
        all_results.push(ids);
    }
    Ok(query_metrics(
        if exact {
            "exact-bypass-vector-index"
        } else {
            "ann-ivf-pq-nprobes-32-refine-2"
        },
        timings,
        all_results,
    ))
}

fn create_pointer(path: &Path) -> Result<()> {
    let connection = Connection::open(path)?;
    connection.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA synchronous = FULL;
         CREATE TABLE active_state(
             singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
             revision INTEGER NOT NULL,
             lance_version INTEGER NOT NULL
         );
         INSERT INTO active_state VALUES(1, 0, 0);",
    )?;
    Ok(())
}

fn pointer_cas(path: &Path, expected: u64, revision: u64, version: u64) -> Result<()> {
    let mut connection = Connection::open(path)?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let changed = transaction.execute(
        "UPDATE active_state SET revision = ?1, lance_version = ?2
         WHERE singleton = 1 AND revision = ?3",
        params![revision as i64, version as i64, expected as i64],
    )?;
    anyhow::ensure!(changed == 1, "Lance pointer compare-and-swap failed");
    transaction.commit()?;
    Ok(())
}

fn pointer(path: &Path) -> Result<(u64, u64)> {
    let (revision, version) = Connection::open(path)?.query_row(
        "SELECT revision, lance_version FROM active_state WHERE singleton = 1",
        [],
        |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
    )?;
    Ok((revision as u64, version as u64))
}

async fn crash_test(root: &Path, executable: &Path) -> Result<CrashMetrics> {
    let pointer_path = root.join(POINTER_NAME);
    let before = pointer(&pointer_path)?;
    invoke_crash_child(executable, root, "before")?;
    let before_commit_old_visible = pointer(&pointer_path)? == before;
    invoke_crash_child(executable, root, "after")?;
    let after = pointer(&pointer_path)?;
    let after_commit_new_visible = after.0 == before.0 + 1 && after.1 > before.1;
    let database = connect(path_string(&root.join("lance"))?).execute().await?;
    database
        .open_table(TABLE_NAME)
        .version(after.1)
        .execute()
        .await
        .context("reopen committed Lance version")?;
    Ok(CrashMetrics {
        before_commit_old_visible,
        after_commit_new_visible,
    })
}

fn invoke_crash_child(executable: &Path, root: &Path, phase: &str) -> Result<()> {
    let status = Command::new(executable)
        .arg("lance-crash")
        .arg(root)
        .arg(phase)
        .status()
        .context("launch Lance crash child")?;
    anyhow::ensure!(status.code() == Some(86), "unexpected crash child {status}");
    Ok(())
}

fn path_string(path: &Path) -> Result<&str> {
    path.to_str()
        .with_context(|| format!("non-UTF-8 path {}", path.display()))
}
