#![allow(dead_code)] // production evidence-assembly authority; mounted via RegisteredGlobalDb

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use tracedecay_application::{
    DisclosureClass, RequestAdmission, RequestContext as ProductRequestContext,
};
use tracedecay_domain::{RetrievalAnchorId, UtcMicros, canonical_sha256};

use tracedecay_runtime_core::db::engine::{QueryExecutor, params};
use tracedecay_runtime_core::db::{Database, DatabaseAccessMode};
use tracedecay_runtime_core::store_runtime::registry::StoreRuntimeHandle;

/// Typed Stage-C adapter for canonical V3 evidence assemblies.
///
/// This is intentionally an alternate path until callers can construct the V3
/// records directly. It accepts only a daemon-verified runtime handle and the
/// authoritative profile identity carried by the daemon; it never infers
/// either identity from a path, label, database, or request payload.
#[derive(Clone)]
pub struct RuntimeEvidenceAssemblyStore {
    profile_id: tracedecay_domain::UserProfileId,
    runtime: StoreRuntimeHandle,
    authority: tracedecay_runtime_core::db::DatabaseAuthority,
}

#[derive(Clone, Debug, PartialEq, Eq)]
// Resolved carries the full anchor record; boxing would ripple through store match sites.
#[allow(clippy::large_enum_variant)]
pub(crate) enum EvidenceAssemblyAnchorResolutionV1 {
    Resolved {
        /// Both persisted anchor generations resolve. The store is mid-cutover:
        /// the observation and repository-provenance writers still commit V2
        /// records, so narrowing this to V3 would report a live anchor as
        /// `Unavailable` — a falsified absence, not a real one.
        record: tracedecay_store::StoredRetrievalAnchorRecordV1,
        derivatives: Vec<tracedecay_store::RetrievalAnchorDerivativeV1>,
    },
    Tombstone(tracedecay_store::RetrievalAnchorTombstoneV1),
    Unavailable,
}

impl RuntimeEvidenceAssemblyStore {
    pub fn new(
        profile_id: tracedecay_domain::UserProfileId,
        runtime: StoreRuntimeHandle,
        authority: tracedecay_runtime_core::db::DatabaseAuthority,
    ) -> tracedecay_store::EvidenceAssemblyStoreResult<Self> {
        let binding = runtime.binding();
        if binding.shard_id.profile_id != profile_id
            || authority.canonical_database_path() != runtime.locator().path()
            || !matches!(
                binding.shard_id.scope,
                tracedecay_store::StoreShardScopeV1::Project { .. }
                    | tracedecay_store::StoreShardScopeV1::ProjectSessions { .. }
                    | tracedecay_store::StoreShardScopeV1::ProfileSessions
            )
        {
            return Err(evidence_runtime_invalid(
                "evidence runtime identity does not match the injected profile scope",
            ));
        }
        Ok(Self {
            profile_id,
            runtime,
            authority,
        })
    }

    pub(crate) fn profile_id(&self) -> &tracedecay_domain::UserProfileId {
        &self.profile_id
    }

    fn validate_owner(
        &self,
        owner: &tracedecay_store::EvidenceAssemblyOwnerV1,
    ) -> tracedecay_store::EvidenceAssemblyStoreResult<()> {
        owner.validate()?;
        let project_matches = match &self.runtime.binding().shard_id.scope {
            tracedecay_store::StoreShardScopeV1::Project { project_id }
            | tracedecay_store::StoreShardScopeV1::ProjectSessions { project_id } => {
                owner.owner.project_id() == Some(project_id)
            }
            tracedecay_store::StoreShardScopeV1::ProfileSessions => {
                owner.owner.project_id().is_none()
            }
            _ => false,
        };
        if owner.owner.profile_id() != &self.profile_id || !project_matches {
            return Err(tracedecay_store::EvidenceAssemblyStoreError::Unavailable);
        }
        Ok(())
    }

    pub(crate) async fn resolve_anchor(
        &self,
        context: &ProductRequestContext,
        owner: &tracedecay_store::EvidenceAssemblyOwnerV1,
        anchor_id: &RetrievalAnchorId,
    ) -> tracedecay_store::EvidenceAssemblyStoreResult<EvidenceAssemblyAnchorResolutionV1> {
        authorize_runtime_anchor_resolution_at(context, owner, evidence_runtime_now())?;
        self.validate_owner(owner)?;
        anchor_id.validate().map_err(evidence_runtime_invalid)?;

        let anchor_owner = tracedecay_store::RetrievalAnchorOwnerV1::V3(owner.owner.clone());
        let database =
            Database::publish_runtime(self.runtime.clone(), DatabaseAccessMode::ReadOnly)
                .await
                .map_err(|_| tracedecay_store::EvidenceAssemblyStoreError::Unavailable)?;
        if database.canonical_database_path() != self.authority.canonical_database_path() {
            return Err(tracedecay_store::EvidenceAssemblyStoreError::Unavailable);
        }
        let snapshot = database
            .begin_engine_read_snapshot("resolve evidence assembly anchor")
            .await
            .map_err(|_| tracedecay_store::EvidenceAssemblyStoreError::Unavailable)?;
        resolve_anchor_snapshot(&snapshot, anchor_id, &anchor_owner).await
    }

    fn read(
        &self,
        operation: tracedecay_store::EvidenceAssemblyReadOperationV1,
    ) -> tracedecay_store::EvidenceAssemblyStoreResult<tracedecay_store::EvidenceAssemblyReadResultV1>
    {
        match &operation {
            tracedecay_store::EvidenceAssemblyReadOperationV1::PublicationByIdempotency {
                owner,
                ..
            }
            | tracedecay_store::EvidenceAssemblyReadOperationV1::ContributionPage {
                owner, ..
            } => self.validate_owner(owner)?,
        }
        let request = evidence_runtime_read_request(self.runtime.binding(), operation)?;
        let probe = EvidenceRuntimeProbe::from_control(request.control());
        let outcome = self
            .runtime
            .dispatch_read(request, &probe)
            .map_err(|_| tracedecay_store::EvidenceAssemblyStoreError::Unavailable)?;
        if !matches!(
            outcome.coverage(),
            tracedecay_store::RuntimeReadCoverageV1::Latest { .. }
                | tracedecay_store::RuntimeReadCoverageV1::Complete { .. }
        ) {
            return Err(tracedecay_store::EvidenceAssemblyStoreError::Unavailable);
        }
        let result = match outcome.value() {
            Some(tracedecay_store::RuntimeReadResultV1::Repository {
                result: tracedecay_store::RepositoryReadResultV1::Project(project),
            }) => match project.as_ref() {
                tracedecay_store::ProjectReadResultV1::EvidenceAssembly(result) => result.clone(),
                _ => return Err(tracedecay_store::EvidenceAssemblyStoreError::Unavailable),
            },
            _ => return Err(tracedecay_store::EvidenceAssemblyStoreError::Unavailable),
        };
        match &result {
            tracedecay_store::EvidenceAssemblyReadResultV1::Publication(Some(receipt)) => {
                self.validate_owner(&receipt.owner)?;
                receipt.validate()?;
            }
            tracedecay_store::EvidenceAssemblyReadResultV1::ContributionPage(Some(page)) => {
                self.validate_owner(&page.contribution.owner)?;
                if page.span.owner != page.contribution.owner.owner {
                    return Err(evidence_runtime_invalid(
                        "evidence runtime drilldown owner mismatch",
                    ));
                }
                page.contribution.validate()?;
                page.span.validate()?;
                for occurrence in &page.occurrences {
                    occurrence.validate()?;
                    if occurrence.owner != page.contribution.owner.owner {
                        return Err(evidence_runtime_invalid(
                            "evidence runtime occurrence owner mismatch",
                        ));
                    }
                }
            }
            _ => {}
        }
        Ok(result)
    }
}

async fn resolve_anchor_snapshot(
    snapshot: &(impl QueryExecutor + Sync),
    anchor_id: &RetrievalAnchorId,
    owner: &tracedecay_store::RetrievalAnchorOwnerV1,
) -> tracedecay_store::EvidenceAssemblyStoreResult<EvidenceAssemblyAnchorResolutionV1> {
    let owner_json = serde_json::to_string(owner)
        .map_err(|_| tracedecay_store::EvidenceAssemblyStoreError::Unavailable)?;
    let mut rows = snapshot
        .query(
            "SELECT anchor.anchor_json, anchor.projection_generation,
                    disposition.disposition_id, disposition.state,
                    disposition.superseded_by, disposition.reason_class,
                    disposition.effective_at, disposition.record_json
             FROM retrieval_anchors AS anchor
             LEFT JOIN retrieval_anchor_dispositions AS disposition
               ON disposition.sequence = (
                   SELECT latest.sequence
                   FROM retrieval_anchor_dispositions AS latest
                   WHERE latest.anchor_id = anchor.anchor_id
                     AND latest.owner_json = anchor.owner_json
                   ORDER BY latest.sequence DESC LIMIT 1
               )
             WHERE anchor.anchor_id = ?1 AND anchor.owner_json = ?2",
            params![anchor_id.as_str(), owner_json.as_str()],
        )
        .await
        .map_err(|_| tracedecay_store::EvidenceAssemblyStoreError::Unavailable)?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|_| tracedecay_store::EvidenceAssemblyStoreError::Unavailable)?
    else {
        return Ok(EvidenceAssemblyAnchorResolutionV1::Unavailable);
    };
    let anchor_json: String = row
        .get(0)
        .map_err(|_| tracedecay_store::EvidenceAssemblyStoreError::Unavailable)?;
    let projection_generation: String = row
        .get(1)
        .map_err(|_| tracedecay_store::EvidenceAssemblyStoreError::Unavailable)?;
    let disposition_columns = (
        row.get::<Option<String>>(2),
        row.get::<Option<String>>(3),
        row.get::<Option<String>>(4),
        row.get::<Option<String>>(5),
        row.get::<Option<i64>>(6),
        row.get::<Option<String>>(7),
    );
    drop(rows);

    let disposition = match disposition_columns {
        (Ok(None), Ok(None), Ok(None), Ok(None), Ok(None), Ok(None)) => None,
        (
            Ok(Some(disposition_id)),
            Ok(Some(state)),
            Ok(superseded_by),
            Ok(Some(reason_class)),
            Ok(Some(effective_at)),
            Ok(Some(record_json)),
        ) => {
            let record: tracedecay_store::RetrievalAnchorDispositionRecordV1 =
                serde_json::from_str(&record_json)
                    .map_err(|_| tracedecay_store::EvidenceAssemblyStoreError::Unavailable)?;
            record
                .validate()
                .map_err(|_| tracedecay_store::EvidenceAssemblyStoreError::Unavailable)?;
            if record.disposition_id() != disposition_id
                || record.anchor_id() != anchor_id
                || record.owner() != owner
                || record.state().as_str() != state
                || record.superseded_by().map(RetrievalAnchorId::as_str) != superseded_by.as_deref()
                || record.reason_class().as_str() != reason_class
                || record.effective_at().0 != effective_at
            {
                return Err(tracedecay_store::EvidenceAssemblyStoreError::Unavailable);
            }
            Some(record)
        }
        _ => return Err(tracedecay_store::EvidenceAssemblyStoreError::Unavailable),
    };

    if let Some(disposition) = disposition {
        if matches!(
            disposition.state(),
            tracedecay_store::AnchorDispositionStateV1::Redacted
                | tracedecay_store::AnchorDispositionStateV1::Expired
                | tracedecay_store::AnchorDispositionStateV1::Quarantined
                | tracedecay_store::AnchorDispositionStateV1::Deleted
                | tracedecay_store::AnchorDispositionStateV1::Unavailable
        ) {
            let tombstone = tracedecay_store::RetrievalAnchorTombstoneV1::new(
                anchor_id.clone(),
                owner.clone(),
                disposition.state(),
                disposition.reason_class(),
                disposition.effective_at(),
            )
            .map_err(|_| tracedecay_store::EvidenceAssemblyStoreError::Unavailable)?;
            return Ok(EvidenceAssemblyAnchorResolutionV1::Tombstone(tombstone));
        }
        if disposition.state() != tracedecay_store::AnchorDispositionStateV1::Active {
            return Ok(EvidenceAssemblyAnchorResolutionV1::Unavailable);
        }
    }

    let record: tracedecay_store::StoredRetrievalAnchorRecordV1 =
        serde_json::from_str(&anchor_json)
            .map_err(|_| tracedecay_store::EvidenceAssemblyStoreError::Unavailable)?;
    record
        .validate()
        .map_err(|_| tracedecay_store::EvidenceAssemblyStoreError::Unavailable)?;
    if record.anchor_id() != anchor_id
        || record.owner() != owner.clone()
        || record.projection_generation().as_str() != projection_generation
    {
        return Err(tracedecay_store::EvidenceAssemblyStoreError::Unavailable);
    }

    let mut rows = snapshot
        .query(
            "SELECT lineage.derivative_kind, lineage.derivative_id,
                    lineage.direct_evidence
             FROM retrieval_anchor_reverse_lineage AS lineage
             WHERE lineage.source_anchor_id = ?1 AND lineage.owner_json = ?2
               AND NOT EXISTS (
                   SELECT 1
                   FROM retrieval_anchor_derivative_tombstones AS tombstone
                   WHERE tombstone.source_anchor_id = lineage.source_anchor_id
                     AND tombstone.owner_json = lineage.owner_json
                     AND tombstone.derivative_kind = lineage.derivative_kind
                     AND tombstone.derivative_id = lineage.derivative_id
               )
             ORDER BY lineage.derivative_kind, lineage.derivative_id",
            params![anchor_id.as_str(), owner_json],
        )
        .await
        .map_err(|_| tracedecay_store::EvidenceAssemblyStoreError::Unavailable)?;
    let mut derivatives = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|_| tracedecay_store::EvidenceAssemblyStoreError::Unavailable)?
    {
        let kind = tracedecay_store::AnchorDerivativeKindV1::parse(
            &row.get::<String>(0)
                .map_err(|_| tracedecay_store::EvidenceAssemblyStoreError::Unavailable)?,
        )
        .map_err(|_| tracedecay_store::EvidenceAssemblyStoreError::Unavailable)?;
        let derivative_id: String = row
            .get(1)
            .map_err(|_| tracedecay_store::EvidenceAssemblyStoreError::Unavailable)?;
        let direct_evidence: i64 = row
            .get(2)
            .map_err(|_| tracedecay_store::EvidenceAssemblyStoreError::Unavailable)?;
        if !matches!(direct_evidence, 0 | 1) {
            return Err(tracedecay_store::EvidenceAssemblyStoreError::Unavailable);
        }
        derivatives.push(
            tracedecay_store::RetrievalAnchorDerivativeV1::new(
                anchor_id.clone(),
                owner.clone(),
                kind,
                derivative_id,
                direct_evidence == 1,
            )
            .map_err(|_| tracedecay_store::EvidenceAssemblyStoreError::Unavailable)?,
        );
    }
    Ok(EvidenceAssemblyAnchorResolutionV1::Resolved {
        record,
        derivatives,
    })
}

fn authorize_runtime_anchor_resolution_at(
    context: &ProductRequestContext,
    owner: &tracedecay_store::EvidenceAssemblyOwnerV1,
    observed_at: UtcMicros,
) -> tracedecay_store::EvidenceAssemblyStoreResult<()> {
    owner.validate()?;
    if context.validate().is_err()
        || context.admission_at(observed_at) != RequestAdmission::Admitted
        || context.grant().disclosure < DisclosureClass::Evidence
        || owner.owner.project_id() != Some(&context.scope().project_id)
        || owner.scope_digest != context.scope().scope_digest
    {
        return Err(tracedecay_store::EvidenceAssemblyStoreError::Unavailable);
    }
    Ok(())
}

impl tracedecay_store::EvidenceAssemblyStore for RuntimeEvidenceAssemblyStore {
    async fn publish_or_replay(
        &self,
        write: tracedecay_store::EvidenceAssemblyWriteV1,
    ) -> tracedecay_store::EvidenceAssemblyStoreResult<
        tracedecay_store::EvidenceAssemblyPublicationOutcomeV1,
    > {
        write.validate()?;
        self.validate_owner(&write.owner)?;
        let expected = write.receipt.clone();
        let read = tracedecay_store::EvidenceAssemblyReadOperationV1::PublicationByIdempotency {
            owner: write.owner.clone(),
            idempotency_key: write.idempotency_key.clone(),
        };
        let request = evidence_runtime_submit_request(self.runtime.binding(), write)?;
        let probe = Arc::new(EvidenceRuntimeProbe::from_control(request.control()));
        let replayed = match self
            .runtime
            .dispatch_submit_authorized(request, probe, self.authority.clone())
            .await
            .map_err(|_| tracedecay_store::EvidenceAssemblyStoreError::Unavailable)?
        {
            tracedecay_store::RuntimeSubmitOutcomeV1::Committed { .. }
            | tracedecay_store::RuntimeSubmitOutcomeV1::CommittedAfterCancellation { .. } => false,
            tracedecay_store::RuntimeSubmitOutcomeV1::ExactReplay { .. } => true,
            tracedecay_store::RuntimeSubmitOutcomeV1::IdempotencyConflict { .. } => {
                return Err(tracedecay_store::EvidenceAssemblyStoreError::ReplayConflict);
            }
            _ => return Err(tracedecay_store::EvidenceAssemblyStoreError::Unavailable),
        };
        let receipt = match self.read(read)? {
            tracedecay_store::EvidenceAssemblyReadResultV1::Publication(Some(receipt))
                if receipt == expected =>
            {
                receipt
            }
            tracedecay_store::EvidenceAssemblyReadResultV1::Publication(Some(_)) => {
                return Err(tracedecay_store::EvidenceAssemblyStoreError::ReplayConflict);
            }
            _ => return Err(tracedecay_store::EvidenceAssemblyStoreError::Unavailable),
        };
        Ok(if replayed {
            tracedecay_store::EvidenceAssemblyPublicationOutcomeV1::Replayed(receipt)
        } else {
            tracedecay_store::EvidenceAssemblyPublicationOutcomeV1::Published(receipt)
        })
    }

    fn drilldown_contribution(
        &self,
        owner: &tracedecay_store::EvidenceAssemblyOwnerV1,
        contribution_id: &tracedecay_domain::RetrieverContributionIdV1,
        start_ordinal: u64,
        page_size: u64,
    ) -> impl std::future::Future<
        Output = tracedecay_store::EvidenceAssemblyStoreResult<
            Option<tracedecay_store::EvidenceAssemblyDrilldownPageV1>,
        >,
    > + Send {
        let owner = owner.clone();
        let contribution_id = contribution_id.clone();
        async move {
            match self.read(
                tracedecay_store::EvidenceAssemblyReadOperationV1::ContributionPage {
                    owner,
                    contribution_id,
                    start_ordinal,
                    page_size,
                },
            )? {
                tracedecay_store::EvidenceAssemblyReadResultV1::ContributionPage(page) => Ok(page),
                tracedecay_store::EvidenceAssemblyReadResultV1::Publication(_) => {
                    Err(tracedecay_store::EvidenceAssemblyStoreError::Unavailable)
                }
            }
        }
    }
}

fn evidence_runtime_submit_request(
    binding: &tracedecay_store::StoreRuntimeBindingV1,
    write: tracedecay_store::EvidenceAssemblyWriteV1,
) -> tracedecay_store::EvidenceAssemblyStoreResult<tracedecay_store::RuntimeSubmitRequestV1> {
    let command_digest = canonical_sha256(&write).map_err(evidence_runtime_invalid)?;
    let suffix = evidence_runtime_digest_suffix(command_digest.as_str())?;
    let idempotency_suffix =
        evidence_runtime_digest_suffix(write.idempotency_key.as_digest().as_str())?.to_owned();
    let admitted_at = evidence_runtime_now();
    let admission_bytes = serde_json::to_vec(&write)
        .map_err(evidence_runtime_invalid)?
        .len();
    let payload = tracedecay_store::RepositoryWritePayloadV1::EvidenceAssembly(Box::new(write));
    let metadata = tracedecay_store::StoreOperationMetadataV1 {
        operation_id: tracedecay_store::StoreOperationIdV1::new(format!(
            "operation.evidence-assembly.{suffix}"
        ))
        .map_err(evidence_runtime_invalid)?,
        client_id: tracedecay_store::StoreClientIdV1::new("client.evidence-assembly")
            .map_err(evidence_runtime_invalid)?,
        shard_id: binding.shard_id.clone(),
        incarnation: binding.incarnation,
        authority_epoch: binding.authority_epoch,
        idempotency: tracedecay_store::IdempotencyIdentityV1 {
            key: tracedecay_store::StoreIdempotencyKeyV1::new(format!(
                "evidence-assembly.{idempotency_suffix}"
            ))
            .map_err(evidence_runtime_invalid)?,
            command_digest: tracedecay_store::CommandDigestV1::new(command_digest.as_str())
                .map_err(evidence_runtime_invalid)?,
        },
        durability: tracedecay_store::DurabilityClassV1::Full,
        priority: tracedecay_store::OperationPriorityV1::Foreground,
        admission_bytes: u64::try_from(admission_bytes).unwrap_or(u64::MAX).max(1),
        admitted_at,
    };
    let compatibility = tracedecay_store::RuntimeBatchCompatibilityV1::from_operation(&metadata)
        .map_err(evidence_runtime_invalid)?;
    let transaction_scope = tracedecay_store::RuntimeTransactionScopeV1 {
        transaction_id: tracedecay_store::RuntimeTransactionIdV1::new(format!(
            "transaction.{}",
            metadata.operation_id.as_str()
        ))
        .map_err(evidence_runtime_invalid)?,
        compatibility,
        opened_at: admitted_at,
    };
    tracedecay_store::RuntimeSubmitRequestV1::new(
        tracedecay_store::RepositoryOperationEnvelopeV1 { metadata, payload },
        transaction_scope,
        evidence_runtime_control(suffix, admitted_at)?,
    )
    .map_err(evidence_runtime_invalid)
}

fn evidence_runtime_read_request(
    binding: &tracedecay_store::StoreRuntimeBindingV1,
    operation: tracedecay_store::EvidenceAssemblyReadOperationV1,
) -> tracedecay_store::EvidenceAssemblyStoreResult<tracedecay_store::RuntimeReadRequestV1> {
    let command_digest = canonical_sha256(&operation).map_err(evidence_runtime_invalid)?;
    let suffix = evidence_runtime_digest_suffix(command_digest.as_str())?;
    let admission_bytes = serde_json::to_vec(&operation)
        .map_err(evidence_runtime_invalid)?
        .len();
    let requested_at = evidence_runtime_now();
    tracedecay_store::RuntimeReadRequestV1::new(
        binding.clone(),
        tracedecay_store::ConsistencyModeV1::LatestAvailable,
        tracedecay_store::RuntimeReadOperationV1::Repository {
            op: tracedecay_store::RepositoryReadOperationV1::Project(
                tracedecay_store::ProjectReadOperationV1::EvidenceAssembly(operation),
            ),
        },
        tracedecay_store::OperationPriorityV1::Foreground,
        u64::try_from(admission_bytes).unwrap_or(u64::MAX).max(1),
        evidence_runtime_control(suffix, requested_at)?,
    )
    .map_err(evidence_runtime_invalid)
}

fn evidence_runtime_control(
    suffix: &str,
    requested_at: UtcMicros,
) -> tracedecay_store::EvidenceAssemblyStoreResult<tracedecay_store::RuntimeRequestControlV1> {
    Ok(tracedecay_store::RuntimeRequestControlV1 {
        requested_at,
        deadline: tracedecay_store::RuntimeDeadlineV1 {
            deadline_id: tracedecay_store::RuntimeDeadlineIdV1::new(format!(
                "deadline.evidence-assembly.{suffix}"
            ))
            .map_err(evidence_runtime_invalid)?,
        },
        cancellation: tracedecay_store::RuntimeCancellationIdentityV1 {
            cancellation_id: tracedecay_store::RuntimeCancellationIdV1::new(format!(
                "cancellation.evidence-assembly.{suffix}"
            ))
            .map_err(evidence_runtime_invalid)?,
            generation: 1,
        },
    })
}

fn evidence_runtime_digest_suffix(
    digest: &str,
) -> tracedecay_store::EvidenceAssemblyStoreResult<&str> {
    digest
        .strip_prefix("sha256:")
        .ok_or_else(|| evidence_runtime_invalid("non-SHA-256 evidence runtime digest"))
}

fn evidence_runtime_now() -> UtcMicros {
    let micros = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros();
    UtcMicros(i64::try_from(micros).unwrap_or(i64::MAX))
}

struct EvidenceRuntimeProbe {
    cancellation: tracedecay_store::RuntimeCancellationIdentityV1,
    deadline: tracedecay_store::RuntimeDeadlineV1,
}

impl EvidenceRuntimeProbe {
    fn from_control(control: &tracedecay_store::RuntimeRequestControlV1) -> Self {
        Self {
            cancellation: control.cancellation.clone(),
            deadline: control.deadline.clone(),
        }
    }
}

impl tracedecay_store::RuntimeRequestProbeV1 for EvidenceRuntimeProbe {
    fn cancellation_identity(&self) -> &tracedecay_store::RuntimeCancellationIdentityV1 {
        &self.cancellation
    }

    fn deadline_identity(&self) -> &tracedecay_store::RuntimeDeadlineV1 {
        &self.deadline
    }

    fn interruption(&self) -> Option<tracedecay_store::RuntimeInterruptionV1> {
        None
    }
}

fn evidence_runtime_invalid(
    error: impl std::fmt::Display,
) -> tracedecay_store::EvidenceAssemblyStoreError {
    tracedecay_store::EvidenceAssemblyStoreError::InvalidData(error.to_string())
}

#[cfg(test)]
#[path = "evidence_assembly_tests.rs"]
mod tests;
