//! Provider-usage capture contracts: per-message token counters that ride a
//! transcript record must land as correlated `ProviderUsage` facts (the costs
//! projection deliberately drops `UncorrelatedUsage`), with exact counts, the
//! record's own model when it names one, and typed-unknown model otherwise.
//! Placeholder records that represent no billed provider request must not
//! produce usage facts at all.

use std::fs;
use std::path::PathBuf;

use serde_json::{Value, json};
use tracedecay_capture::{claude, cursor, cursor_composer};
use tracedecay_domain::{
    CanonicalObservationEnvelopeV1, CanonicalObservationFactV1, CanonicalUnknownStateV1,
    ObservationSourceRangeV1, ProviderUsageCounterSemanticsV1, ProviderUsageCountersV1,
    ProviderUsageModelV1, ProviderUsageScopeV1,
};

fn provider_usage_facts(
    envelope: &CanonicalObservationEnvelopeV1,
) -> Vec<&CanonicalObservationFactV1> {
    envelope
        .facts()
        .iter()
        .filter(|fact| {
            matches!(
                fact,
                CanonicalObservationFactV1::ProviderUsage { .. }
                    | CanonicalObservationFactV1::UncorrelatedUsage { .. }
            )
        })
        .collect()
}

#[test]
fn cursor_transcript_token_count_lands_as_correlated_provider_usage() {
    let native = json!({
        "type": "assistant",
        "role": "assistant",
        "model": "gpt-5.2",
        "message": {"content": "Refactored the module."},
        "tokenCount": {"inputTokens": 1200, "outputTokens": 340}
    });
    let record_id =
        cursor::observation_native_record_id("cursor", "cursor-usage-session", &native).unwrap();
    let envelope = cursor::normalize_cursor_observation(
        &native,
        "cursor-usage-session",
        record_id.clone(),
        ObservationSourceRangeV1::new(0, 64).unwrap(),
        None,
        None,
    )
    .unwrap();

    let facts = provider_usage_facts(&envelope);
    assert_eq!(facts.len(), 1, "exactly one usage fact");
    let CanonicalObservationFactV1::ProviderUsage {
        model,
        native_scope,
        counter_semantics,
        counters,
        native_kind,
        native_field,
        ..
    } = facts[0]
    else {
        panic!(
            "cursor tokenCount must land as ProviderUsage, got {:?}",
            facts[0]
        );
    };
    assert_eq!(
        *model,
        ProviderUsageModelV1::Known {
            model: "gpt-5.2".to_owned(),
        }
    );
    assert_eq!(*native_scope, ProviderUsageScopeV1::Message);
    assert_eq!(*counter_semantics, ProviderUsageCounterSemanticsV1::Delta);
    assert_eq!(
        *counters,
        ProviderUsageCountersV1::Known {
            input_tokens: Some(1200),
            output_tokens: Some(340),
            cache_read_tokens: None,
            cache_write_tokens: None,
            reasoning_tokens: None,
            total_tokens: None,
        }
    );
    assert_eq!(native_kind, "assistant");
    assert_eq!(native_field, "tokenCount");
    // Correlation evidence: the envelope binds the usage to its message/session.
    let relations = serde_json::to_value(envelope.relations()).unwrap();
    assert_eq!(relations["session_id"], "cursor-usage-session");
    assert_eq!(relations["message_id"], record_id.as_str());
}

#[test]
fn cursor_transcript_usage_without_model_stays_typed_unknown() {
    let native = json!({
        "type": "assistant",
        "message": {
            "content": "Done.",
            "usage": {"input_tokens": 7, "output_tokens": 3}
        }
    });
    let record_id =
        cursor::observation_native_record_id("cursor", "cursor-modelless", &native).unwrap();
    let envelope = cursor::normalize_cursor_observation(
        &native,
        "cursor-modelless",
        record_id,
        ObservationSourceRangeV1::new(0, 32).unwrap(),
        None,
        None,
    )
    .unwrap();

    let facts = provider_usage_facts(&envelope);
    assert_eq!(facts.len(), 1);
    let CanonicalObservationFactV1::ProviderUsage {
        model,
        counters,
        native_field,
        ..
    } = facts[0]
    else {
        panic!(
            "message.usage must land as ProviderUsage, got {:?}",
            facts[0]
        );
    };
    assert_eq!(
        *model,
        ProviderUsageModelV1::Unknown {
            reason: CanonicalUnknownStateV1::Absent,
        }
    );
    assert_eq!(
        *counters,
        ProviderUsageCountersV1::Known {
            input_tokens: Some(7),
            output_tokens: Some(3),
            cache_read_tokens: None,
            cache_write_tokens: None,
            reasoning_tokens: None,
            total_tokens: None,
        }
    );
    assert_eq!(native_field, "message.usage");
}

#[test]
fn cursor_composer_bubble_token_count_lands_as_correlated_provider_usage() {
    let fixture_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/provider_normalization/cursor_composer");
    let native: Value = serde_json::from_str(
        &fs::read_to_string(fixture_root.join("assistant_bubble.input.json")).unwrap(),
    )
    .unwrap();
    let record_id = cursor_composer::cursor_composer_native_record_id("comp-1", "b-asst").unwrap();
    let envelope = cursor_composer::normalize_cursor_composer_observation(
        &native,
        "comp-1",
        record_id.clone(),
        ObservationSourceRangeV1::new(1, 2).unwrap(),
        1,
    )
    .unwrap();

    let facts = provider_usage_facts(&envelope);
    assert_eq!(facts.len(), 1);
    let CanonicalObservationFactV1::ProviderUsage {
        model,
        native_scope,
        counter_semantics,
        counters,
        native_kind,
        native_field,
        ..
    } = facts[0]
    else {
        panic!(
            "composer tokenCount must land as ProviderUsage, got {:?}",
            facts[0]
        );
    };
    // The fixture bubble names no model of its own; session-level modelConfig
    // is not per-message evidence and must not be inferred.
    assert_eq!(
        *model,
        ProviderUsageModelV1::Unknown {
            reason: CanonicalUnknownStateV1::Absent,
        }
    );
    assert_eq!(*native_scope, ProviderUsageScopeV1::Message);
    assert_eq!(*counter_semantics, ProviderUsageCounterSemanticsV1::Delta);
    assert_eq!(
        *counters,
        ProviderUsageCountersV1::Known {
            input_tokens: Some(1200),
            output_tokens: Some(340),
            cache_read_tokens: None,
            cache_write_tokens: None,
            reasoning_tokens: None,
            total_tokens: None,
        }
    );
    assert_eq!(native_kind, "bubble");
    assert_eq!(native_field, "tokenCount");
    let relations = serde_json::to_value(envelope.relations()).unwrap();
    assert_eq!(relations["session_id"], "comp-1");
    assert_eq!(relations["message_id"], record_id.as_str());
}

/// Real Claude assistant records (live `~/.claude` shape: `input_tokens`,
/// `output_tokens`, `cache_creation_input_tokens`, `cache_read_input_tokens`,
/// plus non-counter fields like `output_tokens_details` and `server_tool_use`)
/// must record their true non-zero counts and the real model id.
#[test]
fn claude_real_usage_shape_records_true_counts_and_model() {
    let native = json!({
        "type": "assistant",
        "uuid": "real-usage-uuid",
        "timestamp": "2026-08-01T00:00:00.000Z",
        "message": {
            "id": "msg_real_usage",
            "role": "assistant",
            "model": "claude-opus-5",
            "content": [{"type": "text", "text": "Traced the ingest path."}],
            "usage": {
                "input_tokens": 2,
                "cache_creation_input_tokens": 11561,
                "cache_read_input_tokens": 23137,
                "output_tokens": 460,
                "output_tokens_details": {"thinking_tokens": 195},
                "server_tool_use": {"web_search_requests": 0},
                "service_tier": "standard"
            }
        }
    });
    let record_id = claude::stable_record_id(&native, "claude-real-session", 0).unwrap();
    let envelope = claude::normalize(
        &native,
        "claude-real-session",
        record_id,
        ObservationSourceRangeV1::new(0, 128).unwrap(),
    )
    .unwrap();

    let facts = provider_usage_facts(&envelope);
    assert_eq!(facts.len(), 1);
    let CanonicalObservationFactV1::ProviderUsage {
        model, counters, ..
    } = facts[0]
    else {
        panic!(
            "claude usage must land as ProviderUsage, got {:?}",
            facts[0]
        );
    };
    assert_eq!(
        *model,
        ProviderUsageModelV1::Known {
            model: "claude-opus-5".to_owned(),
        }
    );
    assert_eq!(
        *counters,
        ProviderUsageCountersV1::Known {
            input_tokens: Some(2),
            output_tokens: Some(460),
            cache_read_tokens: Some(23137),
            cache_write_tokens: Some(11561),
            reasoning_tokens: None,
            total_tokens: None,
        }
    );
}

/// Claude API-error placeholders (`isApiErrorMessage: true`, model
/// `"<synthetic>"`, all-zero usage) represent no billed provider request and
/// must not produce any usage fact — recording them attributed zero-token
/// rows to the non-model `"<synthetic>"` in billing.
#[test]
fn claude_api_error_placeholder_records_no_provider_usage() {
    let placeholder_usage = json!({
        "input_tokens": 0,
        "output_tokens": 0,
        "cache_creation_input_tokens": 0,
        "cache_read_input_tokens": 0
    });
    for native in [
        json!({
            "type": "assistant",
            "uuid": "api-error-uuid",
            "isApiErrorMessage": true,
            "apiErrorStatus": 529,
            "timestamp": "2026-08-01T00:00:00.000Z",
            "message": {
                "id": "msg_api_error",
                "role": "assistant",
                "model": "<synthetic>",
                "content": [{"type": "text", "text": "API Error: overloaded"}],
                "usage": placeholder_usage.clone()
            }
        }),
        // Older placeholder shape: synthetic model without the explicit flag.
        json!({
            "type": "assistant",
            "uuid": "synthetic-model-uuid",
            "timestamp": "2026-08-01T00:00:00.000Z",
            "message": {
                "id": "msg_synthetic_model",
                "role": "assistant",
                "model": "<synthetic>",
                "content": [{"type": "text", "text": "API Error: overloaded"}],
                "usage": placeholder_usage.clone()
            }
        }),
    ] {
        let record_id = claude::stable_record_id(&native, "claude-error-session", 0).unwrap();
        let envelope = claude::normalize(
            &native,
            "claude-error-session",
            record_id,
            ObservationSourceRangeV1::new(0, 128).unwrap(),
        )
        .unwrap();
        assert!(
            provider_usage_facts(&envelope).is_empty(),
            "placeholder record must not produce usage facts: {:?}",
            envelope.facts()
        );
    }
}
