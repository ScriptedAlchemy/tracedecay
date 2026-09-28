//! Canonical CLI/MCP wire contracts for the project-info and runtime reads:
//! `tracedecay_status`, `tracedecay_active_project`, `tracedecay_remote_status`,
//! and `tracedecay_runtime`, which the project's graph-tool owner answers, and
//! the profile project-registry reads (`tracedecay_project_list`,
//! `tracedecay_project_search`, `tracedecay_project_context`), which the
//! daemon's profile owner answers.
//!
//! Presentation-only transport keys such as `format` are removed before these
//! request bodies are decoded. Sections whose authority lives above this crate
//! (the generation census, storage telemetry, branch diagnostics, session
//! ingest health, GitHub source) cross as that authority's own serialized
//! value.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracedecay_domain::errors::StoreResetRequiredV1;

use crate::code_index_freshness::{
    CodeIndexReadinessWaitOutcomeV1, CodeIndexReadinessWaitV1, CodeIndexWorktreeFreshnessV1,
};
use crate::doctor::{DoctorReportV1, LanguageServerReadV1, ResidentMemoryHolderReadV1};
use crate::project_registry::{ProjectRegistrySummary, ProjectRepoGroup, PublicCodeProject};
use crate::storage::{SchemaConvergenceFindingV1, TableGrowthDoctorEvidenceV1};

#[derive(Clone, Debug, Default, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StatusSurfaceRequestV1 {
    /// Report only that the project route is admitted, reading no graph,
    /// index, or session state. Default false.
    #[serde(default)]
    pub admission_only: bool,
    /// Full tracked-branch diagnostic list. Default false.
    #[serde(default)]
    pub include_branch_diagnostics: bool,
    /// Storage-health snapshot. Default false.
    #[serde(default)]
    pub include_storage_health: bool,
    /// Session-ingest health. Default false.
    #[serde(default)]
    pub include_session_ingest: bool,
    /// Git staleness object. Default false.
    #[serde(default)]
    pub include_staleness: bool,
    /// Hold the status read until the code index reaches `state` (`fresh`:
    /// status `current`; `ready`: also native graph serving; `graph_ready`: a
    /// published generation's native graph serves, whatever the freshness),
    /// for at most `timeout_ms`. The payload's `wait` reports `reached`,
    /// `timed_out` with `last_state`, or `unavailable` with `reason`.
    pub wait_for: Option<CodeIndexReadinessWaitV1>,
}

/// `tracedecay_status` for an `admission_only` read, or the project's status.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(untagged)]
pub enum StatusResultV1 {
    Admission(StatusAdmissionV1),
    Project(Box<ProjectStatusV1>),
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StatusAdmissionV1 {
    pub project_admitted: bool,
    pub project_root: String,
    /// The serving MCP server's request counters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope_prefix: Option<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectStatusV1 {
    pub project_root: String,
    /// The exact-scope sealed-generation census.
    pub graph_statistics: Value,
    pub memory: StatusMemoryV1,
    pub schema_convergence: StatusSchemaConvergenceV1,
    /// Registered stores this binary serves only as a typed reset-required
    /// refusal, each with the command that resets it.
    pub reset_required_stores: Vec<StoreResetRequiredV1>,
    pub code_index_freshness: StatusCodeIndexFreshnessV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code_index_freshness_warning: Option<String>,
    /// Whether exact-scope retrieval can serve, when the census authority is
    /// attached.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retrieval_serving: Option<StatusRetrievalServingV1>,
    /// The checkout's GitHub source as its advisory owner observed it.
    pub github_source: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storage_health: Option<Value>,
    /// The serving MCP server's request counters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch_diagnostics: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub live_branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub serving_branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch_drifted: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch_resolution: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tracked_branch_count: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch_mismatch: Option<StatusBranchMismatchV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch_warnings: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_ingest: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_history_catch_up: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_staleness: Option<StatusGitStalenessV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope_prefix: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wait: Option<CodeIndexReadinessWaitOutcomeV1>,
}

/// The daemon's resident memory as one project sees it.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StatusMemoryV1 {
    pub status: StatusMemoryPressureV1,
    pub resident_bytes: Option<u64>,
    pub limit_bytes: u64,
    pub high_watermark_bytes: u64,
    pub low_watermark_bytes: u64,
    pub psi_some_avg10: Option<f64>,
    pub idle_window_seconds: u64,
    pub shed_order: Vec<String>,
    pub retained_bytes: u64,
    pub unmeasured_owners: usize,
    /// This project's retained owners; other projects' belong to the
    /// daemon-wide Doctor inventory.
    pub owners: Vec<StatusMemoryOwnerV1>,
}

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StatusMemoryPressureV1 {
    Unobserved,
    Nominal,
    OverBudget,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StatusMemoryOwnerV1 {
    pub project_id: String,
    pub kind: String,
    pub holders: Vec<ResidentMemoryHolderReadV1>,
    pub content_digest: Option<String>,
    pub bytes: Option<u64>,
    pub measured: bool,
    pub idle_seconds: u64,
    pub protected: bool,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StatusSchemaConvergenceV1 {
    pub status: StatusSchemaConvergenceStateV1,
    pub findings: Vec<SchemaConvergenceFindingV1>,
}

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StatusSchemaConvergenceStateV1 {
    Completed,
    InProgress,
    Degraded,
}

/// The code-index freshness of the served worktree, labeled the way readiness
/// waits report their last state.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum StatusCodeIndexFreshnessV1 {
    Current {
        worktree: Box<CodeIndexWorktreeFreshnessV1>,
    },
    Stale {
        worktree: Box<CodeIndexWorktreeFreshnessV1>,
    },
    Restoring {
        worktree: Box<CodeIndexWorktreeFreshnessV1>,
    },
    Warming {
        worktree: Box<CodeIndexWorktreeFreshnessV1>,
    },
    Parked {
        worktree: Box<CodeIndexWorktreeFreshnessV1>,
    },
    Unavailable {
        reason: String,
    },
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum StatusRetrievalServingV1 {
    /// A sealed complete generation serves the exact worktree. The ages tell a
    /// routine rebuild window from a wedged route.
    Serving {
        freshness: StatusServingFreshnessV1,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        condition: Option<StatusServingConditionV1>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        seated_generation_age_seconds: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        last_reconcile_age_seconds: Option<i64>,
    },
    /// The census answered and nothing is servable yet.
    Unavailable { reason: String },
}

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StatusServingFreshnessV1 {
    Current,
    Restoring,
    LastCompleteStale,
    Unknown,
}

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StatusServingConditionV1 {
    ArtifactRestore,
    SourceVerification,
    Rebuilding,
    Stalled,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StatusBranchMismatchV1 {
    pub git_branch: Option<String>,
    pub indexed_branch: Option<String>,
    pub serving_branch: Option<String>,
}

/// The sealed generation's Git watermark, the commit its source snapshot was
/// captured from, against the worktree's checked-out commit.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum StatusGitStalenessV1 {
    /// HEAD is the commit the sealed generation was built from.
    Current { watermark: String },
    /// HEAD has moved off the commit the sealed generation was built from.
    Stale { watermark: String, head: String },
    /// No watermark or no HEAD to compare it with.
    Unavailable {
        reason: StatusGitStalenessUnavailableV1,
    },
}

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StatusGitStalenessUnavailableV1 {
    /// No complete generation has sealed for the worktree yet.
    NoSealedGeneration,
    /// The sealed generation captured no commit, as in an unborn repository.
    SealedGenerationHasNoCommit,
    /// The worktree's checked-out commit could not be read.
    GitHeadUnreadable,
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ActiveProjectSurfaceRequestV1 {}

/// The resolved active project of this MCP session.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ActiveProjectResultV1 {
    pub project_id: Option<String>,
    pub repository_id: String,
    pub project_root: String,
    pub resolution_source: ActiveProjectResolutionSourceV1,
    pub storage: ActiveProjectStorageV1,
    pub branch: ActiveProjectBranchV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope_prefix: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ActiveProjectResolutionSourceV1 {
    ActiveProject,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ActiveProjectStorageV1 {
    pub class: String,
    pub mode: String,
    pub data_root: String,
    pub graph_db_path: String,
    pub graph_db_exists: bool,
    pub graph_db_size_bytes: u64,
    pub sessions_db_path: String,
    pub response_handle_root: String,
    pub lcm_payload_root: String,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ActiveProjectBranchV1 {
    pub current_branch: Option<String>,
    pub open_active_branch: Option<String>,
    pub serving_branch: Option<String>,
    pub branch_resolution: String,
    pub branch_drifted: bool,
    pub tracked_branch_count: usize,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteStatusSurfaceRequestV1 {}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeSurfaceRequestV1 {
    /// Run the exhaustive observation-authority audit and include
    /// authority_audit_ok (true = audit ran and passed, false = audit ran and
    /// failed, null = audit did not run), authority_audit_reason (typed:
    /// authority_invariant_failed, authority_store_unavailable,
    /// authority_store_missing, authority_audit_not_run), and
    /// authority_audit_error (observed detail) in database telemetry. Also
    /// includes session-temporal health. Default false.
    #[serde(default)]
    pub authority_audit: bool,
    /// Include session-temporal health without the authority audit. Default
    /// false.
    #[serde(default)]
    pub session_temporal_health: bool,
    /// Include the daemon-owned canonical Doctor report and typed per-table
    /// growth evidence. Default false.
    #[serde(default)]
    pub doctor_report: bool,
    /// Include Cursor transcript-ingest health from the daemon-retained
    /// project session authority. Default false.
    #[serde(default)]
    pub session_ingest_health: bool,
    /// Ask a connection's first request for only the daemon-mounted database
    /// integrity telemetry, for post-update startup validation; the daemon
    /// core answers that probe before a project owner exists, and a project
    /// owner's snapshot already carries the same telemetry. Default false.
    #[serde(default)]
    pub startup_health: bool,
}

/// Process, database, and session-observation telemetry for the running
/// daemon, with the sections the request opted into.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeResultV1 {
    /// Unix epoch seconds.
    pub captured_at: u64,
    pub tracedecay_version: String,
    pub host_os: String,
    /// Cached process sample.
    pub process: Value,
    /// Store telemetry, with the authority-audit verdict when it ran.
    pub database: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_temporal_health: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor_session_ingest: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor_session_placeholder_paths: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doctor_report: Option<RuntimeDoctorReportV1>,
}

/// The daemon's canonical Doctor report as this route could read it.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RuntimeDoctorReportV1 {
    Observed {
        report: DoctorReportV1,
        table_growth_evidence: Vec<TableGrowthDoctorEvidenceV1>,
        schema_convergences: Vec<SchemaConvergenceFindingV1>,
        language_servers: LanguageServerReadV1,
    },
    /// The reader is mounted and the read failed.
    Unknown {
        table_growth_evidence: Vec<TableGrowthDoctorEvidenceV1>,
        schema_convergences: Vec<SchemaConvergenceFindingV1>,
        language_servers: LanguageServerReadV1,
    },
    /// No Doctor reader is mounted on this route.
    Unsupported {
        table_growth_evidence: Vec<TableGrowthDoctorEvidenceV1>,
        schema_convergences: Vec<SchemaConvergenceFindingV1>,
        language_servers: LanguageServerReadV1,
    },
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectListSurfaceRequestV1 {
    /// Maximum projects to return (default: 25, max: 100).
    pub limit: Option<usize>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectSearchSurfaceRequestV1 {
    /// Case-insensitive substring query over registry project metadata.
    pub query: String,
    /// Maximum projects to return (default: 10, max: 50).
    pub limit: Option<usize>,
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectContextSurfaceRequestV1 {
    /// Registered project id to inspect. Omit to use path or the active
    /// project.
    #[serde(default)]
    #[schemars(with = "RegisteredProjectIdSelectorV1")]
    pub project_selector: Option<RegisteredProjectIdSelectorV1>,
    /// Project path or registered alias to resolve.
    pub path: Option<String>,
}

/// The selector is inlined: the MCP selector policy reads its shape in place.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
#[schemars(inline)]
pub struct RegisteredProjectIdSelectorV1 {
    /// Registered project id to query.
    pub project_id: String,
}

/// A bounded page of the profile's registered projects.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum ProjectRegistryListingResultV1 {
    Ok {
        title: String,
        registry_path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        query: Option<String>,
        limit: usize,
        truncated: bool,
        summary: ProjectRegistrySummary,
        project_tree: Vec<ProjectRepoGroup>,
        projects: Vec<PublicCodeProject>,
    },
}

/// One registered project's context, or the registry's typed miss.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum ProjectContextResultV1 {
    Ok {
        is_active: bool,
        registry_path: String,
        project: Box<PublicCodeProject>,
        /// Alias rows exactly as the registry serializes them.
        aliases: Vec<Value>,
        /// Store rows exactly as the registry serializes them.
        stores: Vec<Value>,
    },
    /// The registry answered and no registered project matches the selector.
    NotFound {
        registry_path: String,
        project: (),
        aliases: Vec<Value>,
        stores: Vec<Value>,
    },
}
