use serde_json::Value;
use tracedecay_domain::{
    CanonicalObservationFactV1, ObservationId, tool_result_output_cut_state,
    tool_result_visible_text,
};
use tracedecay_tokenizer::count_ordinary_tokens;

/// Build a tool-result fact with the real token count and cut marker.
///
/// Counts the output the host recorded, which is what entered the agent's
/// context, footers and notices included, with `o200k_base`. Hosts do not
/// persist MCP `_meta`, so ingest is the only authority. The tool-body
/// `token_count` field is ignored so source_read's payload cannot replace
/// the served-output count.
pub fn accounted_tool_result(
    invocation_id: Option<ObservationId>,
    content: Value,
    success: Option<bool>,
) -> CanonicalObservationFactV1 {
    accounted_tool_result_with_recorded_output(invocation_id, content, success, None)
}

/// Same as [`accounted_tool_result`], counting `recorded_output` when stored
/// `content` is `Null`. Codex and Composer keep Null canonical content; the
/// unused-context meter still scores the text the host recorded. Native
/// output is not an MCP envelope, so it counts as recorded: a JSON string
/// counts verbatim and structured values count as their recorded JSON.
pub fn accounted_tool_result_with_recorded_output(
    invocation_id: Option<ObservationId>,
    content: Value,
    success: Option<bool>,
    recorded_output: Option<&Value>,
) -> CanonicalObservationFactV1 {
    let visible = tool_result_visible_text(&content)
        .or_else(|| recorded_output.and_then(recorded_output_text));
    let token_count = visible
        .as_deref()
        .and_then(|text| count_ordinary_tokens(text).ok());
    let cut = match (
        tool_result_output_cut_state(&content),
        recorded_output.and_then(tool_result_output_cut_state),
    ) {
        (Some(true), _) | (_, Some(true)) => Some(true),
        (Some(false), _) | (_, Some(false)) => Some(false),
        _ => None,
    };
    CanonicalObservationFactV1::tool_result(invocation_id, content, success, token_count, cut)
}

/// Text a host recorded for a native tool result, counted exactly as served.
/// Unlike [`tool_result_visible_text`], this never treats the value as an MCP
/// envelope: a recorded JSON string keeps its wrapper fields in the count.
fn recorded_output_text(output: &Value) -> Option<String> {
    match output {
        Value::Null => None,
        Value::String(text) => Some(text.clone()),
        other => Some(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tracedecay_domain::{CanonicalObservationFactV1, tool_result_visible_text};
    use tracedecay_tokenizer::count_ordinary_tokens;

    use super::{accounted_tool_result, accounted_tool_result_with_recorded_output};

    fn result_fields(
        fact: &CanonicalObservationFactV1,
    ) -> (Option<u64>, Option<bool>, &serde_json::Value) {
        match fact {
            CanonicalObservationFactV1::ToolResult {
                token_count,
                cut,
                content,
                ..
            } => (*token_count, *cut, content),
            other => panic!("expected tool_result, got {other:?}"),
        }
    }

    #[test]
    fn records_the_real_tokenizer_count_with_unknown_cut_state() {
        let text = "crates/tracedecay-graph-query/src/context/source_read.rs";
        let fact = accounted_tool_result(None, json!(text), Some(true));
        let (token_count, cut, _) = result_fields(&fact);
        assert_eq!(token_count, Some(count_ordinary_tokens(text).unwrap()));
        assert_eq!(cut, None);
    }

    #[test]
    fn counts_served_json_text_as_served_not_reserialized() {
        let served = "{\n  \"results\": [\n    {\n      \"name\": \"read_source\",\n      \"line\": 144\n    }\n  ]\n}";
        let fact = accounted_tool_result(None, json!(served), Some(true));
        let (token_count, _, _) = result_fields(&fact);
        assert_eq!(token_count, Some(count_ordinary_tokens(served).unwrap()));
    }

    #[test]
    fn records_cut_when_the_json_envelope_was_truncated() {
        let content = json!({
            "truncated": true,
            "preview": "head",
            "handle": "h1",
            "retrieve_tool": "tracedecay_retrieve",
        });
        let fact = accounted_tool_result(None, content, Some(true));
        let (_, cut, _) = result_fields(&fact);
        assert_eq!(cut, Some(true));
    }

    #[test]
    fn ignores_source_read_body_token_count() {
        let content = json!({
            "file": "lib.rs",
            "token_count": 4,
            "body": "fn main() { println!(\"hello from source_read\"); }",
        });
        let fact = accounted_tool_result(None, content.clone(), Some(true));
        let (token_count, cut, _) = result_fields(&fact);
        let visible = tool_result_visible_text(&content).unwrap();
        let expected = count_ordinary_tokens(&visible).unwrap();
        assert_eq!(token_count, Some(expected));
        assert_ne!(token_count, Some(4));
        assert_eq!(cut, None);
    }

    #[test]
    fn null_content_does_not_invent_a_count_or_cut() {
        let fact = accounted_tool_result(None, json!(null), Some(true));
        let (token_count, cut, _) = result_fields(&fact);
        assert_eq!(token_count, None);
        assert_eq!(cut, None);
    }

    #[test]
    fn counts_codex_output_when_canonical_content_is_null() {
        let fact = accounted_tool_result_with_recorded_output(
            None,
            json!(null),
            Some(true),
            Some(&json!("test result: ok")),
        );
        let (token_count, cut, content) = result_fields(&fact);
        assert_eq!(content, &json!(null));
        assert_eq!(
            token_count,
            Some(count_ordinary_tokens("test result: ok").unwrap())
        );
        assert_eq!(cut, None);
    }

    #[test]
    fn counts_composer_result_when_canonical_content_is_null() {
        let fact = accounted_tool_result_with_recorded_output(
            None,
            json!(null),
            Some(true),
            Some(&json!("{\"ok\":true}")),
        );
        let (token_count, cut, content) = result_fields(&fact);
        assert_eq!(content, &json!(null));
        assert_eq!(
            token_count,
            Some(count_ordinary_tokens("{\"ok\":true}").unwrap())
        );
        assert_eq!(cut, None);
    }

    #[test]
    fn counts_native_json_output_verbatim_not_the_envelope_field() {
        // Codex `payload.output` and Composer `toolFormerData.result` are
        // native result values, not MCP envelopes: the agent read the JSON
        // string with its wrapper fields, so the count must keep them.
        let recorded = json!("{\"output\":\"ok\",\"duration\":12}");
        let fact = accounted_tool_result_with_recorded_output(
            None,
            json!(null),
            Some(true),
            Some(&recorded),
        );
        let (token_count, cut, _) = result_fields(&fact);
        assert_eq!(
            token_count,
            Some(count_ordinary_tokens("{\"output\":\"ok\",\"duration\":12}").unwrap())
        );
        assert_eq!(cut, None);
    }

    #[test]
    fn counts_native_structured_output_as_recorded() {
        let recorded = json!({"output": "ok", "duration": 12});
        let fact = accounted_tool_result_with_recorded_output(
            None,
            json!(null),
            Some(true),
            Some(&recorded),
        );
        let (token_count, _, _) = result_fields(&fact);
        assert_eq!(
            token_count,
            Some(count_ordinary_tokens(&recorded.to_string()).unwrap())
        );
    }

    #[test]
    fn unused_context_meter_spot_checks_three_real_counts() {
        let spots = [
            "crates/tracedecay-graph-query/src/context/source_read.rs:144\nfn estimate_tokens",
            "pub fn estimate_tokens(s: &str) -> u32 {\n    s.chars().count().div_ceil(4)\n}",
            "# Truncated Response\n\npreview of Walk::read",
        ];
        for body in spots {
            let fact = accounted_tool_result(None, json!(body), Some(true));
            let (token_count, cut, _) = result_fields(&fact);
            let expected = count_ordinary_tokens(body).unwrap();
            let expected_cut = body.contains("# Truncated Response").then_some(true);
            assert_eq!(token_count, Some(expected), "{body}");
            assert_eq!(cut, expected_cut, "{body}");
            let wire = serde_json::to_value(&fact).unwrap();
            assert_eq!(wire["token_count"], expected, "{body}");
            assert_eq!(
                wire.get("cut").cloned(),
                expected_cut.map(|value| json!(value)),
                "{body}"
            );
        }
    }

    #[test]
    fn marks_native_truncation_warning_as_cut() {
        let recorded = json!("Warning: truncated output (original token count: 6923)\nfirst page");
        let fact = accounted_tool_result_with_recorded_output(
            None,
            json!(null),
            Some(true),
            Some(&recorded),
        );
        let (_, cut, _) = result_fields(&fact);
        assert_eq!(cut, Some(true));
    }

    #[test]
    fn marks_native_truncation_warning_after_a_wrapper_header_as_cut() {
        let recorded = json!(
            "Wall time: 2.5000 seconds\nProcess exited with code 0\nOutput:\nWarning: truncated output (original token count: 6923)\nfirst page"
        );
        let fact = accounted_tool_result_with_recorded_output(
            None,
            json!(null),
            Some(true),
            Some(&recorded),
        );
        let (_, cut, _) = result_fields(&fact);
        assert_eq!(cut, Some(true));
    }

    #[test]
    fn does_not_mark_truncation_words_in_served_text() {
        for content in [
            json!("// handle truncated output here\nfn parse() {}"),
            json!("// Warning: truncated names are rejected\nfn parse() {}"),
        ] {
            let fact = accounted_tool_result(None, content, Some(true));
            let (_, cut, _) = result_fields(&fact);
            assert_eq!(cut, None);
        }
    }

    #[test]
    fn honors_an_explicit_truncated_false_marker() {
        let fact = accounted_tool_result(
            None,
            json!({"metadata": {"truncated": false}, "content": "full page"}),
            Some(true),
        );
        let (_, cut, _) = result_fields(&fact);
        assert_eq!(cut, Some(false));
    }

    #[test]
    fn leaves_opaque_output_cut_state_unknown() {
        let fact = accounted_tool_result_with_recorded_output(
            None,
            json!(null),
            Some(true),
            Some(&json!("an unmarked legacy result")),
        );
        let (token_count, cut, _) = result_fields(&fact);
        assert_eq!(
            token_count,
            Some(count_ordinary_tokens("an unmarked legacy result").unwrap())
        );
        assert_eq!(cut, None);
    }
}
