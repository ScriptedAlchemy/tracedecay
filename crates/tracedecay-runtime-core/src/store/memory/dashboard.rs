//! Dashboard compatibility read models (overview, banks, vector points, growth, oplog).

use std::collections::BTreeSet;

use crate::memory::encoding::HolographicEncoder;

use crate::db::DatabaseMemoryTransaction as Transaction;
use crate::db::engine::params;
use serde_json::Value;

use tracedecay_domain::{FactId, FactOwnerV1, UtcMicros};
use tracedecay_store::{
    FactStoreError, LegacyFactQuery, ProjectMemoryDashboardFactDetailQueryV1,
    ProjectMemoryDashboardFactDetailV1, ProjectMemoryDashboardMemoryOverviewQueryV1,
    ProjectMemoryDashboardMemoryOverviewV1, ProjectMemoryDashboardOplogEntryV1,
    ProjectMemoryDashboardOplogQueryV1, ProjectMemoryDashboardVectorPointV1,
    ProjectMemoryDashboardVectorPointsQueryV1, ProjectMemoryFactHistoryQueryV1,
    ProjectMemoryFactIdV1, ProjectMemoryFactProjectionV1, ProjectMemoryFactTargetV1,
    ProjectMemoryResult,
};

use super::crud::project_memory_fact_history_tx;
use super::primitives::{
    OwnerKey, PROJECT_MEMORY_READ_OPERATION, compatibility_source_store_id, from_json,
    nonnegative_u64, row_i64, row_optional_i64, row_optional_string, row_string, storage_error,
    storage_message,
};
use super::projection::{
    load_project_memory_projection_tx, load_project_memory_projections_tx,
    project_memory_legacy_mapping_tx, resolve_project_memory_target_tx,
};

// Dashboard reads deliberately start from the immutable owner-bound V1 mapping.
// The legacy tables remain a compatibility projection, never an alternate fact
// authority or a source for ownerless rows.
async fn dashboard_project_memory_fact_summaries_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    limit: usize,
) -> ProjectMemoryResult<Vec<tracedecay_store::ProjectMemoryDashboardFactSummaryV1>> {
    let key = OwnerKey::new(owner)?;
    let source_store_id = compatibility_source_store_id()?;
    let limit = i64::try_from(limit).map_err(|_| FactStoreError::InvalidQueryLimit {
        limit,
        max: usize::MAX,
    })?;
    let mut rows = transaction
        .query(
            "SELECT mappings.fact_id, legacy_facts.hrr_vector IS NOT NULL
             FROM memory_facts AS legacy_facts
             JOIN memory_v2_facts AS mappings
               ON mappings.fact_id = legacy_facts.canonical_fact_id
             WHERE mappings.owner_kind = ?1
               AND mappings.project_id = ?2
               AND mappings.owner_json = ?3
               AND ?4 = 'legacy-memory-v1'
             ORDER BY legacy_facts.trust_score DESC,
                      legacy_facts.updated_at DESC,
                      mappings.fact_id ASC
             LIMIT ?5",
            params![
                key.kind,
                key.project_id.as_str(),
                key.json.as_str(),
                source_store_id.as_str(),
                limit,
            ],
        )
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_READ_OPERATION, error))?;
    let mut mapped = Vec::with_capacity(usize::try_from(limit).unwrap_or_default());
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_READ_OPERATION, error))?
    {
        let fact_id = FactId::new(row_string(&row, 0, PROJECT_MEMORY_READ_OPERATION)?)
            .map_err(FactStoreError::from)?;
        mapped.push((
            fact_id,
            row_i64(&row, 1, PROJECT_MEMORY_READ_OPERATION)? != 0,
        ));
    }
    drop(rows);
    let fact_ids = mapped
        .iter()
        .map(|(fact_id, _)| fact_id.clone())
        .collect::<Vec<_>>();
    let projections = load_project_memory_projections_tx(transaction, owner, &fact_ids).await?;
    if projections.len() != mapped.len() {
        return Err(storage_message(
            PROJECT_MEMORY_READ_OPERATION,
            "owner-bound dashboard mapping has no canonical fact projection",
        )
        .into());
    }
    Ok(mapped
        .into_iter()
        .zip(projections)
        .map(
            |((_, has_hrr_vector), fact)| tracedecay_store::ProjectMemoryDashboardFactSummaryV1 {
                has_hrr_vector: has_hrr_vector
                    && matches!(&fact, ProjectMemoryFactProjectionV1::Available(_)),
                fact,
            },
        )
        .collect())
}

async fn dashboard_project_memory_entities_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    limit: usize,
) -> ProjectMemoryResult<Vec<tracedecay_store::ProjectMemoryDashboardEntityV1>> {
    let key = OwnerKey::new(owner)?;
    let source_store_id = compatibility_source_store_id()?;
    let limit = i64::try_from(limit).map_err(|_| FactStoreError::InvalidQueryLimit {
        limit,
        max: usize::MAX,
    })?;
    let mut rows = transaction
        .query(
            "SELECT entities.entity_id, entities.name, entities.entity_type,
                    entities.aliases, entities.created_at,
                    COUNT(DISTINCT legacy_facts.fact_id)
             FROM memory_facts AS legacy_facts
             JOIN memory_v2_facts AS mappings
               ON mappings.fact_id = legacy_facts.canonical_fact_id
             JOIN memory_fact_entities AS relations
               ON relations.fact_id = legacy_facts.fact_id
             JOIN memory_entities AS entities
               ON entities.entity_id = relations.entity_id
             WHERE mappings.owner_kind = ?1
               AND mappings.project_id = ?2
               AND mappings.owner_json = ?3
               AND ?4 = 'legacy-memory-v1'
             GROUP BY entities.entity_id, entities.name, entities.entity_type,
                      entities.aliases, entities.created_at
             ORDER BY COUNT(DISTINCT legacy_facts.fact_id) DESC,
                      entities.name ASC, entities.entity_id ASC
             LIMIT ?5",
            params![
                key.kind,
                key.project_id.as_str(),
                key.json.as_str(),
                source_store_id.as_str(),
                limit,
            ],
        )
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_READ_OPERATION, error))?;
    let mut entities = Vec::with_capacity(usize::try_from(limit).unwrap_or_default());
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_READ_OPERATION, error))?
    {
        let aliases = from_json::<Vec<String>>(
            &row_string(&row, 3, PROJECT_MEMORY_READ_OPERATION)?,
            PROJECT_MEMORY_READ_OPERATION,
        )?;
        entities.push(tracedecay_store::ProjectMemoryDashboardEntityV1::new(
            tracedecay_store::ProjectMemoryLegacyEntityTargetV1::new(
                owner.clone(),
                row_i64(&row, 0, PROJECT_MEMORY_READ_OPERATION)?,
            )?,
            row_string(&row, 1, PROJECT_MEMORY_READ_OPERATION)?,
            row_string(&row, 2, PROJECT_MEMORY_READ_OPERATION)?,
            aliases,
            UtcMicros(row_i64(&row, 4, PROJECT_MEMORY_READ_OPERATION)?),
            nonnegative_u64(
                row_i64(&row, 5, PROJECT_MEMORY_READ_OPERATION)?,
                "dashboard entity fact count",
            )?,
        )?);
    }
    Ok(entities)
}

async fn dashboard_project_memory_fact_entity_links_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    fact_ids: &BTreeSet<String>,
    entity_ids: &BTreeSet<i64>,
    limit: usize,
) -> ProjectMemoryResult<Vec<tracedecay_store::ProjectMemoryDashboardFactEntityLinkV1>> {
    if fact_ids.is_empty() || entity_ids.is_empty() || limit == 0 {
        return Ok(Vec::new());
    }
    let key = OwnerKey::new(owner)?;
    let source_store_id = compatibility_source_store_id()?;
    let fetch_limit = i64::try_from(limit).map_err(|_| FactStoreError::InvalidQueryLimit {
        limit,
        max: usize::MAX,
    })?;
    let mut rows = transaction
        .query(
            "SELECT mappings.fact_id, relations.entity_id
             FROM memory_facts AS legacy_facts
             JOIN memory_v2_facts AS mappings
               ON mappings.fact_id = legacy_facts.canonical_fact_id
             JOIN memory_fact_entities AS relations
               ON relations.fact_id = legacy_facts.fact_id
             WHERE mappings.owner_kind = ?1
               AND mappings.project_id = ?2
               AND mappings.owner_json = ?3
               AND ?4 = 'legacy-memory-v1'
             ORDER BY legacy_facts.trust_score DESC,
                      legacy_facts.updated_at DESC,
                      mappings.fact_id ASC, relations.entity_id ASC
             LIMIT ?5",
            params![
                key.kind,
                key.project_id.as_str(),
                key.json.as_str(),
                source_store_id.as_str(),
                fetch_limit,
            ],
        )
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_READ_OPERATION, error))?;
    let mut links = Vec::with_capacity(usize::try_from(fetch_limit).unwrap_or_default());
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_READ_OPERATION, error))?
    {
        let fact_id = row_string(&row, 0, PROJECT_MEMORY_READ_OPERATION)?;
        let entity_id = row_i64(&row, 1, PROJECT_MEMORY_READ_OPERATION)?;
        if !fact_ids.contains(&fact_id) || !entity_ids.contains(&entity_id) {
            continue;
        }
        let fact_id = FactId::new(fact_id).map_err(FactStoreError::from)?;
        links.push(
            tracedecay_store::ProjectMemoryDashboardFactEntityLinkV1::new(
                ProjectMemoryFactTargetV1::Canonical(ProjectMemoryFactIdV1::new(
                    owner.clone(),
                    fact_id,
                )?),
                tracedecay_store::ProjectMemoryLegacyEntityTargetV1::new(owner.clone(), entity_id)?,
            )?,
        );
    }
    Ok(links)
}

async fn dashboard_project_memory_owner_count_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    entity_count: bool,
) -> ProjectMemoryResult<u64> {
    let key = OwnerKey::new(owner)?;
    let source_store_id = compatibility_source_store_id()?;
    let sql = if entity_count {
        "SELECT COUNT(DISTINCT relations.entity_id)
         FROM memory_facts AS legacy_facts
         JOIN memory_v2_facts AS mappings
           ON mappings.fact_id = legacy_facts.canonical_fact_id
         JOIN memory_fact_entities AS relations
           ON relations.fact_id = legacy_facts.fact_id
         WHERE mappings.owner_kind = ?1
           AND mappings.project_id = ?2
           AND mappings.owner_json = ?3
           AND ?4 = 'legacy-memory-v1'"
    } else {
        "SELECT COUNT(*)
         FROM memory_facts AS legacy_facts
         JOIN memory_v2_facts AS mappings
           ON mappings.fact_id = legacy_facts.canonical_fact_id
         WHERE mappings.owner_kind = ?1
           AND mappings.project_id = ?2
           AND mappings.owner_json = ?3
           AND ?4 = 'legacy-memory-v1'"
    };
    let mut rows = transaction
        .query(
            sql,
            params![
                key.kind,
                key.project_id.as_str(),
                key.json.as_str(),
                source_store_id.as_str(),
            ],
        )
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_READ_OPERATION, error))?;
    let row = rows
        .next()
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_READ_OPERATION, error))?
        .ok_or_else(|| {
            storage_message(
                PROJECT_MEMORY_READ_OPERATION,
                "compatibility dashboard owner count is missing",
            )
        })?;
    nonnegative_u64(
        row_i64(&row, 0, PROJECT_MEMORY_READ_OPERATION)?,
        "compatibility dashboard owner count",
    )
    .map_err(Into::into)
}

#[derive(Clone, Copy)]
enum ProjectMemoryDashboardNamedCountKind {
    Category,
    EntityType,
    TrustBucket,
}

async fn dashboard_project_memory_named_counts_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    kind: ProjectMemoryDashboardNamedCountKind,
) -> ProjectMemoryResult<Vec<tracedecay_store::ProjectMemoryDashboardNamedCountV1>> {
    let key = OwnerKey::new(owner)?;
    let source_store_id = compatibility_source_store_id()?;
    let (sql, limit) = match kind {
        ProjectMemoryDashboardNamedCountKind::Category => (
            "SELECT legacy_facts.category, COUNT(*)
             FROM memory_facts AS legacy_facts
             JOIN memory_v2_facts AS mappings
               ON mappings.fact_id = legacy_facts.canonical_fact_id
             WHERE mappings.owner_kind = ?1
               AND mappings.project_id = ?2
               AND mappings.owner_json = ?3
               AND ?4 = 'legacy-memory-v1'
             GROUP BY legacy_facts.category
             ORDER BY COUNT(*) DESC, legacy_facts.category ASC
             LIMIT 128",
            128,
        ),
        ProjectMemoryDashboardNamedCountKind::EntityType => (
            "SELECT entities.entity_type, COUNT(DISTINCT entities.entity_id)
             FROM memory_facts AS legacy_facts
             JOIN memory_v2_facts AS mappings
               ON mappings.fact_id = legacy_facts.canonical_fact_id
             JOIN memory_fact_entities AS relations
               ON relations.fact_id = legacy_facts.fact_id
             JOIN memory_entities AS entities
               ON entities.entity_id = relations.entity_id
             WHERE mappings.owner_kind = ?1
               AND mappings.project_id = ?2
               AND mappings.owner_json = ?3
               AND ?4 = 'legacy-memory-v1'
             GROUP BY entities.entity_type
             ORDER BY COUNT(DISTINCT entities.entity_id) DESC, entities.entity_type ASC
             LIMIT 128",
            128,
        ),
        ProjectMemoryDashboardNamedCountKind::TrustBucket => (
            "SELECT CASE
                        WHEN legacy_facts.trust_score < 0.0 THEN 0
                        WHEN legacy_facts.trust_score >= 1.0 THEN 9
                        ELSE CAST(legacy_facts.trust_score * 10.0 AS INTEGER)
                    END AS bucket,
                    COUNT(*)
             FROM memory_facts AS legacy_facts
             JOIN memory_v2_facts AS mappings
               ON mappings.fact_id = legacy_facts.canonical_fact_id
             WHERE mappings.owner_kind = ?1
               AND mappings.project_id = ?2
               AND mappings.owner_json = ?3
               AND ?4 = 'legacy-memory-v1'
             GROUP BY bucket
             ORDER BY bucket ASC
             LIMIT 10",
            10,
        ),
    };
    let mut rows = transaction
        .query(
            sql,
            params![
                key.kind,
                key.project_id.as_str(),
                key.json.as_str(),
                source_store_id.as_str(),
            ],
        )
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_READ_OPERATION, error))?;
    let mut counts = Vec::with_capacity(limit);
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_READ_OPERATION, error))?
    {
        let name = match kind {
            ProjectMemoryDashboardNamedCountKind::TrustBucket => {
                format!("trust-{}", row_i64(&row, 0, PROJECT_MEMORY_READ_OPERATION)?)
            }
            ProjectMemoryDashboardNamedCountKind::Category
            | ProjectMemoryDashboardNamedCountKind::EntityType => {
                row_string(&row, 0, PROJECT_MEMORY_READ_OPERATION)?
            }
        };
        counts.push(tracedecay_store::ProjectMemoryDashboardNamedCountV1::new(
            name,
            nonnegative_u64(
                row_i64(&row, 1, PROJECT_MEMORY_READ_OPERATION)?,
                "compatibility dashboard named count",
            )?,
        )?);
    }
    Ok(counts)
}

fn dashboard_compatibility_dimension(dimension: Option<i64>) -> ProjectMemoryResult<Option<u32>> {
    dimension
        .map(|value| {
            let value = u32::try_from(value).map_err(|_| {
                storage_message(
                    PROJECT_MEMORY_READ_OPERATION,
                    "dashboard HRR dimension is outside u32 range",
                )
            })?;
            if value == 0 {
                return Err(storage_message(
                    PROJECT_MEMORY_READ_OPERATION,
                    "dashboard HRR dimension must be positive",
                ));
            }
            Ok(value)
        })
        .transpose()
        .map_err(Into::into)
}

async fn dashboard_compatibility_hrr_coverage_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
) -> ProjectMemoryResult<Vec<tracedecay_store::ProjectMemoryDashboardHrrCoverageV1>> {
    let key = OwnerKey::new(owner)?;
    let source_store_id = compatibility_source_store_id()?;
    let mut rows = transaction
        .query(
            "SELECT legacy_facts.category,
                    COUNT(*),
                    COALESCE(SUM(CASE WHEN legacy_facts.hrr_vector IS NOT NULL THEN 1 ELSE 0 END), 0),
                    MAX(CASE WHEN banks.bank_name IS NULL THEN 0 ELSE 1 END),
                    MAX(banks.hrr_dim),
                    MAX(banks.updated_at),
                    MAX(CASE WHEN dirty.bank_name IS NULL THEN 0 ELSE 1 END)
             FROM memory_facts AS legacy_facts
             JOIN memory_v2_facts AS mappings
               ON mappings.fact_id = legacy_facts.canonical_fact_id
             LEFT JOIN memory_v2_banks AS banks
               ON banks.owner_kind = mappings.owner_kind
              AND banks.project_id = mappings.project_id
              AND banks.owner_json = mappings.owner_json
              AND banks.bank_name = legacy_facts.category
             LEFT JOIN memory_v2_bank_dirty AS dirty
               ON dirty.owner_kind = mappings.owner_kind
              AND dirty.project_id = mappings.project_id
              AND dirty.owner_json = mappings.owner_json
              AND dirty.bank_name = legacy_facts.category
             WHERE mappings.owner_kind = ?1
               AND mappings.project_id = ?2
               AND mappings.owner_json = ?3
               AND ?4 = 'legacy-memory-v1'
             GROUP BY legacy_facts.category
             ORDER BY COUNT(*) DESC, legacy_facts.category ASC
             LIMIT 128",
            params![
                key.kind,
                key.project_id.as_str(),
                key.json.as_str(),
                source_store_id.as_str(),
            ],
        )
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_READ_OPERATION, error))?;
    let mut coverage = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_READ_OPERATION, error))?
    {
        let category = row_string(&row, 0, PROJECT_MEMORY_READ_OPERATION)?;
        let fact_count = nonnegative_u64(
            row_i64(&row, 1, PROJECT_MEMORY_READ_OPERATION)?,
            "dashboard category fact count",
        )?;
        let vector_count = nonnegative_u64(
            row_i64(&row, 2, PROJECT_MEMORY_READ_OPERATION)?,
            "dashboard category vector count",
        )?;
        let has_bank = row_i64(&row, 3, PROJECT_MEMORY_READ_OPERATION)? != 0;
        let dirty = row_i64(&row, 6, PROJECT_MEMORY_READ_OPERATION)? != 0;
        let state = if vector_count < fact_count {
            tracedecay_store::ProjectMemoryDashboardHrrStateV1::MissingVectors
        } else if !has_bank {
            tracedecay_store::ProjectMemoryDashboardHrrStateV1::MissingBank
        } else if dirty {
            tracedecay_store::ProjectMemoryDashboardHrrStateV1::StaleBank
        } else {
            tracedecay_store::ProjectMemoryDashboardHrrStateV1::Ready
        };
        let coverage_basis_points = vector_count
            .saturating_mul(10_000)
            .checked_div(fact_count)
            .map_or(0, |basis| u16::try_from(basis).unwrap_or(10_000));
        coverage.push(tracedecay_store::ProjectMemoryDashboardHrrCoverageV1::new(
            category.clone(),
            fact_count,
            vector_count,
            coverage_basis_points,
            category,
            if has_bank { vector_count } else { 0 },
            dashboard_compatibility_dimension(row_optional_i64(
                &row,
                4,
                PROJECT_MEMORY_READ_OPERATION,
            )?)?,
            row_optional_i64(&row, 5, PROJECT_MEMORY_READ_OPERATION)?.map(UtcMicros),
            state,
        )?);
    }
    Ok(coverage)
}

fn dashboard_compatibility_memory_bank_from_row(
    row: &crate::db::engine::Row,
) -> ProjectMemoryResult<tracedecay_store::ProjectMemoryDashboardMemoryBankV1> {
    tracedecay_store::ProjectMemoryDashboardMemoryBankV1::new(
        row_string(row, 0, PROJECT_MEMORY_READ_OPERATION)?,
        dashboard_compatibility_dimension(row_optional_i64(
            row,
            1,
            PROJECT_MEMORY_READ_OPERATION,
        )?)?,
        nonnegative_u64(
            row_i64(row, 3, PROJECT_MEMORY_READ_OPERATION)?,
            "dashboard bank fact count",
        )?,
        nonnegative_u64(
            row_i64(row, 4, PROJECT_MEMORY_READ_OPERATION)?,
            "dashboard bank bundled fact count",
        )?,
        row_optional_i64(row, 2, PROJECT_MEMORY_READ_OPERATION)?.map(UtcMicros),
    )
    .map_err(Into::into)
}

async fn dashboard_compatibility_memory_banks_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
) -> ProjectMemoryResult<Vec<tracedecay_store::ProjectMemoryDashboardMemoryBankV1>> {
    let key = OwnerKey::new(owner)?;
    let mut rows = transaction
        .query(
            "SELECT banks.bank_name, banks.hrr_dim, banks.updated_at,
                    banks.fact_count, banks.fact_count
             FROM memory_v2_banks AS banks
             WHERE banks.owner_kind = ?1
               AND banks.project_id = ?2
               AND banks.owner_json = ?3
             ORDER BY banks.fact_count DESC, banks.bank_name ASC
             LIMIT 128",
            params![key.kind, key.project_id.as_str(), key.json.as_str()],
        )
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_READ_OPERATION, error))?;
    let mut banks = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_READ_OPERATION, error))?
    {
        banks.push(dashboard_compatibility_memory_bank_from_row(&row)?);
    }
    Ok(banks)
}

async fn dashboard_project_memory_growth_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
) -> ProjectMemoryResult<Vec<tracedecay_store::ProjectMemoryDashboardGrowthPointV1>> {
    let key = OwnerKey::new(owner)?;
    let source_store_id = compatibility_source_store_id()?;
    let mut rows = transaction
        .query(
            "WITH latest_days AS (
                 SELECT date(legacy_facts.created_at, 'unixepoch') AS period,
                        COUNT(*) AS fact_count
                 FROM memory_facts AS legacy_facts
                 JOIN memory_v2_facts AS mappings
                   ON mappings.fact_id = legacy_facts.canonical_fact_id
                 WHERE mappings.owner_kind = ?1
                   AND mappings.project_id = ?2
                   AND mappings.owner_json = ?3
                   AND ?4 = 'legacy-memory-v1'
                   AND legacy_facts.created_at > 0
                 GROUP BY period
                 ORDER BY period DESC
                 LIMIT 180
             ), prior AS (
                 SELECT COUNT(*) AS fact_count
                 FROM memory_facts AS legacy_facts
                 JOIN memory_v2_facts AS mappings
                   ON mappings.fact_id = legacy_facts.canonical_fact_id
                 WHERE mappings.owner_kind = ?5
                   AND mappings.project_id = ?6
                   AND mappings.owner_json = ?7
                   AND ?8 = 'legacy-memory-v1'
                   AND legacy_facts.created_at > 0
                   AND date(legacy_facts.created_at, 'unixepoch') < (
                       SELECT MIN(period) FROM latest_days
                   )
             )
             SELECT latest_days.period, latest_days.fact_count,
                    prior.fact_count + SUM(latest_days.fact_count)
                        OVER (ORDER BY latest_days.period ASC)
             FROM latest_days CROSS JOIN prior
             ORDER BY latest_days.period ASC",
            params![
                key.kind,
                key.project_id.as_str(),
                key.json.as_str(),
                source_store_id.as_str(),
                key.kind,
                key.project_id.as_str(),
                key.json.as_str(),
                source_store_id.as_str(),
            ],
        )
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_READ_OPERATION, error))?;
    let mut growth = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_READ_OPERATION, error))?
    {
        growth.push(tracedecay_store::ProjectMemoryDashboardGrowthPointV1::new(
            row_string(&row, 0, PROJECT_MEMORY_READ_OPERATION)?,
            nonnegative_u64(
                row_i64(&row, 1, PROJECT_MEMORY_READ_OPERATION)?,
                "dashboard daily fact count",
            )?,
            nonnegative_u64(
                row_i64(&row, 2, PROJECT_MEMORY_READ_OPERATION)?,
                "dashboard cumulative fact count",
            )?,
        )?);
    }
    Ok(growth)
}

pub(super) async fn dashboard_project_memory_overview_tx(
    transaction: &Transaction<'_>,
    query: &ProjectMemoryDashboardMemoryOverviewQueryV1,
) -> ProjectMemoryResult<ProjectMemoryDashboardMemoryOverviewV1> {
    let owner = query.owner();
    let fact_count = dashboard_project_memory_owner_count_tx(transaction, owner, false).await?;
    let entity_count = dashboard_project_memory_owner_count_tx(transaction, owner, true).await?;
    let facts =
        dashboard_project_memory_fact_summaries_tx(transaction, owner, query.fact_limit()).await?;
    let entities =
        dashboard_project_memory_entities_tx(transaction, owner, query.graph_limit()).await?;
    let fact_ids = facts
        .iter()
        .map(|fact| fact.fact.fact_id().as_str().to_owned())
        .collect::<BTreeSet<_>>();
    let entity_ids = entities
        .iter()
        .map(|entity| entity.target.legacy_entity_id())
        .collect::<BTreeSet<_>>();
    let fact_entity_links = dashboard_project_memory_fact_entity_links_tx(
        transaction,
        owner,
        &fact_ids,
        &entity_ids,
        query.graph_limit(),
    )
    .await?;
    let categories = dashboard_project_memory_named_counts_tx(
        transaction,
        owner,
        ProjectMemoryDashboardNamedCountKind::Category,
    )
    .await?;
    let entity_types = dashboard_project_memory_named_counts_tx(
        transaction,
        owner,
        ProjectMemoryDashboardNamedCountKind::EntityType,
    )
    .await?;
    let hrr_coverage = dashboard_compatibility_hrr_coverage_tx(transaction, owner).await?;
    let memory_banks = dashboard_compatibility_memory_banks_tx(transaction, owner).await?;
    let trust_histogram = dashboard_project_memory_named_counts_tx(
        transaction,
        owner,
        ProjectMemoryDashboardNamedCountKind::TrustBucket,
    )
    .await?;
    let growth = dashboard_project_memory_growth_tx(transaction, owner).await?;
    ProjectMemoryDashboardMemoryOverviewV1::new(
        owner.clone(),
        fact_count,
        entity_count,
        memory_banks.len() as u64,
        facts,
        entities,
        fact_entity_links,
        categories,
        entity_types,
        hrr_coverage,
        memory_banks,
        trust_histogram,
        growth,
    )
    .map_err(Into::into)
}

async fn dashboard_project_memory_entities_for_fact_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    fact_id: &FactId,
) -> ProjectMemoryResult<Vec<tracedecay_store::ProjectMemoryDashboardEntityV1>> {
    let key = OwnerKey::new(owner)?;
    let source_store_id = compatibility_source_store_id()?;
    let mut rows = transaction
        .query(
            "SELECT entities.entity_id, entities.name, entities.entity_type,
                    entities.aliases, entities.created_at,
                    COUNT(DISTINCT related_mappings.fact_id)
             FROM memory_facts AS target_facts
             JOIN memory_v2_facts AS target_mappings
               ON target_mappings.fact_id = target_facts.canonical_fact_id
             JOIN memory_fact_entities AS target_relations
               ON target_relations.fact_id = target_facts.fact_id
             JOIN memory_entities AS entities
               ON entities.entity_id = target_relations.entity_id
             LEFT JOIN memory_fact_entities AS related_relations
               ON related_relations.entity_id = entities.entity_id
             LEFT JOIN memory_facts AS related_facts
               ON related_facts.fact_id = related_relations.fact_id
             LEFT JOIN memory_v2_facts AS related_mappings
               ON related_mappings.fact_id = related_facts.canonical_fact_id
              AND related_mappings.owner_kind = ?1
              AND related_mappings.project_id = ?2
              AND related_mappings.owner_json = ?3
              AND ?4 = 'legacy-memory-v1'
             WHERE target_mappings.owner_kind = ?1
               AND target_mappings.project_id = ?2
               AND target_mappings.owner_json = ?3
               AND ?4 = 'legacy-memory-v1'
               AND target_mappings.fact_id = ?5
             GROUP BY entities.entity_id, entities.name, entities.entity_type,
                      entities.aliases, entities.created_at
             ORDER BY entities.name ASC, entities.entity_id ASC
             LIMIT 128",
            params![
                key.kind,
                key.project_id.as_str(),
                key.json.as_str(),
                source_store_id.as_str(),
                fact_id.as_str(),
            ],
        )
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_READ_OPERATION, error))?;
    let mut entities = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_READ_OPERATION, error))?
    {
        entities.push(tracedecay_store::ProjectMemoryDashboardEntityV1::new(
            tracedecay_store::ProjectMemoryLegacyEntityTargetV1::new(
                owner.clone(),
                row_i64(&row, 0, PROJECT_MEMORY_READ_OPERATION)?,
            )?,
            row_string(&row, 1, PROJECT_MEMORY_READ_OPERATION)?,
            row_string(&row, 2, PROJECT_MEMORY_READ_OPERATION)?,
            from_json::<Vec<String>>(
                &row_string(&row, 3, PROJECT_MEMORY_READ_OPERATION)?,
                PROJECT_MEMORY_READ_OPERATION,
            )?,
            UtcMicros(row_i64(&row, 4, PROJECT_MEMORY_READ_OPERATION)?),
            nonnegative_u64(
                row_i64(&row, 5, PROJECT_MEMORY_READ_OPERATION)?,
                "dashboard entity fact count",
            )?,
        )?);
    }
    Ok(entities)
}

pub(super) async fn dashboard_project_memory_fact_detail_tx(
    transaction: &Transaction<'_>,
    query: &ProjectMemoryDashboardFactDetailQueryV1,
) -> ProjectMemoryResult<Option<ProjectMemoryDashboardFactDetailV1>> {
    let owner = query.target().owner();
    let Some(fact_id) = resolve_project_memory_target_tx(transaction, query.target()).await? else {
        return Ok(None);
    };
    if project_memory_legacy_mapping_tx(transaction, owner, &fact_id)
        .await?
        .is_none()
    {
        return Ok(None);
    }
    let Some(fact) = load_project_memory_projection_tx(transaction, owner, &fact_id).await? else {
        return Ok(None);
    };
    let entities =
        dashboard_project_memory_entities_for_fact_tx(transaction, owner, &fact_id).await?;
    let target =
        ProjectMemoryFactTargetV1::Canonical(ProjectMemoryFactIdV1::new(owner.clone(), fact_id)?);
    let history = project_memory_fact_history_tx(
        transaction,
        &ProjectMemoryFactHistoryQueryV1::new(target, None, 128)?,
    )
    .await?;
    ProjectMemoryDashboardFactDetailV1::new(fact, entities, Some(history))
        .map(Some)
        .map_err(Into::into)
}

fn dashboard_project_memory_like_pattern(search: &str) -> String {
    let escaped = search
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_");
    format!("%{escaped}%")
}

pub(super) async fn dashboard_project_memory_vector_points_tx(
    transaction: &Transaction<'_>,
    query: &ProjectMemoryDashboardVectorPointsQueryV1,
) -> ProjectMemoryResult<Vec<ProjectMemoryDashboardVectorPointV1>> {
    let key = OwnerKey::new(query.owner())?;
    let source_store_id = compatibility_source_store_id()?;
    let limit = i64::try_from(query.limit()).map_err(|_| FactStoreError::InvalidQueryLimit {
        limit: query.limit(),
        max: usize::MAX,
    })?;
    let search = query
        .search()
        .filter(|search| !search.trim().is_empty())
        .map(dashboard_project_memory_like_pattern);
    let mut rows = transaction
        .query(
            // The V1 dashboard reported a fact's graph connections as its
            // entity-link count; parity keeps both columns on that basis.
            "SELECT mappings.fact_id, legacy_facts.hrr_vector, banks.bank_name,
                    COUNT(DISTINCT relations.entity_id),
                    COUNT(DISTINCT relations.entity_id)
             FROM memory_facts AS legacy_facts
             JOIN memory_v2_facts AS mappings
               ON mappings.fact_id = legacy_facts.canonical_fact_id
             LEFT JOIN memory_v2_banks AS banks
               ON banks.owner_kind = mappings.owner_kind
              AND banks.project_id = mappings.project_id
              AND banks.owner_json = mappings.owner_json
              AND banks.bank_name = legacy_facts.category
             LEFT JOIN memory_fact_entities AS relations
               ON relations.fact_id = legacy_facts.fact_id
             WHERE mappings.owner_kind = ?1
               AND mappings.project_id = ?2
               AND mappings.owner_json = ?3
               AND ?4 = 'legacy-memory-v1'
               AND (
                    ?5 IS NULL
                    OR legacy_facts.content LIKE ?5 ESCAPE '\\'
                    OR legacy_facts.tags LIKE ?5 ESCAPE '\\'
               )
             GROUP BY mappings.fact_id, legacy_facts.hrr_vector, banks.bank_name
             ORDER BY legacy_facts.trust_score DESC,
                      legacy_facts.updated_at DESC,
                      mappings.fact_id ASC
             LIMIT ?6",
            params![
                key.kind,
                key.project_id.as_str(),
                key.json.as_str(),
                source_store_id.as_str(),
                search,
                limit,
            ],
        )
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_READ_OPERATION, error))?;
    let mut raw_points = Vec::with_capacity(query.limit());
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_READ_OPERATION, error))?
    {
        let fact_id = FactId::new(row_string(&row, 0, PROJECT_MEMORY_READ_OPERATION)?)
            .map_err(FactStoreError::from)?;
        let vector = match row.get::<crate::db::engine::Value>(1) {
            Ok(crate::db::engine::Value::Blob(bytes)) => HolographicEncoder::deserialize(&bytes)
                .ok()
                .filter(|vector| {
                    !vector.is_empty()
                        && vector.len() <= 16_384
                        && vector.iter().all(|value| value.is_finite())
                }),
            Ok(crate::db::engine::Value::Null | _) | Err(_) => None,
        };
        raw_points.push((
            fact_id,
            vector,
            row_optional_string(&row, 2, PROJECT_MEMORY_READ_OPERATION)?,
            nonnegative_u64(
                row_i64(&row, 3, PROJECT_MEMORY_READ_OPERATION)?,
                "dashboard vector entity count",
            )?,
            nonnegative_u64(
                row_i64(&row, 4, PROJECT_MEMORY_READ_OPERATION)?,
                "dashboard vector connection count",
            )?,
        ));
    }
    drop(rows);
    let fact_ids = raw_points
        .iter()
        .map(|(fact_id, ..)| fact_id.clone())
        .collect::<Vec<_>>();
    let facts = load_project_memory_projections_tx(transaction, query.owner(), &fact_ids).await?;
    if facts.len() != raw_points.len() {
        return Err(storage_message(
            PROJECT_MEMORY_READ_OPERATION,
            "owner-bound dashboard vector mapping has no canonical fact projection",
        )
        .into());
    }
    let mut points = Vec::with_capacity(raw_points.len());
    for ((_, vector, bank_name, entity_count, connection_count), fact) in
        raw_points.into_iter().zip(facts)
    {
        let vector = matches!(&fact, ProjectMemoryFactProjectionV1::Available(_))
            .then_some(vector)
            .flatten();
        points.push(ProjectMemoryDashboardVectorPointV1::new(
            tracedecay_store::ProjectMemoryDashboardFactSummaryV1 {
                has_hrr_vector: vector.is_some(),
                fact,
            },
            vector,
            bank_name,
            entity_count,
            connection_count,
        )?);
    }
    Ok(points)
}

fn dashboard_project_memory_oplog_operation(value: &str) -> String {
    match value {
        "add" | "update" | "remove" | "feedback" | "reject_secret_like" | "curate_apply" => {
            value.to_owned()
        }
        _ => "legacy_mutation".to_owned(),
    }
}

fn dashboard_project_memory_oplog_details(
    raw: Option<String>,
) -> tracedecay_store::ProjectMemoryDashboardOplogDetailsV1 {
    match raw {
        Some(raw) if serde_json::from_str::<Value>(&raw).is_ok() => {
            tracedecay_store::ProjectMemoryDashboardOplogDetailsV1::Redacted
        }
        Some(_) | None => tracedecay_store::ProjectMemoryDashboardOplogDetailsV1::Unknown,
    }
}

pub(super) async fn dashboard_project_memory_oplog_tx(
    transaction: &Transaction<'_>,
    query: &ProjectMemoryDashboardOplogQueryV1,
) -> ProjectMemoryResult<Vec<ProjectMemoryDashboardOplogEntryV1>> {
    let key = OwnerKey::new(query.owner())?;
    let source_store_id = compatibility_source_store_id()?;
    let limit = i64::try_from(query.limit()).map_err(|_| FactStoreError::InvalidQueryLimit {
        limit: query.limit(),
        max: usize::MAX,
    })?;
    let mut rows = transaction
        .query(
            "SELECT oplog.id, oplog.ts, oplog.op, oplog.fact_id, oplog.detail_json
             FROM memory_oplog AS oplog
             JOIN memory_facts AS legacy_facts
               ON legacy_facts.fact_id = oplog.fact_id
             JOIN memory_v2_facts AS mappings
               ON mappings.fact_id = legacy_facts.canonical_fact_id
             WHERE mappings.owner_kind = ?1
               AND mappings.project_id = ?2
               AND mappings.owner_json = ?3
               AND ?4 = 'legacy-memory-v1'
             ORDER BY oplog.id DESC
             LIMIT ?5",
            params![
                key.kind,
                key.project_id.as_str(),
                key.json.as_str(),
                source_store_id.as_str(),
                limit,
            ],
        )
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_READ_OPERATION, error))?;
    let mut entries = Vec::with_capacity(query.limit());
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_READ_OPERATION, error))?
    {
        let legacy_fact_id = row_i64(&row, 3, PROJECT_MEMORY_READ_OPERATION)?;
        entries.push(ProjectMemoryDashboardOplogEntryV1::new(
            row_i64(&row, 0, PROJECT_MEMORY_READ_OPERATION)?,
            UtcMicros(row_i64(&row, 1, PROJECT_MEMORY_READ_OPERATION)?),
            dashboard_project_memory_oplog_operation(&row_string(
                &row,
                2,
                PROJECT_MEMORY_READ_OPERATION,
            )?),
            Some(ProjectMemoryFactTargetV1::Legacy(LegacyFactQuery::new(
                query.owner().clone(),
                source_store_id.clone(),
                legacy_fact_id,
            )?)),
            dashboard_project_memory_oplog_details(row_optional_string(
                &row,
                4,
                PROJECT_MEMORY_READ_OPERATION,
            )?),
        )?);
    }
    Ok(entries)
}
