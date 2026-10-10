use serde_json::Value;
use tracedecay_domain::{
    CanonicalObservationFactV1, ObservationId, tool_result_output_was_cut, tool_result_visible_text,
};
use tracedecay_tokenizer::count_ordinary_tokens;

/// Build a tool-result fact with the real token count and cut marker.
///
/// Counts the output the host recorded, which is what entered the agent's
/// context, footers and notices included, with `o200k_base`. Hosts do not
/// persist MCP `_meta`, so a serve-time stamp could not reach this fact. The
/// tool-body `token_count` field is ignored so source_read's payload cannot
/// replace the served-output count.
pub fn accounted_tool_result(
    invocation_id: Option<ObservationId>,
    content: Value,
    success: Option<bool>,
) -> CanonicalObservationFactV1 {
    let token_count =
        tool_result_visible_text(&content).and_then(|text| count_ordinary_tokens(&text).ok());
    let cut = (!content.is_null()).then(|| tool_result_output_was_cut(&content));
    CanonicalObservationFactV1::tool_result(invocation_id, content, success, token_count, cut)
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tracedecay_domain::{CanonicalObservationFactV1, tool_result_visible_text};
    use tracedecay_tokenizer::count_ordinary_tokens;

    use super::accounted_tool_result;

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
    fn records_the_real_tokenizer_count_and_uncut_marker() {
        let text = "crates/tracedecay-graph-query/src/context/source_read.rs";
        let fact = accounted_tool_result(None, json!(text), Some(true));
        let (token_count, cut, _) = result_fields(&fact);
        assert_eq!(token_count, Some(count_ordinary_tokens(text).unwrap()));
        assert_eq!(cut, Some(false));
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
        assert_eq!(cut, Some(false));
    }

    #[test]
    fn null_content_does_not_invent_a_count_or_cut() {
        let fact = accounted_tool_result(None, json!(null), Some(true));
        let (token_count, cut, _) = result_fields(&fact);
        assert_eq!(token_count, None);
        assert_eq!(cut, None);
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
            assert_eq!(token_count, Some(expected), "{body}");
            assert_eq!(cut, Some(body.contains("# Truncated Response")), "{body}");
            let wire = serde_json::to_value(&fact).unwrap();
            assert_eq!(wire["token_count"], expected, "{body}");
            assert_eq!(wire["cut"], body.contains("# Truncated Response"), "{body}");
        }
    }
}
