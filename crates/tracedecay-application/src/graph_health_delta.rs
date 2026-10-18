//! Scoped health deltas: the observability cursor, the persisted watermark point, and the dimension-by-dimension comparison against it.
//!
//! The application primitive and MCP adapters call this one use case after
//! opening an admitted, generation-pinned graph reader.

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;

use tracedecay_contracts::retrieval::{
    HealthDeltaCoverageV1, HealthDeltaCurrentnessV1, HealthDeltaPointV1, HealthDeltaResult,
    HealthDeltaScopeV1, HealthDimensionDeltaV1, HealthDimensionPointV1,
};
use tracedecay_contracts::{ApplicationContractError, ObservabilityApplicationV1};
use tracedecay_domain::{
    CoverageStateV1, HealthDimensionObservedV1, HealthSnapshotObservedV1, ManifestDigest,
    ObservabilityEnvelopeV1, ObservabilityPayloadV1, ObservabilityRetentionClassV1,
    ObservabilityTerminalResultV1, UtcMicros, canonical_sha256, sha256_hex_suffix,
};

use tracedecay_global_db::RegisteredGlobalDb;
use tracedecay_graph_query::GraphQueryError;

use crate::observability::RegisteredObservabilityPortV1;

use tracedecay_graph_query::health::{
    HEALTH_DEFINITION, HealthDimension, VerifiedHealthSnapshotV1, compute_verified_health_snapshot,
};
use tracedecay_graph_query::queries::GraphQueryManager;

const HEALTH_DELTA_SCHEMA_VERSION: u32 = 1;
const HEALTH_DELTA_CURSOR_PREFIX: &str = "health-delta.v1.";

#[derive(Debug, thiserror::Error)]
pub enum HealthDeltaError {
    #[error("health-delta path_prefix must be one canonical project-relative path")]
    InvalidPathPrefix,
    #[error("invalid health-delta cursor")]
    MalformedCursor,
    #[error("health-delta cursor is unknown or expired")]
    UnknownCursor,
    #[error("health-delta point failed identity validation")]
    CorruptPoint,
    #[error(
        "the before point was measured under health definition {recorded}, not {HEALTH_DEFINITION}"
    )]
    DefinitionMismatch { recorded: String },
    #[error("the before point predates recorded health definitions")]
    DefinitionUnrecorded,
    #[error(transparent)]
    Graph(GraphQueryError),
    #[error("health-delta storage failed: {0}")]
    Storage(String),
}

type Result<T> = std::result::Result<T, HealthDeltaError>;

fn storage(context: &str, error: impl std::fmt::Display) -> HealthDeltaError {
    HealthDeltaError::Storage(format!("{context}: {error}"))
}

#[derive(Debug)]
enum PersistHealthDeltaError {
    IdempotencyConflict,
    Other(HealthDeltaError),
}

fn health_delta_dimensions(
    snapshot: &VerifiedHealthSnapshotV1,
) -> BTreeMap<String, HealthDimensionPointV1> {
    HealthDimension::ALL
        .into_iter()
        .map(|dimension| {
            (
                dimension.name().to_owned(),
                HealthDimensionPointV1 {
                    score_ppm: dimension.score_ppm(snapshot),
                    denominator: dimension.denominator(snapshot),
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
                return Err(HealthDeltaError::InvalidPathPrefix);
            }
            Ok(trimmed.to_owned())
        })
        .transpose()?;
    let scope_digest = canonical_sha256(&(
        "tracedecay.health-delta.scope.v1",
        project_id.as_deref(),
        path_prefix.as_deref(),
    ))
    .map_err(|error| storage("failed to bind health-delta scope", error))?;
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
    .map_err(|error| storage("failed to seal health-delta watermark", error))
}

fn health_delta_cursor(scope: &HealthDeltaScopeV1, generation: &str) -> Result<String> {
    let digest = canonical_sha256(&(
        "tracedecay.health-delta.cursor.v2",
        scope.scope_digest.as_str(),
        generation,
        HEALTH_DEFINITION,
    ))
    .map_err(|error| storage("failed to bind health-delta cursor", error))?;
    Ok(format!(
        "{HEALTH_DELTA_CURSOR_PREFIX}{}",
        sha256_hex_suffix(digest.as_str()).unwrap_or_default()
    ))
}

fn health_delta_digest_from_cursor(cursor: &str) -> Result<&str> {
    cursor
        .strip_prefix(HEALTH_DELTA_CURSOR_PREFIX)
        .filter(|value| {
            value.len() == 64
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
        .ok_or(HealthDeltaError::MalformedCursor)
}

fn health_delta_envelope(
    scope: &HealthDeltaScopeV1,
    point: &HealthDeltaPointV1,
    cursor: &str,
) -> ObservabilityEnvelopeV1 {
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
        definition: Some(HEALTH_DEFINITION.to_owned()),
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
    ObservabilityEnvelopeV1 {
        event_id: cursor.to_owned(),
        event_kind: "health.snapshot.observed.v1".to_owned(),
        schema_revision: HEALTH_DELTA_SCHEMA_VERSION,
        idempotency_key: cursor.to_owned(),
        trace_id: cursor.to_owned(),
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
    }
}

async fn record_health_delta_envelope(
    db: &RegisteredGlobalDb,
    envelope: ObservabilityEnvelopeV1,
) -> std::result::Result<(), PersistHealthDeltaError> {
    let port = RegisteredObservabilityPortV1::new(db);
    ObservabilityApplicationV1::new(port, port)
        .record(envelope)
        .await
        .map_err(|error| match error {
            ApplicationContractError::Domain(message)
                if message == "observability idempotency conflict" =>
            {
                PersistHealthDeltaError::IdempotencyConflict
            }
            error => PersistHealthDeltaError::Other(storage(
                "failed to retain health-delta observation",
                error,
            )),
        })?;
    Ok(())
}

#[tracing::instrument(
    name = "usecases.graph.health_delta.persist_canonical",
    level = "trace",
    skip_all
)]
async fn persist_canonical_health_delta_point(
    db: &RegisteredGlobalDb,
    scope: &HealthDeltaScopeV1,
    point: &HealthDeltaPointV1,
    cursor: &str,
) -> Result<HealthDeltaPointV1> {
    match record_health_delta_envelope(db, health_delta_envelope(scope, point, cursor)).await {
        Ok(()) | Err(PersistHealthDeltaError::IdempotencyConflict) => {
            load_health_delta_point(db, scope, cursor).await
        }
        Err(PersistHealthDeltaError::Other(error)) => Err(error),
    }
}

#[tracing::instrument(name = "usecases.graph.health_delta.find", level = "trace", skip_all)]
async fn find_health_delta_point(
    db: &RegisteredGlobalDb,
    scope: &HealthDeltaScopeV1,
    cursor: &str,
) -> Result<Option<HealthDeltaPointV1>> {
    health_delta_digest_from_cursor(cursor)?;
    let port = RegisteredObservabilityPortV1::new(db);
    let stored: Pin<
        Box<
            dyn Future<
                    Output = std::result::Result<
                        Option<ObservabilityEnvelopeV1>,
                        ApplicationContractError,
                    >,
                > + Send
                + '_,
        >,
    > = Box::pin(port.read_exact(scope.scope_digest.as_str(), cursor));
    stored
        .await
        .map_err(|error| storage("failed to read health-delta observation", error))?
        .map(|envelope| verify_health_delta_point(scope, envelope))
        .transpose()
}

#[tracing::instrument(name = "usecases.graph.health_delta.load", level = "trace", skip_all)]
async fn load_health_delta_point(
    db: &RegisteredGlobalDb,
    scope: &HealthDeltaScopeV1,
    cursor: &str,
) -> Result<HealthDeltaPointV1> {
    find_health_delta_point(db, scope, cursor)
        .await?
        .ok_or(HealthDeltaError::UnknownCursor)
}

fn verify_health_delta_point(
    scope: &HealthDeltaScopeV1,
    envelope: ObservabilityEnvelopeV1,
) -> Result<HealthDeltaPointV1> {
    let ObservabilityPayloadV1::HealthSnapshot(payload) = envelope.payload else {
        return Err(HealthDeltaError::CorruptPoint);
    };
    let stored = HealthDeltaPointV1 {
        watermark: ManifestDigest::new(envelope.watermark)
            .map_err(|_| HealthDeltaError::CorruptPoint)?,
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
    if payload.scope_digest != scope.scope_digest.as_str() || stored.watermark != recomputed {
        return Err(HealthDeltaError::CorruptPoint);
    }
    match payload.definition {
        Some(recorded) if recorded == HEALTH_DEFINITION => Ok(stored),
        Some(recorded) => Err(HealthDeltaError::DefinitionMismatch { recorded }),
        None => Err(HealthDeltaError::DefinitionUnrecorded),
    }
}

fn health_dimension_deltas(
    before: &HealthDeltaPointV1,
    after: &HealthDeltaPointV1,
) -> BTreeMap<String, HealthDimensionDeltaV1> {
    let measured =
        |dimension: HealthDimension, point: &HealthDeltaPointV1, value: &HealthDimensionPointV1| {
            dimension
                .no_evidence(value.denominator, point.files_analyzed)
                .is_none()
        };
    HealthDimension::ALL
        .into_iter()
        .filter_map(|dimension| {
            let before_value = before.dimensions.get(dimension.name())?;
            let after_value = after.dimensions.get(dimension.name())?;
            let delta_ppm = after_value.score_ppm as i64 - before_value.score_ppm as i64;
            let status = if !measured(dimension, before, before_value)
                || !measured(dimension, after, after_value)
            {
                "no_evidence"
            } else if delta_ppm > 1_000 {
                "improved"
            } else if delta_ppm < -1_000 {
                "degraded"
            } else {
                "unchanged"
            };
            Some((
                dimension.name().to_owned(),
                HealthDimensionDeltaV1 {
                    before_ppm: before_value.score_ppm,
                    after_ppm: after_value.score_ppm,
                    delta_ppm,
                    before_denominator: before_value.denominator,
                    after_denominator: after_value.denominator,
                    status: status.to_owned(),
                },
            ))
        })
        .collect()
}

fn health_delta_result(
    scope: HealthDeltaScopeV1,
    (before, before_cursor): (HealthDeltaPointV1, String),
    (after, after_cursor): (HealthDeltaPointV1, String),
) -> HealthDeltaResult {
    let delta = i64::from(after.quality_signal) - i64::from(before.quality_signal);
    let comparable = before.files_analyzed > 0 && after.files_analyzed > 0;
    let files = after.files_analyzed;
    let observed_at = after.observed_at;
    HealthDeltaResult {
        schema_version: HEALTH_DELTA_SCHEMA_VERSION,
        scope,
        dimensions: health_dimension_deltas(&before, &after),
        before,
        after,
        before_cursor,
        after_cursor,
        pass: comparable && delta >= 0,
        delta,
        coverage: HealthDeltaCoverageV1 {
            eligible: Some(files),
            visited: Some(files),
            denominator: (files > 0).then_some(files),
            completeness: if comparable { "complete" } else { "unknown" }.to_owned(),
        },
        currentness: HealthDeltaCurrentnessV1 {
            state: "current".to_owned(),
            observed_at,
        },
    }
}

#[tracing::instrument(name = "usecases.graph.health_delta", level = "trace", skip_all)]
pub async fn compute_verified_health_delta(
    project_id: Option<String>,
    graph: &GraphQueryManager<'_>,
    db: &RegisteredGlobalDb,
    before_cursor: Option<&str>,
    path_prefix: Option<&str>,
) -> Result<HealthDeltaResult> {
    let scope = health_delta_scope(project_id, path_prefix)?;
    let current_cursor = health_delta_cursor(&scope, graph.generation().as_str())?;
    let pinned_before = if let Some(cursor) = before_cursor {
        let stored: Pin<Box<dyn Future<Output = Result<HealthDeltaPointV1>> + Send + '_>> =
            Box::pin(load_health_delta_point(db, &scope, cursor));
        let stored = stored.await?;
        Some((stored, cursor.to_owned()))
    } else {
        None
    };
    let current: Pin<Box<dyn Future<Output = Result<Option<HealthDeltaPointV1>>> + Send + '_>> =
        Box::pin(find_health_delta_point(db, &scope, &current_cursor));
    let current = current.await?;
    let after = if let Some(after) = current {
        after
    } else {
        let snapshot: Pin<
            Box<
                dyn Future<
                        Output = tracedecay_graph_query::GraphQueryResult<VerifiedHealthSnapshotV1>,
                    > + Send
                    + '_,
            >,
        > = Box::pin(compute_verified_health_snapshot(
            graph,
            scope.path_prefix.as_deref(),
        ));
        let snapshot = snapshot.await.map_err(HealthDeltaError::Graph)?;
        let observed_at = tracedecay_contracts::now_micros();
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
        tracing::Instrument::instrument(
            persist_canonical_health_delta_point(db, &scope, &after, &current_cursor),
            tracing::trace_span!("usecases.graph.health_delta.persist_canonical"),
        )
        .await?
    };
    let after = (after, current_cursor);
    let before = pinned_before.unwrap_or_else(|| after.clone());
    Ok(health_delta_result(scope, before, after))
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
            let error = health_delta_scope(Some("project.first".to_owned()), Some(path))
                .expect_err("noncanonical path must not enter a persisted health-delta scope");
            assert!(
                matches!(error, HealthDeltaError::InvalidPathPrefix),
                "{path}: {error}"
            );
        }
    }

    #[test]
    fn health_delta_cursor_is_scope_and_generation_bound() {
        let scope = health_delta_scope(Some("project.first".to_owned()), Some("src/core"))
            .expect("canonical scope");
        let cursor = health_delta_cursor(&scope, "generation.first").expect("cursor");
        let digest = health_delta_digest_from_cursor(&cursor).expect("canonical cursor");

        assert_eq!(digest.len(), 64);
        assert_ne!(
            cursor,
            health_delta_cursor(&scope, "generation.second").expect("next cursor")
        );
        let error = health_delta_digest_from_cursor("health-delta.v1.not-a-digest").unwrap_err();
        assert!(
            matches!(error, HealthDeltaError::MalformedCursor),
            "{error}"
        );
    }

    #[tokio::test]
    async fn health_observation_exact_read_survives_capped_history_and_enforces_scope() {
        let harness = tracedecay_global_db::tests::harness::RegisteredGlobalDbHarness::open(
            "health-delta-generation-reuse",
        )
        .await;
        let scope = health_delta_scope(Some("project.first".to_owned()), Some("src/core"))
            .expect("canonical scope");
        let dimensions = BTreeMap::from([(
            "acyclicity".to_owned(),
            HealthDimensionPointV1 {
                score_ppm: 900_000,
                denominator: Some(10),
            },
        )]);
        let observed_at = UtcMicros(100);
        let point = HealthDeltaPointV1 {
            watermark: health_delta_watermark(&scope, observed_at, 90, 2, 3, &dimensions)
                .expect("watermark"),
            observed_at,
            quality_signal: 90,
            files_analyzed: 2,
            function_denominator: 3,
            dimensions,
        };
        let cursor = health_delta_cursor(&scope, "generation.first").expect("cursor");
        record_health_delta_envelope(
            &harness.registered,
            health_delta_envelope(&scope, &point, &cursor),
        )
        .await
        .expect("persist current point");
        let template = harness
            .registered
            .read_observability_event(scope.scope_digest.as_str(), &cursor)
            .await
            .expect("read persisted template")
            .expect("persisted template");
        let template = serde_json::from_str::<ObservabilityEnvelopeV1>(
            template
                .metadata_json
                .as_deref()
                .expect("template envelope"),
        )
        .expect("decode template envelope");
        let noise = (0..10_001)
            .map(|index| {
                let mut envelope = template.clone();
                envelope.event_id = format!("health-noise:{index}");
                envelope.idempotency_key = format!("health-noise:{index}");
                envelope.event_time_micros = 1_000_000 + index;
                envelope.observation_time_micros = envelope.event_time_micros;
                envelope.valid_from_micros = Some(envelope.event_time_micros);
                envelope.producer_sequence = envelope.event_time_micros as u64;
                tracedecay_global_db::AnalyticsEventInsert {
                    provider: "tracedecay-observability".to_owned(),
                    project_id: envelope.scope_ref.clone(),
                    session_id: None,
                    timestamp: envelope.event_time_micros.div_euclid(1_000_000),
                    event_kind: envelope.event_kind.clone(),
                    hook_name: None,
                    tool_name: None,
                    tool_category: None,
                    skill_name: None,
                    hint_category: None,
                    hint_id: Some(envelope.idempotency_key.clone()),
                    outcome: Some("succeeded".to_owned()),
                    metadata_json: Some(
                        serde_json::to_string(&envelope).expect("encode noise envelope"),
                    ),
                }
            })
            .collect::<Vec<_>>();
        harness
            .registered
            .append_analytics_events(&noise)
            .await
            .expect("append newer unrelated history");

        let reused = find_health_delta_point(&harness.registered, &scope, &cursor)
            .await
            .expect("load current point")
            .expect("point for exact generation");
        assert_eq!(reused, point);
        assert!(
            find_health_delta_point(
                &harness.registered,
                &scope,
                &health_delta_cursor(&scope, "generation.second").expect("next cursor"),
            )
            .await
            .expect("query next generation")
            .is_none()
        );
        let other_scope = health_delta_scope(Some("project.other".to_owned()), Some("src/core"))
            .expect("other scope");
        assert!(
            find_health_delta_point(&harness.registered, &other_scope, &cursor)
                .await
                .expect("query other scope")
                .is_none()
        );
    }

    #[tokio::test]
    async fn simultaneous_generation_observations_return_one_canonical_point() {
        let harness = tracedecay_global_db::tests::harness::RegisteredGlobalDbHarness::open(
            "health-delta-concurrent-generation",
        )
        .await;
        let scope = health_delta_scope(Some("project.first".to_owned()), Some("src"))
            .expect("canonical scope");
        let dimensions = BTreeMap::from([(
            "acyclicity".to_owned(),
            HealthDimensionPointV1 {
                score_ppm: 900_000,
                denominator: Some(10),
            },
        )]);
        let point = |observed_at: UtcMicros| HealthDeltaPointV1 {
            watermark: health_delta_watermark(&scope, observed_at, 90, 2, 3, &dimensions)
                .expect("watermark"),
            observed_at,
            quality_signal: 90,
            files_analyzed: 2,
            function_denominator: 3,
            dimensions: dimensions.clone(),
        };
        let first = point(UtcMicros(100));
        let second = point(UtcMicros(101));
        let cursor = health_delta_cursor(&scope, "generation.first").expect("cursor");

        let (first_result, second_result) = tokio::join!(
            persist_canonical_health_delta_point(&harness.registered, &scope, &first, &cursor),
            persist_canonical_health_delta_point(&harness.registered, &scope, &second, &cursor),
        );
        let first_result = first_result.expect("first concurrent observation");
        let second_result = second_result.expect("second concurrent observation");

        assert_eq!(first_result, second_result);
        assert!(first_result == first || first_result == second);
        assert_eq!(
            find_health_delta_point(&harness.registered, &scope, &cursor)
                .await
                .expect("repeat exact read")
                .expect("canonical point"),
            first_result
        );
    }

    fn scored_point(
        scope: &HealthDeltaScopeV1,
        files_analyzed: u64,
        quality_signal: u32,
        dimensions: &[(HealthDimension, u64, Option<u64>)],
    ) -> HealthDeltaPointV1 {
        let dimensions = dimensions
            .iter()
            .map(|(dimension, score_ppm, denominator)| {
                (
                    dimension.name().to_owned(),
                    HealthDimensionPointV1 {
                        score_ppm: *score_ppm,
                        denominator: *denominator,
                    },
                )
            })
            .collect::<BTreeMap<_, _>>();
        let observed_at = UtcMicros(100);
        HealthDeltaPointV1 {
            watermark: health_delta_watermark(
                scope,
                observed_at,
                quality_signal,
                files_analyzed,
                4,
                &dimensions,
            )
            .expect("watermark"),
            observed_at,
            quality_signal,
            files_analyzed,
            function_denominator: 4,
            dimensions,
        }
    }

    fn unlabeled_release_cursor(scope: &HealthDeltaScopeV1, generation: &str) -> String {
        let digest = canonical_sha256(&(
            "tracedecay.health-delta.cursor.v1",
            scope.scope_digest.as_str(),
            generation,
        ))
        .expect("legacy cursor digest");
        format!(
            "{HEALTH_DELTA_CURSOR_PREFIX}{}",
            sha256_hex_suffix(digest.as_str()).expect("legacy cursor hex")
        )
    }

    async fn record_point_under(
        db: &RegisteredGlobalDb,
        scope: &HealthDeltaScopeV1,
        point: &HealthDeltaPointV1,
        cursor: &str,
        definition: Option<&str>,
    ) {
        let mut envelope = health_delta_envelope(scope, point, cursor);
        let ObservabilityPayloadV1::HealthSnapshot(payload) = &mut envelope.payload else {
            panic!("health-delta envelopes carry a health snapshot");
        };
        payload.definition = definition.map(str::to_owned);
        record_health_delta_envelope(db, envelope)
            .await
            .expect("record point");
    }

    #[tokio::test]
    async fn an_unlabeled_released_point_is_refused_and_never_reused_as_after() {
        let harness = tracedecay_global_db::tests::harness::RegisteredGlobalDbHarness::open(
            "health-delta-unlabeled-point",
        )
        .await;
        let scope = health_delta_scope(Some("project.first".to_owned()), Some("src"))
            .expect("canonical scope");
        let point = scored_point(
            &scope,
            2,
            8706,
            &[(HealthDimension::Modularity, 500_000, Some(2))],
        );
        let released = unlabeled_release_cursor(&scope, "generation.first");
        record_point_under(&harness.registered, &scope, &point, &released, None).await;

        let error = load_health_delta_point(&harness.registered, &scope, &released)
            .await
            .expect_err("an unlabeled before point must not be compared");
        assert!(
            matches!(error, HealthDeltaError::DefinitionUnrecorded),
            "{error}"
        );
        let current = health_delta_cursor(&scope, "generation.first").expect("cursor");
        assert_ne!(current, released);
        assert!(
            find_health_delta_point(&harness.registered, &scope, &current)
                .await
                .expect("read current generation")
                .is_none(),
            "the current definition must mint its own point at an observed generation"
        );
    }

    #[tokio::test]
    async fn a_point_from_another_definition_is_refused() {
        let harness = tracedecay_global_db::tests::harness::RegisteredGlobalDbHarness::open(
            "health-delta-other-definition",
        )
        .await;
        let scope =
            health_delta_scope(Some("project.first".to_owned()), None).expect("canonical scope");
        let point = scored_point(
            &scope,
            2,
            8706,
            &[(HealthDimension::Modularity, 500_000, Some(2))],
        );
        let cursor = health_delta_cursor(&scope, "generation.first").expect("cursor");
        record_point_under(
            &harness.registered,
            &scope,
            &point,
            &cursor,
            Some("health.2026-01-01"),
        )
        .await;

        let error = load_health_delta_point(&harness.registered, &scope, &cursor)
            .await
            .expect_err("a point from another definition must not be compared");
        assert!(
            matches!(
                &error,
                HealthDeltaError::DefinitionMismatch { recorded } if recorded == "health.2026-01-01"
            ),
            "{error}"
        );
    }

    #[test]
    fn an_emptied_scope_fails_and_counts_only_the_current_files() {
        let scope = health_delta_scope(Some("project.first".to_owned()), Some("src"))
            .expect("canonical scope");
        let before = scored_point(
            &scope,
            3,
            8706,
            &[(HealthDimension::Modularity, 500_000, Some(2))],
        );
        let emptied = scored_point(
            &scope,
            0,
            10_000,
            &[(HealthDimension::Modularity, 1_000_000, None)],
        );

        let result = health_delta_result(
            scope,
            (before, "before".to_owned()),
            (emptied, "after".to_owned()),
        );

        assert_eq!(result.delta, 1_294);
        assert!(
            !result.pass,
            "an emptied scope must not pass on the empty-graph score"
        );
        assert_eq!(result.coverage.eligible, Some(0));
        assert_eq!(result.coverage.visited, Some(0));
        assert_eq!(result.coverage.denominator, None);
        assert_eq!(result.coverage.completeness, "unknown");
        assert_eq!(result.dimensions["modularity"].status, "no_evidence");
    }

    #[test]
    fn a_comparable_delta_counts_the_current_files_and_reads_each_dimension() {
        let scope =
            health_delta_scope(Some("project.first".to_owned()), None).expect("canonical scope");
        let before = scored_point(
            &scope,
            3,
            7_000,
            &[
                (HealthDimension::Modularity, 500_000, Some(2)),
                (HealthDimension::Acyclicity, 1_000_000, None),
                (HealthDimension::Redundancy, 900_000, Some(4)),
            ],
        );
        let after = scored_point(
            &scope,
            2,
            6_900,
            &[
                (HealthDimension::Modularity, 666_667, Some(3)),
                (HealthDimension::Acyclicity, 500_000, Some(2)),
                (HealthDimension::Redundancy, 899_500, Some(4)),
            ],
        );

        let result = health_delta_result(
            scope,
            (before, "before".to_owned()),
            (after, "after".to_owned()),
        );

        assert!(!result.pass);
        assert_eq!(result.delta, -100);
        assert_eq!(result.coverage.eligible, Some(2));
        assert_eq!(result.coverage.visited, Some(2));
        assert_eq!(result.coverage.denominator, Some(2));
        assert_eq!(result.coverage.completeness, "complete");
        assert_eq!(result.currentness.state, "current");
        assert_eq!(result.dimensions["modularity"].status, "improved");
        assert_eq!(result.dimensions["acyclicity"].status, "no_evidence");
        assert_eq!(result.dimensions["redundancy"].status, "unchanged");
    }

    #[test]
    fn a_baseline_passes_only_when_the_scope_has_files() {
        let scope =
            health_delta_scope(Some("project.first".to_owned()), None).expect("canonical scope");
        let baseline = |files| {
            let point = scored_point(&scope, files, 9_000, &[]);
            health_delta_result(
                scope.clone(),
                (point.clone(), "cursor".to_owned()),
                (point, "cursor".to_owned()),
            )
        };

        assert!(baseline(2).pass);
        assert!(!baseline(0).pass);
    }
}
