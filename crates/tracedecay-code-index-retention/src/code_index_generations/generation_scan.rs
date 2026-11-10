use std::fs::File;
use std::io::Read;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use sha2::{Digest, Sha256};
use tracedecay_domain::canonical_text::{encode_tagged_lowercase_hex, is_lowercase_hex};

use super::{
    CodeGenerationRetentionErrorV1, GenerationDigestVerificationV1,
    MAX_GENERATION_METADATA_PREFIX_BYTES, SealedGenerationManifestMetadataV1, deferred_if_absent,
    storage,
};

const MAX_FORMAT_REVISION_PREFIX_BYTES: usize = 4 * 1024;
const GENERATION_METADATA_READ_CHUNK_BYTES: usize = 4 * 1024;

pub(super) fn read_generation_format_revision_from_file(
    file: &mut File,
    path: &Path,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<u32, CodeGenerationRetentionErrorV1> {
    if is_cancelled() {
        return Err(CodeGenerationRetentionErrorV1::Cancelled);
    }
    let mut prefix = Vec::with_capacity(MAX_FORMAT_REVISION_PREFIX_BYTES);
    file.take(MAX_FORMAT_REVISION_PREFIX_BYTES as u64)
        .read_to_end(&mut prefix)
        .map_err(storage)?;
    if is_cancelled() {
        return Err(CodeGenerationRetentionErrorV1::Cancelled);
    }
    parse_json_u32_field(&prefix, b"format_revision").ok_or_else(|| {
        CodeGenerationRetentionErrorV1::UnsafeState(format!(
            "generation file '{}' has no readable format revision in its bounded prefix",
            path.display()
        ))
    })
}

#[tracing::instrument(name = "usecases.retention.read_metadata", level = "trace", skip_all)]
pub(super) fn read_generation_metadata(
    path: &Path,
    verification: GenerationDigestVerificationV1,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<(u32, SealedGenerationManifestMetadataV1, String, u64), CodeGenerationRetentionErrorV1>
{
    if is_cancelled() {
        return Err(CodeGenerationRetentionErrorV1::Cancelled);
    }
    // Enumerating a content-addressed name does not authorize following an
    // alias to another generation. This metadata witness adds no pathname open.
    let named_before = path.symlink_metadata().map_err(deferred_if_absent)?;
    if !named_before.file_type().is_file() {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(format!(
            "generation file '{}' is not a regular file",
            path.display()
        )));
    }
    let mut file = File::open(path).map_err(deferred_if_absent)?;
    let opened_before = file.metadata().map_err(storage)?;
    #[cfg(unix)]
    if !same_generation_file_identity(&named_before, &opened_before) {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(format!(
            "generation file '{}' changed while opening",
            path.display()
        )));
    }
    let size_bytes = opened_before.len();
    let mut hasher = Sha256::new();
    let (format_revision, manifest) =
        read_generation_metadata_prefix(&mut file, path, verification, &mut hasher, is_cancelled)?;
    let state_digest = match verification {
        GenerationDigestVerificationV1::Full => {
            // The prefix reader hashed exactly the bytes it consumed from this
            // handle. Continue at that position: a small manifest must never
            // turn a full integrity check into a prefix-only digest.
            let mut buffer = vec![0u8; 1024 * 1024];
            loop {
                if is_cancelled() {
                    return Err(CodeGenerationRetentionErrorV1::Cancelled);
                }
                let bytes_read = file.read(&mut buffer).map_err(storage)?;
                if bytes_read == 0 {
                    break;
                }
                hasher.update(&buffer[..bytes_read]);
                if is_cancelled() {
                    return Err(CodeGenerationRetentionErrorV1::Cancelled);
                }
            }
            encode_tagged_lowercase_hex("sha256:", &hasher.finalize())
        }
        GenerationDigestVerificationV1::MetadataOnly => named_state_digest(path)?,
    };
    // The manifest, digest and reported size must belong to the pathname
    // enumerated by retention, not a replaced inode or modified old handle.
    // Metadata-only scans cannot fall back to trusting a substituted name.
    let named_after = path.symlink_metadata().map_err(deferred_if_absent)?;
    let opened_after = file.metadata().map_err(storage)?;
    #[cfg(unix)]
    let unchanged = same_generation_file_identity(&named_before, &named_after)
        && same_generation_file_identity(&named_before, &opened_after);
    #[cfg(not(unix))]
    let unchanged = named_after.file_type().is_file()
        && opened_after.is_file()
        && named_after.len() == size_bytes
        && opened_after.len() == size_bytes;
    if !unchanged {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(format!(
            "generation file '{}' changed during metadata read",
            path.display()
        )));
    }
    Ok((format_revision, manifest, state_digest, size_bytes))
}

#[cfg(unix)]
fn same_generation_file_identity(
    original: &std::fs::Metadata,
    current: &std::fs::Metadata,
) -> bool {
    original.file_type().is_file()
        && current.file_type().is_file()
        && original.dev() == current.dev()
        && original.ino() == current.ino()
        && original.len() == current.len()
        && original.mtime() == current.mtime()
        && original.mtime_nsec() == current.mtime_nsec()
        && original.ctime() == current.ctime()
        && original.ctime_nsec() == current.ctime_nsec()
}

/// Read only as far as complete metadata requires, up to the existing bound.
/// Geometrically spaced probes keep repeated prefix parsing linear in bytes
/// even when a manifest is large. Reads stay small and cancellable between
/// probes; the prefix allocation is released before full-payload hashing.
fn read_generation_metadata_prefix(
    file: &mut File,
    path: &Path,
    verification: GenerationDigestVerificationV1,
    hasher: &mut Sha256,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<(u32, SealedGenerationManifestMetadataV1), CodeGenerationRetentionErrorV1> {
    let mut prefix = Vec::with_capacity(GENERATION_METADATA_READ_CHUNK_BYTES);
    let mut buffer = [0u8; GENERATION_METADATA_READ_CHUNK_BYTES];
    let mut next_probe = GENERATION_METADATA_READ_CHUNK_BYTES;
    while prefix.len() < MAX_GENERATION_METADATA_PREFIX_BYTES {
        if is_cancelled() {
            return Err(CodeGenerationRetentionErrorV1::Cancelled);
        }
        let remaining = MAX_GENERATION_METADATA_PREFIX_BYTES - prefix.len();
        let read_limit = buffer.len().min(remaining).min(next_probe - prefix.len());
        let bytes_read = file.read(&mut buffer[..read_limit]).map_err(storage)?;
        if bytes_read == 0 {
            break;
        }
        if verification == GenerationDigestVerificationV1::Full {
            hasher.update(&buffer[..bytes_read]);
        }
        prefix.extend_from_slice(&buffer[..bytes_read]);
        if is_cancelled() {
            return Err(CodeGenerationRetentionErrorV1::Cancelled);
        }
        if prefix.len() >= next_probe {
            if parse_json_u32_field(&prefix, b"format_revision").is_some()
                && extract_json_object_field(&prefix, b"manifest").is_some()
            {
                break;
            }
            next_probe = next_probe
                .saturating_mul(2)
                .min(MAX_GENERATION_METADATA_PREFIX_BYTES);
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
    Ok((format_revision, manifest))
}

fn named_state_digest(path: &Path) -> Result<String, CodeGenerationRetentionErrorV1> {
    let named = path
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_prefix("generation-"))
        .and_then(|name| name.strip_suffix(".json"))
        .filter(|digest| is_lowercase_hex(digest, 64))
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
    // A prefix ending in digits is not yet a complete number. In particular,
    // do not accept the integer prefix of a fractional or exponent value.
    let terminator = prefix[end..]
        .iter()
        .copied()
        .find(|byte| !byte.is_ascii_whitespace())?;
    if !matches!(terminator, b',' | b'}') {
        return None;
    }
    serde_json::from_slice(&prefix[start..end]).ok()
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

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::fs::{self, File, OpenOptions};
    use std::io::{Seek, SeekFrom, Write};
    #[cfg(unix)]
    use std::os::unix::fs::MetadataExt;
    use std::path::{Path, PathBuf};

    use sha2::{Digest, Sha256};
    use tracedecay_code_index::production::SEALED_GENERATION_FORMAT_REVISION_V1;
    use tracedecay_domain::canonical_text::encode_tagged_lowercase_hex;

    use super::{
        CodeGenerationRetentionErrorV1, GENERATION_METADATA_READ_CHUNK_BYTES,
        GenerationDigestVerificationV1, MAX_GENERATION_METADATA_PREFIX_BYTES, parse_json_u32_field,
        read_generation_format_revision_from_file, read_generation_metadata,
        read_generation_metadata_prefix,
    };

    fn generation_bytes(manifest_padding: usize, payload_bytes: usize) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "format_revision": SEALED_GENERATION_FORMAT_REVISION_V1,
            "manifest": {
                "generation_id": "generation.v1.scan",
                "padding": "x".repeat(manifest_padding),
                "seal": { "sealed_at": 7 },
            },
            "payload": "x".repeat(payload_bytes),
        }))
        .unwrap()
    }

    fn write_generation(root: &Path, bytes: &[u8]) -> (PathBuf, String) {
        let digest = encode_tagged_lowercase_hex("sha256:", &Sha256::digest(bytes));
        let path = root.join(format!("generation-{}.json", &digest[7..]));
        fs::write(&path, bytes).unwrap();
        (path, digest)
    }

    #[test]
    fn metadata_only_stops_at_the_complete_manifest() {
        let root = tempfile::tempdir().unwrap();
        let bytes = generation_bytes(0, 256 * 1024);
        let (path, expected_digest) = write_generation(root.path(), &bytes);
        let mut revision_file = File::open(&path).unwrap();
        assert_eq!(
            read_generation_format_revision_from_file(&mut revision_file, &path, &|| false)
                .unwrap(),
            SEALED_GENERATION_FORMAT_REVISION_V1
        );
        drop(revision_file);
        let mut file = File::open(&path).unwrap();
        let (revision, manifest) = read_generation_metadata_prefix(
            &mut file,
            &path,
            GenerationDigestVerificationV1::MetadataOnly,
            &mut Sha256::new(),
            &|| false,
        )
        .unwrap();
        assert_eq!(revision, SEALED_GENERATION_FORMAT_REVISION_V1);
        assert_eq!(manifest.generation_id.as_str(), "generation.v1.scan");
        assert_eq!(manifest.seal.sealed_at.0, 7);
        assert_eq!(
            file.stream_position().unwrap(),
            GENERATION_METADATA_READ_CHUNK_BYTES as u64
        );
        let (_, _, digest, size) =
            read_generation_metadata(&path, GenerationDigestVerificationV1::MetadataOnly, &|| {
                false
            })
            .unwrap();
        assert_eq!(digest, expected_digest);
        assert_eq!(size, bytes.len() as u64);
    }

    #[test]
    fn full_verification_still_hashes_the_payload_after_the_manifest() {
        let root = tempfile::tempdir().unwrap();
        let bytes = generation_bytes(0, 2 * 1024 * 1024);
        let (path, named_digest) = write_generation(root.path(), &bytes);
        let (_, _, digest, size) =
            read_generation_metadata(&path, GenerationDigestVerificationV1::Full, &|| false)
                .unwrap();
        assert_eq!(digest, named_digest);
        assert_eq!(size, bytes.len() as u64);

        let mut file = OpenOptions::new().write(true).open(&path).unwrap();
        file.seek(SeekFrom::End(-3)).unwrap();
        file.write_all(b"y").unwrap();
        drop(file);
        let (_, _, changed_digest, _) =
            read_generation_metadata(&path, GenerationDigestVerificationV1::Full, &|| false)
                .unwrap();
        assert_ne!(changed_digest, named_digest);
        assert_eq!(
            changed_digest,
            encode_tagged_lowercase_hex("sha256:", &Sha256::digest(fs::read(&path).unwrap()))
        );
    }

    #[test]
    fn a_manifest_spanning_multiple_chunks_is_read_completely() {
        let root = tempfile::tempdir().unwrap();
        let bytes = generation_bytes(12 * 1024, 64 * 1024);
        let (path, _) = write_generation(root.path(), &bytes);
        let mut file = File::open(&path).unwrap();
        let (_, manifest) = read_generation_metadata_prefix(
            &mut file,
            &path,
            GenerationDigestVerificationV1::MetadataOnly,
            &mut Sha256::new(),
            &|| false,
        )
        .unwrap();
        assert_eq!(manifest.generation_id.as_str(), "generation.v1.scan");
        assert_eq!(manifest.seal.sealed_at.0, 7);
        assert_eq!(file.stream_position().unwrap(), 16 * 1024);
    }

    #[test]
    fn metadata_beyond_the_existing_prefix_bound_is_refused() {
        let root = tempfile::tempdir().unwrap();
        let mut bytes = vec![b' '; MAX_GENERATION_METADATA_PREFIX_BYTES];
        bytes.extend(generation_bytes(0, 0));
        let (path, _) = write_generation(root.path(), &bytes);
        let mut file = File::open(&path).unwrap();
        let result = read_generation_metadata_prefix(
            &mut file,
            &path,
            GenerationDigestVerificationV1::MetadataOnly,
            &mut Sha256::new(),
            &|| false,
        );
        assert!(matches!(
            result,
            Err(CodeGenerationRetentionErrorV1::UnsafeState(_))
        ));
        assert_eq!(
            file.stream_position().unwrap(),
            MAX_GENERATION_METADATA_PREFIX_BYTES as u64
        );
    }

    #[test]
    fn malformed_manifest_metadata_is_refused_in_both_modes() {
        let root = tempfile::tempdir().unwrap();
        let bytes = serde_json::to_vec(&serde_json::json!({
            "format_revision": SEALED_GENERATION_FORMAT_REVISION_V1,
            "manifest": { "generation_id": "generation.v1.scan" },
            "payload": "x".repeat(64 * 1024),
        }))
        .unwrap();
        let (path, _) = write_generation(root.path(), &bytes);
        for verification in [
            GenerationDigestVerificationV1::MetadataOnly,
            GenerationDigestVerificationV1::Full,
        ] {
            assert!(matches!(
                read_generation_metadata(&path, verification, &|| false),
                Err(CodeGenerationRetentionErrorV1::UnsafeState(_))
            ));
        }
    }

    #[test]
    fn metadata_reads_observe_cancellation_before_and_after_io() {
        let root = tempfile::tempdir().unwrap();
        let (path, _) = write_generation(root.path(), &generation_bytes(0, 64 * 1024));
        let mut file = File::open(&path).unwrap();
        let result = read_generation_metadata_prefix(
            &mut file,
            &path,
            GenerationDigestVerificationV1::MetadataOnly,
            &mut Sha256::new(),
            &|| true,
        );
        assert!(matches!(
            result,
            Err(CodeGenerationRetentionErrorV1::Cancelled)
        ));
        assert_eq!(file.stream_position().unwrap(), 0);

        let checks = Cell::new(0);
        let result = read_generation_metadata_prefix(
            &mut file,
            &path,
            GenerationDigestVerificationV1::MetadataOnly,
            &mut Sha256::new(),
            &|| {
                let previous = checks.get();
                checks.set(previous + 1);
                previous > 0
            },
        );
        assert!(matches!(
            result,
            Err(CodeGenerationRetentionErrorV1::Cancelled)
        ));
        assert_eq!(
            file.stream_position().unwrap(),
            GENERATION_METADATA_READ_CHUNK_BYTES as u64
        );
    }

    #[cfg(unix)]
    #[test]
    fn generation_metadata_refuses_symlink_aliases_in_both_modes() {
        let root = tempfile::tempdir().unwrap();
        let bytes = generation_bytes(0, 32 * 1024);
        let (target, _) = write_generation(root.path(), &bytes);
        let alias = root.path().join(format!("generation-{}.json", "a".repeat(64)));
        std::os::unix::fs::symlink(&target, &alias).unwrap();
        assert_eq!(fs::read(&alias).unwrap(), bytes, "alias resolves to valid bytes");

        for mode in [
            GenerationDigestVerificationV1::MetadataOnly,
            GenerationDigestVerificationV1::Full,
        ] {
            assert!(matches!(
                read_generation_metadata(&alias, mode, &|| false),
                Err(CodeGenerationRetentionErrorV1::UnsafeState(_))
            ));
        }
        let (_, _, digest, size) =
            read_generation_metadata(&target, GenerationDigestVerificationV1::Full, &|| false)
                .unwrap();
        assert_eq!(digest, encode_tagged_lowercase_hex("sha256:", &Sha256::digest(&bytes)));
        assert_eq!(size, bytes.len() as u64);
    }

    #[cfg(unix)]
    #[test]
    fn generation_metadata_refuses_same_byte_inode_rotation_during_read() {
        for mode in [
            GenerationDigestVerificationV1::MetadataOnly,
            GenerationDigestVerificationV1::Full,
        ] {
            let root = tempfile::tempdir().unwrap();
            let bytes = generation_bytes(0, 64 * 1024);
            let (path, _) = write_generation(root.path(), &bytes);
            let substitute = root.path().join("substitute.tmp");
            fs::write(&substitute, &bytes).unwrap();
            let original = fs::metadata(&path).unwrap();
            let foreign = fs::metadata(&substitute).unwrap();
            assert_ne!(
                (original.dev(), original.ino()),
                (foreign.dev(), foreign.ino()),
                "the replacement must use a different real inode"
            );
            let probes = Cell::new(0_usize);
            let result = read_generation_metadata(&path, mode, &|| {
                let previous = probes.get();
                probes.set(previous + 1);
                // The third cancellation checkpoint is after reading the
                // first nonempty prefix from the already-open descriptor.
                if previous == 2 {
                    fs::rename(&substitute, &path).unwrap();
                }
                false
            });
            assert!(probes.get() >= 3, "the real reader must cross the swap");
            assert!(matches!(
                result,
                Err(CodeGenerationRetentionErrorV1::UnsafeState(_))
            ));
            assert_eq!(fs::read(&path).unwrap(), bytes);
        }
    }

    #[test]
    fn format_revision_requires_a_complete_unsigned_json_integer() {
        for input in [
            r#"{"format_revision":21,"manifest":{}}"#,
            r#"{"format_revision":21 }"#,
        ] {
            assert_eq!(
                parse_json_u32_field(input.as_bytes(), b"format_revision"),
                Some(21)
            );
        }
        for input in [
            r#"{"format_revision":21"#,
            r#"{"format_revision":21.5}"#,
            r#"{"format_revision":21e1}"#,
            r#"{"format_revision":021}"#,
            r#"{"format_revision":-1}"#,
            r#"{"format_revision":"21"}"#,
            r#"{"format_revision":4294967296}"#,
        ] {
            assert_eq!(
                parse_json_u32_field(input.as_bytes(), b"format_revision"),
                None
            );
        }
    }
}
