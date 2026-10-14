//! Session-import normalizers against the raw Codex, Cursor, and Pi fixtures
//! the cass comparison measured.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;
use tracedecay_capture::parse_rfc3339_timestamp;
use tracedecay_capture::{codex, cursor, pi};
use tracedecay_domain::{CanonicalObservationFactV1, ObservationSourceRangeV1};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Counts {
    messages: u64,
    tool_calls: u64,
    timestamps: u64,
    parse_errors: u64,
}

fn repo_path(relative: &str) -> PathBuf {
    PathBuf::from(relative)
}

fn load_records(path: &Path) -> Vec<Value> {
    let text =
        fs::read_to_string(path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    let lines: Vec<&str> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    if lines.iter().all(|line| line.trim_start().starts_with('{')) {
        return lines
            .into_iter()
            .map(|line| serde_json::from_str(line).unwrap_or_else(|error| panic!("{error}")))
            .collect();
    }
    vec![serde_json::from_str(&text).unwrap_or_else(|error| panic!("{error}"))]
}

fn raw_timestamp(value: &Value) -> bool {
    value
        .get("timestamp")
        .and_then(Value::as_str)
        .is_some_and(|timestamp| parse_rfc3339_timestamp(timestamp).is_some())
}

fn is_codex_message(value: &Value) -> bool {
    let payload = value.get("payload").unwrap_or(value);
    match payload.get("type").and_then(Value::as_str) {
        Some("message") => {
            matches!(
                payload.get("role").and_then(Value::as_str),
                Some("user" | "assistant")
            )
        }
        Some("agent_message" | "user_message") => true,
        Some("item_completed") => matches!(
            payload.pointer("/item/type").and_then(Value::as_str),
            Some("UserMessage" | "AgentMessage")
        ),
        _ => false,
    }
}

fn is_cursor_message(value: &Value) -> bool {
    matches!(
        value.get("role").and_then(Value::as_str),
        Some("user" | "assistant")
    )
}

fn is_pi_message(value: &Value) -> bool {
    value.get("type").and_then(Value::as_str) == Some("message")
        && matches!(
            value.pointer("/message/role").and_then(Value::as_str),
            Some("user" | "assistant" | "toolResult")
        )
}

fn raw_tool_calls(codex: bool, value: &Value) -> u64 {
    if codex {
        let payload = value.get("payload").unwrap_or(value);
        return match payload.get("type").and_then(Value::as_str) {
            Some("function_call" | "custom_tool_call") => 1,
            _ => 0,
        };
    }
    let mut count = 0_u64;
    let mut stack = vec![value];
    while let Some(current) = stack.pop() {
        match current {
            Value::Object(fields) => {
                if matches!(
                    fields.get("type").and_then(Value::as_str),
                    Some("tool_use" | "toolCall")
                ) {
                    count = count.saturating_add(1);
                }
                stack.extend(fields.values());
            }
            Value::Array(items) => stack.extend(items.iter()),
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
        }
    }
    count
}

fn raw_counts(records: &[Value], message: fn(&Value) -> bool, codex_tools: bool) -> Counts {
    let mut counts = Counts {
        messages: 0,
        tool_calls: 0,
        timestamps: 0,
        parse_errors: 0,
    };
    for value in records {
        if message(value) {
            counts.messages = counts.messages.saturating_add(1);
            if raw_timestamp(value) {
                counts.timestamps = counts.timestamps.saturating_add(1);
            }
        }
        counts.tool_calls = counts
            .tool_calls
            .saturating_add(raw_tool_calls(codex_tools, value));
    }
    counts
}

fn tally(counts: &mut Counts, facts: &[CanonicalObservationFactV1]) {
    for fact in facts {
        match fact {
            CanonicalObservationFactV1::Message { timestamp, .. } => {
                counts.messages = counts.messages.saturating_add(1);
                if timestamp.is_some() {
                    counts.timestamps = counts.timestamps.saturating_add(1);
                }
            }
            CanonicalObservationFactV1::ToolInvocation { .. } => {
                counts.tool_calls = counts.tool_calls.saturating_add(1);
            }
            _ => {}
        }
    }
}

fn range_for(index: usize) -> ObservationSourceRangeV1 {
    let start = u64::try_from(index).unwrap();
    ObservationSourceRangeV1::new(start, start.saturating_add(1)).unwrap()
}

fn parse_codex(records: &[Value]) -> Counts {
    let mut counts = Counts {
        messages: 0,
        tool_calls: 0,
        timestamps: 0,
        parse_errors: 0,
    };
    let session_id = records
        .iter()
        .find_map(|record| {
            if record.get("type").and_then(Value::as_str) == Some("session_meta") {
                record.pointer("/payload/id").and_then(Value::as_str)
            } else {
                None
            }
        })
        .unwrap_or("codex-compare");
    for (index, value) in records.iter().enumerate() {
        let record_id = match codex::codex_native_record_id(session_id, value) {
            Ok(record_id) => record_id,
            Err(_) => {
                counts.parse_errors = counts.parse_errors.saturating_add(1);
                continue;
            }
        };
        match codex::normalize_codex_observation(
            value,
            session_id,
            Some(session_id),
            record_id,
            range_for(index),
        ) {
            Ok(envelope) => tally(&mut counts, envelope.facts()),
            Err(_) => counts.parse_errors = counts.parse_errors.saturating_add(1),
        }
    }
    counts
}

fn parse_cursor(records: &[Value]) -> Counts {
    let mut counts = Counts {
        messages: 0,
        tool_calls: 0,
        timestamps: 0,
        parse_errors: 0,
    };
    for (index, value) in records.iter().enumerate() {
        let record_id = match cursor::observation_native_record_id("cursor-compare", value) {
            Ok(record_id) => record_id,
            Err(_) => {
                counts.parse_errors = counts.parse_errors.saturating_add(1);
                continue;
            }
        };
        match cursor::normalize_cursor_observation(
            value,
            "cursor-compare",
            record_id,
            range_for(index),
            None,
            None,
        ) {
            Ok(envelope) => tally(&mut counts, envelope.facts()),
            Err(_) => counts.parse_errors = counts.parse_errors.saturating_add(1),
        }
    }
    counts
}

fn parse_pi(records: &[Value]) -> Counts {
    let mut counts = Counts {
        messages: 0,
        tool_calls: 0,
        timestamps: 0,
        parse_errors: 0,
    };
    let session_id = pi::session_id_from_file_name(
        "2026-09-25T16-00-00-000Z_5f0c2a8e-3b1d-4c7e-9a2f-6d8e1b4c7a90.jsonl",
    )
    .unwrap();
    for (index, value) in records.iter().enumerate() {
        match pi::normalize_observation(
            value,
            &session_id,
            Some("pi-session.jsonl"),
            range_for(index),
        ) {
            Ok(envelope) => tally(&mut counts, envelope.facts()),
            Err(_) => counts.parse_errors = counts.parse_errors.saturating_add(1),
        }
    }
    counts
}

#[test]
fn session_import_fixtures_match_raw_message_tool_and_timestamp_counts() {
    let codex = [
        load_records(&repo_path(
            "../../benchmark_data/runtime/fixtures/providers/home/.codex/sessions/2026/07/fixture-session.jsonl",
        )),
        load_records(&repo_path(
            "../../tests/fixtures/provider_normalization/codex/session_meta.input.json",
        )),
        load_records(&repo_path(
            "../../tests/fixtures/provider_normalization/codex/agent_message.input.json",
        )),
        load_records(&repo_path(
            "../../tests/fixtures/provider_normalization/codex/function_call.input.json",
        )),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();
    let cursor = [
        load_records(&repo_path(
            "../../benchmark_data/runtime/fixtures/providers/home/.cursor/projects/fixture-runtime-project/agent-transcripts/fixture-session.jsonl",
        )),
        load_records(&repo_path(
            "../../tests/fixtures/provider_normalization/cursor/tool_use.input.json",
        )),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();
    let pi_records = load_records(&repo_path(
        "../../tests/fixtures/transcript_golden/pi/2026-09-25T16-00-00-000Z_5f0c2a8e-3b1d-4c7e-9a2f-6d8e1b4c7a90.jsonl",
    ));

    let codex_raw = raw_counts(&codex, is_codex_message, true);
    let cursor_raw = raw_counts(&cursor, is_cursor_message, false);
    let pi_raw = raw_counts(&pi_records, is_pi_message, false);
    assert_eq!(observed(&codex_raw), (3, 1, 3, 0));
    assert_eq!(observed(&cursor_raw), (3, 1, 2, 0));
    assert_eq!(observed(&pi_raw), (5, 1, 5, 0));
    assert_eq!(observed(&parse_codex(&codex)), (3, 1, 3, 0));
    assert_eq!(observed(&parse_cursor(&cursor)), (3, 1, 2, 0));
    assert_eq!(observed(&parse_pi(&pi_records)), (5, 1, 5, 0));
}

fn observed(counts: &Counts) -> (u64, u64, u64, u64) {
    (
        counts.messages,
        counts.tool_calls,
        counts.timestamps,
        counts.parse_errors,
    )
}
