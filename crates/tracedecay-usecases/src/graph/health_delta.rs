//! Scoped health deltas: the observability cursor, the persisted watermark point, and the dimension-by-dimension comparison against it.
//!
//! The application primitive and MCP adapters call this one use case after
//! opening an admitted, generation-pinned graph reader.

use std::collections::BTreeMap;

use tracedecay_application::retrieval::{
    HealthDeltaCoverageV1, HealthDeltaCurrentnessV1, HealthDeltaPointV1, HealthDeltaResult,
    HealthDeltaScopeV1, HealthDimensionDeltaV1, HealthDimensionPointV1,
};
use tracedecay_application::{
    ObservabilityApplicationV1, ObservabilityHorizonV1, ObservabilityQueryV1,
};
use tracedecay_domain::{
    CoverageStateV1, HealthDimensionObservedV1, HealthSnapshotObservedV1, ManifestDigest,
    ObservabilityEnvelopeV1, ObservabilityPayloadV1, ObservabilityRetentionClassV1,
    ObservabilityTerminalResultV1, UtcMicros, canonical_sha256,
};

use tracedecay_global_db::RegisteredGlobalDb;
use tracedecay_runtime_core::errors::{Result, TraceDecayError};

use crate::observability::RegisteredObservabilityPortV1;

use super::health::{VerifiedHealthSnapshotV1, compute_verified_health_snapshot};
use super::queries::GraphQueryManager;

const HEALTH_DELTA_SCHEMA_VERSION: u32 = 1;
const HEALTH_DELTA_CURSOR_PREFIX: &str = "health-delta.v1.";

fn health_delta_now() -> UtcMicros {
    let micros = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| {
            duration.as_micros().min(i64::MAX as u128) as i64
        });
    UtcMicros(micros)
}

fn health_score_ppm(value: f64) -> u64 {
    (value.clamp(0.0, 1.0) * 1_000_000.0).round() as u64
}

fn health_dimension_values(snapshot: &VerifiedHealthSnapshotV1) -> [(&'static str, f64); 6] {
    [
        ("acyclicity", snapshot.acyclicity),
        ("depth", snapshot.depth),
        ("equality", snapshot.equality),
        ("redundancy", snapshot.redundancy),
        ("modularity", snapshot.modularity),
        ("coverage_discipline", snapshot.coverage_discipline),
    ]
}

fn health_delta_dimensions(
    snapshot: &VerifiedHealthSnapshotV1,
) -> BTreeMap<String, HealthDimensionPointV1> {
    health_dimension_values(snapshot)
        .into_iter()
        .map(|(name, score)| {
            let denominator = match name {
                "acyclicity" => Some(snapshot.total_edges as u64),
                "depth" => Some(snapshot.max_chain as u64),
                "equality" => Some(snapshot.complexity_files as u64),
                "modularity" => Some(snapshot.modularity_components as u64),
                "redundancy" | "coverage_discipline" => Some(snapshot.total_fns as u64),
                _ => None,
            }
            .filter(|value| *value > 0);
            (
                name.to_owned(),
                HealthDimensionPointV1 {
                    score_ppm: health_score_ppm(score),
                    denominator,
                },
            )
        })
        .collect()
}

fn health_delta_scope(
    project_id: Option<String>,
    path_prefix: Option<&str>,
) -> Result<HealthDeltaScopeV1> {
    let path_prefix = path_prefix
        .map(|raw| {
            let trimmed = raw.trim_matches('/');
            if trimmed.is_empty()
                || trimmed.len() > 4_096
                || raw.starts_with('/')
                || raw.contains('\\')
                || raw.chars().any(char::is_control)
                || trimmed
                    .split('/')
                    .any(|component| component.is_empty() || matches!(component, "." | ".."))
            {
                return Err(TraceDecayError::Config {
                    message: "health-delta path_prefix must be one canonical project-relative path"
                        .to_owned(),
                });
            }
            Ok(trimmed.to_owned())
        })
        .transpose()?;
    let scope_digest = canonical_sha256(&(
        "tracedecay.health-delta.scope.v1",
        project_id.as_deref(),
        path_prefix.as_deref(),
    ))
    .map_err(|error| TraceDecayError::Config {
        message: format!("failed to bind health-delta scope: {error}"),
    })?;
    Ok(HealthDeltaScopeV1 {
        project_id,
        scope_digest,
        path_prefix,
    })
}

fn health_delta_watermark(
    scope: &HealthDeltaScopeV1,
    observed_at: UtcMicros,
    quality_signal: u32,
    files_analyzed: u64,
    function_denominator: u64,
    dimensions: &BTreeMap<String, HealthDimensionPointV1>,
) -> Result<ManifestDigest> {
    canonical_sha256(&(
        "tracedecay.health-delta.watermark.v1",
        scope,
        observed_at,
        quality_signal,
        files_analyzed,
        function_denominator,
        dimensions,
    ))
    .map_err(|error| TraceDecayError::Config {
        message: format!("failed to seal health-delta watermark: {error}"),
    })
}

fn health_delta_cursor(watermark: &ManifestDigest) -> String {
    format!(
        "{HEALTH_DELTA_CURSOR_PREFIX}{}",
        watermark
            .as_str()
            .strip_prefix("sha256:")
            .unwrap_or_default()
    )
}

fn health_delta_digest_from_cursor(cursor: &str) -> Result<&str> {
    let digest = cursor
        .strip_prefix(HEALTH_DELTA_CURSOR_PREFIX)
        .filter(|value| {
            value.len() == 64
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
        .ok_or_else(|| TraceDecayError::Config {
            message: "invalid health-delta cursor".to_owned(),
        })?;
    Ok(digest)
}

async fn persist_health_delta_point(
    db: &RegisteredGlobalDb,
    scope: &HealthDeltaScopeV1,
    point: &HealthDeltaPointV1,
) -> Result<String> {
    let cursor = health_delta_cursor(&point.watermark);
    let payload = HealthSnapshotObservedV1 {
        scope_digest: scope.scope_digest.as_str().to_owned(),
        quality_signal: point.quality_signal,
        files_analyzed: point.files_analyzed,
        function_denominator: point.function_denominator,
        dimensions: point
            .dimensions
            .iter()
            .map(|(name, value)| {
                (
                    name.clone(),
                    HealthDimensionObservedV1 {
                        score_ppm: value.score_ppm,
                        denominator: value.denominator,
                    },
                )
            })
            .collect(),
    };
    let coverage = if point.files_analyzed > 0
        && point.function_denominator > 0
        && point
            .dimensions
            .values()
            .all(|dimension| dimension.denominator.is_some())
    {
        CoverageStateV1::Known
    } else {
        CoverageStateV1::Partial
    };
    let observed_at = point.observed_at.0;
    let envelope = ObservabilityEnvelopeV1 {
        event_id: cursor.clone(),
        event_kind: "health.snapshot.observed.v1".to_owned(),
        schema_revision: HEALTH_DELTA_SCHEMA_VERSION,
        idempotency_key: cursor.clone(),
        trace_id: format!("health-delta:{}", scope.scope_digest.as_str()),
        scope_ref: scope.scope_digest.as_str().to_owned(),
        capability: "health_delta".to_owned(),
        operation: "observe".to_owned(),
        event_time_micros: observed_at,
        observation_time_micros: observed_at,
        valid_from_micros: Some(observed_at),
        valid_until_micros: None,
        quantity: Some(f64::from(point.quality_signal)),
        unit: Some("quality_signal".to_owned()),
        terminal_result: Some(ObservabilityTerminalResultV1::Succeeded),
        producer_revision: "health-delta-projector.v1".to_owned(),
        configuration_revision: "effective-project-configuration.v1".to_owned(),
        policy_revision: "local-health-observation.v1".to_owned(),
        watermark: point.watermark.as_str().to_owned(),
        coverage,
        sampling_probability: None,
        retention_class: ObservabilityRetentionClassV1::OptionalLocalDetail30d,
        emitted_count: 1,
        delayed_count: 0,
        dropped_count: 0,
        process_boot_id: format!("health-delta-{}", std::process::id()),
        producer_sequence: observed_at.max(0) as u64,
        payload: ObservabilityPayloadV1::HealthSnapshot(payload),
    };
    let port = RegisteredObservabilityPortV1::new(db);
    ObservabilityApplicationV1::new(port, port)
        .record(envelope)
        .await
        .map_err(|error| TraceDecayError::Config {
            message: format!("failed to retain health-delta observation: {error}"),
        })?;
    Ok(cursor)
}

async fn load_health_delta_point(
    db: &RegisteredGlobalDb,
    scope: &HealthDeltaScopeV1,
    cursor: &str,
) -> Result<HealthDeltaPointV1> {
    health_delta_digest_from_cursor(cursor)?;
    let port = RegisteredObservabilityPortV1::new(db);
    let page = ObservabilityApplicationV1::new(port, port)
        .query(ObservabilityQueryV1 {
            authorized_scope_ref: scope.scope_digest.as_str().to_owned(),
            event_kinds: vec!["health.snapshot.observed.v1".to_owned()],
            horizon: ObservabilityHorizonV1 {
                since_micros: 0,
                until_micros: i64::MAX,
            },
            after_watermark: None,
            limit: 10_000,
        })
        .await
        .map_err(|error| TraceDecayError::Config {
            message: format!("failed to read health-delta observations: {error}"),
        })?;
    let envelope = page
        .events
        .into_iter()
        .find(|event| event.idempotency_key == cursor)
        .ok_or_else(|| TraceDecayError::Config {
            message: "health-delta cursor is unknown or expired".to_owned(),
        })?;
    let ObservabilityPayloadV1::HealthSnapshot(payload) = envelope.payload else {
        return Err(TraceDecayError::Config {
            message: "health-delta cursor snapshot is invalid".to_owned(),
        });
    };
    let stored = HealthDeltaPointV1 {
        watermark: ManifestDigest::new(envelope.watermark).map_err(|_| {
            TraceDecayError::Config {
                message: "health-delta cursor snapshot is invalid".to_owned(),
            }
        })?,
        observed_at: UtcMicros(envelope.event_time_micros),
        quality_signal: payload.quality_signal,
        files_analyzed: payload.files_analyzed,
        function_denominator: payload.function_denominator,
        dimensions: payload
            .dimensions
            .into_iter()
            .map(|(name, value)| {
                (
                    name,
                    HealthDimensionPointV1 {
                        score_ppm: value.score_ppm,
                        denominator: value.denominator,
                    },
                )
            })
            .collect(),
    };
    let recomputed = health_delta_watermark(
        scope,
        stored.observed_at,
        stored.quality_signal,
        stored.files_analyzed,
        stored.function_denominator,
        &stored.dimensions,
    )?;
    if payload.scope_digest != scope.scope_digest.as_str()
        || stored.watermark != recomputed
        || health_delta_cursor(&recomputed) != cursor
    {
        return Err(TraceDecayError::Config {
            message: "health-delta cursor snapshot failed identity validation".to_owned(),
        });
    }
    Ok(stored)
}

fn health_dimension_deltas(
    before: &HealthDeltaPointV1,
    after: &HealthDeltaPointV1,
) -> BTreeMap<String, HealthDimensionDeltaV1> {
    after
        .dimensions
        .iter()
        .filter_map(|(name, after_value)| {
            let before_value = before.dimensions.get(name)?;
            let delta_ppm = after_value.score_ppm as i64 - before_value.score_ppm as i64;
            Some((
                name.clone(),
                HealthDimensionDeltaV1 {
                    before_ppm: before_value.score_ppm,
                    after_ppm: after_value.score_ppm,
                    delta_ppm,
                    before_denominator: before_value.denominator,
                    after_denominator: after_value.denominator,
                    status: if delta_ppm > 1_000 {
                        "improved"
                    } else if delta_ppm < -1_000 {
                        "degraded"
                    } else {
                        "unchanged"
                    }
                    .to_owned(),
                },
            ))
        })
        .collect()
}

pub async fn compute_verified_health_delta(
    project_id: Option<String>,
    graph: &GraphQueryManager<'_>,
    db: &RegisteredGlobalDb,
    before_cursor: Option<&str>,
    path_prefix: Option<&str>,
) -> Result<HealthDeltaResult> {
    let scope = health_delta_scope(project_id, path_prefix)?;
    let pinned_before = if let Some(cursor) = before_cursor {
        let stored = load_health_delta_point(db, &scope, cursor).await?;
        Some((stored, cursor.to_owned()))
    } else {
        None
    };
    let snapshot = compute_verified_health_snapshot(graph, scope.path_prefix.as_deref()).await?;
    let observed_at = health_delta_now();
    let dimensions = health_delta_dimensions(&snapshot);
    let watermark = health_delta_watermark(
        &scope,
        observed_at,
        snapshot.quality_signal,
        snapshot.files_analyzed as u64,
        snapshot.total_fns as u64,
        &dimensions,
    )?;
    let after = HealthDeltaPointV1 {
        watermark,
        observed_at,
        quality_signal: snapshot.quality_signal,
        files_analyzed: snapshot.files_analyzed as u64,
        function_denominator: snapshot.total_fns as u64,
        dimensions,
    };
    let after_cursor = persist_health_delta_point(db, &scope, &after).await?;
    let (before, before_cursor) =
        pinned_before.unwrap_or_else(|| (after.clone(), after_cursor.clone()));
    let delta = i64::from(after.quality_signal) - i64::from(before.quality_signal);
    let eligible = before.files_analyzed.saturating_add(after.files_analyzed);
    let denominator = (eligible > 0).then_some(eligible);
    Ok(HealthDeltaResult {
        schema_version: HEALTH_DELTA_SCHEMA_VERSION,
        scope,
        before: before.clone(),
        after: after.clone(),
        before_cursor,
        after_cursor,
        pass: denominator.is_some() && delta >= 0,
        delta,
        dimensions: health_dimension_deltas(&before, &after),
        coverage: HealthDeltaCoverageV1 {
            eligible: denominator,
            visited: denominator,
            denominator,
            completeness: if denominator.is_some() {
                "complete"
            } else {
                "unknown"
            }
            .to_owned(),
        },
        currentness: HealthDeltaCurrentnessV1 {
            // Currentness follows the exact snapshots observed above. Legacy
            // branch-database existence/fallback state is neither generation
            // evidence nor an authority for this scoped comparison.
            state: if denominator.is_some() {
                "current"
            } else {
                "degraded"
            }
            .to_owned(),
            observed_at,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn health_delta_scope_is_project_and_path_exact() {
        let first = health_delta_scope(Some("project.first".to_owned()), Some("src/core"))
            .expect("canonical scope");
        let second = health_delta_scope(Some("project.second".to_owned()), Some("src/core"))
            .expect("canonical scope");
        let other_path = health_delta_scope(Some("project.first".to_owned()), Some("src/other"))
            .expect("canonical scope");

        assert_eq!(first.project_id.as_deref(), Some("project.first"));
        assert_eq!(first.path_prefix.as_deref(), Some("src/core"));
        assert_ne!(first.scope_digest, second.scope_digest);
        assert_ne!(first.scope_digest, other_path.scope_digest);
    }

    #[test]
    fn health_delta_scope_rejects_noncanonical_paths() {
        for path in ["/src", "src\\lib", "src/../other", "src//lib", "."] {
            assert!(
                health_delta_scope(Some("project.first".to_owned()), Some(path)).is_err(),
                "{path} must not enter a persisted health-delta scope"
            );
        }
    }

    #[test]
    fn health_delta_cursor_round_trips_only_canonical_watermarks() {
        let watermark =
            ManifestDigest::new(format!("sha256:{}", "a".repeat(64))).expect("canonical watermark");
        let cursor = health_delta_cursor(&watermark);
        let digest = health_delta_digest_from_cursor(&cursor).expect("canonical cursor");

        assert_eq!(digest, "a".repeat(64));
        assert!(health_delta_digest_from_cursor("health-delta.v1.not-a-digest").is_err());
    }
}
