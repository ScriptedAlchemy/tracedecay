use tracedecay_runtime_core::db::engine::{Executor, QueryExecutor, params};
use tracedecay_store::canonical_body::LOAD_CANONICAL_BODIES_SQL;
use tracedecay_store::{
    CANONICAL_BODIES_TABLE_SQL, CanonicalBodyError, INLINE_BODY_BYTES, StoredCanonicalBody,
    UPSERT_CANONICAL_BODY_SQL, collect_body_refs, parse_stored_observation, unpack_body,
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
    if !hashes.is_empty() {
        const READ_OPERATION: &str = "read canonical observation body";
        let encoded = serde_json::to_string(&hashes).map_err(|error| storage(operation, error))?;
        let mut rows = conn
            .query(LOAD_CANONICAL_BODIES_SQL, params![encoded])
            .await
            .map_err(|error| storage(READ_OPERATION, error))?;
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| storage(READ_OPERATION, error))?
        {
            let hash: String = row.get(0).map_err(|error| storage(READ_OPERATION, error))?;
            let encoding = row
                .get::<Option<String>>(1)
                .map_err(|error| storage(READ_OPERATION, error))?
                .ok_or_else(|| {
                    storage(
                        READ_OPERATION,
                        CanonicalBodyError::Missing {
                            content_hash: hash.clone(),
                        },
                    )
                })?;
            let blob: Vec<u8> = row.get(2).map_err(|error| storage(READ_OPERATION, error))?;
            let uncompressed = row.get(3).map_err(|error| storage(READ_OPERATION, error))?;
            let bytes = unpack_body(&hash, &encoding, &blob, uncompressed)
                .map_err(|error| storage(READ_OPERATION, error))?;
            bodies.insert(hash, bytes);
        }
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
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tracedecay_domain::{
        ComponentVersion, DurableObservationV1, ObservationId, ObservationIdentityMaterialV1,
        ObservationOrderingDomainV1, ObservationScopeV1, ObservationSourceGenerationV1,
        ObservationSourceIdentityV1, ObservationSourceRangeV1, PayloadReferenceV1, RetentionClass,
        SanitizationReceiptId, SanitizationReceiptRefV1, SanitizationReceiptV1,
        SanitizerDispositionV1, SensitivityV1, SessionId,
    };
    use tracedecay_runtime_core::db::engine::{IntoParams, ReadConnection, Rows};
    use tracedecay_store::slim_stored_json;

    use super::*;
    use tempfile::TempDir;
    use tracedecay_runtime_core::db::engine::TestConnection;

    #[tokio::test]
    async fn body_reads_distinguish_storage_failures_from_missing_rows() {
        let tmp = TempDir::new().unwrap();
        let conn = TestConnection::open(&tmp.path().join("body-read-errors.db"));
        let encoded = serde_json::json!({
            (tracedecay_store::canonical_body::BODY_REF_KEY): {"/payload/text": "missing"}
        })
        .to_string();
        let error = decode_observation_json(&conn, &encoded, "hydrate fixture")
            .await
            .unwrap_err();
        let tracedecay_store::ObservationStoreError::Storage { source, .. } = error else {
            panic!("expected a storage error");
        };
        assert!(source.is::<tracedecay_runtime_core::db::engine::Error>());
        ensure_canonical_bodies_table(&conn).await.unwrap();
        let error = decode_observation_json(&conn, &encoded, "hydrate fixture")
            .await
            .unwrap_err();
        let tracedecay_store::ObservationStoreError::Storage { source, .. } = error else {
            panic!("expected a missing-body error");
        };
        assert!(
            matches!(source.downcast_ref::<CanonicalBodyError>(), Some(CanonicalBodyError::Missing { content_hash }) if content_hash == "missing")
        );
    }

    #[tokio::test]
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

    fn body_observation(payload: serde_json::Value) -> DurableObservationV1 {
        let identity = ObservationIdentityMaterialV1::for_native_record(
            ObservationSourceIdentityV1::new(SessionId::new("session.bodies").unwrap()).unwrap(),
            ObservationScopeV1::Profile,
            ObservationSourceGenerationV1::new(1).unwrap(),
            ObservationSourceRangeV1::new(12, 34).unwrap(),
            ObservationOrderingDomainV1::FileBytes,
            ObservationId::new("record.bodies").unwrap(),
        )
        .unwrap();
        let receipt = SanitizationReceiptV1::new(
            SanitizationReceiptRefV1::new(
                SanitizationReceiptId::new("receipt.bodies").unwrap(),
                ComponentVersion::new("sanitizer.fixture.v1").unwrap(),
            )
            .unwrap(),
            SanitizerDispositionV1::Accepted,
            SensitivityV1::NonSensitive,
            Some(PayloadReferenceV1::for_payload(&payload).unwrap()),
        )
        .unwrap();
        DurableObservationV1::new(
            identity,
            receipt,
            RetentionClass::new("transcript.fixture").unwrap(),
            payload,
        )
        .unwrap()
    }

    struct CountedRead {
        connection: ReadConnection,
        queries: AtomicUsize,
    }

    impl QueryExecutor for CountedRead {
        async fn query<P>(
            &self,
            sql: &str,
            params: P,
        ) -> tracedecay_runtime_core::db::engine::Result<Rows>
        where
            P: IntoParams,
        {
            self.queries.fetch_add(1, Ordering::Relaxed);
            self.connection.query(sql, params).await
        }
    }

    #[tokio::test]
    async fn observation_body_hydration_batches_reads_and_preserves_verification() {
        let tmp = TempDir::new().unwrap();
        let conn = TestConnection::open(&tmp.path().join("observation-bodies.db"));
        ensure_canonical_bodies_table(&conn).await.unwrap();
        let observation = body_observation(serde_json::json!({
            "first": "alpha ".repeat(INLINE_BODY_BYTES),
            "second": "beta ".repeat(INLINE_BODY_BYTES),
            "same_as_first": "alpha ".repeat(INLINE_BODY_BYTES),
        }));
        let expected = serde_json::to_value(&observation).unwrap();
        let (encoded, bodies) = slim_stored_json(&expected.to_string()).unwrap();
        persist_bodies(&conn, &bodies).await.unwrap();
        let hashes = collect_body_refs(&encoded).unwrap();
        assert_eq!(hashes.len(), 2, "repeated payloads share one body");
        // An unrelated corrupt row must not be read by the bounded lookup.
        conn.execute(
            UPSERT_CANONICAL_BODY_SQL,
            params!["unrelated", "identity", b"wrong".as_slice(), 5_i64],
        )
        .await
        .unwrap();
        let counted = CountedRead {
            connection: conn.read_only(),
            queries: AtomicUsize::new(0),
        };
        let decoded = decode_observation_json(&counted, &encoded, "hydrate fixture")
            .await
            .unwrap();
        assert_eq!(serde_json::to_value(decoded).unwrap(), expected);
        assert_eq!(counted.queries.load(Ordering::Relaxed), 1);

        let inline = body_observation(serde_json::json!({"text": "small"}));
        let encoded_inline = serde_json::to_string(&inline).unwrap();
        let decoded = decode_observation_json(&counted, &encoded_inline, "hydrate fixture")
            .await
            .unwrap();
        assert_eq!(
            serde_json::to_value(decoded).unwrap(),
            serde_json::to_value(inline).unwrap()
        );
        assert_eq!(
            counted.queries.load(Ordering::Relaxed),
            1,
            "inline rows issue no body query"
        );

        conn.execute(
            "DELETE FROM session_canonical_bodies WHERE content_hash = ?1",
            params![hashes[0].as_str()],
        )
        .await
        .unwrap();
        let error = decode_observation_json(&counted, &encoded, "hydrate fixture")
            .await
            .unwrap_err();
        let tracedecay_store::ObservationStoreError::Storage { source, .. } = error else {
            panic!("expected missing body refusal");
        };
        assert!(matches!(
            source.downcast_ref::<CanonicalBodyError>(),
            Some(CanonicalBodyError::Missing { content_hash }) if content_hash == &hashes[0]
        ));
        assert_eq!(counted.queries.load(Ordering::Relaxed), 2);

        persist_bodies(&conn, &bodies).await.unwrap();
        conn.execute(
            "UPDATE session_canonical_bodies SET uncompressed_bytes = uncompressed_bytes + 1
             WHERE content_hash = ?1",
            params![hashes[1].as_str()],
        )
        .await
        .unwrap();
        let error = decode_observation_json(&counted, &encoded, "hydrate fixture")
            .await
            .unwrap_err();
        let tracedecay_store::ObservationStoreError::Storage { source, .. } = error else {
            panic!("expected corrupt body refusal");
        };
        assert!(matches!(
            source.downcast_ref::<CanonicalBodyError>(),
            Some(CanonicalBodyError::Corrupt { content_hash, .. }) if content_hash == &hashes[1]
        ));
        assert_eq!(counted.queries.load(Ordering::Relaxed), 3);
    }
}
