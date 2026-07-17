//! Database-backed authority for append-only facts, evidence, and provenance.

use std::error::Error;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::db::Database;
use libsql::{Transaction, params};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tracedecay_domain::{
    ActorId, Confidence, FactAssertionId, FactAssertionKindV1, FactAssertionV1,
    FactCategoryV1, FactCurationActionV1, FactEventId, FactEvidenceId, FactId,
    FactIdentityMaterialV1, FactIdentitySourceV1, FactLineageEventKindV1, FactLineageEventV1,
    FactOwnerV1, FactPayloadV1, LegacyFactMappingV1, PayloadAccessState, ProvenanceId,
    RetentionClass, RetrievalAnchorId, RetrievalAnchorRecordV2, SanitizerDispositionV1,
    SourceStoreId, UtcMicros, VectorWatermark,
};
use tracedecay_store::{
    CompatibilityFactAddCommandV1, CompatibilityFactAddDispositionV1,
    CompatibilityFactAddOutcomeV1, CompatibilityFactAvailabilityV1,
    CompatibilityFactContradictionPageV1, CompatibilityFactContradictionQueryV1,
    CompatibilityFactFeedbackActionV1, CompatibilityFactFeedbackCommandV1,
    CompatibilityFactFeedbackOutcomeV1, CompatibilityFactHistoryQueryV1,
    CompatibilityFactHistoryV1, CompatibilityFactIdV1, CompatibilityFactInspectionV1,
    CompatibilityFactListQueryV1, CompatibilityFactMappingV1, CompatibilityFactPageV1,
    CompatibilityFactProjectionV1, CompatibilityFactProposalImportReceiptV1,
    CompatibilityFactProposalImportV1, CompatibilityFactProposalPageV1,
    CompatibilityFactProposalPromotionV1, CompatibilityFactProposalRecordV1,
    CompatibilityFactProposalRevisionV1, CompatibilityFactProposalStateV1,
    CompatibilityFactRemoveCommandV1, CompatibilityFactRemoveOutcomeV1,
    CompatibilityFactRetrievalCommandV1, CompatibilityFactSearchCursorV1,
    CompatibilityFactSearchHitV1, CompatibilityFactSearchKindV1, CompatibilityFactSearchPageV1,
    CompatibilityFactSearchQuery, CompatibilityFactSearchScoresV1, CompatibilityFactSourceV1,
    CompatibilityFactStatusV1, CompatibilityFactTargetV1, CompatibilityFactTelemetryV1,
    CompatibilityFactUnavailableV1, CompatibilityFactUpdateCommandV1,
    CompatibilityFactUpdateOutcomeV1, CompatibilityFactV1, CompatibilityMemoryAlgebraV1,
    CompatibilityMemoryFeedbackFunnelV1, CompatibilityMemoryRepairStatsV1,
    CompatibilityMemoryStatusV1, CompatibilityProjectionStateV1, CurrentFactsQuery,
    FactAsOfQuery, FactCommitConflict, FactCommitOutcome, FactCommitReceipt,
    FactCompatibilityResult, FactCompatibilityStore, FactCompatibilityStoreError,
    FactCurrentQuery, FactLineageCursor, FactLineageQuery, FactProposalPromotionStateV1,
    FactProposalStore, FactProposalStoreError, FactStore, FactStoreError, FactStoreResult,
    FactWriteBatch, LegacyFactQuery, PromoteFactProposal, PromoteFactProposalOutcome,
    RetrievalAnchorQuery, StoredFactV1,
};

const COMMIT_OPERATION: &str = "commit canonical memory fact";
const QUERY_OPERATION: &str = "query canonical memory facts";
const PROMOTE_OPERATION: &str = "promote canonical memory proposal";
const DEFAULT_TRUST: f64 = 0.5;
const COMPATIBILITY_RETENTION_CLASS: &str = "compatibility-runtime-v1";
const COMPATIBILITY_SOURCE_STORE: &str = "compatibility-runtime-v1";

/// Canonical fact authority over one already-open, authority-bound database.
///
/// This adapter never resolves a path or opens a database. All write and read
/// transactions are delegated to the retained [`Database`] authority.
pub struct DatabaseFactStore<'a> {
    db: &'a Database,
}

impl<'a> DatabaseFactStore<'a> {
    pub const fn new(db: &'a Database) -> Self {
        Self { db }
    }

    async fn commit_batch(&self, batch: &FactWriteBatch) -> FactStoreResult<FactCommitOutcome> {
        let transaction = self
            .db
            .begin_write_transaction(COMMIT_OPERATION)
            .await
            .map_err(|error| storage_error(COMMIT_OPERATION, error))?;
        let attempt = match commit_fact_tx(&transaction, batch).await {
            Ok(attempt) => attempt,
            Err(error) => {
                return match transaction.rollback().await {
                    Ok(()) => Err(error),
                    Err(rollback) => Err(storage_error(
                        COMMIT_OPERATION,
                        std::io::Error::other(format!(
                            "{error}; transaction rollback also failed and writer connection was retired: {rollback}"
                        )),
                    )),
                };
            }
        };
        if attempt.wrote {
            transaction
                .commit()
                .await
                .map_err(|error| storage_error(COMMIT_OPERATION, error))?;
        } else {
            transaction
                .rollback()
                .await
                .map_err(|error| storage_error(COMMIT_OPERATION, error))?;
        }
        Ok(attempt.outcome)
    }
}

async fn finish_read_snapshot<T>(
    snapshot: Transaction,
    result: FactStoreResult<T>,
) -> FactStoreResult<T> {
    match result {
        Ok(value) => {
            snapshot
                .commit()
                .await
                .map_err(|error| storage_error(QUERY_OPERATION, error))?;
            Ok(value)
        }
        Err(error) => match snapshot.rollback().await {
            Ok(()) => Err(error),
            Err(rollback) => Err(storage_error(
                QUERY_OPERATION,
                std::io::Error::other(format!(
                    "{error}; read snapshot rollback also failed: {rollback}"
                )),
            )),
        },
    }
}

impl FactStore for DatabaseFactStore<'_> {
    async fn commit_fact(&self, batch: FactWriteBatch) -> FactStoreResult<FactCommitOutcome> {
        self.commit_batch(&batch).await
    }

    async fn query_current_facts(
        &self,
        query: CurrentFactsQuery,
    ) -> FactStoreResult<Vec<StoredFactV1>> {
        let snapshot = self
            .db
            .begin_isolated_read_snapshot(QUERY_OPERATION)
            .await
            .map_err(|error| storage_error(QUERY_OPERATION, error))?;
        let result = query_current_facts_tx(&snapshot, &query).await;
        finish_read_snapshot(snapshot, result).await
    }

    async fn query_fact_current(
        &self,
        query: FactCurrentQuery,
    ) -> FactStoreResult<Option<StoredFactV1>> {
        let snapshot = self
            .db
            .begin_isolated_read_snapshot(QUERY_OPERATION)
            .await
            .map_err(|error| storage_error(QUERY_OPERATION, error))?;
        let result = query_fact_current_tx(&snapshot, query.owner(), query.fact_id()).await;
        finish_read_snapshot(snapshot, result).await
    }

    async fn query_fact_as_of(
        &self,
        query: FactAsOfQuery,
    ) -> FactStoreResult<Option<StoredFactV1>> {
        let snapshot = self
            .db
            .begin_isolated_read_snapshot(QUERY_OPERATION)
            .await
            .map_err(|error| storage_error(QUERY_OPERATION, error))?;
        let result = query_fact_as_of_tx(&snapshot, &query).await;
        finish_read_snapshot(snapshot, result).await
    }

    async fn query_fact_lineage(
        &self,
        query: FactLineageQuery,
    ) -> FactStoreResult<Vec<FactLineageEventV1>> {
        let snapshot = self
            .db
            .begin_isolated_read_snapshot(QUERY_OPERATION)
            .await
            .map_err(|error| storage_error(QUERY_OPERATION, error))?;
        let result = query_fact_lineage_tx(&snapshot, &query).await;
        finish_read_snapshot(snapshot, result).await
    }

    async fn resolve_legacy_fact(&self, query: LegacyFactQuery) -> FactStoreResult<Option<FactId>> {
        let snapshot = self
            .db
            .begin_isolated_read_snapshot(QUERY_OPERATION)
            .await
            .map_err(|error| storage_error(QUERY_OPERATION, error))?;
        let result = resolve_legacy_fact_tx(&snapshot, &query).await;
        finish_read_snapshot(snapshot, result).await
    }

    async fn get_retrieval_anchor(
        &self,
        query: RetrievalAnchorQuery,
    ) -> FactStoreResult<Option<RetrievalAnchorRecordV2>> {
        let snapshot = self
            .db
            .begin_isolated_read_snapshot(QUERY_OPERATION)
            .await
            .map_err(|error| storage_error(QUERY_OPERATION, error))?;
        let result = get_retrieval_anchor_tx(&snapshot, &query).await;
        finish_read_snapshot(snapshot, result).await
    }
}

impl FactProposalStore for DatabaseFactStore<'_> {
    async fn promote_fact_proposal(
        &self,
        promotion: PromoteFactProposal,
    ) -> Result<PromoteFactProposalOutcome, FactProposalStoreError> {
        let transaction = self
            .db
            .begin_write_transaction(PROMOTE_OPERATION)
            .await
            .map_err(|error| authority_storage_error(PROMOTE_OPERATION, error))?;
        let outcome = match promote_fact_proposal_tx(&transaction, &promotion).await {
            Ok(outcome) => outcome,
            Err(error) => {
                return match transaction.rollback().await {
                    Ok(()) => Err(error),
                    Err(rollback) => Err(authority_storage_error(
                        PROMOTE_OPERATION,
                        std::io::Error::other(format!(
                            "{error}; transaction rollback also failed: {rollback}"
                        )),
                    )),
                };
            }
        };
        if outcome.wrote {
            transaction
                .commit()
                .await
                .map_err(|error| authority_storage_error(PROMOTE_OPERATION, error))?;
        } else {
            transaction
                .rollback()
                .await
                .map_err(|error| authority_storage_error(PROMOTE_OPERATION, error))?;
        }
        Ok(outcome.outcome)
    }
}

struct CommitAttempt {
    outcome: FactCommitOutcome,
    wrote: bool,
}

struct PromotionAttempt {
    outcome: PromoteFactProposalOutcome,
    wrote: bool,
}

#[derive(Clone)]
struct OwnerKey {
    kind: &'static str,
    project_id: String,
    json: String,
}

impl OwnerKey {
    fn new(owner: &FactOwnerV1) -> FactStoreResult<Self> {
        let (kind, project_id) = match owner {
            FactOwnerV1::Profile => ("profile", String::new()),
            FactOwnerV1::Project { project_id } => ("project", project_id.as_str().to_owned()),
        };
        Ok(Self {
            kind,
            project_id,
            json: to_json(owner, "serialize fact owner")?,
        })
    }
}

/// The immutable assertion record deliberately excludes `FactPayloadV1`.
/// Payload bytes belong only in `memory_v2_assertion_payloads`, which is the
/// storage locus erased when an access transition reaches `Deleted`.
#[derive(Serialize)]
struct StoredAssertionHeaderV1<'a> {
    assertion_id: &'a FactAssertionId,
    fact_id: &'a FactId,
    owner: &'a FactOwnerV1,
    kind: &'a FactAssertionKindV1,
    payload_reference: &'a tracedecay_domain::PayloadReferenceV1,
    evidence: &'a [tracedecay_domain::FactEvidenceRefV1],
    asserted_at: UtcMicros,
    actor_id: Option<&'a tracedecay_domain::ActorId>,
}

fn assertion_header_json(assertion: &FactAssertionV1) -> FactStoreResult<String> {
    let payload_reference = assertion.payload().payload_reference()?;
    to_json(
        &StoredAssertionHeaderV1 {
            assertion_id: assertion.assertion_id(),
            fact_id: assertion.fact_id(),
            owner: assertion.owner(),
            kind: assertion.kind(),
            payload_reference: &payload_reference,
            evidence: assertion.evidence(),
            asserted_at: assertion.asserted_at(),
            actor_id: assertion.actor_id(),
        },
        "serialize payload-free fact assertion header",
    )
}

async fn commit_fact_tx(
    transaction: &Transaction,
    batch: &FactWriteBatch,
) -> FactStoreResult<CommitAttempt> {
    let owner = OwnerKey::new(batch.owner())?;
    let actual_last = current_last_event(transaction, &owner, batch.fact_id()).await?;
    if batch_is_exact_replay(transaction, &owner, batch, actual_last.as_ref()).await? {
        return Ok(CommitAttempt {
            outcome: receipt_outcome(transaction, &owner, batch, true).await?,
            wrote: false,
        });
    }
    if let Some(conflict) = batch_identity_collision(transaction, &owner, batch).await? {
        return Ok(CommitAttempt {
            outcome: FactCommitOutcome::Conflict(conflict),
            wrote: false,
        });
    }
    if actual_last.as_ref() != batch.expected_last_event_id() {
        return Ok(CommitAttempt {
            outcome: FactCommitOutcome::Conflict(FactCommitConflict::LastEventMismatch {
                expected: batch.expected_last_event_id().cloned(),
                actual: actual_last,
            }),
            wrote: false,
        });
    }
    ensure_append_order(transaction, &owner, batch, actual_last.as_ref()).await?;

    ensure_fact_identity(transaction, &owner, batch).await?;
    ensure_referenced_anchors(transaction, &owner, batch).await?;
    for anchor in batch.new_anchors() {
        insert_or_verify_anchor(transaction, &owner, anchor).await?;
    }
    if let Some(assertion) = batch.assertion() {
        insert_assertion(transaction, &owner, assertion).await?;
    }
    if let Some(mapping) = batch.legacy_mapping() {
        insert_legacy_mapping(transaction, &owner, mapping).await?;
    }
    for event in batch.events() {
        ensure_event_references(transaction, &owner, event).await?;
    }
    for event in batch.events() {
        insert_event(transaction, &owner, event).await?;
    }
    publish_current_projection(transaction, &owner, batch).await?;

    Ok(CommitAttempt {
        outcome: receipt_outcome(transaction, &owner, batch, false).await?,
        wrote: true,
    })
}

async fn current_last_event(
    transaction: &Transaction,
    owner: &OwnerKey,
    fact_id: &FactId,
) -> FactStoreResult<Option<FactEventId>> {
    let mut rows = transaction
        .query(
            "SELECT last_event_id FROM memory_v2_current_facts
             WHERE fact_id = ?1 AND owner_kind = ?2 AND project_id = ?3",
            params![fact_id.as_str(), owner.kind, owner.project_id.as_str()],
        )
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?
    else {
        return Ok(None);
    };
    Ok(Some(FactEventId::new(row_string(
        &row,
        0,
        QUERY_OPERATION,
    )?)?))
}

async fn ensure_append_order(
    transaction: &Transaction,
    owner: &OwnerKey,
    batch: &FactWriteBatch,
    actual_last: Option<&FactEventId>,
) -> FactStoreResult<()> {
    let Some(last_event_id) = actual_last else {
        return Ok(());
    };
    let first = batch.events().first().ok_or(FactStoreError::EmptyBatch)?;
    let mut rows = transaction
        .query(
            "SELECT occurred_at, event_id FROM memory_v2_lineage_events
             WHERE event_id = ?1 AND fact_id = ?2
               AND owner_kind = ?3 AND project_id = ?4",
            params![
                last_event_id.as_str(),
                batch.fact_id().as_str(),
                owner.kind,
                owner.project_id.as_str(),
            ],
        )
        .await
        .map_err(|error| storage_error(COMMIT_OPERATION, error))?;
    let row = rows
        .next()
        .await
        .map_err(|error| storage_error(COMMIT_OPERATION, error))?
        .ok_or_else(|| storage_message(COMMIT_OPERATION, "current fact points at missing event"))?;
    let last = (
        UtcMicros(row_i64(&row, 0, COMMIT_OPERATION)?),
        FactEventId::new(row_string(&row, 1, COMMIT_OPERATION)?)?,
    );
    if (first.occurred_at(), first.event_id()) <= (last.0, &last.1) {
        return Err(FactStoreError::EventsOutOfOrder);
    }
    Ok(())
}

async fn batch_is_exact_replay(
    transaction: &Transaction,
    owner: &OwnerKey,
    batch: &FactWriteBatch,
    actual_last: Option<&FactEventId>,
) -> FactStoreResult<bool> {
    if actual_last != batch.events().last().map(FactLineageEventV1::event_id) {
        return Ok(false);
    }
    if !fact_identity_matches(transaction, owner, batch).await? {
        return Ok(false);
    }
    for anchor in batch.new_anchors() {
        if !anchor_matches(transaction, owner, anchor).await? {
            return Ok(false);
        }
    }
    if let Some(assertion) = batch.assertion()
        && !assertion_matches(transaction, owner, assertion).await?
    {
        return Ok(false);
    }
    if let Some(mapping) = batch.legacy_mapping()
        && !legacy_mapping_matches(transaction, owner, mapping).await?
    {
        return Ok(false);
    }
    for event in batch.events() {
        if !event_matches(transaction, owner, event).await? {
            return Ok(false);
        }
    }
    Ok(true)
}

async fn batch_identity_collision(
    transaction: &Transaction,
    owner: &OwnerKey,
    batch: &FactWriteBatch,
) -> FactStoreResult<Option<FactCommitConflict>> {
    if fact_exists(transaction, batch.fact_id()).await?
        && !fact_identity_matches(transaction, owner, batch).await?
    {
        return Ok(Some(collision("fact", batch.fact_id().as_str())));
    }
    for anchor in batch.new_anchors() {
        if anchor_exists(transaction, anchor.anchor_id()).await?
            && !anchor_matches(transaction, owner, anchor).await?
        {
            return Ok(Some(collision(
                "retrieval anchor",
                anchor.anchor_id().as_str(),
            )));
        }
    }
    if let Some(assertion) = batch.assertion()
        && assertion_exists(transaction, assertion.assertion_id()).await?
        && !assertion_matches(transaction, owner, assertion).await?
    {
        return Ok(Some(collision(
            "assertion",
            assertion.assertion_id().as_str(),
        )));
    }
    if let Some(mapping) = batch.legacy_mapping()
        && legacy_mapping_exists(transaction, owner, mapping).await?
        && !legacy_mapping_matches(transaction, owner, mapping).await?
    {
        return Ok(Some(collision(
            "legacy mapping",
            mapping.fact_id().as_str(),
        )));
    }
    for event in batch.events() {
        if event_exists(transaction, event.event_id()).await?
            && !event_matches(transaction, owner, event).await?
        {
            return Ok(Some(collision("event", event.event_id().as_str())));
        }
    }
    Ok(None)
}

fn collision(kind: &'static str, id: &str) -> FactCommitConflict {
    FactCommitConflict::IdentityCollision {
        kind,
        id: id.to_owned(),
    }
}

async fn fact_exists(transaction: &Transaction, fact_id: &FactId) -> FactStoreResult<bool> {
    row_exists(
        transaction,
        "SELECT 1 FROM memory_v2_facts WHERE fact_id = ?1",
        [fact_id.as_str()],
    )
    .await
}

async fn fact_identity_matches(
    transaction: &Transaction,
    owner: &OwnerKey,
    batch: &FactWriteBatch,
) -> FactStoreResult<bool> {
    let mut rows = transaction
        .query(
            "SELECT owner_kind, project_id, owner_json, identity_json
             FROM memory_v2_facts WHERE fact_id = ?1",
            [batch.fact_id().as_str()],
        )
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?
    else {
        return Ok(false);
    };
    let identity_matches = match batch.identity_material() {
        Some(identity) => {
            row_string(&row, 3, QUERY_OPERATION)? == to_json(identity, "serialize fact identity")?
        }
        None => true,
    };
    Ok(row_string(&row, 0, QUERY_OPERATION)? == owner.kind
        && row_string(&row, 1, QUERY_OPERATION)? == owner.project_id
        && row_string(&row, 2, QUERY_OPERATION)? == owner.json
        && identity_matches)
}

async fn ensure_referenced_anchors(
    transaction: &Transaction,
    owner: &OwnerKey,
    batch: &FactWriteBatch,
) -> FactStoreResult<()> {
    for anchor_id in batch.referenced_anchor_ids() {
        let mut rows = transaction
            .query(
                "SELECT 1 FROM retrieval_anchors
                 WHERE anchor_id = ?1 AND owner_json = ?2",
                params![anchor_id.as_str(), owner.json.as_str()],
            )
            .await
            .map_err(|error| storage_error(COMMIT_OPERATION, error))?;
        let Some(_row) = rows
            .next()
            .await
            .map_err(|error| storage_error(COMMIT_OPERATION, error))?
        else {
            return Err(FactStoreError::MissingEvidenceAnchor {
                anchor_id: anchor_id.clone(),
            });
        };
    }
    Ok(())
}

async fn insert_or_verify_anchor(
    transaction: &Transaction,
    owner: &OwnerKey,
    anchor: &RetrievalAnchorRecordV2,
) -> FactStoreResult<()> {
    if anchor_exists(transaction, anchor.anchor_id()).await? {
        if anchor_matches(transaction, owner, anchor).await? {
            return Ok(());
        }
        return Err(storage_message(
            COMMIT_OPERATION,
            "retrieval anchor identity collision",
        ));
    }
    transaction
        .execute(
            "INSERT INTO retrieval_anchors(
                anchor_id, anchor_json, owner_json, projection_generation
             ) VALUES(?1, ?2, ?3, ?4)",
            params![
                anchor.anchor_id().as_str(),
                to_json(anchor, "serialize retrieval anchor")?,
                owner.json.as_str(),
                anchor.projection_generation().as_str(),
            ],
        )
        .await
        .map_err(|error| storage_error(COMMIT_OPERATION, error))?;
    for alias in anchor.aliases() {
        transaction
            .execute(
                "INSERT INTO retrieval_anchor_aliases(
                    owner_json, alias_kind, locator_digest, anchor_id
                 ) VALUES(?1, ?2, ?3, ?4)",
                params![
                    owner.json.as_str(),
                    to_json(&alias.kind(), "serialize anchor alias kind")?,
                    to_json(alias.locator_digest(), "serialize anchor locator digest")?,
                    anchor.anchor_id().as_str(),
                ],
            )
            .await
            .map_err(|error| storage_error(COMMIT_OPERATION, error))?;
    }
    Ok(())
}

async fn anchor_exists(
    transaction: &Transaction,
    anchor_id: &RetrievalAnchorId,
) -> FactStoreResult<bool> {
    row_exists(
        transaction,
        "SELECT 1 FROM retrieval_anchors WHERE anchor_id = ?1",
        [anchor_id.as_str()],
    )
    .await
}

async fn anchor_matches(
    transaction: &Transaction,
    owner: &OwnerKey,
    anchor: &RetrievalAnchorRecordV2,
) -> FactStoreResult<bool> {
    let mut rows = transaction
        .query(
            "SELECT anchor_json, owner_json, projection_generation
             FROM retrieval_anchors WHERE anchor_id = ?1",
            [anchor.anchor_id().as_str()],
        )
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?
    else {
        return Ok(false);
    };
    if row_string(&row, 0, QUERY_OPERATION)? != to_json(anchor, "serialize retrieval anchor")?
        || row_string(&row, 1, QUERY_OPERATION)? != owner.json
        || row_string(&row, 2, QUERY_OPERATION)? != anchor.projection_generation().as_str()
    {
        return Ok(false);
    }
    let mut aliases = transaction
        .query(
            "SELECT alias_kind, locator_digest FROM retrieval_anchor_aliases
             WHERE anchor_id = ?1 ORDER BY alias_kind, locator_digest",
            [anchor.anchor_id().as_str()],
        )
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?;
    let mut stored = Vec::new();
    while let Some(row) = aliases
        .next()
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?
    {
        stored.push((
            row_string(&row, 0, QUERY_OPERATION)?,
            row_string(&row, 1, QUERY_OPERATION)?,
        ));
    }
    let mut expected = anchor
        .aliases()
        .iter()
        .map(|alias| {
            Ok((
                to_json(&alias.kind(), "serialize anchor alias kind")?,
                to_json(alias.locator_digest(), "serialize anchor locator digest")?,
            ))
        })
        .collect::<FactStoreResult<Vec<_>>>()?;
    expected.sort();
    Ok(stored == expected)
}

async fn insert_assertion(
    transaction: &Transaction,
    owner: &OwnerKey,
    assertion: &FactAssertionV1,
) -> FactStoreResult<()> {
    if assertion_exists(transaction, assertion.assertion_id()).await? {
        if assertion_matches(transaction, owner, assertion).await? {
            return Ok(());
        }
        return Err(storage_message(
            COMMIT_OPERATION,
            "assertion identity collision",
        ));
    }
    let header_json = assertion_header_json(assertion)?;
    let actor_id = assertion.actor_id().map(ToString::to_string);
    transaction
        .execute(
            "INSERT INTO memory_v2_assertions(
                assertion_id, fact_id, owner_kind, project_id, owner_json,
                assertion_header_json, kind_json, payload_reference_json,
                receipt_json, asserted_at, actor_id
             ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                assertion.assertion_id().as_str(),
                assertion.fact_id().as_str(),
                owner.kind,
                owner.project_id.as_str(),
                owner.json.as_str(),
                header_json,
                to_json(assertion.kind(), "serialize assertion kind")?,
                to_json(
                    &assertion.payload().payload_reference()?,
                    "serialize assertion payload reference",
                )?,
                to_json(assertion.payload().receipt(), "serialize assertion receipt")?,
                assertion.asserted_at().0,
                actor_id,
            ],
        )
        .await
        .map_err(|error| storage_error(COMMIT_OPERATION, error))?;

    for (ordinal, superseded) in superseded_assertions(assertion.kind()).iter().enumerate() {
        transaction
            .execute(
                "INSERT INTO memory_v2_assertion_supersession(
                    assertion_id, fact_id, owner_kind, project_id,
                    superseded_assertion_id, ordinal
                 ) VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    assertion.assertion_id().as_str(),
                    assertion.fact_id().as_str(),
                    owner.kind,
                    owner.project_id.as_str(),
                    superseded.as_str(),
                    ordinal as i64,
                ],
            )
            .await
            .map_err(|error| storage_error(COMMIT_OPERATION, error))?;
    }

    transaction
        .execute(
            "INSERT INTO memory_v2_assertion_payloads(
                assertion_id, fact_id, owner_kind, project_id, payload_json, content
             ) VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                assertion.assertion_id().as_str(),
                assertion.fact_id().as_str(),
                owner.kind,
                owner.project_id.as_str(),
                to_json(assertion.payload(), "serialize assertion payload")?,
                assertion.payload().content(),
            ],
        )
        .await
        .map_err(|error| storage_error(COMMIT_OPERATION, error))?;

    for (ordinal, evidence) in assertion.evidence().iter().enumerate() {
        let evidence_json = to_json(evidence, "serialize fact evidence")?;
        let changed = transaction
            .execute(
                "INSERT OR IGNORE INTO memory_v2_evidence(
                    evidence_id, fact_id, owner_kind, project_id,
                    owner_json, anchor_id, evidence_json
                 ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    evidence.evidence_id().as_str(),
                    assertion.fact_id().as_str(),
                    owner.kind,
                    owner.project_id.as_str(),
                    owner.json.as_str(),
                    evidence.anchor_id().as_str(),
                    evidence_json.as_str(),
                ],
            )
            .await
            .map_err(|error| storage_error(COMMIT_OPERATION, error))?;
        if changed == 0 {
            let mut rows = transaction
                .query(
                    "SELECT evidence_json, owner_json, anchor_id
                     FROM memory_v2_evidence
                     WHERE evidence_id = ?1 AND fact_id = ?2
                       AND owner_kind = ?3 AND project_id = ?4",
                    params![
                        evidence.evidence_id().as_str(),
                        assertion.fact_id().as_str(),
                        owner.kind,
                        owner.project_id.as_str(),
                    ],
                )
                .await
                .map_err(|error| storage_error(COMMIT_OPERATION, error))?;
            let Some(row) = rows
                .next()
                .await
                .map_err(|error| storage_error(COMMIT_OPERATION, error))?
            else {
                return Err(storage_message(
                    COMMIT_OPERATION,
                    "evidence insert disappeared",
                ));
            };
            if row_string(&row, 0, COMMIT_OPERATION)? != evidence_json
                || row_string(&row, 1, COMMIT_OPERATION)? != owner.json
                || row_string(&row, 2, COMMIT_OPERATION)? != evidence.anchor_id().as_str()
            {
                return Err(storage_message(
                    COMMIT_OPERATION,
                    "evidence identity collision",
                ));
            }
        }
        transaction
            .execute(
                "INSERT INTO memory_v2_assertion_evidence(
                    assertion_id, evidence_id, fact_id, owner_kind, project_id, ordinal
                 ) VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    assertion.assertion_id().as_str(),
                    evidence.evidence_id().as_str(),
                    assertion.fact_id().as_str(),
                    owner.kind,
                    owner.project_id.as_str(),
                    ordinal as i64,
                ],
            )
            .await
            .map_err(|error| storage_error(COMMIT_OPERATION, error))?;
    }
    Ok(())
}

fn superseded_assertions(kind: &FactAssertionKindV1) -> Vec<&FactAssertionId> {
    match kind {
        FactAssertionKindV1::Correction { supersedes } => vec![supersedes],
        FactAssertionKindV1::Merge { supersedes } => supersedes.iter().collect(),
        FactAssertionKindV1::Initial | FactAssertionKindV1::LegacyImport => Vec::new(),
    }
}

async fn assertion_exists(
    transaction: &Transaction,
    assertion_id: &FactAssertionId,
) -> FactStoreResult<bool> {
    row_exists(
        transaction,
        "SELECT 1 FROM memory_v2_assertions WHERE assertion_id = ?1",
        [assertion_id.as_str()],
    )
    .await
}

async fn assertion_matches(
    transaction: &Transaction,
    owner: &OwnerKey,
    assertion: &FactAssertionV1,
) -> FactStoreResult<bool> {
    let mut rows = transaction
        .query(
            "SELECT fact_id, owner_kind, project_id, owner_json,
                    assertion_header_json, kind_json, payload_reference_json,
                    receipt_json, asserted_at, actor_id
             FROM memory_v2_assertions WHERE assertion_id = ?1",
            [assertion.assertion_id().as_str()],
        )
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?
    else {
        return Ok(false);
    };
    let stored_actor = row_optional_string(&row, 9, QUERY_OPERATION)?;
    let expected_actor = assertion.actor_id().map(ToString::to_string);
    if row_string(&row, 0, QUERY_OPERATION)? != assertion.fact_id().as_str()
        || row_string(&row, 1, QUERY_OPERATION)? != owner.kind
        || row_string(&row, 2, QUERY_OPERATION)? != owner.project_id
        || row_string(&row, 3, QUERY_OPERATION)? != owner.json
        || row_string(&row, 4, QUERY_OPERATION)? != assertion_header_json(assertion)?
        || row_string(&row, 5, QUERY_OPERATION)?
            != to_json(assertion.kind(), "serialize assertion kind")?
        || row_string(&row, 6, QUERY_OPERATION)?
            != to_json(
                &assertion.payload().payload_reference()?,
                "serialize assertion payload reference",
            )?
        || row_string(&row, 7, QUERY_OPERATION)?
            != to_json(assertion.payload().receipt(), "serialize assertion receipt")?
        || row_i64(&row, 8, QUERY_OPERATION)? != assertion.asserted_at().0
        || stored_actor != expected_actor
    {
        return Ok(false);
    }

    let mut supersession = transaction
        .query(
            "SELECT superseded_assertion_id FROM memory_v2_assertion_supersession
             WHERE assertion_id = ?1 AND fact_id = ?2
               AND owner_kind = ?3 AND project_id = ?4 ORDER BY ordinal",
            params![
                assertion.assertion_id().as_str(),
                assertion.fact_id().as_str(),
                owner.kind,
                owner.project_id.as_str(),
            ],
        )
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?;
    let mut stored_supersession = Vec::new();
    while let Some(row) = supersession
        .next()
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?
    {
        stored_supersession.push(row_string(&row, 0, QUERY_OPERATION)?);
    }
    let expected_supersession = superseded_assertions(assertion.kind())
        .into_iter()
        .map(|id| id.as_str().to_owned())
        .collect::<Vec<_>>();
    if stored_supersession != expected_supersession {
        return Ok(false);
    }

    let mut payload = transaction
        .query(
            "SELECT payload_json, content FROM memory_v2_assertion_payloads
             WHERE assertion_id = ?1 AND fact_id = ?2
               AND owner_kind = ?3 AND project_id = ?4",
            params![
                assertion.assertion_id().as_str(),
                assertion.fact_id().as_str(),
                owner.kind,
                owner.project_id.as_str(),
            ],
        )
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?;
    let payload_row = payload
        .next()
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?;
    drop(payload);
    let payload_matches = match payload_row {
        Some(row) => {
            row_string(&row, 0, QUERY_OPERATION)?
                == to_json(assertion.payload(), "serialize assertion payload")?
                && row_string(&row, 1, QUERY_OPERATION)? == assertion.payload().content()
        }
        None => payload_is_purged_projection(transaction, owner, assertion.fact_id()).await?,
    };
    if !payload_matches {
        return Ok(false);
    }

    let mut evidence = transaction
        .query(
            "SELECT ae.evidence_id, e.evidence_json, e.owner_json, e.anchor_id
             FROM memory_v2_assertion_evidence ae
             JOIN memory_v2_evidence e ON
                e.evidence_id = ae.evidence_id AND e.fact_id = ae.fact_id AND
                e.owner_kind = ae.owner_kind AND e.project_id = ae.project_id
             WHERE ae.assertion_id = ?1 AND ae.fact_id = ?2
               AND ae.owner_kind = ?3 AND ae.project_id = ?4 ORDER BY ae.ordinal",
            params![
                assertion.assertion_id().as_str(),
                assertion.fact_id().as_str(),
                owner.kind,
                owner.project_id.as_str(),
            ],
        )
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?;
    let mut stored_evidence = Vec::new();
    while let Some(row) = evidence
        .next()
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?
    {
        stored_evidence.push((
            row_string(&row, 0, QUERY_OPERATION)?,
            row_string(&row, 1, QUERY_OPERATION)?,
            row_string(&row, 2, QUERY_OPERATION)?,
            row_string(&row, 3, QUERY_OPERATION)?,
        ));
    }
    let expected_evidence = assertion
        .evidence()
        .iter()
        .map(|evidence| {
            Ok((
                evidence.evidence_id().as_str().to_owned(),
                to_json(evidence, "serialize fact evidence")?,
                owner.json.clone(),
                evidence.anchor_id().as_str().to_owned(),
            ))
        })
        .collect::<FactStoreResult<Vec<_>>>()?;
    Ok(stored_evidence == expected_evidence)
}

async fn payload_is_purged_projection(
    transaction: &Transaction,
    owner: &OwnerKey,
    fact_id: &FactId,
) -> FactStoreResult<bool> {
    let mut rows = transaction
        .query(
            "SELECT current_facts.payload_access
             FROM memory_v2_current_facts AS current_facts
             JOIN memory_v2_facts AS facts
               ON facts.fact_id = current_facts.fact_id
              AND facts.owner_kind = current_facts.owner_kind
              AND facts.project_id = current_facts.project_id
             WHERE current_facts.fact_id = ?1
               AND current_facts.owner_kind = ?2
               AND current_facts.project_id = ?3
               AND facts.owner_json = ?4",
            params![
                fact_id.as_str(),
                owner.kind,
                owner.project_id.as_str(),
                owner.json.as_str(),
            ],
        )
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?
    else {
        return Ok(false);
    };
    Ok(matches!(
        parse_payload_access(&row_string(&row, 0, QUERY_OPERATION)?)?,
        PayloadAccessState::Quarantined | PayloadAccessState::Deleted
    ))
}

async fn insert_legacy_mapping(
    transaction: &Transaction,
    owner: &OwnerKey,
    mapping: &LegacyFactMappingV1,
) -> FactStoreResult<()> {
    if legacy_mapping_exists(transaction, owner, mapping).await? {
        if legacy_mapping_matches(transaction, owner, mapping).await? {
            return Ok(());
        }
        return Err(storage_message(
            COMMIT_OPERATION,
            "legacy mapping identity collision",
        ));
    }
    transaction
        .execute(
            "INSERT INTO memory_v2_legacy_map(
                owner_kind, project_id, owner_json, source_store_id,
                legacy_fact_id, fact_id, mapping_json
             ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                owner.kind,
                owner.project_id.as_str(),
                owner.json.as_str(),
                mapping.source_store_id().as_str(),
                mapping.legacy_fact_id(),
                mapping.fact_id().as_str(),
                to_json(mapping, "serialize legacy fact mapping")?,
            ],
        )
        .await
        .map_err(|error| storage_error(COMMIT_OPERATION, error))?;
    Ok(())
}

async fn legacy_mapping_exists(
    transaction: &Transaction,
    owner: &OwnerKey,
    mapping: &LegacyFactMappingV1,
) -> FactStoreResult<bool> {
    row_exists_params(
        transaction,
        "SELECT 1 FROM memory_v2_legacy_map
         WHERE owner_kind = ?1 AND project_id = ?2
           AND source_store_id = ?3 AND legacy_fact_id = ?4",
        params![
            owner.kind,
            owner.project_id.as_str(),
            mapping.source_store_id().as_str(),
            mapping.legacy_fact_id(),
        ],
    )
    .await
}

async fn legacy_mapping_matches(
    transaction: &Transaction,
    owner: &OwnerKey,
    mapping: &LegacyFactMappingV1,
) -> FactStoreResult<bool> {
    let mut rows = transaction
        .query(
            "SELECT owner_json, fact_id, mapping_json FROM memory_v2_legacy_map
             WHERE owner_kind = ?1 AND project_id = ?2
               AND source_store_id = ?3 AND legacy_fact_id = ?4",
            params![
                owner.kind,
                owner.project_id.as_str(),
                mapping.source_store_id().as_str(),
                mapping.legacy_fact_id(),
            ],
        )
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?
    else {
        return Ok(false);
    };
    Ok(row_string(&row, 0, QUERY_OPERATION)? == owner.json
        && row_string(&row, 1, QUERY_OPERATION)? == mapping.fact_id().as_str()
        && row_string(&row, 2, QUERY_OPERATION)?
            == to_json(mapping, "serialize legacy fact mapping")?)
}

async fn ensure_event_references(
    transaction: &Transaction,
    owner: &OwnerKey,
    event: &FactLineageEventV1,
) -> FactStoreResult<()> {
    match event.kind() {
        FactLineageEventKindV1::AssertionRecorded { assertion_id } => {
            if !owned_assertion_exists(transaction, owner, event.fact_id(), assertion_id).await? {
                return Err(storage_message(
                    COMMIT_OPERATION,
                    "lineage assertion reference is missing",
                ));
            }
        }
        FactLineageEventKindV1::TrustChanged { evidence_ids, .. } => {
            ensure_event_evidence(transaction, owner, event.fact_id(), evidence_ids).await?;
        }
        FactLineageEventKindV1::Curated {
            action,
            evidence_ids,
        } => {
            ensure_event_evidence(transaction, owner, event.fact_id(), evidence_ids).await?;
            if let FactCurationActionV1::ContradictedBy { fact_id }
            | FactCurationActionV1::SupersededBy { fact_id }
            | FactCurationActionV1::MergedInto { fact_id } = action
                && !owned_fact_exists(transaction, owner, fact_id).await?
            {
                return Err(storage_message(
                    COMMIT_OPERATION,
                    "lineage curation target is missing",
                ));
            }
        }
        FactLineageEventKindV1::PayloadAccessChanged { .. } => {}
        FactLineageEventKindV1::LegacyImported { mapping } => {
            if !legacy_mapping_matches(transaction, owner, mapping).await? {
                return Err(storage_message(
                    COMMIT_OPERATION,
                    "lineage legacy mapping reference is missing",
                ));
            }
        }
    }
    Ok(())
}

async fn ensure_event_evidence(
    transaction: &Transaction,
    owner: &OwnerKey,
    fact_id: &FactId,
    evidence_ids: &[FactEvidenceId],
) -> FactStoreResult<()> {
    for evidence_id in evidence_ids {
        if !owned_evidence_exists(transaction, owner, fact_id, evidence_id).await? {
            return Err(storage_message(
                COMMIT_OPERATION,
                "lineage evidence reference is missing",
            ));
        }
    }
    Ok(())
}

async fn owned_assertion_exists(
    transaction: &Transaction,
    owner: &OwnerKey,
    fact_id: &FactId,
    assertion_id: &FactAssertionId,
) -> FactStoreResult<bool> {
    row_exists_params(
        transaction,
        "SELECT 1 FROM memory_v2_assertions
         WHERE assertion_id = ?1 AND fact_id = ?2 AND owner_kind = ?3
           AND project_id = ?4 AND owner_json = ?5",
        params![
            assertion_id.as_str(),
            fact_id.as_str(),
            owner.kind,
            owner.project_id.as_str(),
            owner.json.as_str(),
        ],
    )
    .await
}

async fn owned_evidence_exists(
    transaction: &Transaction,
    owner: &OwnerKey,
    fact_id: &FactId,
    evidence_id: &FactEvidenceId,
) -> FactStoreResult<bool> {
    row_exists_params(
        transaction,
        "SELECT 1 FROM memory_v2_evidence
         WHERE evidence_id = ?1 AND fact_id = ?2 AND owner_kind = ?3
           AND project_id = ?4 AND owner_json = ?5",
        params![
            evidence_id.as_str(),
            fact_id.as_str(),
            owner.kind,
            owner.project_id.as_str(),
            owner.json.as_str(),
        ],
    )
    .await
}

async fn owned_fact_exists(
    transaction: &Transaction,
    owner: &OwnerKey,
    fact_id: &FactId,
) -> FactStoreResult<bool> {
    row_exists_params(
        transaction,
        "SELECT 1 FROM memory_v2_facts
         WHERE fact_id = ?1 AND owner_kind = ?2 AND project_id = ?3
           AND owner_json = ?4",
        params![
            fact_id.as_str(),
            owner.kind,
            owner.project_id.as_str(),
            owner.json.as_str(),
        ],
    )
    .await
}

async fn insert_event(
    transaction: &Transaction,
    owner: &OwnerKey,
    event: &FactLineageEventV1,
) -> FactStoreResult<()> {
    if event_exists(transaction, event.event_id()).await? {
        if event_matches(transaction, owner, event).await? {
            return Ok(());
        }
        return Err(storage_message(
            COMMIT_OPERATION,
            "lineage event identity collision",
        ));
    }
    transaction
        .execute(
            "INSERT INTO memory_v2_lineage_events(
                event_id, fact_id, owner_kind, project_id,
                event_json, occurred_at, recorded_at
             ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                event.event_id().as_str(),
                event.fact_id().as_str(),
                owner.kind,
                owner.project_id.as_str(),
                to_json(event, "serialize fact lineage event")?,
                event.occurred_at().0,
                event.occurred_at().0,
            ],
        )
        .await
        .map_err(|error| storage_error(COMMIT_OPERATION, error))?;
    Ok(())
}

async fn event_exists(transaction: &Transaction, event_id: &FactEventId) -> FactStoreResult<bool> {
    row_exists(
        transaction,
        "SELECT 1 FROM memory_v2_lineage_events WHERE event_id = ?1",
        [event_id.as_str()],
    )
    .await
}

async fn event_matches(
    transaction: &Transaction,
    owner: &OwnerKey,
    event: &FactLineageEventV1,
) -> FactStoreResult<bool> {
    let mut rows = transaction
        .query(
            "SELECT fact_id, owner_kind, project_id, event_json, occurred_at
             FROM memory_v2_lineage_events WHERE event_id = ?1",
            [event.event_id().as_str()],
        )
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?
    else {
        return Ok(false);
    };
    Ok(
        row_string(&row, 0, QUERY_OPERATION)? == event.fact_id().as_str()
            && row_string(&row, 1, QUERY_OPERATION)? == owner.kind
            && row_string(&row, 2, QUERY_OPERATION)? == owner.project_id
            && row_string(&row, 3, QUERY_OPERATION)?
                == to_json(event, "serialize fact lineage event")?
            && row_i64(&row, 4, QUERY_OPERATION)? == event.occurred_at().0,
    )
}

#[derive(Clone)]
struct Projection {
    access: PayloadAccessState,
    trust: Confidence,
    active_assertion_id: Option<FactAssertionId>,
    last_event_id: Option<FactEventId>,
    updated_at: UtcMicros,
}

impl Projection {
    fn empty() -> FactStoreResult<Self> {
        Ok(Self {
            access: PayloadAccessState::Eligible,
            trust: Confidence::new(DEFAULT_TRUST)?,
            active_assertion_id: None,
            last_event_id: None,
            updated_at: UtcMicros(0),
        })
    }

    fn apply(&mut self, event: &FactLineageEventV1) -> FactStoreResult<()> {
        match event.kind() {
            FactLineageEventKindV1::AssertionRecorded { assertion_id } => {
                self.active_assertion_id = Some(assertion_id.clone());
            }
            FactLineageEventKindV1::TrustChanged {
                previous, current, ..
            } => {
                if previous != &self.trust {
                    return Err(storage_message(
                        COMMIT_OPERATION,
                        "trust transition is stale",
                    ));
                }
                self.trust = *current;
            }
            FactLineageEventKindV1::PayloadAccessChanged { previous, current } => {
                if previous != &self.access {
                    return Err(storage_message(
                        COMMIT_OPERATION,
                        "payload access transition is stale",
                    ));
                }
                self.access = *current;
                if requires_payload_purge(*current) {
                    self.active_assertion_id = None;
                }
            }
            FactLineageEventKindV1::Curated { .. }
            | FactLineageEventKindV1::LegacyImported { .. } => {}
        }
        self.last_event_id = Some(event.event_id().clone());
        self.updated_at = event.occurred_at();
        Ok(())
    }
}

async fn publish_current_projection(
    transaction: &Transaction,
    owner: &OwnerKey,
    batch: &FactWriteBatch,
) -> FactStoreResult<()> {
    let mut projection = load_current_projection(transaction, owner, batch.fact_id())
        .await?
        .unwrap_or(Projection::empty()?);
    for event in batch.events() {
        projection.apply(event)?;
    }
    if projection.active_assertion_id.is_none() && !requires_payload_purge(projection.access) {
        return Err(storage_message(
            COMMIT_OPERATION,
            "fact projection has no active assertion",
        ));
    }
    let last = projection
        .last_event_id
        .as_ref()
        .ok_or(FactStoreError::EmptyBatch)?;
    transaction
        .execute(
            "INSERT INTO memory_v2_current_facts(
                fact_id, owner_kind, project_id, payload_access, trust_score,
                active_assertion_id, last_event_id, updated_at
             ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(fact_id, owner_kind, project_id) DO UPDATE SET
                payload_access = excluded.payload_access,
                trust_score = excluded.trust_score,
                active_assertion_id = excluded.active_assertion_id,
                last_event_id = excluded.last_event_id,
                updated_at = excluded.updated_at",
            params![
                batch.fact_id().as_str(),
                owner.kind,
                owner.project_id.as_str(),
                payload_access_label(projection.access),
                projection.trust.as_f64(),
                projection
                    .active_assertion_id
                    .as_ref()
                    .map(FactAssertionId::as_str),
                last.as_str(),
                projection.updated_at.0,
            ],
        )
        .await
        .map_err(|error| storage_error(COMMIT_OPERATION, error))?;
    if requires_payload_purge(projection.access) {
        transaction
            .execute_batch("PRAGMA secure_delete = ON;")
            .await
            .map_err(|error| storage_error(COMMIT_OPERATION, error))?;
        transaction
            .execute(
                "DELETE FROM memory_v2_assertion_vectors
                 WHERE fact_id = ?1 AND owner_kind = ?2 AND project_id = ?3",
                params![
                    batch.fact_id().as_str(),
                    owner.kind,
                    owner.project_id.as_str()
                ],
            )
            .await
            .map_err(|error| storage_error(COMMIT_OPERATION, error))?;
        transaction
            .execute(
                "DELETE FROM memory_v2_assertion_payloads
                 WHERE fact_id = ?1 AND owner_kind = ?2 AND project_id = ?3",
                params![
                    batch.fact_id().as_str(),
                    owner.kind,
                    owner.project_id.as_str()
                ],
            )
            .await
            .map_err(|error| storage_error(COMMIT_OPERATION, error))?;
    }
    Ok(())
}

async fn load_current_projection(
    transaction: &Transaction,
    owner: &OwnerKey,
    fact_id: &FactId,
) -> FactStoreResult<Option<Projection>> {
    let mut rows = transaction
        .query(
            "SELECT payload_access, trust_score, active_assertion_id,
                    last_event_id, updated_at
             FROM memory_v2_current_facts
             WHERE fact_id = ?1 AND owner_kind = ?2 AND project_id = ?3",
            params![fact_id.as_str(), owner.kind, owner.project_id.as_str()],
        )
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?
    else {
        return Ok(None);
    };
    Ok(Some(Projection {
        access: parse_payload_access(&row_string(&row, 0, QUERY_OPERATION)?)?,
        trust: Confidence::new(row_f64(&row, 1, QUERY_OPERATION)?)?,
        active_assertion_id: row_optional_string(&row, 2, QUERY_OPERATION)?
            .map(FactAssertionId::new)
            .transpose()?,
        last_event_id: row_optional_string(&row, 3, QUERY_OPERATION)?
            .map(FactEventId::new)
            .transpose()?,
        updated_at: UtcMicros(row_i64(&row, 4, QUERY_OPERATION)?),
    }))
}

async fn receipt_outcome(
    transaction: &Transaction,
    owner: &OwnerKey,
    batch: &FactWriteBatch,
    replay: bool,
) -> FactStoreResult<FactCommitOutcome> {
    let projection = load_current_projection(transaction, owner, batch.fact_id())
        .await?
        .ok_or_else(|| storage_message(COMMIT_OPERATION, "committed projection is missing"))?;
    let last = batch
        .events()
        .last()
        .map(FactLineageEventV1::event_id)
        .ok_or(FactStoreError::EmptyBatch)?;
    let receipt = FactCommitReceipt::new(
        batch.fact_id().clone(),
        batch.owner().clone(),
        batch
            .events()
            .iter()
            .map(|event| event.event_id().clone())
            .collect(),
        last.clone(),
        projection.active_assertion_id,
    )?;
    Ok(if replay {
        FactCommitOutcome::IdempotentReplay(receipt)
    } else {
        FactCommitOutcome::Committed(receipt)
    })
}

async fn ensure_fact_identity(
    transaction: &Transaction,
    owner: &OwnerKey,
    batch: &FactWriteBatch,
) -> FactStoreResult<()> {
    let mut rows = transaction
        .query(
            "SELECT owner_kind, project_id, owner_json, identity_json
             FROM memory_v2_facts WHERE fact_id = ?1",
            [batch.fact_id().as_str()],
        )
        .await
        .map_err(|error| storage_error(COMMIT_OPERATION, error))?;
    if let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(COMMIT_OPERATION, error))?
    {
        let stored_owner_kind = row_string(&row, 0, COMMIT_OPERATION)?;
        let stored_project_id = row_string(&row, 1, COMMIT_OPERATION)?;
        let stored_owner_json = row_string(&row, 2, COMMIT_OPERATION)?;
        let stored_identity = row_string(&row, 3, COMMIT_OPERATION)?;
        let supplied_identity = batch
            .identity_material()
            .map(|identity| to_json(identity, "serialize fact identity"))
            .transpose()?;
        if stored_owner_kind != owner.kind
            || stored_project_id != owner.project_id
            || stored_owner_json != owner.json
            || supplied_identity
                .as_ref()
                .is_some_and(|identity| identity != &stored_identity)
        {
            return identity_collision("fact", batch.fact_id().as_str());
        }
        return Ok(());
    }
    let identity = batch
        .identity_material()
        .ok_or_else(|| FactStoreError::Storage {
            operation: COMMIT_OPERATION,
            source: Box::new(std::io::Error::other(
                "new fact requires deterministic identity material",
            )),
        })?;
    let identity_json = to_json(identity, "serialize fact identity")?;
    let created_at = batch
        .events()
        .first()
        .map(FactLineageEventV1::occurred_at)
        .ok_or(FactStoreError::EmptyBatch)?;
    transaction
        .execute(
            "INSERT INTO memory_v2_facts(
                fact_id, owner_kind, project_id, owner_json, identity_json, created_at
             ) VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                batch.fact_id().as_str(),
                owner.kind,
                owner.project_id.as_str(),
                owner.json.as_str(),
                identity_json,
                created_at.0,
            ],
        )
        .await
        .map_err(|error| storage_error(COMMIT_OPERATION, error))?;
    Ok(())
}

fn storage_error(
    operation: &'static str,
    source: impl Error + Send + Sync + 'static,
) -> FactStoreError {
    FactStoreError::Storage {
        operation,
        source: Box::new(source),
    }
}

fn storage_message(operation: &'static str, message: impl Into<String>) -> FactStoreError {
    storage_error(operation, std::io::Error::other(message.into()))
}

fn authority_storage_error(
    operation: &'static str,
    source: impl Error + Send + Sync + 'static,
) -> FactProposalStoreError {
    FactProposalStoreError::Storage {
        operation,
        source: Box::new(source),
    }
}

fn identity_collision<T>(kind: &'static str, id: &str) -> FactStoreResult<T> {
    Err(storage_message(
        COMMIT_OPERATION,
        format!("{kind} identity collision for {id}"),
    ))
}

fn to_json<T: Serialize + ?Sized>(value: &T, operation: &'static str) -> FactStoreResult<String> {
    serde_json::to_string(value).map_err(|error| storage_error(operation, error))
}

fn from_json<T: DeserializeOwned>(value: &str, operation: &'static str) -> FactStoreResult<T> {
    serde_json::from_str(value).map_err(|error| storage_error(operation, error))
}

fn row_string(row: &libsql::Row, index: i32, operation: &'static str) -> FactStoreResult<String> {
    row.get(index)
        .map_err(|error| storage_error(operation, error))
}

fn row_optional_string(
    row: &libsql::Row,
    index: i32,
    operation: &'static str,
) -> FactStoreResult<Option<String>> {
    row.get(index)
        .map_err(|error| storage_error(operation, error))
}

fn row_i64(row: &libsql::Row, index: i32, operation: &'static str) -> FactStoreResult<i64> {
    row.get(index)
        .map_err(|error| storage_error(operation, error))
}

fn row_optional_f64(
    row: &libsql::Row,
    index: i32,
    operation: &'static str,
) -> FactStoreResult<Option<f64>> {
    row.get(index)
        .map_err(|error| storage_error(operation, error))
}

fn row_f64(row: &libsql::Row, index: i32, operation: &'static str) -> FactStoreResult<f64> {
    row.get(index)
        .map_err(|error| storage_error(operation, error))
}

async fn row_exists(
    transaction: &Transaction,
    sql: &str,
    values: impl libsql::params::IntoParams,
) -> FactStoreResult<bool> {
    let mut rows = transaction
        .query(sql, values)
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?;
    Ok(rows
        .next()
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?
        .is_some())
}

async fn row_exists_params(
    transaction: &Transaction,
    sql: &str,
    values: impl libsql::params::IntoParams,
) -> FactStoreResult<bool> {
    row_exists(transaction, sql, values).await
}

fn payload_access_label(state: PayloadAccessState) -> &'static str {
    match state {
        PayloadAccessState::Eligible => "eligible",
        PayloadAccessState::Redacted => "redacted",
        PayloadAccessState::Quarantined => "quarantined",
        PayloadAccessState::RetentionExpired => "retention_expired",
        PayloadAccessState::Deleted => "deleted",
        PayloadAccessState::Unavailable => "unavailable",
        PayloadAccessState::Ambiguous => "ambiguous",
    }
}

fn parse_payload_access(value: &str) -> FactStoreResult<PayloadAccessState> {
    match value {
        "eligible" => Ok(PayloadAccessState::Eligible),
        "redacted" => Ok(PayloadAccessState::Redacted),
        "quarantined" => Ok(PayloadAccessState::Quarantined),
        "retention_expired" => Ok(PayloadAccessState::RetentionExpired),
        "deleted" => Ok(PayloadAccessState::Deleted),
        "unavailable" => Ok(PayloadAccessState::Unavailable),
        "ambiguous" => Ok(PayloadAccessState::Ambiguous),
        _ => Err(storage_message(
            QUERY_OPERATION,
            format!("unknown payload access state {value:?}"),
        )),
    }
}

fn requires_payload_purge(access: PayloadAccessState) -> bool {
    matches!(
        access,
        PayloadAccessState::Quarantined | PayloadAccessState::Deleted
    )
}

async fn query_current_facts_tx(
    snapshot: &Transaction,
    query: &CurrentFactsQuery,
) -> FactStoreResult<Vec<StoredFactV1>> {
    let owner = OwnerKey::new(query.owner())?;
    let mut rows = match query.after_fact_id() {
        Some(after) => {
            snapshot
                .query(
                    "SELECT fact_id FROM memory_v2_current_facts
                 WHERE owner_kind = ?1 AND project_id = ?2
                   AND active_assertion_id IS NOT NULL AND fact_id > ?3
                 ORDER BY fact_id ASC LIMIT ?4",
                    params![
                        owner.kind,
                        owner.project_id.as_str(),
                        after.as_str(),
                        query.limit() as i64,
                    ],
                )
                .await
        }
        None => {
            snapshot
                .query(
                    "SELECT fact_id FROM memory_v2_current_facts
                 WHERE owner_kind = ?1 AND project_id = ?2
                   AND active_assertion_id IS NOT NULL
                 ORDER BY fact_id ASC LIMIT ?3",
                    params![owner.kind, owner.project_id.as_str(), query.limit() as i64],
                )
                .await
        }
    }
    .map_err(|error| storage_error(QUERY_OPERATION, error))?;
    let mut fact_ids = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?
    {
        fact_ids.push(FactId::new(row_string(&row, 0, QUERY_OPERATION)?)?);
    }
    drop(rows);

    let mut facts = Vec::with_capacity(fact_ids.len());
    for fact_id in fact_ids {
        let fact = load_current_fact_tx(snapshot, &owner, query.owner(), &fact_id)
            .await?
            .ok_or_else(|| {
                storage_message(QUERY_OPERATION, "current fact disappeared in snapshot")
            })?;
        facts.push(fact);
    }
    Ok(facts)
}

async fn query_fact_current_tx(
    snapshot: &Transaction,
    owner: &FactOwnerV1,
    fact_id: &FactId,
) -> FactStoreResult<Option<StoredFactV1>> {
    let key = OwnerKey::new(owner)?;
    load_current_fact_tx(snapshot, &key, owner, fact_id).await
}

async fn load_current_fact_tx(
    snapshot: &Transaction,
    owner: &OwnerKey,
    typed_owner: &FactOwnerV1,
    fact_id: &FactId,
) -> FactStoreResult<Option<StoredFactV1>> {
    let mut rows = snapshot
        .query(
            "SELECT facts.fact_id, current_facts.payload_access, current_facts.trust_score,
                    current_facts.active_assertion_id, current_facts.last_event_id,
                    current_facts.updated_at, payloads.payload_json
             FROM memory_v2_current_facts AS current_facts
             JOIN memory_v2_facts AS facts
               ON facts.fact_id = current_facts.fact_id
              AND facts.owner_kind = current_facts.owner_kind
              AND facts.project_id = current_facts.project_id
             LEFT JOIN memory_v2_assertion_payloads AS payloads
               ON payloads.assertion_id = current_facts.active_assertion_id
              AND payloads.fact_id = current_facts.fact_id
              AND payloads.owner_kind = current_facts.owner_kind
              AND payloads.project_id = current_facts.project_id
             WHERE current_facts.fact_id = ?1
               AND current_facts.owner_kind = ?2
               AND current_facts.project_id = ?3
               AND facts.owner_json = ?4",
            params![
                fact_id.as_str(),
                owner.kind,
                owner.project_id.as_str(),
                owner.json.as_str(),
            ],
        )
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?
    else {
        return Ok(None);
    };
    let stored_id = FactId::new(row_string(&row, 0, QUERY_OPERATION)?)?;
    if &stored_id != fact_id {
        return Err(storage_message(
            QUERY_OPERATION,
            "current fact identity mismatch",
        ));
    }
    let access = parse_payload_access(&row_string(&row, 1, QUERY_OPERATION)?)?;
    let trust = Confidence::new(row_optional_f64(&row, 2, QUERY_OPERATION)?.ok_or_else(|| {
        storage_message(
            QUERY_OPERATION,
            "current fact trust score is unexpectedly null",
        )
    })?)?;
    let Some(active_assertion_id) = row_optional_string(&row, 3, QUERY_OPERATION)? else {
        return Ok(None);
    };
    let active_assertion_id = FactAssertionId::new(active_assertion_id)?;
    let last_event_id = FactEventId::new(row_string(&row, 4, QUERY_OPERATION)?)?;
    let projected_as_of = UtcMicros(row_i64(&row, 5, QUERY_OPERATION)?);
    let payload = match access {
        PayloadAccessState::Eligible => {
            let payload_json = row_optional_string(&row, 6, QUERY_OPERATION)?
                .ok_or(FactStoreError::PayloadAccessMismatch)?;
            Some(from_json::<FactPayloadV1>(&payload_json, QUERY_OPERATION)?)
        }
        _ => None,
    };
    let mapping = load_current_legacy_mapping_tx(snapshot, owner, typed_owner, fact_id).await?;
    StoredFactV1::new(
        stored_id,
        typed_owner.clone(),
        payload,
        access,
        trust,
        active_assertion_id,
        last_event_id,
        mapping,
        projected_as_of,
    )
    .map(Some)
}

async fn query_fact_as_of_tx(
    snapshot: &Transaction,
    query: &FactAsOfQuery,
) -> FactStoreResult<Option<StoredFactV1>> {
    let owner = OwnerKey::new(query.owner())?;
    let mut rows = snapshot
        .query(
            "SELECT event_json FROM memory_v2_lineage_events
             WHERE fact_id = ?1 AND owner_kind = ?2 AND project_id = ?3
               AND occurred_at <= ?4
             ORDER BY occurred_at ASC, event_id ASC",
            params![
                query.fact_id().as_str(),
                owner.kind,
                owner.project_id.as_str(),
                query.as_of().0,
            ],
        )
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?;
    let mut projection = Projection::empty()?;
    let mut observed_event = false;
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?
    {
        let event = from_json::<FactLineageEventV1>(
            &row_string(&row, 0, QUERY_OPERATION)?,
            QUERY_OPERATION,
        )?;
        if event.fact_id() != query.fact_id() || event.owner() != query.owner() {
            return Err(storage_message(
                QUERY_OPERATION,
                "stored lineage event identity mismatch",
            ));
        }
        projection.apply(&event)?;
        observed_event = true;
    }
    drop(rows);
    if !observed_event {
        return Ok(None);
    }
    let Some(active_assertion_id) = projection.active_assertion_id.clone() else {
        return Ok(None);
    };
    let last_event_id = projection
        .last_event_id
        .clone()
        .ok_or(FactStoreError::EmptyBatch)?;
    let (payload, payload_access) = match projection.access {
        PayloadAccessState::Eligible => match load_assertion_payload_tx(
            snapshot,
            &owner,
            query.fact_id(),
            &active_assertion_id,
        )
        .await?
        {
            Some(payload) => (Some(payload), PayloadAccessState::Eligible),
            // A later deletion physically erases the payload and FTS/vector
            // copies. Do not resurrect that data merely because an as-of
            // projection predates the deletion event; retain the lineage but
            // make the unavailable payload explicit.
            None => (None, PayloadAccessState::Unavailable),
        },
        access => (None, access),
    };
    let mapping = load_current_legacy_mapping_tx(snapshot, &owner, query.owner(), query.fact_id())
        .await?
        .filter(|mapping| mapping.migrated_at() <= query.as_of());
    StoredFactV1::new(
        query.fact_id().clone(),
        query.owner().clone(),
        payload,
        payload_access,
        projection.trust,
        active_assertion_id,
        last_event_id,
        mapping,
        projection.updated_at,
    )
    .map(Some)
}

async fn load_assertion_payload_tx(
    snapshot: &Transaction,
    owner: &OwnerKey,
    fact_id: &FactId,
    assertion_id: &FactAssertionId,
) -> FactStoreResult<Option<FactPayloadV1>> {
    let mut rows = snapshot
        .query(
            "SELECT payload_json FROM memory_v2_assertion_payloads
             WHERE assertion_id = ?1 AND fact_id = ?2
               AND owner_kind = ?3 AND project_id = ?4",
            params![
                assertion_id.as_str(),
                fact_id.as_str(),
                owner.kind,
                owner.project_id.as_str(),
            ],
        )
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?
    else {
        return Ok(None);
    };
    from_json(&row_string(&row, 0, QUERY_OPERATION)?, QUERY_OPERATION).map(Some)
}

async fn query_fact_lineage_tx(
    snapshot: &Transaction,
    query: &FactLineageQuery,
) -> FactStoreResult<Vec<FactLineageEventV1>> {
    let owner = OwnerKey::new(query.owner())?;
    let mut rows = match query.after() {
        Some(after) => {
            snapshot
                .query(
                    "SELECT event_json FROM memory_v2_lineage_events
                 WHERE fact_id = ?1 AND owner_kind = ?2 AND project_id = ?3
                   AND (occurred_at > ?4 OR (occurred_at = ?4 AND event_id > ?5))
                 ORDER BY occurred_at ASC, event_id ASC LIMIT ?6",
                    params![
                        query.fact_id().as_str(),
                        owner.kind,
                        owner.project_id.as_str(),
                        after.occurred_at().0,
                        after.event_id().as_str(),
                        query.limit() as i64,
                    ],
                )
                .await
        }
        None => {
            snapshot
                .query(
                    "SELECT event_json FROM memory_v2_lineage_events
                 WHERE fact_id = ?1 AND owner_kind = ?2 AND project_id = ?3
                 ORDER BY occurred_at ASC, event_id ASC LIMIT ?4",
                    params![
                        query.fact_id().as_str(),
                        owner.kind,
                        owner.project_id.as_str(),
                        query.limit() as i64,
                    ],
                )
                .await
        }
    }
    .map_err(|error| storage_error(QUERY_OPERATION, error))?;
    let mut events = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?
    {
        let event = from_json::<FactLineageEventV1>(
            &row_string(&row, 0, QUERY_OPERATION)?,
            QUERY_OPERATION,
        )?;
        if event.fact_id() != query.fact_id() || event.owner() != query.owner() {
            return Err(storage_message(
                QUERY_OPERATION,
                "stored lineage event identity mismatch",
            ));
        }
        events.push(event);
    }
    Ok(events)
}

async fn resolve_legacy_fact_tx(
    snapshot: &Transaction,
    query: &LegacyFactQuery,
) -> FactStoreResult<Option<FactId>> {
    let owner = OwnerKey::new(query.owner())?;
    let mut rows = snapshot
        .query(
            "SELECT fact_id, owner_json FROM memory_v2_legacy_map
             WHERE owner_kind = ?1 AND project_id = ?2
               AND source_store_id = ?3 AND legacy_fact_id = ?4",
            params![
                owner.kind,
                owner.project_id.as_str(),
                query.source_store_id().as_str(),
                query.legacy_fact_id(),
            ],
        )
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?
    else {
        return Ok(None);
    };
    if row_string(&row, 1, QUERY_OPERATION)? != owner.json {
        return Err(FactStoreError::OwnerMismatch);
    }
    let fact_id = FactId::new(row_string(&row, 0, QUERY_OPERATION)?)?;
    query.validate_resolved_fact_id(&fact_id)?;
    Ok(Some(fact_id))
}

async fn get_retrieval_anchor_tx(
    snapshot: &Transaction,
    query: &RetrievalAnchorQuery,
) -> FactStoreResult<Option<RetrievalAnchorRecordV2>> {
    let owner = OwnerKey::new(query.owner())?;
    let mut rows = snapshot
        .query(
            "SELECT anchor_json FROM retrieval_anchors
             WHERE anchor_id = ?1 AND owner_json = ?2",
            params![query.anchor_id().as_str(), owner.json.as_str()],
        )
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?
    else {
        return Ok(None);
    };
    let anchor = from_json::<RetrievalAnchorRecordV2>(
        &row_string(&row, 0, QUERY_OPERATION)?,
        QUERY_OPERATION,
    )?;
    if anchor.anchor_id() != query.anchor_id()
        || FactOwnerV1::from(anchor.owner().clone()) != *query.owner()
        || !anchor_matches(snapshot, &owner, &anchor).await?
    {
        return Err(storage_message(
            QUERY_OPERATION,
            "retrieval anchor identity mismatch",
        ));
    }
    Ok(Some(anchor))
}

async fn load_current_legacy_mapping_tx(
    snapshot: &Transaction,
    owner: &OwnerKey,
    typed_owner: &FactOwnerV1,
    fact_id: &FactId,
) -> FactStoreResult<Option<LegacyFactMappingV1>> {
    let mut rows = snapshot
        .query(
            "SELECT mapping_json FROM memory_v2_legacy_map
             WHERE owner_kind = ?1 AND project_id = ?2 AND fact_id = ?3
             ORDER BY source_store_id ASC LIMIT 1",
            params![owner.kind, owner.project_id.as_str(), fact_id.as_str()],
        )
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?
    else {
        return Ok(None);
    };
    let mapping =
        from_json::<LegacyFactMappingV1>(&row_string(&row, 0, QUERY_OPERATION)?, QUERY_OPERATION)?;
    if mapping.owner() != typed_owner || mapping.fact_id() != fact_id {
        return Err(storage_message(
            QUERY_OPERATION,
            "legacy mapping identity mismatch",
        ));
    }
    Ok(Some(mapping))
}

async fn promote_fact_proposal_tx(
    transaction: &Transaction,
    promotion: &PromoteFactProposal,
) -> Result<PromotionAttempt, FactProposalStoreError> {
    let owner = OwnerKey::new(promotion.owner())?;
    let actual = proposal_current_state(transaction, &owner, promotion.proposal_id()).await?;
    if actual != Some(promotion.expected_state()) {
        if let Some(stored_transition_json) = matching_applied_promotion_transition(
            transaction,
            &owner,
            promotion,
        )
        .await?
        {
            let actual_last = current_last_event(transaction, &owner, promotion.batch().fact_id())
                .await?;
            if actual_last
                .as_ref()
                == promotion.batch().events().last().map(FactLineageEventV1::event_id)
            {
                let commit = commit_fact_tx(transaction, promotion.batch()).await?.outcome;
                if let FactCommitOutcome::IdempotentReplay(receipt) = &commit
                    && promotion_transition_json(promotion, receipt)? == stored_transition_json
                {
                    return Ok(PromotionAttempt {
                        outcome: PromoteFactProposalOutcome::new(
                            promotion.proposal_id().clone(),
                            promotion.expected_state(),
                            commit,
                        )
                        .map_err(FactStoreError::from)?,
                        wrote: false,
                    });
                }
            }
        }
        return Err(FactProposalStoreError::ProposalStateConflict {
            proposal_id: promotion.proposal_id().clone(),
            expected: promotion.expected_state(),
            actual,
        });
    }

    let commit = commit_fact_tx(transaction, promotion.batch())
        .await?
        .outcome;
    if matches!(&commit, FactCommitOutcome::Conflict(_)) {
        return Ok(PromotionAttempt {
            outcome: PromoteFactProposalOutcome::new(
                promotion.proposal_id().clone(),
                promotion.expected_state(),
                commit,
            )
            .map_err(FactStoreError::from)?,
            wrote: false,
        });
    }
    let receipt = match &commit {
        FactCommitOutcome::Committed(receipt) | FactCommitOutcome::IdempotentReplay(receipt) => {
            receipt
        }
        FactCommitOutcome::Conflict(_) => unreachable!("handled above"),
        _ => {
            return Err(authority_storage_error(
                PROMOTE_OPERATION,
                std::io::Error::other("unrecognized fact commit outcome"),
            ));
        }
    };
    let transition_json = promotion_transition_json(promotion, receipt)?;
    let transition_id = proposal_transition_id(&transition_json);
    let reviewer_json = promotion
        .reviewer()
        .map(|reviewer| to_json(reviewer, PROMOTE_OPERATION))
        .transpose()?;
    let occurred_at = promotion
        .batch()
        .events()
        .last()
        .ok_or(FactStoreError::EmptyBatch)?
        .occurred_at()
        .0;
    transaction
        .execute(
            "INSERT INTO memory_v2_proposal_transitions(
                transition_id, proposal_id, owner_kind, project_id,
                previous_state, current_state, reviewer_json, validation_json,
                origin, promoted_fact_id, promoted_assertion_id, promoted_event_id,
                transition_json, occurred_at
             ) VALUES(?1, ?2, ?3, ?4, ?5, 'applied', ?6, NULL,
                      'runtime', ?7, ?8, ?9, ?10, ?11)",
            params![
                transition_id.as_str(),
                promotion.proposal_id().as_str(),
                owner.kind,
                owner.project_id.as_str(),
                proposal_state_label(promotion.expected_state()),
                reviewer_json,
                receipt.fact_id().as_str(),
                receipt.active_assertion_id().map(FactAssertionId::as_str),
                receipt.last_event_id().as_str(),
                transition_json,
                occurred_at,
            ],
        )
        .await
        .map_err(|error| authority_storage_error(PROMOTE_OPERATION, error))?;
    let changed = transaction
        .execute(
            "UPDATE memory_v2_proposal_current
             SET state = 'applied', revision = revision + 1,
                 last_transition_id = ?1, updated_at = ?2
             WHERE proposal_id = ?3 AND owner_kind = ?4 AND project_id = ?5
               AND state = ?6",
            params![
                transition_id.as_str(),
                occurred_at,
                promotion.proposal_id().as_str(),
                owner.kind,
                owner.project_id.as_str(),
                proposal_state_label(promotion.expected_state()),
            ],
        )
        .await
        .map_err(|error| authority_storage_error(PROMOTE_OPERATION, error))?;
    if changed != 1 {
        return Err(FactProposalStoreError::ProposalStateConflict {
            proposal_id: promotion.proposal_id().clone(),
            expected: promotion.expected_state(),
            actual: proposal_current_state(transaction, &owner, promotion.proposal_id()).await?,
        });
    }
    Ok(PromotionAttempt {
        outcome: PromoteFactProposalOutcome::new(
            promotion.proposal_id().clone(),
            promotion.expected_state(),
            commit,
        )
        .map_err(FactStoreError::from)?,
        wrote: true,
    })
}

async fn proposal_current_state(
    transaction: &Transaction,
    owner: &OwnerKey,
    proposal_id: &ProvenanceId,
) -> Result<Option<FactProposalPromotionStateV1>, FactProposalStoreError> {
    let mut rows = transaction
        .query(
            "SELECT current_state.state, proposals.owner_json
             FROM memory_v2_proposal_current AS current_state
             JOIN memory_v2_proposals AS proposals
               ON proposals.proposal_id = current_state.proposal_id
              AND proposals.owner_kind = current_state.owner_kind
              AND proposals.project_id = current_state.project_id
             WHERE current_state.proposal_id = ?1
               AND current_state.owner_kind = ?2
               AND current_state.project_id = ?3",
            params![proposal_id.as_str(), owner.kind, owner.project_id.as_str(),],
        )
        .await
        .map_err(|error| authority_storage_error(PROMOTE_OPERATION, error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| authority_storage_error(PROMOTE_OPERATION, error))?
    else {
        return Ok(None);
    };
    let owner_json = row
        .get::<String>(1)
        .map_err(|error| authority_storage_error(PROMOTE_OPERATION, error))?;
    if owner_json != owner.json {
        return Err(authority_storage_error(
            PROMOTE_OPERATION,
            std::io::Error::other("proposal owner identity mismatch"),
        ));
    }
    let state = row
        .get::<String>(0)
        .map_err(|error| authority_storage_error(PROMOTE_OPERATION, error))?;
    parse_proposal_current_state(&state)
}

async fn matching_applied_promotion_transition(
    transaction: &Transaction,
    owner: &OwnerKey,
    promotion: &PromoteFactProposal,
) -> Result<Option<String>, FactProposalStoreError> {
    let mut rows = transaction
        .query(
            "SELECT current_state.state, proposals.owner_json,
                    transition.previous_state, transition.current_state,
                    transition.promoted_fact_id, transition.promoted_event_id,
                    transition.transition_json
             FROM memory_v2_proposal_current AS current_state
             JOIN memory_v2_proposals AS proposals
               ON proposals.proposal_id = current_state.proposal_id
              AND proposals.owner_kind = current_state.owner_kind
              AND proposals.project_id = current_state.project_id
             JOIN memory_v2_proposal_transitions AS transition
               ON transition.transition_id = current_state.last_transition_id
              AND transition.proposal_id = current_state.proposal_id
              AND transition.owner_kind = current_state.owner_kind
              AND transition.project_id = current_state.project_id
             WHERE current_state.proposal_id = ?1
               AND current_state.owner_kind = ?2
               AND current_state.project_id = ?3",
            params![
                promotion.proposal_id().as_str(),
                owner.kind,
                owner.project_id.as_str(),
            ],
        )
        .await
        .map_err(|error| authority_storage_error(PROMOTE_OPERATION, error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| authority_storage_error(PROMOTE_OPERATION, error))?
    else {
        return Ok(None);
    };
    if row_string(&row, 1, PROMOTE_OPERATION)? != owner.json {
        return Err(authority_storage_error(
            PROMOTE_OPERATION,
            std::io::Error::other("proposal owner identity mismatch"),
        ));
    }
    let last_event_id = promotion
        .batch()
        .events()
        .last()
        .map(FactLineageEventV1::event_id)
        .ok_or(FactStoreError::EmptyBatch)?;
    if row_string(&row, 0, PROMOTE_OPERATION)? != "applied"
        || row_string(&row, 2, PROMOTE_OPERATION)?
            != proposal_state_label(promotion.expected_state())
        || row_string(&row, 3, PROMOTE_OPERATION)? != "applied"
        || row_optional_string(&row, 4, PROMOTE_OPERATION)?.as_deref()
            != Some(promotion.batch().fact_id().as_str())
        || row_optional_string(&row, 5, PROMOTE_OPERATION)?.as_deref()
            != Some(last_event_id.as_str())
    {
        return Ok(None);
    }
    Ok(Some(row_string(&row, 6, PROMOTE_OPERATION)?))
}

fn proposal_state_label(state: FactProposalPromotionStateV1) -> &'static str {
    match state {
        FactProposalPromotionStateV1::PendingApproval => "pending",
        FactProposalPromotionStateV1::Applying => "applying",
    }
}

fn parse_proposal_current_state(
    state: &str,
) -> Result<Option<FactProposalPromotionStateV1>, FactProposalStoreError> {
    match state {
        "pending" => Ok(Some(FactProposalPromotionStateV1::PendingApproval)),
        "applying" => Ok(Some(FactProposalPromotionStateV1::Applying)),
        "applied" | "rejected" => Ok(None),
        _ => Err(authority_storage_error(
            PROMOTE_OPERATION,
            std::io::Error::other(format!("unknown proposal state {state:?}")),
        )),
    }
}

fn promotion_transition_json(
    promotion: &PromoteFactProposal,
    receipt: &FactCommitReceipt,
) -> Result<String, FactProposalStoreError> {
    to_json(
        &json!({
            "proposal_id": promotion.proposal_id().as_str(),
            "previous_state": proposal_state_label(promotion.expected_state()),
            "current_state": "applied",
            "reviewer": promotion.reviewer().map(|reviewer| reviewer.as_str()),
            "fact_id": receipt.fact_id().as_str(),
            "active_assertion_id": receipt.active_assertion_id().map(FactAssertionId::as_str),
            "last_event_id": receipt.last_event_id().as_str(),
        }),
        PROMOTE_OPERATION,
    )
    .map_err(FactProposalStoreError::from)
}

fn proposal_transition_id(transition_json: &str) -> String {
    let digest = Sha256::digest(transition_json.as_bytes());
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut id = String::from("proposal-transition:");
    for byte in digest {
        id.push(char::from(HEX[usize::from(byte >> 4)]));
        id.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    id
}
