use tracedecay_runtime_core::db::engine::{Executor, QueryExecutor, params};
use tracedecay_store::{
    BODY_REF_KEY, CANONICAL_BODIES_TABLE_SQL, CanonicalBodyError, INLINE_BODY_BYTES,
    LOAD_CANONICAL_BODY_SQL, StoredCanonicalBody, UPSERT_CANONICAL_BODY_SQL, collect_body_refs,
    parse_stored_observation, slim_stored_json, unpack_body,
};

use super::super::global_db_operation_error;
use super::codec::storage;

pub(super) const CANONICAL_BODY_MIGRATION: &str = "session-canonical-bodies-v1";
const LCM_CANONICAL_BODY_MIGRATION: &str = "session-canonical-bodies-lcm-v1";
const LCM_PLACEHOLDER_SNIPPET_MIGRATION: &str = "session-canonical-bodies-lcm-snippet-v1";
const OPERATION: &str = "compact session canonical bodies";
const COMPACT_PAGE: i64 = 64;
const INLINE_BODY_BYTES_SQL: i64 = INLINE_BODY_BYTES as i64;
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

pub(crate) async fn compact_observation_bodies(
    conn: &impl Executor,
) -> tracedecay_domain::errors::Result<bool> {
    ensure_canonical_bodies_table(conn).await?;
    if migration_recorded(conn, CANONICAL_BODY_MIGRATION).await? {
        return Ok(false);
    }
    conn.execute_batch(LIFT_OBSERVATION_IMMUTABILITY)
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    let mut rewrote = false;
    loop {
        let mut rows = conn
            .query(
                "SELECT observation_id, observation_json FROM observations
                 WHERE length(observation_json) >= ?1
                   AND instr(observation_json, ?2) = 0
                 LIMIT ?3",
                params![INLINE_BODY_BYTES_SQL, BODY_REF_KEY, COMPACT_PAGE],
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
            break;
        }
        for (observation_id, observation_json) in page {
            let (slim, bodies) = slim_stored_json(&observation_json)
                .map_err(|error| global_db_operation_error(OPERATION, error))?;
            persist_bodies(conn, &bodies).await?;
            if slim != observation_json {
                conn.execute(
                    "UPDATE observations SET observation_json = ?1 WHERE observation_id = ?2",
                    params![slim, observation_id],
                )
                .await
                .map_err(|error| global_db_operation_error(OPERATION, error))?;
                rewrote = true;
            }
        }
    }
    conn.execute_batch(RESTORE_OBSERVATION_IMMUTABILITY)
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    conn.execute(
        "INSERT OR IGNORE INTO global_schema_migrations(migration) VALUES (?1)",
        params![CANONICAL_BODY_MIGRATION],
    )
    .await
    .map_err(|error| global_db_operation_error(OPERATION, error))?;
    Ok(rewrote)
}

pub(crate) async fn compact_attached_lcm_bodies(
    conn: &impl Executor,
) -> tracedecay_domain::errors::Result<bool> {
    let mut rewrote = false;
    if !migration_recorded(conn, LCM_CANONICAL_BODY_MIGRATION).await? {
        rewrote |= compact_lcm_bodies(conn).await?;
        conn.execute(
            "INSERT OR IGNORE INTO global_schema_migrations(migration) VALUES (?1)",
            params![LCM_CANONICAL_BODY_MIGRATION],
        )
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    }
    if !migration_recorded(conn, LCM_PLACEHOLDER_SNIPPET_MIGRATION).await? {
        rewrote |= compact_lcm_placeholders(conn).await?;
        conn.execute(
            "INSERT OR IGNORE INTO global_schema_migrations(migration) VALUES (?1)",
            params![LCM_PLACEHOLDER_SNIPPET_MIGRATION],
        )
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    }
    Ok(rewrote)
}

pub(crate) async fn compact_lcm_bodies(
    conn: &impl Executor,
) -> tracedecay_domain::errors::Result<bool> {
    ensure_canonical_bodies_table(conn).await?;
    if !table_exists(conn, "lcm_raw_messages").await? {
        return Ok(false);
    }
    let mut rewrote = false;
    loop {
        let mut rows = conn
            .query(
                "SELECT store_id, content FROM lcm_raw_messages
                 WHERE content IS NOT NULL AND length(content) >= ?1
                 LIMIT ?2",
                params![INLINE_BODY_BYTES_SQL, COMPACT_PAGE],
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
                row.get::<i64>(0)
                    .map_err(|error| global_db_operation_error(OPERATION, error))?,
                row.get::<String>(1)
                    .map_err(|error| global_db_operation_error(OPERATION, error))?,
            ));
        }
        drop(rows);
        if page.is_empty() {
            break;
        }
        for (store_id, content) in page {
            let body = StoredCanonicalBody::pack(content.as_bytes())
                .map_err(|error| global_db_operation_error(OPERATION, error))?;
            persist_bodies(conn, std::slice::from_ref(&body)).await?;
            let placeholder = tracedecay_lcm::retrieval_content::derived_text_for_snippet(&content);
            conn.execute(
                "UPDATE lcm_raw_messages
                 SET content = NULL, placeholder_text = ?1
                 WHERE store_id = ?2",
                params![placeholder, store_id],
            )
            .await
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
            rewrote = true;
        }
    }
    Ok(rewrote || compact_lcm_placeholders(conn).await?)
}

const SNIPPET_CAP_SQL: i64 = 4096;

pub(crate) async fn compact_lcm_placeholders(
    conn: &impl Executor,
) -> tracedecay_domain::errors::Result<bool> {
    if !table_exists(conn, "lcm_raw_messages").await? {
        return Ok(false);
    }
    let mut rewrote = false;
    loop {
        let mut rows = conn
            .query(
                "SELECT store_id, placeholder_text FROM lcm_raw_messages
                 WHERE placeholder_text IS NOT NULL AND length(placeholder_text) > ?1
                 LIMIT ?2",
                params![SNIPPET_CAP_SQL, COMPACT_PAGE],
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
                row.get::<i64>(0)
                    .map_err(|error| global_db_operation_error(OPERATION, error))?,
                row.get::<String>(1)
                    .map_err(|error| global_db_operation_error(OPERATION, error))?,
            ));
        }
        drop(rows);
        if page.is_empty() {
            break;
        }
        for (store_id, placeholder) in page {
            let snippet = tracedecay_lcm::retrieval_content::derived_text_for_snippet(&placeholder);
            conn.execute(
                "UPDATE lcm_raw_messages SET placeholder_text = ?1 WHERE store_id = ?2",
                params![snippet, store_id],
            )
            .await
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
            rewrote = true;
        }
    }
    Ok(rewrote)
}

pub(crate) async fn decode_observation_json(
    conn: &impl QueryExecutor,
    observation_json: &str,
    operation: &'static str,
) -> tracedecay_store::ObservationStoreResult<tracedecay_domain::DurableObservationV1> {
    let hashes = collect_body_refs(observation_json).map_err(|error| storage(operation, error))?;
    let mut bodies = std::collections::HashMap::new();
    for hash in hashes {
        bodies.insert(
            hash.clone(),
            load_canonical_body(conn, &hash)
                .await
                .map_err(|error| storage(operation, error))?,
        );
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
) -> Result<Vec<u8>, CanonicalBodyError> {
    let mut rows = conn
        .query(LOAD_CANONICAL_BODY_SQL, params![content_hash])
        .await
        .map_err(|_| CanonicalBodyError::Missing {
            content_hash: content_hash.to_owned(),
        })?;
    let row = rows
        .next()
        .await
        .map_err(|_| CanonicalBodyError::Missing {
            content_hash: content_hash.to_owned(),
        })?
        .ok_or_else(|| CanonicalBodyError::Missing {
            content_hash: content_hash.to_owned(),
        })?;
    let encoding: String = row.get(0).map_err(|_| CanonicalBodyError::Missing {
        content_hash: content_hash.to_owned(),
    })?;
    let blob: Vec<u8> = row.get(1).map_err(|_| CanonicalBodyError::Missing {
        content_hash: content_hash.to_owned(),
    })?;
    unpack_body(content_hash, &encoding, &blob)
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
    use tracedecay_store::unpack_body;

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
                params![INLINE_BODY_BYTES_SQL],
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
                params![SNIPPET_CAP_SQL],
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
        let hash = serde_json::from_str::<serde_json::Value>(&stored_json).unwrap()["text"]
            [BODY_REF_KEY]
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
