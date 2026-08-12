use std::fmt::Write as _;

use serde_json::Value;

use super::render::{self, Md};

pub(super) fn skill_list_md(value: &Value) -> String {
    let mut md = Md::new();
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

pub(super) fn skill_view_md(value: &Value) -> String {
    let mut md = Md::new();
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
                    .map(Vec::len)
                    .unwrap_or_default();
                md.bullet(&format!("**{path}** - {bytes} bytes"));
            }
        }
    }
    md.render()
}

pub(super) fn automation_artifact_md(value: &Value) -> String {
    let mut md = Md::new();
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

pub(super) fn automation_run_list_md(value: &Value) -> String {
    let mut md = Md::new();
    md.heading(2, "Automation Runs");
    for key in [
        "status",
        "count",
        "limit",
        "has_more",
        "malformed_row_count",
        "completeness",
    ] {
        let rendered = render::generic_md(value.get(key).unwrap_or(&Value::Null));
        md.field(key, rendered.trim());
    }
    md.blank().heading(3, "Runs");
    let Some(runs) = value.get("runs").and_then(Value::as_array) else {
        md.empty_note("No runs field returned.");
        return md.render();
    };
    if runs.is_empty() {
        md.empty_note("No automation runs recorded.");
    } else {
        for run in runs {
            let run_id = render::field_str(run, "run_id");
            let task = render::field_str(run, "task");
            let status = render::field_str(run, "status");
            let completed_at = render::field_str(run, "completed_at");
            md.bullet(&format!(
                "**{run_id}** - task: {task}; status: {status}; completed_at: {completed_at}"
            ));
        }
    }
    md.render()
}

pub(super) fn automation_run_view_md(value: &Value) -> String {
    let mut md = Md::new();
    let record = value.get("run").unwrap_or(&Value::Null);
    let run_id = render::field_str(record, "run_id");
    md.heading(2, &format!("Automation Run: {run_id}"));
    md.field("status", render::field_str(value, "status"));
    for key in [
        "task",
        "trigger",
        "backend",
        "model",
        "status",
        "started_at",
        "completed_at",
    ] {
        let text = render::field_str(record, key);
        if !text.is_empty() {
            md.field(key, text);
        }
    }
    for key in [
        "reviewed_count",
        "accepted_count",
        "rejected_count",
        "skipped_count",
    ] {
        if let Some(count) = record.get(key).and_then(Value::as_u64) {
            md.field(key, &count.to_string());
        }
    }
    if let Some(artifacts) = record.get("artifacts").and_then(Value::as_array) {
        md.blank().heading(3, "Artifacts");
        if artifacts.is_empty() {
            md.empty_note("No artifacts recorded.");
        } else {
            for artifact in artifacts {
                let kind = render::field_str(artifact, "kind");
                let sha256 = render::field_str(artifact, "sha256");
                md.bullet(&format!("**{kind}** - sha256: {sha256}"));
            }
        }
    }
    md.render()
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
        let _ = write!(line, " ({state})");
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

pub(super) fn analytics_md(value: &Value) -> String {
    let mut md = Md::new();
    md.heading(2, "Usage Analytics");
    md.field("scope", render::field_str(value, "scope"));
    let project_root = render::field_str(value, "project_root");
    if !project_root.is_empty() {
        md.field("project", project_root);
    }
    md.field(
        "window_days",
        &render::field_i64(value, "window_days").to_string(),
    );
    md.field(
        "event_count",
        &render::field_i64(value, "event_count").to_string(),
    );
    if value
        .get("event_count_truncated")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        md.field("event_count_truncated", "true");
    }

    if let Some(tools) = value.get("tools") {
        md.blank().heading(3, "Tools");
        append_analytics_tools(&mut md, tools);
    }
    if let Some(hints) = value.get("hints") {
        md.blank().heading(3, "Hints");
        append_analytics_hints(&mut md, hints);
    }
    if let Some(facts) = value.get("facts") {
        md.blank().heading(3, "Facts");
        append_analytics_facts(&mut md, facts);
    }
    if let Some(automation) = value.get("automation") {
        md.blank().heading(3, "Automation");
        append_analytics_automation(&mut md, automation);
    }
    md.render()
}

fn append_analytics_tools(md: &mut Md, tools: &Value) {
    if !tools
        .get("available")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        md.empty_note("No MCP tool calls recorded in this window.");
        return;
    }
    md.field(
        "distinct_tools_called",
        &render::field_i64(tools, "distinct_tools_called").to_string(),
    );
    if let Some(tiers) = tools.get("tiers").and_then(Value::as_array) {
        md.blank().heading(4, "By Tier");
        for tier in tiers {
            md.bullet(&format!(
                "**{}** - {} calls, {} errors",
                render::field_str(tier, "tier"),
                render::field_i64(tier, "calls"),
                render::field_i64(tier, "errors"),
            ));
        }
    }
    if let Some(top) = tools.get("top_tools").and_then(Value::as_array) {
        md.blank().heading(4, "Top Tools");
        if top.is_empty() {
            md.empty_note("None.");
        } else {
            for tool in top {
                md.bullet(&format!(
                    "**{}** ({}) - {} calls, {} errors",
                    render::field_str(tool, "tool_name"),
                    render::field_str(tool, "tier"),
                    render::field_i64(tool, "calls"),
                    render::field_i64(tool, "errors"),
                ));
            }
        }
    }
    if let Some(zero_call) = tools.get("zero_call_tools") {
        let count = render::field_i64(zero_call, "count");
        md.blank().heading(4, "Zero-Call Defined Tools");
        md.field("count", &count.to_string());
        if let Some(sample) = zero_call.get("sample").and_then(Value::as_array) {
            let names: Vec<&str> = sample.iter().filter_map(Value::as_str).collect();
            if !names.is_empty() {
                md.line(&names.join(", "));
            }
            if zero_call
                .get("sample_truncated")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                md.line(&format!(
                    "... {} more not shown",
                    count as usize - names.len()
                ));
            }
        }
    }
}

fn append_analytics_hints(md: &mut Md, hints: &Value) {
    if !hints
        .get("available")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        md.empty_note("No hint telemetry available for this window.");
        return;
    }
    let Some(by_category) = hints.get("by_category").and_then(Value::as_array) else {
        md.empty_note("No hint categories reported.");
        return;
    };
    let active: Vec<&Value> = by_category
        .iter()
        .filter(|row| {
            ["emitted", "followed", "ignored", "suppressed"]
                .iter()
                .any(|key| render::field_i64(row, key) > 0)
        })
        .collect();
    if active.is_empty() {
        md.empty_note("No hints emitted in this window.");
        return;
    }
    for row in active {
        md.bullet(&format!(
            "**{}** - emitted {}, followed {}, ignored {}, suppressed {}",
            render::field_str(row, "category"),
            render::field_i64(row, "emitted"),
            render::field_i64(row, "followed"),
            render::field_i64(row, "ignored"),
            render::field_i64(row, "suppressed"),
        ));
    }
}

fn append_analytics_facts(md: &mut Md, facts: &Value) {
    if !facts
        .get("available")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        let reason = render::field_str(facts, "reason");
        md.empty_note(if reason.is_empty() {
            "Fact-store funnel unavailable."
        } else {
            reason
        });
        return;
    }
    md.field("facts", &render::field_i64(facts, "facts").to_string());
    md.field(
        "facts_retrieved",
        &render::field_i64(facts, "facts_retrieved").to_string(),
    );
    md.field(
        "retrievals",
        &render::field_i64(facts, "retrievals").to_string(),
    );
    md.field(
        "facts_rated",
        &render::field_i64(facts, "facts_rated").to_string(),
    );
    md.field(
        "helpful_feedback",
        &render::field_i64(facts, "helpful_feedback").to_string(),
    );
    md.field(
        "unhelpful_feedback",
        &render::field_i64(facts, "unhelpful_feedback").to_string(),
    );
}

fn append_analytics_automation(md: &mut Md, automation: &Value) {
    if !automation
        .get("available")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        let reason = render::field_str(automation, "reason");
        md.empty_note(if reason.is_empty() {
            "Automation run ledger unavailable."
        } else {
            reason
        });
        return;
    }
    md.field(
        "records_in_window",
        &render::field_i64(automation, "records_in_window").to_string(),
    );
    let Some(by_job) = automation.get("by_job").and_then(Value::as_array) else {
        md.empty_note("No automation jobs recorded.");
        return;
    };
    if by_job.is_empty() {
        md.empty_note("No automation runs in this window.");
        return;
    }
    for job in by_job {
        md.bullet(&format!(
            "**{}** - succeeded {}, failed {}, skipped {}, other {}",
            render::field_str(job, "job"),
            render::field_i64(job, "succeeded"),
            render::field_i64(job, "failed"),
            render::field_i64(job, "skipped"),
            render::field_i64(job, "other"),
        ));
    }
}
