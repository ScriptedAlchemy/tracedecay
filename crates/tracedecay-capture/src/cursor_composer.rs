use std::collections::BTreeSet;

use serde_json::Value;
use sha2::{Digest, Sha256};
use tracedecay_domain::{
    CanonicalGitEvidenceKindV1, CanonicalMessageRoleV1, CanonicalObservationEnvelopeV1,
    CanonicalObservationEvidenceV1, CanonicalObservationFactV1, CanonicalObservationRelationsV1,
    CanonicalReasoningVisibilityV1, CanonicalUnknownStateV1, CanonicalWorkflowEvidenceKindV1,
    CanonicalWorkflowSemanticKindV1, ObservationId, ObservationOrderingDomainV1,
    ObservationSourceIdentityV1, ProviderId, ProviderUsageContractDimensionV1, SessionId,
};

use crate::ObservationRecordParseErrorV1;

const PROVIDER: &str = "cursor";

pub fn normalize_cursor_composer_observation(
    native: &Value,
    composer_id: &str,
    stable_record_id: ObservationId,
    range: tracedecay_domain::ObservationSourceRangeV1,
    position: u64,
) -> Result<CanonicalObservationEnvelopeV1, ObservationRecordParseErrorV1> {
    normalize_cursor_composer_observation_with_message_id(
        native,
        composer_id,
        stable_record_id.clone(),
        stable_record_id,
        range,
        position,
    )
}

pub fn normalize_cursor_composer_observation_with_projected_message_id(
    native: &Value,
    composer_id: &str,
    stable_record_id: ObservationId,
    projected_message_id: ObservationId,
    range: tracedecay_domain::ObservationSourceRangeV1,
    position: u64,
) -> Result<CanonicalObservationEnvelopeV1, ObservationRecordParseErrorV1> {
    normalize_cursor_composer_observation_with_message_id(
        native,
        composer_id,
        stable_record_id,
        projected_message_id,
        range,
        position,
    )
}

pub fn normalize_cursor_composer_observation_with_message_id(
    native: &Value,
    composer_id: &str,
    stable_record_id: ObservationId,
    projected_message_id: ObservationId,
    range: tracedecay_domain::ObservationSourceRangeV1,
    position: u64,
) -> Result<CanonicalObservationEnvelopeV1, ObservationRecordParseErrorV1> {
    let timestamp = bubble_epoch(native, "createdAt");
    let mut relations = CanonicalObservationRelationsV1::new(
        SessionId::new(composer_id)
            .map_err(|_| ObservationRecordParseErrorV1::NormalizationFailed)?,
    )
    .with_message_id(projected_message_id);
    // Cursor composerId is the native chat/thread identity for store.db / vscdb.
    if let Ok(thread_id) = ObservationId::new(composer_id) {
        relations = relations.with_thread_id(thread_id);
    }
    let mut facts = Vec::new();
    if let Some(project_path) = native
        .get("tracedecayProjectPath")
        .and_then(Value::as_str)
        .filter(|path| !path.trim().is_empty())
    {
        facts.push(CanonicalObservationFactV1::Session {
            project_path: Some(project_path.to_string()),
            location_path: Some(project_path.to_string()),
            transcript_path: native
                .get("tracedecayTranscriptPath")
                .and_then(Value::as_str)
                .map(str::to_string),
            title: native
                .get("tracedecaySessionTitle")
                .and_then(Value::as_str)
                .map(str::to_string),
            started_at: epoch_ms_to_secs(
                native
                    .get("tracedecaySessionStartedAt")
                    .and_then(Value::as_i64),
            ),
            ended_at: epoch_ms_to_secs(
                native
                    .get("tracedecaySessionEndedAt")
                    .and_then(Value::as_i64),
            ),
            source: Some("cursor_composer".to_string()),
            native_source: Some("cursor".to_string()),
            profile: None,
            location_provenance: Some("workspace_envelope".to_string()),
        });
    }

    if let Some(text) = native
        .get("text")
        .and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty())
    {
        facts.push(CanonicalObservationFactV1::Message {
            role: match native.get("type").and_then(Value::as_i64) {
                Some(1) => CanonicalMessageRoleV1::User,
                Some(2) => CanonicalMessageRoleV1::Assistant,
                _ => CanonicalMessageRoleV1::Unknown,
            },
            content: Value::String(text.to_string()),
            model: ["model", "modelId", "modelName", "tracedecaySessionModel"]
                .into_iter()
                .find_map(|key| native.get(key).and_then(Value::as_str))
                .filter(|model| !model.trim().is_empty())
                .map(str::to_string),
            timestamp,
        });
    }

    if let Some(tool) = native.get("toolFormerData").filter(|tool| !tool.is_null()) {
        let invocation_id = composer_observation_id(
            tool.get("toolCallId")
                .or_else(|| tool.get("id"))
                .and_then(Value::as_str),
            &stable_record_id,
        );
        let name = tool
            .get("name")
            .and_then(Value::as_str)
            .filter(|name| !name.trim().is_empty())
            .unwrap_or("tool")
            .to_string();
        facts.push(CanonicalObservationFactV1::ToolInvocation {
            invocation_id: invocation_id.clone(),
            name,
            arguments: Value::Null,
        });
        if tool.get("result").is_some_and(|result| !result.is_null()) {
            facts.push(CanonicalObservationFactV1::ToolResult {
                invocation_id: Some(invocation_id),
                content: Value::Null,
                success: tool
                    .get("status")
                    .and_then(Value::as_str)
                    .and_then(composer_tool_result_success),
            });
        }
    }

    if let Some(thinking) = native
        .pointer("/thinking/text")
        .filter(|thinking| !thinking.is_null())
        .cloned()
    {
        facts.push(CanonicalObservationFactV1::Reasoning {
            visibility: CanonicalReasoningVisibilityV1::Visible,
            content: Some(thinking),
        });
    }

    if let Some(token_count) = native.get("tokenCount") {
        let input_tokens = composer_canonical_u64(token_count.get("inputTokens"));
        let output_tokens = composer_canonical_u64(token_count.get("outputTokens"));
        let total_tokens = composer_canonical_u64(
            token_count
                .get("totalTokens")
                .or_else(|| token_count.get("total_tokens")),
        );
        if input_tokens.is_some() || output_tokens.is_some() || total_tokens.is_some() {
            facts.push(CanonicalObservationFactV1::UncorrelatedUsage {
                input_tokens,
                output_tokens,
                cache_read_tokens: None,
                cache_write_tokens: None,
                reasoning_tokens: None,
                total_tokens,
                native_kind: "bubble".to_string(),
                native_field: "tokenCount".to_string(),
                missing_dimensions: BTreeSet::from([
                    ProviderUsageContractDimensionV1::Model,
                    ProviderUsageContractDimensionV1::Scope,
                    ProviderUsageContractDimensionV1::CounterSemantics,
                ]),
            });
        }
    }

    append_composer_git_facts(native, &mut facts);
    append_composer_todo_lifecycle_facts(native, composer_id, &mut facts);
    if native
        .get("isCompacted")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        facts.push(CanonicalObservationFactV1::Compaction {
            summary: native.get("text").cloned(),
            input_tokens: None,
            output_tokens: None,
        });
    }
    if facts.is_empty() {
        facts.push(CanonicalObservationFactV1::Unknown {
            native_kind: "bubble".to_string(),
            state: CanonicalUnknownStateV1::Absent,
        });
    }

    let mut evidence =
        CanonicalObservationEvidenceV1::new(ObservationOrderingDomainV1::SnapshotOrder, range)
            .with_native_sequence(position);
    if let Some(timestamp) = timestamp {
        evidence = evidence.with_native_timestamp(timestamp);
    }
    CanonicalObservationEnvelopeV1::new(
        ProviderId::new(PROVIDER)
            .map_err(|_| ObservationRecordParseErrorV1::NormalizationFailed)?,
        "bubble",
        stable_record_id,
        relations,
        facts,
        evidence,
    )
    .map_err(|_| ObservationRecordParseErrorV1::NormalizationFailed)
}

fn append_composer_git_facts(native: &Value, facts: &mut Vec<CanonicalObservationFactV1>) {
    if let Some(commits) = native.get("commits").and_then(Value::as_array) {
        for commit in commits {
            let reference = ["hash", "sha", "id"]
                .into_iter()
                .find_map(|key| commit.get(key).and_then(Value::as_str))
                .map(str::to_string);
            facts.push(CanonicalObservationFactV1::Git {
                evidence_kind: CanonicalGitEvidenceKindV1::Commit,
                reference,
                content: None,
            });
        }
    }
    if native
        .get("gitDiffs")
        .and_then(Value::as_array)
        .is_some_and(|diffs| !diffs.is_empty())
    {
        facts.push(CanonicalObservationFactV1::Git {
            evidence_kind: CanonicalGitEvidenceKindV1::Diff,
            reference: None,
            content: None,
        });
    }
    if let Some(pull_requests) = native.get("pullRequests").and_then(Value::as_array) {
        for pull_request in pull_requests {
            let reference = ["url", "htmlUrl", "html_url", "id"]
                .into_iter()
                .find_map(|key| pull_request.get(key).and_then(Value::as_str))
                .map(str::to_string);
            facts.push(CanonicalObservationFactV1::Git {
                evidence_kind: CanonicalGitEvidenceKindV1::PullRequest,
                reference: reference.clone(),
                content: None,
            });
            facts.push(CanonicalObservationFactV1::Workflow {
                evidence_kind: CanonicalWorkflowEvidenceKindV1::PullRequest,
                reference,
                content: None,
            });
        }
    }
}

/// Map native Composer `todos[{id,content,status}]` into `WorkflowLifecycle`
/// `TodoList` + `TodoItem` facts. Only copies fields present on each native item;
/// never invents revision, list ids beyond `composerId`, or statuses.
fn append_composer_todo_lifecycle_facts(
    native: &Value,
    composer_id: &str,
    facts: &mut Vec<CanonicalObservationFactV1>,
) {
    let Some(todos) = native.get("todos").and_then(Value::as_array) else {
        return;
    };
    let mut items = Vec::new();
    for (index, todo) in todos.iter().enumerate() {
        let Some(content) = todo
            .get("content")
            .and_then(Value::as_str)
            .filter(|content| !content.trim().is_empty())
        else {
            continue;
        };
        let Some(item_id) = todo
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.trim().is_empty())
            .map(str::to_string)
        else {
            continue;
        };
        let status = todo
            .get("status")
            .and_then(Value::as_str)
            .filter(|status| !status.trim().is_empty())
            .map(str::to_string);
        items.push((index as u64, item_id, status, content.to_string()));
    }
    if items.is_empty() {
        return;
    }
    facts.push(CanonicalObservationFactV1::WorkflowLifecycle {
        semantic_kind: CanonicalWorkflowSemanticKindV1::TodoList,
        provider_reference: Some(composer_id.to_string()),
        item_id: None,
        parent_reference: None,
        list_reference: None,
        state: None,
        status: None,
        item_order: None,
        revision: None,
        event_sequence: None,
        content: None,
    });
    for (item_order, item_id, status, content) in items {
        facts.push(CanonicalObservationFactV1::WorkflowLifecycle {
            semantic_kind: CanonicalWorkflowSemanticKindV1::TodoItem,
            provider_reference: Some(item_id.clone()),
            item_id: Some(item_id),
            parent_reference: None,
            list_reference: Some(composer_id.to_string()),
            state: None,
            status,
            item_order: Some(item_order),
            revision: None,
            event_sequence: None,
            content: Some(Value::String(content)),
        });
    }
}

pub fn composer_todos_have_admittable_items(native: &Value) -> bool {
    native
        .get("todos")
        .and_then(Value::as_array)
        .is_some_and(|todos| {
            todos.iter().any(|todo| {
                todo.get("id")
                    .and_then(Value::as_str)
                    .is_some_and(|id| !id.trim().is_empty())
                    && todo
                        .get("content")
                        .and_then(Value::as_str)
                        .is_some_and(|content| !content.trim().is_empty())
            })
        })
}

pub fn normalize_cursor_composer_envelope_observation(
    native: &Value,
    composer_id: &str,
    project_path: Option<&str>,
    stable_record_id: ObservationId,
    range: tracedecay_domain::ObservationSourceRangeV1,
    position: u64,
) -> Result<CanonicalObservationEnvelopeV1, ObservationRecordParseErrorV1> {
    let timestamp = bubble_epoch(native, "createdAt");
    let mut relations = CanonicalObservationRelationsV1::new(
        SessionId::new(composer_id)
            .map_err(|_| ObservationRecordParseErrorV1::NormalizationFailed)?,
    );
    if let Ok(thread_id) = ObservationId::new(composer_id) {
        relations = relations.with_thread_id(thread_id);
    }
    let mut facts = Vec::new();
    append_composer_todo_lifecycle_facts(native, composer_id, &mut facts);
    if facts.is_empty() {
        return Err(ObservationRecordParseErrorV1::NormalizationFailed);
    }
    if let Some(project_path) = project_path {
        facts.insert(
            0,
            CanonicalObservationFactV1::Session {
                project_path: Some(project_path.to_string()),
                location_path: Some(project_path.to_string()),
                transcript_path: None,
                title: native
                    .get("name")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                started_at: epoch_ms_to_secs(native.get("createdAt").and_then(Value::as_i64)),
                ended_at: epoch_ms_to_secs(native.get("lastUpdatedAt").and_then(Value::as_i64)),
                source: Some("cursor_composer".to_string()),
                native_source: Some("cursor".to_string()),
                profile: None,
                location_provenance: Some("workspace_envelope".to_string()),
            },
        );
    }
    let mut evidence =
        CanonicalObservationEvidenceV1::new(ObservationOrderingDomainV1::SnapshotOrder, range)
            .with_native_sequence(position);
    if let Some(timestamp) = timestamp {
        evidence = evidence.with_native_timestamp(timestamp);
    }
    CanonicalObservationEnvelopeV1::new(
        ProviderId::new(PROVIDER)
            .map_err(|_| ObservationRecordParseErrorV1::NormalizationFailed)?,
        "envelope",
        stable_record_id,
        relations,
        facts,
        evidence,
    )
    .map_err(|_| ObservationRecordParseErrorV1::NormalizationFailed)
}

fn composer_observation_id(native_id: Option<&str>, fallback: &ObservationId) -> ObservationId {
    native_id
        .and_then(|native_id| ObservationId::new(native_id).ok())
        .unwrap_or_else(|| fallback.clone())
}

fn composer_canonical_u64(value: Option<&Value>) -> Option<u64> {
    value.and_then(|value| {
        value
            .as_u64()
            .or_else(|| value.as_i64().and_then(|value| u64::try_from(value).ok()))
    })
}

/// Exact allowlist for `toolFormerData.status` → `ToolResult.success`.
/// Unknown statuses stay `None` (not a concrete failure/`Some(false)`).
fn composer_tool_result_success(status: &str) -> Option<bool> {
    match status {
        "completed" | "success" | "succeeded" => Some(true),
        _ => None,
    }
}

pub fn composer_observation_with_session(
    bubble: &Value,
    project_path: Option<&str>,
    envelope: Option<&Value>,
) -> Value {
    let mut native = bubble.clone();
    if let Some(object) = native.as_object_mut() {
        if let Some(project_path) = project_path {
            object.insert(
                "tracedecayProjectPath".to_string(),
                Value::String(project_path.to_string()),
            );
        }
        if let Some(envelope) = envelope {
            for (key, value) in [
                ("tracedecaySessionTitle", envelope.get("name")),
                (
                    "tracedecaySessionModel",
                    envelope.pointer("/modelConfig/modelName"),
                ),
                ("tracedecaySessionStartedAt", envelope.get("createdAt")),
                ("tracedecaySessionEndedAt", envelope.get("lastUpdatedAt")),
            ] {
                if let Some(value) = value.filter(|value| !value.is_null()) {
                    object.insert(key.to_string(), value.clone());
                }
            }
        }
    }
    native
}

pub fn cursor_composer_native_record_id(
    composer_id: &str,
    bubble_id: &str,
) -> Result<ObservationId, String> {
    let mut hasher = Sha256::new();
    hasher.update(b"tracedecay.cursor-composer-native-record.v1\0");
    hasher.update(composer_id.as_bytes());
    hasher.update([0]);
    hasher.update(bubble_id.as_bytes());
    ObservationId::new(format!(
        "cursor.composer.sha256:{}",
        hex::encode(hasher.finalize())
    ))
    .map_err(|error| format!("invalid Cursor composer native identity: {error}"))
}

pub fn cursor_composer_envelope_source(
    composer_id: &str,
) -> Result<ObservationSourceIdentityV1, String> {
    let source_key = SessionId::new(format!("{composer_id}:composerData"))
        .map_err(|error| format!("invalid Cursor composer envelope source key: {error}"))?;
    ObservationSourceIdentityV1::for_provider_source(
        ProviderId::new(PROVIDER)
            .map_err(|error| format!("invalid Cursor provider id: {error}"))?,
        SessionId::new(composer_id)
            .map_err(|error| format!("invalid Cursor composer id: {error}"))?,
        source_key,
    )
    .map_err(|error| format!("invalid Cursor composer envelope source: {error}"))
}

pub fn composer_envelope_todo_checkpoint(native: &Value) -> Option<u64> {
    let todos = native.get("todos")?.as_array()?;
    let mut hasher = Sha256::new();
    hasher.update(b"tracedecay.cursor-composer-todo-checkpoint.v1\0");
    let mut any = false;
    for (index, todo) in todos.iter().enumerate() {
        let Some(item_id) = todo
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.trim().is_empty())
        else {
            continue;
        };
        let Some(content) = todo
            .get("content")
            .and_then(Value::as_str)
            .filter(|content| !content.trim().is_empty())
        else {
            continue;
        };
        any = true;
        hasher.update(u64::try_from(index).ok()?.to_le_bytes());
        hasher.update(u64::try_from(item_id.len()).ok()?.to_le_bytes());
        hasher.update(item_id.as_bytes());
        hasher.update(u64::try_from(content.len()).ok()?.to_le_bytes());
        hasher.update(content.as_bytes());
        if let Some(status) = todo
            .get("status")
            .and_then(Value::as_str)
            .filter(|status| !status.trim().is_empty())
        {
            hasher.update([1]);
            hasher.update(u64::try_from(status.len()).ok()?.to_le_bytes());
            hasher.update(status.as_bytes());
        } else {
            hasher.update([0]);
        }
    }
    if !any {
        return None;
    }
    let digest = hasher.finalize();
    let mut bytes = [0_u8; 8];
    bytes.copy_from_slice(&digest[..8]);
    Some(u64::from_le_bytes(bytes).max(1))
}

pub fn cursor_composer_envelope_native_record_id(
    composer_id: &str,
    checkpoint: u64,
) -> Result<ObservationId, String> {
    let mut hasher = Sha256::new();
    hasher.update(b"tracedecay.cursor-composer-envelope.v1\0");
    hasher.update(composer_id.as_bytes());
    hasher.update([0]);
    hasher.update(checkpoint.to_le_bytes());
    ObservationId::new(format!(
        "cursor.composer.envelope.sha256:{}",
        hex::encode(hasher.finalize())
    ))
    .map_err(|error| format!("invalid Cursor composer envelope native identity: {error}"))
}

fn bubble_epoch(bubble: &Value, key: &str) -> Option<i64> {
    epoch_ms_to_secs(bubble.get(key).and_then(Value::as_i64))
}

fn epoch_ms_to_secs(ms: Option<i64>) -> Option<i64> {
    ms.filter(|value| *value > 0).map(|value| value / 1_000)
}
