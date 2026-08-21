use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use sha2::{Digest, Sha256};
use tracedecay_code_index::graph_projection::CodeGraphProjectionError;
use tracedecay_domain::{ProjectId, RepositoryId};
use tracedecay_graph_db::{
    GraphBudgetKind, GraphDbError, GraphGenerationManifest, GraphGenerationManifestProvider,
    GraphNamespace, GraphProjectionId, GraphProjectionIdentity, GraphProjectorRevision,
    SealedCodeGenerationReplay,
};
use tracedecay_store::{GraphProjectionIdentityV1, StoreShardIdV1};

const SEAL_READ_CHECK_BYTES: usize = 64 * 1024;

fn validate_sealed_generation_metadata(metadata: &std::fs::Metadata) -> Result<u64, GraphDbError> {
    if !metadata.file_type().is_file() {
        return Err(GraphDbError::Corrupt {
            message: "sealed code generation replay target is not a regular file".to_owned(),
        });
    }
    if metadata.len() > tracedecay_code_index::production::MAX_SEALED_CODE_GENERATION_BYTES_V1 {
        return Err(GraphDbError::ResetRequired {
            message: "sealed code generation exceeds the canonical byte limit".to_owned(),
        });
    }
    Ok(metadata.len())
}

fn same_file_identity(left: &std::fs::Metadata, right: &std::fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;

        left.dev() == right.dev()
            && left.ino() == right.ino()
            && left.len() == right.len()
            && left.mtime() == right.mtime()
            && left.mtime_nsec() == right.mtime_nsec()
            && left.ctime() == right.ctime()
            && left.ctime_nsec() == right.ctime_nsec()
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;

        // Volume and file-index equality is checked separately through the
        // stable handle authority (`same_windows_handle_identity`); metadata
        // only carries the stable fields here.
        left.file_size() == right.file_size()
            && left.last_write_time() == right.last_write_time()
            && left.creation_time() == right.creation_time()
    }
    #[cfg(not(any(unix, windows)))]
    {
        false
    }
}

/// Confirms the opened handle and the path still denote the same file, via
/// the stable GetFileInformationByHandle authority instead of the unstable
/// `windows_by_handle` metadata surface.
#[cfg(windows)]
fn same_windows_handle_identity(file: &File, path: &std::path::Path) -> Result<bool, GraphDbError> {
    let path_file =
        File::open(path).map_err(|error| GraphDbError::unavailable(error.to_string()))?;
    let path_identity = tracedecay_runtime_core::windows_file::information(&path_file)
        .map_err(|error| GraphDbError::unavailable(error.to_string()))?;
    let handle_identity = tracedecay_runtime_core::windows_file::information(file)
        .map_err(|error| GraphDbError::unavailable(error.to_string()))?;
    Ok(
        path_identity.volume_serial_number == handle_identity.volume_serial_number
            && path_identity.file_index == handle_identity.file_index,
    )
}

struct CheckedSealReader<'a> {
    reader: BufReader<File>,
    check: &'a dyn Fn() -> Result<(), GraphDbError>,
    bytes_read: u64,
    digest: Sha256,
    failure: Option<GraphDbError>,
}

impl CheckedSealReader<'_> {
    fn retain_failure(&mut self, error: GraphDbError) -> std::io::Error {
        self.failure = Some(error);
        std::io::Error::other("sealed code generation checked read failed")
    }

    fn finish(
        self,
        path: &std::path::Path,
        opened_metadata: &std::fs::Metadata,
        admitted_len: u64,
        expected_digest: &str,
    ) -> Result<(), GraphDbError> {
        (self.check)()?;
        let final_file_metadata =
            self.reader
                .get_ref()
                .metadata()
                .map_err(|error| GraphDbError::Corrupt {
                    message: format!(
                        "sealed code generation metadata cannot be revalidated: {error}"
                    ),
                })?;
        let final_path_metadata =
            path.symlink_metadata()
                .map_err(|error| GraphDbError::Corrupt {
                    message: format!("sealed code generation path cannot be revalidated: {error}"),
                })?;
        if !same_file_identity(opened_metadata, &final_file_metadata)
            || !same_file_identity(opened_metadata, &final_path_metadata)
            || self.bytes_read != admitted_len
        {
            return Err(GraphDbError::Corrupt {
                message: "sealed code generation identity or length changed while it was read"
                    .to_owned(),
            });
        }
        #[cfg(windows)]
        if !same_windows_handle_identity(self.reader.get_ref(), path)? {
            return Err(GraphDbError::Corrupt {
                message: "sealed code generation identity or length changed while it was read"
                    .to_owned(),
            });
        }
        if hex::encode(self.digest.finalize()) != expected_digest {
            return Err(GraphDbError::Corrupt {
                message: "sealed code generation filename digest does not match its bytes"
                    .to_owned(),
            });
        }
        Ok(())
    }
}

impl Read for CheckedSealReader<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        if let Err(error) = (self.check)() {
            return Err(self.retain_failure(error));
        }
        let read_len = buffer.len().min(SEAL_READ_CHECK_BYTES);
        let read = match self.reader.read(&mut buffer[..read_len]) {
            Ok(read) => read,
            Err(error) => {
                let error = GraphDbError::Corrupt {
                    message: format!("sealed code generation replay read failed: {error}"),
                };
                return Err(self.retain_failure(error));
            }
        };
        let read = u64::try_from(read).map_err(|_| {
            self.retain_failure(GraphDbError::ResetRequired {
                message: "sealed code generation read length exceeds u64".to_owned(),
            })
        })?;
        let next_len = self.bytes_read.checked_add(read).ok_or_else(|| {
            self.retain_failure(GraphDbError::ResetRequired {
                message: "sealed code generation byte length overflowed".to_owned(),
            })
        })?;
        if next_len > tracedecay_code_index::production::MAX_SEALED_CODE_GENERATION_BYTES_V1 {
            return Err(self.retain_failure(GraphDbError::ResetRequired {
                message: "sealed code generation grew beyond the canonical byte limit".to_owned(),
            }));
        }
        let read = usize::try_from(read).map_err(|_| {
            self.retain_failure(GraphDbError::ResetRequired {
                message: "sealed code generation read length exceeds addressable memory".to_owned(),
            })
        })?;
        self.digest.update(&buffer[..read]);
        self.bytes_read = next_len;
        Ok(read)
    }
}

fn open_checked_seal_reader<'a>(
    path: &std::path::Path,
    check: &'a dyn Fn() -> Result<(), GraphDbError>,
) -> Result<(CheckedSealReader<'a>, std::fs::Metadata, u64), GraphDbError> {
    let path_metadata = path.symlink_metadata().map_err(|error| {
        GraphDbError::unavailable(format!(
            "sealed code generation is unavailable for replay: {error}"
        ))
    })?;
    let admitted_len = validate_sealed_generation_metadata(&path_metadata)?;
    let file = File::open(path).map_err(|error| {
        GraphDbError::unavailable(format!(
            "sealed code generation cannot be opened for replay: {error}"
        ))
    })?;
    let opened_metadata = file.metadata().map_err(|error| GraphDbError::Corrupt {
        message: format!("sealed code generation metadata cannot be read: {error}"),
    })?;
    if !same_file_identity(&path_metadata, &opened_metadata) {
        return Err(GraphDbError::Corrupt {
            message: "sealed code generation identity changed while it was opened".to_owned(),
        });
    }
    #[cfg(windows)]
    if !same_windows_handle_identity(&file, path)? {
        return Err(GraphDbError::Corrupt {
            message: "sealed code generation identity changed while it was opened".to_owned(),
        });
    }
    Ok((
        CheckedSealReader {
            reader: BufReader::with_capacity(SEAL_READ_CHECK_BYTES, file),
            check,
            bytes_read: 0,
            digest: Sha256::new(),
            failure: None,
        },
        opened_metadata,
        admitted_len,
    ))
}

fn decode_verified_seal(
    path: &std::path::Path,
    expected_digest: &str,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<tracedecay_code_index::production::CodeIndexPublishedGenerationV1, GraphDbError> {
    let (mut reader, opened_metadata, admitted_len) = open_checked_seal_reader(path, check)?;
    let decoded =
        tracedecay_code_index::production::CodeIndexPublishedGenerationV1::decode_sealed_reader(
            &mut reader,
            admitted_len,
        );
    if let Some(error) = reader.failure.take() {
        return Err(error);
    }
    let generation = decoded.map_err(|error| GraphDbError::Corrupt {
        message: format!("sealed code generation replay is invalid: {error}"),
    })?;
    reader.finish(path, &opened_metadata, admitted_len, expected_digest)?;
    Ok(generation)
}

#[derive(Clone)]
struct BoundCodeGenerationSourceV1 {
    project_shard: StoreShardIdV1,
    project_id: ProjectId,
    repositories: BTreeSet<RepositoryId>,
    replay_root: PathBuf,
}

#[derive(Default)]
pub(super) struct DaemonCodeGraphManifestProviderV1 {
    sources: RwLock<BTreeMap<StoreShardIdV1, BoundCodeGenerationSourceV1>>,
}

impl DaemonCodeGraphManifestProviderV1 {
    pub(super) fn bind(
        &self,
        project_shard: StoreShardIdV1,
        project_id: ProjectId,
        repository: RepositoryId,
        replay_root: PathBuf,
    ) -> Result<(), GraphDbError> {
        let mut sources = self.sources.write().map_err(|_| {
            GraphDbError::unavailable("code generation manifest provider lock is poisoned")
        })?;
        if let Some(existing) = sources.get_mut(&project_shard) {
            if existing.project_shard != project_shard
                || existing.project_id != project_id
                || existing.replay_root != replay_root
            {
                return Err(GraphDbError::Conflict);
            }
            existing.repositories.insert(repository);
            return Ok(());
        }
        sources.insert(
            project_shard.clone(),
            BoundCodeGenerationSourceV1 {
                project_shard,
                project_id,
                repositories: BTreeSet::from([repository]),
                replay_root,
            },
        );
        Ok(())
    }
}

impl GraphGenerationManifestProvider for DaemonCodeGraphManifestProviderV1 {
    fn hydrate_sealed_code_generation(
        &self,
        owner: &GraphProjectionIdentityV1,
        source: &SealedCodeGenerationReplay,
        check: &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<GraphGenerationManifest, GraphDbError> {
        check()?;
        let binding = self
            .sources
            .read()
            .map_err(|_| {
                GraphDbError::unavailable("code generation manifest provider lock is poisoned")
            })?
            .get(&owner.shard_id)
            .cloned()
            .ok_or_else(|| {
                GraphDbError::unavailable(
                    "sealed code generation replay source is not mounted for this projection",
                )
            })?;
        if owner.shard_id != binding.project_shard
            || !binding.repositories.contains(&source.repository)
            || source.projector_revision.as_str()
                != tracedecay_code_index::graph_projection::CODE_GRAPH_PROJECTOR_REVISION
        {
            return Err(GraphDbError::Conflict);
        }
        let tracedecay_store::StoreShardScopeV1::Project { project_id } =
            &binding.project_shard.scope
        else {
            return Err(GraphDbError::Conflict);
        };
        if project_id != &binding.project_id {
            return Err(GraphDbError::Conflict);
        }

        let digest = source
            .sealed_state_digest
            .as_str()
            .strip_prefix("sha256:")
            .ok_or_else(|| GraphDbError::invalid("sealed state digest is not sha256"))?;
        let path = binding
            .replay_root
            .join(format!("generation-{digest}.json"));
        let generation = decode_verified_seal(&path, digest, check)?;
        if generation.manifest().project_id != binding.project_id
            || generation.snapshot().repository != source.repository
            || generation.manifest().generation_id != source.generation
        {
            return Err(GraphDbError::Conflict);
        }

        let projection = GraphProjectionIdentity::new(
            GraphNamespace::new(owner.namespace.as_str())?,
            GraphProjectionId::new(owner.projection.as_str())?,
        );
        tracedecay_code_index::graph_projection::build_published_code_graph_manifest_checked(
            projection,
            &generation,
            &GraphProjectorRevision::try_from(source.projector_revision.as_str().to_owned())?,
            check,
        )
        .map(Arc::unwrap_or_clone)
        .map_err(classify_sealed_projection_build_error)
    }
}

/// Interruptions from the caller's `check` probe are transport states, not
/// evidence about the sealed payload. Classifying them as corruption would
/// fault-retain the graph slot in the shared capacity-bounded registry and
/// poison later retries of the same immutable artifact.
fn classify_sealed_projection_build_error(error: CodeGraphProjectionError) -> GraphDbError {
    match error {
        CodeGraphProjectionError::Cancelled => GraphDbError::Cancelled,
        CodeGraphProjectionError::DeadlineExceeded => GraphDbError::DeadlineExceeded,
        CodeGraphProjectionError::BudgetExhausted { budget, limit } => {
            // Preserve the exact budget identity across the round-trip; an
            // unrecognized name is a projection-local budget, reported under
            // the read class with its real limit rather than a fabricated one.
            let kind = GraphBudgetKind::from_name(&budget).unwrap_or(GraphBudgetKind::Read);
            GraphDbError::budget_exhausted(kind, limit)
        }
        other => GraphDbError::Corrupt {
            message: format!("sealed code generation graph projection is invalid: {other}"),
        },
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Seek, SeekFrom, Write};
    use std::sync::atomic::{AtomicUsize, Ordering};

    use sha2::{Digest, Sha256};
    use tempfile::TempDir;
    use tracedecay_domain::{CodeGenerationId, ProjectId, RepositoryId};
    use tracedecay_graph_db::{
        GraphBudgetKind, GraphDbError, GraphGenerationManifestProvider, GraphProjectorRevision,
        SealedCodeGenerationReplay, SealedGraphStateDigest,
    };
    use tracedecay_store::{
        BrainId, GraphNamespaceV1, GraphProjectionIdV1, GraphProjectionIdentityV1, StoreShardIdV1,
        UserProfileId,
    };

    use super::{
        DaemonCodeGraphManifestProviderV1, SEAL_READ_CHECK_BYTES, open_checked_seal_reader,
        validate_sealed_generation_metadata,
    };

    fn verify_seal_stream(
        path: &std::path::Path,
        expected_digest: &str,
        check: &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<(), GraphDbError> {
        let (mut reader, opened_metadata, admitted_len) = open_checked_seal_reader(path, check)?;
        let copied = std::io::copy(&mut reader, &mut std::io::sink());
        if let Some(error) = reader.failure.take() {
            return Err(error);
        }
        copied.map_err(|error| GraphDbError::Corrupt {
            message: format!("sealed code generation test read failed: {error}"),
        })?;
        reader.finish(path, &opened_metadata, admitted_len, expected_digest)
    }

    fn fixture(
        replay_root: std::path::PathBuf,
    ) -> (
        DaemonCodeGraphManifestProviderV1,
        GraphProjectionIdentityV1,
        SealedCodeGenerationReplay,
    ) {
        let project = ProjectId::new("project.provider").unwrap();
        let repository = RepositoryId::new("repository.provider").unwrap();
        let shard = StoreShardIdV1::project(
            BrainId::new("brain.provider").unwrap(),
            UserProfileId::new("profile.provider").unwrap(),
            project.clone(),
        );
        let provider = DaemonCodeGraphManifestProviderV1::default();
        provider
            .bind(shard.clone(), project, repository.clone(), replay_root)
            .unwrap();
        (
            provider,
            GraphProjectionIdentityV1 {
                shard_id: shard,
                namespace: GraphNamespaceV1::new("namespace.provider").unwrap(),
                projection: GraphProjectionIdV1::new("code-generation").unwrap(),
            },
            SealedCodeGenerationReplay {
                repository,
                generation: CodeGenerationId::new("generation.provider").unwrap(),
                sealed_state_digest: SealedGraphStateDigest::try_from(format!(
                    "sha256:{}",
                    "a".repeat(64)
                ))
                .unwrap(),
                projector_revision: GraphProjectorRevision::try_from(
                    tracedecay_code_index::graph_projection::CODE_GRAPH_PROJECTOR_REVISION
                        .to_owned(),
                )
                .unwrap(),
            },
        )
    }

    #[test]
    fn exact_seal_provider_rejects_missing_corrupt_and_foreign_sources() {
        let temp = TempDir::new().unwrap();
        let replay_root = temp.path().join("replay");
        std::fs::create_dir_all(&replay_root).unwrap();
        let (provider, owner, source) = fixture(replay_root.clone());

        assert!(matches!(
            provider.hydrate_sealed_code_generation(&owner, &source, &|| Ok(())),
            Err(GraphDbError::Unavailable { .. })
        ));

        let mut foreign = source.clone();
        foreign.repository = RepositoryId::new("repository.foreign").unwrap();
        assert_eq!(
            provider
                .hydrate_sealed_code_generation(&owner, &foreign, &|| Ok(()))
                .unwrap_err(),
            GraphDbError::Conflict
        );

        std::fs::write(
            replay_root.join(format!(
                "generation-{}.json",
                source
                    .sealed_state_digest
                    .as_str()
                    .strip_prefix("sha256:")
                    .unwrap()
            )),
            b"corrupt",
        )
        .unwrap();
        assert!(matches!(
            provider.hydrate_sealed_code_generation(&owner, &source, &|| Ok(())),
            Err(GraphDbError::Corrupt { .. })
        ));
    }

    #[test]
    fn sealed_generation_metadata_rejects_oversized_sparse_source_before_allocation() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("oversized.json");
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(tracedecay_code_index::production::MAX_SEALED_CODE_GENERATION_BYTES_V1 + 1)
            .unwrap();
        let metadata = path.symlink_metadata().unwrap();

        assert!(matches!(
            validate_sealed_generation_metadata(&metadata),
            Err(GraphDbError::ResetRequired { .. })
        ));
    }

    #[test]
    fn sealed_generation_read_rejects_same_length_mutation() {
        let temp = TempDir::new().unwrap();
        let bytes = vec![b'a'; SEAL_READ_CHECK_BYTES * 2];
        let digest = hex::encode(Sha256::digest(&bytes));
        let path = temp.path().join(format!("generation-{digest}.json"));
        std::fs::write(&path, bytes).unwrap();
        let checks = AtomicUsize::new(0);

        let error = verify_seal_stream(&path, &digest, &|| {
            if checks.fetch_add(1, Ordering::SeqCst) == 1 {
                let mut file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
                file.seek(SeekFrom::Start(SEAL_READ_CHECK_BYTES as u64))
                    .unwrap();
                file.write_all(b"z").unwrap();
                file.sync_all().unwrap();
            }
            Ok(())
        })
        .unwrap_err();

        assert!(matches!(error, GraphDbError::Corrupt { .. }));
    }

    #[test]
    fn sealed_projection_build_interruptions_stay_typed_and_never_read_as_corruption() {
        use tracedecay_code_index::graph_projection::CodeGraphProjectionError;

        use super::classify_sealed_projection_build_error;

        assert_eq!(
            classify_sealed_projection_build_error(CodeGraphProjectionError::DeadlineExceeded),
            GraphDbError::DeadlineExceeded
        );
        assert_eq!(
            classify_sealed_projection_build_error(CodeGraphProjectionError::Cancelled),
            GraphDbError::Cancelled
        );
        assert!(matches!(
            classify_sealed_projection_build_error(CodeGraphProjectionError::BudgetExhausted {
                budget: "capacity".to_owned(),
                limit: 7,
            }),
            GraphDbError::BudgetExhausted {
                kind: GraphBudgetKind::Capacity,
                limit: 7,
            }
        ));
        assert!(matches!(
            classify_sealed_projection_build_error(CodeGraphProjectionError::Contract(
                "entity payload is malformed".to_owned()
            )),
            GraphDbError::Corrupt { .. }
        ));
    }

    #[test]
    fn sealed_generation_read_preserves_deadline_error() {
        let temp = TempDir::new().unwrap();
        let bytes = vec![b'a'; SEAL_READ_CHECK_BYTES * 3];
        let digest = hex::encode(Sha256::digest(&bytes));
        let path = temp.path().join(format!("generation-{digest}.json"));
        std::fs::write(&path, bytes).unwrap();
        let checks = AtomicUsize::new(0);

        assert_eq!(
            verify_seal_stream(&path, &digest, &|| {
                if checks.fetch_add(1, Ordering::SeqCst) >= 2 {
                    Err(GraphDbError::DeadlineExceeded)
                } else {
                    Ok(())
                }
            }),
            Err(GraphDbError::DeadlineExceeded)
        );
    }
}
