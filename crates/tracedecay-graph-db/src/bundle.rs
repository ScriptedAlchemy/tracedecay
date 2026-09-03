//! Sealed read bundle: derived read artifacts written at seal, next to the
//! sealed generation they were derived from.
//!
//! A sealed generation is immutable, yet historically every read structure
//! (the interactive catalog, pagination indexes) was re-derived at open. The
//! bundle moves that derivation to seal time: seal writes each derived
//! artifact once, content-addressed and bound to the generation identity, and
//! open loads it instead of re-scanning the projection.
//!
//! Binding uses the one existing identity authority: the manifest identity
//! frames of the recovered-generation digest
//! ([`crate::generation`]'s `write_generation_identity_frames`). The bundle
//! manifest records that identity digest plus one `(name, content digest,
//! byte length)` row per artifact. Artifacts are independently optional — a
//! reader asks for the artifact it needs by name and treats the others as
//! absent without error — so bundles written before a new artifact existed
//! stay serveable.
//!
//! A missing or mismatched artifact is a TYPED state
//! ([`SealedReadBundleArtifactStateV1::Absent`] /
//! [`SealedReadBundleArtifactStateV1::Stale`]), never a silent fallback:
//! the caller decides to re-derive and says so. Bundle files retire with
//! their generation through the existing generation retirement path.

use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracedecay_domain::canonical_text::encode_lowercase_hex;

use crate::generation::generation_identity_frames_digest;
use crate::{GraphDbError, GraphGenerationManifestIdentity, SealedGraphStateDigest};

/// Format marker of the bundle manifest file.
pub const SEALED_READ_BUNDLE_FORMAT_V1: &str = "tracedecay.sealed-read-bundle.v1";

/// Upper bound of one bundle artifact's byte length. Matches the durable
/// sealed-generation ceiling: a derived read structure can never legitimately
/// exceed the generation it was derived from.
pub const MAX_SEALED_READ_BUNDLE_ARTIFACT_BYTES_V1: u64 = 8 * 1024 * 1024 * 1024;

const MAX_SEALED_READ_BUNDLE_ARTIFACTS_V1: usize = 64;
const MAX_SEALED_READ_BUNDLE_MANIFEST_BYTES_V1: u64 = 1024 * 1024;
const IO_CHUNK_BYTES: usize = 1024 * 1024;

/// Distinguishes concurrent or retried staging attempts that share a PID so
/// an aborted catalog write cannot collide with the next attempt's temporary.
static BUNDLE_TMP_SEQ: AtomicU64 = AtomicU64::new(1);

/// One derived artifact row of a sealed read bundle manifest.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SealedReadBundleArtifactV1 {
    /// Artifact name (lowercase alphanumeric and `-`), e.g.
    /// `interactive-catalog`.
    pub name: String,
    /// `sha256:<hex>` digest of the artifact bytes.
    pub digest: String,
    /// Exact byte length of the artifact file.
    pub bytes: u64,
}

/// The durable manifest of one generation's sealed read bundle.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SealedReadBundleManifestV1 {
    /// Always [`SEALED_READ_BUNDLE_FORMAT_V1`].
    pub format: String,
    /// The sealed source state digest this bundle is filed under
    /// (`sha256:<hex>`, the same digest that names the sealed generation
    /// file).
    pub sealed_state_digest: String,
    /// `sha256:<hex>` over the generation identity frames — projection,
    /// generation, source generation, watermark, dependencies — in the exact
    /// canonical encoding of the recovered-generation digest.
    pub generation_identity_digest: String,
    /// Derived artifacts, each independently optional for readers.
    pub artifacts: Vec<SealedReadBundleArtifactV1>,
}

/// Typed load state of one bundle artifact. `Absent` and `Stale` are states,
/// not errors: they name why the caller is about to re-derive, and the caller
/// must log that fallback rather than take it silently.
#[derive(Debug)]
pub enum SealedReadBundleArtifactStateV1 {
    /// The artifact was found, its digest and length verified against the
    /// manifest row, and the manifest verified against the generation
    /// identity.
    Loaded {
        artifact: SealedReadBundleArtifactV1,
        bytes: Vec<u8>,
    },
    /// No bundle manifest exists for this sealed generation, or the bundle
    /// exists but never wrote an artifact of this name.
    Absent { reason: String },
    /// A bundle exists but cannot be trusted: corrupt manifest, an identity
    /// digest bound to a different generation, or artifact bytes that no
    /// longer match their recorded digest or length.
    Stale { detail: String },
}

/// Stages artifact files for one generation's bundle, then commits them with
/// an identity-bound manifest. Dropping the writer without committing removes
/// every staged temporary file, including a write that aborted before the
/// artifact was recorded in `staged`.
pub struct SealedReadBundleWriterV1 {
    root: PathBuf,
    sealed: SealedGraphStateDigest,
    staged: Vec<(SealedReadBundleArtifactV1, PathBuf)>,
    pending: Option<PathBuf>,
    committed: bool,
}

impl SealedReadBundleWriterV1 {
    pub fn create(root: &Path, sealed: &SealedGraphStateDigest) -> Result<Self, GraphDbError> {
        if !root.is_dir() {
            return Err(GraphDbError::unavailable(
                "sealed read bundle root is not a directory",
            ));
        }
        sweep_aborted_sealed_read_bundle_temporaries(root, sealed)?;
        Ok(Self {
            root: root.to_path_buf(),
            sealed: sealed.clone(),
            staged: Vec::new(),
            pending: None,
            committed: false,
        })
    }

    fn temporary_path(&self, name: &str) -> Result<PathBuf, GraphDbError> {
        Ok(bundle_tmp_path(
            &self.root,
            &sealed_hex(&self.sealed)?,
            name,
        ))
    }

    fn abort_pending(&mut self) {
        if let Some(pending) = self.pending.take() {
            let _ = std::fs::remove_file(pending);
        }
    }

    /// Streams one artifact through `write` into a durable temporary file,
    /// hashing and counting as it goes. The artifact becomes part of the
    /// bundle only when [`Self::commit`] succeeds.
    pub fn stage_artifact(
        &mut self,
        name: &str,
        write: &mut dyn FnMut(&mut dyn Write) -> Result<(), GraphDbError>,
    ) -> Result<(), GraphDbError> {
        validate_artifact_name(name)?;
        if self.staged.len() >= MAX_SEALED_READ_BUNDLE_ARTIFACTS_V1 {
            return Err(GraphDbError::invalid(
                "sealed read bundle artifact count exceeds its bound",
            ));
        }
        if self
            .staged
            .iter()
            .any(|(artifact, _)| artifact.name == name)
        {
            return Err(GraphDbError::invalid(
                "sealed read bundle artifact name is already staged",
            ));
        }
        let temporary = self.temporary_path(name)?;
        self.pending = Some(temporary.clone());
        // Isolate `?` / `return` so a write abort cannot skip pending cleanup.
        let result = (|| {
            hotpath::measure_block!("graph_db.bundle.write", {
                let file = std::fs::OpenOptions::new()
                    .create_new(true)
                    .write(true)
                    .open(&temporary)
                    .map_err(|error| {
                        GraphDbError::unavailable(format!(
                            "failed to create sealed read bundle artifact stage: {error}"
                        ))
                    })?;
                let mut hashing = HashingFileWriter {
                    file,
                    digest: Sha256::new(),
                    bytes: 0,
                    failure: None,
                };
                // Derivations emit token-sized writes (serde_json streams a
                // few bytes per call); coalesce them so the hash and the file
                // see IO_CHUNK_BYTES-sized chunks instead of one syscall and
                // one Sha256 update per token.
                {
                    let mut buffered = io::BufWriter::with_capacity(IO_CHUNK_BYTES, &mut hashing);
                    write(&mut buffered)?;
                    buffered.flush().map_err(|error| {
                        GraphDbError::unavailable(format!(
                            "failed to flush sealed read bundle artifact stage: {error}"
                        ))
                    })?;
                }
                if let Some(failure) = hashing.failure.take() {
                    return Err(failure);
                }
                hashing.file.sync_all().map_err(|error| {
                    GraphDbError::unavailable(format!(
                        "failed to sync sealed read bundle artifact: {error}"
                    ))
                })?;
                Ok(SealedReadBundleArtifactV1 {
                    name: name.to_owned(),
                    digest: format!(
                        "sha256:{}",
                        encode_lowercase_hex(&hashing.digest.finalize())
                    ),
                    bytes: hashing.bytes,
                })
            })
        })();
        let artifact = match result {
            Ok(artifact) => artifact,
            Err(error) => {
                self.abort_pending();
                return Err(error);
            }
        };
        hotpath::gauge!("graph_db.bundle.write.bytes").set(artifact.bytes as f64);
        self.pending = None;
        self.staged.push((artifact, temporary));
        Ok(())
    }

    /// Renames every staged artifact into place, then writes the manifest
    /// bound to `identity` and syncs the directory. The manifest write is the
    /// commit point: without a manifest the artifacts are invisible.
    pub fn commit(
        mut self,
        identity: &GraphGenerationManifestIdentity,
        check: &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<SealedReadBundleManifestV1, GraphDbError> {
        check()?;
        let hex = sealed_hex(&self.sealed)?;
        let identity_digest = generation_identity_frames_digest(identity, check)?;
        let mut artifacts = Vec::with_capacity(self.staged.len());
        for (artifact, temporary) in std::mem::take(&mut self.staged) {
            let target = self.root.join(artifact_file_name(&hex, &artifact.name));
            remove_stale_regular_file(&target)?;
            std::fs::rename(&temporary, &target).map_err(|error| {
                GraphDbError::unavailable(format!(
                    "failed to place sealed read bundle artifact: {error}"
                ))
            })?;
            artifacts.push(artifact);
        }
        artifacts.sort_by(|left, right| left.name.cmp(&right.name));
        let manifest = SealedReadBundleManifestV1 {
            format: SEALED_READ_BUNDLE_FORMAT_V1.to_owned(),
            sealed_state_digest: self.sealed.as_str().to_owned(),
            generation_identity_digest: format!("sha256:{identity_digest}"),
            artifacts,
        };
        let encoded = serde_json::to_vec(&manifest).map_err(|error| {
            GraphDbError::invalid(format!(
                "failed to encode sealed read bundle manifest: {error}"
            ))
        })?;
        let temporary = bundle_tmp_path(&self.root, &hex, "manifest");
        self.pending = Some(temporary.clone());
        let write_manifest = || -> Result<(), GraphDbError> {
            let mut file = std::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&temporary)
                .map_err(|error| {
                    GraphDbError::unavailable(format!(
                        "failed to create sealed read bundle manifest stage: {error}"
                    ))
                })?;
            file.write_all(&encoded)
                .and_then(|()| file.sync_all())
                .map_err(|error| {
                    GraphDbError::unavailable(format!(
                        "failed to write sealed read bundle manifest: {error}"
                    ))
                })
        };
        if let Err(error) = write_manifest() {
            self.abort_pending();
            self.remove_committed_artifacts(&hex, &manifest);
            return Err(error);
        }
        if let Err(error) = std::fs::rename(&temporary, self.root.join(manifest_file_name(&hex))) {
            self.abort_pending();
            self.remove_committed_artifacts(&hex, &manifest);
            return Err(GraphDbError::unavailable(format!(
                "failed to place sealed read bundle manifest: {error}"
            )));
        }
        self.pending = None;
        sync_bundle_directory(&self.root)?;
        self.committed = true;
        Ok(manifest)
    }

    fn remove_committed_artifacts(&self, hex: &str, manifest: &SealedReadBundleManifestV1) {
        for artifact in &manifest.artifacts {
            let _ = std::fs::remove_file(self.root.join(artifact_file_name(hex, &artifact.name)));
        }
    }
}

impl Drop for SealedReadBundleWriterV1 {
    fn drop(&mut self) {
        self.abort_pending();
        if self.committed {
            return;
        }
        for (_, temporary) in &self.staged {
            let _ = std::fs::remove_file(temporary);
        }
    }
}

/// Loads one named artifact of the generation's sealed read bundle, verifying
/// the manifest's identity binding and the artifact's content digest before
/// any byte is trusted.
pub fn load_sealed_read_bundle_artifact(
    root: &Path,
    sealed: &SealedGraphStateDigest,
    identity: &GraphGenerationManifestIdentity,
    name: &str,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<SealedReadBundleArtifactStateV1, GraphDbError> {
    validate_artifact_name(name)?;
    check()?;
    hotpath::measure_block!("graph_db.bundle.load", {
        let hex = sealed_hex(sealed)?;
        let manifest_path = root.join(manifest_file_name(&hex));
        let manifest_bytes = match std::fs::metadata(&manifest_path) {
            Ok(metadata) if !metadata.is_file() => {
                return Ok(SealedReadBundleArtifactStateV1::Stale {
                    detail: "sealed read bundle manifest path is not a regular file".to_owned(),
                });
            }
            Ok(metadata) if metadata.len() > MAX_SEALED_READ_BUNDLE_MANIFEST_BYTES_V1 => {
                return Ok(SealedReadBundleArtifactStateV1::Stale {
                    detail: "sealed read bundle manifest exceeds its byte bound".to_owned(),
                });
            }
            Ok(_) => std::fs::read(&manifest_path).map_err(|error| {
                GraphDbError::unavailable(format!(
                    "failed to read sealed read bundle manifest: {error}"
                ))
            })?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(SealedReadBundleArtifactStateV1::Absent {
                    reason: "no sealed read bundle exists for this generation".to_owned(),
                });
            }
            Err(error) => {
                return Err(GraphDbError::unavailable(format!(
                    "failed to stat sealed read bundle manifest: {error}"
                )));
            }
        };
        let manifest: SealedReadBundleManifestV1 = match serde_json::from_slice(&manifest_bytes) {
            Ok(manifest) => manifest,
            Err(error) => {
                return Ok(SealedReadBundleArtifactStateV1::Stale {
                    detail: format!("sealed read bundle manifest is corrupt: {error}"),
                });
            }
        };
        let verified = hotpath::measure_block!("graph_db.bundle.verify", {
            verify_manifest_binding(&manifest, sealed, identity, check)
        })?;
        if let Some(detail) = verified {
            return Ok(SealedReadBundleArtifactStateV1::Stale { detail });
        }
        let Some(artifact) = manifest
            .artifacts
            .iter()
            .find(|artifact| artifact.name == name)
            .cloned()
        else {
            return Ok(SealedReadBundleArtifactStateV1::Absent {
                reason: format!("sealed read bundle has no `{name}` artifact"),
            });
        };
        if artifact.bytes > MAX_SEALED_READ_BUNDLE_ARTIFACT_BYTES_V1 {
            return Ok(SealedReadBundleArtifactStateV1::Stale {
                detail: "sealed read bundle artifact exceeds its byte bound".to_owned(),
            });
        }
        let path = root.join(artifact_file_name(&hex, &artifact.name));
        let file = match std::fs::File::open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(SealedReadBundleArtifactStateV1::Stale {
                    detail: format!("sealed read bundle artifact `{name}` file is missing"),
                });
            }
            Err(error) => {
                return Err(GraphDbError::unavailable(format!(
                    "failed to open sealed read bundle artifact: {error}"
                )));
            }
        };
        let expected_bytes = usize::try_from(artifact.bytes).map_err(|_| {
            GraphDbError::invalid("sealed read bundle artifact byte length overflows")
        })?;
        let mut bytes = Vec::new();
        if bytes.try_reserve_exact(expected_bytes).is_err() {
            return Err(GraphDbError::unavailable(
                "sealed read bundle artifact does not fit in memory",
            ));
        }
        let mut digest = Sha256::new();
        let mut reader = file.take(artifact.bytes.saturating_add(1));
        let mut chunk = vec![0_u8; IO_CHUNK_BYTES];
        loop {
            check()?;
            let read = reader.read(&mut chunk).map_err(|error| {
                GraphDbError::unavailable(format!(
                    "failed to read sealed read bundle artifact: {error}"
                ))
            })?;
            if read == 0 {
                break;
            }
            if bytes.len() + read > expected_bytes {
                return Ok(SealedReadBundleArtifactStateV1::Stale {
                    detail: format!(
                        "sealed read bundle artifact `{name}` is longer than its manifest row"
                    ),
                });
            }
            digest.update(&chunk[..read]);
            bytes.extend_from_slice(&chunk[..read]);
        }
        if bytes.len() != expected_bytes {
            return Ok(SealedReadBundleArtifactStateV1::Stale {
                detail: format!(
                    "sealed read bundle artifact `{name}` is shorter than its manifest row"
                ),
            });
        }
        let actual = format!("sha256:{}", encode_lowercase_hex(&digest.finalize()));
        if actual != artifact.digest {
            return Ok(SealedReadBundleArtifactStateV1::Stale {
                detail: format!(
                    "sealed read bundle artifact `{name}` digest mismatch: expected `{}`, observed `{actual}`",
                    artifact.digest
                ),
            });
        }
        hotpath::gauge!("graph_db.bundle.load.bytes").set(artifact.bytes as f64);
        Ok(SealedReadBundleArtifactStateV1::Loaded { artifact, bytes })
    })
}

/// Removes the generation's bundle manifest and every bundle file filed under
/// its sealed digest. Idempotent: an absent bundle retires successfully.
/// Called from the same retirement pass that collects the generation itself.
pub fn retire_sealed_read_bundle(
    root: &Path,
    sealed: &SealedGraphStateDigest,
) -> Result<(), GraphDbError> {
    hotpath::measure_block!("graph_db.bundle.retire", {
        let hex = sealed_hex(sealed)?;
        // Manifest first: once it is gone the bundle is Absent, so a crash
        // between the two removals can never leave dangling references.
        let mut removed = remove_bundle_file(&root.join(manifest_file_name(&hex)))?;
        let entries = match std::fs::read_dir(root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => {
                return Err(GraphDbError::unavailable(format!(
                    "failed to enumerate sealed read bundle root: {error}"
                )));
            }
        };
        let visible = format!("read-bundle-{hex}.");
        let staged = format!(".read-bundle-{hex}.");
        for entry in entries {
            let entry = entry.map_err(|error| {
                GraphDbError::unavailable(format!(
                    "failed to enumerate sealed read bundle root: {error}"
                ))
            })?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if name.starts_with(&visible) || name.starts_with(&staged) {
                removed |= remove_bundle_file(&entry.path())?;
            }
        }
        if removed {
            sync_bundle_directory(root)?;
        }
        Ok(())
    })
}

/// Removes aborted staging temporaries for one sealed digest without touching
/// a committed bundle. Activation retries call this (via [`SealedReadBundleWriterV1::create`])
/// so a prior OOM or cancelled catalog write cannot stack `.tmp` files.
pub fn sweep_aborted_sealed_read_bundle_temporaries(
    root: &Path,
    sealed: &SealedGraphStateDigest,
) -> Result<(), GraphDbError> {
    let hex = sealed_hex(sealed)?;
    let prefix = format!(".read-bundle-{hex}.");
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(GraphDbError::unavailable(format!(
                "failed to enumerate sealed read bundle temporaries: {error}"
            )));
        }
    };
    for entry in entries {
        let entry = entry.map_err(|error| {
            GraphDbError::unavailable(format!(
                "failed to enumerate sealed read bundle temporaries: {error}"
            ))
        })?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if name.starts_with(&prefix) && name.ends_with(".tmp") {
            let _ = remove_bundle_file(&entry.path())?;
        }
    }
    Ok(())
}

fn verify_manifest_binding(
    manifest: &SealedReadBundleManifestV1,
    sealed: &SealedGraphStateDigest,
    identity: &GraphGenerationManifestIdentity,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<Option<String>, GraphDbError> {
    if manifest.format != SEALED_READ_BUNDLE_FORMAT_V1 {
        return Ok(Some(format!(
            "sealed read bundle manifest format `{}` is not `{SEALED_READ_BUNDLE_FORMAT_V1}`",
            manifest.format
        )));
    }
    if manifest.sealed_state_digest != sealed.as_str() {
        return Ok(Some(
            "sealed read bundle manifest names a different sealed state digest".to_owned(),
        ));
    }
    let expected = format!(
        "sha256:{}",
        generation_identity_frames_digest(identity, check)?
    );
    if manifest.generation_identity_digest != expected {
        return Ok(Some(format!(
            "sealed read bundle is bound to generation identity `{}`, expected `{expected}`",
            manifest.generation_identity_digest
        )));
    }
    if manifest.artifacts.len() > MAX_SEALED_READ_BUNDLE_ARTIFACTS_V1 {
        return Ok(Some(
            "sealed read bundle manifest artifact count exceeds its bound".to_owned(),
        ));
    }
    for artifact in &manifest.artifacts {
        if validate_artifact_name(&artifact.name).is_err() {
            return Ok(Some(
                "sealed read bundle manifest contains an invalid artifact name".to_owned(),
            ));
        }
    }
    Ok(None)
}

struct HashingFileWriter {
    file: std::fs::File,
    digest: Sha256,
    bytes: u64,
    failure: Option<GraphDbError>,
}

impl Write for HashingFileWriter {
    fn write(&mut self, chunk: &[u8]) -> io::Result<usize> {
        let next = self
            .bytes
            .checked_add(chunk.len() as u64)
            .filter(|next| *next <= MAX_SEALED_READ_BUNDLE_ARTIFACT_BYTES_V1)
            .ok_or_else(|| {
                self.failure = Some(GraphDbError::invalid(
                    "sealed read bundle artifact exceeds its byte bound",
                ));
                io::Error::other("sealed read bundle artifact exceeds its byte bound")
            })?;
        self.file.write_all(chunk)?;
        self.digest.update(chunk);
        self.bytes = next;
        Ok(chunk.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

fn bundle_tmp_path(root: &Path, hex: &str, name: &str) -> PathBuf {
    let seq = BUNDLE_TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    root.join(format!(
        ".read-bundle-{hex}.{name}.{}.{seq}.tmp",
        std::process::id()
    ))
}

fn sealed_hex(sealed: &SealedGraphStateDigest) -> Result<String, GraphDbError> {
    sealed
        .as_str()
        .strip_prefix("sha256:")
        .map(str::to_owned)
        .ok_or_else(|| GraphDbError::invalid("sealed graph state digest is not sha256"))
}

fn manifest_file_name(hex: &str) -> String {
    format!("read-bundle-{hex}.json")
}

fn artifact_file_name(hex: &str, name: &str) -> String {
    format!("read-bundle-{hex}.{name}.bin")
}

fn validate_artifact_name(name: &str) -> Result<(), GraphDbError> {
    if name.is_empty()
        || name.len() > 64
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err(GraphDbError::invalid(
            "sealed read bundle artifact name is invalid",
        ));
    }
    Ok(())
}

fn remove_stale_regular_file(path: &Path) -> Result<(), GraphDbError> {
    match path.symlink_metadata() {
        Ok(metadata) if metadata.file_type().is_file() => {
            std::fs::remove_file(path).map_err(|error| {
                GraphDbError::unavailable(format!(
                    "failed to replace stale sealed read bundle file: {error}"
                ))
            })
        }
        Ok(_) => Err(GraphDbError::unavailable(
            "sealed read bundle path is not a regular file",
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(GraphDbError::unavailable(format!(
            "failed to inspect sealed read bundle path: {error}"
        ))),
    }
}

fn remove_bundle_file(path: &Path) -> Result<bool, GraphDbError> {
    match path.symlink_metadata() {
        Ok(metadata) if metadata.file_type().is_file() => {
            std::fs::remove_file(path).map_err(|error| {
                GraphDbError::unavailable(format!(
                    "failed to retire sealed read bundle file: {error}"
                ))
            })?;
            Ok(true)
        }
        Ok(_) => Err(GraphDbError::unavailable(
            "sealed read bundle retirement found a non-regular file",
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(GraphDbError::unavailable(format!(
            "failed to inspect sealed read bundle file: {error}"
        ))),
    }
}

fn sync_bundle_directory(root: &Path) -> Result<(), GraphDbError> {
    tracedecay_private_fs::framed_log::sync_directory(
        root,
        tracedecay_private_fs::framed_log::DirectorySyncPolicy::Strict,
    )
    .map_err(|error| {
        GraphDbError::unavailable(format!("failed to sync sealed read bundle root: {error}"))
    })
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;
    use crate::{
        GraphGenerationId, GraphNamespace, GraphProjectionId, GraphProjectionIdentity,
        GraphWatermark, SourceGeneration,
    };

    fn identity(generation: &str) -> GraphGenerationManifestIdentity {
        GraphGenerationManifestIdentity::new(
            GraphProjectionIdentity::new(
                GraphNamespace::new("bundle-probe").unwrap(),
                GraphProjectionId::new("code-graph").unwrap(),
            ),
            GraphGenerationId::new(generation).unwrap(),
            SourceGeneration::new(format!("source-{generation}")).unwrap(),
            GraphWatermark::new(format!("watermark-{generation}")).unwrap(),
            vec![],
        )
    }

    fn sealed() -> SealedGraphStateDigest {
        SealedGraphStateDigest::try_from(format!("sha256:{}", "ab".repeat(32))).unwrap()
    }

    fn write_bundle(root: &Path, generation: &str, payload: &[u8]) -> SealedReadBundleManifestV1 {
        let mut writer = SealedReadBundleWriterV1::create(root, &sealed()).unwrap();
        writer
            .stage_artifact("interactive-catalog", &mut |out| {
                out.write_all(payload)
                    .map_err(|error| GraphDbError::unavailable(error.to_string()))
            })
            .unwrap();
        writer.commit(&identity(generation), &|| Ok(())).unwrap()
    }

    #[test]
    fn staged_bundle_round_trips_with_identity_binding() {
        let temp = TempDir::new().unwrap();
        let manifest = write_bundle(temp.path(), "generation-a", b"catalog-bytes");
        assert_eq!(manifest.format, SEALED_READ_BUNDLE_FORMAT_V1);
        assert_eq!(manifest.artifacts.len(), 1);
        assert_eq!(manifest.artifacts[0].bytes, 13);

        let state = load_sealed_read_bundle_artifact(
            temp.path(),
            &sealed(),
            &identity("generation-a"),
            "interactive-catalog",
            &|| Ok(()),
        )
        .unwrap();
        match state {
            SealedReadBundleArtifactStateV1::Loaded { artifact, bytes } => {
                assert_eq!(artifact, manifest.artifacts[0]);
                assert_eq!(bytes, b"catalog-bytes");
            }
            other => panic!("expected loaded artifact, got {other:?}"),
        }
    }

    #[test]
    fn missing_bundle_is_typed_absent_and_unknown_artifact_is_typed_absent() {
        let temp = TempDir::new().unwrap();
        let state = load_sealed_read_bundle_artifact(
            temp.path(),
            &sealed(),
            &identity("generation-a"),
            "interactive-catalog",
            &|| Ok(()),
        )
        .unwrap();
        assert!(matches!(
            state,
            SealedReadBundleArtifactStateV1::Absent { .. }
        ));

        write_bundle(temp.path(), "generation-a", b"catalog-bytes");
        let state = load_sealed_read_bundle_artifact(
            temp.path(),
            &sealed(),
            &identity("generation-a"),
            "identity-index",
            &|| Ok(()),
        )
        .unwrap();
        match state {
            SealedReadBundleArtifactStateV1::Absent { reason } => {
                assert!(reason.contains("identity-index"), "{reason}");
            }
            other => panic!("expected absent artifact, got {other:?}"),
        }
    }

    #[test]
    fn foreign_generation_identity_is_typed_stale() {
        let temp = TempDir::new().unwrap();
        write_bundle(temp.path(), "generation-a", b"catalog-bytes");
        let state = load_sealed_read_bundle_artifact(
            temp.path(),
            &sealed(),
            &identity("generation-b"),
            "interactive-catalog",
            &|| Ok(()),
        )
        .unwrap();
        match state {
            SealedReadBundleArtifactStateV1::Stale { detail } => {
                assert!(detail.contains("bound to generation identity"), "{detail}");
            }
            other => panic!("expected stale bundle, got {other:?}"),
        }
    }

    #[test]
    fn tampered_artifact_bytes_are_typed_stale() {
        let temp = TempDir::new().unwrap();
        write_bundle(temp.path(), "generation-a", b"catalog-bytes");
        let artifact = temp.path().join(format!(
            "read-bundle-{}.interactive-catalog.bin",
            "ab".repeat(32)
        ));
        std::fs::write(&artifact, b"tampered-byte").unwrap();
        let state = load_sealed_read_bundle_artifact(
            temp.path(),
            &sealed(),
            &identity("generation-a"),
            "interactive-catalog",
            &|| Ok(()),
        )
        .unwrap();
        match state {
            SealedReadBundleArtifactStateV1::Stale { detail } => {
                assert!(detail.contains("digest mismatch"), "{detail}");
            }
            other => panic!("expected stale artifact, got {other:?}"),
        }
    }

    #[test]
    fn retirement_removes_manifest_artifacts_and_stage_leftovers() {
        let temp = TempDir::new().unwrap();
        write_bundle(temp.path(), "generation-a", b"catalog-bytes");
        let hex = "ab".repeat(32);
        std::fs::write(
            temp.path().join(format!(".read-bundle-{hex}.orphan.1.tmp")),
            b"orphan",
        )
        .unwrap();
        std::fs::write(temp.path().join("generation-unrelated.json"), b"keep").unwrap();

        retire_sealed_read_bundle(temp.path(), &sealed()).unwrap();

        let remaining: Vec<String> = std::fs::read_dir(temp.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(remaining, vec!["generation-unrelated.json".to_owned()]);

        // Idempotent on an absent bundle.
        retire_sealed_read_bundle(temp.path(), &sealed()).unwrap();
    }

    #[test]
    fn dropped_writer_removes_staged_temporaries() {
        let temp = TempDir::new().unwrap();
        {
            let mut writer = SealedReadBundleWriterV1::create(temp.path(), &sealed()).unwrap();
            writer
                .stage_artifact("interactive-catalog", &mut |out| {
                    out.write_all(b"asdf")
                        .map_err(|error| GraphDbError::unavailable(error.to_string()))
                })
                .unwrap();
        }
        assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 0);
    }

    #[test]
    fn aborted_write_removes_in_progress_temporary() {
        let temp = TempDir::new().unwrap();
        let mut writer = SealedReadBundleWriterV1::create(temp.path(), &sealed()).unwrap();
        let error = writer
            .stage_artifact("interactive-catalog", &mut |out| {
                out.write_all(&[0u8; 4096])
                    .map_err(|error| GraphDbError::unavailable(error.to_string()))?;
                Err(GraphDbError::unavailable("catalog write aborted"))
            })
            .expect_err("aborted staging must fail");
        assert!(error.to_string().contains("catalog write aborted"));
        drop(writer);
        assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 0);
    }

    #[test]
    fn panic_during_write_removes_in_progress_temporary() {
        let temp = TempDir::new().unwrap();
        let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut writer = SealedReadBundleWriterV1::create(temp.path(), &sealed()).unwrap();
            writer
                .stage_artifact("interactive-catalog", &mut |out| {
                    out.write_all(&[0u8; 4096])
                        .map_err(|error| GraphDbError::unavailable(error.to_string()))?;
                    panic!("catalog write panicked");
                })
                .expect("panic is the abort path");
        }));
        assert!(panicked.is_err());
        assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 0);
    }

    #[test]
    fn create_sweeps_aborted_temporaries_without_touching_committed_or_foreign_digests() {
        let temp = TempDir::new().unwrap();
        write_bundle(temp.path(), "generation-a", b"catalog-bytes");
        let hex = "ab".repeat(32);
        let foreign = "cd".repeat(32);
        std::fs::write(
            temp.path()
                .join(format!(".read-bundle-{hex}.interactive-catalog.1.tmp")),
            vec![0u8; 32],
        )
        .unwrap();
        std::fs::write(
            temp.path()
                .join(format!(".read-bundle-{hex}.interactive-catalog.9.2.tmp")),
            vec![0u8; 32],
        )
        .unwrap();
        std::fs::write(
            temp.path()
                .join(format!(".read-bundle-{foreign}.interactive-catalog.1.tmp")),
            b"keep-foreign",
        )
        .unwrap();

        drop(SealedReadBundleWriterV1::create(temp.path(), &sealed()).unwrap());

        let mut remaining: Vec<String> = std::fs::read_dir(temp.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        remaining.sort();
        assert_eq!(
            remaining,
            vec![
                format!(".read-bundle-{foreign}.interactive-catalog.1.tmp"),
                format!("read-bundle-{hex}.interactive-catalog.bin"),
                format!("read-bundle-{hex}.json"),
            ]
        );
    }
}
