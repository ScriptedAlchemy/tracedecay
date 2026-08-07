//! Application-owned read models for dashboard code-graph journeys.
//!
//! Dashboard and HTTP adapters receive complete bounded results through this
//! port. They never receive a database path, connection, Grafeo handle,
//! mutable publisher, or unverified generation.

use std::future::Future;
use std::pin::Pin;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_domain::{
    CodeGenerationId, ManifestDigest, ProjectId, RepositoryId, UserProfileId, WorktreeId,
};

use crate::ResolvedScope;

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct VerifiedDashboardGraphGenerationV1 {
    pub project_id: ProjectId,
    pub profile_id: UserProfileId,
    pub repository_id: RepositoryId,
    pub worktree_id: WorktreeId,
    pub code_generation_id: CodeGenerationId,
    pub graph_generation_id: String,
    pub projection_id: String,
    pub recovered_state_digest: ManifestDigest,
}

impl VerifiedDashboardGraphGenerationV1 {
    pub fn matches_scope(&self, scope: &ResolvedScope) -> bool {
        self.project_id == scope.project_id
            && self.repository_id == scope.repository_id
            && self.worktree_id == scope.worktree_id
    }

    pub fn cache_identity(&self) -> String {
        format!(
            "{}:{}:{}:{}",
            self.project_id, self.profile_id, self.graph_generation_id, self.recovered_state_digest
        )
    }
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DashboardGraphSpanV1 {
    pub start_line: i64,
    pub end_line: i64,
    pub start_column: i64,
    pub end_column: i64,
    pub attrs_start_line: i64,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DashboardGraphNodeV1 {
    pub id: String,
    pub kind: String,
    pub name: Option<String>,
    pub qualified_name: Option<String>,
    pub file_path: Option<String>,
    pub start_line: Option<i64>,
    pub end_line: Option<i64>,
    pub start_column: Option<i64>,
    pub end_column: Option<i64>,
    pub attrs_start_line: Option<i64>,
    pub doc: Option<String>,
    pub signature: Option<String>,
    pub visibility: Option<String>,
    pub is_async: Option<i64>,
    pub branches: Option<i64>,
    pub loops: Option<i64>,
    pub returns: Option<i64>,
    pub max_nesting: Option<i64>,
    pub unsafe_blocks: Option<i64>,
    pub unchecked_calls: Option<i64>,
    pub assertions: Option<i64>,
    pub updated_at: Option<i64>,
    pub parent_id: Option<String>,
    pub degree: Option<i64>,
    pub span: Option<DashboardGraphSpanV1>,
    pub edge_kind: Option<String>,
    pub edge_line: Option<i64>,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DashboardGraphEdgeV1 {
    pub source: String,
    pub target: String,
    pub kind: String,
    pub line: Option<i64>,
    pub source_name: Option<String>,
    pub target_name: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DashboardGraphKindCountV1 {
    pub kind: String,
    pub count: i64,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DashboardGraphLanguageCountV1 {
    pub language: String,
    pub count: i64,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DashboardGraphLargestFileV1 {
    pub path: String,
    pub node_count: i64,
    pub size: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DashboardGraphTotalsV1 {
    pub nodes: u64,
    pub edges: u64,
    pub files: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DashboardGraphOverviewV1 {
    pub totals: DashboardGraphTotalsV1,
    pub nodes_by_kind: Vec<DashboardGraphKindCountV1>,
    pub edges_by_kind: Vec<DashboardGraphKindCountV1>,
    pub files_by_language: Vec<DashboardGraphLanguageCountV1>,
    pub largest_files: Vec<DashboardGraphLargestFileV1>,
    pub top_connected: Vec<DashboardGraphNodeV1>,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DashboardGraphSearchV1 {
    pub query: String,
    pub limit: i64,
    pub offset: i64,
    pub total: i64,
    pub results: Vec<DashboardGraphNodeV1>,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DashboardGraphNeighborsV1 {
    pub node_id: String,
    pub callers: Vec<DashboardGraphNodeV1>,
    pub callees: Vec<DashboardGraphNodeV1>,
    pub edges: Vec<DashboardGraphEdgeV1>,
    pub edges_by_kind: Vec<DashboardGraphKindCountV1>,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DashboardGraphSubgraphV1 {
    pub seed_id: Option<String>,
    pub mode: String,
    pub nodes: Vec<DashboardGraphNodeV1>,
    pub edges: Vec<DashboardGraphEdgeV1>,
    pub nodes_capped: bool,
    pub edges_capped: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DashboardGraphPathV1 {
    pub from: String,
    pub to: String,
    pub found: bool,
    pub path: Vec<String>,
    pub nodes: Vec<DashboardGraphNodeV1>,
    pub edges: Vec<DashboardGraphEdgeV1>,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "value")]
pub enum DashboardGraphReadPayloadV1 {
    Overview(DashboardGraphOverviewV1),
    Search(DashboardGraphSearchV1),
    Node(Option<DashboardGraphNodeV1>),
    Neighbors(Option<DashboardGraphNeighborsV1>),
    Subgraph(DashboardGraphSubgraphV1),
    Path(DashboardGraphPathV1),
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum DashboardGraphReadOperationV1 {
    Overview,
    Search {
        query: String,
        limit: i64,
        offset: i64,
    },
    Node {
        node_id: String,
    },
    Neighbors {
        node_id: String,
        limit: i64,
    },
    Subgraph {
        node_id: Option<String>,
        query: String,
        node_limit: i64,
        edge_limit: i64,
    },
    Path {
        from: String,
        to: String,
        max_depth: i64,
    },
}

#[derive(Clone, Debug)]
pub struct DashboardGraphReadRequestV1 {
    pub scope: ResolvedScope,
    pub operation: DashboardGraphReadOperationV1,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DashboardGraphReadV1 {
    pub generation: VerifiedDashboardGraphGenerationV1,
    pub payload: DashboardGraphReadPayloadV1,
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum DashboardGraphReadErrorV1 {
    #[error("the exact project graph registry is missing")]
    MissingRegistry,
    #[error("the exact project graph authority is unavailable: {detail}")]
    Unavailable { detail: String },
    #[error("the requested graph generation is stale: {detail}")]
    Stale { detail: String },
    #[error("the graph read was cancelled")]
    Cancelled,
    #[error("the graph read timed out")]
    TimedOut,
    #[error("the graph read is not authorized")]
    Denied,
    #[error("the graph read request is invalid: {detail}")]
    InvalidRequest { detail: String },
    #[error("the verified graph projection is corrupt: {detail}")]
    Corrupt { detail: String },
}

pub type DashboardGraphReadFutureV1<'a> = Pin<
    Box<dyn Future<Output = Result<DashboardGraphReadV1, DashboardGraphReadErrorV1>> + Send + 'a>,
>;

pub trait DashboardGraphReadPortV1: Send + Sync {
    fn read<'a>(&'a self, request: DashboardGraphReadRequestV1) -> DashboardGraphReadFutureV1<'a>;
}
