//! Rendering and memory enrichment support for the verified context handler.

use std::fmt::Write as _;

use serde_json::{Value, json};

use crate::context::{
    CONTEXT_CODE_HEADING, CONTEXT_ENTRY_POINTS_HEADING, CONTEXT_EXTENSION_POINTS_HEADING,
    CONTEXT_INDEX_COVERAGE_HINT_HEADING, CONTEXT_MEMORY_FEEDBACK_HINT,
    CONTEXT_MEMORY_MATCHES_HEADING, CONTEXT_RELATED_SYMBOLS_HEADING, CONTEXT_SEEN_NODE_IDS_LABEL,
    CONTEXT_TEST_COVERAGE_HEADING,
};
use crate::errors::Result;
use crate::memory::types::{FactSearchResult, SearchFactsRequest};
use crate::text::utf8_prefix_at_or_before;
use crate::tracedecay::TraceDecay;

const CONTEXT_MEMORY_MATCH_LIMIT: usize = 3;
const CONTEXT_MEMORY_MATCH_LIMIT_MAX: usize = 10;
const CONTEXT_LANE_TRUNCATED_NOTE: &str =
    "\n... lane truncated; retrieve the full response handle for omitted details.\n";

pub(super) fn context_markdown_lane_preview(markdown: &str) -> String {
    let mut preview = String::with_capacity(markdown.len().min(24_000));
    let mut lane = String::new();
    let mut lane_key = String::new();
    let mut in_fence = false;

    for line in markdown.split_inclusive('\n') {
        if !in_fence && let Some(key) = context_lane_key(line) {
            push_context_lane_preview(&mut preview, &lane_key, &lane);
            lane.clear();
            lane_key = key.to_string();
        }
        lane.push_str(line);
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
        }
    }
    push_context_lane_preview(&mut preview, &lane_key, &lane);
    preview
}

fn context_lane_key(line: &str) -> Option<&str> {
    if line.starts_with("### ") || line.starts_with(CONTEXT_SEEN_NODE_IDS_LABEL) {
        Some(line.trim_end())
    } else {
        None
    }
}

fn push_context_lane_preview(preview: &mut String, lane_key: &str, lane: &str) {
    if lane.is_empty() {
        return;
    }
    let budget = context_lane_budget(lane_key);
    if lane.len() <= budget {
        preview.push_str(lane);
        return;
    }
    let prefix = utf8_prefix_at_or_before(lane, budget);
    preview.push_str(prefix);
    if crate::mcp::tools::render::has_open_markdown_fence(prefix) {
        preview.push_str("\n```\n");
    }
    preview.push_str(CONTEXT_LANE_TRUNCATED_NOTE);
}

fn context_lane_budget(lane_key: &str) -> usize {
    if lane_key.starts_with(CONTEXT_SEEN_NODE_IDS_LABEL) {
        usize::MAX
    } else if lane_key.starts_with(CONTEXT_CODE_HEADING) {
        8_500
    } else if lane_key.starts_with(CONTEXT_RELATED_SYMBOLS_HEADING) {
        3_500
    } else if lane_key.starts_with(CONTEXT_ENTRY_POINTS_HEADING)
        || lane_key.starts_with(CONTEXT_TEST_COVERAGE_HEADING)
    {
        3_000
    } else if lane_key.starts_with(CONTEXT_MEMORY_MATCHES_HEADING) {
        2_500
    } else if lane_key.starts_with(CONTEXT_EXTENSION_POINTS_HEADING) {
        1_500
    } else if lane_key.starts_with(CONTEXT_INDEX_COVERAGE_HINT_HEADING) {
        1_000
    } else {
        2_000
    }
}

pub(super) fn insert_context_memory_section(
    output: &mut String,
    memory_matches: &[FactSearchResult],
    memory_matches_error: Option<&str>,
) {
    let Some(section) = context_memory_section(memory_matches, memory_matches_error) else {
        return;
    };
    if let Some(idx) = output.find(&format!("\n{CONTEXT_ENTRY_POINTS_HEADING}")) {
        output.insert_str(idx, &section);
    } else {
        output.push_str(&section);
    }
}

pub(super) fn context_memory_section(
    memory_matches: &[FactSearchResult],
    memory_matches_error: Option<&str>,
) -> Option<String> {
    let mut section = String::new();
    if !memory_matches.is_empty() {
        section.push('\n');
        section.push_str(CONTEXT_MEMORY_MATCHES_HEADING);
        section.push('\n');
        for hit in memory_matches {
            let fact = &hit.fact;
            let _ = writeln!(
                section,
                "- fact_id={} category={} trust={:.2} score={:.3}: {}",
                fact.fact_id,
                fact.category,
                fact.trust_score,
                hit.score,
                compact_memory_content(&fact.content)
            );
        }
        section.push('\n');
        section.push_str(CONTEXT_MEMORY_FEEDBACK_HINT);
        section.push('\n');
        return Some(section);
    }
    if let Some(err) = memory_matches_error {
        let _ = writeln!(
            section,
            "\n{CONTEXT_MEMORY_MATCHES_HEADING}\nUnavailable: {err}"
        );
        return Some(section);
    }
    None
}

fn compact_memory_content(content: &str) -> String {
    content.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub(super) struct ContextMemoryOptions {
    include_memory: bool,
    limit: usize,
    min_trust: f64,
}

pub(super) fn context_memory_options(args: &Value) -> ContextMemoryOptions {
    let include_memory = args
        .get("include_memory")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let limit = args
        .get("memory_limit")
        .and_then(Value::as_u64)
        .map_or(CONTEXT_MEMORY_MATCH_LIMIT, |value| value as usize)
        .clamp(1, CONTEXT_MEMORY_MATCH_LIMIT_MAX);
    let min_trust = args
        .get("memory_min_trust")
        .and_then(Value::as_f64)
        .unwrap_or(0.5)
        .clamp(0.0, 1.0);
    ContextMemoryOptions {
        include_memory,
        limit,
        min_trust,
    }
}

pub(super) fn context_memory_enabled(options: &ContextMemoryOptions) -> bool {
    options.include_memory
}

pub(super) fn context_memory_analytics_value(
    options: &ContextMemoryOptions,
    memory_matches: &[FactSearchResult],
    memory_matches_error: Option<&str>,
) -> Value {
    let fact_ids: Vec<Value> = memory_matches
        .iter()
        .map(|hit| Value::from(hit.fact.fact_id))
        .collect();
    json!({
        "include_memory": options.include_memory,
        "limit": options.limit,
        "min_trust": options.min_trust,
        "match_count": fact_ids.len(),
        "fact_ids": fact_ids,
        "error": memory_matches_error,
    })
}

pub(super) async fn context_memory_matches(
    cg: &TraceDecay,
    task: &str,
    options: &ContextMemoryOptions,
) -> Result<Vec<FactSearchResult>> {
    cg.search_facts_untracked(SearchFactsRequest {
        query: task.to_string(),
        category: None,
        limit: Some(options.limit),
        min_trust: Some(options.min_trust),
        include_why: false,
    })
    .await
}
