use super::*;

use tracedecay_application::{
    EXECUTION_TOPOLOGY_EVENT_KINDS_V1, EXECUTION_TOPOLOGY_PROJECTOR_REVISION_V1,
    ExecutionTopologyRollupBuildV1, ExecutionTopologyRollupRetentionV1,
    MAX_EXECUTION_TOPOLOGY_EVENTS_V1, ObservabilityHorizonV1, ObservabilityPageV1,
    ObservabilityQueryPort, ObservabilityQueryV1, build_empty_execution_topology_daily_rollup,
    build_execution_topology_daily_rollup, check_execution_topology_rollup_retention_json,
};
use tracedecay_global_db::{
    ObservabilityRollupCompactionV1, ObservabilityRollupDirtyDayClaimV1,
    ObservabilityRollupEmptyDayClaimOutcomeV1, ObservabilityRollupEmptyDayClaimV1,
    ObservabilityRollupRebuildV1,
};

use crate::observability::RegisteredObservabilityPortV1;

const UTC_DAY_SECONDS: i64 = 86_400;
const MICROS_PER_SECOND: i64 = 1_000_000;
const ROLLUP_LEASE_SECONDS: u32 = 30;
const TELEMETRY_DROP_EVENT_KIND: &str = "telemetry.drop.observed.v1";

struct RollupPublicationV1 {
    authorized_scope_ref: String,
    day_start_seconds: i64,
    source_watermark: i64,
    coverage: CoverageStateV1,
    dirty_claim: Option<ObservabilityRollupDirtyDayClaimV1>,
    empty_day_claim: Option<ObservabilityRollupEmptyDayClaimV1>,
    fragment_json: String,
}

/// Advances at most one dirty day, proved-quiet day, existing frontier day, or
/// retained-fragment compaction. Request paths never perform this work.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RollupAdvanceOutcome {
    Progressed,
    None,
    Deferred,
}

pub(super) async fn run_one_rollup_maintenance(
    db: &RegisteredGlobalDb,
    identity: &ObservabilityProducerIdentityV1,
    persistence_deadline: Duration,
    frontier_initialized: &mut bool,
) -> RollupAdvanceOutcome {
    if !*frontier_initialized {
        match timeout(
            persistence_deadline,
            db.initialize_observability_rollup_frontier(&identity.authorized_scope_ref),
        )
        .await
        {
            Ok(Ok(_)) => *frontier_initialized = true,
            Ok(Err(error)) => {
                tracing::warn!(%error, "observability rollup frontier initialization failed");
                return RollupAdvanceOutcome::Deferred;
            }
            Err(_) => {
                tracing::warn!(
                    "observability rollup frontier initialization exceeded persistence deadline"
                );
                return RollupAdvanceOutcome::Deferred;
            }
        }
    }
    let dirty = rebuild_one_dirty_day(db, identity, persistence_deadline).await;
    if dirty != RollupAdvanceOutcome::None {
        return dirty;
    }
    let empty = close_one_empty_day(db, identity, persistence_deadline).await;
    if empty != RollupAdvanceOutcome::None {
        return empty;
    }
    compact_one_rollup_fragment(db, identity, persistence_deadline).await
}

async fn compact_one_rollup_fragment(
    db: &RegisteredGlobalDb,
    identity: &ObservabilityProducerIdentityV1,
    persistence_deadline: Duration,
) -> RollupAdvanceOutcome {
    let now_micros = now_micros().0;
    let candidate = match timeout(
        persistence_deadline,
        db.next_observability_rollup_compaction(&identity.authorized_scope_ref),
    )
    .await
    {
        Ok(Ok(Some(candidate))) => candidate,
        Ok(Ok(None)) => return RollupAdvanceOutcome::None,
        Ok(Err(error)) => {
            tracing::warn!(%error, "observability rollup compaction selection failed");
            return RollupAdvanceOutcome::Deferred;
        }
        Err(_) => {
            tracing::warn!("observability rollup compaction selection exceeded deadline");
            return RollupAdvanceOutcome::Deferred;
        }
    };
    let source_fragment_json = candidate.fragment_json.clone();
    let compaction = tokio::task::spawn_blocking(move || {
        check_execution_topology_rollup_retention_json(&source_fragment_json, now_micros)
    });
    let fragment_json = match timeout(persistence_deadline, compaction).await {
        Ok(Ok(Ok(ExecutionTopologyRollupRetentionV1::Unchanged))) => {
            candidate.fragment_json.clone()
        }
        Ok(Ok(Ok(ExecutionTopologyRollupRetentionV1::Updated { fragment_json }))) => fragment_json,
        Ok(Ok(Err(error))) => {
            tracing::warn!(%error, "observability rollup application compaction refused");
            return RollupAdvanceOutcome::Deferred;
        }
        Ok(Err(error)) => {
            tracing::warn!(%error, "observability rollup application compaction task failed");
            return RollupAdvanceOutcome::Deferred;
        }
        Err(_) => {
            tracing::warn!("observability rollup application compaction exceeded deadline");
            return RollupAdvanceOutcome::Deferred;
        }
    };
    match timeout(
        persistence_deadline,
        db.compact_observability_rollup_fragment(ObservabilityRollupCompactionV1 {
            candidate,
            fragment_json,
        }),
    )
    .await
    {
        Ok(Ok(_)) => RollupAdvanceOutcome::Progressed,
        Ok(Err(error)) => {
            tracing::warn!(%error, "observability rollup compaction publication failed");
            RollupAdvanceOutcome::Deferred
        }
        Err(_) => {
            tracing::warn!("observability rollup compaction publication exceeded deadline");
            RollupAdvanceOutcome::Deferred
        }
    }
}

async fn rebuild_one_dirty_day(
    db: &RegisteredGlobalDb,
    identity: &ObservabilityProducerIdentityV1,
    persistence_deadline: Duration,
) -> RollupAdvanceOutcome {
    let claimant_id = format!("observability-rollup:{}", identity.process_boot_id);
    let claim = match timeout(
        persistence_deadline,
        db.claim_observability_rollup_dirty_day(
            &identity.authorized_scope_ref,
            &claimant_id,
            ROLLUP_LEASE_SECONDS,
        ),
    )
    .await
    {
        Ok(Ok(Some(claim))) => claim,
        Ok(Ok(None)) => return RollupAdvanceOutcome::None,
        Ok(Err(error)) => {
            tracing::warn!(%error, "observability rollup dirty-day claim failed");
            return RollupAdvanceOutcome::Deferred;
        }
        Err(_) => {
            tracing::warn!("observability rollup dirty-day claim exceeded persistence deadline");
            return RollupAdvanceOutcome::Deferred;
        }
    };

    let result = timeout(
        persistence_deadline,
        build_and_publish_claimed_day(db, &claim),
    )
    .await;
    match result {
        Ok(Ok(())) => return RollupAdvanceOutcome::Progressed,
        Ok(Err(error)) => {
            tracing::warn!(%error, "observability rollup dirty-day rebuild deferred");
        }
        Err(_) => {
            tracing::warn!("observability rollup dirty-day rebuild exceeded persistence deadline");
        }
    }

    match timeout(
        persistence_deadline,
        db.release_observability_rollup_dirty_day(&claim),
    )
    .await
    {
        Ok(Ok(_)) => {}
        Ok(Err(error)) => {
            tracing::warn!(%error, "observability rollup dirty-day release failed");
        }
        Err(_) => {
            tracing::warn!("observability rollup dirty-day release exceeded persistence deadline");
        }
    }
    RollupAdvanceOutcome::Deferred
}

async fn close_one_empty_day(
    db: &RegisteredGlobalDb,
    identity: &ObservabilityProducerIdentityV1,
    persistence_deadline: Duration,
) -> RollupAdvanceOutcome {
    let claimant_id = format!("observability-rollup-empty:{}", identity.process_boot_id);
    let outcome = match timeout(
        persistence_deadline,
        db.claim_observability_rollup_empty_day(
            &identity.authorized_scope_ref,
            &claimant_id,
            ROLLUP_LEASE_SECONDS,
        ),
    )
    .await
    {
        Ok(Ok(outcome)) => outcome,
        Ok(Err(error)) => {
            tracing::warn!(%error, "observability empty-day claim failed");
            return RollupAdvanceOutcome::Deferred;
        }
        Err(_) => {
            tracing::warn!("observability empty-day claim exceeded persistence deadline");
            return RollupAdvanceOutcome::Deferred;
        }
    };
    let claim = match outcome {
        ObservabilityRollupEmptyDayClaimOutcomeV1::Claimed(claim) => claim,
        ObservabilityRollupEmptyDayClaimOutcomeV1::AdvancedExisting { .. } => {
            return RollupAdvanceOutcome::Progressed;
        }
        ObservabilityRollupEmptyDayClaimOutcomeV1::NotReady { .. } => {
            return RollupAdvanceOutcome::None;
        }
        ObservabilityRollupEmptyDayClaimOutcomeV1::DirtyDay { .. }
        | ObservabilityRollupEmptyDayClaimOutcomeV1::Leased { .. } => {
            return RollupAdvanceOutcome::Deferred;
        }
    };
    let result = timeout(
        persistence_deadline,
        build_and_publish_empty_day(db, &claim),
    )
    .await;
    match result {
        Ok(Ok(())) => return RollupAdvanceOutcome::Progressed,
        Ok(Err(error)) => {
            tracing::warn!(%error, "observability empty-day publication deferred");
        }
        Err(_) => {
            tracing::warn!("observability empty-day publication exceeded persistence deadline");
        }
    }
    match timeout(
        persistence_deadline,
        db.release_observability_rollup_empty_day(&claim),
    )
    .await
    {
        Ok(Ok(_)) => {}
        Ok(Err(error)) => {
            tracing::warn!(%error, "observability empty-day release failed");
        }
        Err(_) => {
            tracing::warn!("observability empty-day release exceeded persistence deadline");
        }
    }
    RollupAdvanceOutcome::Deferred
}

async fn build_and_publish_empty_day(
    db: &RegisteredGlobalDb,
    claim: &ObservabilityRollupEmptyDayClaimV1,
) -> Result<(), String> {
    let horizon = day_horizon(claim.day_start_seconds)?;
    let page = query_rollup_source_page(db, &claim.authorized_scope_ref, &horizon).await?;
    if !page.events.is_empty()
        || page.watermark != "analytics:empty"
        || page.coverage != CoverageStateV1::Known
        || page.next_watermark.is_some()
    {
        return Err("observability empty-day source proof was not exact and empty".to_owned());
    }
    let ExecutionTopologyRollupBuildV1 {
        coverage,
        fragment: _,
        fragment_json,
    } = build_empty_execution_topology_daily_rollup(
        &claim.authorized_scope_ref,
        &horizon,
        now_micros().0,
    )
    .map_err(|error| format!("observability empty-day application build refused: {error}"))?;
    publish_rollup(
        db,
        RollupPublicationV1 {
            authorized_scope_ref: claim.authorized_scope_ref.clone(),
            day_start_seconds: claim.day_start_seconds,
            source_watermark: 0,
            coverage,
            dirty_claim: None,
            empty_day_claim: Some(claim.clone()),
            fragment_json,
        },
    )
    .await
}

async fn build_and_publish_claimed_day(
    db: &RegisteredGlobalDb,
    claim: &ObservabilityRollupDirtyDayClaimV1,
) -> Result<(), String> {
    let horizon = day_horizon(claim.day_start_seconds)?;
    let page = query_rollup_source_page(db, &claim.authorized_scope_ref, &horizon).await?;
    let expected_watermark = format!("analytics:{}", claim.source_watermark);
    if page.watermark != expected_watermark {
        return Err("observability rollup source watermark changed during rebuild".to_owned());
    }
    let observed_at_micros = now_micros().0;
    let ExecutionTopologyRollupBuildV1 {
        coverage,
        fragment: _,
        fragment_json,
    } = build_execution_topology_daily_rollup(
        &claim.authorized_scope_ref,
        &horizon,
        observed_at_micros,
        page,
    )
    .map_err(|error| format!("observability rollup application build refused: {error}"))?;
    publish_rollup(
        db,
        RollupPublicationV1 {
            authorized_scope_ref: claim.authorized_scope_ref.clone(),
            day_start_seconds: claim.day_start_seconds,
            source_watermark: claim.source_watermark,
            coverage,
            dirty_claim: Some(claim.clone()),
            empty_day_claim: None,
            fragment_json,
        },
    )
    .await
}

async fn query_rollup_source_page(
    db: &RegisteredGlobalDb,
    authorized_scope_ref: &str,
    horizon: &ObservabilityHorizonV1,
) -> Result<ObservabilityPageV1, String> {
    RegisteredObservabilityPortV1::new(db)
        .query(ObservabilityQueryV1 {
            authorized_scope_ref: authorized_scope_ref.to_owned(),
            event_kinds: EXECUTION_TOPOLOGY_EVENT_KINDS_V1
                .iter()
                .map(|kind| (*kind).to_owned())
                .chain(std::iter::once(TELEMETRY_DROP_EVENT_KIND.to_owned()))
                .collect(),
            horizon: horizon.clone(),
            after_watermark: None,
            limit: MAX_EXECUTION_TOPOLOGY_EVENTS_V1,
        })
        .await
        .map_err(|error| format!("observability rollup source query failed: {error}"))
}

fn day_horizon(day_start_seconds: i64) -> Result<ObservabilityHorizonV1, String> {
    let since_micros = day_start_seconds
        .checked_mul(MICROS_PER_SECOND)
        .ok_or_else(|| "observability rollup day horizon overflow".to_owned())?;
    let until_micros = day_start_seconds
        .checked_add(UTC_DAY_SECONDS)
        .and_then(|seconds| seconds.checked_mul(MICROS_PER_SECOND))
        .ok_or_else(|| "observability rollup day horizon overflow".to_owned())?;
    Ok(ObservabilityHorizonV1 {
        since_micros,
        until_micros,
    })
}

async fn publish_rollup(
    db: &RegisteredGlobalDb,
    publication: RollupPublicationV1,
) -> Result<(), String> {
    db.rebuild_observability_rollup(ObservabilityRollupRebuildV1 {
        authorized_scope_ref: publication.authorized_scope_ref,
        day_start_seconds: publication.day_start_seconds,
        projector_revision: EXECUTION_TOPOLOGY_PROJECTOR_REVISION_V1.to_owned(),
        source_watermark: publication.source_watermark,
        coverage: publication.coverage,
        idempotency_key: format!(
            "execution-topology:{}:{}:{}",
            publication.day_start_seconds,
            publication.source_watermark,
            EXECUTION_TOPOLOGY_PROJECTOR_REVISION_V1
        ),
        dirty_claim: publication.dirty_claim,
        empty_day_claim: publication.empty_day_claim,
        fragment_json: publication.fragment_json,
    })
    .await
    .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonicalizes_application_rollup_fragment_for_registered_storage() {
        let application_fragment = build_empty_execution_topology_daily_rollup(
            "test-scope",
            &ObservabilityHorizonV1 {
                since_micros: 0,
                until_micros: 86_400_000_000,
            },
            1,
        )
        .expect("empty application rollup fragment builds");
        let expected = serde_json::to_string(
            &serde_json::from_str::<serde_json::Value>(&application_fragment.fragment_json)
                .expect("application fragment is JSON"),
        )
        .expect("registered storage canonical form serializes");

        assert_eq!(application_fragment.fragment_json, expected);
    }
}
