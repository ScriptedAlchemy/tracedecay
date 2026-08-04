//! Incremental, bounded Git-history projection for health reads.
//!
//! Native Git is authoritative. Grafeo stores one bounded worktree projection
//! inside the daemon-owned project graph. Durable state contains only a fixed
//! frontier and scalar counters; commit and path churn records are incremental
//! entities, so a batch never serializes the whole growing projection.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::Path;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_application::{
    GitHealthProjectionAvailabilityV1, GitHealthProjectionCoverageV1,
    GitHealthProjectionPartialReasonV1, GitHealthProjectionSnapshotV1, GitHealthProjectionSourceV1,
    GitHealthProjectionUnavailableReasonV1, ResolvedScope,
};
use tracedecay_domain::{GitOidV1, canonical_sha256};
use tracedecay_graph_db::{
    GraphCancellation, GraphDb, GraphDbError, GraphDbLocation, GraphDbOpenOptions, GraphDurability,
    GraphEntity, GraphEntityId, GraphFormatVersion, GraphLabel, GraphMutation, GraphNamespace,
    GraphProjectionId, GraphProjectionReadRequest, GraphProperty, GraphPropertyName,
    GraphWatermark, GraphWriteBatch, ProjectionReplacement, SourceGeneration,
};

use crate::application::context::CancellationToken;

mod native;

pub(crate) use native::capture_source;
use native::{CollectCommitError, collect_commit_record, is_ancestor, require_current_target};

const HISTORY_WINDOW_SECS: i64 = 90 * 24 * 60 * 60;
const WINDOW_BUCKET_SECS: i64 = 24 * 60 * 60;
const MAX_CHANGED_FILES_PER_COMMIT: usize = 20_000;
const MAX_COMMIT_RECORD_PATH_BYTES: usize = 768 * 1024;
const MAX_WINDOW_COMMITS: usize = 20_000;
const MAX_UNIQUE_PATHS: usize = 20_000;
const MAX_CHANGED_PATH_REFERENCES: usize = 50_000;
const MAX_PATH_BYTES: usize = 8 * 1024 * 1024;
const MAX_DURABLE_FRONTIER: usize = 512;
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
    pending: VecDeque<GitOidV1>,
    counters: ProjectionCountersV1,
    expiration_complete: bool,
    complete: bool,
}

impl WorkingStateV1 {
    fn empty(target: GitHealthProjectionSourceV1) -> Self {
        Self {
            pending: VecDeque::from([target.commit.clone()]),
            target,
            counters: ProjectionCountersV1::default(),
            expiration_complete: true,
            complete: false,
        }
    }

    fn from_ready(target: GitHealthProjectionSourceV1, ready: &ReadyStateV1) -> Self {
        let mut pending = VecDeque::new();
        if target.commit != ready.source.commit {
            pending.push_back(target.commit.clone());
        }
        let expiration_complete =
            target.window_start_epoch_secs == ready.source.window_start_epoch_secs;
        Self {
            target,
            pending,
            counters: ready.counters.clone(),
            expiration_complete,
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

    pub(crate) fn from_database(database: GraphDb) -> Self {
        Self { database }
    }

    pub(crate) fn database(&self) -> GraphDb {
        self.database.clone()
    }

    pub(crate) fn capture_source(
        repository_root: &Path,
        scope: &ResolvedScope,
        now_epoch_secs: i64,
    ) -> Result<GitHealthProjectionSourceV1, GitHealthProjectionError> {
        capture_source(repository_root, scope, now_epoch_secs)
    }

    pub(crate) fn read(&self, scope: &ResolvedScope) -> GitHealthProjectionAvailabilityV1 {
        match self.read_inner(scope) {
            Ok(availability) => availability,
            Err(error) => GitHealthProjectionAvailabilityV1::Unavailable {
                reason: error.unavailable_reason(),
            },
        }
    }

    fn read_inner(
        &self,
        scope: &ResolvedScope,
    ) -> Result<GitHealthProjectionAvailabilityV1, GitHealthProjectionError> {
        let cancellation: Arc<dyn GraphCancellation> =
            Arc::new(TokenCancellation(CancellationToken::new()));
        let ready =
            self.read_state::<ReadyStateV1>(scope, READY_ENTITY, Arc::clone(&cancellation))?;
        let working =
            self.read_state::<WorkingStateV1>(scope, WORKING_ENTITY, Arc::clone(&cancellation))?;
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
        Ok(GitHealthProjectionAvailabilityV1::Ready {
            snapshot: self.snapshot(scope, ready, cancellation)?,
        })
    }

    fn snapshot(
        &self,
        scope: &ResolvedScope,
        ready: ReadyStateV1,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<GitHealthProjectionSnapshotV1, GitHealthProjectionError> {
        let entities = self.projection_entities(scope, cancellation)?;
        let mut file_churn = BTreeMap::new();
        for entity in entities {
            if !entity.labels.contains(&GraphLabel::new(FILE_LABEL)?) {
                continue;
            }
            let path = string_property(&entity, FILE_PATH_PROPERTY)?;
            let churn = usize_property(&entity, FILE_CHURN_PROPERTY)?;
            if file_churn.insert(path.to_owned(), churn).is_some() {
                return Err(GitHealthProjectionError::Corrupt(
                    "Git health projection contains duplicate file paths".to_owned(),
                ));
            }
        }
        if file_churn.len() != ready.counters.unique_paths {
            return Err(GitHealthProjectionError::Corrupt(
                "Git health projection path count disagrees with ready state".to_owned(),
            ));
        }
        Ok(GitHealthProjectionSnapshotV1 {
            source: ready.source,
            commits_projected: ready.counters.commits_projected,
            batches_completed: ready.counters.batches_completed,
            file_churn,
            coverage: ready.counters.coverage,
        })
    }

    pub(crate) fn advance(
        &self,
        repository_root: &Path,
        scope: &ResolvedScope,
        now_epoch_secs: i64,
        commit_batch_limit: usize,
        cancellation: &CancellationToken,
    ) -> Result<GitHealthProjectionProgressV1, GitHealthProjectionError> {
        if commit_batch_limit == 0 {
            return Err(GitHealthProjectionError::InvalidBatchLimit);
        }
        cancellation_checkpoint(cancellation)?;
        let target = capture_source(repository_root, scope, now_epoch_secs)?;
        let graph_cancellation: Arc<dyn GraphCancellation> =
            Arc::new(TokenCancellation(cancellation.clone()));
        let ready =
            self.read_state::<ReadyStateV1>(scope, READY_ENTITY, Arc::clone(&graph_cancellation))?;
        let persisted_working = self.read_state::<WorkingStateV1>(
            scope,
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
                    require_current_target(repository_root, scope, now_epoch_secs, &target)?;
                    self.initialize_target(
                        scope,
                        &repository,
                        ready.as_ref(),
                        target.clone(),
                        Arc::clone(&graph_cancellation),
                    )?
                }
            };
        let mut mutations = if working.expiration_complete {
            Vec::new()
        } else {
            let mutations =
                self.expire_outside_window(scope, &mut working, Arc::clone(&graph_cancellation))?;
            working.expiration_complete = true;
            mutations
        };
        if !mutations.is_empty() {
            working.counters.batches_completed = checked_add_u64(
                working.counters.batches_completed,
                1,
                "completed expiry batch count",
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
            require_current_target(repository_root, scope, now_epoch_secs, &target)?;
            self.database.apply(GraphWriteBatch::new(
                namespace(scope)?,
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
            return Ok(GitHealthProjectionProgressV1 {
                target,
                commits_examined: 0,
                complete: working.complete,
            });
        }
        let mut commits_examined = 0usize;
        let mut queue_items_examined = 0usize;
        let mut batch_seen = BTreeSet::new();
        let mut churn_updates = BTreeMap::<String, usize>::new();

        while queue_items_examined < commit_batch_limit && !working.complete {
            cancellation_checkpoint(cancellation)?;
            let Some(oid) = working.pending.pop_front() else {
                break;
            };
            queue_items_examined = checked_add(queue_items_examined, 1, "queue count")?;
            if !batch_seen.insert(oid.clone()) {
                continue;
            }
            if self
                .commit_record(scope, &oid, Arc::clone(&graph_cancellation))?
                .is_some()
            {
                continue;
            }
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
            if record.committed_at_epoch_secs < working.target.window_start_epoch_secs {
                continue;
            }
            if let Some(reason) = self.admission_failure(
                scope,
                &working,
                &record,
                &churn_updates,
                Arc::clone(&graph_cancellation),
            )? {
                working.mark_partial(reason);
                break;
            }
            for file in &record.changed_files {
                let previous = match churn_updates.get(file).copied() {
                    Some(previous) => previous,
                    None => self
                        .file_churn(scope, file, Arc::clone(&graph_cancellation))?
                        .unwrap_or(0),
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
                if self
                    .file_churn(scope, file, Arc::clone(&graph_cancellation))?
                    .is_none()
                    && churn_updates.get(file) == Some(&1)
                {
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
        require_current_target(repository_root, scope, now_epoch_secs, &target)?;
        self.database.apply(GraphWriteBatch::new(
            namespace(scope)?,
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
        scope: &ResolvedScope,
        repository: &gix::Repository,
        ready: Option<&ReadyStateV1>,
        target: GitHealthProjectionSourceV1,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<WorkingStateV1, GitHealthProjectionError> {
        let reusable = ready
            .filter(|ready| ready.source.scope == target.scope)
            .filter(|ready| ready.counters.coverage == GitHealthProjectionCoverageV1::Complete)
            .filter(|ready| is_ancestor(repository, &ready.source.commit, &target.commit));
        let working = reusable.map_or_else(
            || WorkingStateV1::empty(target.clone()),
            |ready| WorkingStateV1::from_ready(target.clone(), ready),
        );
        if reusable.is_none() {
            self.database.replace_projection(ProjectionReplacement {
                namespace: namespace(scope)?,
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
        scope: &ResolvedScope,
        working: &mut WorkingStateV1,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<Vec<GraphMutation>, GitHealthProjectionError> {
        let entities = self.projection_entities(scope, Arc::clone(&cancellation))?;
        let mut expired = Vec::new();
        for entity in entities {
            if !entity.labels.contains(&GraphLabel::new(COMMIT_LABEL)?) {
                continue;
            }
            let record = commit_record_from_entity(&entity)?;
            if record.committed_at_epoch_secs < working.target.window_start_epoch_secs {
                expired.push(record);
            }
        }
        if expired.is_empty() {
            return Ok(Vec::new());
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
                *decrements.entry(file).or_default() =
                    checked_add(*decrements.get(&file).unwrap_or(&0), 1, "expiry decrement")?;
            }
            mutations.push(GraphMutation::DeleteEntity(commit_entity_id(&record.oid)?));
        }
        for (path, decrement) in decrements {
            let prior = self
                .file_churn(scope, &path, Arc::clone(&cancellation))?
                .ok_or_else(|| {
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
        scope: &ResolvedScope,
        working: &WorkingStateV1,
        record: &CommitRecordV1,
        pending_churn: &BTreeMap<String, usize>,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<Option<GitHealthProjectionPartialReasonV1>, GitHealthProjectionError> {
        if working.counters.commits_projected >= MAX_WINDOW_COMMITS {
            return Ok(Some(GitHealthProjectionPartialReasonV1::CommitLimit));
        }
        let next_references = working
            .counters
            .changed_path_references
            .saturating_add(record.changed_files.len());
        if next_references > MAX_CHANGED_PATH_REFERENCES {
            return Ok(Some(GitHealthProjectionPartialReasonV1::ChangedPathLimit));
        }
        let next_bytes = working
            .counters
            .path_bytes
            .saturating_add(record.changed_files.iter().map(String::len).sum::<usize>());
        if next_bytes > MAX_PATH_BYTES {
            return Ok(Some(GitHealthProjectionPartialReasonV1::PathBytesLimit));
        }
        let mut new_paths = 0usize;
        for file in &record.changed_files {
            if !pending_churn.contains_key(file)
                && self
                    .file_churn(scope, file, Arc::clone(&cancellation))?
                    .is_none()
            {
                new_paths = checked_add(new_paths, 1, "new path count")?;
            }
        }
        if working.counters.unique_paths.saturating_add(new_paths) > MAX_UNIQUE_PATHS {
            return Ok(Some(GitHealthProjectionPartialReasonV1::UniquePathLimit));
        }
        Ok(None)
    }

    fn projection_entities(
        &self,
        scope: &ResolvedScope,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<Vec<GraphEntity>, GitHealthProjectionError> {
        let mut entities = Vec::new();
        let mut after_entity = None;
        loop {
            let page = self.database.read_projection(GraphProjectionReadRequest {
                namespace: namespace(scope)?,
                projection: projection()?,
                after_entity,
                after_relation: None,
                max_entities: PROJECTION_PAGE_SIZE,
                max_relations: 0,
                cancellation: Arc::clone(&cancellation),
            })?;
            entities.extend(page.entities);
            if entities.len() > MAX_PROJECTION_ENTITIES {
                return Err(GitHealthProjectionError::Corrupt(
                    "Git health projection exceeds its entity bound".to_owned(),
                ));
            }
            let Some(next) = page.next_entity else {
                break;
            };
            after_entity = Some(next);
        }
        Ok(entities)
    }

    fn read_state<T: for<'de> Deserialize<'de>>(
        &self,
        scope: &ResolvedScope,
        identity: &str,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<Option<T>, GitHealthProjectionError> {
        let Some(entity) = self.database.entity(
            &namespace(scope)?,
            &GraphEntityId::new(identity)?,
            cancellation,
        )?
        else {
            return Ok(None);
        };
        let payload = bytes_property(&entity, STATE_PROPERTY)?;
        serde_json::from_slice(payload)
            .map(Some)
            .map_err(|error| GitHealthProjectionError::Corrupt(error.to_string()))
    }

    fn commit_record(
        &self,
        scope: &ResolvedScope,
        oid: &GitOidV1,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<Option<CommitRecordV1>, GitHealthProjectionError> {
        let Some(entity) =
            self.database
                .entity(&namespace(scope)?, &commit_entity_id(oid)?, cancellation)?
        else {
            return Ok(None);
        };
        commit_record_from_entity(&entity).map(Some)
    }

    fn file_churn(
        &self,
        scope: &ResolvedScope,
        path: &str,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<Option<usize>, GitHealthProjectionError> {
        let Some(entity) =
            self.database
                .entity(&namespace(scope)?, &file_entity_id(path)?, cancellation)?
        else {
            return Ok(None);
        };
        if string_property(&entity, FILE_PATH_PROPERTY)? != path {
            return Err(GitHealthProjectionError::Corrupt(
                "Git health path digest collision".to_owned(),
            ));
        }
        usize_property(&entity, FILE_CHURN_PROPERTY).map(Some)
    }
}

fn state_entity<T: Serialize>(
    identity: &str,
    state: &T,
) -> Result<GraphEntity, GitHealthProjectionError> {
    GraphEntity::new(
        GraphEntityId::new(identity)?,
        BTreeSet::from([GraphLabel::new(STATE_LABEL)?]),
        BTreeMap::from([(
            GraphPropertyName::new(STATE_PROPERTY)?,
            GraphProperty::Bytes(
                serde_json::to_vec(state)
                    .map_err(|error| GitHealthProjectionError::Corrupt(error.to_string()))?,
            ),
        )]),
    )
    .map_err(Into::into)
}

fn commit_entity(record: &CommitRecordV1) -> Result<GraphEntity, GitHealthProjectionError> {
    GraphEntity::new(
        commit_entity_id(&record.oid)?,
        BTreeSet::from([GraphLabel::new(COMMIT_LABEL)?]),
        BTreeMap::from([(
            GraphPropertyName::new(COMMIT_PROPERTY)?,
            GraphProperty::Bytes(
                serde_json::to_vec(record)
                    .map_err(|error| GitHealthProjectionError::Corrupt(error.to_string()))?,
            ),
        )]),
    )
    .map_err(Into::into)
}

fn file_entity(path: &str, churn: usize) -> Result<GraphEntity, GitHealthProjectionError> {
    GraphEntity::new(
        file_entity_id(path)?,
        BTreeSet::from([GraphLabel::new(FILE_LABEL)?]),
        BTreeMap::from([
            (
                GraphPropertyName::new(FILE_PATH_PROPERTY)?,
                GraphProperty::String(path.to_owned()),
            ),
            (
                GraphPropertyName::new(FILE_CHURN_PROPERTY)?,
                GraphProperty::I64(i64::try_from(churn).map_err(|_| {
                    GitHealthProjectionError::Corrupt(
                        "Git health churn exceeds the persisted range".to_owned(),
                    )
                })?),
            ),
        ]),
    )
    .map_err(Into::into)
}

fn commit_record_from_entity(
    entity: &GraphEntity,
) -> Result<CommitRecordV1, GitHealthProjectionError> {
    serde_json::from_slice(bytes_property(entity, COMMIT_PROPERTY)?)
        .map_err(|error| GitHealthProjectionError::Corrupt(error.to_string()))
}

fn bytes_property<'a>(
    entity: &'a GraphEntity,
    name: &str,
) -> Result<&'a [u8], GitHealthProjectionError> {
    entity
        .properties
        .get(&GraphPropertyName::new(name)?)
        .and_then(|property| match property {
            GraphProperty::Bytes(bytes) => Some(bytes.as_slice()),
            _ => None,
        })
        .ok_or_else(|| {
            GitHealthProjectionError::Corrupt(format!(
                "Git health entity `{}` has no `{name}` byte property",
                entity.identity
            ))
        })
}

fn string_property<'a>(
    entity: &'a GraphEntity,
    name: &str,
) -> Result<&'a str, GitHealthProjectionError> {
    entity
        .properties
        .get(&GraphPropertyName::new(name)?)
        .and_then(|property| match property {
            GraphProperty::String(value) => Some(value.as_str()),
            _ => None,
        })
        .ok_or_else(|| {
            GitHealthProjectionError::Corrupt(format!(
                "Git health entity `{}` has no `{name}` string property",
                entity.identity
            ))
        })
}

fn usize_property(entity: &GraphEntity, name: &str) -> Result<usize, GitHealthProjectionError> {
    entity
        .properties
        .get(&GraphPropertyName::new(name)?)
        .and_then(|property| match property {
            GraphProperty::I64(value) => usize::try_from(*value).ok(),
            _ => None,
        })
        .ok_or_else(|| {
            GitHealthProjectionError::Corrupt(format!(
                "Git health entity `{}` has no non-negative `{name}` property",
                entity.identity
            ))
        })
}

fn commit_entity_id(oid: &GitOidV1) -> Result<GraphEntityId, GitHealthProjectionError> {
    GraphEntityId::new(format!("git-health-commit:{}", oid.as_str())).map_err(Into::into)
}

fn file_entity_id(path: &str) -> Result<GraphEntityId, GitHealthProjectionError> {
    GraphEntityId::new(format!("git-health-file:{}", file_digest(path))).map_err(Into::into)
}

fn file_digest(path: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(path.as_bytes()))
}

fn namespace(scope: &ResolvedScope) -> Result<GraphNamespace, GitHealthProjectionError> {
    let digest = canonical_sha256(&(
        NAMESPACE_DOMAIN,
        &scope.project_id,
        &scope.repository_id,
        &scope.worktree_id,
    ))
    .map_err(|error| GitHealthProjectionError::Corrupt(error.to_string()))?;
    GraphNamespace::new(format!(
        "git-health-{}",
        digest
            .as_str()
            .strip_prefix("sha256:")
            .unwrap_or(digest.as_str())
    ))
    .map_err(Into::into)
}

fn projection() -> Result<GraphProjectionId, GitHealthProjectionError> {
    GraphProjectionId::new(PROJECTION).map_err(Into::into)
}

fn coalesce_mutations(mutations: Vec<GraphMutation>) -> Vec<GraphMutation> {
    let mut unique = BTreeMap::<(u8, String), GraphMutation>::new();
    for mutation in mutations {
        let key = match &mutation {
            GraphMutation::DeleteRelation(identity) => (0, identity.as_str().to_owned()),
            GraphMutation::DeleteEntity(identity) => (1, identity.as_str().to_owned()),
            GraphMutation::UpsertEntity(entity) => (2, entity.identity.as_str().to_owned()),
            GraphMutation::UpsertRelation(relation) => (3, relation.identity.as_str().to_owned()),
        };
        unique.insert(key, mutation);
    }
    unique.into_values().collect()
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
