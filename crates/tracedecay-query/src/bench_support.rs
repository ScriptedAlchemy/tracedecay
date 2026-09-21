//! Shared fixture helpers for the daemon-free index and search benches.
//!
//! Both binaries walk the same committed corpus, seal through the same
//! in-memory publication and projection authorities, and drain sealed pages
//! with the production batch cursor. The page budgets stay at each binary:
//! indexing profiles the daemon's commit width, search profiles a narrower
//! query ingest.

use std::fmt;
use std::io::Cursor;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tracedecay_code_index::languages::{LanguageRegistry, StaticLanguageRegistry};
use tracedecay_code_index::production::{
    CodeIndexAtomicPublicationPort, CodeIndexExecutionControlV1, CodeIndexGenerationScopeV1,
    CodeIndexPublicationStoreErrorV1, CodeIndexPublishedGenerationV1,
    VerifiedSealedLexicalPageBatchBoundsV1, VerifiedSealedLexicalPageBatchReadV1,
    VerifiedSealedLexicalPageSourceV1, VerifiedSealedLexicalPageV1,
    VerifiedSealedLexicalSourceReceiptV1,
};
use tracedecay_code_index::projection::{
    ChunkProjectionDecisionV1, CodeChunkProjectionSink, ProjectionReceiptBuilderV1,
    ProjectionSinkErrorV1, ProjectionSinkReceiptV1,
};
use tracedecay_domain::{
    CodeGenerationId, LanguageId, ManifestDigest, ProjectionBatchRequestV1, ProjectionOperationV1,
    ProjectionOutcomeV1,
};

const DEFAULT_CORPUS_RELATIVE: &str = "benchmark_data/index-bench/corpus";

pub(crate) struct CorpusFile {
    relative_path: String,
    language: LanguageId,
    bytes: Vec<u8>,
}

pub(crate) struct AdmittedFile {
    pub(crate) logical_path: String,
    pub(crate) language: LanguageId,
    pub(crate) bytes: Arc<[u8]>,
}

pub(crate) struct SealedDrainBounds {
    pub(crate) batch_pages: usize,
    pub(crate) batch_retained_bytes: usize,
    pub(crate) page_chunks: usize,
    pub(crate) page_bytes: usize,
}

/// Working directory of the profiling job is not the crate root.
pub(crate) fn default_corpus_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(DEFAULT_CORPUS_RELATIVE)
}

/// Ordered, `.gitignore`-blind directory walk. An ignore-crate walk would
/// consult repository and global ignore files, which makes the admitted file
/// set depend on the machine.
pub(crate) fn load_corpus(root: &Path) -> Result<Vec<CorpusFile>, String> {
    let registry = StaticLanguageRegistry::new();
    let mut files = Vec::new();
    collect_corpus(root, root, &registry, &mut files)?;
    files.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    if files.is_empty() {
        return Err(format!(
            "corpus {} admitted no files with a known language extension",
            root.display()
        ));
    }
    Ok(files)
}

fn collect_corpus(
    root: &Path,
    directory: &Path,
    registry: &StaticLanguageRegistry,
    files: &mut Vec<CorpusFile>,
) -> Result<(), String> {
    let mut entries = std::fs::read_dir(directory)
        .map_err(|error| format!("read {}: {error}", directory.display()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("read {}: {error}", directory.display()))?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|error| format!("stat {}: {error}", path.display()))?;
        if file_type.is_dir() {
            collect_corpus(root, &path, registry, files)?;
            continue;
        }
        if !file_type.is_file() {
            continue;
        }
        let Some(extension) = path.extension().and_then(std::ffi::OsStr::to_str) else {
            continue;
        };
        let Some(descriptor) = registry.descriptor_for_extension(&extension.to_lowercase()) else {
            continue;
        };
        if !descriptor.capabilities.extraction {
            continue;
        }
        let relative = path
            .strip_prefix(root)
            .map_err(|error| format!("relativize {}: {error}", path.display()))?;
        let Some(relative_path) = relative.to_str() else {
            return Err(format!("corpus path {} is not Unicode", relative.display()));
        };
        let bytes =
            std::fs::read(&path).map_err(|error| format!("read {}: {error}", path.display()))?;
        files.push(CorpusFile {
            relative_path: relative_path.replace('\\', "/"),
            language: descriptor.language.clone(),
            bytes,
        });
    }
    Ok(())
}

pub(crate) fn replicate(corpus: &[CorpusFile], replicas: usize) -> Vec<AdmittedFile> {
    let mut admitted = Vec::with_capacity(corpus.len().saturating_mul(replicas));
    for replica in 0..replicas {
        for file in corpus {
            let logical_path = if replica == 0 {
                file.relative_path.clone()
            } else {
                format!("replica{replica:02}/{}", file.relative_path)
            };
            admitted.push(AdmittedFile {
                logical_path,
                language: file.language.clone(),
                bytes: Arc::from(file.bytes.clone()),
            });
        }
    }
    // Canonical file order is over the whole admitted set, not per replica.
    admitted.sort_by(|left, right| left.logical_path.cmp(&right.logical_path));
    admitted
}

pub(crate) struct ActiveControl;

impl CodeIndexExecutionControlV1 for ActiveControl {
    fn is_cancelled(&self) -> bool {
        false
    }

    fn is_deadline_exceeded(&self) -> bool {
        false
    }
}

/// In-memory compare-and-swap publication authority. The benchmark measures
/// indexing or query evaluation, not the daemon's database publication store.
#[derive(Default)]
pub(crate) struct MemoryPublicationStore {
    active: Arc<
        Mutex<
            std::collections::BTreeMap<
                CodeIndexGenerationScopeV1,
                Arc<CodeIndexPublishedGenerationV1>,
            >,
        >,
    >,
}

impl CodeIndexAtomicPublicationPort for MemoryPublicationStore {
    fn load_active(
        &self,
        scope: &CodeIndexGenerationScopeV1,
    ) -> Result<Option<Arc<CodeIndexPublishedGenerationV1>>, CodeIndexPublicationStoreErrorV1> {
        Ok(self
            .active
            .lock()
            .map_err(|_| CodeIndexPublicationStoreErrorV1::CompareAndSwap)?
            .get(scope)
            .map(Arc::clone))
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

/// Applies every decision without a downstream model or store.
pub(crate) struct ApplyingProjectionSink;

impl CodeChunkProjectionSink for ApplyingProjectionSink {
    fn project_changed_chunks(
        &mut self,
        request: &ProjectionBatchRequestV1,
        receipt_builder: ProjectionReceiptBuilderV1<'_>,
    ) -> Result<ProjectionSinkReceiptV1, ProjectionSinkErrorV1> {
        let mut decisions = Vec::with_capacity(
            request.changes.added_or_changed.len() + request.changes.deleted.len(),
        );
        decisions.extend(request.changes.added_or_changed.iter().map(|change| {
            ChunkProjectionDecisionV1 {
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
            }
        }));
        decisions.extend(
            request
                .changes
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
        );
        receipt_builder
            .build(&decisions)
            .map_err(|error| ProjectionSinkErrorV1::Rejected(error.to_string()))
    }
}

pub(crate) fn sealed_state_digest(sealed: &[u8]) -> Result<ManifestDigest, String> {
    let envelope: serde_json::Value = serde_json::from_slice(sealed)
        .map_err(|error| format!("decode sealed generation envelope: {error}"))?;
    let digest = envelope
        .get("state_digest")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "sealed generation envelope has no state digest".to_owned())?;
    ManifestDigest::try_from(digest.to_owned())
        .map_err(|error| format!("sealed generation state digest: {error:?}"))
}

/// Drain the sealed generation through the bounded batch path the daemon uses
/// to ingest an artifact. Page budgets stay with the caller so the two benches
/// do not silently share a commit width.
pub(crate) fn drain_pages(
    sealed: &[u8],
    sealed_len: u64,
    state_digest: &ManifestDigest,
    control: &impl CodeIndexExecutionControlV1,
    bounds: SealedDrainBounds,
) -> Result<
    (
        Vec<VerifiedSealedLexicalPageV1>,
        VerifiedSealedLexicalSourceReceiptV1,
    ),
    String,
> {
    let batch = VerifiedSealedLexicalPageBatchBoundsV1::new(
        bounds.batch_pages,
        bounds.batch_retained_bytes,
    )
    .map_err(|error| format!("sealed lexical batch bounds: {error}"))?;
    let mut source = VerifiedSealedLexicalPageSourceV1::open(
        Cursor::new(sealed.to_vec()),
        sealed_len,
        state_digest.clone(),
        bounds.page_chunks,
        bounds.page_bytes,
        control,
    )
    .map_err(|error| format!("open sealed lexical page source: {error}"))?;
    let mut pages = Vec::new();
    loop {
        let read = source
            .next_page_batch_if(control, batch, |staged| {
                NonZeroUsize::new(staged.len())
                    .ok_or_else(|| "sealed lexical batch staged no pages".to_owned())
            })
            .map_err(|error| format!("stage sealed lexical page batch: {error}"))?
            .map_err(|error| format!("admit sealed lexical page batch: {error}"))?;
        match read {
            VerifiedSealedLexicalPageBatchReadV1::Pages(batch) => pages.extend(batch),
            VerifiedSealedLexicalPageBatchReadV1::Complete(receipt) => {
                return Ok((pages, receipt));
            }
        }
    }
}

/// Peak resident set size in bytes, or `None` off Linux.
pub(crate) fn peak_rss_bytes() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    for line in status.lines() {
        let Some(value) = line.strip_prefix("VmHWM:") else {
            continue;
        };
        let kilobytes = value.split_whitespace().next()?.parse::<u64>().ok()?;
        return kilobytes.checked_mul(1024);
    }
    None
}

pub(crate) fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

pub(crate) fn percentile(sorted: &[u64], percent: usize) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let rank = (sorted.len() * percent).div_ceil(100);
    sorted[rank.saturating_sub(1).min(sorted.len() - 1)]
}

pub(crate) fn identity<T>(value: &str) -> T
where
    T: TryFrom<String>,
    <T as TryFrom<String>>::Error: fmt::Debug,
{
    T::try_from(value.to_owned()).unwrap_or_else(|error| {
        panic!("deterministic benchmark identity {value:?} must be valid: {error:?}")
    })
}
