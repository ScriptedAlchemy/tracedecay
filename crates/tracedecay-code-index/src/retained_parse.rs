//! Bounded retained parse-tree pool for production code indexing.
//!
//! The leaf parser owns Tree-sitter state. This pool owns only checkout/
//! document partitioning, deterministic eviction, and aggregate operational
//! measurements. It is process-local and is never serialized with a code
//! generation.

use std::{
    collections::{BTreeMap, VecDeque},
    sync::{Arc, Mutex},
};

use thiserror::Error;
use tracedecay_code_extraction::incremental::{
    ParseDocumentIdentity, ParseError, ParseLimits, ParseReport, ParseResetReason, ParseReuse,
    RetainedParseDocument,
};
use tracedecay_domain::{ManifestDigest, ProjectId, RepositoryId, WorktreeId};

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
    pub changed_bytes: u64,
    pub parse_micros: u64,
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
}

#[derive(Default)]
struct RetainedParsePoolState {
    documents: BTreeMap<ParseDocumentKey, Arc<Mutex<RetainedEntry>>>,
    source_bytes: BTreeMap<ParseDocumentKey, usize>,
    lru: VecDeque<ParseDocumentKey>,
    stats: RetainedParsePoolStats,
}

/// Cloneable production pool. Documents parse concurrently under per-document
/// locks; the map lock is held only for admission, eviction, and accounting.
#[derive(Clone)]
pub struct SharedRetainedParsePool {
    limits: RetainedParsePoolLimits,
    state: Arc<Mutex<RetainedParsePoolState>>,
}

impl Default for SharedRetainedParsePool {
    fn default() -> Self {
        Self {
            limits: RetainedParsePoolLimits::default(),
            state: Arc::new(Mutex::new(RetainedParsePoolState::default())),
        }
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
        Ok(Self {
            limits,
            state: Arc::new(Mutex::new(RetainedParsePoolState::default())),
        })
    }

    pub fn parse(
        &self,
        identity: ParseDocumentIdentity,
        language_id: &str,
        source: &str,
    ) -> Result<ParseReport, ParseError> {
        if source.len() > self.limits.max_total_source_bytes {
            self.record_failure();
            return Err(ParseError::SourceTooLarge {
                size: source.len(),
                limit: self.limits.max_total_source_bytes,
            });
        }
        let key = ParseDocumentKey::for_identity(&identity);
        let existing = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            touch(&mut state.lru, &key);
            state.documents.get(&key).cloned()
        };

        match existing {
            Some(entry) => self.parse_existing(key, entry, identity, language_id, source),
            None => {
                // Serialize only first admission. The second lookup closes the
                // race between the optimistic lookup above and this bounded
                // parse, so one document can never acquire two retained trees.
                let mut state = self
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if let Some(entry) = state.documents.get(&key).cloned() {
                    touch(&mut state.lru, &key);
                    drop(state);
                    return self.parse_existing(key, entry, identity, language_id, source);
                }
                let (document, report) = match RetainedParseDocument::open(
                    identity,
                    language_id,
                    source,
                    self.limits.document,
                ) {
                    Ok(parsed) => parsed,
                    Err(error) => {
                        self.record_failure();
                        return Err(error);
                    }
                };
                let current_size = document.retained_source_bytes();
                let entry = Arc::new(Mutex::new(RetainedEntry { document }));
                state.documents.insert(key.clone(), Arc::clone(&entry));
                state.source_bytes.insert(key.clone(), current_size);
                touch(&mut state.lru, &key);
                evict_to_limits(&mut state, &key, self.limits);
                record_success(&mut state.stats, &report);
                state.stats.retained_documents = state.documents.len();
                state.stats.retained_source_bytes = state.source_bytes.values().copied().sum();
                Ok(report)
            }
        }
    }

    fn parse_existing(
        &self,
        key: ParseDocumentKey,
        entry: Arc<Mutex<RetainedEntry>>,
        identity: ParseDocumentIdentity,
        language_id: &str,
        source: &str,
    ) -> Result<ParseReport, ParseError> {
        let mut retained = entry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let report = if retained.document.language_id() == language_id {
            retained.document.reparse(identity, source)
        } else {
            RetainedParseDocument::open(identity, language_id, source, self.limits.document).map(
                |(document, mut report)| {
                    retained.document = document;
                    report.reuse = ParseReuse::Reset {
                        reason: ParseResetReason::LanguageChanged,
                    };
                    report
                },
            )
        };
        let report = match report {
            Ok(report) => report,
            Err(error) => {
                drop(retained);
                self.record_failure();
                return Err(error);
            }
        };
        let current_size = retained.document.retained_source_bytes();
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
            touch(&mut state.lru, &key);
            evict_to_limits(&mut state, &key, self.limits);
        }
        record_success(&mut state.stats, &report);
        state.stats.retained_documents = state.documents.len();
        state.stats.retained_source_bytes = state.source_bytes.values().copied().sum();
        Ok(report)
    }

    pub fn stats(&self) -> RetainedParsePoolStats {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .stats
            .clone()
    }

    pub fn clear(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.documents.clear();
        state.source_bytes.clear();
        state.lru.clear();
        state.stats.retained_documents = 0;
        state.stats.retained_source_bytes = 0;
    }

    fn record_failure(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.stats.failed_parses = state.stats.failed_parses.saturating_add(1);
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

fn record_success(stats: &mut RetainedParsePoolStats, report: &ParseReport) {
    match report.reuse {
        ParseReuse::Initial => stats.initial_parses = stats.initial_parses.saturating_add(1),
        ParseReuse::Incremental => {
            stats.incremental_parses = stats.incremental_parses.saturating_add(1);
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
    stats.changed_bytes = stats
        .changed_bytes
        .saturating_add(report.metrics.changed_bytes as u64);
    stats.parse_micros = stats
        .parse_micros
        .saturating_add(report.metrics.parse_elapsed.as_micros() as u64);
}
