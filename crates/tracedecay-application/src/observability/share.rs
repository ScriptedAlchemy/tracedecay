use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracedecay_domain::{AnalyticsModeV1, CoverageStateV1};

use super::{ObservabilityFuture, ObservabilityHorizonV1};
use crate::ApplicationContractError;

pub const AGGREGATE_SHARE_MIN_CONTRIBUTION_WINDOWS_V1: u64 = 100;
pub const AGGREGATE_SHARE_MAX_DIMENSIONS_V1: usize = 4;
pub const AGGREGATE_SHARE_MAX_CELLS_V1: usize = 256;

#[derive(
    Clone, Copy, Debug, Deserialize, Eq, JsonSchema, Ord, PartialEq, PartialOrd, Serialize,
)]
#[serde(rename_all = "snake_case")]
pub enum AggregateShareMetricV1 {
    RetrievalQueries,
    RetrievalAnswered,
    /// Lanes the planner admitted, denominated by the lanes it requested.
    RetrievalLanesAdmitted,
    /// Final ranked candidates a lane reached, denominated by what it returned.
    RetrieverUniqueContributions,
    /// Candidates promoted into context, denominated by candidates composed.
    RetrievalContextSelected,
    /// Cataloged sources actually searched, denominated by eligible sources.
    /// Denied and unresolved sources stay in the censored/unknown columns so a
    /// denial can never be read back as an absence.
    RetrievalSourcesSearched,
    /// Context packets whose use was independently verified, denominated by
    /// packets supplied. Self-reports never enter the numerator.
    ContextIndependentlyVerifiedUse,
    /// Summed baseline-to-candidate delta of one frozen retrieval ablation.
    RetrievalAblationDelta,
    /// Consent transitions that left sharing authorized. Transitions into
    /// `Off`/`LocalOnly` are local receipts and never enter the share.
    AnalyticsConsentChanges,
    AdoptionEligible,
    AdoptionIndependentlyUseful,
    OperationLatency,
    TelemetryDropsLowerBound,
    StorageLatency,
    IndexPublication,
}

#[derive(
    Clone, Copy, Debug, Deserialize, Eq, JsonSchema, Ord, PartialEq, PartialOrd, Serialize,
)]
#[serde(rename_all = "snake_case")]
pub enum AggregateShareUnitV1 {
    Events,
    Ratio,
    Microseconds,
}

#[derive(
    Clone, Copy, Debug, Deserialize, Eq, JsonSchema, Ord, PartialEq, PartialOrd, Serialize,
)]
#[serde(rename_all = "snake_case")]
pub enum AggregateCapabilityV1 {
    Retrieval,
    Adoption,
    Runtime,
    Storage,
    Index,
    /// The analytics capability observing itself: consent lifecycle only.
    Analytics,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AggregateOutcomeV1 {
    Completed,
    Abstained,
    Cancelled,
    TimedOut,
    Failed,
    Partial,
    Unknown,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AggregateOsFamilyV1 {
    Linux,
    Macos,
    Windows,
    Other,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "value")]
pub enum AggregateShareDimensionV1 {
    Capability(AggregateCapabilityV1),
    Outcome(AggregateOutcomeV1),
    Os(AggregateOsFamilyV1),
    ProductVersion { major: u16, minor: u16 },
    Coverage(CoverageStateV1),
}

impl AggregateShareDimensionV1 {
    const fn discriminant(&self) -> u8 {
        match self {
            Self::Capability(_) => 0,
            Self::Outcome(_) => 1,
            Self::Os(_) => 2,
            Self::ProductVersion { .. } => 3,
            Self::Coverage(_) => 4,
        }
    }
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct AggregateShareCellV1 {
    pub metric: AggregateShareMetricV1,
    pub unit: AggregateShareUnitV1,
    pub dimensions: Vec<AggregateShareDimensionV1>,
    pub eligible: u64,
    pub observed: u64,
    pub completed: u64,
    pub censored: u64,
    pub unknown: u64,
    pub value: Option<f64>,
    pub coverage: CoverageStateV1,
    pub contribution_windows: u64,
}

impl AggregateShareCellV1 {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.contribution_windows < AGGREGATE_SHARE_MIN_CONTRIBUTION_WINDOWS_V1 {
            return Err("aggregate_share_contribution_floor");
        }
        if self.dimensions.len() > AGGREGATE_SHARE_MAX_DIMENSIONS_V1
            || self
                .dimensions
                .iter()
                .enumerate()
                .any(|(index, dimension)| {
                    self.dimensions[..index]
                        .iter()
                        .any(|prior| prior.discriminant() == dimension.discriminant())
                })
        {
            return Err("aggregate_share_dimensions");
        }
        if self.observed > self.eligible
            || self
                .completed
                .saturating_add(self.censored)
                .saturating_add(self.unknown)
                > self.observed
            || self.value.is_some_and(|value| !value.is_finite())
            || (self.coverage == CoverageStateV1::Known && self.value.is_none())
            || (self.coverage == CoverageStateV1::Known
                && (self.observed != self.eligible || self.censored > 0 || self.unknown > 0))
        {
            return Err("aggregate_share_counts");
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct AggregateSharePacketV1 {
    pub schema_revision: u32,
    pub descriptor_revision: String,
    pub horizon: ObservabilityHorizonV1,
    pub generated_at_micros: i64,
    pub cells: Vec<AggregateShareCellV1>,
    pub suppressed_cell_count: u64,
    pub capped_cell_count: u64,
}

impl AggregateSharePacketV1 {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.schema_revision != 1
            || self.descriptor_revision != "aggregate-share.v1"
            || self.horizon.until_micros <= self.horizon.since_micros
            || self.generated_at_micros < self.horizon.until_micros
        {
            return Err("aggregate_share_packet");
        }
        if self.cells.len() > AGGREGATE_SHARE_MAX_CELLS_V1 {
            return Err("aggregate_share_cell_limit");
        }
        self.cells
            .iter()
            .try_for_each(AggregateShareCellV1::validate)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AggregateShareExportRequestV1 {
    pub mode: AnalyticsModeV1,
    /// Local authorization input only. It is never copied into the packet.
    pub authorized_scope_ref: String,
    pub horizon: ObservabilityHorizonV1,
    pub max_cells: u16,
}

impl AggregateShareExportRequestV1 {
    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        if self.mode != AnalyticsModeV1::AggregateShare {
            return Err(ApplicationContractError::Domain(
                "aggregate_share_not_enabled".to_owned(),
            ));
        }
        if self.authorized_scope_ref.is_empty()
            || self.authorized_scope_ref.len() > 128
            || self.authorized_scope_ref.chars().any(char::is_control)
        {
            return Err(ApplicationContractError::Inconsistent {
                field: "aggregate_share.authorized_scope_ref",
            });
        }
        if self.horizon.until_micros <= self.horizon.since_micros {
            return Err(ApplicationContractError::InvalidRange {
                field: "aggregate_share.horizon",
            });
        }
        if self.max_cells == 0 || usize::from(self.max_cells) > AGGREGATE_SHARE_MAX_CELLS_V1 {
            return Err(ApplicationContractError::InvalidRange {
                field: "aggregate_share.max_cells",
            });
        }
        Ok(())
    }
}

pub trait ObservabilityAggregateExportPort: Send + Sync {
    fn export_aggregate<'a>(
        &'a self,
        request: AggregateShareExportRequestV1,
    ) -> ObservabilityFuture<'a, AggregateSharePacketV1>;
}

pub struct ObservabilityAggregateExportApplicationV1<E> {
    exporter: E,
}

impl<E> ObservabilityAggregateExportApplicationV1<E>
where
    E: ObservabilityAggregateExportPort,
{
    pub const fn new(exporter: E) -> Self {
        Self { exporter }
    }

    pub async fn export(
        &self,
        request: AggregateShareExportRequestV1,
    ) -> Result<AggregateSharePacketV1, ApplicationContractError> {
        request.validate()?;
        let packet = self.exporter.export_aggregate(request).await?;
        packet
            .validate()
            .map_err(|error| ApplicationContractError::Domain(error.to_owned()))?;
        Ok(packet)
    }
}
