use serde_json::Value;

use super::render::{self, Md};

#[derive(Clone, Copy)]
pub(super) struct FactStoreView<'a> {
    args: &'a Value,
    payload: &'a Value,
}

impl<'a> FactStoreView<'a> {
    pub(super) fn new(args: &'a Value, payload: &'a Value) -> Self {
        Self { args, payload }
    }
}

#[derive(Clone, Copy)]
pub(super) struct SkillListView<'a> {
    payload: &'a Value,
}

impl<'a> SkillListView<'a> {
    pub(super) fn new(payload: &'a Value) -> Self {
        Self { payload }
    }
}

#[derive(Clone, Copy)]
pub(super) struct SkillView<'a> {
    payload: &'a Value,
}

impl<'a> SkillView<'a> {
    pub(super) fn new(payload: &'a Value) -> Self {
        Self { payload }
    }
}

#[derive(Clone, Copy)]
pub(super) struct AutomationArtifactView<'a> {
    payload: &'a Value,
}

impl<'a> AutomationArtifactView<'a> {
    pub(super) fn new(payload: &'a Value) -> Self {
        Self { payload }
    }
}

pub(super) fn fact_store_md(view: FactStoreView<'_>) -> String {
    let mut md = Md::new();
    let args = view.args;
    let value = view.payload;
    md.heading(2, "Fact Store");
    let action = render::field_str(value, "action");
    if !action.is_empty() {
        md.field("action", action);
    }
    append_fact_store_request(&mut md, args);
    if let Some(count) = value.get("count").and_then(Value::as_i64) {
        md.field("count", &count.to_string());
    }

    if let Some(removed) = value.get("removed").and_then(Value::as_bool) {
        md.field("removed", if removed { "true" } else { "false" });
    }
    append_fact_store_diff(&mut md, value);

    if let Some(fact) = value.get("fact").filter(|fact| fact.is_object()) {
        md.blank().heading(3, "Fact");
        append_fact_md(&mut md, fact, value);
    }

    if let Some(results) = value.get("results").and_then(Value::as_array) {
        md.blank().heading(3, "Facts");
        if results.is_empty() {
            md.empty_note("No matching facts.");
        } else {
            for result in results {
                append_fact_md(&mut md, fact_payload(result), result);
            }
        }
    }

    if let Some(history) = value.get("trust_history").and_then(Value::as_array) {
        md.blank().heading(3, "Trust History");
        if history.is_empty() {
            md.empty_note("No trust feedback recorded.");
        } else {
            for item in history.iter().take(10) {
                md.bullet(&compact_json_summary(item));
            }
            if history.len() > 10 {
                md.bullet(&format!("... {} more", history.len() - 10));
            }
        }
    }

    md.render()
}

pub(super) fn skill_list_md(view: SkillListView<'_>) -> String {
    let mut md = Md::new();
    let value = view.payload;
    md.heading(2, "Managed Skills");
    md.field("status", render::field_str(value, "status"));
    md.field("count", &render::field_i64(value, "count").to_string());
    let profile_root = render::field_str(value, "profile_root");
    if !profile_root.is_empty() {
        md.field("profile_root", profile_root);
    }
    md.blank().heading(3, "Skills");
    let Some(skills) = value.get("skills").and_then(Value::as_array) else {
        md.empty_note("No skills field returned.");
        return md.render();
    };
    if skills.is_empty() {
        md.empty_note("No managed skills.");
    } else {
        for skill in skills {
            append_skill_item(&mut md, skill);
        }
    }
    md.render()
}

pub(super) fn skill_view_md(view: SkillView<'_>) -> String {
    let mut md = Md::new();
    let value = view.payload;
    let skill = value.get("skill").unwrap_or(value);
    let metadata = skill.get("metadata").unwrap_or(skill);
    let id = value_str(metadata, "/id");
    md.heading(2, &format!("Managed Skill: {id}"));
    md.field("status", render::field_str(value, "status"));
    for (label, pointer) in [
        ("title", "/title"),
        ("state", "/state"),
        ("category", "/category"),
        ("checksum", "/checksum"),
    ] {
        let text = value_str(metadata, pointer);
        if !text.is_empty() {
            md.field(label, text);
        }
    }
    let targets = string_array(metadata.get("targets"));
    if !targets.is_empty() {
        md.field("targets", &targets);
    }
    if let Some(included) = value.get("support_files_included").and_then(Value::as_bool) {
        md.field(
            "support_files_included",
            if included { "true" } else { "false" },
        );
    }
    let summary = value_str(metadata, "/summary");
    if !summary.is_empty() {
        md.blank().heading(3, "Summary").line(summary);
    }
    let body = value_str(skill, "/body_markdown");
    if !body.is_empty() {
        md.blank().heading(3, "Body").line(body);
    }
    if let Some(files) = skill.get("support_files").and_then(Value::as_array) {
        md.blank().heading(3, "Support Files");
        if files.is_empty() {
            md.empty_note("No support files.");
        } else {
            for file in files {
                let path = value_str(file, "/path");
                let bytes = file
                    .get("bytes")
                    .and_then(Value::as_array)
                    .map(|bytes| bytes.len())
                    .unwrap_or_default();
                md.bullet(&format!("**{path}** - {bytes} bytes"));
            }
        }
    }
    md.render()
}

pub(super) fn automation_artifact_md(view: AutomationArtifactView<'_>) -> String {
    let mut md = Md::new();
    let value = view.payload;
    md.heading(2, "Automation Run Artifact");
    md.field("status", render::field_str(value, "status"));
    md.field("run_id", render::field_str(value, "run_id"));
    let artifact = value.get("artifact").unwrap_or(&Value::Null);
    for key in ["kind", "path", "sha256"] {
        let text = render::field_str(artifact, key);
        if !text.is_empty() {
            md.field(key, text);
        }
    }
    if let Some(size) = value_i64(artifact, "/size_bytes") {
        md.field("size_bytes", &size.to_string());
    }
    if let Some(payload) = value.get("payload") {
        md.blank().heading(3, "Payload");
        md.line(render::generic_md(payload).trim());
    }
    md.render()
}

fn append_fact_store_request(md: &mut Md, args: &Value) {
    for key in [
        "query",
        "entity",
        "category",
        "min_trust",
        "threshold",
        "limit",
    ] {
        let text = compact_scalar(args.get(key));
        if !text.is_empty() {
            md.field(key, &text);
        }
    }
    let entities = string_list(args.get("entities"));
    if !entities.is_empty() {
        md.field("entities", &entities.join(", "));
    }
}

fn append_fact_store_diff(md: &mut Md, value: &Value) {
    for key in ["diff", "closest_fact_id", "similarity", "reason", "error"] {
        let text = compact_scalar(value.get(key));
        if !text.is_empty() {
            md.field(key, &text);
        }
    }
}

fn fact_payload(value: &Value) -> &Value {
    value
        .get("fact")
        .filter(|fact| fact.is_object())
        .unwrap_or(value)
}

fn append_fact_md(md: &mut Md, fact: &Value, envelope: &Value) {
    let id = fact
        .get("fact_id")
        .and_then(Value::as_i64)
        .map(|id| format!("#{id}"))
        .unwrap_or_else(|| "#?".to_string());
    let category = compact_scalar(fact.get("category"));
    let trust = fact
        .get("trust_score")
        .and_then(Value::as_f64)
        .map(|score| format!("{score:.3}"))
        .unwrap_or_default();
    let content = compact_text(&render::field_str(fact, "content"));
    let mut head = id;
    if !category.is_empty() {
        head.push(' ');
        head.push_str(&category);
    }
    if !trust.is_empty() {
        head.push_str(" trust ");
        head.push_str(&trust);
    }
    if let Some(score) = envelope
        .get("score")
        .and_then(Value::as_f64)
        .map(|score| format!("{score:.3}"))
    {
        head.push_str(" score ");
        head.push_str(&score);
    }
    if !content.is_empty() {
        head.push_str(": ");
        head.push_str(&content);
    }
    md.bullet(&head);

    let detail = fact_detail_line(fact);
    if !detail.is_empty() {
        md.line(&format!("  {detail}"));
    }
    let why = compact_text(&render::field_str(envelope, "why"));
    if !why.is_empty() {
        md.line(&format!("  why: {why}"));
    }
}

fn fact_detail_line(fact: &Value) -> String {
    let mut parts = Vec::new();
    let entities = string_list(fact.get("entities"));
    if !entities.is_empty() {
        parts.push(format!("entities: {}", entities.join(", ")));
    }
    let tags = string_list(fact.get("tags"));
    if !tags.is_empty() {
        parts.push(format!("tags: {}", tags.join(", ")));
    }
    let source = compact_scalar(fact.get("source"));
    if !source.is_empty() {
        parts.push(format!("source: {source}"));
    }
    parts.join("; ")
}

fn string_list(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(compact_text)
                .filter(|text| !text.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

fn compact_scalar(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) => compact_text(text),
        Some(Value::Number(number)) => number.to_string(),
        Some(Value::Bool(true)) => "true".to_string(),
        Some(Value::Bool(false)) => "false".to_string(),
        _ => String::new(),
    }
}

fn compact_text(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn compact_json_summary(value: &Value) -> String {
    match value {
        Value::Object(map) => {
            let mut parts = Vec::new();
            for key in ["created_at", "trust_score", "feedback", "source", "note"] {
                let text = compact_scalar(map.get(key));
                if !text.is_empty() {
                    parts.push(format!("{key}: {text}"));
                }
            }
            if parts.is_empty() {
                serde_json::to_string(value).unwrap_or_default()
            } else {
                parts.join("; ")
            }
        }
        _ => compact_scalar(Some(value)),
    }
}

fn value_str<'a>(value: &'a Value, pointer: &str) -> &'a str {
    value.pointer(pointer).and_then(Value::as_str).unwrap_or("")
}

fn value_i64(value: &Value, pointer: &str) -> Option<i64> {
    value.pointer(pointer).and_then(Value::as_i64)
}

fn string_array(value: Option<&Value>) -> String {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default()
}

fn append_skill_item(md: &mut Md, skill: &Value) {
    let metadata = skill.get("metadata").unwrap_or(skill);
    let id = value_str(metadata, "/id");
    let title = value_str(metadata, "/title");
    let state = value_str(metadata, "/state");
    let mut line = if title.is_empty() || title == id {
        format!("**{id}**")
    } else {
        format!("**{id}** - {title}")
    };
    if !state.is_empty() {
        line.push_str(&format!(" ({state})"));
    }
    md.bullet(&line);

    let summary = value_str(metadata, "/summary");
    if !summary.is_empty() {
        md.line(&format!(
            "  summary: {}",
            summary.split_whitespace().collect::<Vec<_>>().join(" ")
        ));
    }
    let category = value_str(metadata, "/category");
    let targets = string_array(metadata.get("targets"));
    let support_count = skill
        .get("support_file_count")
        .and_then(Value::as_i64)
        .or_else(|| {
            skill
                .get("support_files")
                .and_then(Value::as_array)
                .map(|files| files.len() as i64)
        });
    let mut details = Vec::new();
    if !category.is_empty() {
        details.push(format!("category: {category}"));
    }
    if !targets.is_empty() {
        details.push(format!("targets: {targets}"));
    }
    if let Some(count) = support_count {
        details.push(format!("support_files: {count}"));
    }
    if !details.is_empty() {
        md.line(&format!("  {}", details.join("; ")));
    }
}
