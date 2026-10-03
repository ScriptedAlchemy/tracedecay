//! Bounded retained parse-tree pool for production code indexing.
//!
//! The leaf parser owns Tree-sitter state. This pool owns only checkout/
//! document partitioning, deterministic eviction, and aggregate operational
//! measurements. It is process-local and is never serialized with a code
//! generation.
//!
//! Retained documents are parsed, reparsed and copied inside the pool's own
//! allocator heap, so the pages that heap occupies are what the pool holds
//! and releasing it returns them whole. The extraction a build receives is
//! made outside it and is charged by that build's generation.

use std::{
    collections::{BTreeMap, VecDeque},
    sync::{
        Arc, Mutex, PoisonError, RwLock, TryLockError, Weak,
        atomic::{AtomicU64, Ordering},
    },
    time::Instant,
};

use thiserror::Error;
use tracedecay_code_extraction::incremental::{
    ParseDocumentIdentity, ParseError, ParseLimits, ParseReport, ParseResetReason, ParseReuse,
    RetainedParseDocument,
};
use tracedecay_code_extraction::parsed_extraction::{
    ParsedExtraction, ParsedExtractionArtifactV1, ParsedExtractionDisposition,
};
use tracedecay_code_extraction::{ExtractionArtifactV1, LanguageExtractor};
use tracedecay_domain::process_heap::OwnerHeapV1;
use tracedecay_domain::{ExtractorRevision, ManifestDigest, ProjectId, RepositoryId, WorktreeId};

const DEFAULT_MAX_RETAINED_DOCUMENTS: usize = 256;
const DEFAULT_MAX_RETAINED_SOURCE_BYTES: usize = 64 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetainedParsePoolLimits {
    pub max_documents: usize,
    pub max_total_source_bytes: usize,
    pub document: ParseLimits,
}

impl Default for RetainedParsePoolLimits {
    fn default() -> Self {
        Self {
            max_documents: DEFAULT_MAX_RETAINED_DOCUMENTS,
            max_total_source_bytes: DEFAULT_MAX_RETAINED_SOURCE_BYTES,
            document: ParseLimits::default(),
        }
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum RetainedParsePoolOpenError {
    #[error("retained parse pool limits must admit at least one document and one source byte")]
    EmptyCapacity,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RetainedParsePoolStats {
    pub retained_documents: usize,
    pub retained_source_bytes: usize,
    pub initial_parses: u64,
    pub incremental_parses: u64,
    pub noop_parses: u64,
    pub reset_parses: u64,
    pub partial_parses: u64,
    pub failed_parses: u64,
    pub evicted_documents: u64,
    /// Top-level extraction-range bytes reparsed through retained-tree reuse.
    /// Cold and reset work is reported by its distinct parse counters.
    pub changed_bytes: u64,
    pub parse_micros: u64,
    pub full_extractions: u64,
    pub incremental_extractions: u64,
    pub reset_extractions: u64,
    pub visited_top_level_nodes: u64,
    pub extracted_bytes: u64,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum ParseDocumentKey {
    Repository {
        project_id: ProjectId,
        repository_id: RepositoryId,
        worktree_id: Option<WorktreeId>,
        logical_path: String,
    },
    SessionOverlay {
        scope_identity: ManifestDigest,
        document_identity: ManifestDigest,
        logical_path: String,
    },
}

impl ParseDocumentKey {
    fn for_identity(identity: &ParseDocumentIdentity) -> Self {
        match identity {
            ParseDocumentIdentity::Repository {
                project_id,
                repository_id,
                worktree_id,
                logical_path,
                ..
            } => Self::Repository {
                project_id: project_id.clone(),
                repository_id: repository_id.clone(),
                worktree_id: worktree_id.clone(),
                logical_path: logical_path.clone(),
            },
            ParseDocumentIdentity::SessionOverlay {
                scope_identity,
                document_identity,
                logical_path,
                ..
            } => Self::SessionOverlay {
                scope_identity: scope_identity.clone(),
                document_identity: document_identity.clone(),
                logical_path: logical_path.clone(),
            },
        }
    }
}

struct RetainedEntry {
    document: RetainedParseDocument,
    artifact: Option<ExtractionArtifactV1>,
    artifact_revision: Option<ExtractorRevision>,
}

#[derive(Default)]
struct RetainedParsePoolState {
    documents: BTreeMap<ParseDocumentKey, Arc<Mutex<RetainedEntry>>>,
    source_bytes: BTreeMap<ParseDocumentKey, usize>,
    lru: VecDeque<ParseDocumentKey>,
    stats: RetainedParsePoolStats,
    clear_epoch: u64,
    last_retained: Option<Instant>,
}

/// What the pool retains, for the resident-memory inventory.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetainedParseHoldingV1 {
    pub documents: usize,
    /// Pages of the pool's heap; `None` when the allocator has no owner heaps.
    pub bytes: Option<u64>,
    pub last_retained: Instant,
}

/// Outcome of releasing the retained documents.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RetainedParsePoolReleaseV1 {
    /// Every document was dropped and the pool's heap returned; `bytes` is
    /// what that heap occupied, `None` when the allocator has no owner heaps.
    Released { bytes: Option<u64> },
    /// A parse is running in the pool.
    Busy,
    /// Nothing was retained.
    Empty,
}

/// The pool's allocator heap. Parses allocate into it under the read lock;
/// measuring and replacing it takes the write lock, so neither runs while a
/// thread is allocating into it.
struct RetainedParseHeapV1 {
    heap: RwLock<Option<OwnerHeapV1>>,
    /// The heap's pages at the last measurement that no parse overlapped.
    bytes: AtomicU64,
    /// The allocator provides owner heaps, so the pool's bytes are measured.
    owner_heaps: bool,
}

impl RetainedParseHeapV1 {
    fn new() -> Self {
        let heap = OwnerHeapV1::new();
        Self {
            owner_heaps: heap.is_some(),
            heap: RwLock::new(heap),
            bytes: AtomicU64::new(0),
        }
    }

    /// Measure under the write lock, when no thread allocates into the heap.
    fn measure(&self, heap: &Option<OwnerHeapV1>) -> u64 {
        let bytes = heap.as_ref().map_or(0, OwnerHeapV1::resident_bytes);
        self.bytes.store(bytes, Ordering::Release);
        bytes
    }
}

/// Cloneable production pool. Documents parse concurrently under per-document
/// locks; the map lock is held only for admission, eviction, and accounting.
#[derive(Clone)]
pub struct SharedRetainedParsePool {
    limits: RetainedParsePoolLimits,
    state: Arc<Mutex<RetainedParsePoolState>>,
    first_admissions: Arc<Mutex<BTreeMap<ParseDocumentKey, Weak<Mutex<()>>>>>,
    heap: Arc<RetainedParseHeapV1>,
}

impl Default for SharedRetainedParsePool {
    fn default() -> Self {
        Self::with_limits(RetainedParsePoolLimits::default())
    }
}

impl SharedRetainedParsePool {
    pub fn new(limits: RetainedParsePoolLimits) -> Result<Self, RetainedParsePoolOpenError> {
        if limits.max_documents == 0
            || limits.max_total_source_bytes == 0
            || limits.document.max_source_bytes == 0
        {
            return Err(RetainedParsePoolOpenError::EmptyCapacity);
        }
        Ok(Self::with_limits(limits))
    }

    fn with_limits(limits: RetainedParsePoolLimits) -> Self {
        Self {
            limits,
            state: Arc::new(Mutex::new(RetainedParsePoolState::default())),
            first_admissions: Arc::new(Mutex::new(BTreeMap::new())),
            heap: Arc::new(RetainedParseHeapV1::new()),
        }
    }

    /// Run work whose allocations the retained documents keep in the pool's
    /// heap.
    fn in_heap<R>(&self, work: impl FnOnce() -> R) -> R {
        let heap = self
            .heap
            .heap
            .read()
            .unwrap_or_else(PoisonError::into_inner);
        match heap.as_ref() {
            Some(heap) => heap.scope(work),
            None => work(),
        }
    }

    /// What the pool retains now, or `None` when it holds no document. The
    /// heap is re-measured unless a parse is allocating into it, in which
    /// case the measurement that parse's build started from stands.
    pub fn holding(&self) -> Option<RetainedParseHoldingV1> {
        let (documents, last_retained) = {
            let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            (state.documents.len(), state.last_retained?)
        };
        if documents == 0 {
            return None;
        }
        let bytes = self
            .heap
            .owner_heaps
            .then(|| match self.heap.heap.try_write() {
                Ok(heap) => self.heap.measure(&heap),
                Err(TryLockError::Poisoned(heap)) => self.heap.measure(&heap.into_inner()),
                Err(TryLockError::WouldBlock) => self.heap.bytes.load(Ordering::Acquire),
            });
        Some(RetainedParseHoldingV1 {
            documents,
            bytes,
            last_retained,
        })
    }

    pub fn parse(
        &self,
        identity: ParseDocumentIdentity,
        language_id: &str,
        source: &str,
    ) -> Result<ParseReport, ParseError> {
        self.parse_internal(identity, language_id, source, source, None, None)
            .map(|(report, _)| report)
    }

    pub fn parse_and_extract(
        &self,
        identity: ParseDocumentIdentity,
        language_id: &str,
        source: &str,
        extractor: &dyn LanguageExtractor,
    ) -> Result<(ParseReport, ParsedExtraction), ParseError> {
        crate::observe::measure_hot_loop!("code_index.collect.retained", {
            let (report, extraction) =
                self.parse_and_extract_artifact(identity, language_id, source, extractor)?;
            Ok((
                report,
                ParsedExtraction {
                    result: extraction.artifact.result,
                    disposition: extraction.disposition,
                    metrics: extraction.metrics,
                },
            ))
        })
    }

    /// Parse and extract one full canonical artifact from the pool-owned tree.
    /// The retained artifact, including import bindings, is the previous-state
    /// authority for incremental merging; this path never acquires a second
    /// parser.
    pub fn parse_and_extract_artifact(
        &self,
        identity: ParseDocumentIdentity,
        language_id: &str,
        source: &str,
        extractor: &dyn LanguageExtractor,
    ) -> Result<(ParseReport, ParsedExtractionArtifactV1), ParseError> {
        let prepared_source = extractor.prepare_parse_source(source);
        let (report, extraction) = self.parse_internal(
            identity,
            language_id,
            source,
            prepared_source.as_ref(),
            Some((extractor, None)),
            None,
        )?;
        extraction
            .map(|extraction| (report, extraction))
            .ok_or(ParseError::ParseFailed)
    }

    pub fn parse_and_extract_artifact_for_revision_with_control(
        &self,
        identity: ParseDocumentIdentity,
        language_id: &str,
        source: &str,
        extractor: &dyn LanguageExtractor,
        artifact_revision: &ExtractorRevision,
        control: Option<&dyn Fn() -> bool>,
    ) -> Result<(ParseReport, ParsedExtractionArtifactV1), ParseError> {
        crate::observe::measure_hot_loop!("code_index.collect.retained_artifact", {
            let prepared_source = extractor.prepare_parse_source(source);
            let (report, extraction) = self.parse_internal(
                identity,
                language_id,
                source,
                prepared_source.as_ref(),
                Some((extractor, Some(artifact_revision))),
                control,
            )?;
            match extraction {
                Some(extraction) => Ok((report, extraction)),
                None => Err(ParseError::ParseFailed),
            }
        })
    }

    fn parse_internal(
        &self,
        identity: ParseDocumentIdentity,
        language_id: &str,
        source: &str,
        prepared_source: &str,
        extraction: Option<(&dyn LanguageExtractor, Option<&ExtractorRevision>)>,
        control: Option<&dyn Fn() -> bool>,
    ) -> Result<(ParseReport, Option<ParsedExtractionArtifactV1>), ParseError> {
        let grammar_key = extraction
            .map(|(extractor, _)| extractor.retained_grammar_key(identity.logical_path()));
        let grammar_key = grammar_key.as_deref();
        crate::observe::measure_hot_loop!("code_index.collect.parse", {
            if source.len() > self.limits.max_total_source_bytes {
                self.record_failure();
                return Err(ParseError::SourceTooLarge {
                    size: source.len(),
                    limit: self.limits.max_total_source_bytes,
                });
            }
            // Everything a retained document keeps lives in the pool's heap,
            // its identity and the pool's bookkeeping included: allocated on
            // an indexing worker, those long-lived blocks pin that worker's
            // pages among the build's transient ones.
            let (identity, key, existing, admission_epoch) = self.in_heap(|| {
                let identity = identity.clone();
                let key = ParseDocumentKey::for_identity(&identity);
                let mut state = self
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                touch(&mut state.lru, &key);
                let existing = state.documents.get(&key).cloned();
                (identity, key, existing, state.clear_epoch)
            });

            match existing {
                Some(entry) => self.parse_existing(
                    key,
                    entry,
                    identity,
                    language_id,
                    source,
                    prepared_source,
                    grammar_key,
                    extraction,
                    control,
                ),
                None => {
                    // Serialize first admission per document. Unrelated documents
                    // parse concurrently; a second lookup after acquiring this
                    // key's gate keeps one retained tree for duplicate callers.
                    let first_admission = self.in_heap(|| self.first_admission(&key));
                    let _first_admission_guard = first_admission
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    let mut state = self
                        .state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    if let Some(entry) = state.documents.get(&key).cloned() {
                        self.in_heap(|| touch(&mut state.lru, &key));
                        drop(state);
                        return self.parse_existing(
                            key,
                            entry,
                            identity,
                            language_id,
                            source,
                            prepared_source,
                            grammar_key,
                            extraction,
                            control,
                        );
                    }
                    drop(state);
                    let opened = self.in_heap(|| {
                        let (document, report) = self.open_document(
                            identity,
                            language_id,
                            source,
                            prepared_source,
                            grammar_key,
                            control,
                        )?;
                        let parsed = extraction
                            .map(|(extractor, _)| {
                                document.extract_canonical_artifact(extractor, &report, None)
                            })
                            .transpose()?;
                        let retained_artifact =
                            parsed.as_ref().map(|parsed| parsed.artifact.clone());
                        Ok((document, report, parsed, retained_artifact))
                    });
                    let (document, report, parsed, retained_artifact) = match opened {
                        Ok(opened) => opened,
                        Err(error) => {
                            self.record_failure_at(admission_epoch);
                            return Err(error);
                        }
                    };
                    let current_size = document.retained_source_bytes();
                    self.in_heap(|| {
                        let entry = Arc::new(Mutex::new(RetainedEntry {
                            document,
                            artifact: retained_artifact,
                            artifact_revision: extraction
                                .and_then(|(_, revision)| revision.cloned()),
                        }));
                        let mut state = self
                            .state
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        if state.clear_epoch != admission_epoch {
                            return;
                        }
                        state.documents.insert(key.clone(), Arc::clone(&entry));
                        state.source_bytes.insert(key.clone(), current_size);
                        state.last_retained = Some(Instant::now());
                        touch(&mut state.lru, &key);
                        evict_to_limits(&mut state, &key, self.limits);
                        record_success(&mut state.stats, &report, parsed.as_ref());
                        state.stats.retained_documents = state.documents.len();
                        state.stats.retained_source_bytes =
                            state.source_bytes.values().copied().sum();
                    });
                    Ok((report, parsed))
                }
            }
        })
    }

    /// Parse and extract one full canonical artifact without retaining its
    /// tree. A full build parses every file once; its trees would only pay
    /// off for an increment that reparses the same document before eviction,
    /// which a pool bounded far below a repository's file count almost never
    /// sees, while holding them keeps each document's tree and parser alive.
    pub fn parse_and_extract_artifact_unretained_with_control(
        &self,
        identity: ParseDocumentIdentity,
        language_id: &str,
        source: &str,
        extractor: &dyn LanguageExtractor,
        control: Option<&dyn Fn() -> bool>,
    ) -> Result<(ParseReport, ParsedExtractionArtifactV1), ParseError> {
        crate::observe::measure_hot_loop!("code_index.collect.unretained_artifact", {
            if source.len() > self.limits.max_total_source_bytes {
                self.record_failure();
                return Err(ParseError::SourceTooLarge {
                    size: source.len(),
                    limit: self.limits.max_total_source_bytes,
                });
            }
            let prepared_source = extractor.prepare_parse_source(source);
            let grammar_key = extractor.retained_grammar_key(identity.logical_path());
            let opened = self.open_and_extract(
                identity,
                language_id,
                source,
                prepared_source.as_ref(),
                Some(grammar_key.as_str()),
                Some(extractor),
                control,
            );
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match opened {
                Ok((_, report, Some(extraction))) => {
                    record_success(&mut state.stats, &report, Some(&extraction));
                    Ok((report, extraction))
                }
                Ok((_, _, None)) => {
                    state.stats.failed_parses = state.stats.failed_parses.saturating_add(1);
                    Err(ParseError::ParseFailed)
                }
                Err(error) => {
                    state.stats.failed_parses = state.stats.failed_parses.saturating_add(1);
                    Err(error)
                }
            }
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn open_and_extract(
        &self,
        identity: ParseDocumentIdentity,
        language_id: &str,
        source: &str,
        prepared_source: &str,
        grammar_key: Option<&str>,
        extractor: Option<&dyn LanguageExtractor>,
        control: Option<&dyn Fn() -> bool>,
    ) -> Result<
        (
            RetainedParseDocument,
            ParseReport,
            Option<ParsedExtractionArtifactV1>,
        ),
        ParseError,
    > {
        let (document, report) = self.open_document(
            identity,
            language_id,
            source,
            prepared_source,
            grammar_key,
            control,
        )?;
        let parsed = extractor
            .map(|extractor| document.extract_canonical_artifact(extractor, &report, None))
            .transpose()?;
        Ok((document, report, parsed))
    }

    fn open_document(
        &self,
        identity: ParseDocumentIdentity,
        language_id: &str,
        source: &str,
        prepared_source: &str,
        grammar_key: Option<&str>,
        control: Option<&dyn Fn() -> bool>,
    ) -> Result<(RetainedParseDocument, ParseReport), ParseError> {
        match grammar_key {
            Some(grammar_key) => RetainedParseDocument::open_prepared_with_control(
                identity,
                language_id,
                grammar_key,
                source,
                prepared_source,
                self.limits.document,
                control,
            ),
            None => {
                RetainedParseDocument::open(identity, language_id, source, self.limits.document)
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn parse_existing(
        &self,
        key: ParseDocumentKey,
        entry: Arc<Mutex<RetainedEntry>>,
        identity: ParseDocumentIdentity,
        language_id: &str,
        source: &str,
        prepared_source: &str,
        grammar_key: Option<&str>,
        extraction: Option<(&dyn LanguageExtractor, Option<&ExtractorRevision>)>,
        control: Option<&dyn Fn() -> bool>,
    ) -> Result<(ParseReport, Option<ParsedExtractionArtifactV1>), ParseError> {
        let mut retained = entry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let language_changed = retained.document.language_id() != language_id;
        let report = self.in_heap(|| {
            if !language_changed {
                return match grammar_key {
                    Some(_) => retained.document.reparse_prepared_with_control(
                        identity,
                        source,
                        prepared_source,
                        control,
                    ),
                    None => retained.document.reparse(identity, source),
                };
            }
            self.open_document(
                identity,
                language_id,
                source,
                prepared_source,
                grammar_key,
                control,
            )
            .map(|(document, mut report)| {
                retained.document = document;
                report.reuse = ParseReuse::Reset {
                    reason: ParseResetReason::LanguageChanged,
                };
                report
            })
        });
        let report = match report {
            Ok(report) => report,
            Err(error) => {
                drop(retained);
                self.record_failure();
                return Err(error);
            }
        };
        let extraction = match extraction {
            Some((extractor, artifact_revision)) => {
                let previous = if language_changed
                    || retained.artifact_revision.as_ref() != artifact_revision
                {
                    None
                } else {
                    retained.artifact.as_ref()
                };
                let extracted = self.in_heap(|| {
                    retained
                        .document
                        .extract_canonical_artifact(extractor, &report, previous)
                        .map(|extraction| {
                            let artifact = extraction.artifact.clone();
                            (extraction, artifact)
                        })
                });
                match extracted {
                    Ok((extraction, artifact)) => {
                        retained.artifact = Some(artifact);
                        retained.artifact_revision = self.in_heap(|| artifact_revision.cloned());
                        Some(extraction)
                    }
                    Err(error) => {
                        retained.artifact = None;
                        retained.artifact_revision = None;
                        drop(retained);
                        self.record_failure();
                        return Err(error);
                    }
                }
            }
            None => {
                retained.artifact = None;
                retained.artifact_revision = None;
                None
            }
        };
        let current_size = retained.document.retained_source_bytes();
        self.in_heap(|| {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let is_still_retained = state
                .documents
                .get(&key)
                .is_some_and(|current| Arc::ptr_eq(current, &entry));
            if is_still_retained {
                state.source_bytes.insert(key.clone(), current_size);
                state.last_retained = Some(Instant::now());
                touch(&mut state.lru, &key);
                evict_to_limits(&mut state, &key, self.limits);
            }
            record_success(&mut state.stats, &report, extraction.as_ref());
            state.stats.retained_documents = state.documents.len();
            state.stats.retained_source_bytes = state.source_bytes.values().copied().sum();
        });
        Ok((report, extraction))
    }

    pub fn stats(&self) -> RetainedParsePoolStats {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .stats
            .clone()
    }

    /// Drop every retained document and return the pool's heap whole. The
    /// next increment reparses what it re-extracts from scratch. Never waits:
    /// a parse in the pool answers busy.
    pub fn release(&self) -> RetainedParsePoolReleaseV1 {
        let mut heap = match self.heap.heap.try_write() {
            Ok(heap) => heap,
            Err(TryLockError::Poisoned(heap)) => heap.into_inner(),
            Err(TryLockError::WouldBlock) => return RetainedParsePoolReleaseV1::Busy,
        };
        let documents = {
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            if state.documents.is_empty() {
                return RetainedParsePoolReleaseV1::Empty;
            }
            state.clear_epoch = state.clear_epoch.wrapping_add(1);
            state.source_bytes.clear();
            state.lru.clear();
            state.stats.retained_documents = 0;
            state.stats.retained_source_bytes = 0;
            std::mem::take(&mut state.documents)
        };
        let bytes = self.heap.owner_heaps.then(|| self.heap.measure(&heap));
        drop(documents);
        *heap = OwnerHeapV1::new();
        self.heap.bytes.store(0, Ordering::Release);
        RetainedParsePoolReleaseV1::Released { bytes }
    }

    fn record_failure(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.stats.failed_parses = state.stats.failed_parses.saturating_add(1);
    }

    fn record_failure_at(&self, admission_epoch: u64) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.clear_epoch == admission_epoch {
            state.stats.failed_parses = state.stats.failed_parses.saturating_add(1);
        }
    }

    fn first_admission(&self, key: &ParseDocumentKey) -> Arc<Mutex<()>> {
        let mut first_admissions = self
            .first_admissions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(admission) = first_admissions.get(key).and_then(Weak::upgrade) {
            return admission;
        }
        first_admissions.retain(|_, admission| admission.strong_count() != 0);
        let admission = Arc::new(Mutex::new(()));
        first_admissions.insert(key.clone(), Arc::downgrade(&admission));
        admission
    }
}

fn touch(lru: &mut VecDeque<ParseDocumentKey>, key: &ParseDocumentKey) {
    lru.retain(|candidate| candidate != key);
    lru.push_back(key.clone());
}

fn evict_to_limits(
    state: &mut RetainedParsePoolState,
    protected: &ParseDocumentKey,
    limits: RetainedParsePoolLimits,
) {
    loop {
        let bytes: usize = state.source_bytes.values().copied().sum();
        if state.documents.len() <= limits.max_documents && bytes <= limits.max_total_source_bytes {
            break;
        }
        let Some(candidate) = state.lru.pop_front() else {
            break;
        };
        if &candidate == protected {
            state.lru.push_back(candidate);
            if state.documents.len() == 1 {
                break;
            }
            continue;
        }
        // Removing the map's Arc is safe while another caller owns a clone:
        // that parse completes atomically but no longer counts as retained and
        // cannot reinsert itself after this eviction.
        state.documents.remove(&candidate);
        state.source_bytes.remove(&candidate);
        state.stats.evicted_documents = state.stats.evicted_documents.saturating_add(1);
    }
}

fn record_success(
    stats: &mut RetainedParsePoolStats,
    report: &ParseReport,
    extraction: Option<&ParsedExtractionArtifactV1>,
) {
    match report.reuse {
        ParseReuse::Initial => stats.initial_parses = stats.initial_parses.saturating_add(1),
        ParseReuse::Incremental => {
            stats.incremental_parses = stats.incremental_parses.saturating_add(1);
            stats.changed_bytes = stats
                .changed_bytes
                .saturating_add(report.metrics.changed_bytes as u64);
        }
        ParseReuse::Noop => stats.noop_parses = stats.noop_parses.saturating_add(1),
        ParseReuse::Reset { .. } => stats.reset_parses = stats.reset_parses.saturating_add(1),
    }
    if matches!(
        report.completeness,
        tracedecay_code_extraction::incremental::ParseCompleteness::Partial { .. }
    ) {
        stats.partial_parses = stats.partial_parses.saturating_add(1);
    }
    stats.parse_micros = stats
        .parse_micros
        .saturating_add(report.metrics.parse_elapsed.as_micros() as u64);
    if let Some(extraction) = extraction {
        match extraction.disposition {
            ParsedExtractionDisposition::FullDocument => {
                stats.full_extractions = stats.full_extractions.saturating_add(1);
            }
            ParsedExtractionDisposition::ChangedRegions => {
                stats.incremental_extractions = stats.incremental_extractions.saturating_add(1);
            }
            ParsedExtractionDisposition::Reset { .. } => {
                stats.reset_extractions = stats.reset_extractions.saturating_add(1);
            }
        }
        stats.visited_top_level_nodes = stats
            .visited_top_level_nodes
            .saturating_add(extraction.metrics.visited_top_level_nodes as u64);
        stats.extracted_bytes = stats
            .extracted_bytes
            .saturating_add(extraction.metrics.visited_bytes as u64);
    }
    crate::observe::add_parse_bytes(report.metrics.source_bytes as u64);
    if matches!(report.reuse, ParseReuse::Noop | ParseReuse::Incremental) {
        crate::observe::add_reused_parses(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracedecay_code_extraction::{
        LanguageRegistry, parsed_extraction::ParsedExtractionResetReason,
    };
    use tracedecay_domain::RepositoryDirtyStateV1;

    use tracedecay_domain::test_fixtures::id;

    fn identity() -> ParseDocumentIdentity {
        ParseDocumentIdentity::Repository {
            project_id: id("project.retained"),
            repository_id: id("repository.retained"),
            worktree_id: None,
            reference: None,
            commit: None,
            tree: None,
            dirty: RepositoryDirtyStateV1::Dirty,
            logical_path: "src/lib.rs".to_owned(),
        }
    }

    #[test]
    fn extractor_revision_change_discards_retained_extraction_artifact() {
        let source = "mod inner { pub fn value() {} }\npub use inner::*;\n";
        let pool = SharedRetainedParsePool::default();
        let registry = LanguageRegistry::new();
        let extractor = registry
            .extractor_for_file("src/lib.rs")
            .expect("Rust extractor");
        let v3 = id::<ExtractorRevision>("extractor.rust.v3");
        let v4 = id::<ExtractorRevision>("extractor.rust.v4");

        pool.parse_and_extract_artifact_for_revision_with_control(
            identity(),
            "rust",
            source,
            extractor,
            &v3,
            None,
        )
        .expect("historical extraction");
        {
            let entry = pool
                .state
                .lock()
                .expect("retained pool lock")
                .documents
                .values()
                .next()
                .cloned()
                .expect("retained document");
            entry
                .lock()
                .expect("retained entry lock")
                .artifact
                .as_mut()
                .expect("retained artifact")
                .imports
                .clear();
        }

        let (_, extraction) = pool
            .parse_and_extract_artifact_for_revision_with_control(
                identity(),
                "rust",
                source,
                extractor,
                &v4,
                None,
            )
            .expect("current extraction");

        assert_eq!(
            extraction.disposition,
            ParsedExtractionDisposition::Reset {
                reason: ParsedExtractionResetReason::MissingPriorExtraction,
            }
        );
        assert!(
            extraction
                .artifact
                .imports
                .iter()
                .any(|row| row.is_public && row.is_glob),
            "current extraction must not inherit the stale import row set"
        );
    }
}
