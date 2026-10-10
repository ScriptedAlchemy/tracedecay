use serde_json::Value;
use tracedecay_domain::{
    CanonicalObservationFactV1, ObservationId, tool_result_output_was_cut, tool_result_visible_text,
};
use tracedecay_tokenizer::count_ordinary_tokens;

/// Build a tool-result fact with the real served token count and cut marker.
///
/// Counts visible output with `o200k_base`. A host serve stamp on the native
/// item (`token_count` / `cut`, including `_meta`) wins when present. The
/// tool-body `token_count` field is ignored so source_read's payload cannot
/// replace the served-output count.
pub fn accounted_tool_result(
    invocation_id: Option<ObservationId>,
    content: Value,
    success: Option<bool>,
) -> CanonicalObservationFactV1 {
    accounted_tool_result_from_native(invocation_id, content, success, None)
}

/// Same as [`accounted_tool_result`], reading an optional native tool_result item
/// for a serve-time stamp.
pub fn accounted_tool_result_from_native(
    invocation_id: Option<ObservationId>,
    content: Value,
    success: Option<bool>,
    native: Option<&Value>,
) -> CanonicalObservationFactV1 {
    let token_count = native.and_then(serve_token_count).or_else(|| {
        tool_result_visible_text(&content).and_then(|text| count_ordinary_tokens(&text).ok())
    });
    let cut = native.and_then(serve_cut).or_else(|| {
        if matches!(content, Value::Null) {
            None
        } else {
            Some(tool_result_output_was_cut(&content))
        }
    });
    CanonicalObservationFactV1::tool_result(invocation_id, content, success, token_count, cut)
}

fn serve_token_count(native: &Value) -> Option<u64> {
    native
        .get("token_count")
        .and_then(as_nonnegative_u64)
        .or_else(|| native.get("_meta").and_then(serve_token_count))
}

fn serve_cut(native: &Value) -> Option<bool> {
    native
        .get("cut")
        .and_then(as_cut)
        .or_else(|| native.get("_meta").and_then(serve_cut))
}

fn as_nonnegative_u64(value: &Value) -> Option<u64> {
    match value {
        Value::Number(number) => number
            .as_u64()
            .or_else(|| number.as_i64().and_then(|count| u64::try_from(count).ok())),
        _ => None,
    }
}

fn as_cut(value: &Value) -> Option<bool> {
    match value {
        Value::Bool(applied) => Some(*applied),
        Value::Object(map) => map.get("applied").and_then(Value::as_bool),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tracedecay_domain::{CanonicalObservationFactV1, tool_result_visible_text};
    use tracedecay_tokenizer::count_ordinary_tokens;

    use super::{accounted_tool_result, accounted_tool_result_from_native};

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
    fn records_cut_when_the_json_envelope_was_truncated() {
        let content = json!({
            "truncated": true,
            "cut": true,
            "preview": "head",
            "handle": "h1",
            "retrieve_tool": "tracedecay_retrieve",
        });
        let fact = accounted_tool_result(None, content, Some(true));
        let (_, cut, _) = result_fields(&fact);
        assert_eq!(cut, Some(true));
    }

    #[test]
    fn prefers_the_serve_stamp_over_recounting() {
        let native = json!({"token_count": 9, "cut": false, "content": "ignored"});
        let fact = accounted_tool_result_from_native(
            None,
            json!("much longer served body than nine tokens"),
            Some(true),
            Some(&native),
        );
        let (token_count, cut, _) = result_fields(&fact);
        assert_eq!(token_count, Some(9));
        assert_eq!(cut, Some(false));
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
}
