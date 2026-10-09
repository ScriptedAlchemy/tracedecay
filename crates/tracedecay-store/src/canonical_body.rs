//! Content-addressed session bodies.
//!
//! Observation JSON and LCM raw rows used to each store the full canonical
//! text. Large strings live once in `session_canonical_bodies`; the row keeps
//! a hash ref. Bodies at or above [`INLINE_BODY_BYTES`] are deflated when that
//! shrinks them.

use std::io::{Read, Write};

use flate2::Compression;
use flate2::read::DeflateDecoder;
use flate2::write::DeflateEncoder;
use serde_json::Value;
use thiserror::Error;
use tracedecay_domain::canonical_text::sha256_hex;
use tracedecay_domain::{DurableObservationV1, ObservationContractError};

pub const INLINE_BODY_BYTES: usize = 4096;
pub const BODY_REF_KEY: &str = "tracedecay.body_ref";
pub const ENCODING_IDENTITY: &str = "identity";
pub const ENCODING_DEFLATE: &str = "deflate";

pub const CANONICAL_BODIES_TABLE_SQL: &str = "
        CREATE TABLE IF NOT EXISTS session_canonical_bodies (
            content_hash TEXT PRIMARY KEY,
            encoding TEXT NOT NULL CHECK(encoding IN ('identity', 'deflate')),
            body BLOB NOT NULL,
            uncompressed_bytes INTEGER NOT NULL CHECK(uncompressed_bytes >= 0)
        );";

pub const UPSERT_CANONICAL_BODY_SQL: &str = "INSERT OR IGNORE INTO session_canonical_bodies
            (content_hash, encoding, body, uncompressed_bytes)
         VALUES (?1, ?2, ?3, ?4)";

pub const LOAD_CANONICAL_BODY_SQL: &str =
    "SELECT encoding, body FROM session_canonical_bodies WHERE content_hash = ?1";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredCanonicalBody {
    pub content_hash: String,
    pub encoding: &'static str,
    pub blob: Vec<u8>,
    pub uncompressed_bytes: i64,
}

#[derive(Debug, Error)]
pub enum CanonicalBodyError {
    #[error("canonical body {content_hash} is missing")]
    Missing { content_hash: String },
    #[error("canonical body {content_hash} is not valid {encoding} payload")]
    Corrupt {
        content_hash: String,
        encoding: String,
    },
    #[error("stored observation JSON is not valid")]
    InvalidJson,
    #[error(transparent)]
    Observation(#[from] ObservationContractError),
    #[error(transparent)]
    Serde(#[from] serde_json::Error),
}

impl StoredCanonicalBody {
    pub fn pack(bytes: &[u8]) -> Result<Self, CanonicalBodyError> {
        let uncompressed_bytes =
            i64::try_from(bytes.len()).map_err(|_| CanonicalBodyError::Corrupt {
                content_hash: sha256_hex(bytes),
                encoding: ENCODING_IDENTITY.to_owned(),
            })?;
        let content_hash = sha256_hex(bytes);
        if bytes.len() >= INLINE_BODY_BYTES
            && let Some(blob) = deflate_if_smaller(bytes)
        {
            return Ok(Self {
                content_hash,
                encoding: ENCODING_DEFLATE,
                blob,
                uncompressed_bytes,
            });
        }
        Ok(Self {
            content_hash,
            encoding: ENCODING_IDENTITY,
            blob: bytes.to_vec(),
            uncompressed_bytes,
        })
    }

    pub fn unpack(&self) -> Result<Vec<u8>, CanonicalBodyError> {
        unpack_body(&self.content_hash, self.encoding, &self.blob)
    }
}

pub fn unpack_body(
    content_hash: &str,
    encoding: &str,
    blob: &[u8],
) -> Result<Vec<u8>, CanonicalBodyError> {
    let bytes = match encoding {
        ENCODING_IDENTITY => blob.to_vec(),
        ENCODING_DEFLATE => inflate(blob).ok_or_else(|| CanonicalBodyError::Corrupt {
            content_hash: content_hash.to_owned(),
            encoding: encoding.to_owned(),
        })?,
        other => {
            return Err(CanonicalBodyError::Corrupt {
                content_hash: content_hash.to_owned(),
                encoding: other.to_owned(),
            });
        }
    };
    if sha256_hex(&bytes) != content_hash {
        return Err(CanonicalBodyError::Corrupt {
            content_hash: content_hash.to_owned(),
            encoding: encoding.to_owned(),
        });
    }
    Ok(bytes)
}

pub fn stored_json_needs_hydrate(json: &str) -> bool {
    json.contains(BODY_REF_KEY)
}

pub fn collect_body_refs(json: &str) -> Result<Vec<String>, CanonicalBodyError> {
    if !stored_json_needs_hydrate(json) {
        return Ok(Vec::new());
    }
    let value: Value = serde_json::from_str(json).map_err(|_| CanonicalBodyError::InvalidJson)?;
    let mut hashes = Vec::new();
    collect_refs(&value, &mut hashes);
    Ok(hashes)
}

pub fn slim_json_value(value: &mut Value) -> Result<Vec<StoredCanonicalBody>, CanonicalBodyError> {
    let mut bodies = Vec::new();
    slim_value(value, &mut bodies)?;
    Ok(bodies)
}

pub fn hydrate_json_value<E>(
    value: &mut Value,
    mut load: impl FnMut(&str) -> Result<Vec<u8>, E>,
) -> Result<bool, E>
where
    E: From<CanonicalBodyError>,
{
    hydrate_value(value, &mut load)
}

pub fn slim_stored_json(
    json: &str,
) -> Result<(String, Vec<StoredCanonicalBody>), CanonicalBodyError> {
    let mut value: Value =
        serde_json::from_str(json).map_err(|_| CanonicalBodyError::InvalidJson)?;
    let bodies = slim_json_value(&mut value)?;
    if bodies.is_empty() {
        return Ok((json.to_owned(), bodies));
    }
    Ok((value.to_string(), bodies))
}

pub fn parse_stored_observation<E>(
    json: &str,
    mut load: impl FnMut(&str) -> Result<Vec<u8>, E>,
) -> Result<DurableObservationV1, E>
where
    E: From<CanonicalBodyError>,
{
    if !stored_json_needs_hydrate(json) {
        return serde_json::from_str(json).map_err(|error| E::from(error.into()));
    }
    let mut value: Value =
        serde_json::from_str(json).map_err(|_| E::from(CanonicalBodyError::InvalidJson))?;
    hydrate_json_value(&mut value, &mut load)?;
    serde_json::from_value(value).map_err(|error| E::from(error.into()))
}

fn slim_value(
    value: &mut Value,
    bodies: &mut Vec<StoredCanonicalBody>,
) -> Result<(), CanonicalBodyError> {
    match value {
        Value::String(text) if text.len() >= INLINE_BODY_BYTES => {
            let body = StoredCanonicalBody::pack(text.as_bytes())?;
            *value = body_ref_value(&body.content_hash);
            bodies.push(body);
        }
        Value::Array(items) => {
            for item in items {
                slim_value(item, bodies)?;
            }
        }
        Value::Object(map) if body_ref_hash(map).is_none() => {
            for child in map.values_mut() {
                slim_value(child, bodies)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn hydrate_value<E>(
    value: &mut Value,
    load: &mut impl FnMut(&str) -> Result<Vec<u8>, E>,
) -> Result<bool, E>
where
    E: From<CanonicalBodyError>,
{
    match value {
        Value::Array(items) => {
            let mut changed = false;
            for item in items {
                changed |= hydrate_value(item, load)?;
            }
            Ok(changed)
        }
        Value::Object(map) => {
            if let Some(hash) = body_ref_hash(map).map(str::to_owned) {
                let bytes = load(&hash)?;
                let text = String::from_utf8(bytes).map_err(|_| {
                    E::from(CanonicalBodyError::Corrupt {
                        content_hash: hash,
                        encoding: ENCODING_IDENTITY.to_owned(),
                    })
                })?;
                *value = Value::String(text);
                return Ok(true);
            }
            let mut changed = false;
            for child in map.values_mut() {
                changed |= hydrate_value(child, load)?;
            }
            Ok(changed)
        }
        _ => Ok(false),
    }
}

fn collect_refs(value: &Value, hashes: &mut Vec<String>) {
    match value {
        Value::Array(items) => {
            for item in items {
                collect_refs(item, hashes);
            }
        }
        Value::Object(map) => {
            if let Some(hash) = body_ref_hash(map) {
                hashes.push(hash.to_owned());
                return;
            }
            for child in map.values() {
                collect_refs(child, hashes);
            }
        }
        _ => {}
    }
}

fn body_ref_value(content_hash: &str) -> Value {
    Value::Object(serde_json::Map::from_iter([(
        BODY_REF_KEY.to_owned(),
        Value::String(content_hash.to_owned()),
    )]))
}

fn body_ref_hash(map: &serde_json::Map<String, Value>) -> Option<&str> {
    if map.len() != 1 {
        return None;
    }
    map.get(BODY_REF_KEY).and_then(Value::as_str)
}

fn deflate_if_smaller(bytes: &[u8]) -> Option<Vec<u8>> {
    let mut encoder = DeflateEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(bytes).ok()?;
    let compressed = encoder.finish().ok()?;
    (compressed.len() < bytes.len()).then_some(compressed)
}

fn inflate(blob: &[u8]) -> Option<Vec<u8>> {
    let mut decoder = DeflateDecoder::new(blob);
    let mut out = Vec::new();
    decoder.read_to_end(&mut out).ok()?;
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::HashMap;

    #[test]
    fn large_repeated_text_deflates_and_round_trips() {
        let text = "m".repeat(INLINE_BODY_BYTES + 8);
        let stored = StoredCanonicalBody::pack(text.as_bytes()).unwrap();
        assert_eq!(stored.encoding, ENCODING_DEFLATE);
        assert!(stored.blob.len() < text.len());
        assert_eq!(stored.unpack().unwrap(), text.as_bytes());
    }

    #[test]
    fn slim_replaces_only_large_strings() {
        let mut value = json!({
            "observation_id": "obs.1",
            "payload": {"text": "m".repeat(INLINE_BODY_BYTES + 1)},
        });
        let bodies = slim_json_value(&mut value).unwrap();
        assert_eq!(bodies.len(), 1);
        assert_eq!(value["observation_id"], "obs.1");
        assert_eq!(
            value["payload"]["text"][BODY_REF_KEY],
            bodies[0].content_hash
        );
    }

    #[test]
    fn hydrate_restores_the_original_string() {
        let text = "m".repeat(INLINE_BODY_BYTES + 1);
        let mut value = json!({"text": text});
        let bodies = slim_json_value(&mut value).unwrap();
        let lookup: HashMap<_, _> = bodies
            .iter()
            .map(|body| (body.content_hash.clone(), body.unpack().unwrap()))
            .collect();
        assert!(
            hydrate_json_value(&mut value, |hash| {
                lookup
                    .get(hash)
                    .cloned()
                    .ok_or(CanonicalBodyError::Missing {
                        content_hash: hash.to_owned(),
                    })
            })
            .unwrap()
        );
        assert_eq!(value["text"], text);
    }
}
