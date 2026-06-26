use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::errors::{Result, TraceDecayError};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HermesPendingSkillWrite {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subsystem: Option<String>,
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

pub(crate) fn load_pending_skill_writes(
    hermes_home: &Path,
    include_payloads: bool,
) -> Result<Vec<HermesPendingSkillWrite>> {
    let pending_dir = hermes_home.join("pending").join("skills");
    let entries = match fs::read_dir(&pending_dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => {
            return Err(config_error(format!(
                "failed to read Hermes pending skills '{}': {e}",
                pending_dir.display()
            )))
        }
    };
    let mut pending = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| {
            config_error(format!(
                "failed to read Hermes pending skill entry '{}': {e}",
                pending_dir.display()
            ))
        })?;
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let contents = fs::read_to_string(&path).map_err(|e| {
            config_error(format!(
                "failed to read Hermes pending skill '{}': {e}",
                path.display()
            ))
        })?;
        let value: Value = match serde_json::from_str(&contents) {
            Ok(value) => value,
            Err(_) => continue,
        };
        let payload = value.get("payload").cloned();
        let name = payload
            .as_ref()
            .and_then(|payload| payload.get("name"))
            .and_then(Value::as_str)
            .map(str::to_string);
        pending.push(HermesPendingSkillWrite {
            id: value.get("id").and_then(Value::as_str).map_or_else(
                || {
                    path.file_stem()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned()
                },
                str::to_string,
            ),
            subsystem: value
                .get("subsystem")
                .and_then(Value::as_str)
                .map(str::to_string),
            source_path: path,
            action: value
                .get("action")
                .and_then(Value::as_str)
                .map(str::to_string),
            name,
            summary: value
                .get("summary")
                .and_then(Value::as_str)
                .map(str::to_string),
            origin: value
                .get("origin")
                .and_then(Value::as_str)
                .map(str::to_string),
            created_at: value.get("created_at").cloned(),
            payload: include_payloads.then_some(payload).flatten(),
        });
    }
    pending.sort_by(|left, right| {
        pending_created_at_cmp(left.created_at.as_ref(), right.created_at.as_ref())
            .then_with(|| left.id.cmp(&right.id))
    });
    Ok(pending)
}

pub(crate) fn pending_skill_ids_by_name(
    pending: &[HermesPendingSkillWrite],
) -> BTreeMap<String, Vec<String>> {
    let mut by_skill: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for record in pending {
        let Some(name) = &record.name else {
            continue;
        };
        by_skill
            .entry(name.clone())
            .or_default()
            .push(record.id.clone());
    }
    by_skill
}

fn pending_created_at_cmp(left: Option<&Value>, right: Option<&Value>) -> Ordering {
    match (created_at_sort_key(left), created_at_sort_key(right)) {
        (Some(left), Some(right)) => left.cmp(&right),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

#[derive(Debug, Clone, PartialEq)]
enum CreatedAtSortKey {
    Number(f64),
    Text(String),
}

impl Eq for CreatedAtSortKey {}

impl PartialOrd for CreatedAtSortKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for CreatedAtSortKey {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (Self::Number(left), Self::Number(right)) => {
                left.partial_cmp(right).unwrap_or(Ordering::Equal)
            }
            (Self::Number(_), Self::Text(_)) => Ordering::Less,
            (Self::Text(_), Self::Number(_)) => Ordering::Greater,
            (Self::Text(left), Self::Text(right)) => left.cmp(right),
        }
    }
}

fn created_at_sort_key(value: Option<&Value>) -> Option<CreatedAtSortKey> {
    let value = value?;
    if let Some(number) = value.as_f64() {
        return Some(CreatedAtSortKey::Number(number));
    }
    value
        .as_str()
        .map(|text| CreatedAtSortKey::Text(text.to_string()))
}

fn config_error(message: impl Into<String>) -> TraceDecayError {
    TraceDecayError::Config {
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use std::cmp::Ordering;

    use serde_json::json;

    use super::pending_created_at_cmp;

    #[test]
    fn pending_created_at_sort_orders_numbers_strings_and_missing_values() {
        assert_eq!(
            pending_created_at_cmp(Some(&json!(1.5)), Some(&json!(2.0))),
            Ordering::Less
        );
        assert_eq!(
            pending_created_at_cmp(Some(&json!("2026-06-24T00:00:00Z")), Some(&json!(2.0))),
            Ordering::Greater
        );
        assert_eq!(
            pending_created_at_cmp(Some(&json!(2.0)), None),
            Ordering::Less
        );
    }
}
