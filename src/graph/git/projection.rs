//! Incremental, bounded Git-history projection for health reads.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::Path;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_application::{
    GitHealthProjectionAvailabilityV1, GitHealthProjectionBindingV1,
    GitHealthProjectionChurnEntryV1, GitHealthProjectionChurnPageV1, GitHealthProjectionCoverageV1,
    GitHealthProjectionPartialReasonV1, GitHealthProjectionSnapshotV1, GitHealthProjectionSourceV1,
    GitHealthProjectionUnavailableReasonV1,
};
use tracedecay_domain::GitOidV1;
use tracedecay_graph_db::{
    GraphCancellation, GraphDb, GraphDbError, GraphDbLocation, GraphDbOpenOptions, GraphDurability,
    GraphEntityId, GraphFormatVersion, GraphLabel, GraphMutation, GraphProjectionReadRequest,
    GraphProjectionTelemetryRequest, GraphSnapshot, GraphWatermark, GraphWriteBatch,
    ProjectionReplacement, SourceGeneration,
};

use crate::application::context::CancellationToken;

mod native;
mod persistence;

pub(crate) use native::capture_source;
use native::{
    AncestorCheckV1, CollectCommitError, collect_commit_record, is_ancestor_bounded,
    require_current_target,
};
use persistence::{
    authenticate_snapshot_entities, coalesce_mutations, commit_entity, commit_entity_id,
    commit_record_from_entity, file_entity, file_entity_id, namespace, projection, state_entity,
};

const HISTORY_WINDOW_SECS: i64 = 90 * 24 * 60 * 60;
const WINDOW_BUCKET_SECS: i64 = 24 * 60 * 60;
pub(super) const MAX_CHANGED_FILES_PER_COMMIT: usize = 20_000;
const MAX_COMMIT_RECORD_PATH_BYTES: usize = 768 * 1024;
const MAX_WINDOW_COMMITS: usize = 20_000;
const MAX_UNIQUE_PATHS: usize = 20_000;
const MAX_CHANGED_PATH_REFERENCES: usize = 50_000;
const MAX_PATH_BYTES: usize = 8 * 1024 * 1024;
const MAX_DURABLE_FRONTIER: usize = 512;
const MAX_HISTORY_COMMITS_TRAVERSED: usize = 100_000;
const MAX_ANCESTRY_COMMITS: usize = 100_000;
const MAX_PROJECTION_ENTITIES: usize = MAX_WINDOW_COMMITS + MAX_UNIQUE_PATHS + 2;
const PROJECTION_PAGE_SIZE: usize = 256;
const GRAPH_FORMAT_VERSION: u32 = 2;
const PROJECTION: &str = "git-health";
const READY_ENTITY: &str = "git-health-ready";
const WORKING_ENTITY: &str = "git-health-working";
const STATE_PROPERTY: &str = "state";
const COMMIT_PROPERTY: &str = "commit";
const FILE_PATH_PROPERTY: &str = "path";
const FILE_CHURN_PROPERTY: &str = "churn";
const COMMIT_LABEL: &str = "GitHealthCommit";
const FILE_LABEL: &str = "GitHealthFile";
const STATE_LABEL: &str = "GitHealthProjectionState";
const GENERATION_DOMAIN: &str = "tracedecay.git-health.projection-generation.v1";
const NAMESPACE_DOMAIN: &str = "tracedecay.git-health.namespace.v1";
const RESET_GENERATION_DOMAIN: &str = "tracedecay.git-health.reset-generation.v1";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct GitHealthProjectionProgressV1 {
    pub target: GitHealthProjectionSourceV1,
    pub commits_examined: usize,
    pub complete: bool,
}

#[derive(Debug, Error)]
pub(crate) enum GitHealthProjectionError {
    #[error("Git health projection was cancelled")]
    Cancelled,
    #[error("Git health projection batch limit must be positive")]
    InvalidBatchLimit,
    #[error("Git health projection scope no longer matches the mounted worktree")]
    ScopeDrift,
    #[error("native Git health source is unavailable: {0}")]
    Git(String),
    #[error("Git health graph projection is unavailable: {0}")]
    Graph(String),
    #[error("Git health projection store requires reset: {0}")]
    ResetRequired(String),
    #[error("Git health projection is corrupt: {0}")]
    Corrupt(String),
}

impl GitHealthProjectionError {
    pub(crate) const fn unavailable_reason(&self) -> GitHealthProjectionUnavailableReasonV1 {
        match self {
            Self::ScopeDrift => GitHealthProjectionUnavailableReasonV1::ScopeDrift,
            Self::Git(_) => GitHealthProjectionUnavailableReasonV1::NativeGitUnavailable,
            Self::Graph(_) | Self::InvalidBatchLimit => {
                GitHealthProjectionUnavailableReasonV1::ProjectionStoreUnavailable
            }
            Self::ResetRequired(_) => GitHealthProjectionUnavailableReasonV1::ResetRequired,
            Self::Corrupt(_) => GitHealthProjectionUnavailableReasonV1::CorruptProjection,
            Self::Cancelled => GitHealthProjectionUnavailableReasonV1::ProjectionStoreUnavailable,
        }
    }
}

impl From<GraphDbError> for GitHealthProjectionError {
    fn from(error: GraphDbError) -> Self {
        match error {
            GraphDbError::Cancelled => Self::Cancelled,
            GraphDbError::ResetRequired { message } => Self::ResetRequired(message),
            GraphDbError::Corrupt { message } => Self::Corrupt(message),
            other => Self::Graph(other.to_string()),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct ProjectionCountersV1 {
    commits_projected: usize,
    batches_completed: u64,
    unique_paths: usize,
    changed_path_references: usize,
    path_bytes: usize,
    coverage: GitHealthProjectionCoverageV1,
}

impl Default for ProjectionCountersV1 {
    fn default() -> Self {
        Self {
            commits_projected: 0,
            batches_completed: 0,
            unique_paths: 0,
            changed_path_references: 0,
            path_bytes: 0,
            coverage: GitHealthProjectionCoverageV1::Complete,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct ReadyStateV1 {
    source: GitHealthProjectionSourceV1,
    counters: ProjectionCountersV1,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct WorkingStateV1 {
    target: GitHealthProjectionSourceV1,
    stop_at: Option<GitOidV1>,
    pending: VecDeque<GitOidV1>,
    counters: ProjectionCountersV1,
    expiration_complete: bool,
    history_commits_traversed: usize,
    complete: bool,
}

impl WorkingStateV1 {
    fn empty(target: GitHealthProjectionSourceV1) -> Self {
        Self {
            stop_at: None,
            pending: VecDeque::from([target.commit.clone()]),
            target,
            counters: ProjectionCountersV1::default(),
            expiration_complete: true,
            history_commits_traversed: 0,
            complete: false,
        }
    }

    fn from_ready(target: GitHealthProjectionSourceV1, ready: &ReadyStateV1) -> Self {
        let mut pending = VecDeque::new();
        if target.commit != ready.source.commit {
            pending.push_back(target.commit.clone());
        }
        Self {
            stop_at: Some(ready.source.commit.clone()),
            expiration_complete: target.window_start_epoch_secs
                == ready.source.window_start_epoch_secs,
            target,
            pending,
            counters: ready.counters.clone(),
            history_commits_traversed: 0,
            complete: false,
        }
    }

    fn mark_partial(&mut self, reason: GitHealthProjectionPartialReasonV1) {
        self.counters.coverage = GitHealthProjectionCoverageV1::Partial { reason };
        self.pending.clear();
        self.complete = true;
    }

    fn admit_parents(&mut self, parents: &[GitOidV1]) {
        if self.pending.len().saturating_add(parents.len()) > MAX_DURABLE_FRONTIER {
            self.mark_partial(GitHealthProjectionPartialReasonV1::FrontierLimit);
        } else {
            self.pending.extend(parents.iter().cloned());
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct CommitRecordV1 {
    oid: GitOidV1,
    tree: GitOidV1,
    committed_at_epoch_secs: i64,
    parents: Vec<GitOidV1>,
    changed_files: Vec<String>,
}

#[derive(Clone)]
struct TokenCancellation(CancellationToken);

impl GraphCancellation for TokenCancellation {
    fn is_cancelled(&self) -> bool {
        self.0.is_cancelled()
    }
}

#[derive(Clone)]
pub(crate) struct GitHealthProjectionStoreV1 {
    database: GraphDb,
}

impl GitHealthProjectionStoreV1 {
    pub(crate) fn open(
        path: &Path,
        cancellation: &CancellationToken,
    ) -> Result<Self, GitHealthProjectionError> {
        cancellation_checkpoint(cancellation)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| {
                GitHealthProjectionError::Graph(format!(
                    "could not create project graph directory: {error}"
                ))
            })?;
        }
        let graph_cancellation: Arc<dyn GraphCancellation> =
            Arc::new(TokenCancellation(cancellation.clone()));
        let database = GraphDb::open(GraphDbOpenOptions {
            location: GraphDbLocation::Persistent(path.to_path_buf()),
            expected_format: GraphFormatVersion::new(GRAPH_FORMAT_VERSION)?,
            durability: GraphDurability::Sync,
            cancellation: graph_cancellation,
        })?;
        Ok(Self { database })
    }

    pub(crate) fn database(&self) -> GraphDb {
        self.database.clone()
    }

    pub(crate) fn from_database(database: GraphDb) -> Self {
        Self { database }
    }

    pub(crate) fn reset_binding(
        &self,
        binding: &GitHealthProjectionBindingV1,
        cancellation: &CancellationToken,
    ) -> Result<(), GitHealthProjectionError> {
        cancellation_checkpoint(cancellation)?;
        let generation = tracedecay_domain::canonical_sha256(&(RESET_GENERATION_DOMAIN, binding))
            .map_err(|error| GitHealthProjectionError::Corrupt(error.to_string()))?;
        let graph_cancellation: Arc<dyn GraphCancellation> =
            Arc::new(TokenCancellation(cancellation.clone()));
        self.database.replace_projection(ProjectionReplacement {
            namespace: namespace(binding)?,
            projection: projection()?,
            source_generation: SourceGeneration::new(generation.as_str())?,
            next_watermark: GraphWatermark::new(format!("{}:reset", generation.as_str()))?,
            entities: Vec::new(),
            relations: Vec::new(),
            cancellation: graph_cancellation,
        })?;
        Ok(())
    }

    pub(crate) fn read(
        &self,
        binding: &GitHealthProjectionBindingV1,
    ) -> GitHealthProjectionAvailabilityV1 {
        self.read_inner(binding).unwrap_or_else(|error| {
            GitHealthProjectionAvailabilityV1::Unavailable {
                reason: error.unavailable_reason(),
            }
        })
    }

    fn read_inner(
        &self,
        binding: &GitHealthProjectionBindingV1,
    ) -> Result<GitHealthProjectionAvailabilityV1, GitHealthProjectionError> {
        let cancellation: Arc<dyn GraphCancellation> =
            Arc::new(TokenCancellation(CancellationToken::new()));
        let ready =
            self.read_state::<ReadyStateV1>(binding, READY_ENTITY, Arc::clone(&cancellation))?;
        let working =
            self.read_state::<WorkingStateV1>(binding, WORKING_ENTITY, Arc::clone(&cancellation))?;
        self.authenticate_projection_commit(
            binding,
            ready.as_ref(),
            working.as_ref(),
            Arc::clone(&cancellation),
        )?;
        if let Some(working) = working.as_ref().filter(|working| !working.complete) {
            return Ok(GitHealthProjectionAvailabilityV1::Warming {
                target: Some(working.target.clone()),
            });
        }
        let Some(ready) = ready else {
            return Ok(GitHealthProjectionAvailabilityV1::Warming {
                target: working.map(|working| working.target),
            });
        };
        if working
            .as_ref()
            .is_some_and(|working| working.target != ready.source)
        {
            return Err(GitHealthProjectionError::Corrupt(
                "complete working state disagrees with ready source".to_owned(),
            ));
        }
        let entities = self.projection_entities(binding, cancellation)?;
        authenticate_snapshot_entities(entities, &ready)?;
        Ok(GitHealthProjectionAvailabilityV1::Ready {
            snapshot: GitHealthProjectionSnapshotV1 {
                source: ready.source,
                commits_projected: ready.counters.commits_projected,
                batches_completed: ready.counters.batches_completed,
                churn_entries: ready.counters.unique_paths,
                coverage: ready.counters.coverage,
            },
        })
    }

    pub(crate) fn read_churn_page(
        &self,
        binding: &GitHealthProjectionBindingV1,
        snapshot: &GitHealthProjectionSnapshotV1,
        after_cursor: Option<&str>,
        limit: usize,
    ) -> Result<GitHealthProjectionChurnPageV1, GitHealthProjectionError> {
        if snapshot.source.binding != *binding {
            return Err(GitHealthProjectionError::ScopeDrift);
        }
        let cancellation: Arc<dyn GraphCancellation> =
            Arc::new(TokenCancellation(CancellationToken::new()));
        let database = self.database.snapshot()?;
        self.authenticate_projection_source(
            &database,
            binding,
            &snapshot.source,
            snapshot.batches_completed,
            Some(
                snapshot
                    .commits_projected
                    .saturating_add(snapshot.churn_entries)
                    .saturating_add(2),
            ),
            Arc::clone(&cancellation),
        )?;
        let page = database.read_projection(GraphProjectionReadRequest {
            namespace: namespace(binding)?,
            projection: projection()?,
            after_entity: after_cursor.map(GraphEntityId::new).transpose()?,
            after_relation: None,
            max_entities: limit.min(PROJECTION_PAGE_SIZE),
            max_relations: 0,
            cancellation,
        })?;
        let file_label = std::collections::BTreeSet::from([GraphLabel::new(FILE_LABEL)?]);
        let mut entries = Vec::new();
        for entity in page.entities {
            if entity.labels == file_label {
                let (path, churn) = persistence::file_record_from_entity(&entity)?;
                entries.push(GitHealthProjectionChurnEntryV1 { path, churn });
            }
        }
        Ok(GitHealthProjectionChurnPageV1 {
            entries,
            next_cursor: page
                .next_entity
                .map(|identity| identity.as_str().to_owned()),
        })
    }

    fn authenticate_projection_commit(
        &self,
        binding: &GitHealthProjectionBindingV1,
        ready: Option<&ReadyStateV1>,
        working: Option<&WorkingStateV1>,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<(), GitHealthProjectionError> {
        let Some(source) = working
            .map(|state| {
                (
                    &state.target,
                    state.counters.batches_completed,
                    state.complete.then(|| {
                        state
                            .counters
                            .commits_projected
                            .saturating_add(state.counters.unique_paths)
                            .saturating_add(2)
                    }),
                )
            })
            .or_else(|| {
                ready.map(|state| {
                    (
                        &state.source,
                        state.counters.batches_completed,
                        Some(
                            state
                                .counters
                                .commits_projected
                                .saturating_add(state.counters.unique_paths)
                                .saturating_add(2),
                        ),
                    )
                })
            })
        else {
            return Ok(());
        };
        let database = self.database.snapshot()?;
        self.authenticate_projection_source(
            &database,
            binding,
            source.0,
            source.1,
            source.2,
            cancellation,
        )
    }

    fn authenticate_projection_source(
        &self,
        database: &GraphSnapshot,
        binding: &GitHealthProjectionBindingV1,
        source: &GitHealthProjectionSourceV1,
        batches_completed: u64,
        expected_entities: Option<usize>,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<(), GitHealthProjectionError> {
        let telemetry = database
            .projection_telemetry(GraphProjectionTelemetryRequest {
                namespace: namespace(binding)?,
                projection: projection()?,
                cancellation,
            })?
            .ok_or_else(|| {
                GitHealthProjectionError::Corrupt(
                    "persisted Git health state has no Grafeo projection commit".to_owned(),
                )
            })?;
        let expected_watermark = if batches_completed == 0 {
            format!("{}:initialize", source.projection_generation.as_str())
        } else {
            format!(
                "{}:{}",
                source.projection_generation.as_str(),
                batches_completed
            )
        };
        if telemetry.source_generation.as_str() != source.projection_generation.as_str()
            || telemetry.watermark.as_str() != expected_watermark
            || telemetry.relation_count != 0
            || expected_entities.is_some_and(|expected| {
                u64::try_from(expected).ok() != Some(telemetry.entity_count)
            })
        {
            return Err(GitHealthProjectionError::Corrupt(
                "Grafeo projection generation, watermark, or cardinality does not authenticate persisted Git health state"
                    .to_owned(),
            ));
        }
        Ok(())
    }

    pub(crate) fn advance(
        &self,
        repository_root: &Path,
        binding: &GitHealthProjectionBindingV1,
        now_epoch_secs: i64,
        commit_batch_limit: usize,
        cancellation: &CancellationToken,
    ) -> Result<GitHealthProjectionProgressV1, GitHealthProjectionError> {
        self.advance_with_history_limit(
            repository_root,
            binding,
            now_epoch_secs,
            commit_batch_limit,
            MAX_HISTORY_COMMITS_TRAVERSED,
            cancellation,
        )
    }

    fn advance_with_history_limit(
        &self,
        repository_root: &Path,
        binding: &GitHealthProjectionBindingV1,
        now_epoch_secs: i64,
        commit_batch_limit: usize,
        history_commit_limit: usize,
        cancellation: &CancellationToken,
    ) -> Result<GitHealthProjectionProgressV1, GitHealthProjectionError> {
        if commit_batch_limit == 0 {
            return Err(GitHealthProjectionError::InvalidBatchLimit);
        }
        cancellation_checkpoint(cancellation)?;
        let target = capture_source(repository_root, binding, now_epoch_secs)?;
        let graph_cancellation: Arc<dyn GraphCancellation> =
            Arc::new(TokenCancellation(cancellation.clone()));
        let ready = self.read_state::<ReadyStateV1>(
            binding,
            READY_ENTITY,
            Arc::clone(&graph_cancellation),
        )?;
        let persisted_working = self.read_state::<WorkingStateV1>(
            binding,
            WORKING_ENTITY,
            Arc::clone(&graph_cancellation),
        )?;
        if ready.as_ref().is_some_and(|ready| ready.source == target)
            && persisted_working
                .as_ref()
                .is_none_or(|working| working.complete || working.target != target)
        {
            return Ok(GitHealthProjectionProgressV1 {
                target,
                commits_examined: 0,
                complete: true,
            });
        }
        let repository = gix::open(repository_root)
            .map_err(|error| GitHealthProjectionError::Git(error.to_string()))?;
        let mut working =
            match persisted_working.filter(|state| !state.complete && state.target == target) {
                Some(working) => working,
                None => {
                    require_current_target(repository_root, binding, now_epoch_secs, &target)?;
                    self.initialize_target(
                        binding,
                        &repository,
                        ready.as_ref(),
                        target.clone(),
                        Arc::clone(&graph_cancellation),
                    )?
                }
            };
        let mut mutations = Vec::new();
        if !working.expiration_complete {
            mutations =
                self.expire_outside_window(binding, &mut working, Arc::clone(&graph_cancellation))?;
            working.expiration_complete = true;
            if !mutations.is_empty() {
                return self.publish_batch(
                    repository_root,
                    binding,
                    now_epoch_secs,
                    working,
                    target,
                    mutations,
                    0,
                    cancellation,
                    graph_cancellation,
                );
            }
        }
        let mut commits_examined = 0usize;
        let mut queue_items_examined = 0usize;
        let mut batch_seen = BTreeSet::new();
        let (existing_commits, existing_churn) =
            self.projection_index(binding, Arc::clone(&graph_cancellation))?;
        let mut churn_updates = BTreeMap::<String, usize>::new();
        while queue_items_examined < commit_batch_limit && !working.complete {
            cancellation_checkpoint(cancellation)?;
            let Some(oid) = working.pending.pop_front() else {
                break;
            };
            queue_items_examined =
                checked_add(queue_items_examined, 1, "examined frontier item count")?;
            if !batch_seen.insert(oid.clone()) {
                continue;
            }
            if working.stop_at.as_ref() == Some(&oid) || existing_commits.contains(&oid) {
                continue;
            }
            if working.history_commits_traversed >= history_commit_limit {
                working.mark_partial(GitHealthProjectionPartialReasonV1::HistoryTraversalLimit);
                break;
            }
            working.history_commits_traversed = checked_add(
                working.history_commits_traversed,
                1,
                "history traversal count",
            )?;
            commits_examined = checked_add(commits_examined, 1, "examined commit count")?;
            let record = match collect_commit_record(&repository, &oid, cancellation) {
                Ok(record) => record,
                Err(CollectCommitError::PathLimit) => {
                    working.mark_partial(GitHealthProjectionPartialReasonV1::CommitPathLimit);
                    break;
                }
                Err(CollectCommitError::Partial(reason)) => {
                    working.mark_partial(reason);
                    break;
                }
                Err(CollectCommitError::Projection(error)) => return Err(error),
            };
            if record.committed_at_epoch_secs >= working.target.window_end_epoch_secs {
                working.admit_parents(&record.parents);
                continue;
            }
            if record.committed_at_epoch_secs < working.target.window_start_epoch_secs {
                working.admit_parents(&record.parents);
                continue;
            }
            if let Some(reason) =
                self.admission_failure(&working, &record, &churn_updates, &existing_churn)?
            {
                working.mark_partial(reason);
                break;
            }
            for file in &record.changed_files {
                let previous = match churn_updates.get(file).copied() {
                    Some(previous) => previous,
                    None => existing_churn.get(file).copied().unwrap_or(0),
                };
                churn_updates.insert(file.clone(), checked_add(previous, 1, "file churn")?);
            }
            working.counters.commits_projected = checked_add(
                working.counters.commits_projected,
                1,
                "projected commit count",
            )?;
            working.counters.changed_path_references = checked_add(
                working.counters.changed_path_references,
                record.changed_files.len(),
                "changed path count",
            )?;
            working.counters.path_bytes = checked_add(
                working.counters.path_bytes,
                record.changed_files.iter().map(String::len).sum(),
                "path byte count",
            )?;
            for file in &record.changed_files {
                if !existing_churn.contains_key(file) && churn_updates.get(file) == Some(&1) {
                    working.counters.unique_paths =
                        checked_add(working.counters.unique_paths, 1, "unique path count")?;
                }
            }
            working.admit_parents(&record.parents);
            mutations.push(GraphMutation::UpsertEntity(commit_entity(&record)?));
        }
        for (path, churn) in churn_updates {
            mutations.push(GraphMutation::UpsertEntity(file_entity(&path, churn)?));
        }
        self.publish_batch(
            repository_root,
            binding,
            now_epoch_secs,
            working,
            target,
            mutations,
            commits_examined,
            cancellation,
            graph_cancellation,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn publish_batch(
        &self,
        repository_root: &Path,
        binding: &GitHealthProjectionBindingV1,
        now_epoch_secs: i64,
        mut working: WorkingStateV1,
        target: GitHealthProjectionSourceV1,
        mut mutations: Vec<GraphMutation>,
        commits_examined: usize,
        cancellation: &CancellationToken,
        graph_cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<GitHealthProjectionProgressV1, GitHealthProjectionError> {
        working.counters.batches_completed = checked_add_u64(
            working.counters.batches_completed,
            1,
            "completed batch count",
        )?;
        if working.pending.is_empty() {
            working.complete = true;
        }
        mutations.push(GraphMutation::UpsertEntity(state_entity(
            WORKING_ENTITY,
            &working,
        )?));
        if working.complete {
            mutations.push(GraphMutation::UpsertEntity(state_entity(
                READY_ENTITY,
                &ReadyStateV1 {
                    source: working.target.clone(),
                    counters: working.counters.clone(),
                },
            )?));
        }
        cancellation_checkpoint(cancellation)?;
        require_current_target(repository_root, binding, now_epoch_secs, &target)?;
        self.database.apply(GraphWriteBatch::new(
            namespace(binding)?,
            projection()?,
            SourceGeneration::new(target.projection_generation.as_str())?,
            GraphWatermark::new(format!(
                "{}:{}",
                target.projection_generation.as_str(),
                working.counters.batches_completed
            ))?,
            coalesce_mutations(mutations),
            graph_cancellation,
        )?)?;
        Ok(GitHealthProjectionProgressV1 {
            target,
            commits_examined,
            complete: working.complete,
        })
    }

    fn initialize_target(
        &self,
        binding: &GitHealthProjectionBindingV1,
        repository: &gix::Repository,
        ready: Option<&ReadyStateV1>,
        target: GitHealthProjectionSourceV1,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<WorkingStateV1, GitHealthProjectionError> {
        let reusable_candidate = ready
            .filter(|ready| ready.source.binding == target.binding)
            .filter(|ready| ready.counters.coverage == GitHealthProjectionCoverageV1::Complete)
            .filter(|ready| {
                target.window_start_epoch_secs >= ready.source.window_start_epoch_secs
                    && target.window_end_epoch_secs >= ready.source.window_end_epoch_secs
            });
        let reusable = match reusable_candidate {
            Some(ready)
                if is_ancestor_bounded(
                    repository,
                    &ready.source.commit,
                    &target.commit,
                    MAX_ANCESTRY_COMMITS,
                    || cancellation.is_cancelled(),
                )? == AncestorCheckV1::Ancestor =>
            {
                Some(ready)
            }
            _ => None,
        };
        let working = reusable.map_or_else(
            || WorkingStateV1::empty(target.clone()),
            |ready| WorkingStateV1::from_ready(target.clone(), ready),
        );
        if reusable.is_none() {
            self.database.replace_projection(ProjectionReplacement {
                namespace: namespace(binding)?,
                projection: projection()?,
                source_generation: SourceGeneration::new(target.projection_generation.as_str())?,
                next_watermark: GraphWatermark::new(format!(
                    "{}:initialize",
                    target.projection_generation.as_str()
                ))?,
                entities: vec![state_entity(WORKING_ENTITY, &working)?],
                relations: Vec::new(),
                cancellation,
            })?;
        }
        Ok(working)
    }

    fn expire_outside_window(
        &self,
        binding: &GitHealthProjectionBindingV1,
        working: &mut WorkingStateV1,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<Vec<GraphMutation>, GitHealthProjectionError> {
        let entities = self.projection_entities(binding, Arc::clone(&cancellation))?;
        let mut stored_churn = BTreeMap::new();
        let mut expired = Vec::new();
        for entity in entities {
            if entity
                .labels
                .contains(&tracedecay_graph_db::GraphLabel::new(COMMIT_LABEL)?)
            {
                let record = commit_record_from_entity(&entity, None)?;
                if record.committed_at_epoch_secs < working.target.window_start_epoch_secs
                    || record.committed_at_epoch_secs >= working.target.window_end_epoch_secs
                {
                    expired.push(record);
                }
            } else if entity
                .labels
                .contains(&tracedecay_graph_db::GraphLabel::new(FILE_LABEL)?)
            {
                let (path, churn) = persistence::file_record_from_entity(&entity)?;
                stored_churn.insert(path, churn);
            }
        }
        let mut decrements = BTreeMap::<String, usize>::new();
        let mut mutations = Vec::new();
        for record in expired {
            working.counters.commits_projected = checked_sub(
                working.counters.commits_projected,
                1,
                "expired commit count",
            )?;
            working.counters.changed_path_references = checked_sub(
                working.counters.changed_path_references,
                record.changed_files.len(),
                "expired changed path count",
            )?;
            working.counters.path_bytes = checked_sub(
                working.counters.path_bytes,
                record.changed_files.iter().map(String::len).sum(),
                "expired path byte count",
            )?;
            for file in record.changed_files {
                let prior = decrements.get(&file).copied().unwrap_or(0);
                decrements.insert(file, checked_add(prior, 1, "expiry decrement")?);
            }
            mutations.push(GraphMutation::DeleteEntity(commit_entity_id(&record.oid)?));
        }
        for (path, decrement) in decrements {
            let prior = stored_churn.get(&path).copied().ok_or_else(|| {
                GitHealthProjectionError::Corrupt(format!(
                    "expired commit references missing churn path `{path}`"
                ))
            })?;
            let remaining = checked_sub(prior, decrement, "expired file churn")?;
            if remaining == 0 {
                working.counters.unique_paths = checked_sub(
                    working.counters.unique_paths,
                    1,
                    "expired unique path count",
                )?;
                mutations.push(GraphMutation::DeleteEntity(file_entity_id(&path)?));
            } else {
                mutations.push(GraphMutation::UpsertEntity(file_entity(&path, remaining)?));
            }
        }
        Ok(mutations)
    }

    fn admission_failure(
        &self,
        working: &WorkingStateV1,
        record: &CommitRecordV1,
        pending_churn: &BTreeMap<String, usize>,
        existing_churn: &BTreeMap<String, usize>,
    ) -> Result<Option<GitHealthProjectionPartialReasonV1>, GitHealthProjectionError> {
        if working.counters.commits_projected >= MAX_WINDOW_COMMITS {
            return Ok(Some(GitHealthProjectionPartialReasonV1::CommitLimit));
        }
        if working
            .counters
            .changed_path_references
            .saturating_add(record.changed_files.len())
            > MAX_CHANGED_PATH_REFERENCES
        {
            return Ok(Some(GitHealthProjectionPartialReasonV1::ChangedPathLimit));
        }
        if working
            .counters
            .path_bytes
            .saturating_add(record.changed_files.iter().map(String::len).sum::<usize>())
            > MAX_PATH_BYTES
        {
            return Ok(Some(GitHealthProjectionPartialReasonV1::PathBytesLimit));
        }
        let mut new_paths = 0usize;
        for file in &record.changed_files {
            if !pending_churn.contains_key(file) && !existing_churn.contains_key(file) {
                new_paths = checked_add(new_paths, 1, "new path count")?;
            }
        }
        if working.counters.unique_paths.saturating_add(new_paths) > MAX_UNIQUE_PATHS {
            return Ok(Some(GitHealthProjectionPartialReasonV1::UniquePathLimit));
        }
        Ok(None)
    }

    fn projection_index(
        &self,
        binding: &GitHealthProjectionBindingV1,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<(BTreeSet<GitOidV1>, BTreeMap<String, usize>), GitHealthProjectionError> {
        let mut commits = BTreeSet::new();
        let mut churn = BTreeMap::new();
        for entity in self.projection_entities(binding, cancellation)? {
            if entity
                .labels
                .contains(&tracedecay_graph_db::GraphLabel::new(COMMIT_LABEL)?)
            {
                commits.insert(commit_record_from_entity(&entity, None)?.oid);
            } else if entity
                .labels
                .contains(&tracedecay_graph_db::GraphLabel::new(FILE_LABEL)?)
            {
                let (path, count) = persistence::file_record_from_entity(&entity)?;
                churn.insert(path, count);
            }
        }
        Ok((commits, churn))
    }
}

fn checked_add(left: usize, right: usize, field: &str) -> Result<usize, GitHealthProjectionError> {
    left.checked_add(right)
        .ok_or_else(|| GitHealthProjectionError::Corrupt(format!("Git health {field} overflowed")))
}

fn checked_sub(left: usize, right: usize, field: &str) -> Result<usize, GitHealthProjectionError> {
    left.checked_sub(right)
        .ok_or_else(|| GitHealthProjectionError::Corrupt(format!("Git health {field} underflowed")))
}

fn checked_add_u64(left: u64, right: u64, field: &str) -> Result<u64, GitHealthProjectionError> {
    left.checked_add(right)
        .ok_or_else(|| GitHealthProjectionError::Corrupt(format!("Git health {field} overflowed")))
}

fn cancellation_checkpoint(
    cancellation: &CancellationToken,
) -> Result<(), GitHealthProjectionError> {
    if cancellation.is_cancelled() {
        Err(GitHealthProjectionError::Cancelled)
    } else {
        Ok(())
    }
}

#[cfg(test)]
#[path = "projection_tests.rs"]
mod tests;
