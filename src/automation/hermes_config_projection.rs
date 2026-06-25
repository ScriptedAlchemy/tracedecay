use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::errors::{Result, TraceDecayError};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HermesConfigProjection {
    pub config_path: PathBuf,
    pub exists: bool,
    pub config_yaml_path: PathBuf,
    pub config_yaml_exists: bool,
    pub config_format: String,
    pub profile_home: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_root_pin: Option<Value>,
    pub curator: HermesCuratorConfigProjection,
    pub self_improvement: HermesSelfImprovementConfigProjection,
    pub write_approval: HermesWriteApprovalConfigProjection,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auxiliary_curator: Option<HermesAuxiliaryTaskProjection>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HermesCuratorConfigProjection {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interval_hours: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_idle_hours: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stale_after_days: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub archive_after_days: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HermesSelfImprovementConfigProjection {
    pub memory_nudge_interval: u64,
    pub skill_creation_nudge_interval: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct HermesWriteApprovalConfigProjection {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory: Option<Value>,
    pub memory_enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skills: Option<Value>,
    pub skills_enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HermesAuxiliaryTaskProjection {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    pub api_key_configured: bool,
}

pub(crate) fn load_config_projection(hermes_home: &Path) -> Result<HermesConfigProjection> {
    let config_path = hermes_home.join("config.json");
    let config_yaml_path = hermes_home.join("config.yaml");
    let json_config = match fs::read_to_string(&config_path) {
        Ok(contents) => {
            let value: Value = serde_json::from_str(&contents).map_err(|e| {
                config_error(format!(
                    "failed to parse Hermes profile config '{}': {e}",
                    config_path.display()
                ))
            })?;
            Some(value)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            return Err(config_error(format!(
                "failed to read Hermes profile config '{}': {e}",
                config_path.display()
            )));
        }
    };
    let json_project_root_pin = json_config
        .as_ref()
        .and_then(|value| {
            value
                .get("project_root")
                .or_else(|| value.get("projectRoot"))
                .or_else(|| value.get("project_root_pin"))
        })
        .cloned();
    let yaml_config = load_hermes_yaml_projection(&config_yaml_path)?;
    let json_memory_write_approval = json_write_approval(json_config.as_ref(), "memory");
    let json_skills_write_approval = json_write_approval(json_config.as_ref(), "skills");
    let project_root_pin = yaml_config
        .get("plugins.tracedecay.project_root")
        .cloned()
        .map(Value::String)
        .or(json_project_root_pin)
        .or_else(|| yaml_config.get("terminal.cwd").cloned().map(Value::String))
        .or_else(|| yaml_config.get("project_root").cloned().map(Value::String))
        .or_else(|| {
            yaml_config
                .get("project_root_pin")
                .cloned()
                .map(Value::String)
        });
    let config_format = match (config_path.is_file(), config_yaml_path.is_file()) {
        (_, true) => "yaml",
        (true, false) => "json",
        (false, false) => "missing",
    }
    .to_string();
    let memory_write_approval =
        yaml_write_approval(&yaml_config, "memory").or(json_memory_write_approval);
    let skills_write_approval =
        yaml_write_approval(&yaml_config, "skills").or(json_skills_write_approval);
    Ok(HermesConfigProjection {
        exists: config_path.is_file() || config_yaml_path.is_file(),
        config_yaml_exists: config_yaml_path.is_file(),
        config_yaml_path,
        config_format,
        config_path,
        profile_home: hermes_home.to_path_buf(),
        project_root_pin,
        curator: HermesCuratorConfigProjection {
            enabled: yaml_bool(&yaml_config, "curator.enabled"),
            interval_hours: yaml_u64(&yaml_config, "curator.interval_hours"),
            min_idle_hours: yaml_u64(&yaml_config, "curator.min_idle_hours"),
            stale_after_days: yaml_u64(&yaml_config, "curator.stale_after_days"),
            archive_after_days: yaml_u64(&yaml_config, "curator.archive_after_days"),
        },
        self_improvement: HermesSelfImprovementConfigProjection {
            memory_nudge_interval: yaml_u64(&yaml_config, "memory.nudge_interval").unwrap_or(10),
            skill_creation_nudge_interval: yaml_u64(&yaml_config, "skills.creation_nudge_interval")
                .unwrap_or(10),
        },
        write_approval: HermesWriteApprovalConfigProjection {
            memory_enabled: write_approval_enabled(memory_write_approval.as_ref()),
            memory: memory_write_approval,
            skills_enabled: write_approval_enabled(skills_write_approval.as_ref()),
            skills: skills_write_approval,
        },
        auxiliary_curator: auxiliary_curator_projection(&yaml_config),
    })
}

fn json_write_approval(config: Option<&Value>, area: &str) -> Option<Value> {
    let config = config?;
    json_path(config, &[area, "write_approval"])
        .or_else(|| json_path(config, &[area, "writeApproval"]))
        .or_else(|| json_path(config, &["write_approval", area]))
        .or_else(|| json_path(config, &["writeApproval", area]))
        .cloned()
}

pub(crate) fn json_path<'a>(value: &'a Value, path: &[&str]) -> Option<&'a Value> {
    path.iter()
        .try_fold(value, |current, segment| current.get(*segment))
}

pub(crate) fn load_hermes_yaml_projection(path: &Path) -> Result<BTreeMap<String, String>> {
    let contents = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(e) => {
            return Err(config_error(format!(
                "failed to read Hermes profile config '{}': {e}",
                path.display()
            )));
        }
    };
    Ok(parse_simple_yaml_projection(&contents))
}

pub(crate) fn parse_simple_yaml_projection(contents: &str) -> BTreeMap<String, String> {
    let mut values = BTreeMap::new();
    let mut stack: Vec<(usize, String)> = Vec::new();
    for raw_line in contents.lines() {
        let without_comment = strip_yaml_comment(raw_line);
        if without_comment.trim().is_empty() {
            continue;
        }
        let indent = without_comment
            .chars()
            .take_while(|ch| ch.is_whitespace())
            .count();
        let line = without_comment.trim();
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim();
        if key.is_empty() || key.starts_with('-') {
            continue;
        }
        while stack
            .last()
            .is_some_and(|(previous_indent, _)| *previous_indent >= indent)
        {
            stack.pop();
        }
        let value = value.trim();
        if value.is_empty() {
            stack.push((indent, key.to_string()));
            continue;
        }
        let mut path = stack
            .iter()
            .map(|(_, part)| part.as_str())
            .collect::<Vec<_>>();
        path.push(key);
        values.insert(
            path.join("."),
            value
                .trim_matches('"')
                .trim_matches('\'')
                .trim()
                .to_string(),
        );
    }
    values
}

pub(crate) fn strip_yaml_comment(line: &str) -> &str {
    let mut in_single = false;
    let mut in_double = false;
    let mut escaped = false;
    for (idx, ch) in line.char_indices() {
        match ch {
            '\\' if in_double => escaped = !escaped,
            '"' if !in_single && !escaped => in_double = !in_double,
            '\'' if !in_double => in_single = !in_single,
            '#' if !in_single && !in_double => return &line[..idx],
            _ => escaped = false,
        }
    }
    line
}

fn yaml_u64(values: &BTreeMap<String, String>, key: &str) -> Option<u64> {
    values.get(key).and_then(|value| value.parse().ok())
}

pub(crate) fn yaml_bool(values: &BTreeMap<String, String>, key: &str) -> Option<bool> {
    values
        .get(key)
        .and_then(|value| match value.to_ascii_lowercase().as_str() {
            "true" | "yes" | "on" => Some(true),
            "false" | "no" | "off" => Some(false),
            _ => None,
        })
}

fn yaml_write_approval(values: &BTreeMap<String, String>, area: &str) -> Option<Value> {
    [
        format!("{area}.write_approval"),
        format!("{area}.writeApproval"),
        format!("write_approval.{area}"),
        format!("writeApproval.{area}"),
    ]
    .iter()
    .find_map(|key| values.get(key).map(|value| yaml_scalar_value(value)))
}

fn write_approval_enabled(value: Option<&Value>) -> bool {
    let Some(value) = value else {
        return false;
    };
    if let Some(enabled) = value.as_bool() {
        return enabled;
    }
    if let Some(number) = value.as_f64() {
        return (number - 1.0).abs() < f64::EPSILON;
    }
    value.as_str().is_some_and(|text| {
        matches!(
            text.trim().to_ascii_lowercase().as_str(),
            "on" | "true" | "yes" | "1" | "approve" | "enabled"
        )
    })
}

pub(crate) fn yaml_scalar_value(value: &str) -> Value {
    match value.to_ascii_lowercase().as_str() {
        "true" | "yes" | "on" => Value::Bool(true),
        "false" | "no" | "off" => Value::Bool(false),
        _ => value.parse::<u64>().map_or_else(
            |_| Value::String(value.to_string()),
            |number| Value::Number(number.into()),
        ),
    }
}

fn auxiliary_curator_projection(
    values: &BTreeMap<String, String>,
) -> Option<HermesAuxiliaryTaskProjection> {
    let provider = values
        .get("curator.auxiliary.provider")
        .or_else(|| values.get("auxiliary.curator.provider"))
        .cloned();
    let model = values
        .get("curator.auxiliary.model")
        .or_else(|| values.get("auxiliary.curator.model"))
        .cloned();
    let base_url = values
        .get("curator.auxiliary.base_url")
        .or_else(|| values.get("auxiliary.curator.base_url"))
        .cloned();
    let api_key_configured = values
        .keys()
        .any(|key| key.starts_with("curator.auxiliary.") && key.contains("api_key"))
        || values
            .keys()
            .any(|key| key.starts_with("auxiliary.curator.") && key.contains("api_key"));
    (provider.is_some() || model.is_some() || base_url.is_some() || api_key_configured).then_some(
        HermesAuxiliaryTaskProjection {
            provider,
            model,
            base_url,
            api_key_configured,
        },
    )
}

fn config_error(message: impl Into<String>) -> TraceDecayError {
    TraceDecayError::Config {
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::{json_path, parse_simple_yaml_projection, strip_yaml_comment, yaml_scalar_value};

    use serde_json::json;

    #[test]
    fn yaml_comment_stripping_preserves_quoted_hashes() {
        assert_eq!(
            strip_yaml_comment("base_url: https://example.invalid # comment"),
            "base_url: https://example.invalid "
        );
        assert_eq!(
            strip_yaml_comment("base_url: \"https://example.invalid/v1#curator\" # comment"),
            "base_url: \"https://example.invalid/v1#curator\" "
        );
        assert_eq!(
            strip_yaml_comment("label: 'skill # one' # comment"),
            "label: 'skill # one' "
        );
    }

    #[test]
    fn simple_yaml_projection_handles_nested_values_and_comments() {
        let projection = parse_simple_yaml_projection(
            r#"
curator:
  enabled: true
  interval_hours: 24 # trailing comment
  auxiliary:
    base_url: "https://example.invalid/v1#curator" # keep fragment
skills:
  write_approval: enabled
"#,
        );

        assert_eq!(
            projection.get("curator.enabled").map(String::as_str),
            Some("true")
        );
        assert_eq!(
            projection.get("curator.interval_hours").map(String::as_str),
            Some("24")
        );
        assert_eq!(
            projection
                .get("curator.auxiliary.base_url")
                .map(String::as_str),
            Some("https://example.invalid/v1#curator")
        );
        assert_eq!(
            projection.get("skills.write_approval").map(String::as_str),
            Some("enabled")
        );
    }

    #[test]
    fn yaml_scalar_value_projects_booleans_and_unsigned_numbers() {
        assert_eq!(yaml_scalar_value("on"), json!(true));
        assert_eq!(yaml_scalar_value("off"), json!(false));
        assert_eq!(yaml_scalar_value("24"), json!(24));
        assert_eq!(yaml_scalar_value("manual"), json!("manual"));
    }

    #[test]
    fn bridge_projection_helpers_handle_nested_values() {
        let config = json!({
            "memory": {
                "writeApproval": "approve"
            }
        });
        assert_eq!(
            json_path(&config, &["memory", "writeApproval"]).cloned(),
            Some(json!("approve"))
        );
        assert_eq!(
            json_path(&config, &["missing", "writeApproval"]).cloned(),
            None
        );
    }
}
