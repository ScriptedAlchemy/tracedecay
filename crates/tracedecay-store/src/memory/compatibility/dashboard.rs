use tracedecay_domain::{DomainError, FactOwnerV1, UtcMicros};

use super::super::queries::validate_limit;
use super::super::{FactLineageError, FactLineageResult};
use super::curation::MAX_FACT_CURATION_TARGETS;
use super::{FactEntityTarget, FactHistory, FactProjection, FactTarget, validate_text};

const MAX_FACT_DASHBOARD_FACTS: usize = 100;

const MAX_FACT_DASHBOARD_GRAPH: usize = 1_000;

pub(in crate::memory) const MAX_FACT_DASHBOARD_VECTORS: usize = 2_000;

pub(in crate::memory) const MAX_FACT_DASHBOARD_OPLOG: usize = 300;

/// Explicit, bounded dashboard overview request. It is intentionally not a
/// general query language: the dashboard receives one finite snapshot shape.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DashboardMemoryOverviewQuery {
    owner: FactOwnerV1,
    fact_limit: usize,
    graph_limit: usize,
}

impl DashboardMemoryOverviewQuery {
    pub fn new(
        owner: FactOwnerV1,
        fact_limit: usize,
        graph_limit: usize,
    ) -> FactLineageResult<Self> {
        owner.validate()?;
        validate_limit(fact_limit, MAX_FACT_DASHBOARD_FACTS)?;
        validate_limit(graph_limit, MAX_FACT_DASHBOARD_GRAPH)?;
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
pub struct DashboardFactSummary {
    pub fact: FactProjection,
    pub has_hrr_vector: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DashboardEntity {
    pub target: FactEntityTarget,
    pub name: String,
    pub entity_type: String,
    pub aliases: Vec<String>,
    pub created_at: UtcMicros,
    pub fact_count: u64,
}

impl DashboardEntity {
    pub fn new(
        target: FactEntityTarget,
        name: String,
        entity_type: String,
        aliases: Vec<String>,
        created_at: UtcMicros,
        fact_count: u64,
    ) -> FactLineageResult<Self> {
        target.validate()?;
        validate_text(&name, "dashboard entity name")?;
        validate_text(&entity_type, "dashboard entity type")?;
        if aliases.len() > MAX_FACT_CURATION_TARGETS {
            return Err(FactLineageError::InvalidQueryLimit {
                limit: aliases.len(),
                max: MAX_FACT_CURATION_TARGETS,
            });
        }
        for alias in &aliases {
            validate_text(alias, "dashboard entity alias")?;
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
pub struct DashboardFactEntityLink {
    pub fact: FactTarget,
    pub entity: FactEntityTarget,
}

impl DashboardFactEntityLink {
    pub fn new(fact: FactTarget, entity: FactEntityTarget) -> FactLineageResult<Self> {
        fact.validate()?;
        entity.validate()?;
        if fact.owner() != entity.owner() {
            return Err(FactLineageError::OwnerMismatch);
        }
        Ok(Self { fact, entity })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DashboardNamedCount {
    pub name: String,
    pub count: u64,
}

impl DashboardNamedCount {
    pub fn new(name: String, count: u64) -> FactLineageResult<Self> {
        validate_text(&name, "dashboard count name")?;
        Ok(Self { name, count })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DashboardHrrState {
    Ready,
    MissingVectors,
    MissingBank,
    StaleBank,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DashboardHrrCoverage {
    pub category: String,
    pub fact_count: u64,
    pub hrr_vector_count: u64,
    pub coverage_basis_points: u16,
    pub bank_name: String,
    pub bank_fact_count: u64,
    pub dimension: Option<u32>,
    pub updated_at: Option<UtcMicros>,
    pub state: DashboardHrrState,
}

impl DashboardHrrCoverage {
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
        state: DashboardHrrState,
    ) -> FactLineageResult<Self> {
        validate_text(&category, "dashboard HRR category")?;
        validate_text(&bank_name, "dashboard HRR bank name")?;
        if coverage_basis_points > 10_000 {
            return Err(FactLineageError::Contract(DomainError::NonCanonical {
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
pub struct DashboardMemoryBank {
    pub name: String,
    pub dimension: Option<u32>,
    pub fact_count: u64,
    pub bundled_fact_count: u64,
    pub updated_at: Option<UtcMicros>,
}

impl DashboardMemoryBank {
    pub fn new(
        name: String,
        dimension: Option<u32>,
        fact_count: u64,
        bundled_fact_count: u64,
        updated_at: Option<UtcMicros>,
    ) -> FactLineageResult<Self> {
        validate_text(&name, "dashboard memory bank name")?;
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
pub struct DashboardGrowthPoint {
    pub period: String,
    pub fact_count: u64,
    pub cumulative_fact_count: u64,
}

impl DashboardGrowthPoint {
    pub fn new(
        period: String,
        fact_count: u64,
        cumulative_fact_count: u64,
    ) -> FactLineageResult<Self> {
        validate_text(&period, "dashboard growth period")?;
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
pub struct DashboardMemoryOverview {
    pub owner: FactOwnerV1,
    pub fact_count: u64,
    pub entity_count: u64,
    pub bank_count: u64,
    pub facts: Vec<DashboardFactSummary>,
    pub entities: Vec<DashboardEntity>,
    pub fact_entity_links: Vec<DashboardFactEntityLink>,
    pub categories: Vec<DashboardNamedCount>,
    pub entity_types: Vec<DashboardNamedCount>,
    pub hrr_coverage: Vec<DashboardHrrCoverage>,
    pub memory_banks: Vec<DashboardMemoryBank>,
    pub trust_histogram: Vec<DashboardNamedCount>,
    pub growth: Vec<DashboardGrowthPoint>,
}

impl DashboardMemoryOverview {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        owner: FactOwnerV1,
        fact_count: u64,
        entity_count: u64,
        bank_count: u64,
        facts: Vec<DashboardFactSummary>,
        entities: Vec<DashboardEntity>,
        fact_entity_links: Vec<DashboardFactEntityLink>,
        categories: Vec<DashboardNamedCount>,
        entity_types: Vec<DashboardNamedCount>,
        hrr_coverage: Vec<DashboardHrrCoverage>,
        memory_banks: Vec<DashboardMemoryBank>,
        trust_histogram: Vec<DashboardNamedCount>,
        growth: Vec<DashboardGrowthPoint>,
    ) -> FactLineageResult<Self> {
        owner.validate()?;
        for fact in &facts {
            if fact.fact.owner() != &owner {
                return Err(FactLineageError::OwnerMismatch);
            }
        }
        if facts.len() > MAX_FACT_DASHBOARD_FACTS {
            return Err(FactLineageError::InvalidQueryLimit {
                limit: facts.len(),
                max: MAX_FACT_DASHBOARD_FACTS,
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
        if bounded > MAX_FACT_DASHBOARD_GRAPH {
            return Err(FactLineageError::InvalidQueryLimit {
                limit: bounded,
                max: MAX_FACT_DASHBOARD_GRAPH,
            });
        }
        for entity in &entities {
            if entity.target.owner() != &owner {
                return Err(FactLineageError::OwnerMismatch);
            }
        }
        for link in &fact_entity_links {
            if link.fact.owner() != &owner || link.entity.owner() != &owner {
                return Err(FactLineageError::OwnerMismatch);
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
pub struct DashboardFactDetailQuery {
    target: FactTarget,
}

impl DashboardFactDetailQuery {
    pub fn new(target: FactTarget) -> FactLineageResult<Self> {
        target.validate()?;
        Ok(Self { target })
    }

    pub fn target(&self) -> &FactTarget {
        &self.target
    }
}

/// Detail includes lineage when the backend can resolve it, but keeps the same
/// availability-preserving fact projection used by list and search views.
#[derive(Clone, Debug, PartialEq)]
pub struct DashboardFactDetail {
    pub fact: FactProjection,
    pub entities: Vec<DashboardEntity>,
    pub history: Option<FactHistory>,
}

impl DashboardFactDetail {
    pub fn new(
        fact: FactProjection,
        entities: Vec<DashboardEntity>,
        history: Option<FactHistory>,
    ) -> FactLineageResult<Self> {
        if entities.len() > MAX_FACT_DASHBOARD_GRAPH {
            return Err(FactLineageError::InvalidQueryLimit {
                limit: entities.len(),
                max: MAX_FACT_DASHBOARD_GRAPH,
            });
        }
        let owner = fact.owner();
        if entities
            .iter()
            .any(|entity| entity.target.validate().is_err() || entity.target.owner() != owner)
        {
            return Err(FactLineageError::OwnerMismatch);
        }
        if let Some(history) = &history
            && history.owner() != owner
        {
            return Err(FactLineageError::OwnerMismatch);
        }
        Ok(Self {
            fact,
            entities,
            history,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DashboardVectorPointsQuery {
    owner: FactOwnerV1,
    search: Option<String>,
    limit: usize,
}

impl DashboardVectorPointsQuery {
    pub fn new(
        owner: FactOwnerV1,
        search: Option<String>,
        limit: usize,
    ) -> FactLineageResult<Self> {
        owner.validate()?;
        validate_limit(limit, MAX_FACT_DASHBOARD_VECTORS)?;
        if let Some(search) = &search {
            validate_text(search, "dashboard vector search")?;
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
pub struct DashboardVectorPoint {
    pub fact: DashboardFactSummary,
    pub vector: Option<Vec<f64>>,
    pub bank_name: Option<String>,
    pub entity_count: u64,
    pub connection_count: u64,
}

impl DashboardVectorPoint {
    pub fn new(
        fact: DashboardFactSummary,
        vector: Option<Vec<f64>>,
        bank_name: Option<String>,
        entity_count: u64,
        connection_count: u64,
    ) -> FactLineageResult<Self> {
        if let Some(vector) = &vector
            && (vector.len() > 16_384 || vector.iter().any(|value| !value.is_finite()))
        {
            return Err(FactLineageError::Contract(DomainError::NonCanonical {
                field: "dashboard vector point",
            }));
        }
        if let Some(bank_name) = &bank_name {
            validate_text(bank_name, "dashboard vector bank name")?;
        }
        if matches!(fact.fact, FactProjection::Unavailable(_)) && vector.is_some() {
            return Err(FactLineageError::Contract(DomainError::NonCanonical {
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
pub struct DashboardOplogQuery {
    owner: FactOwnerV1,
    limit: usize,
}

impl DashboardOplogQuery {
    pub fn new(owner: FactOwnerV1, limit: usize) -> FactLineageResult<Self> {
        owner.validate()?;
        validate_limit(limit, MAX_FACT_DASHBOARD_OPLOG)?;
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
pub enum DashboardOplogDetails {
    Available { summary: String },
    Redacted,
    Unknown,
}

impl DashboardOplogDetails {
    pub fn available(summary: String) -> FactLineageResult<Self> {
        validate_text(&summary, "dashboard oplog detail")?;
        Ok(Self::Available { summary })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DashboardOplogEntry {
    pub id: i64,
    pub occurred_at: UtcMicros,
    pub operation: String,
    pub fact: Option<FactTarget>,
    pub details: DashboardOplogDetails,
}

impl DashboardOplogEntry {
    pub fn new(
        id: i64,
        occurred_at: UtcMicros,
        operation: String,
        fact: Option<FactTarget>,
        details: DashboardOplogDetails,
    ) -> FactLineageResult<Self> {
        if id <= 0 {
            return Err(FactLineageError::Contract(DomainError::NonCanonical {
                field: "dashboard oplog id",
            }));
        }
        validate_text(&operation, "dashboard oplog operation")?;
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
