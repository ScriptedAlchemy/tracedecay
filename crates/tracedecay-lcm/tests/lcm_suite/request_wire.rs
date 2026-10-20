use tracedecay_lcm::{LcmCompressionRequest, LcmPreflightRequest};

const FULL_COMPRESSION_REQUEST: &str = r#"{"provider":"cursor","session_id":"session-1","messages":[{"role":"user","content":"hi"}],"current_tokens":120,"focus_topic":"billing","ignore_session_patterns":["ignored-*"],"stateless_session_patterns":["stateless-*"],"ignore_message_patterns":["^noise"],"expected_current_frontier_store_id":7,"threshold_tokens":1,"max_assembly_tokens":2,"leaf_chunk_tokens":3,"max_source_messages":4,"summary_fan_in":5,"incremental_max_depth":-1,"fresh_tail_count":0,"dynamic_leaf_chunk_enabled":false,"dynamic_leaf_chunk_max":8,"context_length":9,"reserve_tokens_floor":10,"summarizer":{"mode":"provided","summary_text":"summary","route":"backup"}}"#;
const MINIMAL_COMPRESSION_REQUEST: &str = r#"{"provider":"cursor","session_id":"session-1","messages":[],"current_tokens":null,"focus_topic":null,"summarizer":{"mode":"noop"}}"#;
const FULL_PREFLIGHT_REQUEST: &str = r#"{"provider":"cursor","session_id":"session-1","messages":[{"role":"user","content":"hi"}],"current_tokens":120,"threshold_tokens":1,"max_assembly_tokens":2,"leaf_chunk_tokens":3,"max_source_messages":4,"summary_fan_in":5,"incremental_max_depth":-1,"fresh_tail_count":0,"dynamic_leaf_chunk_enabled":true,"dynamic_leaf_chunk_max":8,"context_length":9,"reserve_tokens_floor":10,"ignore_session_patterns":["ignored-*"],"stateless_session_patterns":["stateless-*"]}"#;
const MINIMAL_PREFLIGHT_REQUEST: &str =
    r#"{"provider":"cursor","session_id":"session-1","current_tokens":null}"#;

fn reencoded_compression(json: &str) -> String {
    let request: LcmCompressionRequest =
        serde_json::from_str(json).expect("compression request should deserialize");
    serde_json::to_string(&request).expect("compression request should serialize")
}

fn reencoded_preflight(json: &str) -> String {
    let request: LcmPreflightRequest =
        serde_json::from_str(json).expect("preflight request should deserialize");
    serde_json::to_string(&request).expect("preflight request should serialize")
}

#[test]
fn compression_request_wire_bytes_round_trip_flat() {
    assert_eq!(
        reencoded_compression(FULL_COMPRESSION_REQUEST),
        FULL_COMPRESSION_REQUEST
    );
    assert_eq!(
        reencoded_compression(MINIMAL_COMPRESSION_REQUEST),
        MINIMAL_COMPRESSION_REQUEST
    );
}

#[test]
fn preflight_request_wire_bytes_round_trip_flat() {
    assert_eq!(
        reencoded_preflight(FULL_PREFLIGHT_REQUEST),
        FULL_PREFLIGHT_REQUEST
    );
    assert_eq!(
        reencoded_preflight(MINIMAL_PREFLIGHT_REQUEST),
        MINIMAL_PREFLIGHT_REQUEST
    );
}

#[test]
fn explicit_null_and_unknown_overrides_read_as_absent() {
    let with_nulls = r#"{"provider":"cursor","session_id":"session-1","messages":[],"current_tokens":null,"focus_topic":null,"threshold_tokens":null,"max_assembly_tokens":null,"leaf_chunk_tokens":null,"max_source_messages":null,"summary_fan_in":null,"incremental_max_depth":null,"fresh_tail_count":null,"dynamic_leaf_chunk_enabled":null,"dynamic_leaf_chunk_max":null,"context_length":null,"reserve_tokens_floor":null,"future_override":99,"summarizer":{"mode":"noop"}}"#;
    assert_eq!(
        reencoded_compression(with_nulls),
        MINIMAL_COMPRESSION_REQUEST
    );

    let preflight_with_nulls = r#"{"provider":"cursor","session_id":"session-1","current_tokens":null,"threshold_tokens":null,"dynamic_leaf_chunk_enabled":null,"future_override":99}"#;
    assert_eq!(
        reencoded_preflight(preflight_with_nulls),
        MINIMAL_PREFLIGHT_REQUEST
    );
}

#[test]
fn duplicate_or_mistyped_overrides_are_rejected() {
    for rejected in [
        r#"{"provider":"cursor","session_id":"session-1","current_tokens":null,"threshold_tokens":1,"threshold_tokens":2}"#,
        r#"{"provider":"cursor","session_id":"session-1","current_tokens":null,"max_source_messages":-1}"#,
        r#"{"provider":"cursor","session_id":"session-1","current_tokens":null,"dynamic_leaf_chunk_enabled":"yes"}"#,
    ] {
        assert!(
            serde_json::from_str::<LcmPreflightRequest>(rejected).is_err(),
            "{rejected} should be rejected"
        );
    }
    assert!(
        serde_json::from_str::<LcmCompressionRequest>(
            r#"{"provider":"cursor","session_id":"session-1","messages":[],"current_tokens":null,"focus_topic":null,"fresh_tail_count":1,"fresh_tail_count":2,"summarizer":{"mode":"noop"}}"#
        )
        .is_err()
    );
}
