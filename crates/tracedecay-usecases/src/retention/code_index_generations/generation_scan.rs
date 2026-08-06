use std::fs::File;
use std::io::Read;
use std::path::Path;

use sha2::{Digest, Sha256};

use super::{
    CodeGenerationRetentionErrorV1, GenerationDigestVerificationV1,
    MAX_GENERATION_METADATA_PREFIX_BYTES, SealedGenerationManifestMetadataV1, storage,
};

pub(super) fn read_generation_metadata(
    path: &Path,
    verification: GenerationDigestVerificationV1,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<(u32, SealedGenerationManifestMetadataV1, String, u64), CodeGenerationRetentionErrorV1>
{
    let mut file = File::open(path).map_err(storage)?;
    let size_bytes = file.metadata().map_err(storage)?.len();
    let mut hasher = Sha256::new();
    let mut prefix = Vec::with_capacity(MAX_GENERATION_METADATA_PREFIX_BYTES);
    let mut buffer = vec![0u8; 1024 * 1024];
    loop {
        let bytes_read = file.read(&mut buffer).map_err(storage)?;
        if bytes_read == 0 {
            break;
        }
        if verification == GenerationDigestVerificationV1::Full {
            hasher.update(&buffer[..bytes_read]);
        }
        let remaining = MAX_GENERATION_METADATA_PREFIX_BYTES.saturating_sub(prefix.len());
        prefix.extend_from_slice(&buffer[..bytes_read.min(remaining)]);
        if is_cancelled() {
            return Err(CodeGenerationRetentionErrorV1::Cancelled);
        }
        if verification == GenerationDigestVerificationV1::MetadataOnly
            && prefix.len() >= MAX_GENERATION_METADATA_PREFIX_BYTES
        {
            break;
        }
    }
    let format_revision = parse_json_u32_field(&prefix, b"format_revision").ok_or_else(|| {
        CodeGenerationRetentionErrorV1::UnsafeState(format!(
            "generation file '{}' has no readable format revision in its metadata prefix",
            path.display()
        ))
    })?;
    let manifest_bytes = extract_json_object_field(&prefix, b"manifest").ok_or_else(|| {
        CodeGenerationRetentionErrorV1::UnsafeState(format!(
            "generation file '{}' has no complete manifest within its bounded metadata prefix",
            path.display()
        ))
    })?;
    let manifest = serde_json::from_slice(manifest_bytes).map_err(|error| {
        CodeGenerationRetentionErrorV1::UnsafeState(format!(
            "generation file '{}' has unreadable manifest metadata: {error}",
            path.display()
        ))
    })?;
    let state_digest = match verification {
        GenerationDigestVerificationV1::Full => {
            format!("sha256:{}", hex::encode(hasher.finalize()))
        }
        GenerationDigestVerificationV1::MetadataOnly => named_state_digest(path)?,
    };
    Ok((format_revision, manifest, state_digest, size_bytes))
}

fn named_state_digest(path: &Path) -> Result<String, CodeGenerationRetentionErrorV1> {
    let named = path
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_prefix("generation-"))
        .and_then(|name| name.strip_suffix(".json"))
        .filter(|digest| {
            digest.len() == 64
                && digest
                    .bytes()
                    .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
        })
        .ok_or_else(|| {
            CodeGenerationRetentionErrorV1::UnsafeState(format!(
                "generation file '{}' does not name a SHA-256 content digest",
                path.display()
            ))
        })?;
    Ok(format!("sha256:{named}"))
}

fn parse_json_u32_field(prefix: &[u8], field: &[u8]) -> Option<u32> {
    let start = json_field_value_start(prefix, field)?;
    let end = prefix[start..]
        .iter()
        .position(|byte| !byte.is_ascii_digit())
        .map_or(prefix.len(), |offset| start + offset);
    std::str::from_utf8(&prefix[start..end]).ok()?.parse().ok()
}

fn extract_json_object_field<'a>(prefix: &'a [u8], field: &[u8]) -> Option<&'a [u8]> {
    let start = json_field_value_start(prefix, field)?;
    if prefix.get(start) != Some(&b'{') {
        return None;
    }
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (offset, byte) in prefix[start..].iter().copied().enumerate() {
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            continue;
        }
        match byte {
            b'"' => in_string = true,
            b'{' => depth += 1,
            b'}' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(&prefix[start..=start + offset]);
                }
            }
            _ => {}
        }
    }
    None
}

fn json_field_value_start(prefix: &[u8], field: &[u8]) -> Option<usize> {
    let quoted = [b"\"".as_slice(), field, b"\"".as_slice()].concat();
    let key_start = prefix
        .windows(quoted.len())
        .position(|window| window == quoted)?;
    let mut cursor = key_start + quoted.len();
    while prefix.get(cursor).is_some_and(u8::is_ascii_whitespace) {
        cursor += 1;
    }
    if prefix.get(cursor) != Some(&b':') {
        return None;
    }
    cursor += 1;
    while prefix.get(cursor).is_some_and(u8::is_ascii_whitespace) {
        cursor += 1;
    }
    Some(cursor)
}
