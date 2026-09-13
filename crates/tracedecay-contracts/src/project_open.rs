use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectOpenStatusStateV1 {
    Converging,
    Completed,
    Stalled,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectOpenStatusReasonV1 {
    Converging,
    Ready,
    UnrepairableVerdict,
    DeferredRepositoryDiscovery,
    RetryBackoff,
    Unavailable,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectOpenStatusV1 {
    pub state: ProjectOpenStatusStateV1,
    pub reason: ProjectOpenStatusReasonV1,
    pub retry_after_ms: Option<u64>,
    pub detail: Option<String>,
}
