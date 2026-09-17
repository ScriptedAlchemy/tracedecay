use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SchemaConvergenceStageV1 {
    RegisteredSchema,
    RuntimeWriterLedger,
}

impl SchemaConvergenceStageV1 {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::RegisteredSchema => "registered_schema",
            Self::RuntimeWriterLedger => "runtime_writer_ledger",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SchemaConvergenceStateV1 {
    PendingSchemaMigration,
    ReleasedShapeConvergenceInProgress,
    Degraded,
    Completed,
}

impl SchemaConvergenceStateV1 {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PendingSchemaMigration => "pending_schema_migration",
            Self::ReleasedShapeConvergenceInProgress => "released_shape_convergence_in_progress",
            Self::Degraded => "degraded",
            Self::Completed => "completed",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "unit", rename_all = "snake_case", deny_unknown_fields)]
pub enum SchemaConvergenceProgressV1 {
    Rows { done: u64, remaining: u64 },
    Pages { done: u64, remaining: u64 },
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SchemaConvergenceFindingV1 {
    pub store: String,
    pub stage: SchemaConvergenceStageV1,
    pub state: SchemaConvergenceStateV1,
    pub progress: Option<SchemaConvergenceProgressV1>,
    pub started_at_micros: i64,
    pub degraded_row: Option<String>,
}
