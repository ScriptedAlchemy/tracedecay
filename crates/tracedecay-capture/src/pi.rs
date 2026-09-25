//! Pi coding-agent session entries.
//!
//! A Pi session file is JSONL: a `session` header line, then entries that each
//! carry a session-unique `id` and the `parentId` they branch from. The entry
//! id is the stable record identity, so a record keeps its identity when Pi
//! rewrites the file (version migration) or a later branch reorders it.

use serde_json::{Map, Value};
use tracedecay_domain::{
    CanonicalBoundaryKindV1, CanonicalGitEvidenceKindV1, CanonicalMessageRoleV1,
    CanonicalObservationEnvelopeV1, CanonicalObservationEvidenceV1, CanonicalObservationFactV1,
    CanonicalObservationRelationsV1, CanonicalReasoningVisibilityV1, CanonicalUnknownStateV1,
    ObservationId, ObservationOrderingDomainV1, ObservationSourceRangeV1, ProviderId, SessionId,
};

use crate::ObservationRecordParseErrorV1;
use crate::content::content_is_empty;
use crate::timestamp::{parse_rfc3339_timestamp, timestamp_secs};

const PROVIDER: &str = "pi";
const HEADER_RECORD_ID: &str = "session";

/// The validated `session` header that opens every Pi session file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PiSessionHeader {
    pub session_id: String,
    pub cwd: String,
    /// Session id of the file this session was forked or cloned from.
    pub parent_session_id: Option<String>,
}

/// Parse the header line. Anything other than a `session` object carrying a
/// non-empty id and cwd is not a Pi session file.
pub fn parse_session_header(line: &[u8]) -> Option<PiSessionHeader> {
    session_header(&serde_json::from_slice::<Value>(line).ok()?)
}

fn session_header(header: &Value) -> Option<PiSessionHeader> {
    if header.get("type").and_then(Value::as_str) != Some("session") {
        return None;
    }
    let session_id = non_empty_str(header, "id")?.to_owned();
    let cwd = non_empty_str(header, "cwd")?.to_owned();
    let parent_session_id = header
        .get("parentSession")
        .and_then(Value::as_str)
        .and_then(session_id_from_file_name);
    Some(PiSessionHeader {
        session_id,
        cwd,
        parent_session_id,
    })
}

/// Pi names session files `<timestamp>_<session-id>.jsonl`.
pub fn session_id_from_file_name(path: &str) -> Option<String> {
    let name = path.rsplit(['/', '\\']).next()?;
    let (_, session_id) = name.strip_suffix(".jsonl")?.split_once('_')?;
    (!session_id.is_empty()).then(|| session_id.to_owned())
}

/// `{session}:{entry id}` for entries, `{session}:session` for the header.
pub fn native_record_id(
    session_id: &str,
    entry_id: &str,
) -> Result<ObservationId, ObservationRecordParseErrorV1> {
    ObservationId::new(format!("{session_id}:{entry_id}")).map_err(|_| invalid())
}

pub fn normalize_observation(
    native: &Value,
    session_id: &str,
    range: ObservationSourceRangeV1,
) -> Result<CanonicalObservationEnvelopeV1, ObservationRecordParseErrorV1> {
    hotpath::gauge!("capture.pi.record_bytes").inc(range.end() - range.start());
    let envelope = normalize_pi_entry(native, session_id, range);
    if envelope.is_err() {
        hotpath::gauge!("capture.pi.normalize_failures").inc(1u64);
    }
    envelope
}

#[hotpath::measure(label = "capture.pi.normalize")]
fn normalize_pi_entry(
    native: &Value,
    session_id: &str,
    range: ObservationSourceRangeV1,
) -> Result<CanonicalObservationEnvelopeV1, ObservationRecordParseErrorV1> {
    let kind = native
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(invalid)?;
    let session = SessionId::new(session_id).map_err(|_| invalid())?;
    let mut relations = CanonicalObservationRelationsV1::new(session);
    let mut facts = Vec::new();
    let timestamp = native
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(parse_rfc3339_timestamp);

    let (stable_record_id, native_kind) = if kind == "session" {
        let header = session_header(native).ok_or_else(invalid)?;
        if header.session_id != session_id {
            return Err(invalid());
        }
        if let Some(parent) = header.parent_session_id {
            relations =
                relations.with_parent_session_id(SessionId::new(parent).map_err(|_| invalid())?);
        }
        facts.push(session_fact(Some(header.cwd), None, timestamp));
        (
            native_record_id(session_id, HEADER_RECORD_ID)?,
            kind.to_owned(),
        )
    } else {
        let entry_id = non_empty_str(native, "id").ok_or_else(invalid)?;
        let stable_record_id = native_record_id(session_id, entry_id)?;
        if let Some(parent_id) = native.get("parentId").and_then(Value::as_str) {
            relations = relations.with_parent_message_id(native_record_id(session_id, parent_id)?);
        }
        let native_kind = match kind {
            "message" => {
                let message = native
                    .get("message")
                    .filter(|message| message.is_object())
                    .ok_or_else(invalid)?;
                let role = message
                    .get("role")
                    .and_then(Value::as_str)
                    .ok_or_else(invalid)?;
                append_message(&mut facts, message, role, &stable_record_id)?;
                relations = relations.with_message_id(stable_record_id.clone());
                format!("message.{role}")
            }
            "compaction" => {
                facts.push(CanonicalObservationFactV1::Compaction {
                    summary: native
                        .get("summary")
                        .filter(|summary| !content_is_empty(summary))
                        .cloned(),
                    input_tokens: None,
                    output_tokens: None,
                });
                facts.push(CanonicalObservationFactV1::Boundary {
                    boundary_kind: CanonicalBoundaryKindV1::CompactionBoundary,
                });
                kind.to_owned()
            }
            "session_info" => {
                let title = non_empty_str(native, "name").map(str::to_owned);
                facts.push(session_fact(None, title, None));
                kind.to_owned()
            }
            other => {
                facts.push(unsupported(other));
                other.to_owned()
            }
        };
        (stable_record_id, native_kind)
    };

    let mut evidence =
        CanonicalObservationEvidenceV1::new(ObservationOrderingDomainV1::FileBytes, range);
    if let Some(timestamp) = timestamp {
        evidence = evidence.with_native_timestamp(timestamp);
    }
    CanonicalObservationEnvelopeV1::new(
        ProviderId::new(PROVIDER).map_err(|_| invalid())?,
        &native_kind,
        stable_record_id,
        relations,
        facts,
        evidence,
    )
    .map_err(|_| invalid())
}

fn session_fact(
    project_path: Option<String>,
    title: Option<String>,
    started_at: Option<i64>,
) -> CanonicalObservationFactV1 {
    CanonicalObservationFactV1::Session {
        project_path,
        location_path: None,
        transcript_path: None,
        title,
        started_at,
        ended_at: None,
        source: Some("pi_session".to_owned()),
        native_source: Some(PROVIDER.to_owned()),
        profile: None,
        location_provenance: None,
    }
}

fn append_message(
    facts: &mut Vec<CanonicalObservationFactV1>,
    message: &Value,
    role: &str,
    stable_record_id: &ObservationId,
) -> Result<(), ObservationRecordParseErrorV1> {
    let timestamp = message.get("timestamp").and_then(timestamp_secs);
    let model = message
        .get("model")
        .and_then(Value::as_str)
        .map(str::to_owned);
    match role {
        "user" | "custom" => push_text_message(
            facts,
            CanonicalMessageRoleV1::User,
            message.get("content"),
            None,
            timestamp,
        ),
        "assistant" => {
            let blocks = message
                .get("content")
                .and_then(Value::as_array)
                .ok_or_else(invalid)?;
            let mut visible = Vec::new();
            for block in blocks {
                match block.get("type").and_then(Value::as_str) {
                    Some("thinking") => {
                        let text = block
                            .get("thinking")
                            .filter(|text| !content_is_empty(text))
                            .cloned();
                        let redacted = block.get("redacted").and_then(Value::as_bool) == Some(true);
                        facts.push(CanonicalObservationFactV1::Reasoning {
                            visibility: if redacted || text.is_none() {
                                CanonicalReasoningVisibilityV1::Redacted
                            } else {
                                CanonicalReasoningVisibilityV1::Visible
                            },
                            content: text,
                        });
                    }
                    Some("toolCall") => append_tool_call(facts, block, timestamp)?,
                    _ => visible.push(content_block(block)),
                }
            }
            push_text_message(
                facts,
                CanonicalMessageRoleV1::Assistant,
                Some(&Value::Array(visible)),
                model,
                timestamp,
            );
        }
        "toolResult" => {
            let invocation_id = non_empty_str(message, "toolCallId")
                .map(ObservationId::new)
                .transpose()
                .map_err(|_| invalid())?;
            facts.push(CanonicalObservationFactV1::ToolResult {
                invocation_id,
                content: visible_content(message.get("content").unwrap_or(&Value::Null)),
                success: message
                    .get("isError")
                    .and_then(Value::as_bool)
                    .map(|is_error| !is_error),
            });
        }
        "bashExecution" => {
            // A user-run `!` command, not a model tool call: the invocation id
            // is rooted in the record id so it is never served as the host's.
            let invocation_id = ObservationId::new(format!("{}:tool:0", stable_record_id.as_str()))
                .map_err(|_| invalid())?;
            facts.push(CanonicalObservationFactV1::ToolInvocation {
                invocation_id: invocation_id.clone(),
                name: "bash".to_owned(),
                arguments: serde_json::json!({ "command": message.get("command") }),
            });
            facts.push(CanonicalObservationFactV1::ToolResult {
                invocation_id: Some(invocation_id),
                content: message.get("output").cloned().unwrap_or(Value::Null),
                success: message
                    .get("exitCode")
                    .and_then(Value::as_i64)
                    .map(|code| code == 0),
            });
        }
        // The prompt and tool loadout replay; not conversation content.
        other => facts.push(unsupported(&format!("message.{other}"))),
    }
    if facts.is_empty() {
        return Err(ObservationRecordParseErrorV1::Empty);
    }
    Ok(())
}

fn push_text_message(
    facts: &mut Vec<CanonicalObservationFactV1>,
    role: CanonicalMessageRoleV1,
    content: Option<&Value>,
    model: Option<String>,
    timestamp: Option<i64>,
) {
    let Some(content) = content
        .map(visible_content)
        .filter(|c| !content_is_empty(c))
    else {
        return;
    };
    facts.push(CanonicalObservationFactV1::Message {
        role,
        content,
        model,
        timestamp,
    });
}

/// Pi's built-in `edit` and `write` tools name the file they change in
/// `arguments.path`; that call is the edited-file evidence.
// ponytail: an edit is recorded from the model's call, so a call whose
// result later reports `isError` still names its path; correlating the
// result would need cross-record state.
fn append_tool_call(
    facts: &mut Vec<CanonicalObservationFactV1>,
    block: &Value,
    timestamp: Option<i64>,
) -> Result<(), ObservationRecordParseErrorV1> {
    let name = non_empty_str(block, "name").ok_or_else(invalid)?;
    let invocation_id = ObservationId::new(non_empty_str(block, "id").ok_or_else(invalid)?)
        .map_err(|_| invalid())?;
    let arguments = block.get("arguments").cloned().unwrap_or(Value::Null);
    if matches!(name, "edit" | "write")
        && let Some(path) = arguments
            .get("path")
            .and_then(Value::as_str)
            .filter(|path| !path.is_empty())
    {
        let mut content = Map::new();
        content.insert("type".to_owned(), Value::String("toolCall".to_owned()));
        content.insert(
            "id".to_owned(),
            Value::String(invocation_id.as_str().to_owned()),
        );
        content.insert("change_type".to_owned(), Value::String(name.to_owned()));
        if let Some(micros) = timestamp.and_then(|secs| secs.checked_mul(1_000_000)) {
            content.insert("edited_at_micros".to_owned(), Value::from(micros));
        }
        facts.push(CanonicalObservationFactV1::Git {
            evidence_kind: CanonicalGitEvidenceKindV1::FileEdit,
            reference: Some(path.to_owned()),
            content: Some(Value::Object(content)),
        });
    }
    facts.push(CanonicalObservationFactV1::ToolInvocation {
        invocation_id,
        name: name.to_owned(),
        arguments,
    });
    Ok(())
}

/// Image payloads are base64; the transcript keeps their presence and media
/// type, never the bytes.
fn visible_content(content: &Value) -> Value {
    match content {
        Value::Array(blocks) => Value::Array(blocks.iter().map(content_block).collect()),
        other => other.clone(),
    }
}

fn content_block(block: &Value) -> Value {
    if block.get("type").and_then(Value::as_str) == Some("image") {
        return serde_json::json!({ "type": "image", "mimeType": block.get("mimeType") });
    }
    block.clone()
}

fn unsupported(native_kind: &str) -> CanonicalObservationFactV1 {
    CanonicalObservationFactV1::Unknown {
        native_kind: native_kind.to_owned(),
        state: CanonicalUnknownStateV1::Unsupported,
    }
}

fn non_empty_str<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
}

const fn invalid() -> ObservationRecordParseErrorV1 {
    ObservationRecordParseErrorV1::InvalidCanonicalEnvelope
}
