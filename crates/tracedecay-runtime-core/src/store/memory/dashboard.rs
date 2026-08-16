//! Canonical project-memory dashboard read models.

use std::collections::{BTreeMap, BTreeSet};

use crate::db::DatabaseMemoryTransaction as Transaction;
use crate::db::engine::params;
use crate::memory::encoding::HolographicEncoder;
use crate::memory::entities::normalize_entity;

use tracedecay_domain::{FactId, FactLineageEventKindV1, FactLineageEventV1, FactOwnerV1};
use tracedecay_store::{
    FactReadControl, FactStoreError, FactStoreResult, ProjectMemoryDashboardEntityV1,
    ProjectMemoryDashboardFactDetailQueryV1, ProjectMemoryDashboardFactDetailV1,
    ProjectMemoryDashboardFactEntityLinkV1, ProjectMemoryDashboardFactSummaryV1,
    ProjectMemoryDashboardGrowthPointV1, ProjectMemoryDashboardMemoryOverviewQueryV1,
    ProjectMemoryDashboardMemoryOverviewV1, ProjectMemoryDashboardNamedCountV1,
    ProjectMemoryDashboardOplogEntryV1, ProjectMemoryDashboardOplogQueryV1,
    ProjectMemoryDashboardVectorPointV1, ProjectMemoryDashboardVectorPointsQueryV1,
    ProjectMemoryEntityIdV1, ProjectMemoryFactHistoryQueryV1, ProjectMemoryFactIdV1,
    ProjectMemoryFactListQueryV1, ProjectMemoryFactProjectionV1, ProjectMemoryFactV1,
};

use super::crud::{
    list_project_memory_facts_controlled_tx, project_memory_fact_history_controlled_tx,
};
use super::primitives::{
    OwnerKey, PROJECT_MEMORY_READ_OPERATION, ensure_project_memory_read_active, from_json,
    nonnegative_u64, row_i64, row_string, storage_error, storage_message,
};
use super::projection::{
    load_project_memory_projection_controlled_tx, load_project_memory_projections_controlled_tx,
};
use super::scoring::project_memory_holographic_error;

#[derive(Clone)]
struct EntityAggregate {
    name: String,
    fact_ids: BTreeSet<FactId>,
}

fn dashboard_fact_summary(
    projection: ProjectMemoryFactProjectionV1,
) -> ProjectMemoryDashboardFactSummaryV1 {
    ProjectMemoryDashboardFactSummaryV1 { fact: projection }
}

async fn dashboard_canonical_fact_count_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    read_control: &FactReadControl,
) -> FactStoreResult<u64> {
    ensure_project_memory_read_active(read_control)?;
    let key = OwnerKey::new(owner)?;
    let mut rows = transaction
        .query(
            "SELECT COUNT(*)
             FROM memory_v2_current_facts AS current_facts
             JOIN memory_v2_facts AS facts
               ON facts.fact_id = current_facts.fact_id
              AND facts.owner_kind = current_facts.owner_kind
              AND facts.project_id = current_facts.project_id
             WHERE current_facts.owner_kind = ?1
               AND current_facts.project_id = ?2
               AND facts.owner_json = ?3
               AND current_facts.payload_access = 'eligible'
               AND current_facts.active_assertion_id IS NOT NULL",
            params![key.kind, key.project_id.as_str(), key.json.as_str()],
        )
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_READ_OPERATION, error))?;
    ensure_project_memory_read_active(read_control)?;
    let row = rows
        .next()
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_READ_OPERATION, error))?
        .ok_or_else(|| storage_message(PROJECT_MEMORY_READ_OPERATION, "fact count is missing"))?;
    nonnegative_u64(
        row_i64(&row, 0, PROJECT_MEMORY_READ_OPERATION)?,
        "dashboard fact count",
    )
}

async fn dashboard_canonical_entity_count_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    read_control: &FactReadControl,
) -> FactStoreResult<u64> {
    ensure_project_memory_read_active(read_control)?;
    let key = OwnerKey::new(owner)?;
    let mut rows = transaction
        .query(
            "SELECT COUNT(DISTINCT lower(trim(CAST(entities.value AS TEXT))))
             FROM memory_v2_current_facts AS current_facts
             JOIN memory_v2_facts AS facts
               ON facts.fact_id = current_facts.fact_id
              AND facts.owner_kind = current_facts.owner_kind
              AND facts.project_id = current_facts.project_id
             JOIN memory_v2_assertion_payloads AS payloads
               ON payloads.assertion_id = current_facts.active_assertion_id
              AND payloads.fact_id = current_facts.fact_id
              AND payloads.owner_kind = current_facts.owner_kind
              AND payloads.project_id = current_facts.project_id
             JOIN json_each(payloads.payload_json, '$.entities') AS entities
             WHERE current_facts.owner_kind = ?1
               AND current_facts.project_id = ?2
               AND facts.owner_json = ?3
               AND current_facts.payload_access = 'eligible'
               AND current_facts.active_assertion_id IS NOT NULL
               AND trim(CAST(entities.value AS TEXT)) <> ''",
            params![key.kind, key.project_id.as_str(), key.json.as_str()],
        )
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_READ_OPERATION, error))?;
    ensure_project_memory_read_active(read_control)?;
    let row = rows
        .next()
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_READ_OPERATION, error))?
        .ok_or_else(|| storage_message(PROJECT_MEMORY_READ_OPERATION, "entity count is missing"))?;
    nonnegative_u64(
        row_i64(&row, 0, PROJECT_MEMORY_READ_OPERATION)?,
        "dashboard entity count",
    )
}

async fn dashboard_canonical_projections_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    limit: usize,
    read_control: &FactReadControl,
) -> FactStoreResult<Vec<ProjectMemoryFactProjectionV1>> {
    ensure_project_memory_read_active(read_control)?;
    let query = ProjectMemoryFactListQueryV1::new(owner.clone(), None, None, None, limit)?;
    let projections = list_project_memory_facts_controlled_tx(transaction, &query, read_control)
        .await?
        .facts()
        .to_vec();
    ensure_project_memory_read_active(read_control)?;
    Ok(projections)
}

fn dashboard_entity_aggregates(
    facts: &[ProjectMemoryFactProjectionV1],
    read_control: &FactReadControl,
) -> FactStoreResult<BTreeMap<String, EntityAggregate>> {
    let mut entities = BTreeMap::<String, EntityAggregate>::new();
    for projection in facts {
        ensure_project_memory_read_active(read_control)?;
        let ProjectMemoryFactProjectionV1::Available(fact) = projection else {
            continue;
        };
        for entity in fact.entities() {
            ensure_project_memory_read_active(read_control)?;
            let normalized = normalize_entity(entity);
            if normalized.is_empty() {
                continue;
            }
            let key = normalized.to_ascii_lowercase();
            let aggregate = entities.entry(key).or_insert_with(|| EntityAggregate {
                name: normalized,
                fact_ids: BTreeSet::new(),
            });
            aggregate.fact_ids.insert(fact.fact_id().clone());
        }
    }
    Ok(entities)
}

fn dashboard_entities(
    owner: &FactOwnerV1,
    aggregates: &BTreeMap<String, EntityAggregate>,
    limit: usize,
    read_control: &FactReadControl,
) -> FactStoreResult<Vec<ProjectMemoryDashboardEntityV1>> {
    ensure_project_memory_read_active(read_control)?;
    let mut entities = aggregates.values().cloned().collect::<Vec<_>>();
    entities.sort_by(|left, right| {
        right
            .fact_ids
            .len()
            .cmp(&left.fact_ids.len())
            .then_with(|| left.name.cmp(&right.name))
    });
    entities.truncate(limit);
    let mut projected = Vec::with_capacity(entities.len());
    for entity in entities {
        ensure_project_memory_read_active(read_control)?;
        projected.push(ProjectMemoryDashboardEntityV1::new(
            ProjectMemoryEntityIdV1::new(owner.clone(), entity.name.clone())?,
            entity.name,
            entity.fact_ids.len() as u64,
        )?);
    }
    Ok(projected)
}

fn dashboard_fact_entity_links(
    owner: &FactOwnerV1,
    aggregates: &BTreeMap<String, EntityAggregate>,
    included_entities: &[ProjectMemoryDashboardEntityV1],
    limit: usize,
    read_control: &FactReadControl,
) -> FactStoreResult<Vec<ProjectMemoryDashboardFactEntityLinkV1>> {
    ensure_project_memory_read_active(read_control)?;
    let included = included_entities
        .iter()
        .map(|entity| entity.target.entity().to_ascii_lowercase())
        .collect::<BTreeSet<_>>();
    let mut links = Vec::new();
    for (key, entity) in aggregates {
        ensure_project_memory_read_active(read_control)?;
        if !included.contains(key) {
            continue;
        }
        for fact_id in &entity.fact_ids {
            ensure_project_memory_read_active(read_control)?;
            links.push(ProjectMemoryDashboardFactEntityLinkV1::new(
                ProjectMemoryFactIdV1::new(owner.clone(), fact_id.clone())?,
                ProjectMemoryEntityIdV1::new(owner.clone(), entity.name.clone())?,
            )?);
            if links.len() == limit {
                return Ok(links);
            }
        }
    }
    Ok(links)
}

async fn dashboard_category_counts_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    read_control: &FactReadControl,
) -> FactStoreResult<Vec<ProjectMemoryDashboardNamedCountV1>> {
    ensure_project_memory_read_active(read_control)?;
    let key = OwnerKey::new(owner)?;
    let mut rows = transaction
        .query(
            "SELECT json_extract(payloads.payload_json, '$.category'), COUNT(*)
             FROM memory_v2_current_facts AS current_facts
             JOIN memory_v2_facts AS facts
               ON facts.fact_id = current_facts.fact_id
              AND facts.owner_kind = current_facts.owner_kind
              AND facts.project_id = current_facts.project_id
             JOIN memory_v2_assertion_payloads AS payloads
               ON payloads.assertion_id = current_facts.active_assertion_id
              AND payloads.fact_id = current_facts.fact_id
              AND payloads.owner_kind = current_facts.owner_kind
              AND payloads.project_id = current_facts.project_id
             WHERE current_facts.owner_kind = ?1
               AND current_facts.project_id = ?2
               AND facts.owner_json = ?3
               AND current_facts.payload_access = 'eligible'
               AND current_facts.active_assertion_id IS NOT NULL
             GROUP BY json_extract(payloads.payload_json, '$.category')
             ORDER BY COUNT(*) DESC, json_extract(payloads.payload_json, '$.category') ASC
             LIMIT 128",
            params![key.kind, key.project_id.as_str(), key.json.as_str()],
        )
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_READ_OPERATION, error))?;
    let mut counts = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_READ_OPERATION, error))?
    {
        ensure_project_memory_read_active(read_control)?;
        counts.push(ProjectMemoryDashboardNamedCountV1::new(
            row_string(&row, 0, PROJECT_MEMORY_READ_OPERATION)?,
            nonnegative_u64(
                row_i64(&row, 1, PROJECT_MEMORY_READ_OPERATION)?,
                "dashboard category count",
            )?,
        )?);
    }
    Ok(counts)
}

async fn dashboard_trust_histogram_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    read_control: &FactReadControl,
) -> FactStoreResult<Vec<ProjectMemoryDashboardNamedCountV1>> {
    ensure_project_memory_read_active(read_control)?;
    let key = OwnerKey::new(owner)?;
    let mut rows = transaction
        .query(
            "SELECT CASE
                        WHEN current_facts.trust_score < 0.0 THEN 0
                        WHEN current_facts.trust_score >= 1.0 THEN 9
                        ELSE CAST(current_facts.trust_score * 10.0 AS INTEGER)
                    END AS bucket,
                    COUNT(*)
             FROM memory_v2_current_facts AS current_facts
             JOIN memory_v2_facts AS facts
               ON facts.fact_id = current_facts.fact_id
              AND facts.owner_kind = current_facts.owner_kind
              AND facts.project_id = current_facts.project_id
             WHERE current_facts.owner_kind = ?1
               AND current_facts.project_id = ?2
               AND facts.owner_json = ?3
               AND current_facts.payload_access = 'eligible'
               AND current_facts.active_assertion_id IS NOT NULL
             GROUP BY bucket
             ORDER BY bucket ASC
             LIMIT 10",
            params![key.kind, key.project_id.as_str(), key.json.as_str()],
        )
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_READ_OPERATION, error))?;
    let mut counts = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_READ_OPERATION, error))?
    {
        ensure_project_memory_read_active(read_control)?;
        let bucket = row_i64(&row, 0, PROJECT_MEMORY_READ_OPERATION)?;
        counts.push(ProjectMemoryDashboardNamedCountV1::new(
            format!("trust-{bucket}"),
            nonnegative_u64(
                row_i64(&row, 1, PROJECT_MEMORY_READ_OPERATION)?,
                "dashboard trust count",
            )?,
        )?);
    }
    Ok(counts)
}

async fn dashboard_growth_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    read_control: &FactReadControl,
) -> FactStoreResult<Vec<ProjectMemoryDashboardGrowthPointV1>> {
    ensure_project_memory_read_active(read_control)?;
    let key = OwnerKey::new(owner)?;
    let mut rows = transaction
        .query(
            "SELECT strftime('%Y-%m', facts.created_at / 1000000, 'unixepoch') AS period,
                    COUNT(*)
             FROM memory_v2_facts AS facts
             JOIN memory_v2_current_facts AS current_facts
               ON current_facts.fact_id = facts.fact_id
              AND current_facts.owner_kind = facts.owner_kind
              AND current_facts.project_id = facts.project_id
             WHERE facts.owner_kind = ?1
               AND facts.project_id = ?2
               AND facts.owner_json = ?3
               AND current_facts.payload_access = 'eligible'
               AND current_facts.active_assertion_id IS NOT NULL
             GROUP BY period
             ORDER BY period ASC
             LIMIT 1000",
            params![key.kind, key.project_id.as_str(), key.json.as_str()],
        )
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_READ_OPERATION, error))?;
    let mut cumulative = 0_u64;
    let mut growth = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_READ_OPERATION, error))?
    {
        ensure_project_memory_read_active(read_control)?;
        let count = nonnegative_u64(
            row_i64(&row, 1, PROJECT_MEMORY_READ_OPERATION)?,
            "dashboard growth count",
        )?;
        cumulative = cumulative.saturating_add(count);
        growth.push(ProjectMemoryDashboardGrowthPointV1::new(
            row_string(&row, 0, PROJECT_MEMORY_READ_OPERATION)?,
            count,
            cumulative,
        )?);
    }
    Ok(growth)
}

pub(super) async fn dashboard_project_memory_overview_tx(
    transaction: &Transaction<'_>,
    query: &ProjectMemoryDashboardMemoryOverviewQueryV1,
    read_control: &FactReadControl,
) -> FactStoreResult<ProjectMemoryDashboardMemoryOverviewV1> {
    ensure_project_memory_read_active(read_control)?;
    let fact_count =
        dashboard_canonical_fact_count_tx(transaction, query.owner(), read_control).await?;
    let graph_facts = dashboard_canonical_projections_tx(
        transaction,
        query.owner(),
        query.graph_limit(),
        read_control,
    )
    .await?;
    let projections = dashboard_canonical_projections_tx(
        transaction,
        query.owner(),
        query.fact_limit(),
        read_control,
    )
    .await?;
    let mut facts = Vec::with_capacity(projections.len());
    for projection in projections {
        ensure_project_memory_read_active(read_control)?;
        facts.push(dashboard_fact_summary(projection));
    }
    let aggregates = dashboard_entity_aggregates(&graph_facts, read_control)?;
    let entity_count =
        dashboard_canonical_entity_count_tx(transaction, query.owner(), read_control).await?;
    let entities = dashboard_entities(
        query.owner(),
        &aggregates,
        query.graph_limit(),
        read_control,
    )?;
    let fact_entity_links = dashboard_fact_entity_links(
        query.owner(),
        &aggregates,
        &entities,
        query.graph_limit(),
        read_control,
    )?;
    let categories = dashboard_category_counts_tx(transaction, query.owner(), read_control).await?;
    let trust_histogram =
        dashboard_trust_histogram_tx(transaction, query.owner(), read_control).await?;
    let growth = dashboard_growth_tx(transaction, query.owner(), read_control).await?;
    ensure_project_memory_read_active(read_control)?;

    ProjectMemoryDashboardMemoryOverviewV1::new(
        query.owner().clone(),
        fact_count,
        entity_count,
        facts,
        entities,
        fact_entity_links,
        categories,
        trust_histogram,
        growth,
    )
}

fn dashboard_entities_for_fact(
    fact: &ProjectMemoryFactV1,
    read_control: &FactReadControl,
) -> FactStoreResult<Vec<ProjectMemoryDashboardEntityV1>> {
    let mut seen = BTreeSet::new();
    let mut entities = Vec::new();
    for entity in fact.entities() {
        ensure_project_memory_read_active(read_control)?;
        let entity = normalize_entity(entity);
        if entity.is_empty() || !seen.insert(entity.to_ascii_lowercase()) {
            continue;
        }
        entities.push(ProjectMemoryDashboardEntityV1::new(
            ProjectMemoryEntityIdV1::new(fact.owner().clone(), entity.clone())?,
            entity,
            1,
        )?);
    }
    Ok(entities)
}

pub(super) async fn dashboard_project_memory_fact_detail_tx(
    transaction: &Transaction<'_>,
    query: &ProjectMemoryDashboardFactDetailQueryV1,
    read_control: &FactReadControl,
) -> FactStoreResult<Option<ProjectMemoryDashboardFactDetailV1>> {
    ensure_project_memory_read_active(read_control)?;
    let target = query.target();
    let fact = load_project_memory_projection_controlled_tx(
        transaction,
        target.owner(),
        target.fact_id(),
        read_control,
    )
    .await?;
    ensure_project_memory_read_active(read_control)?;
    let Some(fact) = fact else {
        return Ok(None);
    };
    let entities = match &fact {
        ProjectMemoryFactProjectionV1::Available(fact) => {
            dashboard_entities_for_fact(fact, read_control)?
        }
        ProjectMemoryFactProjectionV1::Unavailable(_) => Vec::new(),
    };
    ensure_project_memory_read_active(read_control)?;
    let history = project_memory_fact_history_controlled_tx(
        transaction,
        &ProjectMemoryFactHistoryQueryV1::new(target.clone(), None, 128)?,
        read_control,
    )
    .await?;
    ensure_project_memory_read_active(read_control)?;
    ProjectMemoryDashboardFactDetailV1::new(fact, entities, Some(history)).map(Some)
}

async fn dashboard_vector_fact_ids_tx(
    transaction: &Transaction<'_>,
    query: &ProjectMemoryDashboardVectorPointsQueryV1,
    read_control: &FactReadControl,
) -> FactStoreResult<Vec<FactId>> {
    ensure_project_memory_read_active(read_control)?;
    let key = OwnerKey::new(query.owner())?;
    let limit = i64::try_from(query.limit()).map_err(|_| FactStoreError::InvalidQueryLimit {
        limit: query.limit(),
        max: usize::MAX,
    })?;
    let mut rows = match query.search() {
        Some(search) => {
            transaction
                .query(
                    "SELECT current_facts.fact_id
                     FROM memory_v2_current_facts AS current_facts
                     JOIN memory_v2_facts AS facts
                       ON facts.fact_id = current_facts.fact_id
                      AND facts.owner_kind = current_facts.owner_kind
                      AND facts.project_id = current_facts.project_id
                     JOIN memory_v2_assertion_payloads AS payloads
                       ON payloads.assertion_id = current_facts.active_assertion_id
                      AND payloads.fact_id = current_facts.fact_id
                      AND payloads.owner_kind = current_facts.owner_kind
                      AND payloads.project_id = current_facts.project_id
                     WHERE current_facts.owner_kind = ?1
                       AND current_facts.project_id = ?2
                       AND facts.owner_json = ?3
                       AND current_facts.payload_access = 'eligible'
                       AND current_facts.active_assertion_id IS NOT NULL
                       AND instr(lower(payloads.payload_json), lower(?4)) > 0
                     ORDER BY current_facts.updated_at DESC, current_facts.fact_id ASC
                     LIMIT ?5",
                    params![
                        key.kind,
                        key.project_id.as_str(),
                        key.json.as_str(),
                        search,
                        limit,
                    ],
                )
                .await
        }
        None => {
            transaction
                .query(
                    "SELECT current_facts.fact_id
                     FROM memory_v2_current_facts AS current_facts
                     JOIN memory_v2_facts AS facts
                       ON facts.fact_id = current_facts.fact_id
                      AND facts.owner_kind = current_facts.owner_kind
                      AND facts.project_id = current_facts.project_id
                     WHERE current_facts.owner_kind = ?1
                       AND current_facts.project_id = ?2
                       AND facts.owner_json = ?3
                       AND current_facts.payload_access = 'eligible'
                       AND current_facts.active_assertion_id IS NOT NULL
                     ORDER BY current_facts.updated_at DESC, current_facts.fact_id ASC
                     LIMIT ?4",
                    params![key.kind, key.project_id.as_str(), key.json.as_str(), limit,],
                )
                .await
        }
    }
    .map_err(|error| storage_error(PROJECT_MEMORY_READ_OPERATION, error))?;
    let mut fact_ids = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_READ_OPERATION, error))?
    {
        ensure_project_memory_read_active(read_control)?;
        fact_ids.push(
            FactId::new(row_string(&row, 0, PROJECT_MEMORY_READ_OPERATION)?)
                .map_err(FactStoreError::from)?,
        );
    }
    Ok(fact_ids)
}

pub(super) async fn dashboard_project_memory_vector_points_tx(
    transaction: &Transaction<'_>,
    query: &ProjectMemoryDashboardVectorPointsQueryV1,
    read_control: &FactReadControl,
) -> FactStoreResult<Vec<ProjectMemoryDashboardVectorPointV1>> {
    let fact_ids = dashboard_vector_fact_ids_tx(transaction, query, read_control).await?;
    ensure_project_memory_read_active(read_control)?;
    let facts = load_project_memory_projections_controlled_tx(
        transaction,
        query.owner(),
        &fact_ids,
        read_control,
    )
    .await?;
    ensure_project_memory_read_active(read_control)?;
    let encoder = HolographicEncoder::new();
    facts
        .into_iter()
        .map(|projection| {
            ensure_project_memory_read_active(read_control)?;
            let (vector, entity_count) = match &projection {
                ProjectMemoryFactProjectionV1::Available(fact) => {
                    let entities = fact.entities();
                    (
                        Some(
                            encoder
                                .encode_fact(fact.content(), entities)
                                .map_err(project_memory_holographic_error)?,
                        ),
                        entities
                            .iter()
                            .map(|entity| normalize_entity(entity).to_ascii_lowercase())
                            .filter(|entity| !entity.is_empty())
                            .collect::<BTreeSet<_>>()
                            .len() as u64,
                    )
                }
                ProjectMemoryFactProjectionV1::Unavailable(_) => (None, 0),
            };
            ensure_project_memory_read_active(read_control)?;
            ProjectMemoryDashboardVectorPointV1::new(
                dashboard_fact_summary(projection),
                vector,
                entity_count,
                0,
            )
        })
        .collect::<FactStoreResult<Vec<_>>>()
}

fn dashboard_oplog_operation(kind: &FactLineageEventKindV1) -> &'static str {
    match kind {
        FactLineageEventKindV1::AssertionRecorded { .. } => "assertion_recorded",
        FactLineageEventKindV1::TrustChanged { .. } => "trust_changed",
        FactLineageEventKindV1::Curated { .. } => "curated",
        FactLineageEventKindV1::PayloadAccessChanged { .. } => "payload_access_changed",
    }
}

pub(super) async fn dashboard_project_memory_oplog_tx(
    transaction: &Transaction<'_>,
    query: &ProjectMemoryDashboardOplogQueryV1,
    read_control: &FactReadControl,
) -> FactStoreResult<Vec<ProjectMemoryDashboardOplogEntryV1>> {
    ensure_project_memory_read_active(read_control)?;
    let key = OwnerKey::new(query.owner())?;
    let limit = i64::try_from(query.limit()).map_err(|_| FactStoreError::InvalidQueryLimit {
        limit: query.limit(),
        max: usize::MAX,
    })?;
    let mut rows = transaction
        .query(
            "SELECT events.event_sequence, events.event_json
             FROM memory_v2_lineage_events AS events
             JOIN memory_v2_facts AS facts
               ON facts.fact_id = events.fact_id
              AND facts.owner_kind = events.owner_kind
              AND facts.project_id = events.project_id
             WHERE events.owner_kind = ?1
               AND events.project_id = ?2
               AND facts.owner_json = ?3
             ORDER BY events.event_sequence DESC
             LIMIT ?4",
            params![key.kind, key.project_id.as_str(), key.json.as_str(), limit],
        )
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_READ_OPERATION, error))?;
    let mut entries = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_READ_OPERATION, error))?
    {
        ensure_project_memory_read_active(read_control)?;
        let event = from_json::<FactLineageEventV1>(
            &row_string(&row, 1, PROJECT_MEMORY_READ_OPERATION)?,
            PROJECT_MEMORY_READ_OPERATION,
        )?;
        entries.push(ProjectMemoryDashboardOplogEntryV1::new(
            row_i64(&row, 0, PROJECT_MEMORY_READ_OPERATION)?,
            event.occurred_at(),
            dashboard_oplog_operation(event.kind()).to_owned(),
            Some(ProjectMemoryFactIdV1::new(
                query.owner().clone(),
                event.fact_id().clone(),
            )?),
        )?);
    }
    ensure_project_memory_read_active(read_control)?;
    Ok(entries)
}
