//! Provider-neutral assistant usage taxonomy.

use std::collections::BTreeSet;

use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum UsageKind {
    Tool,
    Skill,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum UsageCategory {
    TraceDecayGraph,
    LcmSession,
    Memory,
    BroadFileSearch,
    Edit,
    Shell,
    WorkflowSkill,
    TraceDecayWorkflowSkill,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct UsageEvent {
    pub kind: UsageKind,
    pub name: String,
    pub category: UsageCategory,
}

impl UsageCategory {
    pub fn dashboard_label(self) -> &'static str {
        match self {
            Self::TraceDecayGraph => "tracedecay_mcp",
            Self::LcmSession => "lcm_session",
            Self::Memory => "memory",
            Self::BroadFileSearch => "broad_code_context",
            Self::Edit => "code_edit",
            Self::Shell => "shell",
            Self::WorkflowSkill => "workflow_skill",
            Self::TraceDecayWorkflowSkill => "tracedecay_workflow_skill",
            Self::Other => "other_tool",
        }
    }
}

pub fn normalize_tool_name(raw: &str) -> String {
    let trimmed = raw.trim();
    let without_mcp = trimmed
        .strip_prefix("mcp__tracedecay__")
        .or_else(|| trimmed.strip_prefix("mcp_tracedecay_"))
        .unwrap_or(trimmed);
    without_mcp.to_ascii_lowercase().replace('-', "_")
}

fn categorize_normalized_tool(normalized: &str, command_hint: Option<&str>) -> UsageCategory {
    if normalized.starts_with("tracedecay_memory")
        || normalized == "tracedecay_fact_store"
        || normalized == "tracedecay_memory_status"
    {
        return UsageCategory::Memory;
    }
    if normalized.starts_with("tracedecay_lcm")
        || normalized.contains("session")
        || normalized.contains("transcript")
    {
        return UsageCategory::LcmSession;
    }
    if normalized.starts_with("tracedecay_") {
        return UsageCategory::TraceDecayGraph;
    }
    if matches!(
        normalized,
        "read" | "readfile" | "cat" | "sed" | "grep" | "rg" | "glob" | "search" | "find"
    ) {
        return UsageCategory::BroadFileSearch;
    }
    if matches!(normalized, "apply_patch" | "edit" | "write") {
        return UsageCategory::Edit;
    }
    if matches!(
        normalized,
        "bash" | "shell" | "exec_command" | "functions.exec_command"
    ) {
        return command_hint.map_or(UsageCategory::Shell, command_category);
    }
    UsageCategory::Other
}

pub fn categorize_skill(raw: &str) -> UsageCategory {
    let normalized = raw.trim().to_ascii_lowercase();
    if normalized.starts_with("tracedecay:") {
        UsageCategory::TraceDecayWorkflowSkill
    } else if normalized.is_empty() {
        UsageCategory::Other
    } else {
        UsageCategory::WorkflowSkill
    }
}

pub fn infer_usage_events(
    tool_names: Option<&str>,
    metadata_json: Option<&str>,
    text: Option<&str>,
) -> Vec<UsageEvent> {
    let metadata = metadata_json.and_then(|raw| serde_json::from_str::<Value>(raw).ok());
    let command_hint = metadata.as_ref().and_then(command_from_metadata);

    let mut events = BTreeSet::new();
    let tools = tool_names
        .into_iter()
        .flat_map(split_tool_names)
        .collect::<Vec<_>>();
    for tool in &tools {
        insert_tool_event(&mut events, tool, command_hint.as_deref());
    }

    if let Some(value) = metadata.as_ref() {
        let mut skills = explicit_skills_from_metadata(value);
        if tools
            .iter()
            .any(|tool| normalize_tool_name(tool) == "skill_view")
        {
            collect_skill_view_metadata(value, &mut skills);
        }
        for skill in skills {
            insert_skill_event(&mut events, &skill);
        }
    }

    for skill in skills_from_text(text.unwrap_or_default()) {
        insert_skill_event(&mut events, &skill);
    }

    events.into_iter().collect()
}

fn insert_tool_event(events: &mut BTreeSet<UsageEvent>, raw: &str, command_hint: Option<&str>) {
    let name = normalize_tool_name(raw);
    if name.is_empty() {
        return;
    }
    events.insert(UsageEvent {
        kind: UsageKind::Tool,
        category: categorize_normalized_tool(&name, command_hint),
        name,
    });
}

fn insert_skill_event(events: &mut BTreeSet<UsageEvent>, raw: &str) {
    let name = raw.trim();
    if name.is_empty() {
        return;
    }
    events.insert(UsageEvent {
        kind: UsageKind::Skill,
        category: categorize_skill(name),
        name: name.to_string(),
    });
}

pub fn split_tool_names(raw: &str) -> impl Iterator<Item = String> + '_ {
    raw.split([',', '\n'])
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(ToOwned::to_owned)
}

fn command_category(command: &str) -> UsageCategory {
    let normalized = command.to_ascii_lowercase();
    let first_word = normalized.split_whitespace().next().unwrap_or_default();
    if matches!(first_word, "rg" | "grep" | "find" | "fd" | "cat" | "sed") {
        UsageCategory::BroadFileSearch
    } else if normalized.contains("apply_patch") {
        UsageCategory::Edit
    } else {
        UsageCategory::Shell
    }
}

fn command_from_metadata(value: &Value) -> Option<String> {
    match value {
        Value::Object(map) => {
            for key in ["cmd", "command", "shell_command"] {
                if let Some(command) = map.get(key).and_then(Value::as_str) {
                    return Some(command.to_string());
                }
            }
            map.values().find_map(command_from_metadata)
        }
        Value::Array(items) => items.iter().find_map(command_from_metadata),
        _ => None,
    }
}

fn explicit_skills_from_metadata(value: &Value) -> Vec<String> {
    let mut skills = Vec::new();
    collect_explicit_skills(value, &mut skills);
    skills
}

fn collect_explicit_skills(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::Array(items) => {
            for item in items {
                collect_explicit_skills(item, out);
            }
        }
        Value::Object(map) => {
            for (key, value) in map {
                if matches!(
                    key.as_str(),
                    "skill" | "skills" | "skill_name" | "skill_names"
                ) {
                    collect_skill_values(value, out);
                } else {
                    collect_explicit_skills(value, out);
                }
            }
        }
        _ => {}
    }
}

fn collect_skill_values(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::String(skill) => {
            if let Some(skill) = normalize_skill_name(skill) {
                out.push(skill);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_skill_values(item, out);
            }
        }
        Value::Object(map) => {
            for value in map.values() {
                collect_skill_values(value, out);
            }
        }
        _ => {}
    }
}

fn skills_from_text(text: &str) -> Vec<String> {
    let mut skills = Vec::new();
    if let Ok(value) = serde_json::from_str::<Value>(text) {
        collect_skills_from_text_json(&value, &mut skills);
    }
    collect_using_skill_mentions(text, &mut skills);
    skills
}

fn collect_skill_view_metadata(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::Array(items) => {
            for item in items {
                collect_skill_view_metadata(item, out);
            }
        }
        Value::Object(map) => {
            if let Some(function) = map.get("function").and_then(Value::as_object) {
                if function.get("name").and_then(Value::as_str) == Some("skill_view") {
                    if let Some(arguments) = function.get("arguments") {
                        collect_skill_view_arguments(arguments, out);
                    }
                }
            }
            for value in map.values() {
                collect_skill_view_metadata(value, out);
            }
        }
        _ => {}
    }
}

fn collect_skill_view_arguments(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            if let Some(name) = map.get("name").and_then(Value::as_str) {
                if let Some(skill) = normalize_skill_name(name) {
                    out.push(skill);
                }
            }
        }
        Value::String(raw) => {
            if let Ok(parsed) = serde_json::from_str::<Value>(raw) {
                collect_skill_view_arguments(&parsed, out);
            } else if let Some(skill) = normalize_skill_name(raw) {
                out.push(skill);
            }
        }
        _ => {}
    }
}

fn collect_skills_from_text_json(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::Array(items) => {
            for item in items {
                collect_skills_from_text_json(item, out);
            }
        }
        Value::Object(map) => {
            let tool_name = map
                .get("name")
                .and_then(Value::as_str)
                .map(str::to_ascii_lowercase);
            let path = map
                .get("input")
                .and_then(|input| input.get("path"))
                .or_else(|| map.get("path"))
                .and_then(Value::as_str);

            if matches!(tool_name.as_deref(), Some("read" | "readfile")) {
                if let Some(skill) = path.and_then(skill_from_path) {
                    out.push(skill);
                }
            } else if path.is_some_and(is_skill_file_path) {
                if let Some(name) = map.get("name").and_then(Value::as_str) {
                    if let Some(skill) = normalize_skill_name(name) {
                        out.push(skill);
                    } else if let Some(skill) = path.and_then(skill_from_path) {
                        out.push(skill);
                    }
                } else if let Some(skill) = path.and_then(skill_from_path) {
                    out.push(skill);
                }
            }

            for value in map.values() {
                collect_skills_from_text_json(value, out);
            }
        }
        _ => {}
    }
}

fn collect_using_skill_mentions(text: &str, out: &mut Vec<String>) {
    for line in text.lines() {
        if !line.to_ascii_lowercase().contains("using ") {
            continue;
        }

        let mut rest = line;
        while let Some(start) = rest.find('`') {
            let after_start = &rest[start + 1..];
            let Some(end) = after_start.find('`') else {
                break;
            };
            if let Some(skill) = skill_from_token(&after_start[..end]) {
                out.push(skill);
            }
            rest = &after_start[end + 1..];
        }
    }
}

fn skill_from_path(path: &str) -> Option<String> {
    if !is_skill_file_path(path) {
        return None;
    }

    let parts = path.split('/').collect::<Vec<_>>();
    let Some(skills_index) = parts.iter().rposition(|part| *part == "skills") else {
        return skill_from_relative_result_path(&parts);
    };
    let skill = if parts.get(skills_index + 3) == Some(&"SKILL.md") {
        parts.get(skills_index + 2).copied()
    } else {
        parts.get(skills_index + 1).copied()
    }?;

    if !is_skill_ident(skill) {
        return None;
    }

    let namespace = skill_path_namespace(&parts, skills_index);
    Some(match namespace {
        Some(namespace) => format!("{namespace}:{skill}"),
        None => format!("skill:{skill}"),
    })
}

fn skill_path_namespace(parts: &[&str], skills_index: usize) -> Option<String> {
    if parts.get(skills_index + 3) == Some(&"SKILL.md") {
        return parts
            .get(skills_index + 1)
            .copied()
            .filter(|namespace| is_skill_ident(namespace))
            .map(ToOwned::to_owned);
    }

    let previous = parts.get(skills_index.checked_sub(1)?)?.trim();
    if is_cache_version(previous) {
        return parts
            .get(skills_index.checked_sub(2)?)
            .copied()
            .filter(|namespace| is_skill_ident(namespace))
            .map(ToOwned::to_owned);
    }
    if is_skill_ident(previous) && !matches!(previous, ".codex" | ".cursor" | "package" | "skills")
    {
        return Some(previous.to_string());
    }

    None
}

fn is_skill_file_path(path: &str) -> bool {
    path.ends_with("/SKILL.md") || path.ends_with("\\SKILL.md")
}

fn normalize_skill_name(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.contains(':') {
        skill_from_token(trimmed)
    } else {
        is_skill_ident(trimmed).then(|| trimmed.to_string())
    }
}

fn skill_from_relative_result_path(parts: &[&str]) -> Option<String> {
    if parts.len() != 3 || parts.get(2) != Some(&"SKILL.md") {
        return None;
    }

    let namespace = *parts.first()?;
    let skill = *parts.get(1)?;
    if is_skill_ident(namespace) && is_skill_ident(skill) {
        Some(format!("{namespace}:{skill}"))
    } else {
        None
    }
}

fn skill_from_token(token: &str) -> Option<String> {
    let cleaned = token.trim_matches(|ch: char| {
        matches!(
            ch,
            '`' | '\'' | '"' | ',' | '.' | ':' | ';' | '(' | ')' | '[' | ']'
        )
    });
    let (namespace, name) = cleaned.split_once(':')?;
    if cleaned.contains("://") || !is_skill_ident(namespace) || !is_skill_ident(name) {
        return None;
    }
    Some(cleaned.to_string())
}

fn is_skill_ident(value: &str) -> bool {
    value
        .chars()
        .next()
        .is_some_and(|ch| ch.is_ascii_lowercase())
        && value.chars().all(|ch| {
            ch.is_ascii_lowercase() || ch.is_ascii_digit() || matches!(ch, '-' | '_' | '.')
        })
}

fn is_cache_version(value: &str) -> bool {
    let has_digit = value.chars().any(|ch| ch.is_ascii_digit());
    has_digit
        && value
            .chars()
            .all(|ch| ch.is_ascii_hexdigit() || matches!(ch, '.' | '-' | '_'))
}

#[cfg(test)]
mod tests {
    use super::{infer_usage_events, UsageCategory, UsageEvent, UsageKind};

    fn assert_usage_event(
        events: &[UsageEvent],
        kind: UsageKind,
        name: &str,
        category: UsageCategory,
    ) {
        assert!(
            events.iter().any(|event| {
                event.kind == kind && event.name == name && event.category == category
            }),
            "missing {kind:?} event {name:?} in {events:#?}"
        );
    }

    #[test]
    fn normalizes_mcp_tracedecay_tool_names() {
        let events =
            infer_usage_events(Some("mcp__tracedecay__tracedecay_context,Read"), None, None);
        assert_usage_event(
            &events,
            UsageKind::Tool,
            "tracedecay_context",
            UsageCategory::TraceDecayGraph,
        );
        assert_usage_event(
            &events,
            UsageKind::Tool,
            "read",
            UsageCategory::BroadFileSearch,
        );
    }

    #[test]
    fn ignores_tool_names_buried_in_metadata() {
        let events = infer_usage_events(
            None,
            Some(
                r#"{
                    "tool_calls": [{"function": {"name": "tracedecay_search"}}],
                    "tools": [{"tool_name": "apply_patch"}]
                }"#,
            ),
            None,
        );
        assert!(
            events.is_empty(),
            "tool usage should come from explicit tool fields, got {events:#?}"
        );
    }

    #[test]
    fn shell_command_metadata_refines_category() {
        let events = infer_usage_events(
            Some("functions.exec_command"),
            Some(r#"{"cmd":"rg -n \"analytics\" src tests"}"#),
            None,
        );
        assert_usage_event(
            &events,
            UsageKind::Tool,
            "functions.exec_command",
            UsageCategory::BroadFileSearch,
        );
    }

    #[test]
    fn infers_skills_from_metadata_and_text() {
        let events = infer_usage_events(
            None,
            Some(r#"{"context":{"skills":["tracedecay:searching-for-code"]}}"#),
            Some("Using `build-web-apps:react-best-practices`."),
        );
        assert_usage_event(
            &events,
            UsageKind::Skill,
            "tracedecay:searching-for-code",
            UsageCategory::TraceDecayWorkflowSkill,
        );
        assert_usage_event(
            &events,
            UsageKind::Skill,
            "build-web-apps:react-best-practices",
            UsageCategory::WorkflowSkill,
        );
    }

    #[test]
    fn ignores_url_and_path_like_colon_tokens() {
        let events = infer_usage_events(
            None,
            Some(r#"{"url":"https://example.com/a:b","path":"C:\\tmp\\SKILL.md"}"#),
            Some("Also ignore file:///tmp/tracedecay:searching-for-code"),
        );
        assert!(
            events.is_empty(),
            "url and path strings should not become skills, got {events:#?}"
        );
    }

    #[test]
    fn normalizes_punctuation_wrapped_skill_mentions() {
        let events = infer_usage_events(
            None,
            None,
            Some("Using `tracedecay:reading-code-cheaply`, then continue."),
        );
        assert_usage_event(
            &events,
            UsageKind::Skill,
            "tracedecay:reading-code-cheaply",
            UsageCategory::TraceDecayWorkflowSkill,
        );
    }

    #[test]
    fn infers_codex_skill_usage_from_real_prose_shape() {
        let events = infer_usage_events(
            None,
            Some(r#"{"source":"codex_rollout"}"#),
            Some(include_str!(
                "../tests/fixtures/analytics/codex_skill_prose.txt"
            )),
        );
        assert_usage_event(
            &events,
            UsageKind::Skill,
            "superpowers:using-superpowers",
            UsageCategory::WorkflowSkill,
        );
        assert_usage_event(
            &events,
            UsageKind::Skill,
            "tracedecay:reading-code-cheaply",
            UsageCategory::TraceDecayWorkflowSkill,
        );
    }

    #[test]
    fn infers_cursor_skill_reads_from_real_tool_use_shape() {
        let events = infer_usage_events(
            Some("ReadFile"),
            Some(r#"{"raw_type":null,"source":"cursor_transcript"}"#),
            Some(include_str!(
                "../tests/fixtures/analytics/cursor_skill_read_text.json"
            )),
        );
        assert_usage_event(
            &events,
            UsageKind::Tool,
            "readfile",
            UsageCategory::BroadFileSearch,
        );
        assert_usage_event(
            &events,
            UsageKind::Skill,
            "tracedecay:curating-project-memory",
            UsageCategory::TraceDecayWorkflowSkill,
        );
        assert_usage_event(
            &events,
            UsageKind::Skill,
            "superpowers:using-superpowers",
            UsageCategory::WorkflowSkill,
        );
    }

    #[test]
    fn infers_hermes_skill_view_from_real_metadata_and_result_shapes() {
        let events = infer_usage_events(
            Some("skill_view"),
            Some(include_str!(
                "../tests/fixtures/analytics/hermes_skill_view_metadata.json"
            )),
            Some(include_str!(
                "../tests/fixtures/analytics/hermes_skill_view_text.json"
            )),
        );
        assert_usage_event(&events, UsageKind::Tool, "skill_view", UsageCategory::Other);
        assert_usage_event(
            &events,
            UsageKind::Skill,
            "github-pr-workflow",
            UsageCategory::WorkflowSkill,
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| event.kind == UsageKind::Skill)
                .count(),
            1,
            "skill_view should count one canonical skill event, got {events:#?}",
        );
    }

    #[test]
    fn ignores_colon_tokens_without_skill_context() {
        let events = infer_usage_events(
            None,
            Some(
                r#"{
                    "scripts": {"test:ui": "vitest"},
                    "query": "is:pr filetype:pdf",
                    "time": "07:49"
                }"#,
            ),
            Some("Aspect ratio 16:9, script bench:full, and tool MCP:browser_navigate."),
        );
        assert!(
            events.is_empty(),
            "unanchored colon tokens should not become skills, got {events:#?}"
        );
    }
}
