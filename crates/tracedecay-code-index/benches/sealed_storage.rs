//! Sealed-generation storage bytes per section, plus a decoded-content digest.
//!
//! Builds a clean generation over the blobs of one git revision, then a
//! successor after a deterministic edit of every seventeenth file, seals both
//! through the partitioned codec, and reports the physical payload packs and
//! manifests a store holds. Packs are deduplicated only by their complete
//! canonical identities. Logical preparation writes are reported separately
//! and split by payload section after undoing the stored encoding, so a codec
//! change shows which section it moved.
//!
//! `decoded_digest` hashes what readers observe after restore: every lexical
//! page's chunks and clone bodies, the lineage roster, symbols, and edges.
//! A storage change that preserves behaviour leaves it unchanged.
//!
//! ```text
//! bazel test //crates/tracedecay-code-index:sealed_storage --test_arg=--bench
//! SEALED_STORAGE_REPO=<git dir> SEALED_STORAGE_REV=<rev> bazel test ...
//! ```
//!
//! The corpus is read from git objects, never from the working tree, so two
//! runs at one revision admit identical bytes regardless of local edits.

use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fmt::Debug,
    io::{BufRead, BufReader, Read, Write},
    path::PathBuf,
    process::{Command, Stdio},
    sync::{Arc, Mutex},
    time::Instant,
};
use tracedecay_runtime_core::resident_memory::{
    ProcessResidentMemoryV1, ResidentMemoryOperationV1, detected_process_resident_memory_limit_v1,
};

use serde::Serialize;
use sha2::{Digest, Sha256};
use tracedecay_code_index::{
    chunks::content_digest,
    languages::{LanguageRegistry, StaticLanguageRegistry},
    lineage::LineageKindV1,
    production::{
        CodeIndexAtomicPublicationPort, CodeIndexBuildRequestV1, CodeIndexCapturedFileV1,
        CodeIndexExecutionControlV1, CodeIndexGenerationScopeV1, CodeIndexPreparationPort,
        CodeIndexProductionConfigV1, CodeIndexProductionOwnerV1, CodeIndexPublicationStoreErrorV1,
        CodeIndexPublishedGenerationV1, CodeIndexRepositoryParseIdentityV1,
        CodeIndexSealedGenerationV1, CodeIndexSealedPublicationV1, MemoryCodeIndexPreparationV1,
        MemorySealedPublicationStoreV1, SealedGenerationContentPackV1,
        SealedGenerationPackReadFactoryV1, SealedGenerationPackReadPassV1,
        SealedGenerationSegmentIdentityV1, SealedGenerationSegmentPublicationV1,
        SealedSegmentReaderV1, SharedDecodedContentPoolV1, VerifiedSealedLexicalPageReadV1,
        VerifiedSealedLexicalPageSourceV1,
    },
};
use tracedecay_domain::{
    ChunkerRevision, CodeGenerationId, FileOccurrenceId, LanguageId, ManifestDigest,
    PolicyRevisionId, PrivacyDomainId, ProjectId, ProjectionKeyV1, ProjectionKindV1,
    RepositoryDirtyStateV1, RepositoryId, SanitizationReceiptId, SanitizedCodeFileV1,
    SanitizedCodeSnapshotV1, SanitizerRevision, SensitivityLevelV1, SnapshotFileDispositionV1,
    TreeId, UtcMicros, WorktreeId,
};

const REPO_ENV: &str = "SEALED_STORAGE_REPO";
const REV_ENV: &str = "SEALED_STORAGE_REV";
/// Matches `tracedecay_index_bench`: prime, so edits spread across languages.
const EDIT_STRIDE: usize = 17;
const EDIT_APPEND: &[u8] = b"\n// sealed-storage successor edit\n";
/// The daemon's text-artifact page bounds; a real repository holds clone
/// bodies larger than a smaller page admits.
const LEXICAL_PAGE_CHUNKS: usize = 256;
const LEXICAL_PAGE_BYTES: usize = 4 * 1024 * 1024;

struct SourceFile {
    logical_path: String,
    language: LanguageId,
    bytes: Arc<[u8]>,
}

struct ActiveControl;

impl CodeIndexExecutionControlV1 for ActiveControl {
    fn is_cancelled(&self) -> bool {
        false
    }

    fn is_deadline_exceeded(&self) -> bool {
        false
    }
}

#[derive(Default)]
struct Sealed {
    manifest: Vec<u8>,
    file_segments: BTreeMap<String, Vec<u8>>,
    segment_kinds: BTreeMap<String, &'static str>,
    evidence: (String, Vec<u8>),
}

#[derive(Serialize)]
struct GenerationReport {
    /// Canonical preparation through commit, including bounded construction.
    seal_ms: u128,
    /// Restore through `decode_partitioned_sealed` alone.
    restore_ms: u128,
    manifest_bytes: usize,
    snapshot_files: usize,
    logical_segments_written: usize,
    logical_segment_bytes_written: usize,
    evidence_bytes_written: usize,
    physical_packs: usize,
    physical_pack_bytes: u64,
    lineage_rows: usize,
    lineage_unchanged_rows: usize,
}

#[derive(Serialize)]
struct Report {
    repository: String,
    revision: String,
    corpus_files: usize,
    corpus_bytes: usize,
    edited_files: usize,
    clean: GenerationReport,
    successor: GenerationReport,
    /// Distinct logical writes observed across both preparations.
    logical_segments_written: usize,
    logical_segment_bytes_written: usize,
    evidence_bytes_written: usize,
    /// Actual physical payload storage for both retained generations.
    store_packs: usize,
    store_pack_bytes: u64,
    store_manifest_bytes: usize,
    store_total_bytes: u64,
    /// Distinct logical writes split by payload section, measured on the
    /// canonical JSON the stored encoding decodes to.
    decoded_section_bytes: BTreeMap<String, usize>,
    decoded_digest: String,
    linked_worktree: LinkedWorktreeReport,
}

/// The clean generation sealed again as a linked worktree of the same
/// project: the logical writes and physical packs, with whole-pack sharing
/// between the two worktree scopes.
#[derive(Serialize)]
struct LinkedWorktreeReport {
    logical_segments_written: usize,
    logical_segments_shared_with_primary: usize,
    manifest_bytes: usize,
    evidence_bytes_written: usize,
    physical_packs: usize,
    physical_packs_shared_with_primary: usize,
    per_worktree_store_bytes: u64,
    shared_store_bytes: u64,
}

fn main() -> Result<(), Box<dyn Error>> {
    let repository = std::env::var_os(REPO_ENV)
        .or_else(|| std::env::var_os("BUILD_WORKSPACE_DIRECTORY"))
        .map(PathBuf::from)
        .ok_or("set SEALED_STORAGE_REPO to the Git checkout to measure")?;
    let revision = std::env::var(REV_ENV).unwrap_or_else(|_| "HEAD".to_owned());
    let revision = git_output(&repository, &["rev-parse", "--verify", &revision])?;
    let sources = git_sources(&repository, &revision)?;
    let corpus_bytes = sources.iter().map(|source| source.bytes.len()).sum();

    let store = ObservedStoreV1::default();
    let mut owner = CodeIndexProductionOwnerV1::new(config()?, store.clone())?;
    let all_paths = sources
        .iter()
        .map(|source| source.logical_path.clone())
        .collect::<BTreeSet<_>>();
    let clean_request = request(
        &sources,
        all_paths.clone(),
        "tree.sealed-storage.clean",
        2_000_000,
        None,
    )?;
    let clean_operation = clean_request.resident_memory.begin_operation();
    let clean = owner.build_and_publish(clean_request, &ActiveControl)?;
    let (clean_sealed, clean_seal_ms) = store.take_sealed()?;
    // The same tree sealed again as a linked worktree of the project.
    let linked_store = ObservedStoreV1::default();
    let _linked = CodeIndexProductionOwnerV1::new(config()?, linked_store.clone())?
        .build_and_publish(
            request(
                &sources,
                all_paths,
                "tree.sealed-storage.clean",
                2_000_000,
                Some(id::<WorktreeId>("worktree.sealed-storage.linked")?),
            )?,
            &ActiveControl,
        )?;
    let (edited, edited_paths) = edit(&sources);
    let edited_files = edited_paths.len();
    let successor_request = request(
        &edited,
        edited_paths,
        "tree.sealed-storage.successor",
        4_000_000,
        None,
    )?;
    let successor_operation = successor_request.resident_memory.begin_operation();
    let successor = owner.build_and_publish(successor_request, &ActiveControl)?;

    let (successor_sealed, successor_seal_ms) = store.take_sealed()?;
    let (linked_sealed, _) = linked_store.take_sealed()?;
    let clean_packs = physical_packs(&[&clean_sealed])?;
    let linked_packs = physical_packs(&[&linked_sealed])?;
    let shared_packs = physical_packs(&[&clean_sealed, &linked_sealed])?;
    let linked_manifest_bytes = (clean_sealed.manifest.len() + linked_sealed.manifest.len()) as u64;
    let linked_worktree = LinkedWorktreeReport {
        logical_segments_written: linked_sealed.file_segments.len(),
        logical_segments_shared_with_primary: linked_sealed
            .file_segments
            .keys()
            .filter(|digest| clean_sealed.file_segments.contains_key(*digest))
            .count(),
        manifest_bytes: linked_sealed.manifest.len(),
        evidence_bytes_written: linked_sealed.evidence.1.len(),
        physical_packs: linked_packs.len(),
        physical_packs_shared_with_primary: linked_packs
            .keys()
            .filter(|digest| clean_packs.contains_key(*digest))
            .count(),
        per_worktree_store_bytes: clean_packs.values().sum::<u64>()
            + linked_packs.values().sum::<u64>()
            + linked_manifest_bytes,
        shared_store_bytes: shared_packs.values().sum::<u64>() + linked_manifest_bytes,
    };
    let mut store_segments = clean_sealed.file_segments.clone();
    store_segments.extend(
        successor_sealed
            .file_segments
            .iter()
            .map(|(digest, bytes)| (digest.clone(), bytes.clone())),
    );

    let mut decoded = Sha256::new();
    let clean_restore_ms =
        digest_decoded(clean.sealed_generation(), &mut decoded, &clean_operation)?;
    let successor_restore_ms = digest_decoded(
        successor.sealed_generation(),
        &mut decoded,
        &successor_operation,
    )?;

    let mut sections = BTreeMap::new();
    let mut segment_kinds = clean_sealed.segment_kinds.clone();
    segment_kinds.extend(successor_sealed.segment_kinds.clone());
    for (digest, bytes) in &store_segments {
        let kind = segment_kinds.get(digest).ok_or("segment kind is missing")?;
        add_sections(bytes, kind, &mut sections)?;
    }
    for sealed in [&clean_sealed, &successor_sealed] {
        add_evidence_sections(&sealed.evidence.1, &mut sections)?;
    }
    let logical_segment_bytes_written = store_segments.values().map(Vec::len).sum::<usize>();
    let evidence_bytes_written = clean_sealed.evidence.1.len() + successor_sealed.evidence.1.len();
    let store_packs = physical_packs(&[&clean_sealed, &successor_sealed])?;
    let store_pack_bytes = store_packs.values().sum::<u64>();
    let store_manifest_bytes = clean_sealed.manifest.len() + successor_sealed.manifest.len();
    let report = Report {
        repository: repository.display().to_string(),
        revision,
        corpus_files: sources.len(),
        corpus_bytes,
        edited_files,
        clean: generation_report(
            &store
                .inner
                .decode(clean.manifest_bytes(), &clean_operation)?,
            &clean_sealed,
            clean_seal_ms,
            clean_restore_ms,
        )?,
        successor: generation_report(
            &store
                .inner
                .decode(successor.manifest_bytes(), &successor_operation)?,
            &successor_sealed,
            successor_seal_ms,
            successor_restore_ms,
        )?,
        logical_segments_written: store_segments.len(),
        logical_segment_bytes_written,
        evidence_bytes_written,
        store_packs: store_packs.len(),
        store_pack_bytes,
        store_manifest_bytes,
        store_total_bytes: store_pack_bytes + store_manifest_bytes as u64,
        decoded_section_bytes: sections,
        decoded_digest: format!("sha256:{}", hex::encode(decoded.finalize())),
        linked_worktree,
    };
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

fn generation_report(
    generation: &CodeIndexPublishedGenerationV1,
    sealed: &Sealed,
    seal_ms: u128,
    restore_ms: u128,
) -> Result<GenerationReport, Box<dyn Error>> {
    let packs = physical_packs(&[sealed])?;
    Ok(GenerationReport {
        seal_ms,
        restore_ms,
        manifest_bytes: sealed.manifest.len(),
        snapshot_files: generation.snapshot().files.len(),
        logical_segments_written: sealed.file_segments.len(),
        logical_segment_bytes_written: sealed.file_segments.values().map(Vec::len).sum(),
        evidence_bytes_written: sealed.evidence.1.len(),
        physical_packs: packs.len(),
        physical_pack_bytes: packs.values().sum(),
        lineage_rows: generation.lineage().len(),
        lineage_unchanged_rows: generation
            .lineage()
            .iter()
            .filter(|row| row.kind == LineageKindV1::Unchanged)
            .count(),
    })
}

fn physical_packs(seals: &[&Sealed]) -> Result<BTreeMap<ManifestDigest, u64>, Box<dyn Error>> {
    let mut packs = BTreeMap::new();
    for seal in seals {
        for identity in CodeIndexPublishedGenerationV1::partitioned_pack_identities(&seal.manifest)?
        {
            if let Some(previous) = packs.insert(identity.digest, identity.size_bytes)
                && previous != identity.size_bytes
            {
                return Err("the same physical pack digest has conflicting byte lengths".into());
            }
        }
    }
    Ok(packs)
}

fn git_output(repository: &PathBuf, args: &[&str]) -> Result<String, Box<dyn Error>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(args)
        .output()?;
    if !output.status.success() {
        return Err(format!("git {args:?}: {}", String::from_utf8_lossy(&output.stderr)).into());
    }
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}

/// Every blob at `revision` whose extension a compiled extractor parses.
fn git_sources(repository: &PathBuf, revision: &str) -> Result<Vec<SourceFile>, Box<dyn Error>> {
    let registry = StaticLanguageRegistry::new();
    let listing = git_output(repository, &["ls-tree", "-r", "--full-tree", revision])?;
    let mut wanted = Vec::new();
    for line in listing.lines() {
        let (meta, path) = line.split_once('\t').ok_or("malformed ls-tree line")?;
        let mut meta = meta.split_whitespace();
        let (Some(_mode), Some("blob"), Some(object)) = (meta.next(), meta.next(), meta.next())
        else {
            continue;
        };
        let Some(extension) = path.rsplit_once('.').map(|(_, extension)| extension) else {
            continue;
        };
        let Some(descriptor) = registry.descriptor_for_extension(&extension.to_lowercase()) else {
            continue;
        };
        if !descriptor.capabilities.extraction {
            continue;
        }
        wanted.push((
            object.to_owned(),
            path.to_owned(),
            descriptor.language.clone(),
        ));
    }
    let mut child = Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(["cat-file", "--batch"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;
    let mut stdin = child.stdin.take().ok_or("git cat-file stdin")?;
    let objects = wanted
        .iter()
        .map(|(object, _, _)| format!("{object}\n"))
        .collect::<String>();
    let writer = std::thread::spawn(move || stdin.write_all(objects.as_bytes()));
    let mut stdout = BufReader::new(child.stdout.take().ok_or("git cat-file stdout")?);
    let mut sources = Vec::with_capacity(wanted.len());
    for (_, path, language) in wanted {
        let mut header = String::new();
        stdout.read_line(&mut header)?;
        let size = header
            .split_whitespace()
            .nth(2)
            .ok_or("malformed cat-file header")?
            .parse::<usize>()?;
        let mut bytes = vec![0; size + 1];
        stdout.read_exact(&mut bytes)?;
        bytes.pop();
        if std::str::from_utf8(&bytes).is_err() {
            continue;
        }
        sources.push(SourceFile {
            logical_path: path,
            language,
            bytes: bytes.into(),
        });
    }
    writer
        .join()
        .map_err(|_| "git cat-file writer panicked")??;
    child.wait()?;
    sources.sort_by(|left, right| left.logical_path.cmp(&right.logical_path));
    Ok(sources)
}

fn edit(sources: &[SourceFile]) -> (Vec<SourceFile>, BTreeSet<String>) {
    let mut changed = BTreeSet::new();
    let edited = sources
        .iter()
        .enumerate()
        .map(|(index, source)| {
            let bytes = if index % EDIT_STRIDE == 0 {
                changed.insert(source.logical_path.clone());
                let mut bytes = source.bytes.to_vec();
                bytes.extend_from_slice(EDIT_APPEND);
                bytes.into()
            } else {
                Arc::clone(&source.bytes)
            };
            SourceFile {
                logical_path: source.logical_path.clone(),
                language: source.language.clone(),
                bytes,
            }
        })
        .collect();
    (edited, changed)
}

fn request(
    sources: &[SourceFile],
    changed: BTreeSet<String>,
    tree: &str,
    sealed_at: i64,
    worktree: Option<WorktreeId>,
) -> Result<CodeIndexBuildRequestV1, Box<dyn Error>> {
    let mut files = Vec::with_capacity(sources.len());
    let mut captured_files = Vec::new();
    let mut identity = Sha256::new();
    for source in sources {
        let digest = content_digest(&source.bytes);
        identity.update(digest.as_str().as_bytes());
        // Like the daemon's, occurrences name content, not the worktree.
        let occurrence = id::<FileOccurrenceId>(&format!(
            "file.sealed-storage.{}",
            &hex::encode(Sha256::digest(
                format!("{}\0{}", source.logical_path, digest.as_str()).as_bytes()
            ))[..32]
        ))?;
        files.push(SanitizedCodeFileV1 {
            file_occurrence_id: occurrence.clone(),
            logical_path: source.logical_path.clone(),
            language: Some(source.language.clone()),
            content_digest: digest,
            disposition: SnapshotFileDispositionV1::Present,
        });
        captured_files.push(CodeIndexCapturedFileV1 {
            file_occurrence_id: occurrence,
            sanitized_bytes: Arc::clone(&source.bytes),
            sensitivity_level: SensitivityLevelV1::Public,
        });
    }
    Ok(CodeIndexBuildRequestV1 {
        resident_memory: std::sync::Arc::new(ProcessResidentMemoryV1::new(
            detected_process_resident_memory_limit_v1(),
        )),
        snapshot: SanitizedCodeSnapshotV1 {
            repository: id("repository.sealed-storage")?,
            worktree,
            reference: None,
            source_revision: None,
            sanitizer_revision: id("sanitizer.sealed-storage.v1")?,
            sanitization_receipts: vec![id::<SanitizationReceiptId>("receipt.sealed-storage")?],
            content_identity: content_digest(&identity.finalize()),
            captured_at: UtcMicros(sealed_at - 1_000_000),
            files,
            omitted_sources: Vec::new(),
        },
        captured_files,
        changed_files: changed,
        invalidations: BTreeSet::new(),
        ignored_source_admissions: Vec::new(),
        repository_parse_identity: CodeIndexRepositoryParseIdentityV1 {
            include_submodules: false,
            tree: Some(id::<TreeId>(tree)?),
            dirty: RepositoryDirtyStateV1::Clean,
        },
        sealed_at: UtcMicros(sealed_at),
        target_projection_key: ProjectionKeyV1 {
            kind: ProjectionKindV1::Lexical,
            schema_revision: "lexical.sealed-storage.v1".to_owned(),
            profile_digest: ManifestDigest::from_sha256_bytes(&Sha256::digest(b"sealed-storage"))?,
        },
    })
}

fn config() -> Result<CodeIndexProductionConfigV1, Box<dyn Error>> {
    Ok(CodeIndexProductionConfigV1 {
        include_submodules: false,
        project_id: id::<ProjectId>("project.sealed-storage")?,
        repository: id::<RepositoryId>("repository.sealed-storage")?,
        sanitizer_revision: id::<SanitizerRevision>("sanitizer.sealed-storage.v1")?,
        policy_revision: id::<PolicyRevisionId>("policy.sealed-storage.v1")?,
        chunker_revision: id::<ChunkerRevision>("chunker.sealed-storage.v1")?,
        privacy_domain: id::<PrivacyDomainId>("privacy.sealed-storage")?,
        privacy_key_epoch: 1,
        max_snapshot_age_micros: None,
    })
}

/// Observe the actual preparation writes so reuse and storage accounting use
/// exactly the segments the production constructor staged.
#[derive(Clone, Default)]
struct ObservedStoreV1 {
    inner: MemorySealedPublicationStoreV1,
    sealed: Arc<Mutex<Option<(Sealed, u128)>>>,
}

impl ObservedStoreV1 {
    fn take_sealed(&self) -> Result<(Sealed, u128), Box<dyn Error>> {
        self.sealed
            .lock()
            .map_err(|_| "sealed observation lock poisoned")?
            .take()
            .ok_or_else(|| "publication observation is missing".into())
    }
}

struct ObservedPreparationV1 {
    inner: MemoryCodeIndexPreparationV1,
    sealed: Sealed,
    evidence_pack: Vec<u8>,
    started: Instant,
}

impl CodeIndexPreparationPort for ObservedPreparationV1 {
    fn bind_parent(
        &mut self,
        parent: &CodeIndexSealedGenerationV1,
    ) -> Result<(), CodeIndexPublicationStoreErrorV1> {
        self.inner.bind_parent(parent)
    }

    fn write_segment(
        &mut self,
        publication: SealedGenerationSegmentPublicationV1<'_>,
    ) -> Result<(), CodeIndexPublicationStoreErrorV1> {
        // The observer retains bytes only for this benchmark's storage report.
        match &publication {
            SealedGenerationSegmentPublicationV1::File { digest, bytes }
            | SealedGenerationSegmentPublicationV1::FileEvidence { digest, bytes }
            | SealedGenerationSegmentPublicationV1::ResolutionIndex { digest, bytes } => {
                let kind = match &publication {
                    SealedGenerationSegmentPublicationV1::File { .. } => "file",
                    SealedGenerationSegmentPublicationV1::FileEvidence { .. } => "file_evidence",
                    _ => "resolution_index",
                };
                self.sealed
                    .file_segments
                    .insert(digest.as_str().to_owned(), bytes.to_vec());
                self.sealed
                    .segment_kinds
                    .insert(digest.as_str().to_owned(), kind);
            }
            SealedGenerationSegmentPublicationV1::CodeGraphPage {
                page_digest, bytes, ..
            } => {
                self.sealed
                    .file_segments
                    .insert(page_digest.as_str().to_owned(), bytes.to_vec());
                self.sealed
                    .segment_kinds
                    .insert(page_digest.as_str().to_owned(), "code_graph");
            }
            SealedGenerationSegmentPublicationV1::GenerationEvidencePage { bytes, .. } => {
                self.evidence_pack.extend_from_slice(bytes);
            }
            SealedGenerationSegmentPublicationV1::GenerationEvidenceCommit {
                segment_digest,
                ..
            } => {
                self.sealed.evidence = (
                    segment_digest.as_str().to_owned(),
                    std::mem::take(&mut self.evidence_pack),
                );
            }
        }
        self.inner.write_segment(publication)
    }

    fn segment_reader(&self) -> Arc<SealedSegmentReaderV1> {
        self.inner.segment_reader()
    }

    fn finalize_content_pack(
        &mut self,
        segments: &[SealedGenerationSegmentIdentityV1],
    ) -> Result<SealedGenerationContentPackV1, CodeIndexPublicationStoreErrorV1> {
        self.inner.finalize_content_pack(segments)
    }

    fn pack_reader(&self) -> Arc<SealedSegmentReaderV1> {
        self.inner.pack_reader()
    }

    fn read_pass_factory(&self) -> Arc<SealedGenerationPackReadFactoryV1> {
        self.inner.read_pass_factory()
    }

    fn abort(self) -> Result<(), CodeIndexPublicationStoreErrorV1> {
        self.inner.abort()
    }
}

impl CodeIndexAtomicPublicationPort for ObservedStoreV1 {
    type Preparation = ObservedPreparationV1;

    fn load_active(
        &self,
        scope: &CodeIndexGenerationScopeV1,
    ) -> Result<Option<CodeIndexSealedGenerationV1>, CodeIndexPublicationStoreErrorV1> {
        self.inner.load_active(scope)
    }

    fn prepare(
        &mut self,
        scope: &CodeIndexGenerationScopeV1,
        expected: Option<&CodeGenerationId>,
        operation: &ResidentMemoryOperationV1,
    ) -> Result<Self::Preparation, CodeIndexPublicationStoreErrorV1> {
        let started = Instant::now();
        Ok(ObservedPreparationV1 {
            inner: self.inner.prepare(scope, expected, operation)?,
            sealed: Sealed::default(),
            evidence_pack: Vec::new(),
            started,
        })
    }

    fn commit(
        &mut self,
        preparation: &mut Self::Preparation,
        publication: &CodeIndexSealedPublicationV1,
    ) -> Result<Arc<[u8]>, CodeIndexPublicationStoreErrorV1> {
        if preparation.sealed.evidence.0.is_empty() || !preparation.evidence_pack.is_empty() {
            return Err(CodeIndexPublicationStoreErrorV1::Unavailable(
                "sealed generation has no complete evidence pack".to_owned(),
            ));
        }
        let manifest = self.inner.commit(&mut preparation.inner, publication)?;
        preparation.sealed.manifest = manifest.to_vec();
        let seal_ms = preparation.started.elapsed().as_millis();
        *self.sealed.lock().map_err(|_| {
            CodeIndexPublicationStoreErrorV1::Unavailable(
                "sealed observation lock poisoned".to_owned(),
            )
        })? = Some((std::mem::take(&mut preparation.sealed), seal_ms));
        Ok(manifest)
    }
}

/// Restore the real published generation and hash everything restore and
/// the lexical drain hand readers. Write observations remain the section report.
fn digest_decoded(
    sealed: &CodeIndexSealedGenerationV1,
    digest: &mut Sha256,
    operation: &ResidentMemoryOperationV1,
) -> Result<u128, Box<dyn Error>> {
    let reader = sealed.begin_read_pass()?;
    let started = Instant::now();
    let generation = CodeIndexPublishedGenerationV1::decode_partitioned_sealed(
        sealed.manifest_bytes(),
        &SharedDecodedContentPoolV1::default(),
        |request, buffer| reader(request, buffer),
        // Keep the restore measurement scoped to the codec, without an admission guard.
        &mut |_| Ok(()),
    )?;
    let restore_ms = started.elapsed().as_millis();
    digest.update(serde_json::to_vec(generation.lineage())?);
    digest.update(serde_json::to_vec(&generation.symbols().symbols)?);
    digest.update(serde_json::to_vec(generation.edges())?);
    digest.update(serde_json::to_vec(generation.imports())?);

    drop(reader);
    let state_digest = ManifestDigest::from_sha256_bytes(&Sha256::digest(sealed.manifest_bytes()))?;
    let lexical_sealed = sealed.clone();
    let mut source = VerifiedSealedLexicalPageSourceV1::open_partitioned_sealed(
        sealed.manifest_bytes(),
        state_digest,
        move |_control, _layout| {
            let reader = lexical_sealed.begin_read_pass()?;
            let read_pass: Box<SealedGenerationPackReadPassV1> =
                Box::new(move |request, buffer, _control| reader(request, buffer));
            Ok(read_pass)
        },
        LEXICAL_PAGE_CHUNKS,
        LEXICAL_PAGE_BYTES,
        operation,
    )?;
    let mut read_pass = source.begin_read_pass(&ActiveControl)?;
    loop {
        match source.next_page(&ActiveControl, read_pass.as_mut())? {
            VerifiedSealedLexicalPageReadV1::Page(page) => {
                for chunk in page.chunks() {
                    digest.update(serde_json::to_vec(chunk.chunk())?);
                }
                digest.update(serde_json::to_vec(page.clone_bodies())?);
                digest.update(serde_json::to_vec(page.imports())?);
            }
            VerifiedSealedLexicalPageReadV1::Complete(receipt) => {
                receipt.verify_completion(Some(source.cursor()))?;
                return Ok(restore_ms);
            }
        }
    }
}

/// Split one stored segment by its production publication kind and payload section. The stored bytes are
/// either the canonical JSON itself or its raw DEFLATE stream.
fn add_sections(
    bytes: &[u8],
    kind: &str,
    sections: &mut BTreeMap<String, usize>,
) -> Result<(), Box<dyn Error>> {
    let json = if bytes.first() == Some(&b'{') {
        bytes.to_vec()
    } else {
        let mut inflated = Vec::new();
        flate2::read::DeflateDecoder::new(bytes).read_to_end(&mut inflated)?;
        inflated
    };
    *sections.entry("total".to_owned()).or_default() += json.len();
    let value: serde_json::Value = serde_json::from_slice(&json)?;
    if kind != "file" {
        *sections.entry(kind.to_owned()).or_default() += json.len();
        return Ok(());
    }
    let file = value.get("file").ok_or("segment has no file payload")?;
    for (key, section) in file.as_object().ok_or("file payload is not an object")? {
        if key == "artifacts" {
            for (artifact, section) in section.as_object().ok_or("artifacts is not an object")? {
                *sections.entry(format!("artifacts.{artifact}")).or_default() +=
                    serde_json::to_vec(section)?.len();
            }
        } else {
            *sections.entry(key.clone()).or_default() += serde_json::to_vec(section)?.len();
        }
    }
    Ok(())
}

/// Split one evidence pack by top-level field.
fn add_evidence_sections(
    bytes: &[u8],
    sections: &mut BTreeMap<String, usize>,
) -> Result<(), Box<dyn Error>> {
    let value = serde_json::from_slice::<serde_json::Value>(bytes)?;
    for (key, section) in value.as_object().ok_or("evidence is not an object")? {
        *sections.entry(format!("evidence.{key}")).or_default() +=
            serde_json::to_vec(section)?.len();
    }
    Ok(())
}

fn id<T>(value: &str) -> Result<T, T::Error>
where
    T: TryFrom<String>,
    T::Error: Debug,
{
    T::try_from(value.to_owned())
}
