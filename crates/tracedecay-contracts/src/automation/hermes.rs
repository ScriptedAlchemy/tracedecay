//! Read-only inventory of the skills owned by the standard Hermes user
//! install. Hermes stays the lifecycle owner; these records only describe it.

use std::collections::BTreeMap;
use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct HermesSkillBridgeSnapshot {
    pub agent_home: PathBuf,
    pub skills_dir: PathBuf,
    pub skill_count: usize,
    pub pending_skill_count: usize,
    pub pending_skill_corrupt_count: usize,
    pub usage_record_count: usize,
    pub archive_count: usize,
    pub skills: Vec<HermesSkillSummary>,
    pub pending_skills: Vec<HermesPendingSkillWrite>,
    /// Hermes-owned usage telemetry keyed by skill name, as Hermes wrote it.
    pub usage_records: BTreeMap<String, Value>,
    pub contracts: HermesSkillBridgeContracts,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct HermesSkillSummary {
    pub name: String,
    pub path: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body_markdown: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Value>,
    pub pending_write_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct HermesPendingSkillWrite {
    pub id: String,
    pub source_path: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct HermesSkillBridgeContracts {
    pub lifecycle_owner: String,
    pub mutation_policy: String,
    pub discovery_policy: String,
}

impl Default for HermesSkillBridgeContracts {
    fn default() -> Self {
        Self {
            lifecycle_owner: "hermes".to_string(),
            mutation_policy: "read_only; use Hermes to mutate Hermes-owned skills".to_string(),
            discovery_policy: "standard_user_install_only".to_string(),
        }
    }
}
