use serde_json::{Map, Value};
use tracedecay_domain::canonical_text::canonical_framed_sha256;

const PROVIDER: &str = "kiro";
const DELIMITED_NATIVE_MESSAGE_ID_DOMAIN: &[u8] = b"tracedecay.kiro-delimited-native-message.v2";
const DELIMITED_NATIVE_MESSAGE_ID_PREFIX: &str = "kiro.message-id.v2.";
const DERIVED_MESSAGE_ID_DOMAIN: &[u8] = b"tracedecay.kiro-derived-message.v3";
const DERIVED_MESSAGE_ID_PREFIX: &str = "kiro.derived-message.v3.";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KiroSnapshotMessage<'a> {
    pub session_id: &'a str,
    pub message_id: &'a str,
    pub role: &'a str,
    pub timestamp: Option<i64>,
    pub ordinal: i64,
    pub text: &'a str,
    pub kind: Option<&'a str>,
    pub model: Option<&'a str>,
}

/// Shapes only the Kiro fields evidenced by checked-in transcript fixtures.
///
/// Discovery metadata and provider-private bags are intentionally absent so
/// the root adapter can admit this bounded payload through sanitization before
/// any durable write.
pub fn snapshot_native_payload(message: KiroSnapshotMessage<'_>) -> Value {
    let mut fields = Map::new();
    fields.insert("provider".to_string(), Value::String(PROVIDER.to_string()));
    fields.insert(
        "session_id".to_string(),
        Value::String(message.session_id.to_string()),
    );
    fields.insert(
        "message_id".to_string(),
        Value::String(message.message_id.to_string()),
    );
    fields.insert("role".to_string(), Value::String(message.role.to_string()));
    fields.insert("ordinal".to_string(), Value::from(message.ordinal));
    fields.insert("text".to_string(), Value::String(message.text.to_string()));
    if let Some(timestamp) = message.timestamp {
        fields.insert("timestamp".to_string(), Value::from(timestamp));
    }
    if let Some(kind) = message.kind {
        fields.insert("kind".to_string(), Value::String(kind.to_string()));
    }
    if let Some(model) = message.model {
        fields.insert("model".to_string(), Value::String(model.to_string()));
    }
    Value::Object(fields)
}

/// Preserves Kiro's two-tier native/derived snapshot message identity.
pub fn stable_message_id(
    session_id: &str,
    native_id: Option<&str>,
    role: &str,
    timestamp: Option<i64>,
    occurrence: usize,
    text: &str,
) -> String {
    if let Some(native_id) = native_id {
        if !session_id.contains(':') && !native_id.contains(':') {
            return format!("{session_id}:{native_id}");
        }
        let digest = canonical_framed_sha256(
            DELIMITED_NATIVE_MESSAGE_ID_DOMAIN,
            &[session_id.as_bytes(), native_id.as_bytes()],
        );
        return format!("{DELIMITED_NATIVE_MESSAGE_ID_PREFIX}{digest}");
    }

    let timestamp_bytes = timestamp.map(i64::to_be_bytes);
    let timestamp_bytes = timestamp_bytes
        .as_ref()
        .map_or(&[][..], |bytes| bytes.as_slice());
    let occurrence_bytes = u64::try_from(occurrence).unwrap_or(u64::MAX).to_be_bytes();
    let digest = canonical_framed_sha256(
        DERIVED_MESSAGE_ID_DOMAIN,
        &[
            session_id.as_bytes(),
            role.as_bytes(),
            timestamp_bytes,
            text.as_bytes(),
            &occurrence_bytes,
        ],
    );
    format!("{DERIVED_MESSAGE_ID_PREFIX}{digest}")
}
