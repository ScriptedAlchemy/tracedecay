//! `tracedecay_config` — dotted-path lookups into TOML and JSON config files.

use std::path::Path;

use serde_json::{Value, json};
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_runtime_core::storage::ProjectPath;

use crate::ToolResult;
use crate::generic_tool_result;

/// Structured TOML / JSON queries by dotted key path.
#[hotpath::measure(label = "mcp.info.config.total")]
pub async fn handle_config(project_root: &Path, args: &Value) -> Result<ToolResult> {
    let key = args
        .get("key")
        .and_then(|v| v.as_str())
        .ok_or_else(|| TraceDecayError::Config {
            message: "missing required parameter: key".to_string(),
        })?
        .to_owned();
    let path = args.get("path").and_then(|v| v.as_str()).map(str::to_owned);
    let glob_pat = args.get("glob").and_then(|v| v.as_str()).map(str::to_owned);

    if path.is_none() && glob_pat.is_none() {
        return Err(TraceDecayError::Config {
            message: "missing required parameter: 'path' or 'glob'".to_string(),
        });
    }
    if path.is_some() && glob_pat.is_some() {
        return Err(TraceDecayError::Config {
            message: "tracedecay_config: 'path' and 'glob' are mutually exclusive".to_string(),
        });
    }

    let scan_root = project_root.to_path_buf();
    let (payload, touched) = hotpath::future!(
        tokio::task::spawn_blocking(move || -> Result<_> {
        let mut files: Vec<String> = Vec::new();
        if let Some(p) = path {
            let project_path = ProjectPath::resolve(&scan_root, Path::new(&p))?;
            files.push(project_path.relative_path_string());
        } else if let Some(pat) = glob_pat {
            let combined = scan_root.join(&pat);
            let walker =
                glob::glob(&combined.to_string_lossy()).map_err(|e| TraceDecayError::Config {
                    message: format!("invalid glob '{pat}': {e}"),
                })?;
            for entry in walker.flatten() {
                if let Ok(project_path) = ProjectPath::resolve(&scan_root, &entry) {
                    files.push(project_path.relative_path_string());
                }
            }
            files.sort();
        }

        let mut matches: Vec<Value> = Vec::new();
        let mut touched: Vec<String> = Vec::new();
        for rel in &files {
            let project_path = ProjectPath::resolve(&scan_root, Path::new(rel))?;
            let abs = project_path.absolute_path();
            let rel = project_path.relative_path_string();
            let Ok(contents) = std::fs::read_to_string(&abs) else {
                continue;
            };
            let Some(parsed) = parse_config_value(&rel, &contents) else {
                continue;
            };
            let parsed = match parsed {
                Ok(value) => value,
                Err(error) => {
                    matches.push(json!({
                        "file": rel,
                        "error": error,
                    }));
                    continue;
                }
            };

            if !touched.contains(&rel) {
                touched.push(rel.clone());
            }
            matches.push(config_match_value(&rel, &key, &contents, &parsed));
        }

        Ok((
            json!({
                "match_count": matches.iter().filter(|m| m.get("found") != Some(&Value::Bool(false))).count(),
                "matches": matches,
            }),
            touched,
        ))
    }),
        label = "mcp.info.config.scan"
    )
    .await
    .map_err(|join_error| TraceDecayError::Config {
        message: format!("tracedecay_config scan failed to join: {join_error}"),
    })??;
    Ok(generic_tool_result(
        Some(project_root),
        args,
        &payload,
        touched,
    ))
}

#[derive(Debug, Clone, Copy)]
enum ConfigFormat {
    Toml,
    Json,
}

fn config_format(path: &str) -> Option<ConfigFormat> {
    let extension = Path::new(path).extension()?;
    if extension.eq_ignore_ascii_case("toml") {
        Some(ConfigFormat::Toml)
    } else if extension.eq_ignore_ascii_case("json") {
        Some(ConfigFormat::Json)
    } else {
        None
    }
}

fn parse_config_value(path: &str, contents: &str) -> Option<std::result::Result<Value, String>> {
    let parsed = match config_format(path)? {
        ConfigFormat::Toml => toml::from_str::<toml::Value>(contents)
            .map(|value| toml_to_json(&value))
            .map_err(|err| format!("toml parse error: {err}")),
        ConfigFormat::Json => serde_json::from_str::<Value>(contents)
            .map_err(|err| format!("json parse error: {err}")),
    };
    Some(parsed)
}

fn lookup_dotted(value: &Value, key: &str) -> Option<Value> {
    let mut cursor = value.clone();
    for segment in key.split('.') {
        cursor = match cursor {
            Value::Object(map) => map.get(segment).cloned()?,
            Value::Array(items) => {
                let idx: usize = segment.parse().ok()?;
                items.get(idx).cloned()?
            }
            _ => return None,
        };
    }
    Some(cursor)
}

fn toml_to_json(v: &toml::Value) -> Value {
    match v {
        toml::Value::String(s) => Value::String(s.clone()),
        toml::Value::Integer(i) => Value::Number((*i).into()),
        toml::Value::Float(f) => {
            serde_json::Number::from_f64(*f).map_or(Value::Null, Value::Number)
        }
        toml::Value::Boolean(b) => Value::Bool(*b),
        toml::Value::Datetime(d) => Value::String(d.to_string()),
        toml::Value::Array(items) => Value::Array(items.iter().map(toml_to_json).collect()),
        toml::Value::Table(t) => {
            let mut map = serde_json::Map::with_capacity(t.len());
            for (k, child) in t {
                map.insert(k.clone(), toml_to_json(child));
            }
            Value::Object(map)
        }
    }
}

fn config_match_value(file: &str, key: &str, contents: &str, parsed: &Value) -> Value {
    match lookup_dotted(parsed, key) {
        Some(value) => json!({
            "file": file,
            "key": key,
            "value": value,
            "line": find_key_line(contents, key),
        }),
        None => json!({
            "file": file,
            "key": key,
            "value": Value::Null,
            "found": false,
        }),
    }
}

fn find_key_line(contents: &str, key: &str) -> Option<u32> {
    let last = key.rsplit('.').next()?;
    let prefixes = config_key_line_prefixes(last);
    for (idx, line) in contents.lines().enumerate() {
        let trimmed = line.trim_start();
        if prefixes.iter().any(|prefix| trimmed.starts_with(prefix)) {
            return Some((idx as u32) + 1);
        }
    }
    None
}

fn config_key_line_prefixes(key: &str) -> [String; 3] {
    [
        format!("{key} ="),
        format!("\"{key}\" ="),
        format!("\"{key}\":"),
    ]
}
