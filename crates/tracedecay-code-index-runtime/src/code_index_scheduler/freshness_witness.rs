//! Durable source-freshness evidence for one activated code generation.
//!
//! Two layers with different authority. The stat signature over every
//! candidate's `(logical path, len, mtime)` is a negative cache: unequal
//! metadata proves the worktree moved without reading a byte, but equal
//! metadata proves nothing, a same-length rewrite whose mtime was preserved
//! (`rsync -a`, `cp --preserve`, `touch -d`, restore tools, a timestamp
//! collision) leaves it unchanged. Source currency is therefore only ever
//! settled against the sealed generation's own per-file content digests
//! (`SanitizedCodeSnapshotV1::files`), re-derived from the bytes on disk
//! through the same bounded read + sanitize + digest path reconciliation uses.
//!
//! [`SourceSweepCacheV1`] makes re-settling cheap without weakening that
//! authority: a digest re-derived once is reused only while the file's full
//! stat identity, change time included, still holds.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs::Metadata;
use std::io::Read;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use gix::bstr::BStr;
use gix::dir::walk::{Action, Delegate, ForDeletionMode};
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
/// candidate: a negative cache only, settled by [`SourceSweepCacheV1`].
pub struct WorktreeStatSweepV1 {
    pub signature: String,
}

/// A cheap stat-level sweep over every ordinary or explicitly admitted source
/// candidate. Ignored admissions are part of the sweep even though gix
/// deliberately omits them from its ordinary candidate set.
#[hotpath::measure(label = "daemon.code_index.freshness.stat_signature")]
pub fn worktree_stat_sweep(
    project_root: &Path,
    ignored_source_admissions: &[CodeIndexIgnoredSourceAdmissionV1],
) -> Result<WorktreeStatSweepV1, CodeIndexSchedulerErrorV1> {
    let repository = tracedecay_runtime_core::git_open::open(project_root)
        .map_err(|error| CodeIndexSchedulerErrorV1::Git(error.to_string()))?;
    let candidate_roster = source_candidates(&repository, ignored_source_admissions)?;
    // One sweep span plus an entries gauge: the stat walk is O(candidates) and
    // must never publish one profiler event per file.
    hotpath::gauge!("daemon.code_index.freshness.stat_signature.candidates")
        .set(candidate_roster.len() as u64);
    let mut buf = Vec::new();
    for candidate in candidate_roster {
        let Ok(metadata) = std::fs::metadata(project_root.join(&candidate.logical_path)) else {
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
        buf.extend_from_slice(candidate.logical_path.as_bytes());
        buf.push(0);
        buf.extend_from_slice(&metadata.len().to_le_bytes());
        buf.extend_from_slice(&mtime_nanos.to_le_bytes());
        buf.push(0xff);
    }
    Ok(WorktreeStatSweepV1 {
        signature: encode_tagged_lowercase_hex("sha256:", &Sha256::digest(&buf)),
    })
}

/// Every ordinary or explicitly admitted path with a registered language,
/// present or not: the roster a stat sweep checks.
fn source_candidates(
    repository: &gix::Repository,
    ignored_source_admissions: &[CodeIndexIgnoredSourceAdmissionV1],
) -> Result<Vec<StatCandidateV1>, CodeIndexSchedulerErrorV1> {
    let classification = classification::WorktreeChangeClassificationV1::classify(repository)
        .map_err(|error| CodeIndexSchedulerErrorV1::Git(error.to_string()))?;
    let registry = StaticLanguageRegistry::new();
    let admitted_paths = ignored_source_admissions
        .iter()
        .map(|admission| admission.logical_path.as_str())
        .collect::<BTreeSet<_>>();
    let mut candidate_paths = classification.candidate_paths();
    candidate_paths.extend(admitted_paths.iter().map(|path| (*path).to_owned()));
    Ok(candidate_paths
        .into_iter()
        .filter_map(|logical_path| {
            let extension = Path::new(&logical_path).extension()?.to_str()?;
            let descriptor = registry.descriptor_for_extension(&extension.to_lowercase())?;
            Some(StatCandidateV1 {
                explicitly_admitted: admitted_paths.contains(logical_path.as_str()),
                language: descriptor.language.clone(),
                logical_path,
            })
        })
        .collect())
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

/// What capture would seal for one candidate's bytes on disk.
#[derive(Clone, Debug, PartialEq, Eq)]
enum CandidateContentV1 {
    Digest(ContentDigest),
    /// The privacy boundary withholds this file from every generation.
    Withheld,
    Unreadable,
}

impl CandidateContentV1 {
    fn derive(project_root: &Path, candidate: &StatCandidateV1) -> Self {
        match read_candidate(project_root, candidate)
            .and_then(|raw| sanitized_digest(candidate, &raw))
        {
            Ok(digest) => Self::Digest(digest),
            Err(CodeIndexSchedulerErrorV1::Privacy(_)) => Self::Withheld,
            Err(_) => Self::Unreadable,
        }
    }

    fn matches(&self, expected: Option<&ContentDigest>) -> bool {
        match (self, expected) {
            (Self::Digest(digest), Some(expected)) => digest == expected,
            // A withheld file's absence from the manifest is the one
            // consistent state.
            (Self::Withheld, None) => true,
            _ => false,
        }
    }
}

/// Timestamps this close to the moment of a stat can still be shared by a
/// later write (coarse kernel clocks, two-second FAT times), so such a stat
/// cannot yet tell the file's current state from its next one.
const RACY_STAT_WINDOW: Duration = Duration::from_secs(2);

/// One inode's stat identity. On Unix the change time is the kernel's own
/// record of every content or metadata write and cannot be set back
/// (`touch -d`, `cp --preserve`, `rsync -a` all advance it), so an equal
/// settled key proves the bytes behind it unchanged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct StatKeyV1 {
    device: u64,
    inode: u64,
    mode: u32,
    size: u64,
    modified_nanos: i128,
    changed_nanos: i128,
}

impl StatKeyV1 {
    #[cfg(unix)]
    fn of(metadata: &Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            mode: metadata.mode(),
            size: metadata.size(),
            modified_nanos: i128::from(metadata.mtime()) * 1_000_000_000
                + i128::from(metadata.mtime_nsec()),
            changed_nanos: i128::from(metadata.ctime()) * 1_000_000_000
                + i128::from(metadata.ctime_nsec()),
        }
    }

    // ponytail: without a change time a stat cannot prove unchanged bytes, so
    // non-Unix keys never settle and every sweep re-derives every digest, as
    // before this cache. Upgrade path: the Windows change time once std
    // exposes it.
    #[cfg(not(unix))]
    fn of(metadata: &Metadata) -> Self {
        Self {
            device: 0,
            inode: 0,
            mode: 0,
            size: metadata.len(),
            modified_nanos: metadata
                .modified()
                .ok()
                .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                .and_then(|elapsed| i128::try_from(elapsed.as_nanos()).ok())
                .unwrap_or(0),
            changed_nanos: 0,
        }
    }

    /// Whether this key, sampled at `sampled_at`, is old enough that any
    /// later write must produce a different one.
    fn settled(&self, sampled_at: SystemTime) -> bool {
        cfg!(unix)
            && sampled_at
                .checked_sub(RACY_STAT_WINDOW)
                .and_then(|horizon| horizon.duration_since(UNIX_EPOCH).ok())
                .and_then(|horizon| i128::try_from(horizon.as_nanos()).ok())
                .is_some_and(|horizon| self.changed_nanos < horizon)
    }
}

/// The key of whatever is at `path` now, `None` when nothing is.
fn sample(path: &Path) -> std::io::Result<Option<StatKeyV1>> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => Ok(Some(StatKeyV1::of(&metadata))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// A candidate roster and the directory and ignore-rule evidence it was
/// enumerated from. Adding, removing or renaming an entry advances its
/// directory's change time, and ignore rules live in the recorded files, so
/// while every recorded key holds the roster is the one a fresh Git walk
/// would produce, and the probe skips that walk.
struct CachedCandidateRosterV1 {
    git_metadata_signature: String,
    admitted_paths: Vec<String>,
    evidence: Vec<(PathBuf, Option<StatKeyV1>)>,
    candidates: Arc<Vec<StatCandidateV1>>,
}

impl CachedCandidateRosterV1 {
    fn holds(&self, git_metadata_signature: &str, admitted_paths: &[String]) -> bool {
        self.git_metadata_signature == git_metadata_signature
            && self.admitted_paths == admitted_paths
            && parallelism::install(|| {
                self.evidence
                    .par_iter()
                    .all(|(path, key)| sample(path).is_ok_and(|now| now == *key))
            })
            .unwrap_or(false)
    }
}

/// Records every directory the Git walk descends into, keyed before the walk
/// reads it, plus the `.gitignore` each one holds.
struct DirectoryEvidenceV1 {
    root: PathBuf,
    sampled_at: SystemTime,
    evidence: Vec<(PathBuf, Option<StatKeyV1>)>,
    settled: bool,
}

impl DirectoryEvidenceV1 {
    fn record(&mut self, path: PathBuf, directory: bool) {
        let key = sample(&path);
        let settled = match &key {
            Ok(Some(key)) => key.settled(self.sampled_at),
            Ok(None) => !directory,
            Err(_) => false,
        };
        match key {
            Ok(key) if settled => self.evidence.push((path, key)),
            _ => self.settled = false,
        }
    }

    fn record_directory(&mut self, directory: PathBuf) {
        let ignore_file = directory.join(".gitignore");
        self.record(directory, true);
        // An absent `.gitignore` needs no key: creating one advances the
        // directory's change time.
        if sample(&ignore_file).is_ok_and(|key| key.is_some()) {
            self.record(ignore_file, false);
        }
    }
}

impl Delegate for DirectoryEvidenceV1 {
    fn emit(
        &mut self,
        _entry: gix::dir::EntryRef<'_>,
        _collapsed_directory_status: Option<gix::dir::entry::Status>,
    ) -> Action {
        if self.settled {
            Action::Continue(())
        } else {
            Action::Break(())
        }
    }

    fn can_recurse(
        &mut self,
        entry: gix::dir::EntryRef<'_>,
        for_deletion: Option<ForDeletionMode>,
        worktree_root_is_repository: bool,
    ) -> bool {
        let recurse = entry.status.can_recurse(
            entry.disk_kind,
            entry.pathspec_match,
            for_deletion,
            worktree_root_is_repository,
        );
        if recurse && self.settled {
            let directory = self
                .root
                .join(gix::path::from_bstr(entry.rela_path.as_ref()));
            self.record_directory(directory);
        }
        recurse
    }
}

/// Keys every directory and ignore-rule file the candidate roster depends on,
/// before the roster's own walk reads them. `None` when any of them changed
/// too recently to be keyed.
fn roster_evidence(
    repository: &gix::Repository,
    project_root: &Path,
    sampled_at: SystemTime,
) -> Option<Vec<(PathBuf, Option<StatKeyV1>)>> {
    let mut recorder = DirectoryEvidenceV1 {
        root: project_root.to_path_buf(),
        sampled_at,
        evidence: Vec::new(),
        settled: true,
    };
    let common_dir = repository.common_dir();
    recorder.record(common_dir.join("config"), false);
    recorder.record(common_dir.join("info").join("exclude"), false);
    let global_excludes = match repository
        .config_snapshot()
        .trusted_path("core.excludesFile")
    {
        Ok(Some(path)) => Some(path),
        Ok(None) => gix::path::env::xdg_config("ignore", &mut |name| std::env::var_os(name)),
        Err(_) => return None,
    };
    if let Some(path) = global_excludes {
        recorder.record(path, false);
    }
    recorder.record_directory(project_root.to_path_buf());
    let index = repository.index_or_empty().ok()?;
    let options = repository
        .dirwalk_options()
        .ok()?
        .emit_untracked(gix::dir::walk::EmissionMode::Matching);
    let interrupt = AtomicBool::new(false);
    repository
        .dirwalk(
            &index,
            Vec::<gix::bstr::BString>::new(),
            &interrupt,
            options,
            &mut recorder,
        )
        .ok()?;
    recorder.settled.then_some(recorder.evidence)
}

/// What one sweep of the witness checked, for the profiler and the proof.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SourceSweepStatsV1 {
    /// Whether the candidate roster came from a fresh Git walk.
    pub walked: bool,
    pub candidates: usize,
    /// Files whose bytes were read because no settled key vouched for them.
    pub hashed: usize,
}

/// What earlier sweeps proved about the worktree: per-file content digests
/// that hold while each file's settled stat key does, and the last candidate
/// roster with the evidence that keeps it valid. A clean sweep then stats
/// files and directories and reads no bytes. It is a cache of the content
/// proof, not a second authority: every entry was derived from the bytes on
/// disk, and a disagreeing key falls back to re-deriving them.
#[derive(Default)]
pub struct SourceSweepCacheV1 {
    contents: HashMap<String, (StatKeyV1, CandidateContentV1)>,
    roster: Option<CachedCandidateRosterV1>,
}

impl SourceSweepCacheV1 {
    /// Whether the bytes on disk still carry exactly the manifest's content
    /// identities: every present manifest file is still a candidate, and
    /// every candidate's canonical digest equals its manifest entry (or the
    /// privacy boundary withholds it and the manifest agrees). Digests no
    /// settled key vouches for are re-derived across the indexing pool.
    #[hotpath::measure(label = "daemon.code_index.freshness.source_sweep")]
    pub fn witness_matches(
        &mut self,
        project_root: &Path,
        ignored_source_admissions: &[CodeIndexIgnoredSourceAdmissionV1],
        git_metadata_signature: &str,
        manifest: &SourceContentManifestV1,
        shutting_down: &AtomicBool,
    ) -> (bool, SourceSweepStatsV1) {
        let mut stats = SourceSweepStatsV1::default();
        let sampled_at = SystemTime::now();
        let admitted_paths = ignored_source_admissions
            .iter()
            .map(|admission| admission.logical_path.clone())
            .collect::<Vec<_>>();
        let candidates = match self.roster.as_ref() {
            Some(roster) if roster.holds(git_metadata_signature, &admitted_paths) => {
                Arc::clone(&roster.candidates)
            }
            _ => {
                stats.walked = true;
                let Ok(repository) = tracedecay_runtime_core::git_open::open(project_root) else {
                    return (false, stats);
                };
                let evidence = roster_evidence(&repository, project_root, sampled_at);
                let Ok(candidates) = source_candidates(&repository, ignored_source_admissions)
                else {
                    return (false, stats);
                };
                let candidates = Arc::new(candidates);
                let live = candidates
                    .iter()
                    .map(|candidate| candidate.logical_path.as_str())
                    .collect::<BTreeSet<_>>();
                self.contents.retain(|path, _| live.contains(path.as_str()));
                self.roster = evidence.map(|evidence| CachedCandidateRosterV1 {
                    git_metadata_signature: git_metadata_signature.to_owned(),
                    admitted_paths,
                    evidence,
                    candidates: Arc::clone(&candidates),
                });
                candidates
            }
        };
        let Ok(present) = parallelism::install(|| {
            candidates
                .par_iter()
                .filter(|candidate| {
                    candidate.explicitly_admitted
                        || !is_generated_path_segment(&candidate.logical_path)
                })
                .filter_map(|candidate| {
                    let absolute = project_root.join(&candidate.logical_path);
                    let metadata = std::fs::symlink_metadata(&absolute).ok()?;
                    // A link's own key says nothing about its target's bytes.
                    let key = if metadata.file_type().is_symlink() {
                        if !std::fs::metadata(&absolute).is_ok_and(|target| target.is_file()) {
                            return None;
                        }
                        None
                    } else if metadata.is_file() {
                        Some(StatKeyV1::of(&metadata))
                    } else {
                        return None;
                    };
                    Some((candidate, key))
                })
                .collect::<Vec<_>>()
        }) else {
            return (false, stats);
        };
        stats.candidates = present.len();
        hotpath::gauge!("daemon.code_index.freshness.source_sweep.candidates")
            .set(present.len() as u64);
        let present_paths = present
            .iter()
            .map(|(candidate, _)| candidate.logical_path.as_str())
            .collect::<BTreeSet<_>>();
        if !manifest
            .files
            .keys()
            .all(|logical_path| present_paths.contains(logical_path.as_str()))
        {
            return (false, stats);
        }
        let mut disputed = Vec::new();
        let mut unvouched = Vec::new();
        for (candidate, key) in present {
            match self.contents.get(&candidate.logical_path) {
                Some((cached, content)) if Some(*cached) == key => {
                    if !content.matches(manifest.files.get(&candidate.logical_path)) {
                        disputed.push(candidate);
                    }
                }
                _ => unvouched.push((candidate, key)),
            }
        }
        stats.hashed = unvouched.len();
        hotpath::gauge!("daemon.code_index.freshness.source_sweep.hashed")
            .set(unvouched.len() as u64);
        let Ok(derived) = parallelism::install(|| {
            unvouched
                .par_iter()
                .map(|(candidate, key)| {
                    parallelism::with_background_cpu_permit(|| {
                        if shutting_down.load(Ordering::Acquire) {
                            return (*candidate, *key, CandidateContentV1::Unreadable);
                        }
                        (
                            *candidate,
                            *key,
                            CandidateContentV1::derive(project_root, candidate),
                        )
                    })
                })
                .collect::<Vec<_>>()
        }) else {
            return (false, stats);
        };
        if shutting_down.load(Ordering::Acquire) {
            return (false, stats);
        }
        for (candidate, key, content) in derived {
            if !content.matches(manifest.files.get(&candidate.logical_path)) {
                disputed.push(candidate);
            }
            // The key was sampled before the read, so a settled key vouches
            // for these bytes: a write after the stat would change it.
            match key {
                Some(key)
                    if key.settled(sampled_at) && content != CandidateContentV1::Unreadable =>
                {
                    self.contents
                        .insert(candidate.logical_path.clone(), (key, content));
                }
                _ => {
                    self.contents.remove(&candidate.logical_path);
                }
            }
        }
        let matches = disputed.is_empty()
            || tracked_files_match_after_clean_filters(project_root, &disputed, manifest);
        (matches, stats)
    }
}

/// A tracked file whose bytes on disk differ from its sealed digest may still
/// be exactly what the generation sealed: a clean-tree generation captures
/// HEAD's blobs, and git's clean filters (`core.autocrlf`, `eol`, `ident`,
/// filter drivers) are what separate those blobs from the checkout. Running
/// the disk bytes through the repository's own filter pipeline, the same
/// conversion gix status applies before it compares content, settles whether
/// the file is current. Untracked and explicitly admitted sources have no
/// blob to be sealed from, so their raw mismatch is final.
fn tracked_files_match_after_clean_filters(
    project_root: &Path,
    disputed: &[&StatCandidateV1],
    manifest: &SourceContentManifestV1,
) -> bool {
    let Ok(repository) = tracedecay_runtime_core::git_open::open(project_root) else {
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
