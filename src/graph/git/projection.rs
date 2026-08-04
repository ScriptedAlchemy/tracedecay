//! Incremental Git-history projection for health and test-risk reads.
//!
//! Native Git objects remain authoritative. Each batch is cancellable and
//! bounded by commit count. A complete prior generation remains readable while
//! a newer ref/tree generation is assembled in the embedded graph projection.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::Path;
use std::sync::Arc;

use gix::bstr::ByteSlice;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_application::{
    GitHealthProjectionAvailabilityV1, GitHealthProjectionSnapshotV1, GitHealthProjectionSourceV1,
    GitHealthProjectionUnavailableReasonV1, ResolvedScope,
};
use tracedecay_domain::{GitOidV1, ManifestDigest, canonical_sha256};
use tracedecay_graph_db::{
    GraphCancellation, GraphDb, GraphDbError, GraphDbLocation, GraphDbOpenOptions, GraphDurability,
    GraphEntity, GraphEntityId, GraphFormatVersion, GraphLabel, GraphMutation, GraphNamespace,
    GraphProjectionId, GraphProperty, GraphPropertyName, GraphRelation, GraphRelationId,
    GraphRelationKind, GraphWatermark, GraphWriteBatch, SourceGeneration,
};

use crate::application::context::CancellationToken;

const HISTORY_WINDOW_SECS: i64 = 90 * 24 * 60 * 60;
const WINDOW_BUCKET_SECS: i64 = 24 * 60 * 60;
const MAX_CHANGED_FILES_PER_COMMIT: usize = 20_000;
const GRAPH_FORMAT_VERSION: u32 = 3;
const PROJECTION: &str = "git-health-topology";
const READY_ENTITY: &str = "git-health-ready";
const WORKING_ENTITY: &str = "git-health-working";
const STATE_PROPERTY: &str = "state";
const COMMIT_PROPERTY: &str = "commit";
const FILE_PATH_PROPERTY: &str = "path";
const COMMIT_LABEL: &str = "GitCommit";
const FILE_LABEL: &str = "GitFile";
const STATE_LABEL: &str = "GitHealthProjectionState";
const PARENT_RELATION: &str = "GitParent";
const TOUCHED_RELATION: &str = "GitTouchedFile";
const GENERATION_DOMAIN: &str = "tracedecay.git-health.projection-generation.v1";

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
    #[error("Git commit exceeds the health projection file-change bound")]
    NativeReadBoundExceeded,
}

impl GitHealthProjectionError {
    pub(crate) const fn unavailable_reason(&self) -> GitHealthProjectionUnavailableReasonV1 {
        match self {
            Self::ScopeDrift => GitHealthProjectionUnavailableReasonV1::ScopeDrift,
            Self::Git(_) | Self::NativeReadBoundExceeded => {
                GitHealthProjectionUnavailableReasonV1::NativeGitUnavailable
            }
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
struct ReadyStateV1 {
    snapshot: GitHealthProjectionSnapshotV1,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct WorkingStateV1 {
    target: GitHealthProjectionSourceV1,
    pending: VecDeque<GitOidV1>,
    file_churn: BTreeMap<String, usize>,
    commits_projected: usize,
    batches_completed: u64,
    complete: bool,
}

impl WorkingStateV1 {
    fn new(target: GitHealthProjectionSourceV1) -> Self {
        Self {
            pending: VecDeque::from([target.commit.clone()]),
            target,
            file_churn: BTreeMap::new(),
            commits_projected: 0,
            batches_completed: 0,
            complete: false,
        }
    }

    fn snapshot(&self) -> GitHealthProjectionSnapshotV1 {
        GitHealthProjectionSnapshotV1 {
            source: self.target.clone(),
            commits_projected: self.commits_projected,
            batches_completed: self.batches_completed,
            file_churn: self.file_churn.clone(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct CommitRecordV1 {
    oid: GitOidV1,
    tree: Option<GitOidV1>,
    committed_at_epoch_secs: Option<i64>,
    parents: Vec<GitOidV1>,
    changed_files: Vec<String>,
    visited_generation: Option<ManifestDigest>,
    complete: bool,
}

#[derive(Clone)]
struct TokenCancellation(CancellationToken);

impl GraphCancellation for TokenCancellation {
    fn is_cancelled(&self) -> bool {
        self.0.is_cancelled()
    }
}

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
                    "could not create projection directory: {error}"
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

    pub(crate) fn read(&self, scope: &ResolvedScope) -> GitHealthProjectionAvailabilityV1 {
        let cancellation: Arc<dyn GraphCancellation> =
            Arc::new(TokenCancellation(CancellationToken::new()));
        let ready = self.read_state::<ReadyStateV1>(scope, READY_ENTITY, Arc::clone(&cancellation));
        let working =
            self.read_state::<WorkingStateV1>(scope, WORKING_ENTITY, Arc::clone(&cancellation));
        match (ready, working) {
            (Err(error), _) | (_, Err(error)) => GitHealthProjectionAvailabilityV1::Unavailable {
                reason: error.unavailable_reason(),
            },
            (Ok(Some(ready)), Ok(Some(working)))
                if !working.complete && working.target != ready.snapshot.source =>
            {
                GitHealthProjectionAvailabilityV1::Refreshing {
                    snapshot: ready.snapshot,
                    target: working.target,
                }
            }
            (Ok(Some(ready)), _) => GitHealthProjectionAvailabilityV1::Ready {
                snapshot: ready.snapshot,
            },
            (Ok(None), Ok(working)) => GitHealthProjectionAvailabilityV1::Warming {
                target: working.map(|state| state.target),
            },
        }
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
        if ready
            .as_ref()
            .is_some_and(|ready| ready.snapshot.source == target)
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
        let mut working = persisted_working
            .filter(|state| !state.complete && state.target == target)
            .unwrap_or_else(|| WorkingStateV1::new(target.clone()));
        let mut mutations = vec![GraphMutation::UpsertEntity(state_entity(
            WORKING_ENTITY,
            &working,
        )?)];
        let mut commits_examined = 0usize;
        let mut queue_items_examined = 0usize;
        let mut batch_seen = BTreeSet::new();
        let mut records = Vec::new();

        while queue_items_examined < commit_batch_limit {
            cancellation_checkpoint(cancellation)?;
            let Some(oid) = working.pending.pop_front() else {
                break;
            };
            queue_items_examined = queue_items_examined.checked_add(1).ok_or_else(|| {
                GitHealthProjectionError::Corrupt(
                    "Git health projection queue count overflowed".to_owned(),
                )
            })?;
            if !batch_seen.insert(oid.clone()) {
                continue;
            }
            let existing = self.commit_record(scope, &oid, Arc::clone(&graph_cancellation))?;
            if existing.as_ref().is_some_and(|record| {
                record.visited_generation.as_ref() == Some(&target.projection_generation)
            }) {
                continue;
            }

            commits_examined = commits_examined.checked_add(1).ok_or_else(|| {
                GitHealthProjectionError::Corrupt(
                    "Git health projection commit count overflowed".to_owned(),
                )
            })?;
            let mut record = match existing {
                Some(record) if record.complete => record,
                _ => collect_commit_record(&repository, &oid, cancellation)?,
            };
            let in_window = record
                .committed_at_epoch_secs
                .is_some_and(|committed_at| committed_at >= working.target.window_start_epoch_secs);
            if in_window {
                for file in &record.changed_files {
                    let churn = working.file_churn.entry(file.clone()).or_default();
                    *churn = churn.checked_add(1).ok_or_else(|| {
                        GitHealthProjectionError::Corrupt(format!(
                            "Git health churn count overflowed for `{file}`"
                        ))
                    })?;
                }
                working.commits_projected =
                    working.commits_projected.checked_add(1).ok_or_else(|| {
                        GitHealthProjectionError::Corrupt(
                            "Git health projected commit count overflowed".to_owned(),
                        )
                    })?;
                working.pending.extend(record.parents.iter().cloned());
            }
            record.visited_generation = Some(target.projection_generation.clone());
            records.push(record);
        }

        let batch_commit_ids: BTreeSet<_> =
            records.iter().map(|record| record.oid.clone()).collect();
        for record in &records {
            append_topology_mutations(
                self,
                scope,
                record,
                &batch_commit_ids,
                Arc::clone(&graph_cancellation),
                &mut mutations,
            )?;
        }

        working.batches_completed = working.batches_completed.checked_add(1).ok_or_else(|| {
            GitHealthProjectionError::Corrupt(
                "Git health completed batch count overflowed".to_owned(),
            )
        })?;
        working.complete = working.pending.is_empty();
        mutations[0] = GraphMutation::UpsertEntity(state_entity(WORKING_ENTITY, &working)?);
        if working.complete {
            mutations.push(GraphMutation::UpsertEntity(state_entity(
                READY_ENTITY,
                &ReadyStateV1 {
                    snapshot: working.snapshot(),
                },
            )?));
        }
        cancellation_checkpoint(cancellation)?;
        self.database.apply(GraphWriteBatch::new(
            namespace(scope)?,
            projection()?,
            SourceGeneration::new(target.projection_generation.as_str())?,
            GraphWatermark::new(format!(
                "{}:{}",
                target.projection_generation.as_str(),
                working.batches_completed
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
        let payload = entity
            .properties
            .get(&GraphPropertyName::new(STATE_PROPERTY)?)
            .and_then(|property| match property {
                GraphProperty::Bytes(bytes) => Some(bytes.as_slice()),
                _ => None,
            })
            .ok_or_else(|| {
                GitHealthProjectionError::Corrupt(format!(
                    "state entity `{identity}` has no byte payload"
                ))
            })?;
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
        let payload = entity
            .properties
            .get(&GraphPropertyName::new(COMMIT_PROPERTY)?)
            .and_then(|property| match property {
                GraphProperty::Bytes(bytes) => Some(bytes.as_slice()),
                _ => None,
            })
            .ok_or_else(|| {
                GitHealthProjectionError::Corrupt(format!(
                    "commit entity `{}` has no commit payload",
                    oid.as_str()
                ))
            })?;
        serde_json::from_slice(payload)
            .map(Some)
            .map_err(|error| GitHealthProjectionError::Corrupt(error.to_string()))
    }
}

fn capture_source(
    repository_root: &Path,
    scope: &ResolvedScope,
    now_epoch_secs: i64,
) -> Result<GitHealthProjectionSourceV1, GitHealthProjectionError> {
    scope
        .validate()
        .map_err(|error| GitHealthProjectionError::Corrupt(error.to_string()))?;
    let identity =
        crate::daemon::code_index_scheduler::identity::IndexingIdentityV1::resolve(repository_root)
            .map_err(|error| GitHealthProjectionError::Git(error.to_string()))?;
    if identity.repository_id() != &scope.repository_id
        || identity.worktree_id() != &scope.worktree_id
        || identity.head_ref() != scope.reference.as_ref()
    {
        return Err(GitHealthProjectionError::ScopeDrift);
    }
    let commit = identity
        .head_commit()
        .ok_or_else(|| GitHealthProjectionError::Git("HEAD has no commit".to_owned()))
        .and_then(|commit| {
            GitOidV1::new(commit.as_str())
                .map_err(|error| GitHealthProjectionError::Corrupt(error.to_string()))
        })?;
    let tree = identity
        .head_tree()
        .ok_or_else(|| GitHealthProjectionError::Git("HEAD commit has no readable tree".to_owned()))
        .and_then(|tree| {
            GitOidV1::new(tree.as_str())
                .map_err(|error| GitHealthProjectionError::Corrupt(error.to_string()))
        })?;
    let window_end_epoch_secs = now_epoch_secs
        .checked_sub(now_epoch_secs.rem_euclid(WINDOW_BUCKET_SECS))
        .ok_or_else(|| {
            GitHealthProjectionError::Corrupt(
                "Git health window end is outside the supported range".to_owned(),
            )
        })?;
    let window_start_epoch_secs = window_end_epoch_secs
        .checked_sub(HISTORY_WINDOW_SECS)
        .ok_or_else(|| {
            GitHealthProjectionError::Corrupt(
                "Git health window start is outside the supported range".to_owned(),
            )
        })?;
    let projection_generation = canonical_sha256(&(
        GENERATION_DOMAIN,
        scope,
        &commit,
        &tree,
        window_start_epoch_secs,
        window_end_epoch_secs,
    ))
    .map_err(|error| GitHealthProjectionError::Corrupt(error.to_string()))?;
    Ok(GitHealthProjectionSourceV1 {
        scope: scope.clone(),
        commit,
        tree,
        projection_generation,
        window_start_epoch_secs,
        window_end_epoch_secs,
    })
}

fn collect_commit_record(
    repository: &gix::Repository,
    oid: &GitOidV1,
    cancellation: &CancellationToken,
) -> Result<CommitRecordV1, GitHealthProjectionError> {
    cancellation_checkpoint(cancellation)?;
    let object_id = gix::ObjectId::from_hex(oid.as_str().as_bytes())
        .map_err(|error| GitHealthProjectionError::Git(error.to_string()))?;
    let object = repository
        .find_object(object_id)
        .map_err(|error| GitHealthProjectionError::Git(error.to_string()))?;
    let commit = object
        .try_into_commit()
        .map_err(|error| GitHealthProjectionError::Git(error.to_string()))?;
    let committed_at_epoch_secs = commit
        .time()
        .map_err(|error| GitHealthProjectionError::Git(error.to_string()))?
        .seconds;
    let tree = GitOidV1::new(
        commit
            .tree_id()
            .map_err(|error| GitHealthProjectionError::Git(error.to_string()))?
            .detach()
            .to_string(),
    )
    .map_err(|error| GitHealthProjectionError::Corrupt(error.to_string()))?;
    let parents = commit
        .parent_ids()
        .map(|parent| {
            GitOidV1::new(parent.detach().to_string())
                .map_err(|error| GitHealthProjectionError::Corrupt(error.to_string()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut changed_files = if let Some(first_parent) = commit.parent_ids().next() {
        let parent_object = repository
            .find_object(first_parent.detach())
            .map_err(|error| GitHealthProjectionError::Git(error.to_string()))?;
        let parent_commit = parent_object
            .try_into_commit()
            .map_err(|error| GitHealthProjectionError::Git(error.to_string()))?;
        changed_files_between(
            &parent_commit
                .tree()
                .map_err(|error| GitHealthProjectionError::Git(error.to_string()))?,
            &commit
                .tree()
                .map_err(|error| GitHealthProjectionError::Git(error.to_string()))?,
            cancellation,
        )?
    } else {
        let entries = commit
            .tree()
            .map_err(|error| GitHealthProjectionError::Git(error.to_string()))?
            .traverse()
            .breadthfirst
            .files()
            .map_err(|error| GitHealthProjectionError::Git(error.to_string()))?;
        if entries.len() > MAX_CHANGED_FILES_PER_COMMIT {
            return Err(GitHealthProjectionError::NativeReadBoundExceeded);
        }
        entries
            .into_iter()
            .filter(|entry| !entry.mode.is_tree())
            .map(|entry| exact_path(entry.filepath.as_bytes()))
            .collect::<Result<Vec<_>, _>>()?
    };
    changed_files.sort();
    changed_files.dedup();
    cancellation_checkpoint(cancellation)?;
    Ok(CommitRecordV1 {
        oid: oid.clone(),
        tree: Some(tree),
        committed_at_epoch_secs: Some(committed_at_epoch_secs),
        parents,
        changed_files,
        visited_generation: None,
        complete: true,
    })
}

fn changed_files_between(
    from: &gix::Tree<'_>,
    to: &gix::Tree<'_>,
    cancellation: &CancellationToken,
) -> Result<Vec<String>, GitHealthProjectionError> {
    let mut changed = Vec::new();
    let mut bound_exceeded = false;
    let mut path_error = None;
    from.changes()
        .map_err(|error| GitHealthProjectionError::Git(error.to_string()))?
        .for_each_to_obtain_tree(to, |change| {
            if cancellation.is_cancelled() {
                return Ok::<_, std::convert::Infallible>(std::ops::ControlFlow::Break(()));
            }
            use gix::object::tree::diff::Change;
            let mut push_path = |path: &[u8]| {
                if changed.len() >= MAX_CHANGED_FILES_PER_COMMIT {
                    bound_exceeded = true;
                    return false;
                }
                match exact_path(path) {
                    Ok(path) => changed.push(path),
                    Err(error) => path_error = Some(error),
                }
                path_error.is_none()
            };
            let keep_going = match change {
                Change::Addition {
                    location,
                    entry_mode,
                    ..
                }
                | Change::Modification {
                    location,
                    entry_mode,
                    ..
                }
                | Change::Deletion {
                    location,
                    entry_mode,
                    ..
                } => entry_mode.is_tree() || push_path(location.as_bytes()),
                Change::Rewrite {
                    source_location,
                    source_entry_mode,
                    location,
                    entry_mode,
                    ..
                } => {
                    (source_entry_mode.is_tree() || push_path(source_location.as_bytes()))
                        && (entry_mode.is_tree() || push_path(location.as_bytes()))
                }
            };
            Ok(if keep_going {
                std::ops::ControlFlow::Continue(())
            } else {
                std::ops::ControlFlow::Break(())
            })
        })
        .map_err(|error| GitHealthProjectionError::Git(error.to_string()))?;
    cancellation_checkpoint(cancellation)?;
    if let Some(error) = path_error {
        return Err(error);
    }
    if bound_exceeded {
        return Err(GitHealthProjectionError::NativeReadBoundExceeded);
    }
    Ok(changed)
}

fn exact_path(path: &[u8]) -> Result<String, GitHealthProjectionError> {
    std::str::from_utf8(path)
        .map(str::to_owned)
        .map_err(|_| GitHealthProjectionError::Git("Git path is not valid UTF-8".to_owned()))
}

fn append_topology_mutations(
    store: &GitHealthProjectionStoreV1,
    scope: &ResolvedScope,
    record: &CommitRecordV1,
    batch_commit_ids: &BTreeSet<GitOidV1>,
    cancellation: Arc<dyn GraphCancellation>,
    mutations: &mut Vec<GraphMutation>,
) -> Result<(), GitHealthProjectionError> {
    mutations.push(GraphMutation::UpsertEntity(commit_entity(record)?));
    for parent in &record.parents {
        if !batch_commit_ids.contains(parent)
            && store
                .commit_record(scope, parent, Arc::clone(&cancellation))?
                .is_none()
        {
            mutations.push(GraphMutation::UpsertEntity(commit_placeholder(parent)?));
        }
        mutations.push(GraphMutation::UpsertRelation(GraphRelation::new(
            GraphRelationId::new(format!(
                "parent:{}:{}",
                record.oid.as_str(),
                parent.as_str()
            ))?,
            commit_entity_id(&record.oid)?,
            commit_entity_id(parent)?,
            GraphRelationKind::new(PARENT_RELATION)?,
            BTreeMap::new(),
        )?));
    }
    for file in &record.changed_files {
        mutations.push(GraphMutation::UpsertEntity(file_entity(file)?));
        mutations.push(GraphMutation::UpsertRelation(GraphRelation::new(
            GraphRelationId::new(format!(
                "touch:{}:{}",
                record.oid.as_str(),
                file_digest(file)
            ))?,
            commit_entity_id(&record.oid)?,
            file_entity_id(file)?,
            GraphRelationKind::new(TOUCHED_RELATION)?,
            BTreeMap::new(),
        )?));
    }
    Ok(())
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

fn commit_placeholder(oid: &GitOidV1) -> Result<GraphEntity, GitHealthProjectionError> {
    commit_entity(&CommitRecordV1 {
        oid: oid.clone(),
        tree: None,
        committed_at_epoch_secs: None,
        parents: Vec::new(),
        changed_files: Vec::new(),
        visited_generation: None,
        complete: false,
    })
}

fn file_entity(path: &str) -> Result<GraphEntity, GitHealthProjectionError> {
    GraphEntity::new(
        file_entity_id(path)?,
        BTreeSet::from([GraphLabel::new(FILE_LABEL)?]),
        BTreeMap::from([(
            GraphPropertyName::new(FILE_PATH_PROPERTY)?,
            GraphProperty::String(path.to_owned()),
        )]),
    )
    .map_err(Into::into)
}

fn commit_entity_id(oid: &GitOidV1) -> Result<GraphEntityId, GitHealthProjectionError> {
    GraphEntityId::new(format!("commit:{}", oid.as_str())).map_err(Into::into)
}

fn file_entity_id(path: &str) -> Result<GraphEntityId, GitHealthProjectionError> {
    GraphEntityId::new(format!("file:{}", file_digest(path))).map_err(Into::into)
}

fn file_digest(path: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(path.as_bytes()))
}

fn namespace(scope: &ResolvedScope) -> Result<GraphNamespace, GitHealthProjectionError> {
    GraphNamespace::new(format!(
        "git-health-{}",
        scope
            .scope_digest
            .as_str()
            .strip_prefix("sha256:")
            .unwrap_or(scope.scope_digest.as_str())
    ))
    .map_err(Into::into)
}

fn projection() -> Result<GraphProjectionId, GitHealthProjectionError> {
    GraphProjectionId::new(PROJECTION).map_err(Into::into)
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
