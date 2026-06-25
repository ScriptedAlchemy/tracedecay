use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::managed_skills::{ManagedSkill, ManagedSkillSource, ManagedSkillState};
use crate::errors::{Result, TraceDecayError};
use crate::tracedecay::current_timestamp;

mod analytics;
mod recommendations;

pub(crate) use analytics::analytics_import_key_for_request;
pub use analytics::{ingest_analytics_events, ingest_project_analytics_events};
pub use recommendations::{skill_improvement_recommendations, stale_skill_recommendations};

const SKILL_USAGE_LEDGER_FILENAME: &str = "skill_usage.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillUsageAction {
    View,
    Use,
    Patch,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillUsageEvent {
    pub skill_name: String,
    pub action: SkillUsageAction,
    pub timestamp: i64,
    pub target: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillUsageRecord {
    pub schema_version: u32,
    pub skill_id: String,
    pub title: Option<String>,
    pub category: Option<String>,
    pub state: Option<ManagedSkillState>,
    pub pinned: bool,
    pub created_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance_source: Option<ManagedSkillSource>,
    #[serde(default)]
    pub targets: Vec<String>,
    pub view_count: u64,
    pub use_count: u64,
    pub patch_count: u64,
    pub first_seen_at: i64,
    pub last_activity_at: i64,
    pub last_viewed_at: Option<i64>,
    pub last_used_at: Option<i64>,
    pub last_patched_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillUsageLedger {
    pub schema_version: u32,
    #[serde(default)]
    pub records: BTreeMap<String, SkillUsageRecord>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub imported_analytics_events: BTreeSet<String>,
}

pub type SkillUsageSummary = SkillUsageRecord;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillStaleRecommendation {
    pub skill_id: String,
    pub stale: bool,
    pub recommendation: String,
    pub reason: String,
    #[serde(default)]
    pub evidence: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillImprovementRecommendation {
    pub skill_id: String,
    pub improvement: bool,
    pub recommendation: String,
    pub reason: String,
    pub priority: String,
    #[serde(default)]
    pub evidence: Vec<String>,
}

impl Default for SkillUsageLedger {
    fn default() -> Self {
        Self {
            schema_version: 1,
            records: BTreeMap::new(),
            imported_analytics_events: BTreeSet::new(),
        }
    }
}

impl SkillUsageRecord {
    fn new(skill_id: String, timestamp: i64) -> Self {
        Self {
            schema_version: 1,
            skill_id,
            title: None,
            category: None,
            state: None,
            pinned: false,
            created_by: None,
            provenance_source: None,
            targets: Vec::new(),
            view_count: 0,
            use_count: 0,
            patch_count: 0,
            first_seen_at: timestamp,
            last_activity_at: timestamp,
            last_viewed_at: None,
            last_used_at: None,
            last_patched_at: None,
        }
    }

    fn merge_skill_metadata(&mut self, skill: &ManagedSkill) {
        self.title = Some(skill.metadata.title.clone());
        self.category = Some(skill.metadata.category.clone());
        self.state = Some(skill.metadata.state);
        self.pinned = skill.metadata.pinned;
        self.created_by = Some(skill.metadata.provenance.actor.clone());
        self.provenance_source = Some(skill.metadata.provenance.source);
    }

    fn record(&mut self, event: &SkillUsageEvent) {
        self.first_seen_at = self.first_seen_at.min(event.timestamp);
        self.last_activity_at = self.last_activity_at.max(event.timestamp);
        if let Some(target) = event.target.as_deref().and_then(normalize_target) {
            insert_sorted_unique(&mut self.targets, target);
        }
        match event.action {
            SkillUsageAction::View => {
                self.view_count = self.view_count.saturating_add(1);
                self.last_viewed_at = Some(max_optional(self.last_viewed_at, event.timestamp));
            }
            SkillUsageAction::Use => {
                self.use_count = self.use_count.saturating_add(1);
                self.last_used_at = Some(max_optional(self.last_used_at, event.timestamp));
            }
            SkillUsageAction::Patch => {
                self.patch_count = self.patch_count.saturating_add(1);
                self.last_patched_at = Some(max_optional(self.last_patched_at, event.timestamp));
            }
        }
    }
}

pub fn skill_usage_ledger_path(profile_root: &Path) -> PathBuf {
    profile_root
        .join("agent_managed")
        .join(SKILL_USAGE_LEDGER_FILENAME)
}

pub async fn load_skill_usage_ledger(profile_root: &Path) -> Result<SkillUsageLedger> {
    let path = skill_usage_ledger_path(profile_root);
    let bytes = match tokio::fs::read(&path).await {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(SkillUsageLedger::default());
        }
        Err(e) => {
            return Err(config_error(format!(
                "failed to read skill usage ledger '{}': {e}",
                path.display()
            )));
        }
    };
    serde_json::from_slice(&bytes).map_err(|e| {
        config_error(format!(
            "failed to parse skill usage ledger '{}': {e}",
            path.display()
        ))
    })
}

pub async fn save_skill_usage_ledger(profile_root: &Path, ledger: &SkillUsageLedger) -> Result<()> {
    let path = skill_usage_ledger_path(profile_root);
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await.map_err(|e| {
            config_error(format!(
                "failed to create skill usage ledger directory '{}': {e}",
                parent.display()
            ))
        })?;
    }
    let bytes = serde_json::to_vec_pretty(ledger).map_err(TraceDecayError::from)?;
    tokio::fs::write(&path, bytes).await.map_err(|e| {
        config_error(format!(
            "failed to write skill usage ledger '{}': {e}",
            path.display()
        ))
    })
}

pub async fn sync_skill_usage_metadata(profile_root: &Path, skill: &ManagedSkill) -> Result<()> {
    let mut ledger = load_skill_usage_ledger(profile_root).await?;
    let skill_id = skill.metadata.id.clone();
    let record = ledger
        .records
        .entry(skill_id.clone())
        .or_insert_with(|| SkillUsageRecord::new(skill_id, 0));
    record.merge_skill_metadata(skill);
    save_skill_usage_ledger(profile_root, &ledger).await
}

pub async fn record_skill_usage_event(
    profile_root: &Path,
    event: SkillUsageEvent,
    skill: Option<&ManagedSkill>,
) -> Result<SkillUsageRecord> {
    let skill_id = ledger_skill_id(&event.skill_name)?;
    let mut ledger = load_skill_usage_ledger(profile_root).await?;
    let record = ledger
        .records
        .entry(skill_id.clone())
        .or_insert_with(|| SkillUsageRecord::new(skill_id, event.timestamp));
    if let Some(skill) = skill {
        record.merge_skill_metadata(skill);
    }
    record.record(&event);
    let updated = record.clone();
    save_skill_usage_ledger(profile_root, &ledger).await?;
    Ok(updated)
}

pub async fn record_skill_usage(
    profile_root: &Path,
    skill: &ManagedSkill,
    action: SkillUsageAction,
    _actor: impl Into<String>,
    targets: Vec<String>,
    target: Option<String>,
    metadata: Option<serde_json::Value>,
) -> Result<SkillUsageRecord> {
    let skill_id = skill.metadata.id.clone();
    let timestamp = current_timestamp();
    let mut ledger = load_skill_usage_ledger(profile_root).await?;
    let updated = {
        let record = ledger
            .records
            .entry(skill_id.clone())
            .or_insert_with(|| SkillUsageRecord::new(skill_id, timestamp));
        record.merge_skill_metadata(skill);
        record.record(&SkillUsageEvent {
            skill_name: skill.metadata.id.clone(),
            action,
            timestamp,
            target,
        });
        for target in targets {
            if let Some(target) = normalize_target(&target) {
                insert_sorted_unique(&mut record.targets, target);
            }
        }
        record.clone()
    };
    if let Some(import_key) = metadata
        .as_ref()
        .and_then(|metadata| metadata.get("imported_analytics_event_key"))
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|key| !key.is_empty())
    {
        ledger
            .imported_analytics_events
            .insert(import_key.to_string());
    }
    save_skill_usage_ledger(profile_root, &ledger).await?;
    Ok(updated)
}

pub async fn load_skill_usage_records(
    profile_root: &Path,
    limit: Option<usize>,
) -> Result<Vec<SkillUsageRecord>> {
    let mut records = list_skill_usage_records(profile_root).await?;
    if let Some(limit) = limit {
        records.truncate(limit);
    }
    Ok(records)
}

pub async fn list_skill_usage_records(profile_root: &Path) -> Result<Vec<SkillUsageRecord>> {
    let mut records = load_skill_usage_ledger(profile_root)
        .await?
        .records
        .into_values()
        .collect::<Vec<_>>();
    records.sort_by(|a, b| a.skill_id.cmp(&b.skill_id));
    Ok(records)
}

pub async fn summarize_skill_usage(
    profile_root: &Path,
    skills: &[ManagedSkill],
) -> Result<Vec<SkillUsageSummary>> {
    let mut ledger = load_skill_usage_ledger(profile_root).await?;
    Ok(skills
        .iter()
        .map(|skill| summarize_skill(skill, ledger.records.remove(&skill.metadata.id)))
        .collect())
}

pub async fn summarize_skill_usage_for(
    profile_root: &Path,
    skill: &ManagedSkill,
) -> Result<SkillUsageSummary> {
    Ok(summarize_skill(
        skill,
        load_skill_usage_ledger(profile_root)
            .await?
            .records
            .remove(&skill.metadata.id),
    ))
}

pub async fn load_skill_usage_record(
    profile_root: &Path,
    skill_id: &str,
) -> Result<Option<SkillUsageRecord>> {
    let skill_id = ledger_skill_id(skill_id)?;
    Ok(load_skill_usage_ledger(profile_root)
        .await?
        .records
        .remove(&skill_id))
}

fn ledger_skill_id(raw: &str) -> Result<String> {
    let mut normalized = raw.trim().to_ascii_lowercase();
    if let Some((_, suffix)) = normalized.rsplit_once(':') {
        normalized = suffix.to_string();
    }
    let mut out = String::new();
    let mut previous_separator = false;
    for ch in normalized.chars() {
        if ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_' {
            out.push(ch);
            previous_separator = false;
        } else if matches!(ch, '-' | '_' | ' ' | '.' | '/') && !previous_separator {
            out.push('-');
            previous_separator = true;
        }
    }
    let out = out.trim_matches('-').to_string();
    if out.is_empty() {
        Err(config_error(format!("unsafe skill usage name '{raw}'")))
    } else {
        Ok(out)
    }
}

fn summarize_skill(skill: &ManagedSkill, record: Option<SkillUsageRecord>) -> SkillUsageSummary {
    let mut record = record.unwrap_or_else(|| SkillUsageRecord::new(skill.metadata.id.clone(), 0));
    record.merge_skill_metadata(skill);
    record
}

fn normalize_target(raw: &str) -> Option<String> {
    let normalized = raw.trim().to_ascii_lowercase().replace('-', "_");
    (!normalized.is_empty()).then_some(normalized)
}

fn insert_sorted_unique(values: &mut Vec<String>, value: String) {
    if values.iter().any(|existing| existing == &value) {
        return;
    }
    values.push(value);
    values.sort();
}

fn max_optional(existing: Option<i64>, timestamp: i64) -> i64 {
    existing.map_or(timestamp, |current| current.max(timestamp))
}

fn config_error(message: String) -> TraceDecayError {
    TraceDecayError::Config { message }
}
