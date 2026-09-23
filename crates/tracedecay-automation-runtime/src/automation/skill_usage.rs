use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::config_error;
use super::managed_skills::{ManagedSkill, ManagedSkillSource, ManagedSkillState};
use tracedecay_domain::errors::Result;
use tracedecay_runtime_core::tracedecay::current_timestamp;

mod analytics;
mod overlap;
mod recommendations;
mod store;

pub use analytics::analytics_import_key_for_request;
pub use analytics::ingest_analytics_events;
pub use analytics::ingest_project_analytics_events;
pub use overlap::{
    DEFAULT_SKILL_OVERLAP_LIMIT, SKILL_OVERLAP_CONTENT_THRESHOLD, SKILL_OVERLAP_TITLE_THRESHOLD,
    SkillOverlapCandidate, detected_skill_overlap_pair, detected_skill_overlap_partner,
    skill_overlap_candidates,
};
pub use recommendations::{skill_improvement_recommendations, stale_skill_recommendations};

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
    /// When the skill last transitioned into the active state; mirrors the
    /// managed skill metadata so outcome scoring works from summaries alone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activated_at: Option<i64>,
    /// View/use totals captured at activation time so activity since activation
    /// is an exact delta rather than a heuristic.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub view_count_at_activation: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub use_count_at_activation: Option<u64>,
    /// Import keys that already counted toward this skill. They are not a
    /// store-wide set: each key names this skill, so another skill must not
    /// share the write.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub imported_analytics_events: BTreeSet<String>,
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
            schema_version: 2,
            records: BTreeMap::new(),
            imported_analytics_events: BTreeSet::new(),
        }
    }
}

impl SkillUsageRecord {
    fn new(skill_id: String, timestamp: i64) -> Self {
        Self {
            schema_version: 2,
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
            activated_at: None,
            view_count_at_activation: None,
            use_count_at_activation: None,
            imported_analytics_events: BTreeSet::new(),
        }
    }

    fn merge_skill_metadata(&mut self, skill: &ManagedSkill) {
        self.schema_version = 2;
        self.title = Some(skill.metadata.title.clone());
        self.category = Some(skill.metadata.category.clone());
        self.state = Some(skill.metadata.state);
        self.pinned = skill.metadata.pinned;
        self.created_by = Some(skill.metadata.provenance.actor.clone());
        self.provenance_source = Some(skill.metadata.provenance.source);
        if self.activated_at != skill.metadata.activated_at {
            self.activated_at = skill.metadata.activated_at;
            if self.activated_at.is_some() {
                self.view_count_at_activation = Some(self.view_count);
                self.use_count_at_activation = Some(self.use_count);
            }
        }
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

pub fn skill_usage_record_path(profile_root: &Path, skill_id: &str) -> PathBuf {
    store::skill_usage_record_path(profile_root, skill_id)
}

#[hotpath::measure(label = "automation.skill_usage.load", future = true)]
pub async fn load_skill_usage_ledger(profile_root: &Path) -> Result<SkillUsageLedger> {
    store::load_ledger(profile_root).await
}

pub async fn sync_skill_usage_metadata(profile_root: &Path, skill: &ManagedSkill) -> Result<()> {
    let skill = skill.clone();
    let skill_id = skill.metadata.id.clone();
    store::update_record(profile_root, &skill_id, 0, move |record| {
        record.merge_skill_metadata(&skill);
    })
    .await
    .map(|_| ())
}

pub async fn record_skill_usage_event(
    profile_root: &Path,
    event: SkillUsageEvent,
    skill: Option<&ManagedSkill>,
) -> Result<SkillUsageRecord> {
    let skill_id = ledger_skill_id(&event.skill_name)?;
    let skill = skill.cloned();
    store::update_record(profile_root, &skill_id, event.timestamp, move |record| {
        if let Some(skill) = skill.as_ref() {
            record.merge_skill_metadata(skill);
        }
        record.record(&event);
    })
    .await
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
    let skill = skill.clone();
    let import_key = metadata
        .as_ref()
        .and_then(|metadata| metadata.get("imported_analytics_event_key"))
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|key| !key.is_empty())
        .map(str::to_string);
    store::update_record(profile_root, &skill_id, timestamp, move |record| {
        record.merge_skill_metadata(&skill);
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
        if let Some(import_key) = import_key {
            record.imported_analytics_events.insert(import_key);
        }
    })
    .await
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

#[cfg(test)]
mod tests {
    use super::{SkillUsageAction, SkillUsageEvent, record_skill_usage_event};

    #[tokio::test]
    async fn recording_one_skill_does_not_rewrite_another_skills_file() {
        let root = tempfile::tempdir().unwrap();
        record_skill_usage_event(
            root.path(),
            SkillUsageEvent {
                skill_name: "skill-a".to_string(),
                action: SkillUsageAction::Use,
                timestamp: 10,
                target: None,
            },
            None,
        )
        .await
        .unwrap();
        record_skill_usage_event(
            root.path(),
            SkillUsageEvent {
                skill_name: "skill-b".to_string(),
                action: SkillUsageAction::View,
                timestamp: 11,
                target: None,
            },
            None,
        )
        .await
        .unwrap();
        let before = super::load_skill_usage_ledger(root.path()).await.unwrap();
        assert_eq!(before.records["skill-b"].view_count, 1);

        record_skill_usage_event(
            root.path(),
            SkillUsageEvent {
                skill_name: "skill-a".to_string(),
                action: SkillUsageAction::Use,
                timestamp: 12,
                target: None,
            },
            None,
        )
        .await
        .unwrap();

        let after = super::load_skill_usage_ledger(root.path()).await.unwrap();
        assert_eq!(after.records["skill-a"].use_count, 2);
        assert_eq!(
            after.records["skill-b"].view_count, 1,
            "skill B's last-view must survive skill A's write"
        );
        for skill in ["skill-a", "skill-b"] {
            assert!(super::skill_usage_record_path(root.path(), skill).is_file());
        }
    }
}
