//! Typed terminals for the graph and port reads whose results are their
//! catalog result schemas, plus the files they report beside the result.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::InvocationAnalyticsV1;
use crate::retrieval::{
    ContextResultV1, ImpactResultV1, NodeResultV1, PortOrderResultV1, PortStatusResultV1,
    RedundancyResultV1, RenamePreviewPrimitiveOutcomeV1, SimilarResultV1, TodosResultV1,
};

/// One graph read's typed result, tagged by its operation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "operation", content = "result", rename_all = "snake_case")]
pub enum GraphToolResultV1 {
    Context(Box<ContextResultV1>),
    Node(NodeResultV1),
    Impact(ImpactResultV1),
    Similar(SimilarResultV1),
    Redundancy(RedundancyResultV1),
    RenamePreview(RenamePreviewPrimitiveOutcomeV1),
    PortStatus(PortStatusResultV1),
    PortOrder(PortOrderResultV1),
    Todos(TodosResultV1),
}

impl GraphToolResultV1 {
    /// Decodes `operation`'s catalog result body.
    pub fn from_result_value(
        operation: tracedecay_tool_catalog::ApplicationSurfaceOperation,
        value: serde_json::Value,
    ) -> serde_json::Result<Self> {
        use tracedecay_tool_catalog::ApplicationSurfaceOperation as Operation;
        Ok(match operation {
            Operation::Context => Self::Context(serde_json::from_value(value)?),
            Operation::Node => Self::Node(serde_json::from_value(value)?),
            Operation::Impact => Self::Impact(serde_json::from_value(value)?),
            Operation::Similar => Self::Similar(serde_json::from_value(value)?),
            Operation::Redundancy => Self::Redundancy(serde_json::from_value(value)?),
            Operation::RenamePreview => Self::RenamePreview(serde_json::from_value(value)?),
            Operation::PortStatus => Self::PortStatus(serde_json::from_value(value)?),
            Operation::PortOrder => Self::PortOrder(serde_json::from_value(value)?),
            Operation::Todos => Self::Todos(serde_json::from_value(value)?),
            operation => {
                return Err(serde::de::Error::custom(format!(
                    "{} is not a graph-tool operation",
                    operation.as_str()
                )));
            }
        })
    }

    /// The result body alone, the shape its catalog result schema names.
    pub fn result_value(&self) -> serde_json::Result<serde_json::Value> {
        match self {
            Self::Context(result) => serde_json::to_value(result),
            Self::Node(result) => serde_json::to_value(result),
            Self::Impact(result) => serde_json::to_value(result),
            Self::Similar(result) => serde_json::to_value(result),
            Self::Redundancy(result) => serde_json::to_value(result),
            Self::RenamePreview(result) => serde_json::to_value(result),
            Self::PortStatus(result) => serde_json::to_value(result),
            Self::PortOrder(result) => serde_json::to_value(result),
            Self::Todos(result) => serde_json::to_value(result),
        }
    }
}

/// A completed graph read: the typed result and what it reports beside it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GraphToolCompletionV1 {
    pub result: GraphToolResultV1,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub touched_files: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code_graph: Option<crate::retrieval::ServedCodeGraphGenerationV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub analytics: Option<InvocationAnalyticsV1>,
}
