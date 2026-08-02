#[cfg(test)]
use tracedecay_domain::canonical_text::{CANONICAL_TEXT_MAX_BYTES, is_canonical_text_within};
use tracedecay_domain::{FactOwnerV1, RetrievalAnchorId, UtcMicros};
use tracedecay_store::{
    AnchorDerivativeKindV1, AnchorDispositionAppendOutcomeV1, AnchorDispositionStateV1,
    RetrievalAnchorDerivativeV1, RetrievalAnchorDispositionRecordV1,
    RetrievalAnchorDispositionStore, RetrievalAnchorOwnerV1, RetrievalAnchorStoreError,
    RetrievalAnchorStoreResult, RetrievalAnchorTombstoneV1,
};

use crate::db::engine::{QueryExecutor, params};
use crate::errors::{Result, TraceDecayError};

const OPERATION: &str = "retrieval anchor authority";

impl From<RetrievalAnchorStoreError> for TraceDecayError {
    fn from(error: RetrievalAnchorStoreError) -> Self {
        authority_error(error.to_string())
    }
}

#[cfg(test)]
fn validate_label(value: &str, field: &str) -> Result<()> {
    if !is_canonical_text_within(value, CANONICAL_TEXT_MAX_BYTES) {
        return Err(authority_error(format!("{field} is not canonical")));
    }
    Ok(())
}

fn authority_error(message: impl Into<String>) -> TraceDecayError {
    TraceDecayError::Database {
        message: message.into(),
        operation: OPERATION.to_owned(),
    }
}

fn database_error(error: impl std::fmt::Display) -> TraceDecayError {
    authority_error(error.to_string())
}

fn owner_json(owner: &impl serde::Serialize) -> Result<String> {
    serde_json::to_string(owner).map_err(database_error)
}

// The disposition legality rules are owned by
// `AnchorDispositionStateV1` in `tracedecay-store`, because the
// rusqlite-runtime `RetrievalAnchorExecutor` appends to the same tables and
// the two must never disagree about what an anchor's history permits. Only
// the refusal wording below is local, and it is observable, so it stays.

fn disposition_transition_allowed(
    current: Option<AnchorDispositionStateV1>,
    next: AnchorDispositionStateV1,
) -> bool {
    AnchorDispositionStateV1::transition_allowed(current, next)
}

fn suppresses_derivatives(state: AnchorDispositionStateV1) -> bool {
    state.suppresses_derivatives()
}

async fn current_disposition(
    connection: &(impl QueryExecutor + Sync),
    anchor_id: &RetrievalAnchorId,
    owner: &str,
) -> Result<Option<AnchorDispositionStateV1>> {
    let mut rows = connection
        .query(
            "SELECT state FROM retrieval_anchor_dispositions
             WHERE anchor_id = ?1 AND owner_json = ?2
             ORDER BY sequence DESC LIMIT 1",
            params![anchor_id.as_str(), owner],
        )
        .await
        .map_err(database_error)?;
    Ok(rows
        .next()
        .await
        .map_err(database_error)?
        .map(|row| row.get::<String>(0).map_err(database_error))
        .transpose()?
        .map(|state| AnchorDispositionStateV1::parse(&state))
        .transpose()?)
}

pub(crate) async fn resolve_anchor_derivatives<O>(
    connection: &(impl QueryExecutor + Sync),
    owner: &O,
    anchor_id: &RetrievalAnchorId,
) -> Result<Vec<RetrievalAnchorDerivativeV1>>
where
    O: serde::Serialize + Clone + Into<RetrievalAnchorOwnerV1>,
{
    let owner_json = owner_json(owner)?;
    if !AnchorDispositionStateV1::serves_derivatives(
        current_disposition(connection, anchor_id, &owner_json).await?,
    ) {
        return Ok(Vec::new());
    }
    let mut rows = connection
        .query(
            "SELECT lineage.derivative_kind, lineage.derivative_id, lineage.direct_evidence
             FROM retrieval_anchor_reverse_lineage AS lineage
             WHERE lineage.source_anchor_id = ?1 AND lineage.owner_json = ?2
               AND NOT EXISTS (
                   SELECT 1 FROM retrieval_anchor_derivative_tombstones AS tombstone
                   WHERE tombstone.source_anchor_id = lineage.source_anchor_id
                     AND tombstone.owner_json = lineage.owner_json
                     AND tombstone.derivative_kind = lineage.derivative_kind
                     AND tombstone.derivative_id = lineage.derivative_id
               )
             ORDER BY lineage.derivative_kind, lineage.derivative_id",
            params![anchor_id.as_str(), owner_json],
        )
        .await
        .map_err(database_error)?;
    let mut derivatives = Vec::new();
    while let Some(row) = rows.next().await.map_err(database_error)? {
        derivatives.push(RetrievalAnchorDerivativeV1::new(
            anchor_id.clone(),
            owner.clone().into(),
            AnchorDerivativeKindV1::parse(&row.get::<String>(0).map_err(database_error)?)?,
            row.get::<String>(1).map_err(database_error)?,
            row.get::<i64>(2).map_err(database_error)? != 0,
        )?);
    }
    Ok(derivatives)
}

#[cfg(test)]
pub(crate) async fn resolve_anchor_derivative(
    connection: &(impl QueryExecutor + Sync),
    owner: &FactOwnerV1,
    kind: AnchorDerivativeKindV1,
    derivative_id: &str,
) -> Result<bool> {
    validate_label(derivative_id, "anchor derivative id")?;
    let owner = owner_json(owner)?;
    let mut rows = connection
        .query(
            "SELECT 1
             FROM retrieval_anchor_reverse_lineage AS lineage
             WHERE lineage.owner_json = ?1
               AND lineage.derivative_kind = ?2
               AND lineage.derivative_id = ?3
               AND lineage.direct_evidence = 1
               AND NOT EXISTS (
                   SELECT 1 FROM retrieval_anchor_derivative_tombstones AS tombstone
                   WHERE tombstone.source_anchor_id = lineage.source_anchor_id
                     AND tombstone.owner_json = lineage.owner_json
                     AND tombstone.derivative_kind = lineage.derivative_kind
                     AND tombstone.derivative_id = lineage.derivative_id
               )
               AND COALESCE((
                   SELECT disposition.state
                   FROM retrieval_anchor_dispositions AS disposition
                   WHERE disposition.anchor_id = lineage.source_anchor_id
                     AND disposition.owner_json = lineage.owner_json
                   ORDER BY disposition.sequence DESC LIMIT 1
               ), 'active') = 'active'
             LIMIT 1",
            params![owner, kind.as_str(), derivative_id],
        )
        .await
        .map_err(database_error)?;
    rows.next()
        .await
        .map_err(database_error)
        .map(|row| row.is_some())
}

pub(crate) async fn tombstone_fact_derivatives_tx<E>(
    transaction: &E,
    owner: &FactOwnerV1,
    fact_id: &str,
    disposition_id: &str,
    effective_at: UtcMicros,
) -> Result<()>
where
    E: crate::db::engine::Executor,
{
    let owner = owner_json(owner)?;
    transaction
        .execute(
            "INSERT OR IGNORE INTO retrieval_anchor_derivative_tombstones (
                source_anchor_id, owner_json, derivative_kind, derivative_id,
                disposition_id, effective_at
             )
             SELECT lineage.source_anchor_id, lineage.owner_json,
                    lineage.derivative_kind, lineage.derivative_id, ?3, ?4
             FROM retrieval_anchor_reverse_lineage AS lineage
             JOIN memory_v2_evidence AS evidence
               ON evidence.anchor_id = lineage.source_anchor_id
              AND evidence.owner_json = lineage.owner_json
              AND evidence.evidence_id = lineage.derivative_id
             WHERE evidence.fact_id = ?1 AND evidence.owner_json = ?2
               AND lineage.derivative_kind = 'contribution'",
            crate::db::engine::params![fact_id, owner.as_str(), disposition_id, effective_at.0],
        )
        .await
        .map(|_| ())
        .map_err(database_error)?;
    transaction
        .execute(
            "INSERT OR IGNORE INTO retrieval_anchor_derivative_tombstones (
                source_anchor_id, owner_json, derivative_kind, derivative_id,
                disposition_id, effective_at
             )
             SELECT lineage.source_anchor_id, lineage.owner_json,
                    lineage.derivative_kind, lineage.derivative_id, ?3, ?4
             FROM retrieval_anchor_reverse_lineage AS lineage
             JOIN memory_v2_lineage_events AS event
               ON event.event_id = lineage.derivative_id
              AND event.fact_id = ?1
             WHERE lineage.owner_json = ?2
               AND lineage.derivative_kind = 'finding'",
            crate::db::engine::params![fact_id, owner.as_str(), disposition_id, effective_at.0],
        )
        .await
        .map(|_| ())
        .map_err(database_error)
}

pub(crate) async fn publish_fact_feedback_finding_tx<E>(
    transaction: &E,
    owner: &FactOwnerV1,
    fact_id: &str,
    event_id: &str,
) -> Result<()>
where
    E: crate::db::engine::Executor,
{
    let owner = owner_json(owner)?;
    transaction
        .execute(
            "INSERT OR IGNORE INTO retrieval_anchor_reverse_lineage (
                source_anchor_id, owner_json, derivative_kind, derivative_id, direct_evidence
             )
             SELECT lineage.source_anchor_id, lineage.owner_json, 'finding', ?3, 1
             FROM retrieval_anchor_reverse_lineage AS lineage
             JOIN memory_v2_evidence AS evidence
               ON evidence.anchor_id = lineage.source_anchor_id
              AND evidence.owner_json = lineage.owner_json
              AND evidence.evidence_id = lineage.derivative_id
             WHERE evidence.fact_id = ?1 AND evidence.owner_json = ?2
               AND lineage.derivative_kind = 'contribution'
               AND lineage.direct_evidence = 1
               AND NOT EXISTS (
                   SELECT 1
                   FROM retrieval_anchor_derivative_tombstones AS tombstone
                   WHERE tombstone.source_anchor_id = lineage.source_anchor_id
                     AND tombstone.owner_json = lineage.owner_json
                     AND tombstone.derivative_kind = lineage.derivative_kind
                     AND tombstone.derivative_id = lineage.derivative_id
               )
               AND COALESCE((
                   SELECT disposition.state
                   FROM retrieval_anchor_dispositions AS disposition
                   WHERE disposition.anchor_id = lineage.source_anchor_id
                     AND disposition.owner_json = lineage.owner_json
                   ORDER BY disposition.sequence DESC LIMIT 1
               ), 'active') = 'active'",
            crate::db::engine::params![fact_id, owner, event_id],
        )
        .await
        .map(|_| ())
        .map_err(database_error)
}

impl super::Database {
    pub(crate) async fn append_retrieval_anchor_disposition(
        &self,
        record: &RetrievalAnchorDispositionRecordV1,
    ) -> Result<AnchorDispositionAppendOutcomeV1> {
        record.validate()?;
        let transaction = self.begin_write_transaction(OPERATION).await?;
        let owner = owner_json(record.owner())?;
        let record_json = serde_json::to_string(record).map_err(database_error)?;
        let mut replay = transaction
            .query(
                "SELECT record_json FROM retrieval_anchor_dispositions
                 WHERE disposition_id = ?1 AND owner_json = ?2",
                params![record.disposition_id(), owner.as_str()],
            )
            .await
            .map_err(database_error)?;
        if let Some(row) = replay.next().await.map_err(database_error)? {
            let outcome = if row.get::<String>(0).map_err(database_error)? == record_json {
                AnchorDispositionAppendOutcomeV1::Replayed
            } else {
                return Err(authority_error("anchor disposition identity collision"));
            };
            drop(replay);
            transaction.commit().await?;
            return Ok(outcome);
        }
        drop(replay);
        if !disposition_transition_allowed(
            current_disposition(&transaction, record.anchor_id(), &owner).await?,
            record.state(),
        ) {
            return Err(authority_error("invalid anchor disposition transition"));
        }
        transaction
            .execute(
                "INSERT INTO retrieval_anchor_dispositions (
                    disposition_id, anchor_id, owner_json, state, superseded_by,
                    reason_class, effective_at, record_json
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    record.disposition_id(),
                    record.anchor_id().as_str(),
                    owner.as_str(),
                    record.state().as_str(),
                    record.superseded_by().map(RetrievalAnchorId::as_str),
                    record.reason_class().as_str(),
                    record.effective_at().0,
                    record_json,
                ],
            )
            .await
            .map_err(database_error)?;
        if suppresses_derivatives(record.state()) {
            transaction
                .execute(
                    "INSERT INTO retrieval_anchor_derivative_tombstones (
                        source_anchor_id, owner_json, derivative_kind, derivative_id,
                        disposition_id, effective_at
                     )
                     SELECT source_anchor_id, owner_json, derivative_kind, derivative_id, ?3, ?4
                     FROM retrieval_anchor_reverse_lineage
                     WHERE source_anchor_id = ?1 AND owner_json = ?2",
                    params![
                        record.anchor_id().as_str(),
                        owner.as_str(),
                        record.disposition_id(),
                        record.effective_at().0,
                    ],
                )
                .await
                .map_err(database_error)?;
        }
        transaction.commit().await?;
        Ok(AnchorDispositionAppendOutcomeV1::Appended)
    }

    pub(crate) async fn publish_retrieval_anchor_derivative(
        &self,
        derivative: &RetrievalAnchorDerivativeV1,
    ) -> Result<AnchorDispositionAppendOutcomeV1> {
        derivative.validate()?;
        let transaction = self.begin_write_transaction(OPERATION).await?;
        let owner = owner_json(derivative.owner())?;
        if !AnchorDispositionStateV1::serves_derivatives(
            current_disposition(&transaction, derivative.source_anchor_id(), &owner).await?,
        ) {
            return Err(authority_error(
                "cannot publish lineage from an unavailable anchor",
            ));
        }
        let changed = transaction
            .execute(
                "INSERT OR IGNORE INTO retrieval_anchor_reverse_lineage (
                    source_anchor_id, owner_json, derivative_kind, derivative_id, direct_evidence
                 ) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    derivative.source_anchor_id().as_str(),
                    owner.as_str(),
                    derivative.kind().as_str(),
                    derivative.derivative_id(),
                    i64::from(derivative.is_direct_evidence()),
                ],
            )
            .await
            .map_err(database_error)?;
        let outcome = if changed == 1 {
            AnchorDispositionAppendOutcomeV1::Appended
        } else {
            let mut rows = transaction
                .query(
                    "SELECT direct_evidence FROM retrieval_anchor_reverse_lineage
                     WHERE source_anchor_id = ?1 AND owner_json = ?2
                       AND derivative_kind = ?3 AND derivative_id = ?4",
                    params![
                        derivative.source_anchor_id().as_str(),
                        owner.as_str(),
                        derivative.kind().as_str(),
                        derivative.derivative_id(),
                    ],
                )
                .await
                .map_err(database_error)?;
            let replayed = rows
                .next()
                .await
                .map_err(database_error)?
                .is_some_and(|row| {
                    row.get::<i64>(0).ok() == Some(i64::from(derivative.is_direct_evidence()))
                });
            drop(rows);
            if !replayed {
                return Err(authority_error("anchor derivative identity collision"));
            }
            AnchorDispositionAppendOutcomeV1::Replayed
        };
        transaction.commit().await?;
        Ok(outcome)
    }

    pub(crate) async fn resolve_retrieval_anchor_derivatives<O>(
        &self,
        owner: &O,
        anchor_id: &RetrievalAnchorId,
    ) -> Result<Vec<RetrievalAnchorDerivativeV1>>
    where
        O: serde::Serialize + Clone + Into<RetrievalAnchorOwnerV1>,
    {
        let connection = self.engine_conn();
        resolve_anchor_derivatives(&connection, owner, anchor_id).await
    }

    #[cfg(test)]
    pub(crate) async fn resolve_retrieval_anchor_derivative(
        &self,
        owner: &FactOwnerV1,
        kind: AnchorDerivativeKindV1,
        derivative_id: &str,
    ) -> Result<bool> {
        let connection = self.engine_conn();
        resolve_anchor_derivative(&connection, owner, kind, derivative_id).await
    }

    pub(crate) async fn retrieval_anchor_disposition_history(
        &self,
        owner: &impl serde::Serialize,
        anchor_id: &RetrievalAnchorId,
    ) -> Result<Vec<RetrievalAnchorDispositionRecordV1>> {
        let owner = owner_json(owner)?;
        let connection = self.engine_conn();
        let mut rows = connection
            .query(
                "SELECT record_json
                 FROM retrieval_anchor_dispositions
                 WHERE anchor_id = ?1 AND owner_json = ?2
                 ORDER BY sequence ASC",
                params![anchor_id.as_str(), owner.as_str()],
            )
            .await
            .map_err(database_error)?;
        let mut history = Vec::new();
        while let Some(row) = rows.next().await.map_err(database_error)? {
            let record: RetrievalAnchorDispositionRecordV1 =
                serde_json::from_str(&row.get::<String>(0).map_err(database_error)?)
                    .map_err(database_error)?;
            record.validate()?;
            if record.anchor_id() != anchor_id || owner_json(record.owner())? != owner {
                return Err(authority_error(
                    "retrieval anchor disposition history identity mismatch",
                ));
            }
            history.push(record);
        }
        Ok(history)
    }
}

impl RetrievalAnchorDispositionStore for super::Database {
    fn append_disposition(
        &self,
        record: RetrievalAnchorDispositionRecordV1,
    ) -> impl std::future::Future<
        Output = RetrievalAnchorStoreResult<AnchorDispositionAppendOutcomeV1>,
    > + Send {
        async move {
            self.append_retrieval_anchor_disposition(&record)
                .await
                .map_err(store_error)
        }
    }

    fn publish_derivative(
        &self,
        derivative: RetrievalAnchorDerivativeV1,
    ) -> impl std::future::Future<
        Output = RetrievalAnchorStoreResult<AnchorDispositionAppendOutcomeV1>,
    > + Send {
        async move {
            self.publish_retrieval_anchor_derivative(&derivative)
                .await
                .map_err(store_error)
        }
    }

    fn current_disposition(
        &self,
        anchor_id: &RetrievalAnchorId,
        owner: &RetrievalAnchorOwnerV1,
    ) -> impl std::future::Future<
        Output = RetrievalAnchorStoreResult<Option<RetrievalAnchorDispositionRecordV1>>,
    > + Send {
        async move {
            self.retrieval_anchor_disposition_history(owner, anchor_id)
                .await
                .map(|history| history.into_iter().last())
                .map_err(store_error)
        }
    }

    fn tombstone(
        &self,
        anchor_id: &RetrievalAnchorId,
        owner: &RetrievalAnchorOwnerV1,
    ) -> impl std::future::Future<
        Output = RetrievalAnchorStoreResult<Option<RetrievalAnchorTombstoneV1>>,
    > + Send {
        async move {
            let Some(record) =
                RetrievalAnchorDispositionStore::current_disposition(self, anchor_id, owner)
                    .await?
            else {
                return Ok(None);
            };
            if !matches!(
                record.state(),
                AnchorDispositionStateV1::Redacted
                    | AnchorDispositionStateV1::Expired
                    | AnchorDispositionStateV1::Quarantined
                    | AnchorDispositionStateV1::Deleted
                    | AnchorDispositionStateV1::Unavailable
            ) {
                return Ok(None);
            }
            RetrievalAnchorTombstoneV1::new(
                record.anchor_id().clone(),
                record.owner().clone(),
                record.state(),
                record.reason_class(),
                record.effective_at(),
            )
            .map(Some)
        }
    }

    fn derivatives(
        &self,
        anchor_id: &RetrievalAnchorId,
        owner: &RetrievalAnchorOwnerV1,
    ) -> impl std::future::Future<
        Output = RetrievalAnchorStoreResult<Vec<RetrievalAnchorDerivativeV1>>,
    > + Send {
        async move {
            self.resolve_retrieval_anchor_derivatives(owner, anchor_id)
                .await
                .map_err(store_error)
        }
    }
}

fn store_error(_error: TraceDecayError) -> RetrievalAnchorStoreError {
    RetrievalAnchorStoreError::Unavailable
}

#[cfg(test)]
mod tests {
    use tracedecay_domain::{FactOwnerV1, RetrievalAnchorId, UtcMicros};
    use tracedecay_store::AnchorDispositionReasonClassV1;

    use super::{
        AnchorDerivativeKindV1, AnchorDispositionAppendOutcomeV1, AnchorDispositionStateV1,
        RetrievalAnchorDerivativeV1, RetrievalAnchorDispositionRecordV1,
    };
    use crate::db::engine::params;
    use crate::db::{Database, DatabaseAuthority, TestDatabaseRuntimeMode};

    async fn open_database(path: &std::path::Path) -> Database {
        let authority =
            DatabaseAuthority::acquire_test(path, "retrieval anchor authority acceptance").unwrap();
        let mode = if path.try_exists().unwrap() {
            TestDatabaseRuntimeMode::Existing
        } else {
            TestDatabaseRuntimeMode::Initialize
        };
        Database::publish_test_runtime(path, &authority, mode)
            .await
            .unwrap()
            .0
    }

    async fn insert_anchor(database: &Database, anchor_id: &str) {
        // Seed through the writer broker: plain conn() is sealed
        // query_only, and the owner must use the canonical internally
        // tagged FactOwnerV1 encoding or disposition FKs reject it.
        let owner = super::owner_json(&FactOwnerV1::Profile).expect("owner json");
        database
            .execute_write_engine(
                "seed retrieval anchor fixture",
                "INSERT INTO retrieval_anchors (
                    anchor_id, anchor_json, owner_json, projection_generation
                 ) VALUES (?1, '{\"target\":\"fixture\"}', ?2, 'generation-1')",
                params![anchor_id, owner],
            )
            .await
            .expect("insert anchor");
    }

    fn disposition(
        disposition_id: &str,
        anchor_id: &str,
        state: AnchorDispositionStateV1,
        at: i64,
    ) -> RetrievalAnchorDispositionRecordV1 {
        RetrievalAnchorDispositionRecordV1::new(
            disposition_id,
            RetrievalAnchorId::new(anchor_id).unwrap(),
            FactOwnerV1::Profile,
            state,
            None,
            AnchorDispositionReasonClassV1::UserRequest,
            UtcMicros(at),
        )
        .unwrap()
    }

    fn derivative(
        anchor_id: &str,
        kind: AnchorDerivativeKindV1,
        derivative_id: &str,
    ) -> RetrievalAnchorDerivativeV1 {
        RetrievalAnchorDerivativeV1::new(
            RetrievalAnchorId::new(anchor_id).unwrap(),
            FactOwnerV1::Profile,
            kind,
            derivative_id,
            true,
        )
        .unwrap()
    }

    fn superseded_disposition(
        disposition_id: &str,
        anchor_id: &str,
        successor_id: &str,
        at: i64,
    ) -> RetrievalAnchorDispositionRecordV1 {
        RetrievalAnchorDispositionRecordV1::new(
            disposition_id,
            RetrievalAnchorId::new(anchor_id).unwrap(),
            FactOwnerV1::Profile,
            AnchorDispositionStateV1::Superseded,
            Some(RetrievalAnchorId::new(successor_id).unwrap()),
            AnchorDispositionReasonClassV1::Correction,
            UtcMicros(at),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn disposition_replay_survives_restart_without_resurrection() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("anchors.db");
        let database = open_database(&path).await;
        insert_anchor(&database, "anchor-1").await;
        let deleted = disposition(
            "disposition-delete-1",
            "anchor-1",
            AnchorDispositionStateV1::Deleted,
            10,
        );
        assert_eq!(
            database
                .append_retrieval_anchor_disposition(&deleted)
                .await
                .unwrap(),
            AnchorDispositionAppendOutcomeV1::Appended
        );
        drop(database);

        let database = open_database(&path).await;
        assert_eq!(
            database
                .append_retrieval_anchor_disposition(&deleted)
                .await
                .unwrap(),
            AnchorDispositionAppendOutcomeV1::Replayed
        );
        assert!(
            database
                .append_retrieval_anchor_disposition(&disposition(
                    "disposition-delete-1",
                    "anchor-1",
                    AnchorDispositionStateV1::Unavailable,
                    10,
                ))
                .await
                .is_err()
        );
        assert!(
            database
                .append_retrieval_anchor_disposition(&disposition(
                    "disposition-active-2",
                    "anchor-1",
                    AnchorDispositionStateV1::Active,
                    11,
                ))
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn deletion_tombstones_reverse_lineage_for_every_derivative_kind() {
        let directory = tempfile::tempdir().unwrap();
        let database = open_database(&directory.path().join("anchors.db")).await;
        insert_anchor(&database, "anchor-1").await;
        for (kind, id) in [
            (AnchorDerivativeKindV1::Span, "span-1"),
            (AnchorDerivativeKindV1::Contribution, "contribution-1"),
            (AnchorDerivativeKindV1::Finding, "finding-1"),
        ] {
            database
                .publish_retrieval_anchor_derivative(&derivative("anchor-1", kind, id))
                .await
                .unwrap();
        }
        assert_eq!(
            database
                .resolve_retrieval_anchor_derivatives(
                    &FactOwnerV1::Profile,
                    &RetrievalAnchorId::new("anchor-1").unwrap(),
                )
                .await
                .unwrap()
                .len(),
            3
        );
        database
            .append_retrieval_anchor_disposition(&disposition(
                "disposition-unavailable-1",
                "anchor-1",
                AnchorDispositionStateV1::Unavailable,
                8,
            ))
            .await
            .unwrap();
        assert!(
            database
                .resolve_retrieval_anchor_derivatives(
                    &FactOwnerV1::Profile,
                    &RetrievalAnchorId::new("anchor-1").unwrap(),
                )
                .await
                .unwrap()
                .is_empty()
        );
        database
            .append_retrieval_anchor_disposition(&disposition(
                "disposition-active-1",
                "anchor-1",
                AnchorDispositionStateV1::Active,
                9,
            ))
            .await
            .unwrap();

        database
            .append_retrieval_anchor_disposition(&disposition(
                "disposition-delete-1",
                "anchor-1",
                AnchorDispositionStateV1::Deleted,
                10,
            ))
            .await
            .unwrap();
        assert!(
            database
                .resolve_retrieval_anchor_derivatives(
                    &FactOwnerV1::Profile,
                    &RetrievalAnchorId::new("anchor-1").unwrap(),
                )
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            database
                .conn()
                .execute(
                    "UPDATE retrieval_anchor_dispositions
                     SET reason_class = 'rewritten'
                     WHERE disposition_id = 'disposition-delete-1'",
                    (),
                )
                .await
                .is_err()
        );
        assert!(
            database
                .conn()
                .execute(
                    "DELETE FROM retrieval_anchor_reverse_lineage
                     WHERE source_anchor_id = 'anchor-1'",
                    (),
                )
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn retained_direct_evidence_keeps_a_derivative_servable() {
        let directory = tempfile::tempdir().unwrap();
        let database = open_database(&directory.path().join("anchors.db")).await;
        insert_anchor(&database, "anchor-1").await;
        insert_anchor(&database, "anchor-2").await;
        let first = derivative(
            "anchor-1",
            AnchorDerivativeKindV1::Contribution,
            "contribution-shared",
        );
        let second = derivative(
            "anchor-2",
            AnchorDerivativeKindV1::Contribution,
            "contribution-shared",
        );
        database
            .publish_retrieval_anchor_derivative(&first)
            .await
            .unwrap();
        database
            .publish_retrieval_anchor_derivative(&second)
            .await
            .unwrap();
        database
            .append_retrieval_anchor_disposition(&disposition(
                "disposition-delete-1",
                "anchor-1",
                AnchorDispositionStateV1::Deleted,
                10,
            ))
            .await
            .unwrap();

        assert!(
            database
                .resolve_retrieval_anchor_derivative(
                    &FactOwnerV1::Profile,
                    AnchorDerivativeKindV1::Contribution,
                    "contribution-shared",
                )
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn only_active_direct_evidence_can_serve_a_derivative() {
        let directory = tempfile::tempdir().unwrap();
        let database = open_database(&directory.path().join("anchors.db")).await;
        insert_anchor(&database, "anchor-1").await;
        let indirect = RetrievalAnchorDerivativeV1::new(
            RetrievalAnchorId::new("anchor-1").unwrap(),
            FactOwnerV1::Profile,
            AnchorDerivativeKindV1::Contribution,
            "contribution-indirect",
            false,
        )
        .unwrap();
        database
            .publish_retrieval_anchor_derivative(&indirect)
            .await
            .unwrap();

        assert_eq!(
            database
                .resolve_retrieval_anchor_derivatives(
                    &FactOwnerV1::Profile,
                    &RetrievalAnchorId::new("anchor-1").unwrap(),
                )
                .await
                .unwrap(),
            vec![indirect.clone()]
        );
        assert!(
            !database
                .resolve_retrieval_anchor_derivative(
                    &FactOwnerV1::Profile,
                    AnchorDerivativeKindV1::Contribution,
                    "contribution-indirect",
                )
                .await
                .unwrap()
        );
        assert!(
            database
                .publish_retrieval_anchor_derivative(
                    &RetrievalAnchorDerivativeV1::new(
                        RetrievalAnchorId::new("anchor-1").unwrap(),
                        FactOwnerV1::Profile,
                        AnchorDerivativeKindV1::Contribution,
                        "contribution-indirect",
                        true,
                    )
                    .unwrap(),
                )
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn production_authority_preserves_history_and_suppresses_superseded_sources() {
        let directory = tempfile::tempdir().unwrap();
        let database = open_database(&directory.path().join("anchors.db")).await;
        insert_anchor(&database, "anchor-1").await;
        insert_anchor(&database, "anchor-2").await;
        let span_derivative =
            derivative("anchor-1", AnchorDerivativeKindV1::Span, "span-superseded");
        database
            .publish_retrieval_anchor_derivative(&span_derivative)
            .await
            .unwrap();

        for record in [
            disposition(
                "disposition-unavailable",
                "anchor-1",
                AnchorDispositionStateV1::Unavailable,
                1,
            ),
            disposition(
                "disposition-active",
                "anchor-1",
                AnchorDispositionStateV1::Active,
                2,
            ),
            superseded_disposition("disposition-superseded", "anchor-1", "anchor-2", 3),
        ] {
            assert_eq!(
                database
                    .append_retrieval_anchor_disposition(&record)
                    .await
                    .unwrap(),
                AnchorDispositionAppendOutcomeV1::Appended
            );
        }

        assert!(
            !database
                .resolve_retrieval_anchor_derivative(
                    &FactOwnerV1::Profile,
                    AnchorDerivativeKindV1::Span,
                    "span-superseded",
                )
                .await
                .unwrap()
        );
        assert!(
            database
                .publish_retrieval_anchor_derivative(&derivative(
                    "anchor-1",
                    AnchorDerivativeKindV1::Finding,
                    "finding-after-supersession",
                ))
                .await
                .is_err()
        );

        database
            .append_retrieval_anchor_disposition(&disposition(
                "disposition-deleted",
                "anchor-1",
                AnchorDispositionStateV1::Deleted,
                4,
            ))
            .await
            .unwrap();
        let history = database
            .retrieval_anchor_disposition_history(
                &FactOwnerV1::Profile,
                &RetrievalAnchorId::new("anchor-1").unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            history
                .iter()
                .map(RetrievalAnchorDispositionRecordV1::state)
                .collect::<Vec<_>>(),
            vec![
                AnchorDispositionStateV1::Unavailable,
                AnchorDispositionStateV1::Active,
                AnchorDispositionStateV1::Superseded,
                AnchorDispositionStateV1::Deleted,
            ]
        );
    }
}
