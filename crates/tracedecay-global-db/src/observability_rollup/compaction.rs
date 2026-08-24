use crate::RegisteredGlobalDb;

use super::{
    ObservabilityRollupCompactionCandidateV1, ObservabilityRollupCompactionReceiptV1,
    ObservabilityRollupCompactionV1, SECONDS_PER_DAY, digest_json, encode_u64, validate_day,
    validate_fragment_json, validate_identifier,
};

const DETAIL_RETENTION_SECONDS: i64 = 30 * SECONDS_PER_DAY;

impl RegisteredGlobalDb {
    /// Returns at most one retained fragment whose protected correction carry
    /// has reached the detail-retention boundary. The application owns the
    /// opaque retention evaluation; storage owns only this bounded selection
    /// and CAS.
    pub async fn next_observability_rollup_compaction(
        &self,
        authorized_scope_ref: &str,
    ) -> Result<Option<ObservabilityRollupCompactionCandidateV1>, String> {
        validate_identifier("scope", authorized_scope_ref)?;
        let snapshot = self
            .read_snapshot()
            .await
            .map_err(|error| format!("failed to begin rollup compaction snapshot: {error}"))?;
        let mut rows = snapshot
            .query(
                "SELECT scope_ref, day_start_seconds, generation, projector_revision,
                        source_watermark, content_digest, fragment_json
                 FROM observability_rollup_generations
                 WHERE scope_ref = ?1
                   AND day_start_seconds + 86400 <= unixepoch() - ?2
                   AND retention_checked_at_seconds IS NULL
                 ORDER BY day_start_seconds
                 LIMIT 1",
                tracedecay_runtime_core::db::engine::params![
                    authorized_scope_ref,
                    DETAIL_RETENTION_SECONDS
                ],
            )
            .await
            .map_err(|error| format!("failed to query rollup compaction candidate: {error}"))?;
        let Some(row) = rows
            .next()
            .await
            .map_err(|error| format!("failed to read rollup compaction candidate: {error}"))?
        else {
            return Ok(None);
        };
        Ok(Some(ObservabilityRollupCompactionCandidateV1 {
            authorized_scope_ref: row
                .get(0)
                .map_err(|error| format!("failed to decode compaction scope: {error}"))?,
            day_start_seconds: row
                .get(1)
                .map_err(|error| format!("failed to decode compaction day: {error}"))?,
            generation: super::decode_u64(&row, 2, "compaction generation")?,
            projector_revision: row
                .get(3)
                .map_err(|error| format!("failed to decode compaction projector: {error}"))?,
            source_watermark: row
                .get(4)
                .map_err(|error| format!("failed to decode compaction watermark: {error}"))?,
            content_digest: row
                .get(5)
                .map_err(|error| format!("failed to decode compaction digest: {error}"))?,
            fragment_json: row
                .get(6)
                .map_err(|error| format!("failed to decode compaction fragment: {error}"))?,
        }))
    }

    /// CAS-publishes one application-evaluated opaque fragment and stamps the
    /// 30-day retention check. A concurrent correction or projector rebuild
    /// wins and makes this candidate stale without overwriting it.
    pub async fn compact_observability_rollup_fragment(
        &self,
        request: ObservabilityRollupCompactionV1,
    ) -> Result<ObservabilityRollupCompactionReceiptV1, String> {
        validate_compaction(&request)?;
        let changed_fragment = request.fragment_json != request.candidate.fragment_json;
        let generation =
            if changed_fragment {
                request.candidate.generation.checked_add(1).ok_or_else(|| {
                    "observability rollup compaction generation overflow".to_owned()
                })?
            } else {
                request.candidate.generation
            };
        let content_digest = if changed_fragment {
            digest_json(&(
                "execution-topology-retention-check.v1",
                request.candidate.content_digest.as_str(),
                request.fragment_json.as_str(),
            ))?
        } else {
            request.candidate.content_digest.clone()
        };
        let transaction = self
            .begin_write_transaction()
            .await
            .map_err(|error| format!("failed to begin rollup fragment compaction: {error}"))?;
        let changed = transaction
            .execute(
                "UPDATE observability_rollup_generations
                 SET generation = ?7, content_digest = ?8, fragment_json = ?9,
                     retention_checked_at_seconds = unixepoch(), published_at_seconds = unixepoch()
                 WHERE scope_ref = ?1 AND day_start_seconds = ?2
                   AND generation = ?3 AND projector_revision = ?4
                   AND source_watermark = ?5 AND content_digest = ?6
                   AND retention_checked_at_seconds IS NULL
                   AND day_start_seconds + 86400 <= unixepoch() - ?10",
                tracedecay_runtime_core::db::engine::params![
                    request.candidate.authorized_scope_ref.as_str(),
                    request.candidate.day_start_seconds,
                    encode_u64(request.candidate.generation, "compaction generation")?,
                    request.candidate.projector_revision.as_str(),
                    request.candidate.source_watermark,
                    request.candidate.content_digest.as_str(),
                    encode_u64(generation, "compacted generation")?,
                    content_digest,
                    request.fragment_json.as_str(),
                    DETAIL_RETENTION_SECONDS
                ],
            )
            .await
            .map_err(|error| format!("failed to CAS compacted rollup fragment: {error}"))?;
        if changed != 1 {
            transaction.rollback().await.map_err(|error| {
                format!("failed to close stale rollup compaction transaction: {error}")
            })?;
            return Err("observability rollup compaction candidate changed".to_owned());
        }
        transaction
            .commit()
            .await
            .map_err(|error| format!("failed to commit rollup fragment compaction: {error}"))?;
        Ok(ObservabilityRollupCompactionReceiptV1 {
            day_start_seconds: request.candidate.day_start_seconds,
            previous_generation: request.candidate.generation,
            generation,
            changed: changed_fragment,
        })
    }
}

fn validate_compaction(request: &ObservabilityRollupCompactionV1) -> Result<(), String> {
    validate_identifier("scope", &request.candidate.authorized_scope_ref)?;
    validate_identifier("projector revision", &request.candidate.projector_revision)?;
    validate_day(request.candidate.day_start_seconds)?;
    if request.candidate.generation == 0
        || request.candidate.source_watermark < 0
        || request.candidate.content_digest.len() != 64
    {
        return Err("invalid observability rollup compaction candidate".to_owned());
    }
    validate_fragment_json(&request.candidate.fragment_json)?;
    validate_fragment_json(&request.fragment_json)
}
