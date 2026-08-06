use serde_json::{Value, json};
use tracedecay_domain::{HydrationStateV1, RetrievalAnchorId};
use tracedecay_runtime_core::db::engine::params;
use tracedecay_sessions::lcm::contracts::validate_payload_ref;

use super::{
    HydrationError, HydrationResolution, PayloadDescriptor, PayloadSource,
    TemporalExecutionSnapshot, TemporalSqlRead, nonnegative_usize, resolve_current,
};
use crate::session_temporal::operations::{
    CanonicalPublicationManifest, PreparedPayload, load_and_verify_receipt,
};

pub(super) async fn resolve_external_manifest(
    conn: &TemporalSqlRead<'_>,
    provider: &str,
    session_id: &str,
    message_id: &str,
    payload_ref: &str,
    content_hash: &str,
) -> Result<HydrationResolution, ()> {
    if validate_payload_ref(payload_ref).is_err() {
        return Ok(unverifiable());
    }

    let mut rows = conn
        .query(
            "SELECT external.content_hash, external.kind, external.byte_count, external.char_count,
                    external.metadata_json, external.created_at,
                    manifest.session_id, manifest.payload_digest, manifest.manifest_json,
                    manifest.receipt_id, manifest.created_at
             FROM lcm_external_payloads AS external
             LEFT JOIN session_external_payload_manifests AS manifest
               ON manifest.payload_ref = external.payload_ref
              AND manifest.session_id = external.session_id
             WHERE external.provider = ?1
               AND external.session_id = ?2
               AND external.message_id = ?3
               AND external.payload_ref = ?4
             LIMIT 2",
            params![provider, session_id, message_id, payload_ref],
        )
        .await
        .map_err(|_| ())?;
    let Some(row) = rows.next().await.map_err(|_| ())? else {
        return Ok(HydrationResolution::Unavailable(HydrationStateV1::Deleted));
    };

    let stored_hash: String = row.get(0).map_err(|_| ())?;
    let kind: String = row.get(1).map_err(|_| ())?;
    let byte_count = nonnegative_usize(row.get::<Option<i64>>(2).map_err(|_| ())?)?;
    let char_count = nonnegative_usize(row.get::<Option<i64>>(3).map_err(|_| ())?)?;
    let metadata: Option<String> = row.get(4).map_err(|_| ())?;
    let external_created_at: i64 = row.get(5).map_err(|_| ())?;
    let manifest_session: Option<String> = row.get(6).map_err(|_| ())?;
    let manifest_digest: Option<String> = row.get(7).map_err(|_| ())?;
    let manifest_json: Option<String> = row.get(8).map_err(|_| ())?;
    let receipt_id: Option<String> = row.get(9).map_err(|_| ())?;
    let manifest_created_at: Option<i64> = row.get(10).map_err(|_| ())?;
    if rows.next().await.map_err(|_| ())?.is_some() {
        return Ok(HydrationResolution::Unavailable(
            HydrationStateV1::RetainedButUnavailable,
        ));
    }
    let (
        Some(manifest_session),
        Some(manifest_digest),
        Some(manifest_json),
        Some(receipt_id),
        Some(manifest_created_at),
    ) = (
        manifest_session,
        manifest_digest,
        manifest_json,
        receipt_id,
        manifest_created_at,
    )
    else {
        return Ok(unverifiable());
    };
    let metadata_value = metadata.map_or(Value::Null, Value::String);
    let expected_manifest_json = json!({
        "provider": provider,
        "session_id": session_id,
        "message_id": message_id,
        "kind": kind,
        "byte_count": byte_count,
        "char_count": char_count,
        "metadata": metadata_value,
    })
    .to_string();
    if stored_hash != content_hash
        || manifest_session != session_id
        || manifest_digest != content_hash
        || manifest_json != expected_manifest_json
        || manifest_created_at != external_created_at
    {
        return Ok(unverifiable());
    }

    let mut publication_rows = conn
        .query(
            "SELECT summary.summary_id, summary.publication_json, summary.created_at
             FROM session_summary_nodes AS summary
             JOIN json_each(summary.publication_json, '$.payloads') AS payload
               ON json_extract(payload.value, '$.payload_ref') = ?1
              AND json_extract(payload.value, '$.digest') = ?2
              AND json_extract(payload.value, '$.manifest_json') = ?3
             WHERE summary.session_id = ?4
               AND json_extract(summary.publication_json, '$.receipt_id') = ?5
             ORDER BY summary.rowid
             LIMIT 2",
            params![
                payload_ref,
                manifest_digest.as_str(),
                manifest_json.as_str(),
                session_id,
                receipt_id.as_str()
            ],
        )
        .await
        .map_err(|_| ())?;
    let Some(publication_row) = publication_rows.next().await.map_err(|_| ())? else {
        return Ok(unverifiable());
    };
    let summary_id: String = publication_row.get(0).map_err(|_| ())?;
    let publication_json: String = publication_row.get(1).map_err(|_| ())?;
    let publication_created_at: i64 = publication_row.get(2).map_err(|_| ())?;
    if publication_rows.next().await.map_err(|_| ())?.is_some() {
        return Ok(unverifiable());
    }
    let publication: CanonicalPublicationManifest = match serde_json::from_str(&publication_json) {
        Ok(publication) => publication,
        Err(_) => return Ok(unverifiable()),
    };
    let expected_payload = PreparedPayload {
        payload_ref: payload_ref.to_string(),
        digest: manifest_digest,
        manifest_json,
    };
    if publication.provider != provider
        || publication.session_id != session_id
        || publication.receipt_id != receipt_id
        || publication
            .payloads
            .iter()
            .filter(|payload| payload.payload_ref == payload_ref)
            .count()
            != 1
        || !publication.payloads.contains(&expected_payload)
        || load_and_verify_receipt(conn, &summary_id, &publication, publication_created_at)
            .await
            .is_err()
    {
        return Ok(unverifiable());
    }

    Ok(HydrationResolution::Available(PayloadDescriptor {
        source: PayloadSource::External {
            provider: provider.to_string(),
            session_id: session_id.to_string(),
            payload_ref: payload_ref.to_string(),
            char_count,
        },
        byte_count,
        content_hash: stored_hash,
    }))
}

pub(in crate::session_temporal) async fn resolve_external_target(
    conn: &TemporalSqlRead<'_>,
    snapshot: &TemporalExecutionSnapshot,
    anchor_id: &RetrievalAnchorId,
    provider: &str,
    session_id: &str,
    payload_ref: &str,
) -> Result<HydrationResolution, HydrationError> {
    match resolve_current(conn, snapshot, anchor_id)
        .await
        .map_err(|_| HydrationError::Unavailable)?
    {
        HydrationResolution::Unavailable(state) => {
            return Ok(HydrationResolution::Unavailable(state));
        }
        HydrationResolution::Available(_) => {}
    }

    let generation =
        i64::try_from(snapshot.watermarks().generation).map_err(|_| HydrationError::Unavailable)?;
    let mut rows = conn
        .query(
            "SELECT raw.message_id, raw.content_hash, raw.storage_kind
             FROM lcm_raw_messages AS raw
             JOIN session_occurrences AS occurrence
               ON occurrence.session_id = raw.session_id
              AND occurrence.source_provider = raw.provider
              AND occurrence.generation = ?4
              AND occurrence.message_id = raw.message_id
              AND occurrence.retrieval_anchor_id = ?5
             WHERE raw.provider = ?1
               AND raw.session_id = ?2
               AND raw.payload_ref = ?3
             ORDER BY raw.store_id, occurrence.occurrence_id
             LIMIT 2",
            params![
                provider,
                session_id,
                payload_ref,
                generation,
                anchor_id.as_str()
            ],
        )
        .await
        .map_err(|_| HydrationError::Unavailable)?;
    let Some(row) = rows.next().await.map_err(|_| HydrationError::Unavailable)? else {
        return Ok(HydrationResolution::Unavailable(HydrationStateV1::Deleted));
    };
    let message_id: String = row.get(0).map_err(|_| HydrationError::Unavailable)?;
    let content_hash: String = row.get(1).map_err(|_| HydrationError::Unavailable)?;
    let storage_kind: String = row.get(2).map_err(|_| HydrationError::Unavailable)?;
    if rows
        .next()
        .await
        .map_err(|_| HydrationError::Unavailable)?
        .is_some()
        || storage_kind != "external"
    {
        return Ok(HydrationResolution::Unavailable(
            HydrationStateV1::RetainedButUnavailable,
        ));
    }
    resolve_external_manifest(
        conn,
        provider,
        session_id,
        &message_id,
        payload_ref,
        &content_hash,
    )
    .await
    .map_err(|_| HydrationError::Unavailable)
}

fn unverifiable() -> HydrationResolution {
    HydrationResolution::Unavailable(HydrationStateV1::UnverifiableLegacy)
}
