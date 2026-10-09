//! Content-addressed session bodies.
//!
//! Observation JSON and LCM raw rows used to each store the full canonical
//! text. Large strings live once in `session_canonical_bodies`; the row keeps
//! a hash ref. Bodies at or above [`INLINE_BODY_BYTES`] are deflated when that
//! shrinks them.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};

use flate2::Compression;
use flate2::read::DeflateDecoder;
use flate2::write::DeflateEncoder;
use serde_json::Value;
use thiserror::Error;
use tracedecay_domain::canonical_text::sha256_hex;
use tracedecay_domain::{DurableObservationV1, ObservationContractError};

pub const INLINE_BODY_BYTES: usize = 4096;
// DurableObservationV1 never serializes this root field. Provider-controlled
// JSON stays under payload and cannot be mistaken for storage metadata.
pub const BODY_REF_KEY: &str = "_tracedecay_canonical_body_refs";
const LEGACY_BODY_REF_KEY: &str = "tracedecay.body_ref";
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

pub const LOAD_CANONICAL_BODY_SQL: &str = "SELECT encoding, body, uncompressed_bytes
         FROM session_canonical_bodies WHERE content_hash = ?1";

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
        unpack_body(
            &self.content_hash,
            self.encoding,
            &self.blob,
            self.uncompressed_bytes,
        )
    }
}

/// Reverses [`StoredCanonicalBody::pack`]: inflates when the row deflates,
/// then proves the bytes by their declared size and content hash. The
/// `uncompressed_bytes` bound keeps a corrupt or hostile row from inflating
/// past the size its writer recorded before either check runs.
pub fn unpack_body(
    content_hash: &str,
    encoding: &str,
    blob: &[u8],
    uncompressed_bytes: i64,
) -> Result<Vec<u8>, CanonicalBodyError> {
    let declared =
        usize::try_from(uncompressed_bytes).map_err(|_| CanonicalBodyError::Corrupt {
            content_hash: content_hash.to_owned(),
            encoding: encoding.to_owned(),
        })?;
    let bytes = match encoding {
        ENCODING_IDENTITY => blob.to_vec(),
        ENCODING_DEFLATE => inflate(blob, declared).ok_or_else(|| CanonicalBodyError::Corrupt {
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
    if bytes.len() != declared || sha256_hex(&bytes) != content_hash {
        return Err(CanonicalBodyError::Corrupt {
            content_hash: content_hash.to_owned(),
            encoding: encoding.to_owned(),
        });
    }
    Ok(bytes)
}

pub fn stored_json_needs_hydrate(json: &str) -> bool {
    json.contains(BODY_REF_KEY) || json.contains(LEGACY_BODY_REF_KEY)
}

pub fn collect_body_refs(json: &str) -> Result<Vec<String>, CanonicalBodyError> {
    if !stored_json_needs_hydrate(json) {
        return Ok(Vec::new());
    }
    let value: Value = serde_json::from_str(json)?;
    Ok(body_refs(&value)?
        .into_values()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect())
}

fn body_refs(value: &Value) -> Result<BTreeMap<String, String>, CanonicalBodyError> {
    match value.as_object().and_then(|map| map.get(BODY_REF_KEY)) {
        None => {
            let mut refs = BTreeMap::new();
            collect_legacy_refs(value, "", &mut refs);
            Ok(refs)
        }
        Some(refs) => serde_json::from_value(refs.clone()).map_err(Into::into),
    }
}

pub fn slim_json_value(value: &mut Value) -> Result<Vec<StoredCanonicalBody>, CanonicalBodyError> {
    let map = value
        .as_object_mut()
        .ok_or(CanonicalBodyError::InvalidJson)?;
    if map.contains_key(BODY_REF_KEY) {
        body_refs(value)?;
        return Ok(Vec::new());
    }
    let mut bodies = Vec::new();
    let mut refs = BTreeMap::new();
    slim_value(value, "", &mut bodies, &mut refs)?;
    value
        .as_object_mut()
        .ok_or(CanonicalBodyError::InvalidJson)?
        .insert(BODY_REF_KEY.to_owned(), serde_json::to_value(refs)?);
    Ok(bodies)
}

pub fn hydrate_json_value<E>(
    value: &mut Value,
    mut load: impl FnMut(&str) -> Result<Vec<u8>, E>,
) -> Result<bool, E>
where
    E: From<CanonicalBodyError>,
{
    let refs = body_refs(value).map_err(E::from)?;
    let has_refs = !refs.is_empty();
    let root_format = value.get(BODY_REF_KEY).is_some();
    for (path, hash) in refs {
        let target = value
            .pointer_mut(&path)
            .filter(|target| {
                if root_format {
                    target.is_null()
                } else {
                    legacy_ref(target) == Some(hash.as_str())
                }
            })
            .ok_or_else(|| E::from(CanonicalBodyError::InvalidJson))?;
        let bytes = load(&hash)?;
        let text = String::from_utf8(bytes).map_err(|_| {
            E::from(CanonicalBodyError::Corrupt {
                content_hash: hash,
                encoding: ENCODING_IDENTITY.to_owned(),
            })
        })?;
        *target = Value::String(text);
    }
    value
        .as_object_mut()
        .ok_or_else(|| E::from(CanonicalBodyError::InvalidJson))?
        .remove(BODY_REF_KEY);
    Ok(has_refs)
}

fn legacy_ref(value: &Value) -> Option<&str> {
    let map = value.as_object()?;
    (map.len() == 1)
        .then(|| map.get(LEGACY_BODY_REF_KEY).and_then(Value::as_str))
        .flatten()
}

fn collect_legacy_refs(value: &Value, path: &str, refs: &mut BTreeMap<String, String>) {
    if let Some(hash) = legacy_ref(value) {
        refs.insert(path.to_owned(), hash.to_owned());
        return;
    }
    match value {
        Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                collect_legacy_refs(item, &format!("{path}/{index}"), refs);
            }
        }
        Value::Object(map) => {
            for (key, child) in map {
                let token = key.replace('~', "~0").replace('/', "~1");
                collect_legacy_refs(child, &format!("{path}/{token}"), refs);
            }
        }
        _ => {}
    }
}

/// Upgrades the nested reference encoding shipped before root reference maps.
/// New observations must use `slim_stored_json` so provider objects are never
/// interpreted as references from the old storage format.
pub fn migrate_stored_json(
    json: &str,
) -> Result<(String, Vec<StoredCanonicalBody>), CanonicalBodyError> {
    let mut value: Value = serde_json::from_str(json)?;
    if value.get(BODY_REF_KEY).is_some() {
        body_refs(&value)?;
        return Ok((json.to_owned(), Vec::new()));
    }
    let mut refs = body_refs(&value)?;
    for path in refs.keys() {
        *value
            .pointer_mut(path)
            .ok_or(CanonicalBodyError::InvalidJson)? = Value::Null;
    }
    let mut bodies = Vec::new();
    slim_value(&mut value, "", &mut bodies, &mut refs)?;
    if refs.is_empty() {
        return Ok((json.to_owned(), bodies));
    }
    value
        .as_object_mut()
        .ok_or(CanonicalBodyError::InvalidJson)?
        .insert(BODY_REF_KEY.to_owned(), serde_json::to_value(refs)?);
    Ok((value.to_string(), bodies))
}

pub fn slim_stored_json(
    json: &str,
) -> Result<(String, Vec<StoredCanonicalBody>), CanonicalBodyError> {
    let mut value: Value = serde_json::from_str(json)?;
    let bodies = slim_json_value(&mut value)?;
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
        serde_json::from_str(json).map_err(|error| E::from(CanonicalBodyError::from(error)))?;
    hydrate_json_value(&mut value, &mut load)?;
    serde_json::from_value(value).map_err(|error| E::from(error.into()))
}

fn slim_value(
    value: &mut Value,
    path: &str,
    bodies: &mut Vec<StoredCanonicalBody>,
    refs: &mut BTreeMap<String, String>,
) -> Result<(), CanonicalBodyError> {
    match value {
        Value::String(text) if text.len() >= INLINE_BODY_BYTES => {
            let body = StoredCanonicalBody::pack(text.as_bytes())?;
            refs.insert(path.to_owned(), body.content_hash.clone());
            *value = Value::Null;
            bodies.push(body);
        }
        Value::Array(items) => {
            for (index, item) in items.iter_mut().enumerate() {
                slim_value(item, &format!("{path}/{index}"), bodies, refs)?;
            }
        }
        Value::Object(map) => {
            for (key, child) in map {
                let token = key.replace('~', "~0").replace('/', "~1");
                slim_value(child, &format!("{path}/{token}"), bodies, refs)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn deflate_if_smaller(bytes: &[u8]) -> Option<Vec<u8>> {
    let mut encoder = DeflateEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(bytes).ok()?;
    let compressed = encoder.finish().ok()?;
    (compressed.len() < bytes.len()).then_some(compressed)
}

fn inflate(blob: &[u8], max_bytes: usize) -> Option<Vec<u8>> {
    let mut decoder =
        DeflateDecoder::new(blob).take(u64::try_from(max_bytes).ok()?.saturating_add(1));
    let mut out = Vec::new();
    decoder.read_to_end(&mut out).ok()?;
    (out.len() <= max_bytes).then_some(out)
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
        assert_eq!(value[BODY_REF_KEY]["/payload/text"], bodies[0].content_hash);
    }

    #[test]
    fn storage_refs_do_not_interpret_provider_objects_and_escape_json_paths() {
        let original = json!({
            "payload": {
                "metadata": {"tracedecay.body_ref": "f".repeat(64)},
                "reserved": {(BODY_REF_KEY): {"/not/a/storage/path": "metadata"}},
                "slash/key~": ["large".repeat(INLINE_BODY_BYTES)],
            },
        });
        let mut stored = original.clone();
        let bodies = slim_json_value(&mut stored).unwrap();
        assert_eq!(bodies.len(), 1);
        assert_eq!(
            collect_body_refs(&stored.to_string()).unwrap(),
            vec![bodies[0].content_hash.clone()]
        );
        hydrate_json_value(&mut stored, |hash| {
            assert_eq!(hash, bodies[0].content_hash);
            bodies[0].unpack()
        })
        .unwrap();
        assert_eq!(stored, original);
    }

    #[test]
    fn small_provider_reference_objects_are_not_storage_references() {
        let original = json!({"payload": {(LEGACY_BODY_REF_KEY): "provider-data"}});
        let (encoded, bodies) = slim_stored_json(&original.to_string()).unwrap();
        assert!(bodies.is_empty());
        assert!(collect_body_refs(&encoded).unwrap().is_empty());
        let mut stored: Value = serde_json::from_str(&encoded).unwrap();
        assert!(
            !hydrate_json_value(&mut stored, |_| -> Result<Vec<u8>, CanonicalBodyError> {
                panic!("provider metadata must not load canonical storage")
            })
            .unwrap()
        );
        assert_eq!(stored, original);
    }

    #[test]
    fn shipped_nested_references_hydrate_and_migrate_idempotently() {
        let body = StoredCanonicalBody::pack(b"previously stored body").unwrap();
        let legacy =
            json!({"payload": {"slash/key~": [{(LEGACY_BODY_REF_KEY): body.content_hash}]}});
        let expected = json!({"payload": {"slash/key~": ["previously stored body"]}});
        let mut hydrated = legacy.clone();
        hydrate_json_value(&mut hydrated, |hash| {
            assert_eq!(hash, body.content_hash);
            body.unpack()
        })
        .unwrap();
        assert_eq!(hydrated, expected);
        let (encoded, bodies) = migrate_stored_json(&legacy.to_string()).unwrap();
        assert!(
            bodies.is_empty(),
            "migration reuses existing canonical bytes"
        );
        assert_eq!(
            collect_body_refs(&encoded).unwrap(),
            vec![body.content_hash.clone()]
        );
        let mut migrated: Value = serde_json::from_str(&encoded).unwrap();
        assert_eq!(
            migrated[BODY_REF_KEY]["/payload/slash~1key~0/0"],
            body.content_hash
        );
        hydrate_json_value(&mut migrated, |_| body.unpack()).unwrap();
        assert_eq!(migrated, expected);
        assert_eq!(
            migrate_stored_json(&encoded).unwrap(),
            (encoded, Vec::new())
        );
    }

    #[test]
    fn shipped_nested_references_preserve_missing_and_corrupt_errors() {
        let legacy = json!({"payload": {(LEGACY_BODY_REF_KEY): "missing"}});
        let missing = hydrate_json_value(&mut legacy.clone(), |hash| {
            Err::<Vec<u8>, _>(CanonicalBodyError::Missing {
                content_hash: hash.to_owned(),
            })
        });
        assert!(matches!(missing, Err(CanonicalBodyError::Missing { .. })));
        let corrupt = hydrate_json_value(&mut legacy.clone(), |hash| {
            unpack_body(hash, ENCODING_IDENTITY, b"wrong bytes", 11)
        });
        assert!(matches!(corrupt, Err(CanonicalBodyError::Corrupt { .. })));
    }

    #[test]
    fn invalid_reference_paths_are_rejected_without_loading_a_body() {
        let mut value = json!({(BODY_REF_KEY): {"/missing": "hash"}});
        let result = hydrate_json_value(&mut value, |_| -> Result<Vec<u8>, CanonicalBodyError> {
            panic!("an invalid pointer must not read body storage")
        });
        assert!(matches!(result, Err(CanonicalBodyError::InvalidJson)));
    }

    #[test]
    fn unpack_rejects_a_declared_size_mismatch() {
        let bytes = "inflate source ".repeat(64).into_bytes();
        let stored = StoredCanonicalBody::pack(&bytes).unwrap();
        for declared in [
            stored.uncompressed_bytes - 1,
            stored.uncompressed_bytes + 1,
            i64::MAX,
            -1,
        ] {
            assert!(
                matches!(
                    unpack_body(
                        &stored.content_hash,
                        stored.encoding,
                        &stored.blob,
                        declared
                    ),
                    Err(CanonicalBodyError::Corrupt { .. })
                ),
                "declared {declared} must not unpack {encoding}",
                encoding = stored.encoding,
            );
        }
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
