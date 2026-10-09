use tracedecay_runtime_core::db::Database;
use tracedecay_runtime_core::db::engine::{Executor, QueryExecutor, params};
use tracedecay_store::{
    CANONICAL_BODIES_TABLE_SQL, CanonicalBodyError, INLINE_BODY_BYTES, LOAD_CANONICAL_BODY_SQL,
    StoredCanonicalBody, UPSERT_CANONICAL_BODY_SQL, collect_body_refs, parse_stored_observation,
    slim_stored_json, unpack_body,
};

use super::super::global_db_operation_error;
use super::codec::storage;

pub(super) const CANONICAL_BODY_MIGRATION: &str = "session-canonical-bodies-v1";
const LCM_CANONICAL_BODY_MIGRATION: &str = "session-canonical-bodies-lcm-v1";
const LCM_PLACEHOLDER_SNIPPET_MIGRATION: &str = "session-canonical-bodies-lcm-snippet-v1";
const OPERATION: &str = "compact session canonical bodies";
const COMPACT_PAGE: i64 = 64;
const LIFT_OBSERVATION_IMMUTABILITY: &str = "
    DROP TRIGGER IF EXISTS observations_immutable_update;
    CREATE TRIGGER observations_immutable_update
        BEFORE UPDATE ON observations BEGIN SELECT 1; END;";
const RESTORE_OBSERVATION_IMMUTABILITY: &str = "
    DROP TRIGGER IF EXISTS observations_immutable_update;
    CREATE TRIGGER observations_immutable_update
        BEFORE UPDATE ON observations BEGIN
            SELECT RAISE(ABORT, 'observations are immutable');
        END;";

pub(crate) async fn ensure_canonical_bodies_table(
    conn: &impl Executor,
) -> tracedecay_domain::errors::Result<()> {
    conn.execute_batch(CANONICAL_BODIES_TABLE_SQL)
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))
}

struct CompactionPage<T> {
    next: Option<T>,
    rewrote: bool,
}

/// Each page restores guards before committing and releases the canonical
/// writer. Interrupted runs can rescan already-slim rows without changing them.
pub(crate) async fn converge_canonical_bodies(
    database: &Database,
) -> tracedecay_domain::errors::Result<()> {
    let mut after = None;
    loop {
        let transaction = database.begin_write_transaction(OPERATION).await?;
        let page = compact_observation_page(&transaction, after.as_deref()).await?;
        transaction.commit().await?;
        after = page.next;
        if after.is_none() {
            break;
        }
        tokio::task::yield_now().await;
    }
    let mut after = 0;
    loop {
        let transaction = database.begin_write_transaction(OPERATION).await?;
        if migration_recorded(&transaction, LCM_CANONICAL_BODY_MIGRATION).await?
            && migration_recorded(&transaction, LCM_PLACEHOLDER_SNIPPET_MIGRATION).await?
        {
            transaction.commit().await?;
            break;
        }
        let page = compact_lcm_page(&transaction, after).await?;
        if page.next.is_none() {
            record_migration(&transaction, LCM_CANONICAL_BODY_MIGRATION).await?;
            record_migration(&transaction, LCM_PLACEHOLDER_SNIPPET_MIGRATION).await?;
        }
        transaction.commit().await?;
        let Some(next) = page.next else {
            break;
        };
        after = next;
        tokio::task::yield_now().await;
    }
    Ok(())
}

async fn record_migration(
    conn: &impl Executor,
    migration: &str,
) -> tracedecay_domain::errors::Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO global_schema_migrations(migration) VALUES (?1)",
        params![migration],
    )
    .await
    .map_err(|error| global_db_operation_error(OPERATION, error))?;
    Ok(())
}

async fn compact_observation_page(
    conn: &impl Executor,
    after: Option<&str>,
) -> tracedecay_domain::errors::Result<CompactionPage<String>> {
    ensure_canonical_bodies_table(conn).await?;
    if migration_recorded(conn, CANONICAL_BODY_MIGRATION).await? {
        return Ok(CompactionPage {
            next: None,
            rewrote: false,
        });
    }
    let mut rows = conn
        .query(
            "SELECT observation_id, observation_json FROM observations
         WHERE (?1 IS NULL OR observation_id > ?1)
         ORDER BY observation_id LIMIT ?2",
            params![after, COMPACT_PAGE],
        )
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    let mut page = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?
    {
        page.push((
            row.get::<String>(0)
                .map_err(|error| global_db_operation_error(OPERATION, error))?,
            row.get::<String>(1)
                .map_err(|error| global_db_operation_error(OPERATION, error))?,
        ));
    }
    drop(rows);
    if page.is_empty() {
        record_migration(conn, CANONICAL_BODY_MIGRATION).await?;
        return Ok(CompactionPage {
            next: None,
            rewrote: false,
        });
    }
    conn.execute_batch(LIFT_OBSERVATION_IMMUTABILITY)
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    let mut next = None;
    let mut rewrote = false;
    for (id, json) in page {
        if json.len() >= INLINE_BODY_BYTES {
            let (slim, bodies) = slim_stored_json(&json)
                .map_err(|error| global_db_operation_error(OPERATION, error))?;
            persist_bodies(conn, &bodies).await?;
            if slim != json {
                conn.execute(
                    "UPDATE observations SET observation_json = ?1 WHERE observation_id = ?2",
                    params![slim, id.as_str()],
                )
                .await
                .map_err(|error| global_db_operation_error(OPERATION, error))?;
                rewrote = true;
            }
        }
        next = Some(id);
    }
    conn.execute_batch(RESTORE_OBSERVATION_IMMUTABILITY)
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    Ok(CompactionPage { next, rewrote })
}

pub(crate) async fn compact_lcm_bodies(
    conn: &impl Executor,
) -> tracedecay_domain::errors::Result<bool> {
    let mut after = 0;
    let mut rewrote = false;
    loop {
        let page = compact_lcm_page(conn, after).await?;
        rewrote |= page.rewrote;
        let Some(next) = page.next else {
            return Ok(rewrote);
        };
        after = next;
    }
}

async fn compact_lcm_page(
    conn: &impl Executor,
    after: i64,
) -> tracedecay_domain::errors::Result<CompactionPage<i64>> {
    ensure_canonical_bodies_table(conn).await?;
    if !table_exists(conn, "lcm_raw_messages").await? {
        return Ok(CompactionPage {
            next: None,
            rewrote: false,
        });
    }
    let mut rows = conn.query(
        "SELECT store_id, content, placeholder_text FROM lcm_raw_messages WHERE store_id > ?1 ORDER BY store_id LIMIT ?2",
        params![after, COMPACT_PAGE]
    ).await.map_err(|error| global_db_operation_error(OPERATION, error))?;
    let mut page = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?
    {
        page.push((
            row.get::<i64>(0)
                .map_err(|error| global_db_operation_error(OPERATION, error))?,
            row.get::<Option<String>>(1)
                .map_err(|error| global_db_operation_error(OPERATION, error))?,
            row.get::<Option<String>>(2)
                .map_err(|error| global_db_operation_error(OPERATION, error))?,
        ));
    }
    drop(rows);
    let mut next = None;
    let mut rewrote = false;
    for (id, content, placeholder) in page {
        next = Some(id);
        let Some(content) = content.filter(|content| content.len() >= INLINE_BODY_BYTES) else {
            if let Some(placeholder) = placeholder {
                let snippet =
                    tracedecay_lcm::retrieval_content::derived_text_for_snippet(&placeholder);
                if snippet != placeholder {
                    conn.execute(
                        "UPDATE lcm_raw_messages SET placeholder_text = ?1 WHERE store_id = ?2",
                        params![snippet, id],
                    )
                    .await
                    .map_err(|error| global_db_operation_error(OPERATION, error))?;
                    rewrote = true;
                }
            }
            continue;
        };
        let body = StoredCanonicalBody::pack(content.as_bytes())
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        persist_bodies(conn, std::slice::from_ref(&body)).await?;
        let placeholder = tracedecay_lcm::retrieval_content::derived_text_for_snippet(&content);
        conn.execute(
            "UPDATE lcm_raw_messages SET content = NULL, placeholder_text = ?1 WHERE store_id = ?2",
            params![placeholder, id],
        )
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
        rewrote = true;
    }
    Ok(CompactionPage { next, rewrote })
}

pub async fn decode_observation_json(
    conn: &impl QueryExecutor,
    observation_json: &str,
    operation: &'static str,
) -> tracedecay_store::ObservationStoreResult<tracedecay_domain::DurableObservationV1> {
    let hashes = collect_body_refs(observation_json).map_err(|error| storage(operation, error))?;
    let mut bodies = std::collections::HashMap::new();
    for hash in hashes {
        bodies.insert(hash.clone(), load_canonical_body(conn, &hash).await?);
    }
    parse_stored_observation(observation_json, |hash| {
        bodies
            .get(hash)
            .cloned()
            .ok_or_else(|| CanonicalBodyError::Missing {
                content_hash: hash.to_owned(),
            })
    })
    .map_err(|error| storage(operation, error))
}

pub(crate) async fn load_canonical_body(
    conn: &impl QueryExecutor,
    content_hash: &str,
) -> tracedecay_store::ObservationStoreResult<Vec<u8>> {
    const OPERATION: &str = "read canonical observation body";
    let mut rows = conn
        .query(LOAD_CANONICAL_BODY_SQL, params![content_hash])
        .await
        .map_err(|error| storage(OPERATION, error))?;
    let row = rows
        .next()
        .await
        .map_err(|error| storage(OPERATION, error))?
        .ok_or_else(|| {
            storage(
                OPERATION,
                CanonicalBodyError::Missing {
                    content_hash: content_hash.to_owned(),
                },
            )
        })?;
    let encoding: String = row.get(0).map_err(|error| storage(OPERATION, error))?;
    let blob: Vec<u8> = row.get(1).map_err(|error| storage(OPERATION, error))?;
    unpack_body(content_hash, &encoding, &blob).map_err(|error| storage(OPERATION, error))
}

async fn persist_bodies(
    conn: &impl Executor,
    bodies: &[StoredCanonicalBody],
) -> tracedecay_domain::errors::Result<()> {
    for body in bodies {
        conn.execute(
            UPSERT_CANONICAL_BODY_SQL,
            params![
                body.content_hash.as_str(),
                body.encoding,
                body.blob.as_slice(),
                body.uncompressed_bytes
            ],
        )
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    }
    Ok(())
}

async fn migration_recorded(
    conn: &impl QueryExecutor,
    migration: &str,
) -> tracedecay_domain::errors::Result<bool> {
    let mut rows = conn
        .query(
            "SELECT 1 FROM global_schema_migrations WHERE migration = ?1",
            params![migration],
        )
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    rows.next()
        .await
        .map(|row| row.is_some())
        .map_err(|error| global_db_operation_error(OPERATION, error))
}

async fn table_exists(
    conn: &impl QueryExecutor,
    name: &str,
) -> tracedecay_domain::errors::Result<bool> {
    let mut rows = conn
        .query(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
            params![name],
        )
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    rows.next()
        .await
        .map(|row| row.is_some())
        .map_err(|error| global_db_operation_error(OPERATION, error))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use tracedecay_runtime_core::db::engine::TestConnection;
    use tracedecay_store::{BODY_REF_KEY, unpack_body};

    async fn compact_observation_bodies(
        conn: &impl Executor,
    ) -> tracedecay_domain::errors::Result<bool> {
        let mut after = None;
        let mut rewrote = false;
        loop {
            let page = compact_observation_page(conn, after.as_deref()).await?;
            rewrote |= page.rewrote;
            after = page.next;
            if after.is_none() {
                return Ok(rewrote);
            }
        }
    }

    fn family_bytes(path: &std::path::Path) -> u64 {
        ["", "-wal", "-shm"].iter().fold(0u64, |total, suffix| {
            let member = if suffix.is_empty() {
                path.to_path_buf()
            } else {
                let mut name = path.as_os_str().to_os_string();
                name.push(*suffix);
                std::path::PathBuf::from(name)
            };
            total.saturating_add(
                std::fs::metadata(member)
                    .map(|meta| meta.len())
                    .unwrap_or(0),
            )
        })
    }

    #[tokio::test]
    async fn body_reads_distinguish_storage_failures_from_missing_rows() {
        let tmp = TempDir::new().unwrap();
        let conn = TestConnection::open(&tmp.path().join("body-read-errors.db"));
        let error = load_canonical_body(&conn, "missing").await.unwrap_err();
        let tracedecay_store::ObservationStoreError::Storage { source, .. } = error else {
            panic!("expected a storage error");
        };
        assert!(source.is::<tracedecay_runtime_core::db::engine::Error>());
        ensure_canonical_bodies_table(&conn).await.unwrap();
        let error = load_canonical_body(&conn, "missing").await.unwrap_err();
        let tracedecay_store::ObservationStoreError::Storage { source, .. } = error else {
            panic!("expected a missing-body error");
        };
        assert!(
            matches!(source.downcast_ref::<CanonicalBodyError>(), Some(CanonicalBodyError::Missing { content_hash }) if content_hash == "missing")
        );
    }

    #[tokio::test]
    async fn compaction_advances_past_unchanged_rows_and_counts_utf8_bytes() {
        let tmp = TempDir::new().unwrap();
        let conn = TestConnection::open(&tmp.path().join("observations.db"));
        conn.execute_batch(
            "CREATE TABLE global_schema_migrations (migration TEXT PRIMARY KEY);
             CREATE TABLE observations (observation_id TEXT PRIMARY KEY, observation_json TEXT NOT NULL);",
        ).await.unwrap();
        let unchanged = serde_json::json!({"pieces": vec!["s".repeat(512); 9]}).to_string();
        for index in 0..=COMPACT_PAGE {
            conn.execute(
                "INSERT INTO observations VALUES (?1, ?2)",
                params![format!("a.{index:03}"), unchanged.as_str()],
            )
            .await
            .unwrap();
        }
        let text = format!("{BODY_REF_KEY}{}", "界".repeat(INLINE_BODY_BYTES / 3 + 1));
        let large = serde_json::json!({"text": text}).to_string();
        assert!(large.len() >= INLINE_BODY_BYTES);
        assert!(large.chars().count() < INLINE_BODY_BYTES);
        conn.execute(
            "INSERT INTO observations VALUES ('z.large', ?1)",
            params![large],
        )
        .await
        .unwrap();

        let first = compact_observation_page(&conn, None).await.unwrap();
        assert!(!first.rewrote);
        assert!(first.next.is_some());
        assert!(
            !migration_recorded(&conn, CANONICAL_BODY_MIGRATION)
                .await
                .unwrap()
        );
        assert!(
            conn.execute(
                "UPDATE observations SET observation_json = '{}' WHERE observation_id = 'a.000'",
                ()
            )
            .await
            .is_err()
        );
        assert!(compact_observation_bodies(&conn).await.unwrap());
        assert!(!compact_observation_bodies(&conn).await.unwrap());
        let mut rows = conn
            .query(
                "SELECT observation_json FROM observations ORDER BY observation_id",
                (),
            )
            .await
            .unwrap();
        for _ in 0..=COMPACT_PAGE {
            assert_eq!(
                rows.next()
                    .await
                    .unwrap()
                    .unwrap()
                    .get::<String>(0)
                    .unwrap(),
                unchanged
            );
        }
        let compacted: String = rows.next().await.unwrap().unwrap().get(0).unwrap();
        let hashes = collect_body_refs(&compacted).unwrap();
        assert_eq!(hashes.len(), 1);
        assert_eq!(
            load_canonical_body(&conn, &hashes[0]).await.unwrap(),
            text.as_bytes()
        );
        assert!(rows.next().await.unwrap().is_none());
        assert!(
            conn.execute("UPDATE observations SET observation_json = '{}'", ())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn compaction_stores_large_bodies_once_and_shrinks_existing_rows() {
        const N: usize = 16;
        const PAYLOAD_CHARS: usize = 80_000;
        let stem = "m".repeat(PAYLOAD_CHARS);
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("user-sessions.db");
        let conn = TestConnection::open(&path);
        conn.execute_batch(&format!(
            "CREATE TABLE global_schema_migrations (migration TEXT PRIMARY KEY);
             CREATE TABLE observations (
                 observation_id TEXT PRIMARY KEY,
                 observation_json TEXT NOT NULL
             );
             CREATE TABLE lcm_raw_messages (
                 store_id INTEGER PRIMARY KEY AUTOINCREMENT,
                 content TEXT,
                 placeholder_text TEXT
             );
             {CANONICAL_BODIES_TABLE_SQL}
             CREATE TRIGGER observations_immutable_update
                 BEFORE UPDATE ON observations BEGIN
                     SELECT RAISE(ABORT, 'observations are immutable');
                 END;"
        ))
        .await
        .unwrap();

        let mut unique_payload_bytes = 0i64;
        for i in 0..N {
            let text = format!("payload-{i:02}-{stem}");
            unique_payload_bytes += i64::try_from(text.len()).unwrap();
            let json = serde_json::json!({ "text": text }).to_string();
            conn.execute(
                "INSERT INTO observations(observation_id, observation_json) VALUES (?1, ?2)",
                params![format!("obs.{i}"), json],
            )
            .await
            .unwrap();
            conn.execute(
                "INSERT INTO lcm_raw_messages(content) VALUES (?1)",
                params![text],
            )
            .await
            .unwrap();
        }
        let before_bytes = family_bytes(&path);
        let started = std::time::Instant::now();
        assert!(compact_observation_bodies(&conn).await.unwrap());
        assert!(compact_lcm_bodies(&conn).await.unwrap());
        let elapsed = started.elapsed();
        let after_bytes = family_bytes(&path);

        let mut census = conn
            .query(
                "SELECT COUNT(*), COALESCE(SUM(length(body)), 0), COALESCE(SUM(uncompressed_bytes), 0)
                 FROM session_canonical_bodies",
                (),
            )
            .await
            .unwrap();
        let row = census.next().await.unwrap().unwrap();
        let stored_bodies: i64 = row.get(0).unwrap();
        let stored_body_bytes: i64 = row.get(1).unwrap();
        let uncompressed_body_bytes: i64 = row.get(2).unwrap();
        assert_eq!(stored_bodies, i64::try_from(N).unwrap());
        assert_eq!(uncompressed_body_bytes, unique_payload_bytes);
        assert!(stored_body_bytes < uncompressed_body_bytes);
        let before_logical: i64 = unique_payload_bytes.saturating_mul(2);
        let after_logical = stored_body_bytes;
        assert!(
            after_logical < before_logical,
            "compaction must store the payload once ({after_logical} vs {before_logical})"
        );

        let observation_copy: i64 = conn
            .query(
                "SELECT COUNT(*) FROM observations WHERE instr(observation_json, ?1) > 0",
                params![stem.as_str()],
            )
            .await
            .unwrap()
            .next()
            .await
            .unwrap()
            .unwrap()
            .get(0)
            .unwrap();
        assert_eq!(observation_copy, 0);
        let lcm_copy: i64 = conn
            .query(
                "SELECT COUNT(*) FROM lcm_raw_messages
                 WHERE content IS NOT NULL AND length(content) >= ?1",
                params![i64::try_from(INLINE_BODY_BYTES).unwrap()],
            )
            .await
            .unwrap()
            .next()
            .await
            .unwrap()
            .unwrap()
            .get(0)
            .unwrap();
        assert_eq!(lcm_copy, 0);
        let long_placeholder: i64 = conn
            .query(
                "SELECT COUNT(*) FROM lcm_raw_messages
                 WHERE length(COALESCE(placeholder_text, '')) > ?1",
                params![
                    i64::try_from(tracedecay_lcm::retrieval_content::MAX_DERIVED_SNIPPET_CHARS)
                        .unwrap()
                ],
            )
            .await
            .unwrap()
            .next()
            .await
            .unwrap()
            .unwrap()
            .get(0)
            .unwrap();
        assert_eq!(
            long_placeholder, 0,
            "CAS rows must not keep a second full body in placeholder_text"
        );

        let expected = format!("payload-00-{stem}");
        let stored_json: String = conn
            .query(
                "SELECT observation_json FROM observations WHERE observation_id = 'obs.0'",
                (),
            )
            .await
            .unwrap()
            .next()
            .await
            .unwrap()
            .unwrap()
            .get(0)
            .unwrap();
        let hash =
            serde_json::from_str::<serde_json::Value>(&stored_json).unwrap()[BODY_REF_KEY]["/text"]
                .as_str()
                .unwrap()
                .to_owned();
        let mut body_rows = conn
            .query(LOAD_CANONICAL_BODY_SQL, params![hash.as_str()])
            .await
            .unwrap();
        let body_row = body_rows.next().await.unwrap().unwrap();
        let encoding: String = body_row.get(0).unwrap();
        let blob: Vec<u8> = body_row.get(1).unwrap();
        assert_eq!(
            unpack_body(&hash, &encoding, &blob).unwrap(),
            expected.as_bytes()
        );
        println!(
            "session-canonical-bodies migration N={N} before_family_bytes={before_bytes} after_family_bytes={after_bytes} before_payload_bytes={before_logical} after_payload_bytes={after_logical} unique_payload_bytes={unique_payload_bytes} stored_body_bytes={stored_body_bytes} elapsed_ms={}",
            elapsed.as_millis()
        );
    }
}
