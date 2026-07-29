use serde_json::Value;
use sha2::{Digest, Sha256};
use tracedecay_domain::ObservationId;

use crate::ObservationRecordParseErrorV1;

const PROVIDER: &str = "codex";

pub fn record_supported(value: &Value) -> bool {
    matches!(
        value.get("type").and_then(Value::as_str),
        Some(
            "session_meta"
                | "turn_context"
                | "event_msg"
                | "response_item"
                | "compacted"
                | "inter_agent_communication"
        )
    )
}

pub fn native_record_id(
    session_id: &str,
    value: &Value,
) -> Result<ObservationId, ObservationRecordParseErrorV1> {
    let mut hasher = Sha256::new();
    hasher.update(b"tracedecay.provider-native-record.v1\0codex\0");
    hasher.update(session_id.as_bytes());
    hasher.update([0]);
    hasher.update(
        serde_json::to_vec(value)
            .map_err(|_| ObservationRecordParseErrorV1::NormalizationFailed)?,
    );
    Ok(ObservationId::new(format!(
        "codex.native.sha256:{}",
        hex::encode(hasher.finalize())
    ))
    .map_err(|_| ObservationRecordParseErrorV1::NormalizationFailed)?)
}

pub const fn provider() -> &'static str {
    PROVIDER
}
