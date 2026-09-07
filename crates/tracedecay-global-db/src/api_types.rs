use std::path::Path;

use tracedecay_runtime_core::storage::{ProjectStorageLocation, classify_registry_storage_fields};

pub use tracedecay_sessions::runtime::{
    SessionActivityRow, SessionIngestHealth, SessionProviderCoverage, SessionProviderCoverageState,
    TranscriptBatch,
};

/// Total savings + call count for a project (or all projects when `project` is None).
#[derive(Debug, Clone, serde::Serialize)]
pub struct SavingsTotal {
    pub saved_tokens: u64,
    pub calls: u64,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SavingsDay {
    /// Start-of-day epoch seconds (UTC).
    pub day: i64,
    pub saved_tokens: u64,
    pub calls: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnalyticsEventInsert {
    pub provider: String,
    pub project_id: String,
    pub session_id: Option<String>,
    pub timestamp: i64,
    pub event_kind: String,
    pub hook_name: Option<String>,
    pub tool_name: Option<String>,
    pub tool_category: Option<String>,
    pub skill_name: Option<String>,
    pub hint_category: Option<String>,
    pub hint_id: Option<String>,
    pub outcome: Option<String>,
    pub metadata_json: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnalyticsEventRecord {
    pub id: i64,
    pub provider: String,
    pub project_id: String,
    pub session_id: Option<String>,
    pub timestamp: i64,
    pub event_kind: String,
    pub hook_name: Option<String>,
    pub tool_name: Option<String>,
    pub tool_category: Option<String>,
    pub skill_name: Option<String>,
    pub hint_category: Option<String>,
    pub hint_id: Option<String>,
    pub outcome: Option<String>,
    pub metadata_json: Option<String>,
}

/// Durable handoff between an owner-derived observability fact and its exact
/// producer-stamped delivery envelope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservabilityEmissionOutboxRecordV1 {
    pub project_id: String,
    pub owner_event_id: String,
    pub owner_fact_json: String,
    pub delivery_envelope_json: String,
}

/// Result of claiming one stable owner fact in the registered outbox.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ObservabilityEmissionClaimV1 {
    Claimed {
        delivery_envelope_json: String,
    },
    Pending {
        delivery_envelope_json: String,
    },
    Settled {
        delivery_envelope_json: String,
        analytics_event_id: i64,
    },
}

impl ObservabilityEmissionClaimV1 {
    pub fn delivery_envelope_json(&self) -> &str {
        match self {
            Self::Claimed {
                delivery_envelope_json,
            }
            | Self::Pending {
                delivery_envelope_json,
            }
            | Self::Settled {
                delivery_envelope_json,
                ..
            } => delivery_envelope_json,
        }
    }
}

impl tracedecay_sessions::runtime::git_correlation::AnalyticsSessionTimestampSource
    for AnalyticsEventRecord
{
    fn as_analytics_session_timestamp(
        &self,
    ) -> Option<tracedecay_sessions::runtime::git_correlation::AnalyticsSessionTimestamp> {
        Some(
            tracedecay_sessions::runtime::git_correlation::AnalyticsSessionTimestamp {
                provider: self.provider.clone(),
                session_id: self.session_id.clone()?,
                timestamp: self.timestamp,
            },
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnalyticsToolCounts {
    pub tool_name: String,
    pub calls: i64,
    pub errors: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnalyticsHintCounts {
    pub category: String,
    pub emitted: i64,
    pub followed: i64,
    pub ignored: i64,
    pub suppressed: i64,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct AnalyticsEventQuery {
    pub provider: Option<String>,
    pub project_id: Option<String>,
    pub session_id: Option<String>,
    pub event_kind: Option<String>,
    /// Inclusive lower bound on `timestamp` (unix seconds). `None` = unbounded.
    pub since: Option<i64>,
    /// Exclusive upper bound on `timestamp` (unix seconds). `None` = unbounded.
    pub until: Option<i64>,
    /// Exclusive row-id cursor used by bounded reverse-chronological scans.
    pub before_id: Option<i64>,
    pub limit: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct CodeProjectRecord {
    pub project_id: String,
    pub canonical_root: String,
    pub display_root: String,
    pub git_common_dir: Option<String>,
    pub git_remote_url: Option<String>,
    pub default_branch: Option<String>,
    pub created_at: i64,
    pub last_seen_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, schemars::JsonSchema)]
pub struct ProjectAliasRecord {
    pub alias_path: String,
    pub project_id: String,
    pub last_seen_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct StoreInstanceUpsert {
    pub store_id: String,
    pub project_id: String,
    pub store_kind: String,
    pub storage_mode: String,
    pub store_relpath: String,
    pub manifest_relpath: Option<String>,
    pub last_verified_at: Option<i64>,
    pub last_write_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, schemars::JsonSchema)]
pub struct StoreInstanceRecord {
    pub store_id: String,
    pub project_id: String,
    pub store_kind: String,
    pub storage_mode: String,
    pub store_relpath: String,
    pub manifest_relpath: Option<String>,
    pub created_at: i64,
    pub last_verified_at: Option<i64>,
    pub last_write_at: Option<i64>,
}

impl StoreInstanceRecord {
    /// Classifies this registry-recorded store instance against a profile root.
    pub fn classify_storage(
        &self,
        project_root: &Path,
        profile_root: &Path,
    ) -> Option<ProjectStorageLocation> {
        classify_registry_storage_fields(
            project_root,
            profile_root,
            &self.storage_mode,
            &self.store_relpath,
            self.manifest_relpath.as_deref(),
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct GraphScopeUpsert {
    pub graph_scope_id: String,
    pub project_id: String,
    pub store_id: String,
    pub branch_name: String,
    pub db_relpath: String,
    pub parent_scope_id: Option<String>,
    pub last_synced_at: Option<i64>,
    pub writable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, schemars::JsonSchema)]
pub struct GraphScopeRecord {
    pub graph_scope_id: String,
    pub project_id: String,
    pub store_id: String,
    pub branch_name: String,
    pub db_relpath: String,
    pub parent_scope_id: Option<String>,
    pub last_synced_at: Option<i64>,
    pub writable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct StoreArtifactUpsert {
    pub store_id: String,
    pub artifact_kind: String,
    pub relpath: String,
    pub size_bytes: Option<i64>,
    pub schema_version: Option<String>,
    pub updated_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, schemars::JsonSchema)]
pub struct StoreArtifactRecord {
    pub store_id: String,
    pub artifact_kind: String,
    pub relpath: String,
    pub size_bytes: Option<i64>,
    pub schema_version: Option<String>,
    pub updated_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ProjectStoreResolution {
    pub project: CodeProjectRecord,
    pub store: StoreInstanceRecord,
    pub graph_scopes: Vec<GraphScopeRecord>,
    pub artifacts: Vec<StoreArtifactRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, schemars::JsonSchema)]
pub struct ProjectStoreContext {
    pub store: StoreInstanceRecord,
    pub graph_scopes: Vec<GraphScopeRecord>,
    pub artifacts: Vec<StoreArtifactRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ProjectRegistryContext {
    pub project: CodeProjectRecord,
    pub aliases: Vec<ProjectAliasRecord>,
    pub stores: Vec<ProjectStoreContext>,
}

/// One complete, bounded snapshot of the registered checkout roots that may
/// still own derived storage for a project.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct RegisteredProjectRootInventoryV1 {
    pub project_id: String,
    pub roots: std::collections::BTreeSet<String>,
    pub terminal_root_count: u64,
    pub inventory_digest: tracedecay_domain::ManifestDigest,
}
