use tracedecay_domain::{DomainError, FactOwnerV1, UtcMicros};

use super::super::queries::validate_limit;
use super::super::{FactStoreError, FactStoreResult};
use super::curation::MAX_COMPATIBILITY_CURATION_TARGETS;
use super::{
    CompatibilityFactHistoryV1, CompatibilityFactProjectionV1, CompatibilityFactTargetV1,
    CompatibilityLegacyEntityTargetV1, validate_compatibility_text,
};

const MAX_COMPATIBILITY_DASHBOARD_FACTS: usize = 100;

const MAX_COMPATIBILITY_DASHBOARD_GRAPH: usize = 1_000;

pub(in crate::memory) const MAX_COMPATIBILITY_DASHBOARD_VECTORS: usize = 2_000;

pub(in crate::memory) const MAX_COMPATIBILITY_DASHBOARD_OPLOG: usize = 300;

/// Explicit, bounded dashboard overview request. It is intentionally not a
/// general query language: the dashboard receives one finite snapshot shape.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompatibilityDashboardMemoryOverviewQueryV1 {
    owner: FactOwnerV1,
    fact_limit: usize,
    graph_limit: usize,
}

impl CompatibilityDashboardMemoryOverviewQueryV1 {
    pub fn new(owner: FactOwnerV1, fact_limit: usize, graph_limit: usize) -> FactStoreResult<Self> {
        owner.validate()?;
        validate_limit(fact_limit, MAX_COMPATIBILITY_DASHBOARD_FACTS)?;
        validate_limit(graph_limit, MAX_COMPATIBILITY_DASHBOARD_GRAPH)?;
        Ok(Self {
            owner,
            fact_limit,
            graph_limit,
        })
    }

    pub fn owner(&self) -> &FactOwnerV1 {
        &self.owner
    }

    pub fn fact_limit(&self) -> usize {
        self.fact_limit
    }

    pub fn graph_limit(&self) -> usize {
        self.graph_limit
    }
}

/// A safe projection for dashboard fact rows. `fact` retains the canonical
/// availability state instead of inventing payload fields for unavailable rows.
#[derive(Clone, Debug, PartialEq)]
pub struct CompatibilityDashboardFactSummaryV1 {
    pub fact: CompatibilityFactProjectionV1,
    pub has_hrr_vector: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompatibilityDashboardEntityV1 {
    pub target: CompatibilityLegacyEntityTargetV1,
    pub name: String,
    pub entity_type: String,
    pub aliases: Vec<String>,
    pub created_at: UtcMicros,
    pub fact_count: u64,
}

impl CompatibilityDashboardEntityV1 {
    pub fn new(
        target: CompatibilityLegacyEntityTargetV1,
        name: String,
        entity_type: String,
        aliases: Vec<String>,
        created_at: UtcMicros,
        fact_count: u64,
    ) -> FactStoreResult<Self> {
        target.validate()?;
        validate_compatibility_text(&name, "dashboard entity name")?;
        validate_compatibility_text(&entity_type, "dashboard entity type")?;
        if aliases.len() > MAX_COMPATIBILITY_CURATION_TARGETS {
            return Err(FactStoreError::InvalidQueryLimit {
                limit: aliases.len(),
                max: MAX_COMPATIBILITY_CURATION_TARGETS,
            });
        }
        for alias in &aliases {
            validate_compatibility_text(alias, "dashboard entity alias")?;
        }
        Ok(Self {
            target,
            name,
            entity_type,
            aliases,
            created_at,
            fact_count,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompatibilityDashboardFactEntityLinkV1 {
    pub fact: CompatibilityFactTargetV1,
    pub entity: CompatibilityLegacyEntityTargetV1,
}

impl CompatibilityDashboardFactEntityLinkV1 {
    pub fn new(
        fact: CompatibilityFactTargetV1,
        entity: CompatibilityLegacyEntityTargetV1,
    ) -> FactStoreResult<Self> {
        fact.validate()?;
        entity.validate()?;
        if fact.owner() != entity.owner() {
            return Err(FactStoreError::OwnerMismatch);
        }
        Ok(Self { fact, entity })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompatibilityDashboardNamedCountV1 {
    pub name: String,
    pub count: u64,
}

impl CompatibilityDashboardNamedCountV1 {
    pub fn new(name: String, count: u64) -> FactStoreResult<Self> {
        validate_compatibility_text(&name, "dashboard count name")?;
        Ok(Self { name, count })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CompatibilityDashboardHrrStateV1 {
    Ready,
    MissingVectors,
    MissingBank,
    StaleBank,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompatibilityDashboardHrrCoverageV1 {
    pub category: String,
    pub fact_count: u64,
    pub hrr_vector_count: u64,
    pub coverage_basis_points: u16,
    pub bank_name: String,
    pub bank_fact_count: u64,
    pub dimension: Option<u32>,
    pub updated_at: Option<UtcMicros>,
    pub state: CompatibilityDashboardHrrStateV1,
}

impl CompatibilityDashboardHrrCoverageV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        category: String,
        fact_count: u64,
        hrr_vector_count: u64,
        coverage_basis_points: u16,
        bank_name: String,
        bank_fact_count: u64,
        dimension: Option<u32>,
        updated_at: Option<UtcMicros>,
        state: CompatibilityDashboardHrrStateV1,
    ) -> FactStoreResult<Self> {
        validate_compatibility_text(&category, "dashboard HRR category")?;
        validate_compatibility_text(&bank_name, "dashboard HRR bank name")?;
        if coverage_basis_points > 10_000 {
            return Err(FactStoreError::Contract(DomainError::NonCanonical {
                field: "dashboard HRR coverage",
            }));
        }
        Ok(Self {
            category,
            fact_count,
            hrr_vector_count,
            coverage_basis_points,
            bank_name,
            bank_fact_count,
            dimension,
            updated_at,
            state,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompatibilityDashboardMemoryBankV1 {
    pub name: String,
    pub dimension: Option<u32>,
    pub fact_count: u64,
    pub bundled_fact_count: u64,
    pub updated_at: Option<UtcMicros>,
}

impl CompatibilityDashboardMemoryBankV1 {
    pub fn new(
        name: String,
        dimension: Option<u32>,
        fact_count: u64,
        bundled_fact_count: u64,
        updated_at: Option<UtcMicros>,
    ) -> FactStoreResult<Self> {
        validate_compatibility_text(&name, "dashboard memory bank name")?;
        Ok(Self {
            name,
            dimension,
            fact_count,
            bundled_fact_count,
            updated_at,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompatibilityDashboardGrowthPointV1 {
    pub period: String,
    pub fact_count: u64,
    pub cumulative_fact_count: u64,
}

impl CompatibilityDashboardGrowthPointV1 {
    pub fn new(
        period: String,
        fact_count: u64,
        cumulative_fact_count: u64,
    ) -> FactStoreResult<Self> {
        validate_compatibility_text(&period, "dashboard growth period")?;
        Ok(Self {
            period,
            fact_count,
            cumulative_fact_count,
        })
    }
}

/// One fixed, bounded dashboard overview shape. Counters and graph relationships
/// stay typed; arbitrary query result rows are not exposed across the store port.
#[derive(Clone, Debug, PartialEq)]
pub struct CompatibilityDashboardMemoryOverviewV1 {
    pub owner: FactOwnerV1,
    pub fact_count: u64,
    pub entity_count: u64,
    pub bank_count: u64,
    pub facts: Vec<CompatibilityDashboardFactSummaryV1>,
    pub entities: Vec<CompatibilityDashboardEntityV1>,
    pub fact_entity_links: Vec<CompatibilityDashboardFactEntityLinkV1>,
    pub categories: Vec<CompatibilityDashboardNamedCountV1>,
    pub entity_types: Vec<CompatibilityDashboardNamedCountV1>,
    pub hrr_coverage: Vec<CompatibilityDashboardHrrCoverageV1>,
    pub memory_banks: Vec<CompatibilityDashboardMemoryBankV1>,
    pub trust_histogram: Vec<CompatibilityDashboardNamedCountV1>,
    pub growth: Vec<CompatibilityDashboardGrowthPointV1>,
}

impl CompatibilityDashboardMemoryOverviewV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        owner: FactOwnerV1,
        fact_count: u64,
        entity_count: u64,
        bank_count: u64,
        facts: Vec<CompatibilityDashboardFactSummaryV1>,
        entities: Vec<CompatibilityDashboardEntityV1>,
        fact_entity_links: Vec<CompatibilityDashboardFactEntityLinkV1>,
        categories: Vec<CompatibilityDashboardNamedCountV1>,
        entity_types: Vec<CompatibilityDashboardNamedCountV1>,
        hrr_coverage: Vec<CompatibilityDashboardHrrCoverageV1>,
        memory_banks: Vec<CompatibilityDashboardMemoryBankV1>,
        trust_histogram: Vec<CompatibilityDashboardNamedCountV1>,
        growth: Vec<CompatibilityDashboardGrowthPointV1>,
    ) -> FactStoreResult<Self> {
        owner.validate()?;
        for fact in &facts {
            if fact.fact.owner() != &owner {
                return Err(FactStoreError::OwnerMismatch);
            }
        }
        if facts.len() > MAX_COMPATIBILITY_DASHBOARD_FACTS {
            return Err(FactStoreError::InvalidQueryLimit {
                limit: facts.len(),
                max: MAX_COMPATIBILITY_DASHBOARD_FACTS,
            });
        }
        let bounded = entities
            .len()
            .max(fact_entity_links.len())
            .max(categories.len())
            .max(entity_types.len())
            .max(hrr_coverage.len())
            .max(memory_banks.len())
            .max(trust_histogram.len())
            .max(growth.len());
        if bounded > MAX_COMPATIBILITY_DASHBOARD_GRAPH {
            return Err(FactStoreError::InvalidQueryLimit {
                limit: bounded,
                max: MAX_COMPATIBILITY_DASHBOARD_GRAPH,
            });
        }
        for entity in &entities {
            if entity.target.owner() != &owner {
                return Err(FactStoreError::OwnerMismatch);
            }
        }
        for link in &fact_entity_links {
            if link.fact.owner() != &owner || link.entity.owner() != &owner {
                return Err(FactStoreError::OwnerMismatch);
            }
        }
        Ok(Self {
            owner,
            fact_count,
            entity_count,
            bank_count,
            facts,
            entities,
            fact_entity_links,
            categories,
            entity_types,
            hrr_coverage,
            memory_banks,
            trust_histogram,
            growth,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompatibilityDashboardFactDetailQueryV1 {
    target: CompatibilityFactTargetV1,
}

impl CompatibilityDashboardFactDetailQueryV1 {
    pub fn new(target: CompatibilityFactTargetV1) -> FactStoreResult<Self> {
        target.validate()?;
        Ok(Self { target })
    }

    pub fn target(&self) -> &CompatibilityFactTargetV1 {
        &self.target
    }
}

/// Detail includes lineage when the backend can resolve it, but keeps the same
/// availability-preserving fact projection used by list and search views.
#[derive(Clone, Debug, PartialEq)]
pub struct CompatibilityDashboardFactDetailV1 {
    pub fact: CompatibilityFactProjectionV1,
    pub entities: Vec<CompatibilityDashboardEntityV1>,
    pub history: Option<CompatibilityFactHistoryV1>,
}

impl CompatibilityDashboardFactDetailV1 {
    pub fn new(
        fact: CompatibilityFactProjectionV1,
        entities: Vec<CompatibilityDashboardEntityV1>,
        history: Option<CompatibilityFactHistoryV1>,
    ) -> FactStoreResult<Self> {
        if entities.len() > MAX_COMPATIBILITY_DASHBOARD_GRAPH {
            return Err(FactStoreError::InvalidQueryLimit {
                limit: entities.len(),
                max: MAX_COMPATIBILITY_DASHBOARD_GRAPH,
            });
        }
        let owner = fact.owner();
        if entities
            .iter()
            .any(|entity| entity.target.validate().is_err() || entity.target.owner() != owner)
        {
            return Err(FactStoreError::OwnerMismatch);
        }
        if let Some(history) = &history
            && history.owner() != owner
        {
            return Err(FactStoreError::OwnerMismatch);
        }
        Ok(Self {
            fact,
            entities,
            history,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompatibilityDashboardVectorPointsQueryV1 {
    owner: FactOwnerV1,
    search: Option<String>,
    limit: usize,
}

impl CompatibilityDashboardVectorPointsQueryV1 {
    pub fn new(owner: FactOwnerV1, search: Option<String>, limit: usize) -> FactStoreResult<Self> {
        owner.validate()?;
        validate_limit(limit, MAX_COMPATIBILITY_DASHBOARD_VECTORS)?;
        if let Some(search) = &search {
            validate_compatibility_text(search, "dashboard vector search")?;
        }
        Ok(Self {
            owner,
            search,
            limit,
        })
    }

    pub fn owner(&self) -> &FactOwnerV1 {
        &self.owner
    }

    pub fn search(&self) -> Option<&str> {
        self.search.as_deref()
    }

    pub fn limit(&self) -> usize {
        self.limit
    }
}

/// A finite point for client-side PCA/similarity. Vectors are capped and checked
/// for finite components, and unavailable facts retain no fabricated vector.
#[derive(Clone, Debug, PartialEq)]
pub struct CompatibilityDashboardVectorPointV1 {
    pub fact: CompatibilityDashboardFactSummaryV1,
    pub vector: Option<Vec<f64>>,
    pub bank_name: Option<String>,
    pub entity_count: u64,
    pub connection_count: u64,
}

impl CompatibilityDashboardVectorPointV1 {
    pub fn new(
        fact: CompatibilityDashboardFactSummaryV1,
        vector: Option<Vec<f64>>,
        bank_name: Option<String>,
        entity_count: u64,
        connection_count: u64,
    ) -> FactStoreResult<Self> {
        if let Some(vector) = &vector
            && (vector.len() > 16_384 || vector.iter().any(|value| !value.is_finite()))
        {
            return Err(FactStoreError::Contract(DomainError::NonCanonical {
                field: "dashboard vector point",
            }));
        }
        if let Some(bank_name) = &bank_name {
            validate_compatibility_text(bank_name, "dashboard vector bank name")?;
        }
        if matches!(fact.fact, CompatibilityFactProjectionV1::Unavailable(_)) && vector.is_some() {
            return Err(FactStoreError::Contract(DomainError::NonCanonical {
                field: "dashboard unavailable vector",
            }));
        }
        Ok(Self {
            fact,
            vector,
            bank_name,
            entity_count,
            connection_count,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompatibilityDashboardOplogQueryV1 {
    owner: FactOwnerV1,
    limit: usize,
}

impl CompatibilityDashboardOplogQueryV1 {
    pub fn new(owner: FactOwnerV1, limit: usize) -> FactStoreResult<Self> {
        owner.validate()?;
        validate_limit(limit, MAX_COMPATIBILITY_DASHBOARD_OPLOG)?;
        Ok(Self { owner, limit })
    }

    pub fn owner(&self) -> &FactOwnerV1 {
        &self.owner
    }

    pub fn limit(&self) -> usize {
        self.limit
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CompatibilityDashboardOplogDetailsV1 {
    Available { summary: String },
    Redacted,
    Unknown,
}

impl CompatibilityDashboardOplogDetailsV1 {
    pub fn available(summary: String) -> FactStoreResult<Self> {
        validate_compatibility_text(&summary, "dashboard oplog detail")?;
        Ok(Self::Available { summary })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompatibilityDashboardOplogEntryV1 {
    pub id: i64,
    pub occurred_at: UtcMicros,
    pub operation: String,
    pub fact: Option<CompatibilityFactTargetV1>,
    pub details: CompatibilityDashboardOplogDetailsV1,
}

impl CompatibilityDashboardOplogEntryV1 {
    pub fn new(
        id: i64,
        occurred_at: UtcMicros,
        operation: String,
        fact: Option<CompatibilityFactTargetV1>,
        details: CompatibilityDashboardOplogDetailsV1,
    ) -> FactStoreResult<Self> {
        if id <= 0 {
            return Err(FactStoreError::Contract(DomainError::NonCanonical {
                field: "dashboard oplog id",
            }));
        }
        validate_compatibility_text(&operation, "dashboard oplog operation")?;
        if let Some(fact) = &fact {
            fact.validate()?;
        }
        Ok(Self {
            id,
            occurred_at,
            operation,
            fact,
            details,
        })
    }
}
