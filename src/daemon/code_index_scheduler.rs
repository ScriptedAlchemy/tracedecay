//! Daemon-owned scheduling and reconciliation for production code generations.
//!
//! Notify events are bounded wake-up hints only. Every run reconstructs its
//! source snapshot from gix's HEAD-tree/index/worktree status before content
//! digests decide whether publication is necessary.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use gix::bstr::ByteSlice;
use notify_debouncer_full::{
    DebounceEventResult, Debouncer, RecommendedCache, new_debouncer,
    notify::{RecommendedWatcher, RecursiveMode},
};
use serde::Serialize;
use thiserror::Error;
use tracedecay_domain::{
    ChunkerRevision, CodeGenerationId, ComponentRevision, ContentDigest,
    ExactAdmissionRuleRevision, FileOccurrenceId, ManifestDigest, PolicyRevisionId,
    PrivacyDomainId, ProjectionBatchReceiptV1, ProjectionBatchRequestV1, ProjectionKeyV1,
    ProjectionKindV1, ProjectionOperationV1, ProjectionOutcomeV1, RepositoryId,
    SanitizationReceiptId, SanitizedCodeFileV1, SanitizedCodeSnapshotV1, SanitizerRevision,
    ScoreDomainId, SnapshotFileDispositionV1, UtcMicros, WorktreeId, canonical_sha256,
};
use tree_sitter::{InputEdit, Parser, Point, Range, Tree};

use crate::{
    application::code_index::{
        DaemonCodeIndexControlV1, ProductionCodeIndexOwnerV1, open_production_code_index_owner_v1,
    },
    code_index::{
        chunks::{ExtractionAdmittedCodeSearchChunkV1, content_digest},
        languages::{LanguageRegistry, StaticLanguageRegistry},
        production::{
            CodeIndexAtomicPublicationPort, CodeIndexBuildRequestV1, CodeIndexCapturedFileV1,
            CodeIndexProductionConfigV1, CodeIndexProductionErrorV1,
            CodeIndexPublicationStoreErrorV1, CodeIndexPublishedGenerationV1,
        },
        projection::{
            ChunkProjectionDecisionV1, CodeChunkProjectionSink, ProjectionSinkErrorV1,
            build_batch_receipt,
        },
    },
    query::retrieval::{
        exact::{CentralExactAdmissionAuthorityV1, ExactLane},
        graph::{CodeGraphEvidenceAdapterV1, GraphLane, production_code_index_freshness},
        lexical::{
            CodeExactProjectionAdapterV1, CodeLexicalProjectionAdapterV1,
            CodeLexicalProjectionMetadataV1, LexicalLane,
        },
        ports::RetrievalPortError,
    },
};

const MAX_PENDING_HINTS: usize = 1_024;
const MAX_SUPERSEDED_RECONCILE_RETRIES: usize = 4;
const WATCH_DEBOUNCE: Duration = Duration::from_millis(75);

type ProductionOwner =
    ProductionCodeIndexOwnerV1<DaemonCodeIndexPublicationStoreV1, DaemonProjectionSinkV1>;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct CodeIndexBytePoolStatsV1 {
    pub inserted: u64,
    pub reused: u64,
}

#[derive(Default)]
pub(super) struct SharedCodeIndexBytePoolV1 {
    bytes: Mutex<BTreeMap<ContentDigest, Weak<[u8]>>>,
    inserted: AtomicU64,
    reused: AtomicU64,
}

impl SharedCodeIndexBytePoolV1 {
    fn intern(&self, bytes: Vec<u8>) -> (ContentDigest, Arc<[u8]>) {
        let digest = content_digest(&bytes);
        let mut pool = self.bytes.lock().expect("code-index byte-pool lock");
        if let Some(shared) = pool.get(&digest).and_then(Weak::upgrade) {
            self.reused.fetch_add(1, Ordering::Relaxed);
            return (digest, shared);
        }
        let shared: Arc<[u8]> = Arc::from(bytes);
        pool.insert(digest.clone(), Arc::downgrade(&shared));
        self.inserted.fetch_add(1, Ordering::Relaxed);
        (digest, shared)
    }

    fn stats(&self) -> CodeIndexBytePoolStatsV1 {
        CodeIndexBytePoolStatsV1 {
            inserted: self.inserted.load(Ordering::Relaxed),
            reused: self.reused.load(Ordering::Relaxed),
        }
    }
}

#[derive(Clone)]
struct DaemonCodeIndexPublicationStoreV1 {
    active: Arc<Mutex<Option<CodeIndexPublishedGenerationV1>>>,
    receipt_path: PathBuf,
}

#[derive(Serialize)]
struct DurablePublicationReceiptV1<'a> {
    generation_id: &'a str,
    snapshot_content_identity: &'a str,
    publication_digest: &'a str,
    sealed_at_micros: i64,
}

impl DaemonCodeIndexPublicationStoreV1 {
    fn new(store_root: &Path) -> Result<Self, CodeIndexSchedulerErrorV1> {
        std::fs::create_dir_all(store_root)?;
        Ok(Self {
            active: Arc::new(Mutex::new(None)),
            receipt_path: store_root.join("active-code-generation-v1.json"),
        })
    }
}

impl CodeIndexAtomicPublicationPort for DaemonCodeIndexPublicationStoreV1 {
    fn load_active(
        &self,
    ) -> Result<Option<CodeIndexPublishedGenerationV1>, CodeIndexPublicationStoreErrorV1> {
        Ok(self
            .active
            .lock()
            .map_err(|_| {
                CodeIndexPublicationStoreErrorV1::Unavailable(
                    "daemon publication lock is poisoned".to_owned(),
                )
            })?
            .clone())
    }

    fn publish_atomically(
        &mut self,
        expected_active_generation: Option<&CodeGenerationId>,
        generation: CodeIndexPublishedGenerationV1,
    ) -> Result<(), CodeIndexPublicationStoreErrorV1> {
        let mut active = self.active.lock().map_err(|_| {
            CodeIndexPublicationStoreErrorV1::Unavailable(
                "daemon publication lock is poisoned".to_owned(),
            )
        })?;
        if active
            .as_ref()
            .map(|current| &current.manifest().generation_id)
            != expected_active_generation
        {
            return Err(CodeIndexPublicationStoreErrorV1::CompareAndSwap);
        }
        let receipt = DurablePublicationReceiptV1 {
            generation_id: generation.manifest().generation_id.as_str(),
            snapshot_content_identity: generation.snapshot().content_identity.as_str(),
            publication_digest: generation.projection().publication_digest().as_str(),
            sealed_at_micros: generation.manifest().seal.sealed_at.0,
        };
        let bytes = serde_json::to_vec(&receipt).map_err(|error| {
            CodeIndexPublicationStoreErrorV1::Unavailable(format!(
                "publication receipt serialization failed: {error}"
            ))
        })?;
        let temporary = self.receipt_path.with_extension("json.tmp");
        std::fs::write(&temporary, bytes).map_err(|error| {
            CodeIndexPublicationStoreErrorV1::Unavailable(format!(
                "publication receipt staging failed: {error}"
            ))
        })?;
        std::fs::rename(&temporary, &self.receipt_path).map_err(|error| {
            CodeIndexPublicationStoreErrorV1::Unavailable(format!(
                "publication receipt activation failed: {error}"
            ))
        })?;
        *active = Some(generation);
        Ok(())
    }
}

#[derive(Default)]
struct DaemonProjectionSinkV1;

impl CodeChunkProjectionSink for DaemonProjectionSinkV1 {
    fn project_changed_chunks(
        &mut self,
        request: ProjectionBatchRequestV1,
    ) -> Result<ProjectionBatchReceiptV1, ProjectionSinkErrorV1> {
        let mut decisions = request
            .changes
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
            .collect::<Vec<_>>();
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
        decisions.extend(
            request
                .changes
                .reused
                .iter()
                .map(|change| ChunkProjectionDecisionV1 {
                    chunk_id: change.chunk_id.clone(),
                    prior_chunk_digest: change.prior_digest.clone(),
                    current_chunk_digest: change.current_digest.clone(),
                    operation: ProjectionOperationV1::Reused,
                    outcome: ProjectionOutcomeV1::Reused,
                    output_digest: None,
                }),
        );
        decisions.sort_by(|left, right| left.chunk_id.cmp(&right.chunk_id));
        build_batch_receipt(&request, &decisions)
            .map_err(|error| ProjectionSinkErrorV1::Rejected(error.to_string()))
    }
}

#[derive(Default)]
struct PendingHintsV1 {
    paths: BTreeSet<PathBuf>,
    overflow: bool,
}

impl PendingHintsV1 {
    fn path(&mut self, path: PathBuf) {
        if self.paths.len() >= MAX_PENDING_HINTS {
            self.paths.clear();
            self.overflow = true;
        } else {
            self.paths.insert(path);
        }
    }

    fn overflow(&mut self) {
        self.paths.clear();
        self.overflow = true;
    }

    fn take(&mut self) -> Self {
        std::mem::take(self)
    }
}

struct SavedTreeV1 {
    bytes: Arc<[u8]>,
    tree: Tree,
}

struct CapturedSnapshotV1 {
    snapshot: SanitizedCodeSnapshotV1,
    captured_files: Vec<CodeIndexCapturedFileV1>,
    bytes_by_path: BTreeMap<String, Arc<[u8]>>,
    changed_paths: BTreeSet<String>,
}

#[derive(Clone, Debug)]
pub(super) struct CodeIndexPublishEvidenceV1 {
    pub generation_id: CodeGenerationId,
    pub repository_id: RepositoryId,
    pub snapshot_content_identity: ContentDigest,
    pub lane_digest: ManifestDigest,
    pub file_occurrence_ids: Vec<FileOccurrenceId>,
    pub incremental_parse_files: usize,
    pub changed_ranges: usize,
    pub reextracted_files: usize,
    pub changed_chunks: usize,
    pub reused_chunks: usize,
    pub overflow_reconciled: bool,
}

#[derive(Clone, Debug)]
pub(super) struct CodeIndexNoopEvidenceV1 {
    pub snapshot_content_identity: ContentDigest,
    pub overflow_reconciled: bool,
}

#[derive(Clone, Debug)]
pub(super) enum CodeIndexReconcileOutcomeV1 {
    Published(CodeIndexPublishEvidenceV1),
    Noop(CodeIndexNoopEvidenceV1),
}

#[derive(Clone)]
pub(super) struct LatestCompleteCodeIndexV1 {
    generation: CodeIndexPublishedGenerationV1,
}

/// Production exact/lexical/graph owners bound to one immutable published
/// generation. Lanes remain independently disableable by omitting a field from
/// composition; this bundle only proves the daemon can mint all three from the
/// same sealed generation evidence.
pub(super) struct ProductionCodeIndexQueryOwnersV1 {
    pub exact: ExactLane<
        CentralExactAdmissionAuthorityV1,
        CodeExactProjectionAdapterV1<CentralExactAdmissionAuthorityV1>,
    >,
    pub lexical: LexicalLane<CodeLexicalProjectionAdapterV1>,
    pub graph: GraphLane<CodeGraphEvidenceAdapterV1>,
}

impl LatestCompleteCodeIndexV1 {
    pub fn exact(
        &self,
    ) -> Result<
        Vec<ExtractionAdmittedCodeSearchChunkV1>,
        crate::code_index::chunks::ChunkingFailureV1,
    > {
        self.generation.admitted_chunks()
    }

    pub fn lexical(&self) -> &[tracedecay_domain::CodeSearchChunkV1] {
        self.generation.chunks().chunks()
    }

    pub fn graph_edges(&self) -> &[tracedecay_domain::CanonicalRelationEdgeV1] {
        self.generation.edges()
    }

    pub fn graph_abstentions(&self) -> &[crate::code_index::chunks::CodeIndexEdgeAbstentionV1] {
        self.generation.edge_abstentions()
    }

    /// Connect Plan 15 exact/lexical/graph production owners to the latest
    /// complete published generation.
    pub fn production_query_owners(
        &self,
    ) -> Result<ProductionCodeIndexQueryOwnersV1, RetrievalPortError> {
        let generation_id = self.generation.manifest().generation_id.clone();
        let freshness = production_code_index_freshness(
            self.generation.manifest().seal.sealed_at,
            ComponentRevision::new("policy.daemon.v1")
                .map_err(|error| RetrievalPortError::Contract(error.to_string()))?,
        )?;
        let metadata = CodeLexicalProjectionMetadataV1 {
            generation: generation_id.clone(),
            repository_id: Some(self.generation.snapshot().repository.clone()),
            freshness: freshness.clone(),
            exact_retriever_revision: ComponentRevision::new("retriever.exact.daemon.v1")
                .map_err(|error| RetrievalPortError::Contract(error.to_string()))?,
            lexical_retriever_revision: ComponentRevision::new("retriever.lexical.daemon.v1")
                .map_err(|error| RetrievalPortError::Contract(error.to_string()))?,
            exact_score_domain: ScoreDomainId::new("score.exact.daemon.v1")
                .map_err(|error| RetrievalPortError::Contract(error.to_string()))?,
        };
        let admitted = self
            .generation
            .admitted_chunks()
            .map_err(|error| RetrievalPortError::Contract(error.to_string()))?;
        let lexical_projection = CodeLexicalProjectionAdapterV1::new_admitted(metadata, admitted)?;
        let authority = CentralExactAdmissionAuthorityV1::new(
            ExactAdmissionRuleRevision::new("exact-rules.daemon.v1")
                .map_err(|error| RetrievalPortError::Contract(error.to_string()))?,
        );
        let exact = ExactLane::new(
            authority.clone(),
            lexical_projection.exact_adapter(authority),
        );
        let lexical = LexicalLane::new(lexical_projection);
        let graph = GraphLane::new(CodeGraphEvidenceAdapterV1::new(
            generation_id,
            Some(self.generation.snapshot().repository.clone()),
            freshness,
            self.generation.edges(),
            self.generation.chunks().chunks(),
        )?);
        Ok(ProductionCodeIndexQueryOwnersV1 {
            exact,
            lexical,
            graph,
        })
    }
}

#[derive(Debug, Error)]
pub(super) enum CodeIndexSchedulerErrorV1 {
    #[error("code-index repository status failed: {0}")]
    Git(String),
    #[error("code-index filesystem operation failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("code-index identity construction failed: {0}")]
    Identity(String),
    #[error("code-index production owner failed: {0}")]
    Production(#[from] CodeIndexProductionErrorV1),
    #[error("code-index production owner configuration failed: {0}")]
    ProductionOpen(String),
    #[error("code-index watcher failed: {0}")]
    Watch(String),
}

pub(super) struct CodeIndexWorktreeSchedulerV1 {
    project_root: PathBuf,
    repository_id: RepositoryId,
    worktree_id: WorktreeId,
    byte_pool: Arc<SharedCodeIndexBytePoolV1>,
    owner: ProductionOwner,
    hints: Arc<Mutex<PendingHintsV1>>,
    wake: Arc<tokio::sync::Notify>,
    epoch: Arc<AtomicU64>,
    shutting_down: Arc<AtomicBool>,
    saved_trees: BTreeMap<String, SavedTreeV1>,
    latest_content_identity: Option<ContentDigest>,
    watcher: Option<Debouncer<RecommendedWatcher, RecommendedCache>>,
    /// Optional PR10 hook: schedule FastEmbed projection without joining it.
    semantic_schedule: Option<crate::application::semantic_runtime::SavedCodeGenerationScheduleHookV1>,
}

impl CodeIndexWorktreeSchedulerV1 {
    pub fn open(
        project_root: &Path,
        store_root: PathBuf,
        byte_pool: Arc<SharedCodeIndexBytePoolV1>,
    ) -> Result<Self, CodeIndexSchedulerErrorV1> {
        let project_root = project_root.canonicalize()?;
        let repository_id = repository_id(&project_root)?;
        let worktree_id = worktree_id(&project_root)?;
        let publication = DaemonCodeIndexPublicationStoreV1::new(&store_root)?;
        let owner = open_production_code_index_owner_v1(
            CodeIndexProductionConfigV1 {
                repository: repository_id.clone(),
                sanitizer_revision: id::<SanitizerRevision>("sanitizer.daemon.v1")?,
                policy_revision: id::<PolicyRevisionId>("policy.daemon.v1")?,
                chunker_revision: id::<ChunkerRevision>("chunker.daemon.v1")?,
                privacy_domain: id::<PrivacyDomainId>("privacy.local-code-index")?,
                privacy_key_epoch: 1,
                max_snapshot_age_micros: None,
            },
            publication,
            DaemonProjectionSinkV1,
        )
        .map_err(|error| CodeIndexSchedulerErrorV1::ProductionOpen(error.to_string()))?;
        let hints = Arc::new(Mutex::new(PendingHintsV1::default()));
        let wake = Arc::new(tokio::sync::Notify::new());
        let epoch = Arc::new(AtomicU64::new(0));
        let mut scheduler = Self {
            project_root,
            repository_id,
            worktree_id,
            byte_pool,
            owner,
            hints,
            wake,
            epoch,
            shutting_down: Arc::new(AtomicBool::new(false)),
            saved_trees: BTreeMap::new(),
            latest_content_identity: None,
            watcher: None,
            semantic_schedule: None,
        };
        scheduler.mount_watcher()?;
        Ok(scheduler)
    }

    /// Connect saved code generations to PR10 `schedule_generation`. The hook
    /// must return immediately; FastEmbed download/indexing never blocks
    /// exact/lexical/graph search.
    pub fn set_semantic_schedule_hook(
        &mut self,
        hook: crate::application::semantic_runtime::SavedCodeGenerationScheduleHookV1,
    ) {
        self.semantic_schedule = Some(hook);
    }

    fn mount_watcher(&mut self) -> Result<(), CodeIndexSchedulerErrorV1> {
        let hints = Arc::clone(&self.hints);
        let wake = Arc::clone(&self.wake);
        let epoch = Arc::clone(&self.epoch);
        let mut debouncer =
            new_debouncer(WATCH_DEBOUNCE, None, move |result: DebounceEventResult| {
                let mut hints = hints.lock().expect("code-index hint lock");
                match result {
                    Ok(events) => {
                        for event in events {
                            if event.need_rescan() {
                                hints.overflow();
                            } else {
                                for path in &event.paths {
                                    hints.path(path.clone());
                                }
                            }
                        }
                    }
                    Err(_) => hints.overflow(),
                }
                DaemonCodeIndexControlV1::advance(&epoch);
                wake.notify_one();
            })
            .map_err(|error| CodeIndexSchedulerErrorV1::Watch(error.to_string()))?;
        debouncer
            .watch(&self.project_root, RecursiveMode::Recursive)
            .map_err(|error| CodeIndexSchedulerErrorV1::Watch(error.to_string()))?;
        self.watcher = Some(debouncer);
        Ok(())
    }

    pub fn notify_path(&self, path: PathBuf) {
        self.hints.lock().expect("code-index hint lock").path(path);
        DaemonCodeIndexControlV1::advance(&self.epoch);
        self.wake.notify_one();
    }

    pub fn notify_overflow(&self) {
        self.hints.lock().expect("code-index hint lock").overflow();
        DaemonCodeIndexControlV1::advance(&self.epoch);
        self.wake.notify_one();
    }

    pub fn reconcile_now(
        &mut self,
    ) -> Result<CodeIndexReconcileOutcomeV1, CodeIndexSchedulerErrorV1> {
        let mut overflow_reconciled = false;
        for retry in 0..=MAX_SUPERSEDED_RECONCILE_RETRIES {
            let hints = self.hints.lock().expect("code-index hint lock").take();
            overflow_reconciled |= hints.overflow;
            let captured = self.capture_authoritative_snapshot()?;
            if self.latest_content_identity.as_ref() == Some(&captured.snapshot.content_identity) {
                return Ok(CodeIndexReconcileOutcomeV1::Noop(CodeIndexNoopEvidenceV1 {
                    snapshot_content_identity: captured.snapshot.content_identity,
                    overflow_reconciled,
                }));
            }

            let (next_trees, incremental_parse_files, changed_ranges) =
                self.incremental_parse_evidence(&captured)?;
            let control = DaemonCodeIndexControlV1::new(
                Arc::clone(&self.epoch),
                Arc::clone(&self.shutting_down),
            );
            let changed_files = captured.changed_paths.clone();
            let generation = self.owner.build_and_publish(
                CodeIndexBuildRequestV1 {
                    snapshot: captured.snapshot.clone(),
                    captured_files: captured.captured_files,
                    changed_files,
                    invalidations: BTreeSet::new(),
                    sealed_at: now_micros(),
                    target_projection_key: projection_key()?,
                },
                &control,
            );
            let generation = match generation {
                Ok(generation) => generation,
                Err(CodeIndexProductionErrorV1::Interrupted(
                    crate::code_index::production::CodeIndexInterruptionV1::Cancelled,
                )) if retry < MAX_SUPERSEDED_RECONCILE_RETRIES
                    && !self.shutting_down.load(Ordering::Acquire) =>
                {
                    std::thread::sleep(WATCH_DEBOUNCE);
                    continue;
                }
                Err(error) => return Err(error.into()),
            };
            self.saved_trees = next_trees;
            self.latest_content_identity = Some(captured.snapshot.content_identity.clone());

            // PR10: enqueue FastEmbed projection without waiting on download/index.
            if let Some(schedule) = &self.semantic_schedule {
                let _scheduled = schedule(&generation);
            }

            let changes = &generation.projection().request().changes;
            let lane_digest = canonical_sha256(&(
                generation.snapshot().content_identity.clone(),
                generation
                    .chunks()
                    .chunks()
                    .iter()
                    .map(|chunk| (&chunk.id, &chunk.content_digest))
                    .collect::<Vec<_>>(),
                generation.edges(),
            ))
            .map_err(|error| CodeIndexSchedulerErrorV1::Identity(error.to_string()))?;
            return Ok(CodeIndexReconcileOutcomeV1::Published(
                CodeIndexPublishEvidenceV1 {
                    generation_id: generation.manifest().generation_id.clone(),
                    repository_id: self.repository_id.clone(),
                    snapshot_content_identity: generation.snapshot().content_identity.clone(),
                    lane_digest,
                    file_occurrence_ids: generation
                        .snapshot()
                        .files
                        .iter()
                        .map(|file| file.file_occurrence_id.clone())
                        .collect(),
                    incremental_parse_files,
                    changed_ranges,
                    reextracted_files: captured.changed_paths.len(),
                    changed_chunks: changes.added_or_changed.len() + changes.deleted.len(),
                    reused_chunks: changes.reused.len(),
                    overflow_reconciled,
                },
            ));
        }
        unreachable!("the bounded reconciliation loop returns on its final attempt")
    }

    pub fn latest_complete(&self) -> Option<LatestCompleteCodeIndexV1> {
        self.owner
            .active_generation()
            .ok()
            .flatten()
            .map(|generation| LatestCompleteCodeIndexV1 { generation })
    }

    fn capture_authoritative_snapshot(
        &self,
    ) -> Result<CapturedSnapshotV1, CodeIndexSchedulerErrorV1> {
        let repository = gix::open(&self.project_root)
            .map_err(|error| CodeIndexSchedulerErrorV1::Git(error.to_string()))?;
        let index = repository
            .index_or_empty()
            .map_err(|error| CodeIndexSchedulerErrorV1::Git(error.to_string()))?;
        let mut candidate_paths = index
            .entries()
            .iter()
            .filter_map(|entry| {
                std::str::from_utf8(entry.path(&index).as_ref())
                    .ok()
                    .map(str::to_owned)
            })
            .collect::<BTreeSet<_>>();
        let mut changed_paths = BTreeSet::new();
        let status = repository
            .status(gix::progress::Discard)
            .map_err(|error| CodeIndexSchedulerErrorV1::Git(error.to_string()))?
            .untracked_files(gix::status::UntrackedFiles::Files)
            .into_iter(Vec::<gix::bstr::BString>::new())
            .map_err(|error| CodeIndexSchedulerErrorV1::Git(error.to_string()))?;
        for item in status {
            let item = item.map_err(|error| CodeIndexSchedulerErrorV1::Git(error.to_string()))?;
            let path = item.location().to_str_lossy().into_owned();
            changed_paths.insert(path.clone());
            candidate_paths.insert(path);
        }

        let registry = StaticLanguageRegistry::new();
        let mut files = Vec::new();
        let mut captured_files = Vec::new();
        let mut bytes_by_path = BTreeMap::new();
        for logical_path in candidate_paths {
            let absolute = self.project_root.join(&logical_path);
            if !absolute.is_file() {
                continue;
            }
            let Some(extension) = absolute.extension().and_then(|value| value.to_str()) else {
                continue;
            };
            let Some(descriptor) = registry.descriptor_for_extension(&extension.to_lowercase())
            else {
                continue;
            };
            let bytes = std::fs::read(&absolute)?;
            let (digest, shared) = self.byte_pool.intern(bytes);
            let occurrence = file_occurrence_id(
                &self.repository_id,
                &self.worktree_id,
                &logical_path,
                &digest,
            )?;
            files.push(SanitizedCodeFileV1 {
                file_occurrence_id: occurrence.clone(),
                logical_path: logical_path.clone(),
                language: Some(descriptor.language.clone()),
                content_digest: digest,
                disposition: SnapshotFileDispositionV1::Present,
            });
            captured_files.push(CodeIndexCapturedFileV1 {
                file_occurrence_id: occurrence,
                sanitized_bytes: shared.to_vec(),
            });
            bytes_by_path.insert(logical_path, shared);
        }
        files.sort_by(|left, right| {
            (&left.logical_path, &left.file_occurrence_id)
                .cmp(&(&right.logical_path, &right.file_occurrence_id))
        });
        captured_files
            .sort_by(|left, right| left.file_occurrence_id.cmp(&right.file_occurrence_id));
        let content_identity = snapshot_content_identity(&files);
        Ok(CapturedSnapshotV1 {
            snapshot: SanitizedCodeSnapshotV1 {
                repository: self.repository_id.clone(),
                worktree: Some(self.worktree_id.clone()),
                reference: None,
                source_revision: None,
                sanitizer_revision: id::<SanitizerRevision>("sanitizer.daemon.v1")?,
                sanitization_receipts: vec![sanitization_receipt(&content_identity)?],
                content_identity,
                captured_at: now_micros(),
                files,
            },
            captured_files,
            bytes_by_path,
            changed_paths,
        })
    }

    fn incremental_parse_evidence(
        &self,
        captured: &CapturedSnapshotV1,
    ) -> Result<(BTreeMap<String, SavedTreeV1>, usize, usize), CodeIndexSchedulerErrorV1> {
        let mut next = BTreeMap::new();
        let mut incremental_files = 0;
        let mut changed_range_count = 0;
        for file in &captured.snapshot.files {
            let bytes = captured
                .bytes_by_path
                .get(&file.logical_path)
                .expect("captured file bytes exist");
            let language = file
                .language
                .as_ref()
                .expect("present source has a language");
            let mut parser = Parser::new();
            parser
                .set_language(
                    &crate::extraction::ts_provider::try_language(language.as_str())
                        .map_err(CodeIndexSchedulerErrorV1::Identity)?,
                )
                .map_err(|error| CodeIndexSchedulerErrorV1::Identity(error.to_string()))?;
            let (tree, changed_ranges) = match self.saved_trees.get(&file.logical_path) {
                Some(saved) if saved.bytes.as_ref() != bytes.as_ref() => {
                    let mut edited = saved.tree.clone();
                    edited.edit(&single_input_edit(&saved.bytes, bytes));
                    let tree = parser.parse(bytes.as_ref(), Some(&edited)).ok_or_else(|| {
                        CodeIndexSchedulerErrorV1::Identity(
                            "tree-sitter incremental parse returned no tree".to_owned(),
                        )
                    })?;
                    let ranges = edited.changed_ranges(&tree).collect::<Vec<_>>();
                    incremental_files += 1;
                    changed_range_count += ranges.len().max(1);
                    (tree, ranges)
                }
                Some(saved) => (saved.tree.clone(), Vec::<Range>::new()),
                None => (
                    parser.parse(bytes.as_ref(), None).ok_or_else(|| {
                        CodeIndexSchedulerErrorV1::Identity(
                            "tree-sitter initial parse returned no tree".to_owned(),
                        )
                    })?,
                    Vec::new(),
                ),
            };
            let _ = changed_ranges;
            next.insert(
                file.logical_path.clone(),
                SavedTreeV1 {
                    bytes: Arc::clone(bytes),
                    tree,
                },
            );
        }
        Ok((next, incremental_files, changed_range_count))
    }
}

impl Drop for CodeIndexWorktreeSchedulerV1 {
    fn drop(&mut self) {
        self.shutting_down.store(true, Ordering::Release);
        self.watcher.take();
    }
}

struct MountedCodeIndexWorktreeV1 {
    scheduler: Arc<Mutex<CodeIndexWorktreeSchedulerV1>>,
    task: tokio::task::JoinHandle<()>,
}

#[derive(Clone)]
pub(super) struct CodeIndexSchedulerRegistryV1 {
    max_worktrees: usize,
    byte_pool: Arc<SharedCodeIndexBytePoolV1>,
    mounted: Arc<tokio::sync::Mutex<BTreeMap<PathBuf, MountedCodeIndexWorktreeV1>>>,
}

impl CodeIndexSchedulerRegistryV1 {
    pub fn new(max_worktrees: usize) -> Self {
        Self {
            max_worktrees,
            byte_pool: Arc::new(SharedCodeIndexBytePoolV1::default()),
            mounted: Arc::new(tokio::sync::Mutex::new(BTreeMap::new())),
        }
    }

    pub fn open_worktree(
        &self,
        project_root: &Path,
        store_root: PathBuf,
    ) -> Result<CodeIndexWorktreeSchedulerV1, CodeIndexSchedulerErrorV1> {
        if self.max_worktrees == 0 {
            return Err(CodeIndexSchedulerErrorV1::Identity(
                "code-index scheduler capacity is zero".to_owned(),
            ));
        }
        CodeIndexWorktreeSchedulerV1::open(project_root, store_root, Arc::clone(&self.byte_pool))
    }

    pub fn byte_pool_stats(&self) -> CodeIndexBytePoolStatsV1 {
        self.byte_pool.stats()
    }

    pub async fn mount_worktree(
        &self,
        project_root: &Path,
        store_root: PathBuf,
        semantic_schedule: Option<
            crate::application::semantic_runtime::SavedCodeGenerationScheduleHookV1,
        >,
    ) -> Result<bool, CodeIndexSchedulerErrorV1> {
        let project_root = project_root.canonicalize()?;
        let mut mounted = self.mounted.lock().await;
        if mounted.contains_key(&project_root) {
            return Ok(false);
        }
        if mounted.len() >= self.max_worktrees {
            return Err(CodeIndexSchedulerErrorV1::Identity(
                "code-index scheduler capacity is exhausted".to_owned(),
            ));
        }
        let mut opened = self.open_worktree(
            &project_root,
            store_root.join(sha256_hex(project_root.to_string_lossy().as_bytes())),
        )?;
        if let Some(hook) = semantic_schedule {
            opened.set_semantic_schedule_hook(hook);
        }
        let scheduler = Arc::new(Mutex::new(opened));
        let (wake, shutting_down) = {
            let scheduler = scheduler.lock().expect("code-index scheduler lock");
            (
                Arc::clone(&scheduler.wake),
                Arc::clone(&scheduler.shutting_down),
            )
        };
        let worker_scheduler = Arc::clone(&scheduler);
        let worker_wake = Arc::clone(&wake);
        let task = tokio::spawn(async move {
            loop {
                worker_wake.notified().await;
                if shutting_down.load(Ordering::Acquire) {
                    return;
                }
                let scheduler = Arc::clone(&worker_scheduler);
                let result = tokio::task::spawn_blocking(move || {
                    scheduler
                        .lock()
                        .expect("code-index scheduler lock")
                        .reconcile_now()
                })
                .await;
                if result.is_err() || shutting_down.load(Ordering::Acquire) {
                    return;
                }
            }
        });
        mounted.insert(project_root, MountedCodeIndexWorktreeV1 { scheduler, task });
        wake.notify_one();
        Ok(true)
    }

    pub async fn notify_path(&self, project_root: &Path, path: PathBuf) -> bool {
        let Ok(project_root) = project_root.canonicalize() else {
            return false;
        };
        let mounted = self.mounted.lock().await;
        let Some(worktree) = mounted.get(&project_root) else {
            return false;
        };
        worktree
            .scheduler
            .lock()
            .expect("code-index scheduler lock")
            .notify_path(path);
        true
    }

    pub async fn latest_generation_id(&self, project_root: &Path) -> Option<CodeGenerationId> {
        let project_root = project_root.canonicalize().ok()?;
        let mounted = self.mounted.lock().await;
        let worktree = mounted.get(&project_root)?;
        worktree
            .scheduler
            .lock()
            .ok()?
            .latest_complete()
            .map(|latest| latest.generation.manifest().generation_id.clone())
    }

    pub async fn shutdown(&self) {
        let mounted = std::mem::take(&mut *self.mounted.lock().await);
        for worktree in mounted.values() {
            let scheduler = worktree
                .scheduler
                .lock()
                .expect("code-index scheduler lock");
            scheduler.shutting_down.store(true, Ordering::Release);
            scheduler.wake.notify_one();
        }
        for (_, worktree) in mounted {
            let _ = worktree.task.await;
        }
    }
}

fn id<T>(value: &str) -> Result<T, CodeIndexSchedulerErrorV1>
where
    T: TryFrom<String>,
    T::Error: std::fmt::Display,
{
    T::try_from(value.to_owned())
        .map_err(|error| CodeIndexSchedulerErrorV1::Identity(error.to_string()))
}

fn repository_id(project_root: &Path) -> Result<RepositoryId, CodeIndexSchedulerErrorV1> {
    let common =
        crate::worktree::git_common_dir(project_root).unwrap_or_else(|| project_root.to_path_buf());
    let digest = sha256_hex(common.to_string_lossy().as_bytes());
    id(&format!("repository.daemon.{digest}"))
}

fn worktree_id(project_root: &Path) -> Result<WorktreeId, CodeIndexSchedulerErrorV1> {
    id(&format!(
        "worktree.daemon.{}",
        sha256_hex(project_root.to_string_lossy().as_bytes())
    ))
}

fn file_occurrence_id(
    repository: &RepositoryId,
    worktree: &WorktreeId,
    logical_path: &str,
    digest: &ContentDigest,
) -> Result<FileOccurrenceId, CodeIndexSchedulerErrorV1> {
    id(&format!(
        "file.daemon.{}",
        sha256_hex(
            format!(
                "{}\0{}\0{logical_path}\0{}",
                repository.as_str(),
                worktree.as_str(),
                digest.as_str()
            )
            .as_bytes()
        )
    ))
}

fn sanitization_receipt(
    content_identity: &ContentDigest,
) -> Result<SanitizationReceiptId, CodeIndexSchedulerErrorV1> {
    id(&format!(
        "receipt.daemon.{}",
        content_identity
            .as_str()
            .strip_prefix("sha256:")
            .unwrap_or(content_identity.as_str())
    ))
}

fn projection_key() -> Result<ProjectionKeyV1, CodeIndexSchedulerErrorV1> {
    Ok(ProjectionKeyV1 {
        kind: ProjectionKindV1::Lexical,
        schema_revision: "lexical.daemon.v1".to_owned(),
        profile_digest: id::<ManifestDigest>(&format!("sha256:{}", "d".repeat(64)))?,
    })
}

fn snapshot_content_identity(files: &[SanitizedCodeFileV1]) -> ContentDigest {
    let mut bytes = Vec::new();
    for file in files {
        bytes.extend_from_slice(file.logical_path.as_bytes());
        bytes.push(0);
        bytes.extend_from_slice(file.content_digest.as_str().as_bytes());
        bytes.push(0xff);
    }
    content_digest(&bytes)
}

fn now_micros() -> UtcMicros {
    UtcMicros(
        i64::try_from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_micros(),
        )
        .unwrap_or(i64::MAX),
    )
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(bytes))
}

fn single_input_edit(old: &[u8], new: &[u8]) -> InputEdit {
    let prefix = old
        .iter()
        .zip(new)
        .take_while(|(left, right)| left == right)
        .count();
    let suffix = old[prefix..]
        .iter()
        .rev()
        .zip(new[prefix..].iter().rev())
        .take_while(|(left, right)| left == right)
        .count();
    let old_end = old.len() - suffix;
    let new_end = new.len() - suffix;
    InputEdit {
        start_byte: prefix,
        old_end_byte: old_end,
        new_end_byte: new_end,
        start_position: point_for_offset(old, prefix),
        old_end_position: point_for_offset(old, old_end),
        new_end_position: point_for_offset(new, new_end),
    }
}

fn point_for_offset(bytes: &[u8], offset: usize) -> Point {
    let prefix = &bytes[..offset.min(bytes.len())];
    let row = prefix.iter().filter(|byte| **byte == b'\n').count();
    let column = prefix
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(prefix.len(), |newline| prefix.len() - newline - 1);
    Point::new(row, column)
}

#[cfg(test)]
mod tests;
