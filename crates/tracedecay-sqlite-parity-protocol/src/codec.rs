use sha2::{Digest, Sha256};

use crate::{
    ErrorCode, ErrorPayload, PROTOCOL_VERSION, Request, SNAPSHOT_DIGEST_ALGORITHM,
    command::validate_request_wire_shape, validate_request,
};

pub fn decode_request_value(value: serde_json::Value) -> Result<Request, ErrorPayload> {
    validate_request_wire_shape(&value)?;
    let request = serde_json::from_value(value).map_err(|error| {
        ErrorPayload::new(
            ErrorCode::InvalidRequest,
            format!("request does not match protocol v{PROTOCOL_VERSION}: {error}"),
        )
    })?;
    validate_request(&request)?;
    Ok(request)
}

pub struct CanonicalRowHasher {
    hasher: Sha256,
}

impl CanonicalRowHasher {
    #[must_use]
    pub fn new() -> Self {
        Self {
            hasher: Sha256::new(),
        }
    }

    pub fn update_null(&mut self) {
        self.update(0, &[]);
    }

    pub fn update_integer(&mut self, value: i64) {
        self.update(1, &value.to_be_bytes());
    }

    pub fn update_real(&mut self, value: f64) {
        self.update(2, &value.to_bits().to_be_bytes());
    }

    pub fn update_text(&mut self, value: &[u8]) {
        self.update(3, value);
    }

    pub fn update_blob(&mut self, value: &[u8]) {
        self.update(4, value);
    }

    fn update(&mut self, tag: u8, bytes: &[u8]) {
        self.hasher.update([tag]);
        self.hasher.update((bytes.len() as u64).to_be_bytes());
        self.hasher.update(bytes);
    }

    #[must_use]
    pub fn finish(self) -> String {
        format!(
            "{SNAPSHOT_DIGEST_ALGORITHM}:{}",
            hex::encode(self.hasher.finalize())
        )
    }
}

impl Default for CanonicalRowHasher {
    fn default() -> Self {
        Self::new()
    }
}
