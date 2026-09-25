//! Git-topology anchor authority over the canonical anchor table.

use std::collections::BTreeMap;

use tracedecay_contracts::retrieval::{
    GitTopologyAnchorAuthority, GitTopologyAnchorAuthorityError, GitTopologyAnchorFuture,
    GitTopologyAnchorPublication, GitTopologyAnchorPublicationOutcome, GitTopologyAnchorResolution,
    GitTopologyAnchorResolutionOutcome,
};
use tracedecay_domain::{ObservationScopeV1, RetrievalAnchorRecord, RetrievalAnchorTarget};
use tracedecay_runtime_core::db::engine::{params, params_from_iter};
use tracedecay_store::StoreShardScopeV1;

use crate::{RegisteredGlobalDb, RegisteredGlobalDbLeaseV1};

#[derive(Clone)]
pub struct RegisteredGitTopologyAnchorAuthority {
    database: RegisteredGlobalDbLeaseV1,
}

impl RegisteredGitTopologyAnchorAuthority {
    pub fn new(database: RegisteredGlobalDbLeaseV1) -> Self {
        Self { database }
    }

    #[hotpath::skip]
    async fn publish_records(
        &self,
        publication: GitTopologyAnchorPublication,
    ) -> Result<GitTopologyAnchorPublicationOutcome, GitTopologyAnchorAuthorityError> {
        if !binding_matches_owner(&self.database, publication.owner()) {
            return Err(GitTopologyAnchorAuthorityError::Unavailable);
        }
        let transaction = self
            .database
            .begin_write_transaction()
            .await
            .map_err(map_database_error)?;
        let records = publication.into_records();
        let anchor_ids = records
            .iter()
            .map(|record| record.anchor_id().as_str().to_owned())
            .collect::<Vec<_>>();
        let existing = read_records(&transaction, &anchor_ids).await?;
        let mut published = false;
        for candidate in records {
            let existing = existing.get(candidate.anchor_id().as_str());
            match existing {
                Some(existing) if existing.is_semantic_replay_of(&candidate) => continue,
                Some(_) => {
                    transaction
                        .rollback()
                        .await
                        .map_err(|_| GitTopologyAnchorAuthorityError::Unavailable)?;
                    return Err(GitTopologyAnchorAuthorityError::Conflict);
                }
                None => {}
            }
            let anchor_json = serde_json::to_string(&candidate)
                .map_err(|_| GitTopologyAnchorAuthorityError::Conflict)?;
            let owner_json = candidate
                .owner_column_json()
                .map_err(|_| GitTopologyAnchorAuthorityError::Conflict)?;
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
            GitTopologyAnchorPublicationOutcome::Published
        } else {
            GitTopologyAnchorPublicationOutcome::Replayed
        })
    }

    #[hotpath::skip]
    async fn resolve_record(
        &self,
        resolution: GitTopologyAnchorResolution,
    ) -> Result<GitTopologyAnchorResolutionOutcome, GitTopologyAnchorAuthorityError> {
        if !binding_matches_owner(&self.database, &resolution.owner) {
            return Err(GitTopologyAnchorAuthorityError::Unavailable);
        }
        let snapshot = self
            .database
            .read_snapshot()
            .await
            .map_err(map_database_error)?;
        let Some(record) = read_record(&snapshot, resolution.anchor_id.as_str()).await? else {
            return Ok(GitTopologyAnchorResolutionOutcome::Unavailable);
        };
        if record.owner() != &resolution.owner {
            return Ok(GitTopologyAnchorResolutionOutcome::Unavailable);
        }
        if !matches!(
            record.target(),
            RetrievalAnchorTarget::GitTopology(_)
                | RetrievalAnchorTarget::ExactRepositoryCommit { .. }
        ) {
            return Ok(GitTopologyAnchorResolutionOutcome::Unavailable);
        }
        Ok(GitTopologyAnchorResolutionOutcome::Resolved(Box::new(
            record,
        )))
    }
}

impl GitTopologyAnchorAuthority for RegisteredGitTopologyAnchorAuthority {
    fn publish<'a>(
        &'a self,
        publication: GitTopologyAnchorPublication,
    ) -> GitTopologyAnchorFuture<'a, GitTopologyAnchorPublicationOutcome> {
        Box::pin(async move { self.publish_records(publication).await })
    }

    fn resolve<'a>(
        &'a self,
        resolution: GitTopologyAnchorResolution,
    ) -> GitTopologyAnchorFuture<'a, GitTopologyAnchorResolutionOutcome> {
        Box::pin(async move { self.resolve_record(resolution).await })
    }
}

async fn read_record(
    connection: &impl tracedecay_runtime_core::db::engine::QueryExecutor,
    anchor_id: &str,
) -> Result<Option<RetrievalAnchorRecord>, GitTopologyAnchorAuthorityError> {
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
        return Err(GitTopologyAnchorAuthorityError::ResetRequired);
    }
    decode_record(&anchor_json, &owner_json, &projection_generation).map(Some)
}

#[hotpath::measure(
    future = true,
    label = "global_db.git_topology_anchor.query.publication_candidates"
)]
async fn read_records(
    connection: &impl tracedecay_runtime_core::db::engine::QueryExecutor,
    anchor_ids: &[String],
) -> Result<BTreeMap<String, RetrievalAnchorRecord>, GitTopologyAnchorAuthorityError> {
    if anchor_ids.is_empty() {
        return Ok(BTreeMap::new());
    }
    let values = (1..=anchor_ids.len())
        .map(|index| format!("(?{index})"))
        .collect::<Vec<_>>()
        .join(", ");
    let mut rows = connection
        .query(
            &format!(
                "WITH requested(anchor_id) AS (VALUES {values})
                 SELECT anchor.anchor_id, anchor.anchor_json, anchor.owner_json,
                        anchor.projection_generation
                 FROM requested
                 JOIN retrieval_anchors AS anchor USING(anchor_id)
                 ORDER BY requested.anchor_id"
            ),
            params_from_iter(anchor_ids.iter().map(String::as_str)),
        )
        .await
        .map_err(map_engine_error)?;
    let mut records = BTreeMap::new();
    while let Some(row) = rows.next().await.map_err(map_engine_error)? {
        let anchor_id = row.get::<String>(0).map_err(map_engine_error)?;
        let anchor_json = row.get::<String>(1).map_err(map_engine_error)?;
        let owner_json = row.get::<String>(2).map_err(map_engine_error)?;
        let projection_generation = row.get::<String>(3).map_err(map_engine_error)?;
        let record = decode_record(&anchor_json, &owner_json, &projection_generation)?;
        if record.anchor_id().as_str() != anchor_id || records.insert(anchor_id, record).is_some() {
            return Err(GitTopologyAnchorAuthorityError::ResetRequired);
        }
    }
    Ok(records)
}

fn decode_record(
    anchor_json: &str,
    owner_json: &str,
    projection_generation: &str,
) -> Result<RetrievalAnchorRecord, GitTopologyAnchorAuthorityError> {
    let record = serde_json::from_str::<RetrievalAnchorRecord>(anchor_json)
        .map_err(|_| GitTopologyAnchorAuthorityError::ResetRequired)?;
    record
        .validate()
        .map_err(|_| GitTopologyAnchorAuthorityError::ResetRequired)?;
    if !record.owner_column_matches(owner_json)
        || record.projection_generation().as_str() != projection_generation
    {
        return Err(GitTopologyAnchorAuthorityError::ResetRequired);
    }
    Ok(record)
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
    error: tracedecay_domain::errors::TraceDecayError,
) -> GitTopologyAnchorAuthorityError {
    if error.reset_required_context().is_some() {
        GitTopologyAnchorAuthorityError::ResetRequired
    } else {
        GitTopologyAnchorAuthorityError::Unavailable
    }
}

fn map_engine_error(
    error: tracedecay_runtime_core::db::engine::Error,
) -> GitTopologyAnchorAuthorityError {
    let detail = error.to_string();
    if detail.contains("no such table") || detail.contains("no such column") {
        GitTopologyAnchorAuthorityError::ResetRequired
    } else {
        GitTopologyAnchorAuthorityError::Unavailable
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tracedecay_runtime_core::db::{
        TestDatabaseRuntimeScope,
        engine::{IntoParams, QueryExecutor, Rows},
    };

    use super::read_records;
    use crate::tests::harness::open_registered_test_database_fixture;

    struct CountingQueryExecutor<'a, T> {
        inner: &'a T,
        queries: AtomicUsize,
    }

    impl<T: QueryExecutor> QueryExecutor for CountingQueryExecutor<'_, T> {
        async fn query<P>(
            &self,
            sql: &str,
            params: P,
        ) -> tracedecay_runtime_core::db::engine::Result<Rows>
        where
            P: IntoParams,
        {
            self.queries.fetch_add(1, Ordering::Relaxed);
            self.inner.query(sql, params).await
        }
    }

    #[tokio::test]
    async fn publication_candidate_lookup_is_batched() {
        let directory = tempfile::tempdir().unwrap();
        let (database, _owner) = open_registered_test_database_fixture(
            &directory.path().join("global.db"),
            TestDatabaseRuntimeScope::Profile,
        )
        .await
        .unwrap();
        let connection = database.read_connection();
        let counted = CountingQueryExecutor {
            inner: &connection,
            queries: AtomicUsize::new(0),
        };
        let anchor_ids = (0..256)
            .map(|index| format!("anchor.git-topology.{index}"))
            .collect::<Vec<_>>();

        let records = read_records(&counted, &anchor_ids).await.unwrap();

        assert!(records.is_empty());
        assert_eq!(counted.queries.load(Ordering::Relaxed), 1);
    }
}
