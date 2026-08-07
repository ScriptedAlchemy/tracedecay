//! Authenticated Grafeo representation for the Git health projection.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use serde::{Serialize, de::DeserializeOwned};
use tracedecay_application::{
    GitHealthProjectionBindingV1, GitHealthProjectionCoverageV1, GitHealthProjectionSourceV1,
    is_canonical_repository_relative_path,
};
use tracedecay_domain::{GitOidV1, canonical_sha256};
use tracedecay_graph_db::{
    GraphCancellation, GraphEntity, GraphEntityId, GraphLabel, GraphMutation, GraphNamespace,
    GraphProjectionId, GraphProjectionReadRequest, GraphProperty, GraphPropertyName,
};

use super::{
    COMMIT_LABEL, COMMIT_PROPERTY, CommitRecordV1, FILE_CHURN_PROPERTY, FILE_LABEL,
    FILE_PATH_PROPERTY, GENERATION_DOMAIN, GitHealthProjectionError, GitHealthProjectionStoreV1,
    HISTORY_WINDOW_SECS, MAX_CHANGED_FILES_PER_COMMIT, MAX_CHANGED_PATH_REFERENCES,
    MAX_COMMIT_RECORD_PATH_BYTES, MAX_DURABLE_FRONTIER, MAX_PATH_BYTES, MAX_PROJECTION_ENTITIES,
    MAX_UNIQUE_PATHS, MAX_WINDOW_COMMITS, NAMESPACE_DOMAIN, PROJECTION, PROJECTION_PAGE_SIZE,
    ProjectionCountersV1, ReadyStateV1, STATE_LABEL, STATE_PROPERTY, WorkingStateV1,
};

pub(super) trait PersistedStateV1: DeserializeOwned {
    fn validate(
        &self,
        binding: &GitHealthProjectionBindingV1,
    ) -> Result<(), GitHealthProjectionError>;
}

impl PersistedStateV1 for ReadyStateV1 {
    fn validate(
        &self,
        binding: &GitHealthProjectionBindingV1,
    ) -> Result<(), GitHealthProjectionError> {
        validate_source(&self.source, binding)?;
        validate_counters(&self.counters)
    }
}

impl PersistedStateV1 for WorkingStateV1 {
    fn validate(
        &self,
        binding: &GitHealthProjectionBindingV1,
    ) -> Result<(), GitHealthProjectionError> {
        validate_source(&self.target, binding)?;
        validate_counters(&self.counters)?;
        if self.pending.len() > MAX_DURABLE_FRONTIER {
            return corrupt("working frontier exceeds its durable bound");
        }
        if self.history_commits_traversed > super::MAX_HISTORY_COMMITS_TRAVERSED {
            return corrupt("working history traversal count exceeds its bound");
        }
        if self.complete && !self.pending.is_empty() {
            return corrupt("complete working state retains a frontier");
        }
        Ok(())
    }
}

pub(super) fn validate_source(
    source: &GitHealthProjectionSourceV1,
    binding: &GitHealthProjectionBindingV1,
) -> Result<(), GitHealthProjectionError> {
    if &source.binding != binding {
        return corrupt("persisted source binding does not match the mounted authority");
    }
    source
        .binding
        .validate()
        .map_err(|error| GitHealthProjectionError::Corrupt(error.to_string()))?;
    if source
        .window_end_epoch_secs
        .checked_sub(source.window_start_epoch_secs)
        != Some(HISTORY_WINDOW_SECS)
    {
        return corrupt("persisted source window is not the canonical Git health window");
    }
    let expected_generation = canonical_sha256(&(
        GENERATION_DOMAIN,
        &source.binding,
        &source.commit,
        &source.tree,
        source.window_start_epoch_secs,
        source.window_end_epoch_secs,
    ))
    .map_err(|error| GitHealthProjectionError::Corrupt(error.to_string()))?;
    if source.projection_generation != expected_generation {
        return corrupt("persisted source generation does not authenticate its identity");
    }
    Ok(())
}

fn validate_counters(counters: &ProjectionCountersV1) -> Result<(), GitHealthProjectionError> {
    if counters.commits_projected > MAX_WINDOW_COMMITS
        || counters.unique_paths > MAX_UNIQUE_PATHS
        || counters.changed_path_references > MAX_CHANGED_PATH_REFERENCES
        || counters.path_bytes > MAX_PATH_BYTES
    {
        return corrupt("persisted projection counters exceed their bounds");
    }
    if counters.unique_paths > counters.changed_path_references {
        return corrupt("persisted unique path count exceeds changed path references");
    }
    if matches!(
        counters.coverage,
        GitHealthProjectionCoverageV1::Partial { .. }
    ) && counters.commits_projected > MAX_WINDOW_COMMITS
    {
        return corrupt("partial coverage retains impossible counters");
    }
    Ok(())
}

impl GitHealthProjectionStoreV1 {
    pub(super) fn read_state<T: PersistedStateV1>(
        &self,
        binding: &GitHealthProjectionBindingV1,
        identity: &str,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<Option<T>, GitHealthProjectionError> {
        let Some(entity) = self.database.entity(
            &namespace(binding)?,
            &GraphEntityId::new(identity)?,
            cancellation,
        )?
        else {
            return Ok(None);
        };
        if entity.labels != BTreeSet::from([GraphLabel::new(STATE_LABEL)?])
            || entity.properties.len() != 1
        {
            return corrupt(format!(
                "state entity `{identity}` does not have its canonical shape"
            ));
        }
        let state: T = serde_json::from_slice(bytes_property(&entity, STATE_PROPERTY)?)
            .map_err(|error| GitHealthProjectionError::Corrupt(error.to_string()))?;
        state.validate(binding)?;
        Ok(Some(state))
    }

    pub(super) fn projection_entities(
        &self,
        binding: &GitHealthProjectionBindingV1,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<Vec<GraphEntity>, GitHealthProjectionError> {
        let mut entities = Vec::new();
        let mut after_entity = None;
        loop {
            let page = self.database.read_projection(GraphProjectionReadRequest {
                namespace: namespace(binding)?,
                projection: projection()?,
                after_entity,
                after_relation: None,
                max_entities: PROJECTION_PAGE_SIZE,
                max_relations: 0,
                cancellation: Arc::clone(&cancellation),
            })?;
            entities.extend(page.entities);
            if entities.len() > MAX_PROJECTION_ENTITIES {
                return corrupt("Git health projection exceeds its entity bound");
            }
            let Some(next) = page.next_entity else {
                break;
            };
            after_entity = Some(next);
        }
        Ok(entities)
    }
}

pub(super) fn authenticate_snapshot_entities(
    entities: Vec<GraphEntity>,
    ready: &ReadyStateV1,
) -> Result<(), GitHealthProjectionError> {
    let mut commits = 0usize;
    let mut changed_references = 0usize;
    let mut path_bytes = 0usize;
    let mut expected_churn = BTreeMap::<String, usize>::new();
    let mut stored_churn = BTreeMap::<String, usize>::new();
    let commit_label = BTreeSet::from([GraphLabel::new(COMMIT_LABEL)?]);
    let file_label = BTreeSet::from([GraphLabel::new(FILE_LABEL)?]);
    let state_label = BTreeSet::from([GraphLabel::new(STATE_LABEL)?]);
    let mut ready_state_seen = false;
    let mut working_state_seen = false;
    for entity in entities {
        if entity.labels == commit_label {
            let record = commit_record_from_entity(&entity, None)?;
            if record.committed_at_epoch_secs < ready.source.window_start_epoch_secs
                || record.committed_at_epoch_secs >= ready.source.window_end_epoch_secs
            {
                return corrupt("persisted commit lies outside the authenticated source window");
            }
            commits = checked_add(commits, 1, "authenticated commit count")?;
            changed_references = checked_add(
                changed_references,
                record.changed_files.len(),
                "authenticated changed path count",
            )?;
            for path in record.changed_files {
                path_bytes = checked_add(path_bytes, path.len(), "authenticated path bytes")?;
                *expected_churn.entry(path).or_default() += 1;
            }
        } else if entity.labels == file_label {
            let (path, churn) = file_record_from_entity(&entity)?;
            if stored_churn.insert(path, churn).is_some() {
                return corrupt("Git health projection contains duplicate file paths");
            }
        } else if entity.labels == state_label {
            match entity.identity.as_str() {
                super::READY_ENTITY if !ready_state_seen => ready_state_seen = true,
                super::WORKING_ENTITY if !working_state_seen => working_state_seen = true,
                _ => return corrupt("Git health projection contains an unauthenticated state"),
            }
        } else {
            return corrupt("Git health projection contains an unauthenticated entity");
        }
    }
    if commits != ready.counters.commits_projected
        || changed_references != ready.counters.changed_path_references
        || path_bytes != ready.counters.path_bytes
        || expected_churn.len() != ready.counters.unique_paths
        || expected_churn != stored_churn
        || !ready_state_seen
        || !working_state_seen
    {
        return corrupt("persisted projection entities do not authenticate ready counters");
    }
    Ok(())
}

pub(super) fn state_entity<T: Serialize>(
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

pub(super) fn commit_entity(
    record: &CommitRecordV1,
) -> Result<GraphEntity, GitHealthProjectionError> {
    validate_commit_record(record)?;
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

pub(super) fn file_entity(
    path: &str,
    churn: usize,
) -> Result<GraphEntity, GitHealthProjectionError> {
    if !is_canonical_repository_relative_path(path) || churn == 0 {
        return corrupt("Git health file entity is not canonical");
    }
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

pub(super) fn commit_record_from_entity(
    entity: &GraphEntity,
    expected_oid: Option<&GitOidV1>,
) -> Result<CommitRecordV1, GitHealthProjectionError> {
    if entity.labels != BTreeSet::from([GraphLabel::new(COMMIT_LABEL)?])
        || entity.properties.len() != 1
    {
        return corrupt("Git health commit entity does not have its canonical shape");
    }
    let record: CommitRecordV1 =
        serde_json::from_slice(bytes_property(entity, COMMIT_PROPERTY)?)
            .map_err(|error| GitHealthProjectionError::Corrupt(error.to_string()))?;
    validate_commit_record(&record)?;
    if entity.identity != commit_entity_id(&record.oid)?
        || expected_oid.is_some_and(|oid| oid != &record.oid)
    {
        return corrupt("Git health commit payload does not authenticate its entity OID");
    }
    Ok(record)
}

fn validate_commit_record(record: &CommitRecordV1) -> Result<(), GitHealthProjectionError> {
    if record.parents.len() > MAX_DURABLE_FRONTIER
        || record.changed_files.len() > MAX_CHANGED_FILES_PER_COMMIT
        || record.changed_files.iter().map(String::len).sum::<usize>()
            > MAX_COMMIT_RECORD_PATH_BYTES
    {
        return corrupt("Git health commit record exceeds its durable bounds");
    }
    let mut previous: Option<&str> = None;
    for path in &record.changed_files {
        if !is_canonical_repository_relative_path(path)
            || previous.is_some_and(|previous| previous >= path.as_str())
        {
            return corrupt("Git health commit paths are not canonical and strictly ordered");
        }
        previous = Some(path);
    }
    Ok(())
}

pub(super) fn file_record_from_entity(
    entity: &GraphEntity,
) -> Result<(String, usize), GitHealthProjectionError> {
    if entity.labels != BTreeSet::from([GraphLabel::new(FILE_LABEL)?])
        || entity.properties.len() != 2
    {
        return corrupt("Git health file entity does not have its canonical shape");
    }
    let path = string_property(entity, FILE_PATH_PROPERTY)?.to_owned();
    let churn = usize_property(entity, FILE_CHURN_PROPERTY)?;
    if entity.identity != file_entity_id(&path)?
        || !is_canonical_repository_relative_path(&path)
        || churn == 0
    {
        return corrupt("Git health file payload does not authenticate its entity");
    }
    Ok((path, churn))
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
                "Git health entity `{}` has no positive `{name}` property",
                entity.identity
            ))
        })
}

pub(super) fn commit_entity_id(oid: &GitOidV1) -> Result<GraphEntityId, GitHealthProjectionError> {
    GraphEntityId::new(format!("git-health-commit:{}", oid.as_str())).map_err(Into::into)
}

pub(super) fn file_entity_id(path: &str) -> Result<GraphEntityId, GitHealthProjectionError> {
    use sha2::{Digest, Sha256};
    GraphEntityId::new(format!(
        "git-health-file:{}",
        hex::encode(Sha256::digest(path.as_bytes()))
    ))
    .map_err(Into::into)
}

pub(super) fn namespace(
    binding: &GitHealthProjectionBindingV1,
) -> Result<GraphNamespace, GitHealthProjectionError> {
    let digest = canonical_sha256(&(
        NAMESPACE_DOMAIN,
        &binding.scope.project_id,
        &binding.profile_id,
        &binding.store_id,
        &binding.scope.repository_id,
        &binding.scope.worktree_id,
    ))
    .map_err(|error| GitHealthProjectionError::Corrupt(error.to_string()))?;
    GraphNamespace::new(format!(
        "git-health-{}",
        digest
            .as_str()
            .strip_prefix("sha256:")
            .unwrap_or(digest.as_str())
            .get(..64)
            .ok_or_else(|| {
                GitHealthProjectionError::Corrupt(
                    "Git health namespace digest is shorter than its identity bound".to_owned(),
                )
            })?
    ))
    .map_err(Into::into)
}

pub(super) fn projection() -> Result<GraphProjectionId, GitHealthProjectionError> {
    GraphProjectionId::new(PROJECTION).map_err(Into::into)
}

pub(super) fn coalesce_mutations(mutations: Vec<GraphMutation>) -> Vec<GraphMutation> {
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
        .ok_or_else(|| GitHealthProjectionError::Corrupt(format!("{field} overflowed")))
}

fn corrupt<T>(message: impl Into<String>) -> Result<T, GitHealthProjectionError> {
    Err(GitHealthProjectionError::Corrupt(message.into()))
}
