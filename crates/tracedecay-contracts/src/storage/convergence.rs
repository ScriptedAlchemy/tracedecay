use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SchemaConvergenceStageV1 {
    RegisteredSchema,
}

impl SchemaConvergenceStageV1 {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::RegisteredSchema => "registered_schema",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SchemaConvergenceStateV1 {
    Pending,
    Running,
    Degraded,
    Completed,
}

impl SchemaConvergenceStateV1 {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Degraded => "degraded",
            Self::Completed => "completed",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SchemaConvergenceFindingV1 {
    pub store: String,
    pub stage: SchemaConvergenceStageV1,
    pub state: SchemaConvergenceStateV1,
    pub started_at_micros: i64,
    pub degraded_row: Option<String>,
}
