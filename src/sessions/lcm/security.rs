//! Compatibility façade for LCM security reducers.

pub use tracedecay_sessions::lcm::security::{
    contains_data_uri, contains_media_payload, has_long_base64_run, heartbeat_noise_reason,
    ignore_message_reason, matches_any_pattern, pattern_matches, quarantine_reason,
    should_externalize,
};

#[allow(unused_imports)]
pub(crate) use tracedecay_sessions::lcm::security::{
    CompiledPatternSet, compile_message_patterns, compile_session_patterns, data_uri_spans,
    ignore_message_reason_with_compiled, long_base64_run_spans, matches_any_compiled_pattern,
    prefers_whole_message_externalization,
};
