use tracedecay_runtime_core::db::engine::{Executor, QueryExecutor, params};
use tracedecay_store::{
    CANONICAL_BODIES_TABLE_SQL, CanonicalBodyError, INLINE_BODY_BYTES, LOAD_CANONICAL_BODY_SQL,
    StoredCanonicalBody, UPSERT_CANONICAL_BODY_SQL, collect_body_refs, parse_stored_observation,
    unpack_body,
};

use super::super::global_db_operation_error;
use super::codec::storage;

const OPERATION: &str = "compact session canonical bodies";
const COMPACT_PAGE: i64 = 64;

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
    unpack_body(
        content_hash,
        &encoding,
        &blob,
        row.get(2).map_err(|error| storage(OPERATION, error))?,
    )
    .map_err(|error| storage(OPERATION, error))
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

    #[asupersync::test]
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

    #[asupersync::test]
    async fn lcm_compaction_stores_large_bodies_once_and_bounds_placeholders() {
        const N: usize = 16;
        let stem = "m".repeat(80_000);
        let tmp = TempDir::new().unwrap();
        let conn = TestConnection::open(&tmp.path().join("user-sessions.db"));
        conn.execute_batch(
            "CREATE TABLE lcm_raw_messages (
                 store_id INTEGER PRIMARY KEY AUTOINCREMENT,
                 content TEXT,
                 placeholder_text TEXT
             );",
        )
        .await
        .unwrap();
        let mut unique_payload_bytes = 0i64;
        for i in 0..N {
            let text = format!("payload-{i:02}-{stem}");
            unique_payload_bytes += i64::try_from(text.len()).unwrap();
            conn.execute(
                "INSERT INTO lcm_raw_messages(content) VALUES (?1)",
                params![text],
            )
            .await
            .unwrap();
        }
        assert!(compact_lcm_bodies(&conn).await.unwrap());
        assert!(!compact_lcm_bodies(&conn).await.unwrap());

        let mut census = conn
            .query(
                "SELECT COUNT(*), COALESCE(SUM(uncompressed_bytes), 0)
                 FROM session_canonical_bodies",
                (),
            )
            .await
            .unwrap();
        let row = census.next().await.unwrap().unwrap();
        assert_eq!(row.get::<i64>(0).unwrap(), i64::try_from(N).unwrap());
        assert_eq!(row.get::<i64>(1).unwrap(), unique_payload_bytes);
        drop(census);
        let inline: i64 = conn
            .query(
                "SELECT COUNT(*) FROM lcm_raw_messages
                 WHERE content IS NOT NULL
                    OR length(COALESCE(placeholder_text, '')) > ?1",
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
            inline, 0,
            "CAS rows keep neither the body nor a full placeholder"
        );
    }
}
