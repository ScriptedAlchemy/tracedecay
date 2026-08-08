//! Registered daily rollup authority for bounded local observability fragments.
//!
//! Canonical events remain the fact authority. This module owns only the
//! rebuildable, daily projection: publication is atomic, correction is
//! watermark-monotone, and reads never fall back to scanning unbounded raw
//! history. The query boundary applies local minimum-support suppression
//! before any cell payload or exact population count leaves storage.

use serde::Serialize;
use sha2::{Digest, Sha256};
use tracedecay_domain::CoverageStateV1;
use tracedecay_runtime_core::db::engine::{Executor, QueryExecutor};

use crate::RegisteredGlobalDb;
mod compaction;
mod dirty;
mod frontier;
mod retention;
mod schema;
mod types;

pub use schema::ensure_observability_rollup_schema;
pub use types::*;

use dirty::{dirty_claim_is_current, range_has_dirty_day, validate_dirty_claim};
use frontier::{empty_day_claim_is_current, settle_empty_day_claim, validate_empty_day_claim};
use types::{
    MAX_FRAGMENT_JSON_BYTES, MAX_FRAGMENT_QUERY_BYTES, OBSERVABILITY_ROLLUP_RETENTION_DAYS_V1,
    PublishedGeneration, SECONDS_PER_DAY, merge_coverage, validate_day, validate_identifier,
};

impl RegisteredGlobalDb {
    pub async fn rebuild_observability_rollup(
        &self,
        request: ObservabilityRollupRebuildV1,
    ) -> Result<ObservabilityRollupRebuildReceiptV1, String> {
        validate_rebuild(&request)?;
        let content_digest = digest_json(&(
            "execution-topology-rollup-fragment.v1",
            request.fragment_json.as_str(),
        ))?;
        let request_digest = digest_json(&(
            request.authorized_scope_ref.as_str(),
            request.day_start_seconds,
            request.projector_revision.as_str(),
            request.source_watermark,
            coverage_name(request.coverage),
            content_digest.as_str(),
        ))?;
        let transaction = self
            .begin_write_transaction()
            .await
            .map_err(|error| format!("failed to begin observability rollup rebuild: {error}"))?;

        if let Some((stored_digest, receipt)) = read_journal_receipt(
            &transaction,
            &request.authorized_scope_ref,
            request.day_start_seconds,
            &request.idempotency_key,
        )
        .await?
        {
            transaction
                .rollback()
                .await
                .map_err(|error| format!("failed to close rollup replay transaction: {error}"))?;
            if stored_digest == request_digest {
                return Ok(receipt);
            }
            return Err("observability rollup idempotency conflict".to_owned());
        }

        if let Some(claim) = &request.dirty_claim
            && !dirty_claim_is_current(&transaction, claim).await?
        {
            transaction.rollback().await.map_err(|error| {
                format!("failed to close superseded dirty-day rebuild: {error}")
            })?;
            return Err("observability rollup dirty-day claim was superseded".to_owned());
        }
        if let Some(claim) = &request.empty_day_claim
            && !empty_day_claim_is_current(&transaction, claim).await?
        {
            transaction.rollback().await.map_err(|error| {
                format!("failed to close superseded empty-day rebuild: {error}")
            })?;
            return Err("observability rollup empty-day claim was superseded".to_owned());
        }

        let current = read_published_generation(
            &transaction,
            &request.authorized_scope_ref,
            request.day_start_seconds,
        )
        .await?;
        if current
            .as_ref()
            .is_some_and(|value| request.source_watermark < value.source_watermark)
        {
            transaction
                .rollback()
                .await
                .map_err(|error| format!("failed to close stale rollup transaction: {error}"))?;
            return Err("observability rollup stale source watermark".to_owned());
        }
        if let Some(current) = &current
            && request.source_watermark == current.source_watermark
            && request.projector_revision == current.projector_revision
            && (request.coverage != current.coverage || content_digest != current.content_digest)
        {
            transaction.rollback().await.map_err(|error| {
                format!("failed to close conflicting rollup transaction: {error}")
            })?;
            return Err("observability rollup changed at the same projector watermark".to_owned());
        }

        let unchanged = current.as_ref().is_some_and(|value| {
            request.source_watermark == value.source_watermark
                && request.projector_revision == value.projector_revision
                && request.coverage == value.coverage
                && content_digest == value.content_digest
        });
        let generation = current.as_ref().map_or(1, |value| {
            value.generation.saturating_add(u64::from(!unchanged))
        });
        let late_correction = current
            .as_ref()
            .is_some_and(|value| request.source_watermark > value.source_watermark);

        if !unchanged {
            replace_published_fragment(&transaction, &request, generation, &content_digest).await?;
        }
        let receipt = ObservabilityRollupRebuildReceiptV1 {
            authorized_scope_ref: request.authorized_scope_ref.clone(),
            day_start_seconds: request.day_start_seconds,
            generation,
            projector_revision: request.projector_revision.clone(),
            source_watermark: request.source_watermark,
            coverage: request.coverage,
            content_digest,
            late_correction,
        };
        insert_journal_receipt(
            &transaction,
            &request.idempotency_key,
            &request_digest,
            &receipt,
        )
        .await?;
        if let Some(claim) = &request.dirty_claim {
            let changed = transaction
                .execute(
                    "DELETE FROM observability_rollup_dirty_days
                     WHERE scope_ref = ?1 AND day_start_seconds = ?2
                       AND source_watermark = ?3 AND claimant_id = ?4
                       AND lease_until_seconds = ?5",
                    tracedecay_runtime_core::db::engine::params![
                        claim.authorized_scope_ref.as_str(),
                        claim.day_start_seconds,
                        claim.source_watermark,
                        claim.claimant_id.as_str(),
                        claim.lease_until_seconds
                    ],
                )
                .await
                .map_err(|error| format!("failed to settle observability dirty day: {error}"))?;
            if changed != 1 {
                transaction.rollback().await.map_err(|error| {
                    format!("failed to close unsettled dirty-day rebuild: {error}")
                })?;
                return Err(
                    "observability rollup dirty-day claim changed during rebuild".to_owned(),
                );
            }
        }
        if let Some(claim) = &request.empty_day_claim {
            if !settle_empty_day_claim(&transaction, claim).await? {
                transaction.rollback().await.map_err(|error| {
                    format!("failed to close unadvanced empty-day rebuild: {error}")
                })?;
                return Err(
                    "observability rollup empty-day claim changed during rebuild".to_owned(),
                );
            }
        }
        transaction
            .commit()
            .await
            .map_err(|error| format!("failed to publish observability rollup: {error}"))?;
        Ok(receipt)
    }

    /// Reads only bounded, projector-owned merge fragments. This internal
    /// authority deliberately does not apply per-day minimum support: privacy
    /// suppression is evaluated on the final merged horizon cell, otherwise
    /// several small daily cohorts could never safely become one supported
    /// local result.
    pub async fn query_observability_rollup_fragments(
        &self,
        query: &ObservabilityRollupFragmentQueryV1,
    ) -> Result<ObservabilityRollupFragmentPageV1, String> {
        validate_fragment_query(query)?;
        let snapshot = self
            .read_snapshot()
            .await
            .map_err(|error| format!("failed to begin observability fragment snapshot: {error}"))?;
        if range_has_dirty_day(
            &snapshot,
            &query.authorized_scope_ref,
            query.since_day_start_seconds,
            query.until_day_start_seconds,
        )
        .await?
        {
            return Ok(ObservabilityRollupFragmentPageV1 {
                fragments: Vec::new(),
                coverage: CoverageStateV1::Stale,
            });
        }
        let mut rows = snapshot
            .query(
                "SELECT scope_ref, day_start_seconds, generation, projector_revision,
                        source_watermark, content_digest, fragment_json, coverage
                 FROM observability_rollup_generations
                 WHERE scope_ref = ?1 AND day_start_seconds >= ?2 AND day_start_seconds < ?3
                 ORDER BY day_start_seconds",
                tracedecay_runtime_core::db::engine::params![
                    query.authorized_scope_ref.as_str(),
                    query.since_day_start_seconds,
                    query.until_day_start_seconds
                ],
            )
            .await
            .map_err(|error| format!("failed to query observability fragments: {error}"))?;
        let mut fragments = Vec::new();
        let mut day_starts = Vec::new();
        let mut bytes = 0usize;
        let mut coverage = CoverageStateV1::Known;
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| format!("failed to read observability fragment: {error}"))?
        {
            let fragment_coverage = parse_coverage(
                &row.get::<String>(7)
                    .map_err(|error| format!("failed to decode fragment coverage: {error}"))?,
            )?;
            coverage = merge_coverage(coverage, fragment_coverage);
            let day_start_seconds = row
                .get(1)
                .map_err(|error| format!("failed to decode fragment day: {error}"))?;
            day_starts.push(day_start_seconds);
            let fragment_json: String = row
                .get(6)
                .map_err(|error| format!("failed to decode rollup fragment: {error}"))?;
            bytes = bytes.saturating_add(fragment_json.len());
            if bytes > MAX_FRAGMENT_QUERY_BYTES {
                return Err("observability rollup fragment query exceeds 32 MiB".to_owned());
            }
            fragments.push(ObservabilityRollupFragmentRecordV1 {
                authorized_scope_ref: row
                    .get(0)
                    .map_err(|error| format!("failed to decode fragment scope: {error}"))?,
                day_start_seconds,
                generation: decode_u64(&row, 2, "fragment generation")?,
                projector_revision: row
                    .get(3)
                    .map_err(|error| format!("failed to decode fragment projector: {error}"))?,
                source_watermark: row
                    .get(4)
                    .map_err(|error| format!("failed to decode fragment watermark: {error}"))?,
                content_digest: row
                    .get(5)
                    .map_err(|error| format!("failed to decode fragment digest: {error}"))?,
                fragment_json,
            });
        }
        let expected_days = usize::try_from(
            (query.until_day_start_seconds - query.since_day_start_seconds) / SECONDS_PER_DAY,
        )
        .map_err(|_| "observability fragment horizon exceeds platform range".to_owned())?;
        let contiguous = day_starts.len() == expected_days
            && day_starts
                .iter()
                .enumerate()
                .all(|(index, day_start_seconds)| {
                    i64::try_from(index).ok().is_some_and(|index| {
                        *day_start_seconds
                            == query
                                .since_day_start_seconds
                                .saturating_add(index.saturating_mul(SECONDS_PER_DAY))
                    })
                });
        if !contiguous {
            coverage = merge_coverage(coverage, CoverageStateV1::Partial);
            fragments.clear();
        }
        Ok(ObservabilityRollupFragmentPageV1 {
            fragments,
            coverage,
        })
    }
}

fn validate_rebuild(request: &ObservabilityRollupRebuildV1) -> Result<(), String> {
    validate_identifier("scope", &request.authorized_scope_ref)?;
    validate_identifier("projector revision", &request.projector_revision)?;
    validate_identifier("idempotency key", &request.idempotency_key)?;
    validate_day(request.day_start_seconds)?;
    if request.source_watermark < 0 {
        return Err("observability rollup source watermark must be nonnegative".to_owned());
    }
    if let Some(claim) = &request.dirty_claim {
        validate_dirty_claim(claim)?;
        if claim.authorized_scope_ref != request.authorized_scope_ref
            || claim.day_start_seconds != request.day_start_seconds
            || claim.source_watermark != request.source_watermark
        {
            return Err("observability rollup dirty claim does not match rebuild".to_owned());
        }
    }
    if let Some(claim) = &request.empty_day_claim {
        validate_empty_day_claim(claim)?;
        if claim.authorized_scope_ref != request.authorized_scope_ref
            || claim.day_start_seconds != request.day_start_seconds
            || request.source_watermark != 0
        {
            return Err("observability rollup empty-day claim does not match rebuild".to_owned());
        }
    }
    if request.dirty_claim.is_some() && request.empty_day_claim.is_some() {
        return Err("observability rollup rebuild cannot hold two day claims".to_owned());
    }
    validate_fragment_json(&request.fragment_json)?;
    Ok(())
}

fn validate_fragment_query(query: &ObservabilityRollupFragmentQueryV1) -> Result<(), String> {
    validate_identifier("scope", &query.authorized_scope_ref)?;
    validate_day(query.since_day_start_seconds)?;
    validate_day(query.until_day_start_seconds)?;
    let days = query
        .until_day_start_seconds
        .checked_sub(query.since_day_start_seconds)
        .filter(|span| *span > 0)
        .map(|span| span / SECONDS_PER_DAY)
        .ok_or_else(|| "observability fragment query horizon is invalid".to_owned())?;
    if days > OBSERVABILITY_ROLLUP_RETENTION_DAYS_V1 {
        return Err("observability fragment query exceeds 395 daily buckets".to_owned());
    }
    Ok(())
}

fn validate_fragment_json(fragment_json: &str) -> Result<(), String> {
    if fragment_json.len() > MAX_FRAGMENT_JSON_BYTES {
        return Err("observability rollup fragment exceeds 4 MiB".to_owned());
    }
    let value: serde_json::Value = serde_json::from_str(fragment_json)
        .map_err(|_| "observability rollup fragment is not valid JSON".to_owned())?;
    if !value.is_object() {
        return Err("observability rollup fragment must be a JSON object".to_owned());
    }
    if serde_json::to_string(&value)
        .map_err(|error| format!("failed to canonicalize rollup fragment: {error}"))?
        != fragment_json
    {
        return Err("observability rollup fragment must use canonical JSON".to_owned());
    }
    Ok(())
}

fn digest_json(value: &impl Serialize) -> Result<String, String> {
    let encoded = serde_json::to_vec(value)
        .map_err(|error| format!("failed to serialize observability rollup digest: {error}"))?;
    Ok(hex::encode(Sha256::digest(encoded)))
}

async fn read_published_generation(
    executor: &impl QueryExecutor,
    scope_ref: &str,
    day_start_seconds: i64,
) -> Result<Option<PublishedGeneration>, String> {
    let mut rows = executor
        .query(
            "SELECT generation, projector_revision, source_watermark, coverage, content_digest
             FROM observability_rollup_generations
             WHERE scope_ref = ?1 AND day_start_seconds = ?2",
            tracedecay_runtime_core::db::engine::params![scope_ref, day_start_seconds],
        )
        .await
        .map_err(|error| format!("failed to inspect observability rollup generation: {error}"))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| format!("failed to read observability rollup generation: {error}"))?
    else {
        return Ok(None);
    };
    Ok(Some(PublishedGeneration {
        generation: decode_u64(&row, 0, "generation")?,
        projector_revision: row
            .get(1)
            .map_err(|error| format!("failed to decode rollup projector revision: {error}"))?,
        source_watermark: row
            .get(2)
            .map_err(|error| format!("failed to decode rollup source watermark: {error}"))?,
        coverage: parse_coverage(
            &row.get::<String>(3)
                .map_err(|error| format!("failed to decode rollup generation coverage: {error}"))?,
        )?,
        content_digest: row
            .get(4)
            .map_err(|error| format!("failed to decode rollup content digest: {error}"))?,
    }))
}

async fn read_journal_receipt(
    executor: &impl QueryExecutor,
    scope_ref: &str,
    day_start_seconds: i64,
    idempotency_key: &str,
) -> Result<Option<(String, ObservabilityRollupRebuildReceiptV1)>, String> {
    let mut rows = executor
        .query(
            "SELECT request_digest, generation, projector_revision, source_watermark,
                    coverage, content_digest, late_correction
             FROM observability_rollup_rebuild_journal
             WHERE scope_ref = ?1 AND day_start_seconds = ?2 AND idempotency_key = ?3",
            tracedecay_runtime_core::db::engine::params![
                scope_ref,
                day_start_seconds,
                idempotency_key
            ],
        )
        .await
        .map_err(|error| format!("failed to inspect observability rollup journal: {error}"))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| format!("failed to read observability rollup journal: {error}"))?
    else {
        return Ok(None);
    };
    let request_digest = row
        .get(0)
        .map_err(|error| format!("failed to decode rollup request digest: {error}"))?;
    Ok(Some((
        request_digest,
        ObservabilityRollupRebuildReceiptV1 {
            authorized_scope_ref: scope_ref.to_owned(),
            day_start_seconds,
            generation: decode_u64(&row, 1, "generation")?,
            projector_revision: row.get(2).map_err(|error| {
                format!("failed to decode rollup journal projector revision: {error}")
            })?,
            source_watermark: row.get(3).map_err(|error| {
                format!("failed to decode rollup journal source watermark: {error}")
            })?,
            coverage: parse_coverage(
                &row.get::<String>(4).map_err(|error| {
                    format!("failed to decode rollup journal coverage: {error}")
                })?,
            )?,
            content_digest: row.get(5).map_err(|error| {
                format!("failed to decode rollup journal content digest: {error}")
            })?,
            late_correction: row
                .get::<i64>(6)
                .map_err(|error| format!("failed to decode rollup correction state: {error}"))?
                != 0,
        },
    )))
}

async fn replace_published_fragment(
    executor: &impl Executor,
    request: &ObservabilityRollupRebuildV1,
    generation: u64,
    content_digest: &str,
) -> Result<(), String> {
    executor
        .execute(
            "INSERT INTO observability_rollup_generations
                 (scope_ref, day_start_seconds, generation, projector_revision,
                  source_watermark, coverage, content_digest, fragment_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(scope_ref, day_start_seconds) DO UPDATE SET
                 generation = excluded.generation,
                 projector_revision = excluded.projector_revision,
                 source_watermark = excluded.source_watermark,
                 coverage = excluded.coverage,
                 content_digest = excluded.content_digest,
                 fragment_json = excluded.fragment_json,
                 retention_checked_at_seconds = NULL,
                 published_at_seconds = unixepoch()",
            tracedecay_runtime_core::db::engine::params![
                request.authorized_scope_ref.as_str(),
                request.day_start_seconds,
                encode_u64(generation, "generation")?,
                request.projector_revision.as_str(),
                request.source_watermark,
                coverage_name(request.coverage),
                content_digest,
                request.fragment_json.as_str(),
            ],
        )
        .await
        .map_err(|error| format!("failed to publish observability rollup generation: {error}"))?;
    Ok(())
}

async fn insert_journal_receipt(
    executor: &impl Executor,
    idempotency_key: &str,
    request_digest: &str,
    receipt: &ObservabilityRollupRebuildReceiptV1,
) -> Result<(), String> {
    executor
        .execute(
            "INSERT INTO observability_rollup_rebuild_journal
                 (scope_ref, day_start_seconds, idempotency_key, request_digest,
                  generation, projector_revision, source_watermark, coverage,
                  content_digest, late_correction)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            tracedecay_runtime_core::db::engine::params![
                receipt.authorized_scope_ref.as_str(),
                receipt.day_start_seconds,
                idempotency_key,
                request_digest,
                encode_u64(receipt.generation, "generation")?,
                receipt.projector_revision.as_str(),
                receipt.source_watermark,
                coverage_name(receipt.coverage),
                receipt.content_digest.as_str(),
                i64::from(receipt.late_correction),
            ],
        )
        .await
        .map_err(|error| format!("failed to journal observability rollup rebuild: {error}"))?;
    Ok(())
}

fn encode_u64(value: u64, field: &str) -> Result<i64, String> {
    i64::try_from(value).map_err(|_| format!("observability rollup {field} exceeds storage range"))
}

fn decode_u64(
    row: &tracedecay_runtime_core::db::engine::Row,
    index: i32,
    field: &str,
) -> Result<u64, String> {
    let value = row
        .get::<i64>(index)
        .map_err(|error| format!("failed to decode rollup {field}: {error}"))?;
    decode_nonnegative(value, field)
}

fn decode_nonnegative(value: i64, field: &str) -> Result<u64, String> {
    u64::try_from(value).map_err(|_| format!("rollup {field} was negative"))
}

const fn coverage_name(coverage: CoverageStateV1) -> &'static str {
    match coverage {
        CoverageStateV1::Known => "known",
        CoverageStateV1::Partial => "partial",
        CoverageStateV1::Stale => "stale",
        CoverageStateV1::Unknown => "unknown",
        CoverageStateV1::Sampled => "sampled",
        CoverageStateV1::Capped => "capped",
    }
}

fn parse_coverage(value: &str) -> Result<CoverageStateV1, String> {
    match value {
        "known" => Ok(CoverageStateV1::Known),
        "partial" => Ok(CoverageStateV1::Partial),
        "stale" => Ok(CoverageStateV1::Stale),
        "unknown" => Ok(CoverageStateV1::Unknown),
        "sampled" => Ok(CoverageStateV1::Sampled),
        "capped" => Ok(CoverageStateV1::Capped),
        _ => Err("invalid observability rollup coverage".to_owned()),
    }
}
