//! Physical pack handles owned by one already-admitted generation read pass.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use tracedecay_code_index::production::{
    CodeIndexProductionErrorV1, SealedGenerationSegmentIdentityV1, SealedGenerationSegmentReadV1,
};
use tracedecay_domain::sha256_hex_suffix;

#[cfg(windows)]
use super::PinnedGenerationFileV1;
use super::{DaemonCodeIndexPublicationStoreV1, PinnedGenerationSegmentV1};

/// Payload and route-control opens are distinct costs. Both counters include
/// failed attempts; neither grows with reads after a pass is admitted.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct PackReadOpenAttemptsV1 {
    pub(super) payload: u64,
    #[cfg(windows)]
    pub(super) ancestors: u64,
}

struct PinnedPackV1 {
    segment: PinnedGenerationSegmentV1,
    path: PathBuf,
}

impl PinnedPackV1 {
    fn require_unchanged(&self) -> Result<(), CodeIndexProductionErrorV1> {
        #[cfg(unix)]
        let unchanged = self
            .path
            .symlink_metadata()
            .ok()
            .zip(self.segment.file.metadata().ok())
            .is_some_and(|(named, opened)| {
                DaemonCodeIndexPublicationStoreV1::same_unix_segment_identity(
                    &self.segment.admitted_metadata,
                    &named,
                ) && DaemonCodeIndexPublicationStoreV1::same_unix_segment_identity(
                    &self.segment.admitted_metadata,
                    &opened,
                )
            });
        #[cfg(windows)]
        let unchanged = match &self.segment.file {
            PinnedGenerationFileV1::Immutable(owner) => {
                let metadata = owner.file().metadata().map_err(|error| {
                    CodeIndexProductionErrorV1::Contract(format!(
                        "sealed generation pack metadata cannot be rechecked: {error}"
                    ))
                })?;
                let current = tracedecay_private_fs::windows_file::information(owner.file())
                    .map_err(|error| CodeIndexProductionErrorV1::Contract(error.to_string()))?;
                let admitted = owner.identity();
                current.volume_serial_number == admitted.volume_serial_number
                    && current.file_index == admitted.file_index
                    && DaemonCodeIndexPublicationStoreV1::same_windows_segment_metadata(
                        &self.segment.admitted_metadata,
                        &metadata,
                    )
            }
            PinnedGenerationFileV1::Shared(_) => false,
        };
        #[cfg(not(any(unix, windows)))]
        let unchanged = false;
        if !unchanged {
            return Err(CodeIndexProductionErrorV1::Contract(
                "sealed generation pack changed during its read pass".to_owned(),
            ));
        }
        Ok(())
    }
}

/// The caller retains the generation or preparation lease for this pass.
/// No store lock, manifest map, or idle reader cache is retained here.
pub(super) struct GenerationPackReadPassV1 {
    packs: [Option<PinnedPackV1>; 2],
    failed: bool,
}

impl GenerationPackReadPassV1 {
    /// The ordinary path/string allowance is already admitted by the caller.
    /// Add both native lease bounds before either ancestor vector or UTF-16
    /// buffer is allocated. The first retained owner can coexist with the
    /// second opener's temporary workspace.
    pub(super) fn immutable_workspace_bytes(
        root: &Path,
        identities: &[SealedGenerationSegmentIdentityV1],
        pending: Option<&BTreeMap<PathBuf, PathBuf>>,
    ) -> Result<usize, CodeIndexProductionErrorV1> {
        #[cfg(windows)]
        {
            let mut workspace = 0_usize;
            for identity in Self::unique_identities(identities)?.into_iter().flatten() {
                let suffix = sha256_hex_suffix(identity.digest.as_str()).ok_or_else(|| {
                    CodeIndexProductionErrorV1::Contract(
                        "sealed pack digest is not sha256".to_owned(),
                    )
                })?;
                let canonical = root.join(format!("segment-{suffix}.json"));
                let path = pending
                    .and_then(|pending| pending.get(&canonical))
                    .unwrap_or(&canonical);
                workspace = workspace.saturating_add(
                    tracedecay_private_fs::windows::immutable_read_workspace_bytes(path)
                        .map_err(|error| CodeIndexProductionErrorV1::Contract(error.to_string()))?,
                );
            }
            Ok(workspace)
        }
        #[cfg(not(windows))]
        {
            let _ = (root, identities, pending);
            Ok(0)
        }
    }

    /// `open_attempts` belongs to the operation, so partial admission reports
    /// every attempted payload open even when a later pack cannot be pinned.
    pub(super) fn open(
        root: &Path,
        identities: &[SealedGenerationSegmentIdentityV1],
        pending: Option<&BTreeMap<PathBuf, PathBuf>>,
        open_attempts: &mut PackReadOpenAttemptsV1,
    ) -> Result<Self, CodeIndexProductionErrorV1> {
        let unique = Self::unique_identities(identities)?;
        let mut pass = Self {
            packs: [None, None],
            failed: false,
        };
        for (slot, identity) in pass.packs.iter_mut().zip(unique.into_iter().flatten()) {
            let suffix = sha256_hex_suffix(identity.digest.as_str()).ok_or_else(|| {
                CodeIndexProductionErrorV1::Contract("sealed pack digest is not sha256".to_owned())
            })?;
            let canonical = root.join(format!("segment-{suffix}.json"));
            let path = pending
                .and_then(|pending| pending.get(&canonical))
                .unwrap_or(&canonical);
            // The canonical opener retains its original pre-open metadata.
            // Never establish a later baseline while assembling the pass.
            #[cfg(not(windows))]
            let segment = DaemonCodeIndexPublicationStoreV1::open_pinned_partitioned_segment_at(
                path,
                SealedGenerationSegmentReadV1::Whole {
                    digest: &identity.digest,
                    size_bytes: identity.size_bytes,
                },
                &mut open_attempts.payload,
            )?;
            #[cfg(windows)]
            let segment = Self::open_immutable_pack(path, identity, open_attempts)?;
            let pinned = PinnedPackV1 {
                segment,
                path: path.clone(),
            };
            *slot = Some(pinned);
        }
        Ok(pass)
    }

    fn unique_identities(
        identities: &[SealedGenerationSegmentIdentityV1],
    ) -> Result<[Option<&SealedGenerationSegmentIdentityV1>; 2], CodeIndexProductionErrorV1> {
        let mut unique: [Option<&SealedGenerationSegmentIdentityV1>; 2] = [None, None];
        for identity in identities {
            if sha256_hex_suffix(identity.digest.as_str()).is_none() {
                return Err(CodeIndexProductionErrorV1::Contract(
                    "sealed pack digest is not sha256".to_owned(),
                ));
            }
            if let Some(known) = unique
                .iter()
                .flatten()
                .find(|known| known.digest == identity.digest)
            {
                if known.size_bytes != identity.size_bytes {
                    return Err(CodeIndexProductionErrorV1::Contract(
                        "sealed pack has conflicting manifest sizes".to_owned(),
                    ));
                }
                continue;
            }
            let slot = unique
                .iter_mut()
                .find(|slot| slot.is_none())
                .ok_or_else(|| {
                    CodeIndexProductionErrorV1::Contract(
                        "sealed generation requires more than two physical packs".to_owned(),
                    )
                })?;
            *slot = Some(identity);
        }

        Ok(unique)
    }

    #[cfg(windows)]
    fn open_immutable_pack(
        path: &Path,
        identity: &SealedGenerationSegmentIdentityV1,
        open_attempts: &mut PackReadOpenAttemptsV1,
    ) -> Result<PinnedGenerationSegmentV1, CodeIndexProductionErrorV1> {
        let unavailable = |error: std::io::Error| {
            CodeIndexProductionErrorV1::Contract(format!(
                "sealed generation immutable pack cannot be admitted: {error}"
            ))
        };
        // Windows pathname metadata is itself an open, including a failed
        // lookup. Keep it in the cold payload count alongside the read handles.
        open_attempts.payload = open_attempts.payload.saturating_add(1);
        let admitted_metadata = path.symlink_metadata().map_err(unavailable)?;
        if !admitted_metadata.is_file() || admitted_metadata.len() != identity.size_bytes {
            return Err(CodeIndexProductionErrorV1::Contract(
                "sealed generation pack identity does not match its manifest".to_owned(),
            ));
        }
        let owner = tracedecay_private_fs::windows::open_immutable_read_file(
            path,
            &mut open_attempts.payload,
            &mut open_attempts.ancestors,
        )
        .map_err(unavailable)?;
        if !DaemonCodeIndexPublicationStoreV1::same_windows_segment_metadata(
            &admitted_metadata,
            owner.admitted_metadata(),
        ) || !DaemonCodeIndexPublicationStoreV1::same_windows_segment_metadata(
            &admitted_metadata,
            &owner.file().metadata().map_err(unavailable)?,
        ) || !DaemonCodeIndexPublicationStoreV1::same_windows_segment_identity(
            path,
            &admitted_metadata,
            owner.file(),
            &mut open_attempts.payload,
        )? {
            return Err(CodeIndexProductionErrorV1::Contract(
                "sealed generation pack changed while its route was pinned".to_owned(),
            ));
        }
        Ok(PinnedGenerationSegmentV1 {
            digest: identity.digest.as_str().to_owned(),
            size_bytes: identity.size_bytes,
            file: PinnedGenerationFileV1::Immutable(owner),
            admitted_metadata,
        })
    }

    pub(super) fn is_invalidated(&self) -> bool {
        self.failed
    }

    pub(super) fn retained_heap_bytes(&self) -> usize {
        self.packs.iter().flatten().fold(0_usize, |bytes, pack| {
            #[cfg(windows)]
            let bytes = bytes.saturating_add(match &pack.segment.file {
                PinnedGenerationFileV1::Shared(_) => 0,
                PinnedGenerationFileV1::Immutable(owner) => owner.retained_heap_bytes(),
            });
            bytes
                .saturating_add(pack.segment.digest.capacity())
                .saturating_add(pack.path.capacity())
        })
    }

    pub(super) fn read(
        &mut self,
        request: SealedGenerationSegmentReadV1<'_>,
        into: &mut Vec<u8>,
    ) -> Result<(), CodeIndexProductionErrorV1> {
        into.clear();
        if self.failed {
            return Err(CodeIndexProductionErrorV1::Contract(
                "sealed generation pack read pass was invalidated".to_owned(),
            ));
        }
        // Malformed requests do not revoke healthy admitted pack handles.
        let index = self.validate_request(request)?;
        let result = self.read_inner(index, request, into);
        if result.is_err() {
            into.clear();
            self.packs = [None, None];
            self.failed = true;
        }
        result
    }

    fn validate_request(
        &self,
        request: SealedGenerationSegmentReadV1<'_>,
    ) -> Result<usize, CodeIndexProductionErrorV1> {
        let (digest, size_bytes, offset, length) = match request {
            SealedGenerationSegmentReadV1::Whole { digest, size_bytes } => {
                (digest, size_bytes, 0, size_bytes)
            }
            SealedGenerationSegmentReadV1::Range {
                digest,
                size_bytes,
                offset,
                length,
            } => (digest, size_bytes, offset, length),
        };
        let (index, pinned) = self
            .packs
            .iter()
            .enumerate()
            .filter_map(|(index, pack)| pack.as_ref().map(|pack| (index, pack)))
            .find(|(_, pinned)| pinned.segment.digest == digest.as_str())
            .ok_or_else(|| {
                CodeIndexProductionErrorV1::Contract(
                    "sealed generation read names a pack outside this pass".to_owned(),
                )
            })?;
        if size_bytes != pinned.segment.size_bytes
            || offset
                .checked_add(length)
                .is_none_or(|end| end > size_bytes)
        {
            return Err(CodeIndexProductionErrorV1::Contract(
                "sealed generation read exceeds its admitted pack identity".to_owned(),
            ));
        }
        usize::try_from(length).map_err(|_| {
            CodeIndexProductionErrorV1::Contract(
                "sealed generation pack range exceeds addressable memory".to_owned(),
            )
        })?;
        Ok(index)
    }

    fn read_inner(
        &mut self,
        index: usize,
        request: SealedGenerationSegmentReadV1<'_>,
        into: &mut Vec<u8>,
    ) -> Result<(), CodeIndexProductionErrorV1> {
        let pinned = self.packs[index].as_mut().ok_or_else(|| {
            CodeIndexProductionErrorV1::Contract(
                "sealed generation read pass lost an admitted pack".to_owned(),
            )
        })?;
        pinned.require_unchanged()?;
        let result = DaemonCodeIndexPublicationStoreV1::read_pinned_partitioned_segment(
            &mut pinned.segment,
            request,
            into,
        );
        pinned.require_unchanged()?;
        result
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, path::Path};

    #[cfg(windows)]
    use std::os::windows::fs::{FileTimesExt, MetadataExt, OpenOptionsExt};

    use sha2::{Digest, Sha256};
    use tempfile::TempDir;
    use tracedecay_code_index::production::{
        SealedGenerationSegmentIdentityV1, SealedGenerationSegmentReadV1,
    };
    use tracedecay_domain::{ManifestDigest, sha256_hex_suffix};

    use super::{
        DaemonCodeIndexPublicationStoreV1, GenerationPackReadPassV1, PackReadOpenAttemptsV1,
        PinnedPackV1,
    };

    fn identity(bytes: &[u8]) -> SealedGenerationSegmentIdentityV1 {
        SealedGenerationSegmentIdentityV1 {
            digest: ManifestDigest::from_sha256_bytes(&Sha256::digest(bytes)).unwrap(),
            size_bytes: bytes.len() as u64,
        }
    }

    fn write_pack(root: &Path, bytes: &[u8]) -> SealedGenerationSegmentIdentityV1 {
        let identity = identity(bytes);
        let suffix = sha256_hex_suffix(identity.digest.as_str()).unwrap();
        std::fs::write(root.join(format!("segment-{suffix}.json")), bytes).unwrap();
        identity
    }

    #[test]
    fn one_pass_reuses_two_deduplicated_physical_pack_handles() {
        let root = TempDir::new().unwrap();
        let content = write_pack(root.path(), b"content payload");
        let evidence = write_pack(root.path(), b"evidence payload");
        let mut open_attempts = PackReadOpenAttemptsV1::default();
        let mut pass = GenerationPackReadPassV1::open(
            root.path(),
            &[content.clone(), evidence.clone(), content.clone()],
            None,
            &mut open_attempts,
        )
        .unwrap();
        assert_eq!(open_attempts.payload, if cfg!(windows) { 8 } else { 2 });
        #[cfg(windows)]
        assert_eq!(
            open_attempts.ancestors,
            2 * root
                .path()
                .ancestors()
                .filter(|path| !path.as_os_str().is_empty())
                .count() as u64,
        );
        let expected_opens = open_attempts;
        let mut bytes = Vec::new();
        for _ in 0..32 {
            for (pack, expected) in [(&content, &b"content"[..]), (&evidence, &b"evidenc"[..])] {
                pass.read(
                    SealedGenerationSegmentReadV1::Range {
                        digest: &pack.digest,
                        size_bytes: pack.size_bytes,
                        offset: 0,
                        length: 7,
                    },
                    &mut bytes,
                )
                .unwrap();
                assert_eq!(bytes, expected);
            }
        }
        assert_eq!(open_attempts, expected_opens);
        assert!(
            pass.read(
                SealedGenerationSegmentReadV1::Range {
                    digest: &content.digest,
                    size_bytes: content.size_bytes,
                    offset: content.size_bytes,
                    length: 1,
                },
                &mut bytes
            )
            .is_err()
        );
        assert_eq!(bytes, [] as [u8; 0]);
        let unknown = identity(b"unknown");
        for request in [
            SealedGenerationSegmentReadV1::Whole {
                digest: &content.digest,
                size_bytes: content.size_bytes + 1,
            },
            SealedGenerationSegmentReadV1::Whole {
                digest: &unknown.digest,
                size_bytes: unknown.size_bytes,
            },
        ] {
            assert!(pass.read(request, &mut bytes).is_err());
            assert_eq!(bytes, [] as [u8; 0]);
        }
        assert_eq!(
            open_attempts, expected_opens,
            "malformed reads do not open paths"
        );
        pass.read(
            SealedGenerationSegmentReadV1::Whole {
                digest: &content.digest,
                size_bytes: content.size_bytes,
            },
            &mut bytes,
        )
        .unwrap();
        assert_eq!(bytes, b"content payload");
        assert_eq!(open_attempts, expected_opens);
    }

    #[test]
    fn preparation_pass_reads_pending_pack_and_refuses_invalid_rosters() {
        let root = TempDir::new().unwrap();
        let bytes = b"unpublished content";
        let pack = identity(bytes);
        let suffix = sha256_hex_suffix(pack.digest.as_str()).unwrap();
        let canonical = root.path().join(format!("segment-{suffix}.json"));
        let staged = root.path().join("pending-content");
        std::fs::write(&staged, bytes).unwrap();
        let pending = BTreeMap::from([(canonical, staged)]);
        let mut pass = GenerationPackReadPassV1::open(
            root.path(),
            std::slice::from_ref(&pack),
            Some(&pending),
            &mut PackReadOpenAttemptsV1::default(),
        )
        .unwrap();
        let mut read = Vec::new();
        pass.read(
            SealedGenerationSegmentReadV1::Whole {
                digest: &pack.digest,
                size_bytes: pack.size_bytes,
            },
            &mut read,
        )
        .unwrap();
        assert_eq!(read, bytes);

        let mut contradictory = pack.clone();
        contradictory.size_bytes += 1;
        assert!(
            GenerationPackReadPassV1::open(
                root.path(),
                &[pack.clone(), contradictory],
                Some(&pending),
                &mut PackReadOpenAttemptsV1::default(),
            )
            .is_err()
        );
        let second = write_pack(root.path(), b"second");
        let third = write_pack(root.path(), b"third");
        assert!(
            GenerationPackReadPassV1::open(
                root.path(),
                &[pack, second, third],
                Some(&pending),
                &mut PackReadOpenAttemptsV1::default()
            )
            .is_err()
        );
    }

    #[test]
    fn partial_admission_keeps_the_real_open_count_on_failure() {
        let root = TempDir::new().unwrap();
        let content = write_pack(root.path(), b"retained content");
        let missing = identity(b"missing evidence");
        let mut opens = PackReadOpenAttemptsV1::default();
        assert!(
            GenerationPackReadPassV1::open(
                root.path(),
                &[content.clone(), missing],
                None,
                &mut opens,
            )
            .is_err()
        );
        let one_pack_opens = if cfg!(windows) { 4 } else { 1 };
        let missing_metadata_opens = u64::from(cfg!(windows));
        assert_eq!(opens.payload, one_pack_opens + missing_metadata_opens);
        let suffix = sha256_hex_suffix(content.digest.as_str()).unwrap();
        std::fs::write(
            root.path().join(format!("segment-{suffix}.json")),
            b"retained content",
        )
        .expect("failed admission released every immutable lease");
        let pass =
            GenerationPackReadPassV1::open(root.path(), &[content], None, &mut opens).unwrap();
        assert_eq!(opens.payload, one_pack_opens * 2 + missing_metadata_opens);
        drop(pass);
    }

    #[cfg(unix)]
    #[test]
    fn pass_refuses_same_inode_mutation_and_replaced_path() {
        let root = TempDir::new().unwrap();
        let pack = write_pack(root.path(), b"original");
        let suffix = sha256_hex_suffix(pack.digest.as_str()).unwrap();
        let path = root.path().join(format!("segment-{suffix}.json"));
        for replace in [false, true] {
            std::fs::write(&path, b"original").unwrap();
            set_modified(&path, 1);
            let mut opens = PackReadOpenAttemptsV1::default();
            let mut pass = GenerationPackReadPassV1::open(
                root.path(),
                std::slice::from_ref(&pack),
                None,
                &mut opens,
            )
            .unwrap();
            let request = SealedGenerationSegmentReadV1::Range {
                digest: &pack.digest,
                size_bytes: pack.size_bytes,
                offset: 0,
                length: 3,
            };
            let mut bytes = Vec::new();
            pass.read(request, &mut bytes).unwrap();
            assert_eq!(bytes, b"ori");
            if replace {
                std::fs::rename(&path, root.path().join("displaced-pack")).unwrap();
            }
            std::fs::write(&path, b"modified").unwrap();
            set_modified(&path, if replace { 1 } else { 2 });
            assert!(pass.read(request, &mut bytes).is_err());
            assert_eq!(bytes, [] as [u8; 0]);
            assert!(pass.is_invalidated());
            let refused_opens = opens;
            std::fs::write(&path, b"original").unwrap();
            set_modified(&path, 1);
            bytes.extend_from_slice(b"stale");
            assert!(pass.read(request, &mut bytes).is_err());
            assert_eq!(bytes, [] as [u8; 0]);
            assert_eq!(opens, refused_opens, "an invalidated pass never reopens");
        }
    }

    #[test]
    fn pass_keeps_the_canonical_openers_original_metadata_baseline() {
        let root = TempDir::new().unwrap();
        let pack = write_pack(root.path(), b"original");
        let suffix = sha256_hex_suffix(pack.digest.as_str()).unwrap();
        let path = root.path().join(format!("segment-{suffix}.json"));
        set_modified(&path, 1);
        #[cfg(not(windows))]
        let segment = DaemonCodeIndexPublicationStoreV1::open_pinned_partitioned_segment_at(
            &path,
            SealedGenerationSegmentReadV1::Whole {
                digest: &pack.digest,
                size_bytes: pack.size_bytes,
            },
            &mut 0,
        )
        .unwrap();
        #[cfg(windows)]
        let segment = GenerationPackReadPassV1::open_immutable_pack(
            &path,
            &pack,
            &mut PackReadOpenAttemptsV1::default(),
        )
        .unwrap();
        #[cfg(not(windows))]
        std::fs::write(&path, b"modified").unwrap();
        set_modified(&path, 2);
        let pinned = PinnedPackV1 { segment, path };
        assert!(pinned.require_unchanged().is_err());
    }

    #[cfg(windows)]
    #[test]
    fn immutable_pass_blocks_same_metadata_replacement_until_release() {
        let root = TempDir::new().unwrap();
        let pack = write_pack(root.path(), b"original");
        let suffix = sha256_hex_suffix(pack.digest.as_str()).unwrap();
        let path = root.path().join(format!("segment-{suffix}.json"));
        set_modified(&path, 1);
        let original = path.metadata().unwrap();
        let replacement_path = root.path().join("replacement");
        std::fs::write(&replacement_path, b"modified").unwrap();
        {
            let replacement = std::fs::OpenOptions::new()
                .write(true)
                .open(&replacement_path)
                .unwrap();
            replacement
                .set_times(
                    std::fs::FileTimes::new()
                        .set_created(original.created().unwrap())
                        .set_modified(original.modified().unwrap()),
                )
                .unwrap();
            let metadata = replacement.metadata().unwrap();
            assert_eq!(metadata.file_attributes(), original.file_attributes());
            assert_eq!(metadata.file_size(), original.file_size());
            assert_eq!(metadata.creation_time(), original.creation_time());
            assert_eq!(metadata.last_write_time(), original.last_write_time());
        }
        let mut opens = PackReadOpenAttemptsV1::default();
        let mut pass = GenerationPackReadPassV1::open(
            root.path(),
            std::slice::from_ref(&pack),
            None,
            &mut opens,
        )
        .unwrap();
        let admitted_opens = opens;
        let displaced = root.path().join("displaced");
        assert!(std::fs::OpenOptions::new().write(true).open(&path).is_err());
        assert!(std::fs::remove_file(&path).is_err());
        assert!(std::fs::rename(&path, &displaced).is_err());
        assert!(std::fs::rename(&replacement_path, &path).is_err());
        let request = SealedGenerationSegmentReadV1::Whole {
            digest: &pack.digest,
            size_bytes: pack.size_bytes,
        };
        let mut bytes = Vec::new();
        for _ in 0..32 {
            pass.read(request, &mut bytes).unwrap();
            assert_eq!(bytes, b"original");
        }
        assert_eq!(opens, admitted_opens);
        drop(pass);
        std::fs::rename(&path, &displaced).expect("closed pass releases leaf and ancestor guards");
        std::fs::rename(&replacement_path, &path)
            .expect("replacement is allowed after the pass ends");
        let mut replacement = GenerationPackReadPassV1::open(
            root.path(),
            std::slice::from_ref(&pack),
            None,
            &mut opens,
        )
        .unwrap();
        assert!(
            replacement.read(request, &mut bytes).is_err(),
            "a new pass must still authenticate whole bytes"
        );
        assert_eq!(bytes, [] as [u8; 0]);
        assert!(replacement.is_invalidated());
    }

    #[cfg(windows)]
    #[test]
    fn metadata_change_poison_releases_all_immutable_pack_owners() {
        let root = TempDir::new().unwrap();
        let content = write_pack(root.path(), b"original");
        let evidence = write_pack(root.path(), b"evidence");
        let content_path = root.path().join(format!(
            "segment-{}.json",
            sha256_hex_suffix(content.digest.as_str()).unwrap()
        ));
        let evidence_path = root.path().join(format!(
            "segment-{}.json",
            sha256_hex_suffix(evidence.digest.as_str()).unwrap()
        ));
        set_modified(&content_path, 1);
        let mut opens = PackReadOpenAttemptsV1::default();
        let mut pass = GenerationPackReadPassV1::open(
            root.path(),
            &[content.clone(), evidence],
            None,
            &mut opens,
        )
        .unwrap();
        let admitted_opens = opens;
        // Attribute-only handles are outside the data-sharing fence, so the
        // original metadata must still be checked on every read.
        set_modified(&content_path, 2);
        let request = SealedGenerationSegmentReadV1::Range {
            digest: &content.digest,
            size_bytes: content.size_bytes,
            offset: 0,
            length: 3,
        };
        let mut bytes = b"stale".to_vec();
        assert!(pass.read(request, &mut bytes).is_err());
        assert_eq!(bytes, [] as [u8; 0]);
        assert!(pass.is_invalidated());
        std::fs::write(&content_path, b"original").expect("poison releases the content lease");
        std::fs::write(&evidence_path, b"evidence").expect("poison releases the evidence lease");
        set_modified(&content_path, 1);
        bytes.extend_from_slice(b"stale");
        assert!(pass.read(request, &mut bytes).is_err());
        assert_eq!(bytes, [] as [u8; 0]);
        assert_eq!(opens, admitted_opens, "poisoned passes do not reopen");
    }

    #[cfg(windows)]
    #[test]
    fn immutable_admission_counts_denied_writer_and_recovers_after_release() {
        let root = TempDir::new().unwrap();
        let pack = write_pack(root.path(), b"original");
        let suffix = sha256_hex_suffix(pack.digest.as_str()).unwrap();
        let path = root.path().join(format!("segment-{suffix}.json"));
        let writer = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        let mut opens = PackReadOpenAttemptsV1::default();
        assert!(
            GenerationPackReadPassV1::open(
                root.path(),
                std::slice::from_ref(&pack),
                None,
                &mut opens
            )
            .is_err()
        );
        assert_eq!(opens.payload, 2, "metadata and the denied leaf both count");
        assert!(opens.ancestors > 0);
        drop(writer);
        let mut pass = GenerationPackReadPassV1::open(
            root.path(),
            std::slice::from_ref(&pack),
            None,
            &mut opens,
        )
        .unwrap();
        assert_eq!(opens.payload, 6);
        let mut bytes = Vec::new();
        pass.read(
            SealedGenerationSegmentReadV1::Whole {
                digest: &pack.digest,
                size_bytes: pack.size_bytes,
            },
            &mut bytes,
        )
        .unwrap();
        assert_eq!(bytes, b"original");
    }

    #[cfg(windows)]
    #[test]
    fn named_validation_counts_failed_pathname_opens() {
        let root = TempDir::new().unwrap();
        let pack = write_pack(root.path(), b"original");
        let suffix = sha256_hex_suffix(pack.digest.as_str()).unwrap();
        let path = root.path().join(format!("segment-{suffix}.json"));
        let exclusive = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&path)
            .unwrap();
        let admitted = exclusive.metadata().unwrap();
        let mut opens = 0;
        assert!(
            DaemonCodeIndexPublicationStoreV1::same_windows_segment_identity(
                &path, &admitted, &exclusive, &mut opens,
            )
            .is_err()
        );
        assert_eq!(
            opens, 1,
            "sharing denial is still a real pathname open attempt"
        );
        drop(exclusive);
        let file = std::fs::File::open(&path).unwrap();
        let admitted = file.metadata().unwrap();
        assert!(
            DaemonCodeIndexPublicationStoreV1::same_windows_segment_identity(
                &path, &admitted, &file, &mut opens,
            )
            .unwrap()
        );
        assert_eq!(opens, 3, "the named handle and final metadata lookup count");
    }

    fn set_modified(path: &Path, seconds: u64) {
        let mut options = std::fs::OpenOptions::new();
        #[cfg(windows)]
        options.access_mode(0x100); // FILE_WRITE_ATTRIBUTES does not request data writes.
        #[cfg(not(windows))]
        options.write(true);
        options
            .open(path)
            .unwrap()
            .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(seconds))
            .unwrap();
    }
}
