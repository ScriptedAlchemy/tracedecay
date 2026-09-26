//! Sealed-generation storage bytes per section, plus a decoded-content digest.
//!
//! Builds a clean generation over the blobs of one git revision, then a
//! successor after a deterministic edit of every seventeenth file, seals both
//! through the partitioned codec, and reports what a store would hold: file
//! segments (deduplicated across the two generations by content address, as
//! the segment directory is), each generation's evidence pack, and manifests.
//! File segment bytes are also split by payload section after undoing the
//! stored encoding, so a codec change shows which section it moved.
//!
//! `decoded_digest` hashes what readers observe after restore: every lexical
//! page's chunks and clone bodies, the lineage roster, symbols, and edges.
//! A storage change that preserves behaviour leaves it unchanged.
//!
//! ```text
//! cargo bench -p tracedecay-code-index --bench sealed_storage
//! SEALED_STORAGE_REPO=<git dir> SEALED_STORAGE_REV=<rev> cargo bench ...
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

use serde::Serialize;
use sha2::{Digest, Sha256};
use tracedecay_code_index::{
    chunks::content_digest,
    languages::{LanguageRegistry, StaticLanguageRegistry},
    lineage::LineageKindV1,
    production::{
        CodeIndexAtomicPublicationPort, CodeIndexBuildRequestV1, CodeIndexCapturedFileV1,
        CodeIndexExecutionControlV1, CodeIndexGenerationScopeV1, CodeIndexProductionConfigV1,
        CodeIndexProductionErrorV1, CodeIndexProductionOwnerV1, CodeIndexPublicationStoreErrorV1,
        CodeIndexPublishedGenerationV1, CodeIndexRepositoryParseIdentityV1,
        SealedGenerationSegmentPublicationV1, SealedGenerationSegmentReadV1,
        VerifiedSealedLexicalPageReadV1, VerifiedSealedLexicalPageSourceV1,
    },
    projection::{
        ChunkProjectionDecisionV1, CodeChunkProjectionSink, ProjectionReceiptBuilderV1,
        ProjectionSinkErrorV1, ProjectionSinkReceiptV1,
    },
};
use tracedecay_domain::{
    ChunkerRevision, CodeGenerationId, FileOccurrenceId, LanguageId, ManifestDigest,
    PolicyRevisionId, PrivacyDomainId, ProjectId, ProjectionBatchRequestV1, ProjectionKeyV1,
    ProjectionKindV1, ProjectionOperationV1, ProjectionOutcomeV1, RepositoryDirtyStateV1,
    RepositoryId, SanitizationReceiptId, SanitizedCodeFileV1, SanitizedCodeSnapshotV1,
    SanitizerRevision, SensitivityLevelV1, SnapshotFileDispositionV1, TreeId, UtcMicros,
    WorktreeId,
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

#[derive(Default, Clone)]
struct MemoryPublication {
    active: Arc<Mutex<BTreeMap<CodeIndexGenerationScopeV1, Arc<CodeIndexPublishedGenerationV1>>>>,
}

impl CodeIndexAtomicPublicationPort for MemoryPublication {
    fn load_active(
        &self,
        scope: &CodeIndexGenerationScopeV1,
    ) -> Result<Option<Arc<CodeIndexPublishedGenerationV1>>, CodeIndexPublicationStoreErrorV1> {
        Ok(self
            .active
            .lock()
            .map_err(|_| CodeIndexPublicationStoreErrorV1::CompareAndSwap)?
            .get(scope)
            .cloned())
    }

    fn publish_atomically(
        &mut self,
        scope: &CodeIndexGenerationScopeV1,
        expected_active_generation: Option<&CodeGenerationId>,
        generation: Arc<CodeIndexPublishedGenerationV1>,
    ) -> Result<(), CodeIndexPublicationStoreErrorV1> {
        let mut active = self
            .active
            .lock()
            .map_err(|_| CodeIndexPublicationStoreErrorV1::CompareAndSwap)?;
        if active
            .get(scope)
            .map(|current| current.manifest().generation_id.clone())
            .as_ref()
            != expected_active_generation
        {
            return Err(CodeIndexPublicationStoreErrorV1::CompareAndSwap);
        }
        active.insert(scope.clone(), generation);
        Ok(())
    }
}

struct ApplyingProjection;

impl CodeChunkProjectionSink for ApplyingProjection {
    fn project_changed_chunks(
        &mut self,
        request: &ProjectionBatchRequestV1,
        receipt_builder: ProjectionReceiptBuilderV1<'_>,
    ) -> Result<ProjectionSinkReceiptV1, ProjectionSinkErrorV1> {
        let changes = &request.changes;
        let decisions = changes
            .added_or_changed
            .iter()
            .map(|change| ChunkProjectionDecisionV1 {
                chunk_id: change.chunk_id.clone(),
                prior_chunk_digest: change.prior_digest.clone(),
                current_chunk_digest: change.current_digest.clone(),
                operation: if change.prior_digest.is_some() {
                    ProjectionOperationV1::Updated
                } else {
                    ProjectionOperationV1::Added
                },
                outcome: ProjectionOutcomeV1::Applied,
                output_digest: change.current_digest.clone(),
            })
            .chain(
                changes
                    .deleted
                    .iter()
                    .map(|change| ChunkProjectionDecisionV1 {
                        chunk_id: change.chunk_id.clone(),
                        prior_chunk_digest: change.prior_digest.clone(),
                        current_chunk_digest: None,
                        operation: ProjectionOperationV1::Deleted,
                        outcome: ProjectionOutcomeV1::Applied,
                        output_digest: None,
                    }),
            )
            .collect::<Vec<_>>();
        receipt_builder
            .build(&decisions)
            .map_err(|error| ProjectionSinkErrorV1::Rejected(error.to_string()))
    }
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

struct Sealed {
    manifest: Vec<u8>,
    file_segments: BTreeMap<String, Vec<u8>>,
    evidence: (String, Vec<u8>),
}

#[derive(Serialize)]
struct GenerationReport {
    seal_ms: u128,
    /// Restore through `decode_partitioned_sealed` alone.
    restore_ms: u128,
    manifest_bytes: usize,
    file_segments: usize,
    file_segments_written: usize,
    file_segment_bytes_written: usize,
    evidence_bytes: usize,
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
    /// What the segment directory holds for both generations.
    store_file_segments: usize,
    store_file_segment_bytes: usize,
    store_evidence_bytes: usize,
    store_manifest_bytes: usize,
    store_total_bytes: usize,
    /// Distinct file segments split by payload section, measured on the
    /// canonical JSON the stored encoding decodes to.
    decoded_section_bytes: BTreeMap<String, usize>,
    decoded_digest: String,
    linked_worktree: LinkedWorktreeReport,
}

/// The clean generation sealed again as a linked worktree of the same
/// project: every file segment it writes, and what two worktree scopes cost
/// when each stores its own segments versus sharing the project's.
#[derive(Serialize)]
struct LinkedWorktreeReport {
    file_segments_written: usize,
    file_segments_shared_with_primary: usize,
    manifest_bytes: usize,
    evidence_bytes: usize,
    per_worktree_segments_total_bytes: usize,
    shared_segments_total_bytes: usize,
}

fn main() -> Result<(), Box<dyn Error>> {
    let repository = std::env::var_os(REPO_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../..")));
    let revision = std::env::var(REV_ENV).unwrap_or_else(|_| "HEAD".to_owned());
    let revision = git_output(&repository, &["rev-parse", "--verify", &revision])?;
    let sources = git_sources(&repository, &revision)?;
    let corpus_bytes = sources.iter().map(|source| source.bytes.len()).sum();

    let mut owner = CodeIndexProductionOwnerV1::new(
        config()?,
        MemoryPublication::default(),
        ApplyingProjection,
    )?;
    let all_paths = sources
        .iter()
        .map(|source| source.logical_path.clone())
        .collect::<BTreeSet<_>>();
    let clean = owner.build_and_publish(
        request(
            &sources,
            all_paths.clone(),
            "tree.sealed-storage.clean",
            2_000_000,
            None,
        )?,
        &ActiveControl,
    )?;
    // The same tree sealed again as a linked worktree of the project.
    let linked = CodeIndexProductionOwnerV1::new(
        config()?,
        MemoryPublication::default(),
        ApplyingProjection,
    )?
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
    let successor = owner.build_and_publish(
        request(
            &edited,
            edited_paths,
            "tree.sealed-storage.successor",
            4_000_000,
            None,
        )?,
        &ActiveControl,
    )?;

    let started = Instant::now();
    let clean_sealed = seal(&clean, None)?;
    let clean_seal_ms = started.elapsed().as_millis();
    let started = Instant::now();
    let successor_sealed = seal(&successor, Some(&clean_sealed.manifest))?;
    let successor_seal_ms = started.elapsed().as_millis();
    let linked_sealed = seal(&linked, None)?;
    let file_segment_bytes =
        |segments: &BTreeMap<String, Vec<u8>>| segments.values().map(Vec::len).sum::<usize>();
    let scope_bytes = |sealed: &Sealed| sealed.manifest.len() + sealed.evidence.1.len();
    let mut shared_segments = clean_sealed.file_segments.clone();
    shared_segments.extend(
        linked_sealed
            .file_segments
            .iter()
            .map(|(digest, bytes)| (digest.clone(), bytes.clone())),
    );
    let linked_worktree = LinkedWorktreeReport {
        file_segments_written: linked_sealed.file_segments.len(),
        file_segments_shared_with_primary: linked_sealed
            .file_segments
            .keys()
            .filter(|digest| clean_sealed.file_segments.contains_key(*digest))
            .count(),
        manifest_bytes: linked_sealed.manifest.len(),
        evidence_bytes: linked_sealed.evidence.1.len(),
        per_worktree_segments_total_bytes: file_segment_bytes(&clean_sealed.file_segments)
            + file_segment_bytes(&linked_sealed.file_segments)
            + scope_bytes(&clean_sealed)
            + scope_bytes(&linked_sealed),
        shared_segments_total_bytes: file_segment_bytes(&shared_segments)
            + scope_bytes(&clean_sealed)
            + scope_bytes(&linked_sealed),
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
        digest_decoded(&clean_sealed, &clean_sealed.file_segments, &mut decoded)?;
    let successor_restore_ms = digest_decoded(&successor_sealed, &store_segments, &mut decoded)?;

    let mut sections = BTreeMap::new();
    for bytes in store_segments.values() {
        add_sections(bytes, &mut sections)?;
    }
    for sealed in [&clean_sealed, &successor_sealed] {
        add_evidence_sections(&sealed.evidence.1, &mut sections)?;
    }
    let store_file_segment_bytes = store_segments.values().map(Vec::len).sum::<usize>();
    let store_evidence_bytes = clean_sealed.evidence.1.len() + successor_sealed.evidence.1.len();
    let store_manifest_bytes = clean_sealed.manifest.len() + successor_sealed.manifest.len();
    let report = Report {
        repository: repository.display().to_string(),
        revision,
        corpus_files: sources.len(),
        corpus_bytes,
        edited_files,
        clean: generation_report(&clean, &clean_sealed, clean_seal_ms, clean_restore_ms),
        successor: generation_report(
            &successor,
            &successor_sealed,
            successor_seal_ms,
            successor_restore_ms,
        ),
        store_file_segments: store_segments.len(),
        store_file_segment_bytes,
        store_evidence_bytes,
        store_manifest_bytes,
        store_total_bytes: store_file_segment_bytes + store_evidence_bytes + store_manifest_bytes,
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
) -> GenerationReport {
    GenerationReport {
        seal_ms,
        restore_ms,
        manifest_bytes: sealed.manifest.len(),
        file_segments: generation.snapshot().files.len(),
        file_segments_written: sealed.file_segments.len(),
        file_segment_bytes_written: sealed.file_segments.values().map(Vec::len).sum(),
        evidence_bytes: sealed.evidence.1.len(),
        lineage_rows: generation.lineage().len(),
        lineage_unchanged_rows: generation
            .lineage()
            .iter()
            .filter(|row| row.kind == LineageKindV1::Unchanged)
            .count(),
    }
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
        if changed.contains(&source.logical_path) {
            captured_files.push(CodeIndexCapturedFileV1 {
                file_occurrence_id: occurrence,
                sanitized_bytes: Arc::clone(&source.bytes),
                sensitivity_level: SensitivityLevelV1::Public,
            });
        }
    }
    Ok(CodeIndexBuildRequestV1 {
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
        },
        captured_files,
        changed_files: changed,
        invalidations: BTreeSet::new(),
        ignored_source_admissions: Vec::new(),
        repository_parse_identity: CodeIndexRepositoryParseIdentityV1 {
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

fn seal(
    generation: &CodeIndexPublishedGenerationV1,
    parent: Option<&[u8]>,
) -> Result<Sealed, CodeIndexProductionErrorV1> {
    let mut file_segments = BTreeMap::new();
    let mut pack = Vec::new();
    let mut evidence = None;
    let manifest = generation.encode_partitioned_sealed_with_parent(parent, |publication| {
        match publication {
            SealedGenerationSegmentPublicationV1::File { digest, bytes } => {
                file_segments.insert(digest.as_str().to_owned(), bytes.to_vec());
            }
            SealedGenerationSegmentPublicationV1::GenerationEvidencePage { bytes, .. } => {
                pack.extend_from_slice(bytes);
            }
            SealedGenerationSegmentPublicationV1::GenerationEvidenceCommit {
                segment_digest,
                ..
            } => {
                evidence = Some((
                    segment_digest.as_str().to_owned(),
                    std::mem::take(&mut pack),
                ));
            }
        }
        Ok(())
    })?;
    Ok(Sealed {
        manifest,
        file_segments,
        evidence: evidence.ok_or_else(|| {
            CodeIndexProductionErrorV1::Contract("sealed generation has no evidence".to_owned())
        })?,
    })
}

/// Restore `sealed` from `segments` (its own plus any it reuses from a
/// parent) and hash everything restore and the lexical drain hand readers.
fn digest_decoded(
    sealed: &Sealed,
    segments: &BTreeMap<String, Vec<u8>>,
    digest: &mut Sha256,
) -> Result<u128, Box<dyn Error>> {
    let mut segments = segments.clone();
    segments.insert(sealed.evidence.0.clone(), sealed.evidence.1.clone());
    let segments = Arc::new(segments);
    let segment = |digest: &ManifestDigest| {
        segments
            .get(digest.as_str())
            .map(Vec::as_slice)
            .ok_or_else(|| CodeIndexProductionErrorV1::Contract("segment is missing".to_owned()))
    };
    let started = Instant::now();
    let generation = CodeIndexPublishedGenerationV1::decode_partitioned_sealed(
        &sealed.manifest,
        |request, buffer| {
            let (bytes, offset, length) = match request {
                SealedGenerationSegmentReadV1::Whole { digest, size_bytes } => {
                    (segment(digest)?, 0, size_bytes)
                }
                SealedGenerationSegmentReadV1::Range {
                    digest,
                    offset,
                    length,
                    ..
                } => (segment(digest)?, offset, length),
            };
            buffer.clear();
            buffer.extend_from_slice(&bytes[offset as usize..(offset + length) as usize]);
            Ok(())
        },
    )?;
    let restore_ms = started.elapsed().as_millis();
    digest.update(serde_json::to_vec(generation.lineage())?);
    digest.update(serde_json::to_vec(&generation.symbols().symbols)?);
    digest.update(serde_json::to_vec(generation.edges())?);
    digest.update(serde_json::to_vec(generation.imports())?);

    let state_digest = ManifestDigest::from_sha256_bytes(&Sha256::digest(&sealed.manifest))?;
    let lexical_segments = Arc::clone(&segments);
    let mut source = VerifiedSealedLexicalPageSourceV1::open_partitioned_sealed(
        &sealed.manifest,
        state_digest,
        move |digest, _, buffer, _control| {
            let bytes = lexical_segments.get(digest.as_str()).ok_or_else(|| {
                CodeIndexProductionErrorV1::Contract("segment is missing".to_owned())
            })?;
            buffer.clear();
            buffer.extend_from_slice(bytes);
            Ok(())
        },
        LEXICAL_PAGE_CHUNKS,
        LEXICAL_PAGE_BYTES,
    )?;
    loop {
        match source.next_page(&ActiveControl)? {
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

/// Split one stored file segment by payload section. The stored bytes are
/// either the canonical JSON itself or its raw DEFLATE stream.
fn add_sections(
    bytes: &[u8],
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
