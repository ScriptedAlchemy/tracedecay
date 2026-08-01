//! Hermes observation normalization, coverage, and bounded-page tests.

use serde_json::{Value, json};
use tracedecay_domain::{
    CanonicalMessageRoleV1, CanonicalObservationEnvelopeV1, CanonicalObservationFactV1,
    CanonicalReasoningVisibilityV1, ObservationId, ObservationIdentityMaterialV1,
    ObservationOrderingDomainV1, ObservationScopeV1, ObservationSourceCursorV1,
    ObservationSourceGenerationV1, ObservationSourceRangeV1, RetentionClass, SessionId,
};
use tracedecay_store::observation::ObservationCoverageReason;

use crate::admission::test_support::PanicHostAdmission;
use crate::observation::ObservationCancellation;
use crate::runtime::shared::StoredCursor;
use tracedecay_runtime_core::privacy::{
    MAX_OBSERVATION_RECORD_BYTES, parse_normalized_observation_record_v1,
};

use super::coverage::admit_rows_with_admission_and_cancellation;
use super::*;

static HERMES_UNIT_FIXTURE_OWNED_STORE_READY: tokio::sync::OnceCell<()> =
    tokio::sync::OnceCell::const_new();

async fn initialize_owned_store_before_foreign_fixture(directory: &std::path::Path) {
    HERMES_UNIT_FIXTURE_OWNED_STORE_READY
        .get_or_init(|| async {
            // Match production startup order: initialize the owned store
            // before any foreign rusqlite fixture initializes SQLite.
            let _connection = tracedecay_runtime_core::db::engine::TestConnection::open(
                &directory.join(".owned-store-initialization.db"),
            );
        })
        .await;
}

#[cfg(windows)]
#[test]
fn windows_sqlite_incarnation_keeps_identity_and_refreshes_resume_fingerprint() {
    use std::io::Write as _;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.db");
    std::fs::write(&path, b"before").unwrap();
    let (before_generation, before_identity, before_resume) =
        sqlite_incarnation(&path).expect("initial Windows SQLite identity");
    std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(b"after")
        .unwrap();

    let (after_generation, after_identity, after_resume) =
        sqlite_incarnation(&path).expect("updated Windows SQLite identity");
    assert_eq!(after_generation, before_generation);
    assert_eq!(after_identity, before_identity);
    assert_ne!(after_resume, before_resume);
}

fn fixture(row_id: i64) -> HermesRow {
    HermesRow {
        id: row_id,
        session_id: "session-redacted".to_string(),
        role: "assistant".to_string(),
        content: Some("safe fixture content".to_string()),
        reasoning: Some("safe redacted reasoning".to_string()),
        tool_name: Some("terminal".to_string()),
        tool_calls: Some(
            json!([{
                "id": "tool-redacted",
                "function": {"name": "terminal", "arguments": "{}"}
            }])
            .to_string(),
        ),
        timestamp: Some(1_750_000_000.0),
        session_model: Some("model-redacted".to_string()),
        parent_session_id: Some("parent-redacted".to_string()),
        session_cwd: None,
        session_source: Some("tui".to_string()),
        session_title: Some("Safe fixture".to_string()),
        session_started_at: Some(1_750_000_000.0),
        session_ended_at: Some(1_750_000_001.0),
        session_input_tokens: Some(10),
        session_output_tokens: Some(5),
        session_cache_read_tokens: Some(4),
        session_cache_write_tokens: Some(3),
        session_reasoning_tokens: Some(2),
        active: 1,
        sql_value_oversized: false,
        sql_measured_bytes: 0,
    }
}

fn normalized(row: &HermesRow, start: u64) -> HermesObservationRecord {
    let source = observation_source(row).unwrap();
    let range = ObservationSourceRangeV1::new(start, row.id as u64).unwrap();
    native_observation_record(row, &fixture_projection(), source, range).unwrap()
}

fn fixture_projection() -> HermesProjectionMetadata {
    HermesProjectionMetadata {
        project_path: None,
        location_path: None,
        profile: Some("fixture".to_string()),
        location_provenance: None,
    }
}

fn canonical(row: &HermesRow, start: u64) -> CanonicalObservationEnvelopeV1 {
    let record = normalized(row, start);
    let encoded = serde_json::to_vec(&record.native).unwrap();
    let parsed = parse_normalized_observation_record_v1(
        &encoded,
        record.range,
        ObservationOrderingDomainV1::SqliteRowId,
        |native| normalize_native_observation(native, record.range),
    )
    .unwrap();
    serde_json::from_value(parsed.value().clone()).unwrap()
}

#[tokio::test]
async fn cancelled_hermes_admission_stops_before_the_next_transactional_row() {
    let project_id = tracedecay_domain::ProjectId::new("project.hermes-cancelled-startup").unwrap();
    let cancellation = ObservationCancellation::default();
    cancellation.cancel();

    let stats = admit_rows_with_admission_and_cancellation(
        &PanicHostAdmission,
        &[fixture(1)],
        ObservationScopeV1::Project { project_id },
        ObservationSourceGenerationV1::new(1).unwrap(),
        1,
        1,
        |_| panic!("cancelled Hermes admission must not route a row"),
        &cancellation,
    )
    .await
    .unwrap();

    assert_eq!(
        stats,
        crate::runtime::shared::TranscriptIngestStats::default()
    );
}

#[test]
fn native_identity_does_not_depend_on_sqlite_row_id() {
    let first = normalized(&fixture(7), 0);
    let relocated = normalized(&fixture(700), 0);
    assert_eq!(first.native_record_id, relocated.native_record_id);
    assert_ne!(first.range, relocated.range);
}

#[test]
fn native_identity_does_not_depend_on_routing_path() {
    let mut first_row = fixture(7);
    first_row.session_cwd = Some("/redacted/first".to_string());
    let mut relocated_row = fixture(7);
    relocated_row.session_cwd = Some("/redacted/second".to_string());
    let first = normalized(&first_row, 0);
    let relocated = normalized(&relocated_row, 0);
    assert_eq!(first.native_record_id, relocated.native_record_id);
    assert_eq!(first.native, relocated.native);
}

#[test]
fn normalized_payload_contains_only_typed_canonical_facts() {
    let envelope = canonical(&fixture(7), 0);
    assert_eq!(envelope.provider().as_str(), PROVIDER);
    assert_eq!(envelope.native_record_kind(), "message");
    assert_eq!(
        envelope.relations().session_id().as_str(),
        "session-redacted"
    );
    assert_eq!(
        envelope.relations().message_id().map(ObservationId::as_str),
        Some("session-redacted:7")
    );
    assert!(envelope.relations().agent_id().is_some());
    assert!(envelope.relations().parent_agent_id().is_some());
    assert_eq!(
        envelope
            .relations()
            .parent_session_id()
            .map(SessionId::as_str),
        Some("parent-redacted")
    );
    let relations = serde_json::to_value(envelope.relations()).unwrap();
    // Hermes has no native thread/turn identifiers; leave those unset.
    assert!(relations.get("thread_id").is_none());
    assert!(relations.get("turn_id").is_none());
    assert_eq!(
        envelope.evidence().ordering_domain(),
        ObservationOrderingDomainV1::SqliteRowId
    );
    assert_eq!(
        envelope.evidence().range(),
        ObservationSourceRangeV1::new(0, 7).unwrap()
    );
    assert!(envelope.facts().iter().any(|fact| matches!(
        fact,
        CanonicalObservationFactV1::Session {
            project_path: None,
            location_path: None,
            transcript_path: None,
            title: Some(title),
            started_at: Some(1_750_000_000),
            ended_at: Some(1_750_000_001),
            source: Some(source),
            native_source: Some(native_source),
            profile: Some(profile),
            location_provenance: None,
        } if title == "Safe fixture"
            && source == "hermes_state_db"
            && native_source == "tui"
            && profile == "fixture"
    )));
    assert!(envelope.facts().iter().any(|fact| matches!(
        fact,
        CanonicalObservationFactV1::Message {
            role: CanonicalMessageRoleV1::Assistant,
            content: Value::String(content),
            model: Some(model),
            timestamp: Some(1_750_000_000),
        } if content == "safe fixture content" && model == "model-redacted"
    )));
    assert!(envelope.facts().iter().any(|fact| matches!(
        fact,
        CanonicalObservationFactV1::ToolInvocation {
            name,
            arguments,
            ..
        } if name == "terminal" && arguments == &json!({})
    )));
    assert!(envelope.facts().iter().any(|fact| matches!(
        fact,
        CanonicalObservationFactV1::Usage {
            input_tokens: Some(10),
            output_tokens: Some(5),
            cache_read_tokens: Some(4),
            cache_write_tokens: Some(3),
            reasoning_tokens: Some(2),
        }
    )));
    assert!(envelope.facts().iter().any(|fact| matches!(
        fact,
        CanonicalObservationFactV1::Reasoning {
            visibility: CanonicalReasoningVisibilityV1::Visible,
            content: Some(Value::String(content)),
        } if content == "safe redacted reasoning"
    )));

    let canonical = serde_json::to_value(&envelope).unwrap();
    for forbidden in ["hermes", "routing", "cwd", "provenance", "metadata"] {
        assert!(canonical.get(forbidden).is_none());
    }
}

#[test]
fn sanitizer_preserves_non_sensitive_v1_message_identity() {
    let mut row = fixture(7);
    row.session_id = "20260101_000000_abc123".to_string();
    let record = normalized(&row, 0);
    let encoded = serde_json::to_vec(&record.native).unwrap();
    let parsed = parse_normalized_observation_record_v1(
        &encoded,
        record.range,
        ObservationOrderingDomainV1::SqliteRowId,
        |native| normalize_native_observation(native, record.range),
    )
    .unwrap();
    let identity = ObservationIdentityMaterialV1::for_native_record(
        record.source,
        ObservationScopeV1::Profile,
        ObservationSourceGenerationV1::new(1).unwrap(),
        record.range,
        ObservationOrderingDomainV1::SqliteRowId,
        record.native_record_id,
    )
    .unwrap();
    let outcome = tracedecay_runtime_core::privacy::ClaudeRecordSanitizerV1::observation_v1()
        .unwrap()
        .sanitize_parsed(
            parsed,
            identity,
            RetentionClass::new(OBSERVATION_RETENTION).unwrap(),
        )
        .unwrap();
    let tracedecay_runtime_core::privacy::ObservationSanitizationOutcomeV1::Durable {
        observation,
        ..
    } = outcome
    else {
        panic!("safe Hermes fixture must remain durable");
    };
    let envelope: CanonicalObservationEnvelopeV1 =
        serde_json::from_value(observation.payload().clone()).unwrap();
    assert_eq!(
        envelope.relations().message_id().map(ObservationId::as_str),
        Some("20260101_000000_abc123:7")
    );
}

#[test]
fn canonical_identity_and_parent_relation_match_native_evidence() {
    let row = fixture(7);
    let record = normalized(&row, 0);
    let expected_record_id = record.native_record_id.clone();
    let expected_parent = stable_native_id("hermes.session", &json!("parent-redacted")).unwrap();
    let envelope = normalize_native_observation(record.native, record.range).unwrap();
    assert_eq!(envelope.stable_record_id(), &expected_record_id);
    assert_eq!(
        envelope.relations().parent_agent_id(),
        Some(&expected_parent)
    );
    let relations = serde_json::to_value(envelope.relations()).unwrap();
    assert!(relations.get("thread_id").is_none());
    assert!(relations.get("turn_id").is_none());
}

#[test]
fn assistant_without_reasoning_is_typed_unavailable() {
    let mut row = fixture(7);
    row.reasoning = None;
    let envelope = canonical(&row, 0);
    assert!(envelope.facts().iter().any(|fact| matches!(
        fact,
        CanonicalObservationFactV1::Reasoning {
            visibility: CanonicalReasoningVisibilityV1::Unavailable,
            content: None,
        }
    )));
}

#[test]
fn empty_assistant_content_keeps_typed_tool_or_reasoning_without_message() {
    let mut tool_row = fixture(7);
    tool_row.content = Some(String::new());
    tool_row.reasoning = None;
    let tool = canonical(&tool_row, 0);
    assert!(
        tool.facts()
            .iter()
            .all(|fact| { !matches!(fact, CanonicalObservationFactV1::Message { .. }) })
    );
    assert!(tool.facts().iter().any(|fact| matches!(
        fact,
        CanonicalObservationFactV1::ToolInvocation {
            name,
            ..
        } if name == "terminal"
    )));

    let mut reasoning_row = fixture(8);
    reasoning_row.content = Some(String::new());
    reasoning_row.tool_calls = None;
    let reasoning = canonical(&reasoning_row, 0);
    assert!(
        reasoning
            .facts()
            .iter()
            .all(|fact| { !matches!(fact, CanonicalObservationFactV1::Message { .. }) })
    );
    assert!(reasoning.facts().iter().any(|fact| matches!(
        fact,
        CanonicalObservationFactV1::Reasoning {
            visibility: CanonicalReasoningVisibilityV1::Visible,
            content: Some(Value::String(content)),
        } if content == "safe redacted reasoning"
    )));
}

#[test]
fn tool_message_preserves_authored_message_and_typed_result() {
    let mut row = fixture(7);
    row.role = "tool".to_string();
    row.reasoning = None;
    row.tool_calls = None;
    let envelope = canonical(&row, 0);
    assert!(envelope.facts().iter().any(|fact| matches!(
        fact,
        CanonicalObservationFactV1::ToolResult {
            invocation_id: None,
            content: Value::String(content),
            success: None,
        } if content == "safe fixture content"
    )));
    assert!(envelope.facts().iter().any(|fact| matches!(
        fact,
        CanonicalObservationFactV1::Message {
            role: CanonicalMessageRoleV1::Tool,
            content: Value::String(content),
            model: Some(model),
            timestamp: Some(1_750_000_000),
        } if content == "safe fixture content" && model == "model-redacted"
    )));
    assert!(envelope.facts().iter().any(|fact| matches!(
        fact,
        CanonicalObservationFactV1::Reasoning {
            visibility: CanonicalReasoningVisibilityV1::NotApplicable,
            content: None,
        }
    )));
}

#[test]
fn same_generation_resumes_sqlite_ordering() {
    let row = fixture(42);
    let source = observation_source(&row).unwrap();
    let generation = ObservationSourceGenerationV1::new(17).unwrap();
    let expected = ObservationSourceCursorV1::for_ordering(
        source,
        ObservationScopeV1::Profile,
        generation,
        ObservationOrderingDomainV1::SqliteRowId,
        20,
    )
    .unwrap();
    let admission = prepare_observation_row(
        &row,
        Some(&fixture_projection()),
        &ObservationScopeV1::Profile,
        generation,
        Some(expected),
        23,
        29,
    )
    .unwrap();
    assert_eq!(admission.range.start(), 20);
    assert_eq!(admission.range.end(), 42);
    assert!(matches!(
        admission.action,
        HermesAdmissionAction::Capture(_)
    ));
}

#[test]
fn replacement_generation_restarts_sqlite_ordering() {
    let row = fixture(42);
    let source = observation_source(&row).unwrap();
    let old_generation = ObservationSourceGenerationV1::new(17).unwrap();
    let expected = ObservationSourceCursorV1::for_ordering(
        source,
        ObservationScopeV1::Profile,
        old_generation,
        ObservationOrderingDomainV1::SqliteRowId,
        900,
    )
    .unwrap();
    let admission = prepare_observation_row(
        &row,
        Some(&fixture_projection()),
        &ObservationScopeV1::Profile,
        ObservationSourceGenerationV1::new(18).unwrap(),
        Some(expected),
        23,
        29,
    )
    .unwrap();
    assert_eq!(admission.range.start(), 0);
    assert_eq!(admission.range.end(), 42);
    assert!(matches!(
        admission.action,
        HermesAdmissionAction::Capture(_)
    ));
}

#[test]
fn malformed_tool_calls_are_complete_typed_coverage() {
    let mut row = fixture(7);
    row.tool_calls = Some("{not-json".to_string());
    let admission = prepare_observation_row(
        &row,
        Some(&fixture_projection()),
        &ObservationScopeV1::Profile,
        ObservationSourceGenerationV1::new(17).unwrap(),
        None,
        23,
        29,
    )
    .unwrap();
    assert!(matches!(
        admission.action,
        HermesAdmissionAction::Cover(ObservationCoverageReason::MalformedFrame)
    ));
}

#[test]
fn missing_route_is_complete_out_of_scope_coverage() {
    let admission = prepare_observation_row(
        &fixture(7),
        None,
        &ObservationScopeV1::Profile,
        ObservationSourceGenerationV1::new(17).unwrap(),
        None,
        23,
        29,
    )
    .unwrap();
    assert!(matches!(
        admission.action,
        HermesAdmissionAction::Cover(ObservationCoverageReason::OutOfScope)
    ));
}

#[test]
fn oversized_record_is_complete_typed_coverage() {
    let mut row = fixture(7);
    row.content = Some("x".repeat(MAX_OBSERVATION_RECORD_BYTES));
    let admission = prepare_observation_row(
        &row,
        Some(&fixture_projection()),
        &ObservationScopeV1::Profile,
        ObservationSourceGenerationV1::new(17).unwrap(),
        None,
        23,
        29,
    )
    .unwrap();
    assert!(matches!(
        admission.action,
        HermesAdmissionAction::Cover(ObservationCoverageReason::OversizedFrame)
    ));
}

#[test]
fn sql_preflight_oversized_is_typed_coverage_without_payload() {
    let mut row = fixture(7);
    row.content = None;
    row.sql_value_oversized = true;
    row.sql_measured_bytes = (MAX_HERMES_VALUE_BYTES as u64).saturating_add(1);
    let admission = prepare_observation_row(
        &row,
        Some(&fixture_projection()),
        &ObservationScopeV1::Profile,
        ObservationSourceGenerationV1::new(17).unwrap(),
        None,
        23,
        29,
    )
    .unwrap();
    assert!(matches!(
        admission.action,
        HermesAdmissionAction::Cover(ObservationCoverageReason::OversizedFrame)
    ));
}

#[test]
fn excessive_structure_is_complete_malformed_coverage() {
    let mut nested = Value::String("redacted".to_string());
    for _ in 0..128 {
        nested = json!({ "nested": nested });
    }
    let mut row = fixture(7);
    row.tool_calls = Some(nested.to_string());
    let admission = prepare_observation_row(
        &row,
        Some(&fixture_projection()),
        &ObservationScopeV1::Profile,
        ObservationSourceGenerationV1::new(17).unwrap(),
        None,
        23,
        29,
    )
    .unwrap();
    assert!(matches!(
        admission.action,
        HermesAdmissionAction::Cover(ObservationCoverageReason::MalformedFrame)
    ));
}

#[test]
fn fixture_backed_hermes_tool_call_reaches_canonical_envelope() {
    // Exact assistant tool-call shape from
    // tests/transcript_ingest_suite/hermes.rs::write_hermes_profile.
    // Provider-parser path: native_observation_record → normalize_native_observation.
    let input: Value = serde_json::from_str(include_str!(
        "../../../../../tests/fixtures/provider_normalization/hermes/assistant_tool_call.input.json"
    ))
    .expect("Hermes golden input");
    let expected: Value = serde_json::from_str(include_str!(
        "../../../../../tests/fixtures/provider_normalization/hermes/assistant_tool_call.expected_envelope.json"
    ))
    .expect("Hermes golden expected envelope");
    let tool_calls = input["tool_calls"].clone();
    let mut row = fixture(input["row_id"].as_i64().unwrap());
    row.session_id = input["session_id"].as_str().unwrap().to_string();
    row.role = input["role"].as_str().unwrap().to_string();
    row.content = input["content"].as_str().map(str::to_string);
    row.reasoning = None;
    row.tool_name = None;
    row.tool_calls = Some(tool_calls.to_string());
    row.timestamp = input["timestamp"].as_f64();
    row.session_model = input["session_model"].as_str().map(str::to_string);
    row.parent_session_id = None;
    row.session_input_tokens = input["session_input_tokens"].as_i64();
    row.session_output_tokens = input["session_output_tokens"].as_i64();
    row.session_cache_read_tokens = input["session_cache_read_tokens"].as_i64();
    row.session_cache_write_tokens = input["session_cache_write_tokens"].as_i64();
    row.session_reasoning_tokens = input["session_reasoning_tokens"].as_i64();

    let record = normalized(&row, 0);
    let native: Value = serde_json::from_value(record.native.clone()).unwrap();
    assert_eq!(native["role"], "assistant");
    assert_eq!(native["tool_calls"], tool_calls);
    assert!(native.get("cwd").is_none());
    assert!(native.get("routing").is_none());

    let envelope = canonical(&row, 0);
    let actual = serde_json::to_value(&envelope).unwrap();
    assert_eq!(actual["version"], expected["version"]);
    assert_eq!(actual["provider"], expected["provider"]);
    assert_eq!(actual["native_record_kind"], expected["native_record_kind"]);
    assert_eq!(actual["evidence"], expected["evidence"]);
    let fact_kinds = actual["facts"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|fact| fact["kind"] != "session")
        .map(|fact| fact["kind"].as_str().unwrap())
        .collect::<Vec<_>>();
    let expected_fact_kinds = expected["fact_kinds"]
        .as_array()
        .unwrap()
        .iter()
        .map(|kind| kind.as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(fact_kinds, expected_fact_kinds);
    let relations = actual["relations"].as_object().unwrap();
    assert_eq!(relations["session_id"], expected["relations"]["session_id"]);
    assert_eq!(
        relations.get("agent_id").is_some(),
        expected["relations"]["agent_id_present"].as_bool().unwrap()
    );
    for absent in expected["relations"]["absent"].as_array().unwrap() {
        assert!(relations.get(absent.as_str().unwrap()).is_none());
    }
    assert!(envelope.facts().iter().any(|fact| matches!(
        fact,
        CanonicalObservationFactV1::ToolInvocation {
            name,
            arguments,
            ..
        } if name == "terminal"
            && arguments.get("command").and_then(Value::as_str)
                == Some("cargo test billing")
    )));
    assert!(
        envelope
            .facts()
            .iter()
            .all(|fact| !matches!(fact, CanonicalObservationFactV1::Message { .. })),
        "empty-content Hermes tool-call turn must not synthesize a Message fact"
    );
    let encoded = actual.to_string();
    for required in expected["encoded_must_contain"].as_array().unwrap() {
        assert!(encoded.contains(required.as_str().unwrap()));
    }
    for rejected in expected["encoded_must_not_contain"].as_array().unwrap() {
        assert!(!encoded.contains(rejected.as_str().unwrap()));
    }
    assert!(
        envelope
            .facts()
            .iter()
            .all(|fact| { !matches!(fact, CanonicalObservationFactV1::WorkflowLifecycle { .. }) }),
        "Hermes checked-in fixture must not emit WorkflowLifecycle"
    );
}

#[test]
fn hermes_workflow_lookalike_fields_do_not_emit_workflow_lifecycle() {
    let input: Value = serde_json::from_str(include_str!(
        "../../../../../tests/fixtures/provider_normalization/hermes/workflow_lookalike.input.json"
    ))
    .expect("Hermes workflow lookalike input");
    let mut row = fixture(input["row_id"].as_i64().unwrap());
    row.session_id = input["session_id"].as_str().unwrap().to_string();
    row.role = input["role"].as_str().unwrap().to_string();
    row.content = input["content"].as_str().map(str::to_string);
    row.reasoning = None;
    row.tool_name = None;
    row.tool_calls = Some(input["tool_calls"].to_string());
    row.timestamp = input["timestamp"].as_f64();
    row.session_model = input["session_model"].as_str().map(str::to_string);
    row.parent_session_id = None;
    row.session_input_tokens = input["session_input_tokens"].as_i64();
    row.session_output_tokens = input["session_output_tokens"].as_i64();
    row.session_cache_read_tokens = input["session_cache_read_tokens"].as_i64();
    row.session_cache_write_tokens = input["session_cache_write_tokens"].as_i64();
    row.session_reasoning_tokens = input["session_reasoning_tokens"].as_i64();

    let record = normalized(&row, 0);
    let mut native = record.native.clone();
    if let Some(object) = native.as_object_mut() {
        for key in ["workflow", "todos", "thread_goal_updated"] {
            if let Some(value) = input.get(key) {
                object.insert(key.to_string(), value.clone());
            }
        }
    }
    let envelope = normalize_native_observation(native, record.range)
        .expect("Hermes must ignore unknown workflow lookalike bags");
    assert!(
        envelope
            .facts()
            .iter()
            .all(|fact| { !matches!(fact, CanonicalObservationFactV1::WorkflowLifecycle { .. }) }),
        "Hermes workflow lookalikes must not become WorkflowLifecycle"
    );
    let encoded = serde_json::to_string(&envelope).unwrap();
    for rejected in [
        "hermes-hostile-task",
        "todo-hostile-1",
        "invented todo",
        "invented goal",
    ] {
        assert!(
            !encoded.contains(rejected),
            "{rejected} must not survive Hermes normalization"
        );
    }
}

/// Hostile `zeroblob` content is rejected by SQL length/typeof before any
/// Rust String/Vec materialization of the payload. Mirrors production by
/// writing then reopening read-only.
#[tokio::test]
async fn zeroblob_content_is_covered_without_materializing_payload() {
    let dir = tempfile::tempdir().unwrap();
    initialize_owned_store_before_foreign_fixture(dir.path()).await;
    let path = dir.path().join("state.db");
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(
            "PRAGMA journal_mode = DELETE;
             CREATE TABLE sessions (
                    id TEXT PRIMARY KEY,
                    model TEXT,
                    parent_session_id TEXT,
                    cwd TEXT,
                    input_tokens INTEGER,
                    output_tokens INTEGER,
                    cache_read_tokens INTEGER,
                    cache_write_tokens INTEGER,
                    reasoning_tokens INTEGER
                );
             CREATE TABLE messages (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    session_id TEXT NOT NULL,
                    role TEXT NOT NULL,
                    content TEXT,
                    tool_name TEXT,
                    tool_calls TEXT,
                    timestamp REAL NOT NULL,
                    reasoning TEXT,
                    active INTEGER NOT NULL DEFAULT 1
                );",
        )
        .unwrap();
        // Generate the hostile value inside SQLite — never as a Rust String/Vec.
        let hostile_bytes = MAX_HERMES_VALUE_BYTES.saturating_add(1);
        conn.execute(
            &format!(
                "INSERT INTO sessions (id, model, input_tokens)
                     VALUES ('sess-zeroblob', 'model', zeroblob({hostile_bytes}))"
            ),
            (),
        )
        .unwrap();
        conn.execute(
            &format!(
                "INSERT INTO messages (session_id, role, content, timestamp)
                     VALUES ('sess-zeroblob', 'user', zeroblob({hostile_bytes}), 1.0)"
            ),
            (),
        )
        .unwrap();
        conn.execute(
            "INSERT INTO messages (session_id, role, content, timestamp)
                 VALUES ('sess-zeroblob', 'assistant', 'safe trailing row', 2.0)",
            (),
        )
        .unwrap();
    }

    let conn = open_read_only_strict(&path).await.unwrap();
    let message_cols = message_columns(&conn).await.unwrap();
    let session_cols = table_columns(&conn, "sessions").await.unwrap();
    let select_sql = select_new_messages_sql(&message_cols, &session_cols);
    let page = read_new_rows_strict(&conn, &select_sql, StoredCursor::default())
        .await
        .unwrap();
    assert_eq!(page.items.len(), 2);
    assert!(page.items[0].sql_value_oversized);
    assert!(page.items[0].content.is_none());
    assert!(
        page.items[0].session_input_tokens.is_none(),
        "dynamic BLOB in INTEGER column must be nulled in SQL"
    );
    assert!(
        page.items[0].sql_measured_bytes > MAX_HERMES_VALUE_BYTES as u64,
        "SQL length charge must reflect hostile size without materializing it"
    );
    assert!(!page.items[1].sql_value_oversized);
    assert_eq!(page.items[1].content.as_deref(), Some("safe trailing row"));

    let admission = prepare_observation_row(
        &page.items[0],
        Some(&fixture_projection()),
        &ObservationScopeV1::Profile,
        ObservationSourceGenerationV1::new(17).unwrap(),
        None,
        23,
        29,
    )
    .unwrap();
    assert!(matches!(
        admission.action,
        HermesAdmissionAction::Cover(ObservationCoverageReason::OversizedFrame)
    ));
}

#[tokio::test]
async fn page_byte_budget_stops_collection_before_unbounded_growth() {
    let dir = tempfile::tempdir().unwrap();
    initialize_owned_store_before_foreign_fixture(dir.path()).await;
    let path = dir.path().join("state.db");
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch(
        "PRAGMA journal_mode = DELETE;
         CREATE TABLE sessions (
                id TEXT PRIMARY KEY,
                model TEXT,
                parent_session_id TEXT,
                cwd TEXT,
                input_tokens INTEGER,
                output_tokens INTEGER,
                cache_read_tokens INTEGER,
                cache_write_tokens INTEGER,
                reasoning_tokens INTEGER
            );
         CREATE TABLE messages (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT NOT NULL,
                role TEXT NOT NULL,
                content TEXT,
                tool_name TEXT,
                tool_calls TEXT,
                timestamp REAL NOT NULL,
                reasoning TEXT,
                active INTEGER NOT NULL DEFAULT 1
            );",
    )
    .unwrap();
    conn.execute(
        "INSERT INTO sessions (id, model) VALUES ('sess-page', 'model')",
        (),
    )
    .unwrap();
    // Build three max-sized TEXT payloads inside SQLite. The product path
    // must gate the second row against the remaining page bytes before
    // rusqlite materializes it as a Rust String.
    let sqlite_blob_bytes = MAX_HERMES_VALUE_BYTES / 2;
    for index in 0..3 {
        conn.execute(
            &format!(
                "INSERT INTO messages (session_id, role, content, timestamp)
                     SELECT 'sess-page', 'user', hex(zeroblob({sqlite_blob_bytes})), ?1"
            ),
            rusqlite::params![f64::from(index)],
        )
        .unwrap();
    }
    drop(conn);

    let conn = open_read_only_strict(&path).await.unwrap();
    let message_cols = message_columns(&conn).await.unwrap();
    let session_cols = table_columns(&conn, "sessions").await.unwrap();
    let select_sql = select_new_messages_sql(&message_cols, &session_cols);
    let page = read_new_rows_strict(&conn, &select_sql, StoredCursor::default())
        .await
        .unwrap();
    assert_eq!(
        page.items.len(),
        1,
        "the second max-sized row must be deferred before String materialization"
    );
    assert!(page.truncated_by_byte_budget);
    assert!(page.items.iter().all(|row| {
        row.content
            .as_ref()
            .is_none_or(|c| c.len() <= MAX_HERMES_VALUE_BYTES)
    }));

    let next = read_new_rows_strict(&conn, &select_sql, page.new_cursor)
        .await
        .unwrap();
    assert_eq!(next.items.len(), 1, "deferred row must resume next page");
}

#[tokio::test]
async fn utf8_byte_gate_rejects_multibyte_text_before_materialization() {
    let dir = tempfile::tempdir().unwrap();
    initialize_owned_store_before_foreign_fixture(dir.path()).await;
    let path = dir.path().join("state.db");
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch(
        "PRAGMA journal_mode = DELETE;
         CREATE TABLE sessions (
                id TEXT PRIMARY KEY, model TEXT, parent_session_id TEXT, cwd TEXT,
                input_tokens INTEGER, output_tokens INTEGER, cache_read_tokens INTEGER,
                cache_write_tokens INTEGER, reasoning_tokens INTEGER
            );
         CREATE TABLE messages (
                id INTEGER PRIMARY KEY AUTOINCREMENT, session_id TEXT NOT NULL,
                role TEXT NOT NULL, content TEXT, tool_name TEXT, tool_calls TEXT,
                timestamp REAL NOT NULL, reasoning TEXT, active INTEGER NOT NULL DEFAULT 1
            );",
    )
    .unwrap();
    conn.execute(
        "INSERT INTO sessions (id, model) VALUES ('sess-utf8', 'model')",
        (),
    )
    .unwrap();
    // 600,000 `é` code points are 1,200,000 UTF-8 bytes. SQLite
    // length(TEXT) would undercount this below the 1 MiB byte ceiling.
    conn.execute(
        "INSERT INTO messages (session_id, role, content, timestamp)
             SELECT 'sess-utf8', 'user',
                    replace(hex(zeroblob(600000)), '00', 'é'), 1.0",
        (),
    )
    .unwrap();
    drop(conn);

    let conn = open_read_only_strict(&path).await.unwrap();
    let message_cols = message_columns(&conn).await.unwrap();
    let session_cols = table_columns(&conn, "sessions").await.unwrap();
    let select_sql = select_new_messages_sql(&message_cols, &session_cols);
    let page = read_new_rows_strict(&conn, &select_sql, StoredCursor::default())
        .await
        .unwrap();
    assert_eq!(page.items.len(), 1);
    assert!(page.items[0].sql_value_oversized);
    assert!(page.items[0].content.is_none());
    assert!(
        page.items[0].sql_measured_bytes > MAX_HERMES_VALUE_BYTES as u64,
        "UTF-8 byte length must drive the typed oversized outcome"
    );
}

fn write_minimal_legacy_state_db(path: &std::path::Path, rows: usize) {
    let mut conn = rusqlite::Connection::open(path).unwrap();
    conn.execute_batch(
        "PRAGMA journal_mode = DELETE;
         CREATE TABLE sessions (id TEXT PRIMARY KEY);
         CREATE TABLE messages (
             id INTEGER PRIMARY KEY AUTOINCREMENT,
             session_id TEXT NOT NULL,
             role TEXT NOT NULL,
             content TEXT,
             timestamp REAL NOT NULL
         );
         INSERT INTO sessions (id) VALUES ('legacy-session');",
    )
    .unwrap();
    let transaction = conn.transaction().unwrap();
    for index in 0..rows {
        transaction
            .execute(
                "INSERT INTO messages (session_id, role, content, timestamp)
                 VALUES ('legacy-session', 'user', ?1, ?2)",
                rusqlite::params![format!("legacy row {index}"), index as f64],
            )
            .unwrap();
    }
    transaction.commit().unwrap();
}

fn sqlite_sidecar(path: &std::path::Path, suffix: &str) -> std::path::PathBuf {
    let mut sidecar = path.as_os_str().to_os_string();
    sidecar.push(suffix);
    sidecar.into()
}

#[tokio::test]
async fn hermes_reader_is_immutable_policy_bound_and_never_creates_files() {
    let dir = tempfile::tempdir().unwrap();
    initialize_owned_store_before_foreign_fixture(dir.path()).await;
    let missing = dir.path().join("missing.db");
    assert!(open_read_only_strict(&missing).await.is_err());
    assert!(!missing.exists());

    let path = dir.path().join("state #?%.db");
    write_minimal_legacy_state_db(&path, 1);
    let before = std::fs::read(&path).unwrap();
    let wal = sqlite_sidecar(&path, "-wal");
    let shm = sqlite_sidecar(&path, "-shm");
    let journal = sqlite_sidecar(&path, "-journal");

    let conn = open_read_only_strict(&path).await.unwrap();
    let policy = conn
        .with(|conn| {
            let pragma = |name| {
                conn.pragma_query_value(None, name, |row| row.get::<_, i64>(0))
                    .unwrap()
            };
            (
                pragma("query_only"),
                pragma("foreign_keys"),
                pragma("trusted_schema"),
                pragma("busy_timeout"),
                conn.limit(rusqlite::limits::Limit::SQLITE_LIMIT_ATTACHED)
                    .unwrap(),
                conn.execute(
                    "INSERT INTO messages (session_id, role, content, timestamp)
                     VALUES ('legacy-session', 'user', 'write probe', 2.0)",
                    [],
                )
                .is_err(),
                conn.execute_batch("ATTACH DATABASE ':memory:' AS other")
                    .is_err(),
            )
        })
        .await
        .unwrap();
    assert_eq!(policy, (1, 1, 0, 0, 0, true, true));
    drop(conn);

    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert!(!wal.exists());
    assert!(!shm.exists());
    assert!(!journal.exists());
}

#[cfg(unix)]
#[tokio::test]
async fn hermes_reader_supports_non_utf8_database_paths() {
    use std::os::unix::ffi::OsStringExt;

    let dir = tempfile::tempdir().unwrap();
    initialize_owned_store_before_foreign_fixture(dir.path()).await;
    let path = dir
        .path()
        .join(std::ffi::OsString::from_vec(b"state-\xff.db".to_vec()));
    write_minimal_legacy_state_db(&path, 1);

    let conn = open_read_only_strict(&path)
        .await
        .expect("open non-UTF-8 foreign database path");
    validate_required_schema(&conn)
        .await
        .expect("read required schema");
}

#[tokio::test]
async fn corrupt_and_incomplete_state_databases_fail_closed() {
    let dir = tempfile::tempdir().unwrap();
    initialize_owned_store_before_foreign_fixture(dir.path()).await;

    let corrupt = dir.path().join("corrupt.db");
    std::fs::write(&corrupt, b"not a sqlite database").unwrap();
    if let Ok(conn) = open_read_only_strict(&corrupt).await {
        assert!(message_columns(&conn).await.is_err());
    }

    let incomplete = dir.path().join("incomplete.db");
    let conn = rusqlite::Connection::open(&incomplete).unwrap();
    conn.execute_batch("CREATE TABLE sessions (id TEXT PRIMARY KEY);")
        .unwrap();
    drop(conn);
    let conn = open_read_only_strict(&incomplete).await.unwrap();
    assert!(message_columns(&conn).await.is_err());

    let malformed = dir.path().join("malformed.db");
    let conn = rusqlite::Connection::open(&malformed).unwrap();
    conn.execute_batch(
        "CREATE TABLE sessions (id TEXT PRIMARY KEY);
         CREATE TABLE messages (
             id INTEGER PRIMARY KEY,
             session_id TEXT NOT NULL,
             content TEXT,
             timestamp REAL NOT NULL
         );",
    )
    .unwrap();
    drop(conn);
    let conn = open_read_only_strict(&malformed).await.unwrap();
    assert!(validate_required_schema(&conn).await.is_err());
}

#[tokio::test]
async fn minimal_legacy_schema_reads_without_optional_columns() {
    let dir = tempfile::tempdir().unwrap();
    initialize_owned_store_before_foreign_fixture(dir.path()).await;
    let path = dir.path().join("state.db");
    write_minimal_legacy_state_db(&path, 1);

    let conn = open_read_only_strict(&path).await.unwrap();
    let select_sql = select_new_messages_sql(
        &message_columns(&conn).await.unwrap(),
        &table_columns(&conn, "sessions").await.unwrap(),
    );
    let page = read_new_rows_strict(&conn, &select_sql, StoredCursor::default())
        .await
        .unwrap();

    assert_eq!(page.items.len(), 1);
    let row = &page.items[0];
    assert_eq!(row.session_id, "legacy-session");
    assert_eq!(row.content.as_deref(), Some("legacy row 0"));
    assert!(row.tool_name.is_none());
    assert!(row.tool_calls.is_none());
    assert!(row.session_model.is_none());
    assert!(row.parent_session_id.is_none());
    assert!(row.session_input_tokens.is_none());
    assert_eq!(row.active, 1);
}

#[tokio::test]
async fn legacy_schema_paginates_without_gaps_or_duplicates() {
    let dir = tempfile::tempdir().unwrap();
    initialize_owned_store_before_foreign_fixture(dir.path()).await;
    let path = dir.path().join("state.db");
    write_minimal_legacy_state_db(&path, CHUNK_ROWS + 3);

    let conn = open_read_only_strict(&path).await.unwrap();
    let select_sql = select_new_messages_sql(
        &message_columns(&conn).await.unwrap(),
        &table_columns(&conn, "sessions").await.unwrap(),
    );
    let first = read_new_rows_strict(&conn, &select_sql, StoredCursor::default())
        .await
        .unwrap();
    let second = read_new_rows_strict(&conn, &select_sql, first.new_cursor)
        .await
        .unwrap();

    assert_eq!(first.items.len(), CHUNK_ROWS);
    assert_eq!(second.items.len(), 3);
    let ids = first
        .items
        .iter()
        .chain(&second.items)
        .map(|row| row.id)
        .collect::<Vec<_>>();
    assert_eq!(ids, (1..=(CHUNK_ROWS + 3) as i64).collect::<Vec<_>>());
}
