//! Durable source-freshness evidence for one activated code generation.
//!
//! Two layers with different authority. The stat signature over every
//! candidate's `(logical path, len, mtime)` is a negative cache: unequal
//! metadata proves the worktree moved without reading a byte, but equal
//! metadata proves nothing — a same-length rewrite whose mtime was preserved
//! (`rsync -a`, `cp --preserve`, `touch -d`, restore tools, a timestamp
//! collision) leaves it unchanged. Source currency is therefore only ever
//! settled against the sealed generation's own per-file content digests
//! (`SanitizedCodeSnapshotV1::files`), re-derived from the bytes on disk
//! through the same bounded read + sanitize + digest path reconciliation uses.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::UNIX_EPOCH;

use gix::bstr::BStr;
use rayon::prelude::*;
use sha2::{Digest, Sha256};
use tracedecay_code_index::production::CodeIndexIgnoredSourceAdmissionV1;
use tracedecay_domain::canonical_text::encode_tagged_lowercase_hex;
use tracedecay_domain::{
    ContentDigest, LanguageId, SanitizedCodeSnapshotV1, SnapshotFileDispositionV1,
};

use super::{CodeIndexSchedulerErrorV1, classification, ignored_dependencies, privacy};
use crate::code_index::chunks::content_digest;
use crate::code_index::languages::{LanguageRegistry, StaticLanguageRegistry};
use crate::code_index::parallelism;
use crate::config::is_generated_path_segment;

const FRESHNESS_WITNESS_FILE_NAME: &str = "freshness_witness.v1";

/// One stat-swept source candidate, carrying what the content proof needs to
/// re-derive its canonical digest exactly as capture does.
struct StatCandidateV1 {
    logical_path: String,
    language: LanguageId,
    explicitly_admitted: bool,
}

/// One stat sweep over every ordinary or explicitly admitted source
/// candidate: the cheap signature plus the candidate roster it hashed, so the
/// content proof compares exactly the files the signature covered.
pub struct WorktreeStatSweepV1 {
    pub signature: String,
    candidates: Vec<StatCandidateV1>,
}

/// A cheap stat-level sweep over every ordinary or explicitly admitted source
/// candidate. Ignored admissions are part of the sweep even though gix
/// deliberately omits them from its ordinary candidate set.
#[hotpath::measure(label = "daemon.code_index.freshness.stat_signature")]
pub fn worktree_stat_sweep(
    project_root: &Path,
    ignored_source_admissions: &[CodeIndexIgnoredSourceAdmissionV1],
) -> Result<WorktreeStatSweepV1, CodeIndexSchedulerErrorV1> {
    let repository = gix::open(project_root)
        .map_err(|error| CodeIndexSchedulerErrorV1::Git(error.to_string()))?;
    let classification = classification::WorktreeChangeClassificationV1::classify(&repository)
        .map_err(|error| CodeIndexSchedulerErrorV1::Git(error.to_string()))?;
    let registry = StaticLanguageRegistry::new();
    let admitted_paths = ignored_source_admissions
        .iter()
        .map(|admission| admission.logical_path.as_str())
        .collect::<BTreeSet<_>>();
    let mut candidate_paths = classification.candidate_paths();
    candidate_paths.extend(admitted_paths.iter().map(|path| (*path).to_owned()));
    // One sweep span plus an entries gauge: the stat walk is O(candidates) and
    // must never publish one profiler event per file.
    hotpath::gauge!("daemon.code_index.freshness.stat_signature.candidates")
        .set(candidate_paths.len() as u64);
    let mut buf = Vec::new();
    let mut candidates = Vec::new();
    for logical_path in candidate_paths {
        let absolute = project_root.join(&logical_path);
        let Some(extension) = absolute.extension().and_then(|value| value.to_str()) else {
            continue;
        };
        let Some(descriptor) = registry.descriptor_for_extension(&extension.to_lowercase()) else {
            continue;
        };
        let Ok(metadata) = std::fs::metadata(&absolute) else {
            continue;
        };
        if !metadata.is_file() {
            continue;
        }
        let mtime_nanos = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map_or(0u128, |elapsed| elapsed.as_nanos());
        buf.extend_from_slice(logical_path.as_bytes());
        buf.push(0);
        buf.extend_from_slice(&metadata.len().to_le_bytes());
        buf.extend_from_slice(&mtime_nanos.to_le_bytes());
        buf.push(0xff);
        candidates.push(StatCandidateV1 {
            explicitly_admitted: admitted_paths.contains(logical_path.as_str()),
            logical_path,
            language: descriptor.language.clone(),
        });
    }
    Ok(WorktreeStatSweepV1 {
        signature: encode_tagged_lowercase_hex("sha256:", &Sha256::digest(&buf)),
        candidates,
    })
}

/// The per-file content identities one sealed generation was reconciled
/// against: `logical path → content digest` of the sanitized bytes for every
/// present file in the generation's snapshot manifest, plus the snapshot's
/// own content identity naming which sealed source these digests belong to.
/// A view of the sealed generation, not a second authority.
#[derive(Clone, Debug)]
pub struct SourceContentManifestV1 {
    snapshot_content_identity: ContentDigest,
    files: Arc<BTreeMap<String, ContentDigest>>,
}

impl SourceContentManifestV1 {
    pub fn for_snapshot(snapshot: &SanitizedCodeSnapshotV1) -> Self {
        Self {
            snapshot_content_identity: snapshot.content_identity.clone(),
            files: Arc::new(
                snapshot
                    .files
                    .iter()
                    .filter(|file| file.disposition == SnapshotFileDispositionV1::Present)
                    .map(|file| (file.logical_path.clone(), file.content_digest.clone()))
                    .collect(),
            ),
        }
    }

    /// Whether this manifest describes the sealed source `snapshot_content_identity`
    /// names: the reconcile that established it verified exactly that snapshot.
    pub fn describes_snapshot(&self, snapshot_content_identity: &ContentDigest) -> bool {
        self.snapshot_content_identity == *snapshot_content_identity
    }
}

impl WorktreeStatSweepV1 {
    /// Whether the bytes on disk still carry exactly the content identities
    /// the generation sealed: every present manifest file is still a
    /// candidate, and every candidate's re-derived canonical digest equals
    /// its manifest entry (or the privacy boundary withholds it and the
    /// manifest agrees). Read + sanitize + digest is per-file pure work over
    /// independent paths, so it fans out across the indexing pool exactly as
    /// capture does.
    #[hotpath::measure(label = "daemon.code_index.freshness.content_verify")]
    pub fn content_matches(
        &self,
        project_root: &Path,
        manifest: &SourceContentManifestV1,
        shutting_down: &AtomicBool,
    ) -> bool {
        let candidates = self
            .candidates
            .iter()
            .filter(|candidate| {
                candidate.explicitly_admitted || !is_generated_path_segment(&candidate.logical_path)
            })
            .collect::<Vec<_>>();
        hotpath::gauge!("daemon.code_index.freshness.content_verify.candidates")
            .set(candidates.len() as u64);
        let candidate_paths = candidates
            .iter()
            .map(|candidate| candidate.logical_path.as_str())
            .collect::<BTreeSet<_>>();
        if !manifest
            .files
            .keys()
            .all(|logical_path| candidate_paths.contains(logical_path.as_str()))
        {
            return false;
        }
        let Ok(disputed) = parallelism::install(|| {
            candidates
                .par_iter()
                .copied()
                .filter(|candidate| {
                    parallelism::with_background_cpu_permit(|| {
                        shutting_down.load(Ordering::Acquire)
                            || !candidate_matches_manifest(project_root, candidate, manifest)
                    })
                })
                .collect::<Vec<_>>()
        }) else {
            return false;
        };
        if shutting_down.load(Ordering::Acquire) {
            return false;
        }
        disputed.is_empty()
            || tracked_files_match_after_clean_filters(project_root, &disputed, manifest)
    }
}

fn read_candidate(
    project_root: &Path,
    candidate: &StatCandidateV1,
) -> Result<Vec<u8>, CodeIndexSchedulerErrorV1> {
    if candidate.explicitly_admitted {
        ignored_dependencies::read_explicitly_admitted_source(
            project_root,
            &candidate.logical_path,
            None,
        )
    } else {
        ignored_dependencies::read_bounded_snapshot_source(
            &project_root.join(&candidate.logical_path),
            None,
        )
    }
}

/// The canonical content digest capture would seal for `raw`: the digest of
/// the sanitized bytes.
fn sanitized_digest(
    candidate: &StatCandidateV1,
    raw: &[u8],
) -> Result<ContentDigest, CodeIndexSchedulerErrorV1> {
    privacy::sanitize_code_file(&candidate.language, raw)
        .map(|(bytes, _, _)| content_digest(&bytes))
}

fn candidate_matches_manifest(
    project_root: &Path,
    candidate: &StatCandidateV1,
    manifest: &SourceContentManifestV1,
) -> bool {
    let digest =
        read_candidate(project_root, candidate).and_then(|raw| sanitized_digest(candidate, &raw));
    match (digest, manifest.files.get(&candidate.logical_path)) {
        (Ok(digest), Some(expected)) => digest == *expected,
        // The privacy boundary withholds this file from every generation, so
        // its absence from the manifest is the one consistent state.
        (Err(CodeIndexSchedulerErrorV1::Privacy(_)), None) => true,
        _ => false,
    }
}

/// A tracked file whose bytes on disk differ from its sealed digest may still
/// be exactly what the generation sealed: a clean-tree generation captures
/// HEAD's blobs, and git's clean filters (`core.autocrlf`, `eol`, `ident`,
/// filter drivers) are what separate those blobs from the checkout. Running
/// the disk bytes through the repository's own filter pipeline — the same
/// conversion gix status applies before it compares content — settles whether
/// the file is current. Untracked and explicitly admitted sources have no
/// blob to be sealed from, so their raw mismatch is final.
fn tracked_files_match_after_clean_filters(
    project_root: &Path,
    disputed: &[&StatCandidateV1],
    manifest: &SourceContentManifestV1,
) -> bool {
    let Ok(repository) = gix::open(project_root) else {
        return false;
    };
    let Ok((mut pipeline, index)) = repository.filter_pipeline(None) else {
        return false;
    };
    disputed.iter().all(|candidate| {
        let Some(expected) = manifest.files.get(&candidate.logical_path) else {
            return false;
        };
        if candidate.explicitly_admitted
            || index
                .entry_by_path(BStr::new(candidate.logical_path.as_str()))
                .is_none()
        {
            return false;
        }
        let rela_path = Path::new(&candidate.logical_path);
        let Ok(file) = std::fs::File::open(project_root.join(rela_path)) else {
            return false;
        };
        let Ok(mut converted) = pipeline.convert_to_git(file, rela_path, &index) else {
            return false;
        };
        let mut bytes = Vec::new();
        converted.read_to_end(&mut bytes).is_ok()
            && sanitized_digest(candidate, &bytes).is_ok_and(|digest| digest == *expected)
    })
}

/// What the last completed reconcile proved the worktree's source to be, in
/// the two forms a later probe consults: the stat signature as the negative
/// cache and the generation's sealed file digests as the proof.
#[derive(Clone)]
pub struct ReconciledSourceWitnessV1 {
    pub stat_signature: String,
    pub content_manifest: SourceContentManifestV1,
}

impl ReconciledSourceWitnessV1 {
    pub fn new(stat_signature: String, snapshot: &SanitizedCodeSnapshotV1) -> Self {
        Self {
            stat_signature,
            content_manifest: SourceContentManifestV1::for_snapshot(snapshot),
        }
    }

    /// Metadata differs → not current, without reading a byte. Metadata equal
    /// → current only when every candidate's content digest still matches
    /// the sealed manifest.
    pub fn matches_worktree(
        &self,
        project_root: &Path,
        ignored_source_admissions: &[CodeIndexIgnoredSourceAdmissionV1],
        shutting_down: &AtomicBool,
    ) -> bool {
        let Ok(sweep) = worktree_stat_sweep(project_root, ignored_source_admissions) else {
            return false;
        };
        sweep.signature == self.stat_signature
            && sweep.content_matches(project_root, &self.content_manifest, shutting_down)
    }
}

/// Durable binding between one sealed generation and the exact ordinary plus
/// ignored-source state against which it was reconciled. An expendable
/// restart optimization: it names the generation whose sealed file digests
/// are the content authority, and carries the stat signature that lets a
/// moved tree skip straight to reconcile.
pub struct RestoreFreshnessWitnessV1 {
    pub generation_id: String,
    pub git_metadata_signature: String,
    pub stat_signature: String,
    pub repository_parse_identity_digest: String,
    pub ignored_source_admissions_digest: String,
    pub ignored_source_paths: Vec<String>,
}

impl RestoreFreshnessWitnessV1 {
    fn witness_path(store_root: &Path) -> PathBuf {
        store_root.join(FRESHNESS_WITNESS_FILE_NAME)
    }

    fn encode(&self) -> String {
        let mut fields = vec![
            self.generation_id.clone(),
            self.git_metadata_signature.clone(),
            self.stat_signature.clone(),
            self.repository_parse_identity_digest.clone(),
            self.ignored_source_admissions_digest.clone(),
            self.ignored_source_paths.len().to_string(),
        ];
        fields.extend(self.ignored_source_paths.iter().cloned());
        fields.push(String::new());
        fields.join("\n")
    }

    fn decode(contents: &str) -> Option<Self> {
        let fields = contents.lines().collect::<Vec<_>>();
        if fields.len() < 6 {
            return None;
        }
        let path_count = fields[5].parse::<usize>().ok()?;
        if fields.len() != 6usize.checked_add(path_count)?
            || fields[..5].iter().any(|field| field.is_empty())
            || fields[6..].iter().any(|path| path.is_empty())
        {
            return None;
        }
        Some(Self {
            generation_id: fields[0].to_owned(),
            git_metadata_signature: fields[1].to_owned(),
            stat_signature: fields[2].to_owned(),
            repository_parse_identity_digest: fields[3].to_owned(),
            ignored_source_admissions_digest: fields[4].to_owned(),
            ignored_source_paths: fields[6..].iter().map(|path| (*path).to_owned()).collect(),
        })
    }

    pub fn load(store_root: &Path) -> Option<Self> {
        let contents = std::fs::read_to_string(Self::witness_path(store_root)).ok()?;
        Self::decode(&contents)
    }

    /// Atomic replacement makes a torn witness indistinguishable from an
    /// absent witness: either state safely forces a full reconcile on restart.
    pub fn persist(&self, store_root: &Path) {
        let path = Self::witness_path(store_root);
        let temp = store_root.join(format!("{FRESHNESS_WITNESS_FILE_NAME}.tmp"));
        if std::fs::write(&temp, self.encode()).is_ok() {
            let _ = std::fs::rename(&temp, &path);
        }
    }
}
