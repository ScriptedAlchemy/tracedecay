//! Native Git topology projected into the daemon-owned project graph.
//!
//! `gix` remains authoritative for object, reference, and parent identity.
//! Grafeo stores a rebuildable projection for cross-domain traversal; its
//! internal handles never escape as Git identity.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;
use std::sync::Arc;

use gix::bstr::ByteSlice as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tracedecay_domain::{
    GitGraphEvidenceIntent, GitGraphEvidencePublicationReceipt, GitGraphEvidenceTarget, GitOidV1,
    ProjectId,
};
use tracedecay_graph_db::{
    GraphDb, GraphDbError, GraphEntity, GraphEntityId, GraphLabel, GraphMutation, GraphNamespace,
    GraphProjectionId, GraphProperty, GraphPropertyName, GraphRelation, GraphRelationId,
    GraphRelationKind, GraphWatermark, GraphWriteBatch, NeverCancelled, SourceGeneration,
};

use crate::errors::Result as TraceDecayResult;

type GitTopologyResult<T> = std::result::Result<T, GitTopologyError>;

const PROJECTION: &str = "git-topology";
const REPOSITORY_ENTITY: &str = "git-repository";
const STATUS_ENTITY: &str = "git-topology-status";
const OBJECT_PREFIX: &str = "git-object:";
const REF_PREFIX: &str = "git-ref:";
const PARENT_KIND: &str = "git-parent";
const REF_TARGET_KIND: &str = "git-ref-target";
const CODE_EVIDENCE_KIND: &str = "git-evidence-code-generation";
const SESSION_EVIDENCE_KIND: &str = "git-evidence-session";
const WORK_EVIDENCE_KIND: &str = "git-evidence-work";
const OID_PROPERTY: &str = "git-oid";
const REF_NAME_PROPERTY: &str = "git-ref-name";
const DIRECT_TARGET_PROPERTY: &str = "git-direct-target";
const PEELED_TARGET_PROPERTY: &str = "git-peeled-target";
const SYMBOLIC_TARGET_PROPERTY: &str = "git-symbolic-target";
const REF_FRONTIER_PROPERTY: &str = "git-ref-frontier";
const PENDING_REF_FRONTIER_PROPERTY: &str = "git-pending-ref-frontier";
const COMMIT_FRONTIER_PROPERTY: &str = "git-commit-frontier";
const PENDING_GENERATION_PROPERTY: &str = "git-pending-generation";
const EVIDENCE_TARGET_PROPERTY: &str = "git-evidence-target";
const STATUS_PROPERTY: &str = "git-topology-state";
const STATUS_GENERATION_PROPERTY: &str = "git-topology-generation";
const STATUS_REASON_PROPERTY: &str = "git-topology-reason";
const STATUS_PROCESSED_PROPERTY: &str = "git-topology-processed-commits";
const STATUS_REMAINING_PROPERTY: &str = "git-topology-remaining-lower-bound";
const STATUS_THROUGHPUT_PROPERTY: &str = "git-topology-throughput-per-second";
const STATUS_ETA_MIN_PROPERTY: &str = "git-topology-eta-seconds-min";
const STATUS_ETA_MAX_PROPERTY: &str = "git-topology-eta-seconds-max";
const STATUS_WATERMARK_PROPERTY: &str = "git-topology-last-watermark";
const MAX_GIT_PARENTS: usize = 128;
const MAX_GIT_EVIDENCE_RELATIONS: usize = 10_000;

#[derive(Debug, Error)]
pub enum GitTopologyError {
    #[error("not a Git repository: {0}")]
    NotRepository(String),
    #[error("Git repository is unavailable: {0}")]
    Repository(String),
    #[error("Git topology is invalid: {0}")]
    Contract(String),
    #[error("Git evidence target is not published: {0}")]
    MissingEvidenceTarget(String),
    #[error("Git topology graph failed: {0}")]
    Graph(#[from] GraphDbError),
}

/// One exact Git reference. Names and symbolic targets remain byte-exact.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct GitReferenceRecord {
    name: Vec<u8>,
    direct_target: Option<GitOidV1>,
    peeled_target: Option<GitOidV1>,
    symbolic_target: Option<Vec<u8>>,
}

impl GitReferenceRecord {
    #[must_use]
    pub fn name(&self) -> &[u8] {
        &self.name
    }

    #[must_use]
    pub fn direct_target(&self) -> Option<&GitOidV1> {
        self.direct_target.as_ref()
    }

    #[must_use]
    pub fn peeled_target(&self) -> Option<&GitOidV1> {
        self.peeled_target.as_ref()
    }

    #[must_use]
    pub fn symbolic_target(&self) -> Option<&[u8]> {
        self.symbolic_target.as_deref()
    }
}

fn measured_throughput(processed: usize, elapsed: std::time::Duration) -> Option<u64> {
    let elapsed_millis = elapsed.as_millis();
    (elapsed_millis > 0).then(|| {
        match u64::try_from(
            (processed as u128)
                .saturating_mul(1_000)
                .saturating_div(elapsed_millis),
        ) {
            Ok(value) => value,
            Err(_) => u64::MAX,
        }
    })
}

#[derive(Clone)]
pub struct GitTopologyStore {
    database: Arc<GraphDb>,
}

impl GitTopologyStore {
    #[must_use]
    pub const fn new(database: Arc<GraphDb>) -> Self {
        Self { database }
    }

    pub fn publish_evidence(
        &self,
        project: &ProjectId,
        evidence: &[GitGraphEvidenceIntent],
    ) -> GitTopologyResult<Vec<GitGraphEvidencePublicationReceipt>> {
        if evidence.len() > MAX_GIT_EVIDENCE_RELATIONS {
            return Err(GitTopologyError::Contract(format!(
                "Git evidence batch exceeds {MAX_GIT_EVIDENCE_RELATIONS} relations"
            )));
        }
        let namespace = namespace(project)?;
        let commit_label = GraphLabel::new("git-commit")?;
        let mut relations = BTreeMap::new();
        let mut digest = Sha256::new();
        digest.update(b"tracedecay.git-evidence\0");
        for intent in evidence {
            intent
                .validate()
                .map_err(|error| GitTopologyError::Contract(error.to_string()))?;
            if intent.project_id() != project {
                return Err(GitTopologyError::Contract(
                    "Git evidence intent belongs to another project".to_owned(),
                ));
            }
            let commit = object_entity_id(intent.commit())?;
            let commit_entity = self
                .database
                .entity(&namespace, &commit, Arc::new(NeverCancelled))?
                .ok_or_else(|| {
                    GitTopologyError::Contract(format!(
                        "evidence commit {} is not published",
                        intent.commit()
                    ))
                })?;
            if !commit_entity.labels.contains(&commit_label) {
                return Err(GitTopologyError::Contract(format!(
                    "evidence object {} is not a commit",
                    intent.commit()
                )));
            }
            let target = evidence_target_entity_id(intent.target())?;
            if self
                .database
                .entity(&namespace, &target, Arc::new(NeverCancelled))?
                .is_none()
            {
                return Err(GitTopologyError::MissingEvidenceTarget(target.to_string()));
            }
            let relation = evidence_relation(intent)?;
            digest.update(relation.identity.as_str().as_bytes());
            digest.update([0]);
            relations.insert(relation.identity.clone(), relation);
        }
        if relations.is_empty() {
            return Ok(Vec::new());
        }
        let digest = hex::encode(digest.finalize());
        let commit = self.database.apply(GraphWriteBatch::new(
            namespace,
            projection()?,
            SourceGeneration::new(format!("git-evidence:{digest}"))?,
            GraphWatermark::new(format!("git-evidence:{digest}"))?,
            relations
                .into_values()
                .map(GraphMutation::UpsertRelation)
                .collect(),
            Arc::new(NeverCancelled),
        )?)?;
        evidence
            .iter()
            .map(|intent| {
                GitGraphEvidencePublicationReceipt::new(
                    intent.intent_digest().clone(),
                    commit.watermark.as_str().to_owned(),
                    commit.sequence,
                )
                .map_err(|error| GitTopologyError::Contract(error.to_string()))
            })
            .collect()
    }

    pub fn reference(
        &self,
        project: &ProjectId,
        name: &[u8],
    ) -> GitTopologyResult<Option<GitReferenceRecord>> {
        let Some(entity) = self.database.entity(
            &namespace(project)?,
            &reference_entity_id(name)?,
            Arc::new(NeverCancelled),
        )?
        else {
            return Ok(None);
        };
        Ok(Some(GitReferenceRecord {
            name: bytes_property(&entity, REF_NAME_PROPERTY)?,
            direct_target: optional_git_oid_property(&entity, DIRECT_TARGET_PROPERTY)?,
            peeled_target: optional_git_oid_property(&entity, PEELED_TARGET_PROPERTY)?,
            symbolic_target: optional_bytes_property(&entity, SYMBOLIC_TARGET_PROPERTY)?,
        }))
    }

    pub fn parents_of(
        &self,
        project: &ProjectId,
        commit: &GitOidV1,
    ) -> GitTopologyResult<Vec<GitOidV1>> {
        let namespace = namespace(project)?;
        let starts = [object_entity_id(commit)?];
        let kinds = BTreeSet::from([GraphRelationKind::new(PARENT_KIND)?]);
        let relations = self.database.outgoing_relations(
            &namespace,
            &starts,
            &kinds,
            MAX_GIT_PARENTS,
            Arc::new(NeverCancelled),
        )?;
        let relations = relations.into_iter().next().ok_or_else(|| {
            GitTopologyError::Contract(
                "Git parent point read omitted its requested commit".to_owned(),
            )
        })?;
        Ok(relations
            .iter()
            .filter_map(|relation| parse_object_entity(&relation.to))
            .collect())
    }

    pub fn evidence_for(
        &self,
        project: &ProjectId,
        commit: &GitOidV1,
    ) -> GitTopologyResult<Vec<GitGraphEvidenceTarget>> {
        let namespace = namespace(project)?;
        let starts = [object_entity_id(commit)?];
        let kinds = BTreeSet::from([
            GraphRelationKind::new(CODE_EVIDENCE_KIND)?,
            GraphRelationKind::new(SESSION_EVIDENCE_KIND)?,
            GraphRelationKind::new(WORK_EVIDENCE_KIND)?,
        ]);
        let relations = self.database.outgoing_relations(
            &namespace,
            &starts,
            &kinds,
            MAX_GIT_EVIDENCE_RELATIONS,
            Arc::new(NeverCancelled),
        )?;
        let relations = relations.into_iter().next().ok_or_else(|| {
            GitTopologyError::Contract(
                "Git evidence point read omitted its requested commit".to_owned(),
            )
        })?;
        Ok(relations
            .iter()
            .filter_map(parse_evidence_relation)
            .collect())
    }

    pub fn freshness(&self, project: &ProjectId) -> GitTopologyResult<GitTopologyFreshness> {
        let entity = self.database.entity(
            &namespace(project)?,
            &GraphEntityId::new(STATUS_ENTITY)?,
            Arc::new(NeverCancelled),
        )?;
        let Some(entity) = entity else {
            return Ok(GitTopologyFreshness {
                state: GitTopologyState::Indexing,
                processed_commits: 0,
                remaining_lower_bound: None,
                throughput_per_second: None,
                eta_seconds_range: None,
                last_watermark: None,
                generation: None,
                reason: Some("not_indexed".to_owned()),
            });
        };
        let state = match string_property(&entity, STATUS_PROPERTY)?.as_str() {
            "indexing" => GitTopologyState::Indexing,
            "complete" => GitTopologyState::Complete,
            "partial" => GitTopologyState::Partial,
            "stalled" => GitTopologyState::Stalled,
            "failed" => GitTopologyState::Failed,
            other => {
                return Err(GitTopologyError::Contract(format!(
                    "unknown Git topology status '{other}'"
                )));
            }
        };
        Ok(GitTopologyFreshness {
            state,
            processed_commits: match optional_u64_property(&entity, STATUS_PROCESSED_PROPERTY)? {
                Some(value) => value,
                None => 0,
            },
            remaining_lower_bound: optional_u64_property(&entity, STATUS_REMAINING_PROPERTY)?,
            throughput_per_second: optional_u64_property(&entity, STATUS_THROUGHPUT_PROPERTY)?,
            eta_seconds_range: optional_u64_property(&entity, STATUS_ETA_MIN_PROPERTY)?
                .zip(optional_u64_property(&entity, STATUS_ETA_MAX_PROPERTY)?),
            last_watermark: optional_string_property(&entity, STATUS_WATERMARK_PROPERTY)?,
            generation: optional_string_property(&entity, STATUS_GENERATION_PROPERTY)?,
            reason: optional_string_property(&entity, STATUS_REASON_PROPERTY)?,
        })
    }

    fn write_freshness(
        &self,
        project: &ProjectId,
        freshness: &GitTopologyFreshness,
    ) -> GitTopologyResult<()> {
        let state = freshness.state.as_str();
        let source = stable_text_digest(&format!(
            "{state}:{}:{:?}:{:?}",
            freshness.processed_commits, freshness.generation, freshness.last_watermark
        ));
        self.database.apply(GraphWriteBatch::new(
            namespace(project)?,
            projection()?,
            SourceGeneration::new(source.clone())?,
            GraphWatermark::new(format!("git-status:{source}"))?,
            vec![GraphMutation::UpsertEntity(freshness_entity(freshness)?)],
            Arc::new(NeverCancelled),
        )?)?;
        Ok(())
    }

    fn load_frontier(&self, project: &ProjectId) -> GitTopologyResult<GitFrontierState> {
        let entity = self.database.projection_entity(
            &namespace(project)?,
            &projection()?,
            &GraphEntityId::new(REPOSITORY_ENTITY)?,
            Arc::new(NeverCancelled),
        )?;
        let Some(entity) = entity else {
            return Ok(GitFrontierState::default());
        };
        Ok(GitFrontierState {
            completed_refs: match decode_json_property(&entity, REF_FRONTIER_PROPERTY)? {
                Some(value) => value,
                None => Vec::new(),
            },
            pending_refs: match decode_json_property(&entity, PENDING_REF_FRONTIER_PROPERTY)? {
                Some(value) => value,
                None => Vec::new(),
            },
            commits: match decode_json_property(&entity, COMMIT_FRONTIER_PROPERTY)? {
                Some(value) => value,
                None => Vec::new(),
            },
            pending_generation: optional_string_property(&entity, PENDING_GENERATION_PROPERTY)?,
        })
    }

    fn published_object_kinds(
        &self,
        project: &ProjectId,
        objects: &[GitOidV1],
    ) -> GitTopologyResult<Vec<Option<bool>>> {
        let identities = objects
            .iter()
            .map(object_entity_id)
            .collect::<GitTopologyResult<Vec<_>>>()?;
        let commit_label = GraphLabel::new("git-commit")?;
        let entities = self.database.projection_entities(
            &namespace(project)?,
            &projection()?,
            &identities,
            Arc::new(NeverCancelled),
        )?;
        Ok(entities
            .into_iter()
            .map(|entity| entity.map(|entity| entity.labels.contains(&commit_label)))
            .collect())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GitTopologyState {
    Indexing,
    Complete,
    Partial,
    Stalled,
    Failed,
}

impl GitTopologyState {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Indexing => "indexing",
            Self::Complete => "complete",
            Self::Partial => "partial",
            Self::Stalled => "stalled",
            Self::Failed => "failed",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitTopologyFreshness {
    pub state: GitTopologyState,
    pub processed_commits: u64,
    pub remaining_lower_bound: Option<u64>,
    pub throughput_per_second: Option<u64>,
    pub eta_seconds_range: Option<(u64, u64)>,
    pub last_watermark: Option<String>,
    pub generation: Option<String>,
    pub reason: Option<String>,
}

#[path = "git_convergence.rs"]
mod convergence;
use convergence::GitFrontierState;
#[cfg(test)]
use convergence::converge_slice;
pub use convergence::{GitEvidenceReceiptSink, GitTopologyConvergenceOwner};

fn namespace(project: &ProjectId) -> GitTopologyResult<GraphNamespace> {
    tracedecay_code_index::graph_projection::project_graph_namespace(project)
        .map_err(|error| GitTopologyError::Contract(error.to_string()))
}

fn projection() -> GitTopologyResult<GraphProjectionId> {
    GraphProjectionId::new(PROJECTION).map_err(Into::into)
}

fn object_entity(oid: &GitOidV1, commit: bool) -> GitTopologyResult<GraphEntity> {
    let mut labels = BTreeSet::from([GraphLabel::new("git-object")?]);
    if commit {
        labels.insert(GraphLabel::new("git-commit")?);
    }
    GraphEntity::new(
        object_entity_id(oid)?,
        labels,
        BTreeMap::from([(
            GraphPropertyName::new(OID_PROPERTY)?,
            GraphProperty::String(oid.as_str().to_owned()),
        )]),
    )
    .map_err(Into::into)
}

fn reference_entity(reference: &GitReferenceRecord) -> GitTopologyResult<GraphEntity> {
    let mut properties = BTreeMap::from([(
        GraphPropertyName::new(REF_NAME_PROPERTY)?,
        GraphProperty::Bytes(reference.name.clone()),
    )]);
    if let Some(target) = &reference.direct_target {
        properties.insert(
            GraphPropertyName::new(DIRECT_TARGET_PROPERTY)?,
            GraphProperty::String(target.to_string()),
        );
    }
    if let Some(target) = &reference.peeled_target {
        properties.insert(
            GraphPropertyName::new(PEELED_TARGET_PROPERTY)?,
            GraphProperty::String(target.to_string()),
        );
    }
    if let Some(target) = &reference.symbolic_target {
        properties.insert(
            GraphPropertyName::new(SYMBOLIC_TARGET_PROPERTY)?,
            GraphProperty::Bytes(target.clone()),
        );
    }
    GraphEntity::new(
        reference_entity_id(&reference.name)?,
        BTreeSet::from([GraphLabel::new("git-reference")?]),
        properties,
    )
    .map_err(Into::into)
}

fn freshness_entity(freshness: &GitTopologyFreshness) -> GitTopologyResult<GraphEntity> {
    let mut properties = BTreeMap::from([
        (
            GraphPropertyName::new(STATUS_PROPERTY)?,
            GraphProperty::String(freshness.state.as_str().to_owned()),
        ),
        (
            GraphPropertyName::new(STATUS_PROCESSED_PROPERTY)?,
            GraphProperty::I64(saturating_i64(freshness.processed_commits)),
        ),
    ]);
    insert_optional_u64(
        &mut properties,
        STATUS_REMAINING_PROPERTY,
        freshness.remaining_lower_bound,
    )?;
    insert_optional_u64(
        &mut properties,
        STATUS_THROUGHPUT_PROPERTY,
        freshness.throughput_per_second,
    )?;
    if let Some((minimum, maximum)) = freshness.eta_seconds_range {
        insert_optional_u64(&mut properties, STATUS_ETA_MIN_PROPERTY, Some(minimum))?;
        insert_optional_u64(&mut properties, STATUS_ETA_MAX_PROPERTY, Some(maximum))?;
    }
    if let Some(generation) = &freshness.generation {
        properties.insert(
            GraphPropertyName::new(STATUS_GENERATION_PROPERTY)?,
            GraphProperty::String(generation.clone()),
        );
    }
    if let Some(watermark) = &freshness.last_watermark {
        properties.insert(
            GraphPropertyName::new(STATUS_WATERMARK_PROPERTY)?,
            GraphProperty::String(watermark.clone()),
        );
    }
    if let Some(reason) = &freshness.reason {
        properties.insert(
            GraphPropertyName::new(STATUS_REASON_PROPERTY)?,
            GraphProperty::String(reason.chars().take(1_024).collect()),
        );
    }
    GraphEntity::new(
        GraphEntityId::new(STATUS_ENTITY)?,
        BTreeSet::from([GraphLabel::new("git-topology-status")?]),
        properties,
    )
    .map_err(Into::into)
}

fn parent_relation(commit: &GitOidV1, parent: &GitOidV1) -> GitTopologyResult<GraphRelation> {
    GraphRelation::new(
        GraphRelationId::new(format!(
            "git-parent:{}:{}",
            commit.as_str(),
            parent.as_str()
        ))?,
        object_entity_id(commit)?,
        object_entity_id(parent)?,
        GraphRelationKind::new(PARENT_KIND)?,
        BTreeMap::new(),
    )
    .map_err(Into::into)
}

fn reference_target_relation(
    reference: &GitReferenceRecord,
    target: &GitOidV1,
) -> GitTopologyResult<GraphRelation> {
    GraphRelation::new(
        GraphRelationId::new(format!(
            "git-ref-target:{}:{}",
            stable_bytes_digest(&reference.name),
            target.as_str()
        ))?,
        reference_entity_id(&reference.name)?,
        object_entity_id(target)?,
        GraphRelationKind::new(REF_TARGET_KIND)?,
        BTreeMap::new(),
    )
    .map_err(Into::into)
}

fn evidence_target_entity_id(target: &GitGraphEvidenceTarget) -> GitTopologyResult<GraphEntityId> {
    match target {
        GitGraphEvidenceTarget::CodeGeneration(generation) => {
            tracedecay_code_index::graph_projection::code_generation_entity_id(generation)
                .map_err(|error| GitTopologyError::Contract(error.to_string()))
        }
        GitGraphEvidenceTarget::Session(session) => {
            tracedecay_global_db::session_temporal::relations::session_entity_id(session)
                .map_err(|error| GitTopologyError::Contract(error.to_string()))
        }
        GitGraphEvidenceTarget::Work(task) => {
            tracedecay_rusqlite_runtime::work::topology::work_task_entity_id(task)
                .map_err(|error| GitTopologyError::Contract(error.to_string()))
        }
    }
}

fn evidence_relation_kind(target: &GitGraphEvidenceTarget) -> &'static str {
    match target {
        GitGraphEvidenceTarget::CodeGeneration(_) => CODE_EVIDENCE_KIND,
        GitGraphEvidenceTarget::Session(_) => SESSION_EVIDENCE_KIND,
        GitGraphEvidenceTarget::Work(_) => WORK_EVIDENCE_KIND,
    }
}

fn evidence_relation(evidence: &GitGraphEvidenceIntent) -> GitTopologyResult<GraphRelation> {
    let target = evidence_target_entity_id(evidence.target())?;
    let target_value = match evidence.target() {
        GitGraphEvidenceTarget::CodeGeneration(generation) => generation.as_str(),
        GitGraphEvidenceTarget::Session(session) => session.as_str(),
        GitGraphEvidenceTarget::Work(task) => task.as_str(),
    };
    let relation_kind = evidence_relation_kind(evidence.target());
    GraphRelation::new(
        GraphRelationId::new(format!(
            "{}:{}:{}",
            relation_kind,
            evidence.commit().as_str(),
            stable_text_digest(target.as_str())
        ))?,
        object_entity_id(evidence.commit())?,
        target,
        GraphRelationKind::new(relation_kind)?,
        BTreeMap::from([(
            GraphPropertyName::new(EVIDENCE_TARGET_PROPERTY)?,
            GraphProperty::String(target_value.to_owned()),
        )]),
    )
    .map_err(Into::into)
}

fn object_entity_id(oid: &GitOidV1) -> GitTopologyResult<GraphEntityId> {
    GraphEntityId::new(format!("{OBJECT_PREFIX}{}", oid.as_str())).map_err(Into::into)
}

fn reference_entity_id(name: &[u8]) -> GitTopologyResult<GraphEntityId> {
    GraphEntityId::new(format!("{REF_PREFIX}{}", stable_bytes_digest(name))).map_err(Into::into)
}

fn parse_object_entity(identity: &GraphEntityId) -> Option<GitOidV1> {
    identity
        .as_str()
        .strip_prefix(OBJECT_PREFIX)
        .and_then(|oid| GitOidV1::new(oid.to_owned()).ok())
}

fn parse_evidence_relation(relation: &GraphRelation) -> Option<GitGraphEvidenceTarget> {
    let target = relation
        .properties
        .get(&GraphPropertyName::new(EVIDENCE_TARGET_PROPERTY).ok()?)
        .and_then(|property| match property {
            GraphProperty::String(value) => Some(value.as_str()),
            _ => None,
        })?;
    match relation.kind.as_str() {
        CODE_EVIDENCE_KIND => tracedecay_domain::CodeGenerationId::new(target.to_owned())
            .ok()
            .map(GitGraphEvidenceTarget::CodeGeneration),
        SESSION_EVIDENCE_KIND => tracedecay_domain::SessionId::new(target.to_owned())
            .ok()
            .map(GitGraphEvidenceTarget::Session),
        WORK_EVIDENCE_KIND => tracedecay_domain::TaskId::new(target.to_owned())
            .ok()
            .map(GitGraphEvidenceTarget::Work),
        _ => None,
    }
}

fn string_property(entity: &GraphEntity, name: &str) -> GitTopologyResult<String> {
    optional_string_property(entity, name)?.ok_or_else(|| {
        GitTopologyError::Contract(format!("Git topology entity has no '{name}' property"))
    })
}

fn optional_string_property(entity: &GraphEntity, name: &str) -> GitTopologyResult<Option<String>> {
    match entity.properties.get(&GraphPropertyName::new(name)?) {
        Some(GraphProperty::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(GitTopologyError::Contract(format!(
            "Git topology property '{name}' is not a string"
        ))),
        None => Ok(None),
    }
}

fn bytes_property(entity: &GraphEntity, name: &str) -> GitTopologyResult<Vec<u8>> {
    optional_bytes_property(entity, name)?.ok_or_else(|| {
        GitTopologyError::Contract(format!("Git topology entity has no '{name}' property"))
    })
}

fn optional_bytes_property(entity: &GraphEntity, name: &str) -> GitTopologyResult<Option<Vec<u8>>> {
    match entity.properties.get(&GraphPropertyName::new(name)?) {
        Some(GraphProperty::Bytes(value)) => Ok(Some(value.clone())),
        Some(_) => Err(GitTopologyError::Contract(format!(
            "Git topology property '{name}' is not bytes"
        ))),
        None => Ok(None),
    }
}

fn optional_git_oid_property(
    entity: &GraphEntity,
    name: &str,
) -> GitTopologyResult<Option<GitOidV1>> {
    optional_string_property(entity, name)?
        .map(GitOidV1::new)
        .transpose()
        .map_err(|error| GitTopologyError::Contract(error.to_string()))
}

fn optional_u64_property(entity: &GraphEntity, name: &str) -> GitTopologyResult<Option<u64>> {
    match entity.properties.get(&GraphPropertyName::new(name)?) {
        Some(GraphProperty::I64(value)) => u64::try_from(*value)
            .map(Some)
            .map_err(|_| GitTopologyError::Contract(format!("'{name}' is negative"))),
        Some(_) => Err(GitTopologyError::Contract(format!(
            "Git topology property '{name}' is not an integer"
        ))),
        None => Ok(None),
    }
}

fn insert_optional_u64(
    properties: &mut BTreeMap<GraphPropertyName, GraphProperty>,
    name: &str,
    value: Option<u64>,
) -> GitTopologyResult<()> {
    if let Some(value) = value {
        properties.insert(
            GraphPropertyName::new(name)?,
            GraphProperty::I64(saturating_i64(value)),
        );
    }
    Ok(())
}

fn git_oid(value: String) -> GitTopologyResult<GitOidV1> {
    GitOidV1::new(value).map_err(|error| GitTopologyError::Contract(error.to_string()))
}

fn parse_object_id(oid: &GitOidV1) -> GitTopologyResult<gix::ObjectId> {
    gix::ObjectId::from_hex(oid.as_str().as_bytes())
        .map_err(|error| GitTopologyError::Contract(error.to_string()))
}

fn stable_text_digest(value: &str) -> String {
    stable_bytes_digest(value.as_bytes())
}

fn stable_bytes_digest(value: &[u8]) -> String {
    hex::encode(Sha256::digest(value))
}

fn map_open_error(error: gix::open::Error) -> GitTopologyError {
    match error {
        gix::open::Error::NotARepository { path, .. } => {
            GitTopologyError::NotRepository(path.display().to_string())
        }
        error => GitTopologyError::Repository(error.to_string()),
    }
}

fn decode_json_property<T: for<'de> Deserialize<'de>>(
    entity: &GraphEntity,
    name: &str,
) -> GitTopologyResult<Option<T>> {
    match entity.properties.get(&GraphPropertyName::new(name)?) {
        Some(GraphProperty::String(json)) => serde_json::from_str(json)
            .map(Some)
            .map_err(|error| GitTopologyError::Contract(error.to_string())),
        Some(_) => Err(GitTopologyError::Contract(format!(
            "Git topology property '{name}' is not canonical JSON text"
        ))),
        None => Ok(None),
    }
}

/// Returns a map of `file_path` → `commit_count` for the last `days` days.
/// Native Git read failures remain a non-fatal absence for health scoring.
pub async fn file_churn(
    project_root: &Path,
    days: u32,
) -> TraceDecayResult<HashMap<String, usize>> {
    let project_root = project_root.to_path_buf();
    match tokio::task::spawn_blocking(move || native_file_churn(&project_root, days)).await {
        Ok(Ok(churn)) => Ok(churn),
        Ok(Err(error)) => {
            tracing::debug!(
                event = "git_file_churn",
                outcome = "unavailable",
                error = %error,
            );
            Ok(HashMap::new())
        }
        Err(error) => {
            tracing::debug!(
                event = "git_file_churn",
                outcome = "task_failed",
                error = %error,
            );
            Ok(HashMap::new())
        }
    }
}

fn native_file_churn(
    project_root: &Path,
    days: u32,
) -> std::result::Result<HashMap<String, usize>, GitTopologyError> {
    let repository = gix::open(project_root).map_err(map_open_error)?;
    let Some(head) = repository
        .head()
        .map_err(|error| GitTopologyError::Repository(error.to_string()))?
        .id()
    else {
        return Ok(HashMap::new());
    };
    let cutoff = std::time::SystemTime::now()
        .checked_sub(std::time::Duration::from_secs(
            u64::from(days).saturating_mul(86_400),
        ))
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| {
            i64::try_from(duration.as_secs()).map_err(|error| {
                GitTopologyError::Contract(format!("Git churn cutoff is out of range: {error}"))
            })
        })
        .transpose()?;
    let walk = repository
        .rev_walk([head.detach()])
        .sorting(gix::revision::walk::Sorting::ByCommitTime(
            Default::default(),
        ))
        .all()
        .map_err(|error| GitTopologyError::Repository(error.to_string()))?;
    let mut churn = HashMap::new();
    for info in walk {
        let info = info.map_err(|error| GitTopologyError::Repository(error.to_string()))?;
        let commit = repository
            .find_commit(info.id)
            .map_err(|error| GitTopologyError::Repository(error.to_string()))?;
        let decoded = commit
            .decode()
            .map_err(|error| GitTopologyError::Repository(error.to_string()))?;
        let committed_at = decoded
            .committer()
            .map_err(|error| GitTopologyError::Repository(error.to_string()))?
            .seconds();
        if cutoff.is_some_and(|cutoff| committed_at < cutoff) {
            break;
        }
        let tree = commit
            .tree()
            .map_err(|error| GitTopologyError::Repository(error.to_string()))?;
        if let Some(parent) = commit.parent_ids().next() {
            let parent_object = parent
                .object()
                .map_err(|error| GitTopologyError::Repository(error.to_string()))?;
            let parent_commit = parent_object
                .try_into_commit()
                .map_err(|error| GitTopologyError::Repository(error.to_string()))?;
            let parent_tree = parent_commit
                .tree()
                .map_err(|error| GitTopologyError::Repository(error.to_string()))?;
            parent_tree
                .changes()
                .map_err(|error| GitTopologyError::Repository(error.to_string()))?
                .for_each_to_obtain_tree(&tree, |change| {
                    use gix::object::tree::diff::Change;
                    let path = match change {
                        Change::Addition { location, .. }
                        | Change::Deletion { location, .. }
                        | Change::Modification { location, .. }
                        | Change::Rewrite { location, .. } => location.to_string(),
                    };
                    *churn.entry(path).or_insert(0) += 1;
                    Ok::<_, std::convert::Infallible>(std::ops::ControlFlow::Continue(()))
                })
                .map_err(|error| GitTopologyError::Repository(error.to_string()))?;
        } else {
            let entries = tree
                .traverse()
                .breadthfirst
                .files()
                .map_err(|error| GitTopologyError::Repository(error.to_string()))?;
            for entry in entries {
                *churn.entry(entry.filepath.to_string()).or_insert(0) += 1;
            }
        }
    }
    Ok(churn)
}

fn saturating_i64(value: u64) -> i64 {
    match i64::try_from(value) {
        Ok(value) => value,
        Err(_) => i64::MAX,
    }
}

#[cfg(test)]
#[path = "git_tests.rs"]
mod tests;
