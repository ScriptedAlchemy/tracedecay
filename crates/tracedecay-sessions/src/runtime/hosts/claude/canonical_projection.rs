use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::{Map, Value};
use tracedecay_domain::{
    CanonicalBoundaryKindV1, CanonicalGitEvidenceKindV1, CanonicalMessageRoleV1,
    CanonicalObservationEnvelopeV1, CanonicalObservationFactV1, CanonicalWorkflowEvidenceKindV1,
};
use tracedecay_runtime_core::logging::log_daemon_event;

use crate::runtime::SessionMessageRecord;
use crate::runtime::shared::{
    ProjectRootMatcherCache, TranscriptLocation, content_storage_text_and_tools,
};
use crate::runtime::source::SessionDraft;

use super::record_metadata::{
    SessionAccumulator, append_claude_location_metadata, append_git_operation_metadata,
    model_fallback_row, pr_link_row,
};
use super::source_records::{
    ClaudeRecordContext, ClaudeRecordDisposition, retain_unchanged_tool_event_ids,
    system_hook_message_from_line,
};
use super::{CLAUDE_MESSAGE_LOCATION_KEYS, PROVIDER};

pub(super) fn map_canonical_claude_record(
    envelope: &CanonicalObservationEnvelopeV1,
    context: &ClaudeRecordContext<'_>,
    worktree_cache: Option<&ProjectRootMatcherCache>,
) -> ClaudeRecordDisposition {
    if envelope.validate().is_err() {
        return ClaudeRecordDisposition::NonConversational;
    }
    let Ok(offset) = i64::try_from(context.offset) else {
        return ClaudeRecordDisposition::NonConversational;
    };
    let source_path = context.source_path.map_or_else(
        || PathBuf::from(format!("claude:{}", context.session_id)),
        PathBuf::from,
    );
    let draft = || SessionDraft {
        session_id: context.session_id.to_owned(),
        project_key: context.project_key.to_owned(),
        project_path: context.project_path.to_owned(),
        title: None,
        metadata_json: None,
        parent_session_id: None,
        is_subagent: false,
        agent_id: None,
        parent_tool_use_id: None,
    };
    let mut accumulator = SessionAccumulator::default();

    if let Some(message) =
        map_canonical_marker_row(envelope, context, &source_path, offset, &mut accumulator)
    {
        return ClaudeRecordDisposition::Message {
            draft: Box::new(draft()),
            message: Box::new(message),
        };
    }

    let message_fact = envelope
        .facts()
        .iter()
        .find(|fact| matches!(fact, CanonicalObservationFactV1::Message { .. }));
    let Some(CanonicalObservationFactV1::Message {
        role,
        content,
        model,
        timestamp,
    }) = message_fact
    else {
        let is_compact_boundary = envelope.facts().iter().any(|fact| {
            matches!(
                fact,
                CanonicalObservationFactV1::Boundary {
                    boundary_kind: CanonicalBoundaryKindV1::CompactionBoundary,
                }
            )
        });
        if !is_compact_boundary {
            return ClaudeRecordDisposition::NonConversational;
        }
        let compact_metadata = envelope.facts().iter().find_map(|fact| match fact {
            CanonicalObservationFactV1::Compaction {
                summary: Some(summary),
                ..
            } => Some(summary),
            _ => None,
        });
        let trigger = compact_metadata
            .and_then(|metadata| metadata.get("trigger"))
            .and_then(Value::as_str);
        let pre_tokens = compact_metadata
            .and_then(|metadata| metadata.get("preTokens"))
            .and_then(Value::as_i64);
        let logical_parent_uuid = envelope
            .relations()
            .parent_message_id()
            .map(|parent| parent.as_str().to_owned());
        let mut metadata = Map::new();
        metadata.insert(
            "source".to_owned(),
            Value::String("claude_compact_boundary".to_owned()),
        );
        if let Some(trigger) = trigger {
            metadata.insert("trigger".to_owned(), Value::String(trigger.to_owned()));
        }
        if let Some(pre_tokens) = pre_tokens {
            metadata.insert("pre_tokens".to_owned(), Value::from(pre_tokens));
        }
        if let Some(logical_parent_uuid) = logical_parent_uuid {
            metadata.insert(
                "logical_parent_uuid".to_owned(),
                Value::String(logical_parent_uuid),
            );
        }
        // LCM native-compaction recognition decodes this envelope to confirm
        // the boundary's preservedSegment.anchorUuid still names the summary.
        insert_canonical_envelope(
            &mut metadata,
            envelope,
            context.session_id,
            envelope.stable_record_id().as_str(),
        );
        let message = SessionMessageRecord {
            provider: PROVIDER.to_owned(),
            message_id: format!(
                "{}:{}",
                super::KIND_COMPACT_BOUNDARY,
                envelope.stable_record_id().as_str()
            ),
            session_id: context.session_id.to_owned(),
            role: "system".to_owned(),
            timestamp: envelope.evidence().native_timestamp(),
            ordinal: offset,
            text: "Claude compaction boundary".to_owned(),
            kind: Some(super::KIND_COMPACT_BOUNDARY.to_owned()),
            model: None,
            tool_names: None,
            source_path: Some(source_path.to_string_lossy().into_owned()),
            source_offset: Some(offset),
            metadata_json: serde_json::to_string(&metadata).ok(),
        };
        return ClaudeRecordDisposition::Message {
            draft: Box::new(draft()),
            message: Box::new(message),
        };
    };
    if envelope.native_record_kind() != "user" && envelope.native_record_kind() != "assistant" {
        return ClaudeRecordDisposition::NonConversational;
    }
    let (text, _) = content_storage_text_and_tools(content, None);
    if text.trim().is_empty() {
        return ClaudeRecordDisposition::NonConversational;
    }
    let tool_names = envelope
        .facts()
        .iter()
        .filter_map(|fact| match fact {
            CanonicalObservationFactV1::ToolInvocation { name, .. } => Some(name.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    let mut metadata = canonical_message_metadata_from_facts(envelope, context, worktree_cache);
    metadata.insert(
        "source_generation".to_string(),
        Value::from(context.file_generation),
    );
    retain_unchanged_tool_event_ids(&mut metadata, context.raw_tool_event_ids);
    let message = SessionMessageRecord {
        provider: PROVIDER.to_owned(),
        message_id: envelope
            .relations()
            .message_id()
            .unwrap_or_else(|| envelope.stable_record_id())
            .as_str()
            .to_owned(),
        session_id: context.session_id.to_owned(),
        role: canonical_role(*role).to_owned(),
        timestamp: timestamp.or_else(|| envelope.evidence().native_timestamp()),
        ordinal: offset,
        text,
        kind: Some("message".to_owned()),
        model: model.clone(),
        tool_names: (!tool_names.is_empty()).then(|| tool_names.join(",")),
        source_path: Some(source_path.to_string_lossy().into_owned()),
        source_offset: Some(offset),
        metadata_json: serde_json::to_string(&metadata).ok(),
    };
    ClaudeRecordDisposition::Message {
        draft: Box::new(draft()),
        message: Box::new(message),
    }
}

fn map_canonical_marker_row(
    envelope: &CanonicalObservationEnvelopeV1,
    context: &ClaudeRecordContext<'_>,
    source_path: &Path,
    offset: i64,
    accumulator: &mut SessionAccumulator,
) -> Option<SessionMessageRecord> {
    for fact in envelope.facts() {
        match fact {
            CanonicalObservationFactV1::Git {
                evidence_kind: CanonicalGitEvidenceKindV1::PullRequest,
                content: Some(native),
                ..
            } => {
                return pr_link_row(
                    native,
                    context.session_id,
                    context.file_generation,
                    source_path,
                    offset,
                    accumulator,
                );
            }
            CanonicalObservationFactV1::Workflow {
                evidence_kind: CanonicalWorkflowEvidenceKindV1::ModelFallback,
                content: Some(native),
                ..
            } => {
                return model_fallback_row(
                    native,
                    context.session_id,
                    context.file_generation,
                    source_path,
                    offset,
                );
            }
            CanonicalObservationFactV1::Workflow {
                evidence_kind: CanonicalWorkflowEvidenceKindV1::Unknown,
                content: Some(native),
                ..
            } => {
                let trusted = context
                    .raw_hook_tool_use_id
                    .filter(|raw| native.get("toolUseID").and_then(Value::as_str) == Some(*raw));
                if let Some(message) =
                    system_hook_message_from_line(native, source_path, context, trusted)
                {
                    return Some(message);
                }
            }
            _ => {}
        }
    }
    None
}

fn canonical_message_metadata_from_facts(
    envelope: &CanonicalObservationEnvelopeV1,
    context: &ClaudeRecordContext<'_>,
    worktree_cache: Option<&ProjectRootMatcherCache>,
) -> Map<String, Value> {
    let mut metadata = Map::new();
    metadata.insert(
        "source".to_string(),
        Value::String("claude_transcript".to_string()),
    );
    metadata.insert(
        "raw_type".to_string(),
        Value::String(envelope.native_record_kind().to_owned()),
    );

    let location_cwd = envelope
        .facts()
        .iter()
        .find_map(|fact| match fact {
            CanonicalObservationFactV1::Session {
                location_path: Some(path),
                ..
            } => Some(PathBuf::from(path)),
            _ => None,
        })
        .or_else(|| context.session_cwd.map(Path::to_path_buf));
    let location_provenance = if envelope.facts().iter().any(|fact| {
        matches!(
            fact,
            CanonicalObservationFactV1::Session {
                location_path: Some(_),
                ..
            }
        )
    }) {
        "transcript_record"
    } else {
        "transcript_session"
    };
    append_claude_location_metadata(
        &mut metadata,
        CLAUDE_MESSAGE_LOCATION_KEYS,
        TranscriptLocation::new(location_cwd.as_deref(), location_provenance),
        worktree_cache,
    );

    let mut tool_events = Vec::new();
    for fact in envelope.facts() {
        match fact {
            CanonicalObservationFactV1::ToolInvocation {
                invocation_id,
                name,
                arguments,
            } => {
                let mut event = Map::new();
                event.insert("type".to_string(), Value::String("tool_use".to_string()));
                event.insert("tool_name".to_string(), Value::String(name.clone()));
                event.insert(
                    "call_id".to_string(),
                    Value::String(invocation_id.as_str().to_owned()),
                );
                event.insert(
                    "input_bytes".to_string(),
                    Value::from(arguments.to_string().len() as u64),
                );
                tool_events.push(Value::Object(event));
            }
            CanonicalObservationFactV1::ToolResult {
                invocation_id,
                content,
                ..
            } => {
                let mut event = Map::new();
                event.insert("type".to_string(), Value::String("tool_result".to_string()));
                if let Some(invocation_id) = invocation_id {
                    event.insert(
                        "call_id".to_string(),
                        Value::String(invocation_id.as_str().to_owned()),
                    );
                }
                event.insert(
                    "output_bytes".to_string(),
                    Value::from(content.to_string().len() as u64),
                );
                tool_events.push(Value::Object(event));
            }
            _ => {}
        }
    }
    if !tool_events.is_empty() {
        metadata.insert("tool_events".to_string(), Value::Array(tool_events));
    }

    if envelope.native_record_kind() == "assistant"
        && let Some(CanonicalObservationFactV1::Workflow {
            evidence_kind: CanonicalWorkflowEvidenceKindV1::Attribution,
            content: Some(Value::Object(attribution)),
            ..
        }) = envelope.facts().iter().find(|fact| {
            matches!(
                fact,
                CanonicalObservationFactV1::Workflow {
                    evidence_kind: CanonicalWorkflowEvidenceKindV1::Attribution,
                    ..
                }
            )
        })
    {
        for (key, value) in attribution {
            metadata.insert(key.clone(), value.clone());
        }
    }

    if envelope.native_record_kind() == "user" {
        for fact in envelope.facts() {
            match fact {
                CanonicalObservationFactV1::Git {
                    evidence_kind: CanonicalGitEvidenceKindV1::FileEdit,
                    content: Some(native),
                    ..
                } => {
                    append_edited_file_from_native(&mut metadata, native);
                }
                CanonicalObservationFactV1::Git {
                    evidence_kind: CanonicalGitEvidenceKindV1::Commit,
                    content: Some(native),
                    ..
                } => {
                    append_git_operation_metadata(&mut metadata, native);
                }
                _ => {}
            }
        }
    }

    if envelope
        .facts()
        .iter()
        .any(|fact| matches!(fact, CanonicalObservationFactV1::Compaction { .. }))
    {
        // Compact-summary rows keep the canonical envelope so LCM can read the
        // native flags and parent id without a second transcript pass.
        insert_canonical_envelope(
            &mut metadata,
            envelope,
            context.session_id,
            envelope.stable_record_id().as_str(),
        );
    }

    metadata
}

fn append_edited_file_from_native(metadata: &mut Map<String, Value>, native: &Value) {
    let Some(tool_use_result) = native
        .get("toolUseResult")
        .filter(|value| value.is_object())
    else {
        return;
    };
    let Some(file_path) = tool_use_result
        .get("filePath")
        .and_then(Value::as_str)
        .filter(|path| !path.is_empty())
    else {
        return;
    };
    let change_type = tool_use_result
        .get("type")
        .and_then(Value::as_str)
        .filter(|kind| !kind.is_empty())
        .unwrap_or("edit")
        .to_string();
    let hunks = tool_use_result
        .get("structuredPatch")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    let mut edited = Map::new();
    edited.insert("path".to_string(), Value::String(file_path.to_string()));
    edited.insert("change_type".to_string(), Value::String(change_type));
    edited.insert("hunks".to_string(), Value::from(hunks as i64));
    metadata.insert("edited_file".to_string(), Value::Object(edited));
}

/// Serializer failure text for the operator log and the row marker: single
/// line and bounded, so a long or multi-line message can neither break the
/// logfmt record nor bloat the persisted metadata.
fn sanitized_serializer_reason(error: &serde_json::Error) -> String {
    const MAX_REASON_CHARS: usize = 200;
    error
        .to_string()
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .take(MAX_REASON_CHARS)
        .collect()
}

/// Attaches the canonical envelope LCM's pairing recognition reads, or
/// records why it is absent.
///
/// Persisting `canonical_envelope: null` would fabricate evidence, but a
/// silently omitted key leaves the row byte-identical to one that never
/// carried pairing evidence — "no pairing evidence" and "never had any" must
/// stay distinguishable. So a serializer failure is both reported to the
/// operator log and marked on the row itself.
fn insert_canonical_envelope(
    metadata: &mut Map<String, Value>,
    envelope: &impl Serialize,
    session_id: &str,
    record_id: &str,
) {
    match serde_json::to_value(envelope) {
        Ok(envelope_value) => {
            metadata.insert("canonical_envelope".to_owned(), envelope_value);
        }
        Err(error) => {
            let reason = sanitized_serializer_reason(&error);
            log_daemon_event(
                "canonical_envelope_unavailable",
                &[
                    ("provider", PROVIDER.to_owned()),
                    ("session_id", session_id.to_owned()),
                    ("record_id", record_id.to_owned()),
                    ("reason", reason.clone()),
                ],
            );
            metadata.insert(
                "canonical_envelope_unavailable".to_owned(),
                Value::String(reason),
            );
        }
    }
}

fn canonical_role(role: CanonicalMessageRoleV1) -> &'static str {
    match role {
        CanonicalMessageRoleV1::User => "user",
        CanonicalMessageRoleV1::Assistant => "assistant",
        CanonicalMessageRoleV1::System => "system",
        CanonicalMessageRoleV1::Tool => "tool",
        CanonicalMessageRoleV1::Unknown => "unknown",
    }
}

#[cfg(test)]
mod canonical_envelope_omission_tests {
    use super::insert_canonical_envelope;
    use serde::{Serialize, Serializer, ser::Error as SerError};
    use serde_json::{Map, Value};

    struct RefusingEnvelope;

    impl Serialize for RefusingEnvelope {
        fn serialize<S: Serializer>(&self, _serializer: S) -> Result<S::Ok, S::Error> {
            Err(S::Error::custom("boundary\nfact\tcannot serialize"))
        }
    }

    #[test]
    fn unserializable_envelope_is_marked_instead_of_omitted() {
        let mut metadata = Map::new();

        insert_canonical_envelope(&mut metadata, &RefusingEnvelope, "session-1", "record-1");

        assert!(
            !metadata.contains_key("canonical_envelope"),
            "a failed serialization must never persist a fabricated envelope: {metadata:?}"
        );
        let reason = metadata["canonical_envelope_unavailable"]
            .as_str()
            .expect("the omission must carry a typed reason");
        assert!(
            reason.starts_with("boundary fact cannot serialize"),
            "the reason must carry the sanitized serializer text: {reason:?}"
        );
    }

    #[test]
    fn serializable_envelope_keeps_the_pairing_evidence() {
        let mut metadata = Map::new();

        insert_canonical_envelope(&mut metadata, &"pairing", "session-1", "record-1");

        assert_eq!(
            metadata["canonical_envelope"],
            Value::String("pairing".into())
        );
        assert!(!metadata.contains_key("canonical_envelope_unavailable"));
    }
}
