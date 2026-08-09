//! Mounted V2 Git-topology anchor authority over the canonical anchor table.

use std::sync::Arc;

use tracedecay_application::retrieval::{
    GitTopologyAnchorAuthorityErrorV2, GitTopologyAnchorAuthorityV2, GitTopologyAnchorFutureV2,
    GitTopologyAnchorPublicationOutcomeV2, GitTopologyAnchorPublicationV2,
    GitTopologyAnchorResolutionOutcomeV2, GitTopologyAnchorResolutionV2,
};
use tracedecay_domain::{ObservationScopeV1, RetrievalAnchorRecordV2, RetrievalAnchorTargetV2};
use tracedecay_runtime_core::db::engine::params;
use tracedecay_store::StoreShardScopeV1;

use crate::RegisteredGlobalDb;

#[derive(Clone)]
pub struct RegisteredGitTopologyAnchorAuthorityV2 {
    database: Arc<RegisteredGlobalDb>,
}

impl RegisteredGitTopologyAnchorAuthorityV2 {
    pub fn new(database: Arc<RegisteredGlobalDb>) -> Self {
        Self { database }
    }

    async fn publish_records(
        &self,
        publication: GitTopologyAnchorPublicationV2,
    ) -> Result<GitTopologyAnchorPublicationOutcomeV2, GitTopologyAnchorAuthorityErrorV2> {
        if !binding_matches_owner(&self.database, publication.owner()) {
            return Err(GitTopologyAnchorAuthorityErrorV2::Unavailable);
        }
        let transaction = self
            .database
            .begin_write_transaction()
            .await
            .map_err(map_database_error)?;
        let mut published = false;
        for candidate in publication.into_records() {
            let existing = read_record(&transaction, candidate.anchor_id().as_str()).await?;
            match existing {
                Some(existing) if existing.is_semantic_replay_of(&candidate) => continue,
                Some(_) => {
                    transaction
                        .rollback()
                        .await
                        .map_err(|_| GitTopologyAnchorAuthorityErrorV2::Unavailable)?;
                    return Err(GitTopologyAnchorAuthorityErrorV2::Conflict);
                }
                None => {}
            }
            let anchor_json = serde_json::to_string(&candidate)
                .map_err(|_| GitTopologyAnchorAuthorityErrorV2::Conflict)?;
            let owner_json = serde_json::to_string(candidate.owner())
                .map_err(|_| GitTopologyAnchorAuthorityErrorV2::Conflict)?;
            transaction
                .execute(
                    "INSERT INTO retrieval_anchors (
                        anchor_id, anchor_json, owner_json, projection_generation
                     ) VALUES (?1, ?2, ?3, ?4)",
                    params![
                        candidate.anchor_id().as_str(),
                        anchor_json,
                        owner_json,
                        candidate.projection_generation().as_str(),
                    ],
                )
                .await
                .map_err(map_engine_error)?;
            published = true;
        }
        transaction.commit().await.map_err(map_engine_error)?;
        Ok(if published {
            GitTopologyAnchorPublicationOutcomeV2::Published
        } else {
            GitTopologyAnchorPublicationOutcomeV2::Replayed
        })
    }

    async fn resolve_record(
        &self,
        resolution: GitTopologyAnchorResolutionV2,
    ) -> Result<GitTopologyAnchorResolutionOutcomeV2, GitTopologyAnchorAuthorityErrorV2> {
        if !binding_matches_owner(&self.database, &resolution.owner) {
            return Err(GitTopologyAnchorAuthorityErrorV2::Unavailable);
        }
        let snapshot = self
            .database
            .read_snapshot()
            .await
            .map_err(map_engine_error)?;
        let Some(record) = read_record(&snapshot, resolution.anchor_id.as_str()).await? else {
            return Ok(GitTopologyAnchorResolutionOutcomeV2::Unavailable);
        };
        if record.owner() != &resolution.owner {
            return Ok(GitTopologyAnchorResolutionOutcomeV2::Unavailable);
        }
        if !matches!(
            record.target(),
            RetrievalAnchorTargetV2::GitTopology(_)
                | RetrievalAnchorTargetV2::ExactRepositoryCommit { .. }
        ) {
            return Ok(GitTopologyAnchorResolutionOutcomeV2::Unavailable);
        }
        Ok(GitTopologyAnchorResolutionOutcomeV2::Resolved(record))
    }
}

impl GitTopologyAnchorAuthorityV2 for RegisteredGitTopologyAnchorAuthorityV2 {
    fn publish<'a>(
        &'a self,
        publication: GitTopologyAnchorPublicationV2,
    ) -> GitTopologyAnchorFutureV2<'a, GitTopologyAnchorPublicationOutcomeV2> {
        Box::pin(async move { self.publish_records(publication).await })
    }

    fn resolve<'a>(
        &'a self,
        resolution: GitTopologyAnchorResolutionV2,
    ) -> GitTopologyAnchorFutureV2<'a, GitTopologyAnchorResolutionOutcomeV2> {
        Box::pin(async move { self.resolve_record(resolution).await })
    }
}

async fn read_record(
    connection: &impl tracedecay_runtime_core::db::engine::QueryExecutor,
    anchor_id: &str,
) -> Result<Option<RetrievalAnchorRecordV2>, GitTopologyAnchorAuthorityErrorV2> {
    let mut rows = connection
        .query(
            "SELECT anchor_json, owner_json, projection_generation
             FROM retrieval_anchors WHERE anchor_id = ?1",
            params![anchor_id],
        )
        .await
        .map_err(map_engine_error)?;
    let Some(row) = rows.next().await.map_err(map_engine_error)? else {
        return Ok(None);
    };
    let anchor_json = row.get::<String>(0).map_err(map_engine_error)?;
    let owner_json = row.get::<String>(1).map_err(map_engine_error)?;
    let projection_generation = row.get::<String>(2).map_err(map_engine_error)?;
    if rows.next().await.map_err(map_engine_error)?.is_some() {
        return Err(GitTopologyAnchorAuthorityErrorV2::ResetRequired);
    }
    let record = serde_json::from_str::<RetrievalAnchorRecordV2>(&anchor_json)
        .map_err(|_| GitTopologyAnchorAuthorityErrorV2::ResetRequired)?;
    record
        .validate()
        .map_err(|_| GitTopologyAnchorAuthorityErrorV2::ResetRequired)?;
    if serde_json::to_string(record.owner()).ok().as_deref() != Some(owner_json.as_str())
        || record.projection_generation().as_str() != projection_generation
    {
        return Err(GitTopologyAnchorAuthorityErrorV2::ResetRequired);
    }
    Ok(Some(record))
}

fn binding_matches_owner(database: &RegisteredGlobalDb, owner: &ObservationScopeV1) -> bool {
    matches!(
        (&database.binding().shard_id.scope, owner),
        (
            StoreShardScopeV1::Project { project_id }
                | StoreShardScopeV1::ProjectSessions { project_id },
            ObservationScopeV1::Project {
                project_id: owner_project,
            },
        ) if project_id == owner_project
    )
}

fn map_database_error(
    error: tracedecay_runtime_core::errors::TraceDecayError,
) -> GitTopologyAnchorAuthorityErrorV2 {
    if error.reset_required_context().is_some() {
        GitTopologyAnchorAuthorityErrorV2::ResetRequired
    } else {
        GitTopologyAnchorAuthorityErrorV2::Unavailable
    }
}

fn map_engine_error(
    error: tracedecay_runtime_core::db::engine::Error,
) -> GitTopologyAnchorAuthorityErrorV2 {
    let detail = error.to_string();
    if detail.contains("no such table") || detail.contains("no such column") {
        GitTopologyAnchorAuthorityErrorV2::ResetRequired
    } else {
        GitTopologyAnchorAuthorityErrorV2::Unavailable
    }
}
