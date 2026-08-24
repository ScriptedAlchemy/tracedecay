//! Differential cover for the pre-decode scope probe.
//!
//! The probe is only allowed to answer `Inert` when the authoritative pipeline
//! would have reached the same verdict after decoding, so the tests here do not
//! restate the probe's rules — they run the real parser and the real context
//! classifiers over the same bytes and require the two to agree.

use serde_json::{Value, json};
use tracedecay_domain::{ObservationOrderingDomainV1, ObservationSourceRangeV1};
use tracedecay_runtime_core::privacy::{
    ObservationRecordParseErrorV1, parse_normalized_observation_record_v1,
};

use super::{CodexFrameScopeProbeV1, probe_codex_frame};
use crate::runtime::codex::{session_meta_from_record, turn_context_from_record};

/// The frame's own file-byte range. A byte range is always non-empty, so the
/// empty-frame fixture is given the smallest legal range and is rejected on its
/// emptiness rather than on its range — the same order the parser uses.
fn range_for(record: &[u8]) -> ObservationSourceRangeV1 {
    ObservationSourceRangeV1::new(0, u64::try_from(record.len()).unwrap().max(1)).unwrap()
}

fn probe(record: &str) -> CodexFrameScopeProbeV1 {
    probe_codex_frame(record.as_bytes(), range_for(record.as_bytes()))
}

/// What the authoritative pipeline does with one frame, observed through the
/// real parser rather than restated.
struct Authoritative {
    /// The parser cleared every gate it applies before the normalizer runs, so
    /// the frame's disposition is the normalizer's to decide.
    reached_normalizer: bool,
    /// One of the two classifiers that can move the session cwd claimed it.
    may_move_cwd: bool,
}

fn authoritative(record: &str) -> Authoritative {
    let bytes = record.as_bytes();
    let mut reached_normalizer = false;
    let mut may_move_cwd = false;
    let _ = parse_normalized_observation_record_v1(
        bytes,
        range_for(bytes),
        ObservationOrderingDomainV1::FileBytes,
        |native: Value| {
            reached_normalizer = true;
            may_move_cwd = session_meta_from_record(&native, std::path::Path::new("rollout.jsonl"))
                .is_some()
                || turn_context_from_record(&native).is_some();
            // The normalizer's own product is irrelevant here; only whether it
            // ran, and what it would have concluded about the cwd.
            Err(ObservationRecordParseErrorV1::NormalizationFailed)
        },
    );
    Authoritative {
        reached_normalizer,
        may_move_cwd,
    }
}

fn nested_arrays(depth: usize) -> String {
    let mut record = String::from("{\"type\":\"event_msg\",\"payload\":");
    record.push_str(&"[".repeat(depth));
    record.push_str(&"]".repeat(depth));
    record.push('}');
    record
}

/// Every frame the probe is asked about, spanning the shapes a rollout really
/// carries plus the encodings a cheaper probe would get wrong.
fn corpus() -> Vec<(&'static str, String)> {
    vec![
        (
            "event message",
            json!({
                "timestamp": "2026-01-01T00:00:01.000Z",
                "type": "event_msg",
                "payload": {"type": "user_message", "message": "hello"}
            })
            .to_string(),
        ),
        (
            "response item with nested content",
            json!({
                "timestamp": "2026-01-01T00:00:02.000Z",
                "type": "response_item",
                "payload": {
                    "type": "message",
                    "role": "assistant",
                    "content": [{"type": "output_text", "text": "a\nb\t\"c\""}]
                }
            })
            .to_string(),
        ),
        (
            "session meta",
            json!({
                "type": "session_meta",
                "payload": {"id": "s", "cwd": "/tmp/workspace"}
            })
            .to_string(),
        ),
        (
            "turn context",
            json!({"type": "turn_context", "payload": {"cwd": "/tmp/workspace"}}).to_string(),
        ),
        (
            "type escaped as unicode",
            r#"{"type":"session_\u006deta","payload":{"cwd":"/tmp/workspace","id":"s"}}"#
                .to_string(),
        ),
        (
            "turn context type escaped as unicode",
            r#"{"type":"turn_\u0063ontext","payload":{"cwd":"/tmp/workspace"}}"#.to_string(),
        ),
        (
            "duplicate type keys resolving to a context record",
            r#"{"type":"event_msg","type":"turn_context","payload":{"cwd":"/tmp/w"}}"#.to_string(),
        ),
        (
            "context type name appearing only in payload text",
            json!({
                "type": "event_msg",
                "payload": {"message": "the session_meta and turn_context lines"}
            })
            .to_string(),
        ),
        ("missing type", json!({"payload": {"a": 1}}).to_string()),
        (
            "non string type",
            json!({"type": 7, "payload": {}}).to_string(),
        ),
        (
            "object type",
            json!({"type": {"nested": "session_meta"}, "payload": {}}).to_string(),
        ),
        ("non object top level", "[1,2,3]".to_string()),
        ("malformed", r#"{"type":"event_msg","#.to_string()),
        ("empty", String::new()),
        (
            "trailing content",
            r#"{"type":"event_msg"} {"type":"turn_context"}"#.to_string(),
        ),
        ("within the depth limit", nested_arrays(90)),
        ("past the depth limit", nested_arrays(120)),
        (
            "past the value limit",
            format!(
                "{{\"type\":\"event_msg\",\"payload\":[{}]}}",
                vec!["0"; 60_000].join(",")
            ),
        ),
    ]
}

/// The load-bearing claim: `Inert` is never asserted over a frame the
/// authoritative pipeline would have treated differently.
///
/// A frame is safe to reject before decoding only when the parser would have
/// handed it to the normalizer at all (otherwise the parser owns a different
/// coverage reason) and the normalizer would have left the session cwd alone
/// (otherwise the scope verdict could change under it).
#[test]
fn inert_is_claimed_only_where_the_authoritative_pipeline_agrees() {
    for (name, record) in corpus() {
        if probe(&record) != CodexFrameScopeProbeV1::Inert {
            continue;
        }
        let observed = authoritative(&record);
        assert!(
            observed.reached_normalizer,
            "{name}: probe claimed inert for a frame the parser rejects before the normalizer"
        );
        assert!(
            !observed.may_move_cwd,
            "{name}: probe claimed inert for a frame that can move the session cwd"
        );
    }
}

/// The probe has to be worth having: the frame shapes that dominate a rollout
/// must actually reach the fast path.
#[test]
fn ordinary_rollout_frames_are_proved_inert() {
    for name in [
        "event message",
        "response item with nested content",
        "context type name appearing only in payload text",
        "missing type",
        "non string type",
        "object type",
        "within the depth limit",
    ] {
        let record = corpus()
            .into_iter()
            .find_map(|(candidate, record)| (candidate == name).then_some(record))
            .unwrap();
        assert_eq!(
            probe(&record),
            CodexFrameScopeProbeV1::Inert,
            "{name}: an ordinary frame must not pay for a decode"
        );
    }
}

/// Anything that can carry context, or that the parser reports under its own
/// coverage reason, must fall through to the parser.
#[test]
fn context_and_gate_failures_stay_undecided() {
    for name in [
        "session meta",
        "turn context",
        "type escaped as unicode",
        "turn context type escaped as unicode",
        "duplicate type keys resolving to a context record",
        "non object top level",
        "malformed",
        "empty",
        "trailing content",
        "past the depth limit",
        "past the value limit",
    ] {
        let record = corpus()
            .into_iter()
            .find_map(|(candidate, record)| (candidate == name).then_some(record))
            .unwrap();
        assert_eq!(
            probe(&record),
            CodexFrameScopeProbeV1::Undecided,
            "{name}: the authoritative parser must own this verdict"
        );
    }
}

/// A frame whose byte range disagrees with its length is a parser gate with its
/// own coverage reason, not a scope question.
#[test]
fn a_range_length_mismatch_stays_undecided() {
    let record = json!({"type": "event_msg", "payload": {}}).to_string();
    let bytes = record.as_bytes();
    let mismatched =
        ObservationSourceRangeV1::new(0, u64::try_from(bytes.len()).unwrap() + 1).unwrap();
    assert_eq!(
        probe_codex_frame(bytes, mismatched),
        CodexFrameScopeProbeV1::Undecided
    );
}
