//! Read-only bridge over Hermes-owned skill lifecycle state.
//!
//! Hermes owns its profile skill directory, write-approval staging, provenance,
//! and curator decisions. `TraceDecay` only projects that state for dashboards and
//! MCP callers that need one normalized read surface.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::errors::{Result, TraceDecayError};

use super::hermes_config_projection::load_config_projection;
use super::hermes_pending_skills::{load_pending_skill_writes, pending_skill_ids_by_name};
use super::hermes_skill_inventory::{
    count_archive_entries, load_skill_ownership_projection, load_skill_summaries,
    load_usage_records, USAGE_FILE,
};

pub use super::hermes_config_projection::{
    HermesAuxiliaryTaskProjection, HermesConfigProjection, HermesCuratorConfigProjection,
    HermesSelfImprovementConfigProjection, HermesWriteApprovalConfigProjection,
};
pub use super::hermes_pending_skills::HermesPendingSkillWrite;
pub use super::hermes_skill_inventory::{HermesSkillOwnershipProjection, HermesSkillSummary};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HermesSkillBridgeSnapshot {
    pub hermes_home: PathBuf,
    pub skills_dir: PathBuf,
    pub skill_count: usize,
    pub pending_skill_count: usize,
    pub usage_record_count: usize,
    pub archive_count: usize,
    pub skills: Vec<HermesSkillSummary>,
    pub pending_skills: Vec<HermesPendingSkillWrite>,
    pub usage_records: BTreeMap<String, Value>,
    pub contracts: HermesBridgeContracts,
    pub config: HermesConfigProjection,
    pub state: HermesStateProjection,
    pub curator: HermesCuratorProjection,
    pub background_review: HermesBackgroundReviewProjection,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HermesBridgeContracts {
    pub lifecycle_owner: String,
    pub write_approval_store: String,
    pub usage_store: String,
    pub background_review_origin: String,
    pub mutation_policy: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HermesStateProjection {
    pub state_db_path: PathBuf,
    pub hermes_state_db_path: PathBuf,
    pub profile_lcm_db_path: PathBuf,
    pub trace_decay_lcm_store_path: PathBuf,
    pub exists: bool,
    pub projection_policy: String,
    pub state_db_projection_policy: String,
    pub raw_lcm_owner: String,
    pub hermes_state_owner: String,
    pub session_db_owner: String,
    pub profile_lcm_store_owner: String,
    pub trace_decay_lcm_store_owner: String,
    pub trace_decay_lcm_role: String,
    pub trace_decay_ingest_role: String,
    pub projected_tables: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HermesCuratorProjection {
    pub owner: String,
    pub trace_decay_role: String,
    pub standalone_automation_blocked: bool,
    pub pending_skill_dir: PathBuf,
    pub usage_path: PathBuf,
    pub archive_dir: PathBuf,
    pub state_path: PathBuf,
    pub state: HermesCuratorStateProjection,
    pub reports_dir: PathBuf,
    pub policy: HermesCuratorPolicyProjection,
    pub config_source: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HermesCuratorStateProjection {
    pub exists: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub paused: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_run_at: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_run_summary: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_report_path: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_count: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HermesCuratorPolicyProjection {
    pub eligible_provenance: Vec<String>,
    pub pinned_exempt: bool,
    pub hub_installed_off_limits: bool,
    pub max_destructive_action: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HermesBackgroundReviewProjection {
    pub owner: String,
    pub origin: String,
    pub memory_nudge_interval: u64,
    pub skill_nudge_interval: u64,
    pub runtime_counters_projected: bool,
    pub counter_owner: String,
}

impl Default for HermesBridgeContracts {
    fn default() -> Self {
        Self {
            lifecycle_owner: "hermes".to_string(),
            write_approval_store: "pending/skills/*.json".to_string(),
            usage_store: "skills/.usage.json".to_string(),
            background_review_origin: "background_review".to_string(),
            mutation_policy:
                "read_only_bridge; TraceDecay must not create, edit, approve, archive, or delete Hermes skills"
                    .to_string(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HermesSkillBridgeOptions {
    pub include_skill_bodies: bool,
    pub include_pending_payloads: bool,
}

pub fn load_hermes_skill_bridge(
    hermes_home: &Path,
    options: HermesSkillBridgeOptions,
) -> Result<HermesSkillBridgeSnapshot> {
    if hermes_home.as_os_str().is_empty() || !hermes_home.is_absolute() {
        return Err(config_error("hermes_home must be an absolute path"));
    }

    let skills_dir = hermes_home.join("skills");
    let usage_records = load_usage_records(&skills_dir)?;
    let pending_skills = load_pending_skill_writes(hermes_home, options.include_pending_payloads)?;
    let pending_by_skill = pending_skill_ids_by_name(&pending_skills);
    let skill_ownership = load_skill_ownership_projection(hermes_home, &usage_records)?;
    let skills = load_skill_summaries(
        &skills_dir,
        &usage_records,
        &pending_by_skill,
        &skill_ownership,
        options.include_skill_bodies,
    )?;
    let archive_dir = skills_dir.join(".archive");
    let archive_count = count_archive_entries(&archive_dir)?;
    let config = load_config_projection(hermes_home)?;
    let state_path = skills_dir.join(".curator_state");
    let curator_state = load_curator_state_projection(&state_path)?;
    let background_review = HermesBackgroundReviewProjection {
        owner: "hermes_runtime".to_string(),
        origin: "background_review".to_string(),
        memory_nudge_interval: config.self_improvement.memory_nudge_interval,
        skill_nudge_interval: config.self_improvement.skill_creation_nudge_interval,
        runtime_counters_projected: false,
        counter_owner: "live_hermes_agent_runtime".to_string(),
    };
    let hermes_state_db_path = hermes_home.join("state.db");
    let profile_lcm_db_path = hermes_home.join(".tracedecay").join("sessions.db");
    let state = HermesStateProjection {
        state_db_path: hermes_state_db_path.clone(),
        hermes_state_db_path: hermes_state_db_path.clone(),
        profile_lcm_db_path: profile_lcm_db_path.clone(),
        trace_decay_lcm_store_path: profile_lcm_db_path,
        exists: hermes_state_db_path.is_file(),
        projection_policy: "session_messages_only".to_string(),
        state_db_projection_policy: "read_only_session_message_projection".to_string(),
        raw_lcm_owner: "hermes_runtime".to_string(),
        hermes_state_owner: "hermes_runtime".to_string(),
        session_db_owner: "hermes_runtime".to_string(),
        profile_lcm_store_owner: "tracedecay_hermes_plugin".to_string(),
        trace_decay_lcm_store_owner: "tracedecay_hermes_plugin".to_string(),
        trace_decay_lcm_role: "hermes_profile_session_store".to_string(),
        trace_decay_ingest_role: "read_only_session_message_projector".to_string(),
        projected_tables: vec!["sessions".to_string(), "session_messages".to_string()],
    };
    let curator = HermesCuratorProjection {
        owner: "hermes".to_string(),
        trace_decay_role: "read_only_projector".to_string(),
        standalone_automation_blocked: true,
        pending_skill_dir: hermes_home.join("pending").join("skills"),
        usage_path: skills_dir.join(USAGE_FILE),
        archive_dir,
        state_path,
        state: curator_state,
        reports_dir: hermes_home.join("logs").join("curator"),
        policy: HermesCuratorPolicyProjection {
            eligible_provenance: vec!["agent".to_string(), "agent_created".to_string()],
            pinned_exempt: true,
            hub_installed_off_limits: true,
            max_destructive_action: "archive".to_string(),
        },
        config_source: "config.yaml:curator".to_string(),
    };

    Ok(HermesSkillBridgeSnapshot {
        hermes_home: hermes_home.to_path_buf(),
        skills_dir,
        skill_count: skills.len(),
        pending_skill_count: pending_skills.len(),
        usage_record_count: usage_records.len(),
        archive_count,
        skills,
        pending_skills,
        usage_records,
        contracts: HermesBridgeContracts::default(),
        config,
        state,
        curator,
        background_review,
    })
}

fn load_curator_state_projection(path: &Path) -> Result<HermesCuratorStateProjection> {
    let contents = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(HermesCuratorStateProjection {
                exists: false,
                paused: Some(false),
                last_run_at: None,
                last_run_summary: None,
                last_report_path: None,
                run_count: Some(0),
            });
        }
        Err(e) => {
            return Err(config_error(format!(
                "failed to read Hermes curator state '{}': {e}",
                path.display()
            )));
        }
    };
    let value: Value = serde_json::from_str(&contents).map_err(|e| {
        config_error(format!(
            "failed to parse Hermes curator state '{}': {e}",
            path.display()
        ))
    })?;
    Ok(HermesCuratorStateProjection {
        exists: true,
        paused: value.get("paused").and_then(Value::as_bool),
        last_run_at: value.get("last_run_at").cloned(),
        last_run_summary: value.get("last_run_summary").cloned(),
        last_report_path: value.get("last_report_path").cloned(),
        run_count: value.get("run_count").and_then(Value::as_u64),
    })
}

fn config_error(message: impl Into<String>) -> TraceDecayError {
    TraceDecayError::Config {
        message: message.into(),
    }
}
