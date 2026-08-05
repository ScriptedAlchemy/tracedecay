//! Storage retention, size, and efficiency read models (Plan 38 §5–§7).
//!
//! This module owns the read models and policies *behind* the landed Doctor
//! `Storage` finding contract (`crate::doctor::DoctorStorageFindingKindV1`,
//! commit be3a113f). It never redefines that contract; it produces its findings.
//!
//! Layout:
//! - [`identity`]: bounded store/table/path identifiers plus byte-size
//!   and free-page-ratio primitives.
//! - [`telemetry`] (§7): per-store size, per-table growth, free-page ratio, soft
//!   budgets, and the [`telemetry::StoreSizeTelemetryPort`] seam over
//!   `dbstat`/pragma sources.
//! - [`debris`] (§5): incident-artifact classification, the quarantine-location
//!   contract, and debris scan read models.
//! - [`compaction`] (§6): the free-page-ratio compaction trigger policy, off the
//!   hot path by construction.
//! - [`inventory`]: orphan / retention-backlog read models.
//! - [`findings`]: pure producers mapping the read models onto
//!   [`crate::doctor::DoctorFindingV1`] with honest evidence states.
//!
//! This crate owns no store or runtime; the telemetry port implementation is a
//! reported seam in the storage runtime crate (see [`telemetry`]).

pub mod compaction;
pub mod debris;
pub mod findings;
pub mod identity;
pub mod inventory;
pub mod telemetry;

pub use compaction::{CompactionDecisionV1, CompactionPlacementV1, CompactionTriggerPolicyV1};
pub use debris::{
    IncidentDebrisArtifactV1, IncidentDebrisKindV1, IncidentDebrisScanV1, QuarantineContractV1,
    QuarantinedArtifactV1,
};
pub use findings::{
    code_generation_retention_finding, incident_debris_finding, orphan_store_finding,
    over_budget_finding, retention_backlog_finding, table_growth_finding,
};
pub use identity::{
    FreePageRatioV1, QuarantineLocationV1, RelativeArtifactPathV1, StorageByteSizeV1, StoreKeyV1,
    TableNameV1,
};
pub use inventory::{
    CodeGenerationRetentionRecordV1, OrphanStoreRecordV1, RetentionBacklogRecordV1,
};
pub use telemetry::{
    SIGNIFICANT_TABLE_GROWTH_ABSOLUTE_BYTES, SIGNIFICANT_TABLE_GROWTH_PERCENT,
    SIGNIFICANT_TABLE_GROWTH_RELATIVE_FLOOR_BYTES, StorageTelemetryFuture, StorageTelemetryReadV1,
    StoreBudgetEvaluationV1, StoreSizeBudgetV1, StoreSizeSampleV1, StoreSizeTelemetryPort,
    TableGrowthBaselinePendingV1, TableGrowthDoctorEvidenceV1, TableGrowthSampleV1,
    TableGrowthTelemetryReadV1, is_significant_table_growth, table_growth_doctor_evidence,
};
