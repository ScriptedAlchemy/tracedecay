//! Physical pack handles owned by one already-admitted generation read pass.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use tracedecay_code_index::production::{
    CodeIndexProductionErrorV1, SealedGenerationSegmentIdentityV1, SealedGenerationSegmentReadV1,
};
use tracedecay_domain::sha256_hex_suffix;

use super::{DaemonCodeIndexPublicationStoreV1, PinnedGenerationSegmentV1};

struct PinnedPackV1 {
    segment: PinnedGenerationSegmentV1,
    path: PathBuf,
}

impl PinnedPackV1 {
    fn require_unchanged(
        &self,
        _open_attempts: &mut u64,
    ) -> Result<(), CodeIndexProductionErrorV1> {
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
        let unchanged = DaemonCodeIndexPublicationStoreV1::same_windows_segment_identity(
            &self.path,
            &self.segment.admitted_metadata,
            &self.segment.file,
            _open_attempts,
        )?;
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
    /// `open_attempts` belongs to the operation, so partial admission reports
    /// every attempted payload open even when a later pack cannot be pinned.
    pub(super) fn open(
        root: &Path,
        identities: &[SealedGenerationSegmentIdentityV1],
        pending: Option<&BTreeMap<PathBuf, PathBuf>>,
        open_attempts: &mut u64,
    ) -> Result<Self, CodeIndexProductionErrorV1> {
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
            let segment = DaemonCodeIndexPublicationStoreV1::open_pinned_partitioned_segment_at(
                path,
                SealedGenerationSegmentReadV1::Whole {
                    digest: &identity.digest,
                    size_bytes: identity.size_bytes,
                },
                open_attempts,
            )?;
            let pinned = PinnedPackV1 {
                segment,
                path: path.to_path_buf(),
            };
            *slot = Some(pinned);
        }
        Ok(pass)
    }

    pub(super) fn is_invalidated(&self) -> bool {
        self.failed
    }

    pub(super) fn retained_heap_bytes(&self) -> usize {
        self.packs.iter().flatten().fold(0_usize, |bytes, pack| {
            bytes
                .saturating_add(pack.segment.digest.capacity())
                .saturating_add(pack.path.capacity())
        })
    }

    pub(super) fn read(
        &mut self,
        request: SealedGenerationSegmentReadV1<'_>,
        into: &mut Vec<u8>,
        open_attempts: &mut u64,
    ) -> Result<(), CodeIndexProductionErrorV1> {
        into.clear();
        if self.failed {
            return Err(CodeIndexProductionErrorV1::Contract(
                "sealed generation pack read pass was invalidated".to_owned(),
            ));
        }
        // Malformed requests do not revoke healthy admitted pack handles.
        let index = self.validate_request(request)?;
        let result = self.read_inner(index, request, into, open_attempts);
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
        open_attempts: &mut u64,
    ) -> Result<(), CodeIndexProductionErrorV1> {
        let pinned = self.packs[index].as_mut().ok_or_else(|| {
            CodeIndexProductionErrorV1::Contract(
                "sealed generation read pass lost an admitted pack".to_owned(),
            )
        })?;
        pinned.require_unchanged(open_attempts)?;
        let result = DaemonCodeIndexPublicationStoreV1::read_pinned_partitioned_segment(
            &mut pinned.segment,
            request,
            into,
        );
        pinned.require_unchanged(open_attempts)?;
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

    use super::{DaemonCodeIndexPublicationStoreV1, GenerationPackReadPassV1, PinnedPackV1};

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
        let mut open_attempts = 0;
        let mut pass = GenerationPackReadPassV1::open(
            root.path(),
            &[content.clone(), evidence.clone(), content.clone()],
            None,
            &mut open_attempts,
        )
        .unwrap();
        let mut expected_opens = if cfg!(windows) { 4 } else { 2 };
        let validation_opens = if cfg!(windows) { 2 } else { 0 };
        assert_eq!(open_attempts, expected_opens);
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
                    &mut open_attempts,
                )
                .unwrap();
                expected_opens += validation_opens;
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
                &mut bytes,
                &mut open_attempts,
            )
            .is_err()
        );
        assert!(bytes.is_empty());
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
            assert!(pass.read(request, &mut bytes, &mut open_attempts).is_err());
            assert!(bytes.is_empty());
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
            &mut open_attempts,
        )
        .unwrap();
        assert_eq!(bytes, b"content payload");
        assert_eq!(open_attempts, expected_opens + validation_opens);
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
        let mut pass =
            GenerationPackReadPassV1::open(root.path(), &[pack.clone()], Some(&pending), &mut 0)
                .unwrap();
        let mut read = Vec::new();
        pass.read(
            SealedGenerationSegmentReadV1::Whole {
                digest: &pack.digest,
                size_bytes: pack.size_bytes,
            },
            &mut read,
            &mut 0,
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
                &mut 0,
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
                &mut 0
            )
            .is_err()
        );
    }

    #[test]
    fn partial_admission_keeps_the_real_open_count_on_failure() {
        let root = TempDir::new().unwrap();
        let content = write_pack(root.path(), b"retained content");
        let missing = identity(b"missing evidence");
        let mut opens = 0;
        assert!(
            GenerationPackReadPassV1::open(
                root.path(),
                &[content.clone(), missing],
                None,
                &mut opens,
            )
            .is_err()
        );
        let one_pack_opens = if cfg!(windows) { 2 } else { 1 };
        assert_eq!(opens, one_pack_opens);
        let pass =
            GenerationPackReadPassV1::open(root.path(), &[content], None, &mut opens).unwrap();
        assert_eq!(opens, one_pack_opens * 2);
        drop(pass);
    }

    #[test]
    fn pass_refuses_same_inode_mutation_and_replaced_path() {
        let root = TempDir::new().unwrap();
        let pack = write_pack(root.path(), b"original");
        let suffix = sha256_hex_suffix(pack.digest.as_str()).unwrap();
        let path = root.path().join(format!("segment-{suffix}.json"));
        for replace in [false, true] {
            std::fs::write(&path, b"original").unwrap();
            set_modified(&path, 1);
            let mut opens = 0;
            let mut pass =
                GenerationPackReadPassV1::open(root.path(), &[pack.clone()], None, &mut opens)
                    .unwrap();
            let request = SealedGenerationSegmentReadV1::Range {
                digest: &pack.digest,
                size_bytes: pack.size_bytes,
                offset: 0,
                length: 3,
            };
            let mut bytes = Vec::new();
            pass.read(request, &mut bytes, &mut opens).unwrap();
            assert_eq!(bytes, b"ori");
            if replace {
                std::fs::rename(&path, root.path().join("displaced-pack")).unwrap();
            }
            std::fs::write(&path, b"modified").unwrap();
            set_modified(&path, if replace { 1 } else { 2 });
            #[cfg(windows)]
            if replace {
                let original = &pass.packs[0].as_ref().unwrap().segment.admitted_metadata;
                let replacement = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
                replacement
                    .set_times(
                        std::fs::FileTimes::new()
                            .set_created(original.created().unwrap())
                            .set_modified(original.modified().unwrap()),
                    )
                    .unwrap();
                let metadata = replacement.metadata().unwrap();
                assert!(metadata.is_file());
                assert_eq!(metadata.file_attributes(), original.file_attributes());
                assert_eq!(metadata.file_size(), original.file_size());
                assert_eq!(metadata.creation_time(), original.creation_time());
                assert_eq!(metadata.last_write_time(), original.last_write_time());
            }
            assert!(pass.read(request, &mut bytes, &mut opens).is_err());
            assert!(bytes.is_empty());
            assert!(pass.is_invalidated());
            let refused_opens = opens;
            std::fs::write(&path, b"original").unwrap();
            set_modified(&path, 1);
            bytes.extend_from_slice(b"stale");
            assert!(pass.read(request, &mut bytes, &mut opens).is_err());
            assert!(bytes.is_empty());
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
        let mut opens = 0;
        let segment = DaemonCodeIndexPublicationStoreV1::open_pinned_partitioned_segment_at(
            &path,
            SealedGenerationSegmentReadV1::Whole {
                digest: &pack.digest,
                size_bytes: pack.size_bytes,
            },
            &mut opens,
        )
        .unwrap();
        std::fs::write(&path, b"modified").unwrap();
        set_modified(&path, 2);
        let pinned = PinnedPackV1 { segment, path };
        assert!(pinned.require_unchanged(&mut opens).is_err());
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
    }

    fn set_modified(path: &Path, seconds: u64) {
        std::fs::OpenOptions::new()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(seconds))
            .unwrap();
    }
}
