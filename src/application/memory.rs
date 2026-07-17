//! Canonical memory use cases over the append-only fact authority.

use std::error::Error as StdError;
use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};

use sha2::{Digest, Sha256};
use thiserror::Error;
use tracedecay_domain::{
    ActorId, Confidence, DomainError, FactCategoryV1, FactId, FactLineageEventV1, FactOwnerV1,
    LocatorDigest, ProvenanceId, RetrievalAnchorId, RetrievalAnchorRecordV2, SourceStoreId,
};
use tracedecay_store::{
    CompatibilityDashboardFactDetailQueryV1, CompatibilityDashboardFactDetailV1,
    CompatibilityDashboardMemoryOverviewQueryV1, CompatibilityDashboardMemoryOverviewV1,
    CompatibilityDashboardOplogEntryV1, CompatibilityDashboardOplogQueryV1,
    CompatibilityDashboardVectorPointV1, CompatibilityDashboardVectorPointsQueryV1,
    CompatibilityFactAddAliasV1, CompatibilityFactAddCommandV1, CompatibilityFactAddOutcomeV1,
    CompatibilityFactContentDigestQueryV1, CompatibilityFactContradictionPageV1,
    CompatibilityFactContradictionQueryV1, CompatibilityFactCurationBatchV1,
    CompatibilityFactCurationOperationV1, CompatibilityFactCurationReceiptV1,
    CompatibilityFactFeedbackActionV1, CompatibilityFactFeedbackCommandV1,
    CompatibilityFactFeedbackDetailsAvailabilityV1, CompatibilityFactFeedbackHistoryQueryV1,
    CompatibilityFactFeedbackHistoryV1, CompatibilityFactFeedbackOutcomeV1,
    CompatibilityFactHistoryQueryV1, CompatibilityFactHistoryV1, CompatibilityFactInspectionV1,
    CompatibilityFactLinkV1, CompatibilityFactListQueryV1, CompatibilityFactMergeCommandV1,
    CompatibilityFactMergeEntitiesV1, CompatibilityFactMergeOutcomeV1,
    CompatibilityFactNormalizeTagsV1, CompatibilityFactPageV1, CompatibilityFactProjectionV1,
    CompatibilityFactProposalImportReceiptV1, CompatibilityFactProposalImportV1,
    CompatibilityFactProposalPageV1, CompatibilityFactProposalPromotionDispositionV1,
    CompatibilityFactProposalPromotionResultV1, CompatibilityFactProposalPromotionV1,
    CompatibilityFactProposalRecordV1, CompatibilityFactProposalRevisionV1,
    CompatibilityFactProposalStateV1, CompatibilityFactRelationV1,
    CompatibilityFactRemoveCommandV1, CompatibilityFactRemoveOutcomeV1,
    CompatibilityFactRepairVectorV1, CompatibilityFactRetrievalCommandV1,
    CompatibilityFactSearchCursorV1, CompatibilityFactSearchFilterV1,
    CompatibilityFactSearchKindV1, CompatibilityFactSearchPageV1, CompatibilityFactSearchQuery,
    CompatibilityFactTargetV1, CompatibilityFactUpdateCommandV1, CompatibilityFactUpdateOutcomeV1,
    CompatibilityFactUpdatePatchV1, CompatibilityFeedbackRepairProgressV1,
    CompatibilityLegacyEntityTargetV1, CompatibilityLegacyMemoryCutoverCommandV1,
    CompatibilityLegacyMemoryCutoverProgressV1, CompatibilityMemoryRepairCommandV1,
    CompatibilityMemoryRepairStatsV1, CompatibilityMemoryStatusV1, CurrentFactsQuery,
    FactAsOfQuery, FactCommitOutcome, FactCompatibilityStore, FactCompatibilityStoreError,
    FactCurrentQuery, FactLineageQuery, FactProposalStore, FactProposalStoreError, FactStore,
    FactStoreError, FactWriteBatch, LegacyFactQuery, PromoteFactProposal,
    PromoteFactProposalOutcome, RetrievalAnchorQuery, StoredFactV1,
};

use crate::application::anchor_resolution::{
    EvidenceAnchorReportResolver, EvidenceAnchorResolutionReport,
};
use crate::memory::hygiene::detect_secret_like;
use crate::memory::trust::DEFAULT_TRUST;
use crate::memory::types::{
    AddFactDiff, AddFactDiffKind, AddFactOutcome, AddFactRequest, ContradictionResult, FactRecord,
    FactRelationKind, FactSearchResult, FeedbackAction, FeedbackRequest, MemoryCategory,
    MemoryFeedbackFunnel, MemoryGroomingOperation, MemoryGroomingReport, MemoryRepairStats,
    MemoryStatus, SearchFactsRequest, TrustHistoryEntry, UpdateFactRequest,
};
use crate::privacy::{
    MemoryFactSanitizationV1, sanitize_memory_fact_payload, sanitize_provider_metadata_text,
};
use crate::sessions::source::canonical_framed_sha256;

#[derive(Debug, Error)]
pub enum MemoryApplicationError {
    #[error("memory owner is invalid")]
    InvalidOwner(#[from] DomainError),
    #[error("evidence anchor is invalid")]
    InvalidEvidenceAnchor(#[source] DomainError),
    #[error("memory request owner does not match the application scope")]
    OwnerMismatch {
        scope: FactOwnerV1,
        request_owner: FactOwnerV1,
    },
    #[error("fact store operation failed")]
    Store(#[from] FactStoreError),
    #[error("memory authority operation failed")]
    Authority(#[from] FactProposalStoreError),
    #[error("memory compatibility authority operation failed")]
    Compatibility(#[from] FactCompatibilityStoreError),
    #[error("memory compatibility input is invalid: {invariant}")]
    InvalidCompatibilityInput { invariant: &'static str },
    #[error("memory compatibility projection cannot be represented by the V1 surface: {invariant}")]
    IncompatibleLegacyProjection { invariant: &'static str },
    #[error("memory authority returned a result violating {invariant}")]
    InvalidAuthorityResult { invariant: &'static str },
    #[error("memory feedback history is unavailable while repair is {progress:?}")]
    FeedbackHistoryUnavailable {
        progress: CompatibilityFeedbackRepairProgressV1,
    },
    #[error("evidence anchor resolution failed")]
    EvidenceAnchor(#[from] EvidenceAnchorResolutionError),
}

/// Stable source identity for the V1 memory mirror. It is product-owned, not
/// derived from a path, database name, or caller input.
pub const RUNTIME_MEMORY_COMPATIBILITY_SOURCE_STORE: &str = "legacy-memory-v1";

/// Immutable identity boundary for V1 numeric fact IDs. The authority remains
/// the sole resolver of the numeric mapping inside its transaction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemoryCompatibilityScope {
    owner: FactOwnerV1,
    source_store_id: SourceStoreId,
}

impl MemoryCompatibilityScope {
    pub fn runtime(owner: FactOwnerV1) -> Result<Self, MemoryApplicationError> {
        Self::new(
            owner,
            SourceStoreId::new(RUNTIME_MEMORY_COMPATIBILITY_SOURCE_STORE).map_err(|_| {
                MemoryApplicationError::InvalidCompatibilityInput {
                    invariant: "runtime compatibility source store identity",
                }
            })?,
        )
    }

    pub fn new(
        owner: FactOwnerV1,
        source_store_id: SourceStoreId,
    ) -> Result<Self, MemoryApplicationError> {
        owner.validate()?;
        source_store_id.validate().map_err(|_| {
            MemoryApplicationError::InvalidCompatibilityInput {
                invariant: "compatibility source store identity",
            }
        })?;
        if source_store_id.as_str() != RUNTIME_MEMORY_COMPATIBILITY_SOURCE_STORE {
            return Err(MemoryApplicationError::InvalidCompatibilityInput {
                invariant: "fixed V1 compatibility source store identity",
            });
        }
        Ok(Self {
            owner,
            source_store_id,
        })
    }

    pub fn owner(&self) -> &FactOwnerV1 {
        &self.owner
    }

    pub fn source_store_id(&self) -> &SourceStoreId {
        &self.source_store_id
    }
}

/// Trusted daemon-issued identity for one V1-facing operation. The raw
/// JSON-RPC identifier is never retained: it is domain-separated and hashed
/// with owner and action before it reaches the fact authority.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemoryOperationContext {
    operation_id: ProvenanceId,
    actor: Option<ActorId>,
}

impl MemoryOperationContext {
    pub fn from_trusted_request_id(
        owner: &FactOwnerV1,
        action: &str,
        request_id: &str,
        actor: Option<ActorId>,
    ) -> Result<Self, MemoryApplicationError> {
        owner.validate()?;
        validate_operation_component(action, "memory operation action")?;
        validate_operation_component(request_id, "memory request identity")?;
        if let Some(actor) = &actor {
            actor
                .validate()
                .map_err(|_| MemoryApplicationError::InvalidCompatibilityInput {
                    invariant: "memory operation actor",
                })?;
        }
        let owner = match owner {
            FactOwnerV1::Profile => "profile".to_owned(),
            FactOwnerV1::Project { project_id } => format!("project:{}", project_id.as_str()),
        };
        let digest = canonical_framed_sha256(
            b"tracedecay.memory.operation.v1",
            &[owner.as_bytes(), action.as_bytes(), request_id.as_bytes()],
        );
        let operation_id =
            ProvenanceId::new(format!("memory-operation.v1.{digest}")).map_err(|_| {
                MemoryApplicationError::InvalidCompatibilityInput {
                    invariant: "derived memory operation identity",
                }
            })?;
        Ok(Self {
            operation_id,
            actor,
        })
    }

    /// Use only for direct non-retriable core calls without a daemon request
    /// identity. Retriable transports must use [`Self::from_trusted_request_id`].
    pub fn generated(
        owner: &FactOwnerV1,
        action: &str,
        actor: Option<ActorId>,
    ) -> Result<Self, MemoryApplicationError> {
        static NONCE: AtomicU64 = AtomicU64::new(0);
        let mut bytes = [0_u8; 16];
        let raw = match getrandom::getrandom(&mut bytes) {
            Ok(()) => format!("generated:{}", hex::encode(bytes)),
            Err(_) => format!(
                "generated:{}:{}",
                crate::runtime_identity::process_run_id(),
                NONCE.fetch_add(1, Ordering::Relaxed)
            ),
        };
        Self::from_trusted_request_id(owner, action, &raw, actor)
    }

    pub fn operation_id(&self) -> &ProvenanceId {
        &self.operation_id
    }

    pub fn actor(&self) -> Option<&ActorId> {
        self.actor.as_ref()
    }
}

fn validate_operation_component(
    value: &str,
    invariant: &'static str,
) -> Result<(), MemoryApplicationError> {
    if value.is_empty() || value.len() > 512 || value.chars().any(char::is_control) {
        return Err(MemoryApplicationError::InvalidCompatibilityInput { invariant });
    }
    Ok(())
}

/// V1 update preserves the existing rejected-secret response without issuing a
/// fact-authority write.
#[derive(Clone, Debug, PartialEq)]
pub enum V1UpdateFactOutcome {
    Updated(Box<FactRecord>),
    RejectedSecretLike { reason: String },
}

/// Finite V1 trust-history projection with explicit repair availability. The
/// entries retain the historical wire shape; callers can distinguish partial,
/// unknown, and complete history without inventing missing sources or events.
#[derive(Clone, Debug, PartialEq)]
pub struct V1FactTrustHistoryV1 {
    pub entries: Vec<TrustHistoryEntry>,
    pub repair_progress: CompatibilityFeedbackRepairProgressV1,
}

/// Legacy status fields and feedback-history repair state from one authority
/// snapshot. Consumers must use this instead of issuing two status reads.
#[derive(Clone, Debug, PartialEq)]
pub struct V1MemoryStatusWithRepairV1 {
    pub status: MemoryStatus,
    pub feedback_history_repair: CompatibilityFeedbackRepairProgressV1,
}

/// Converts one legacy proposal payload into the portable command consumed by
/// the authoritative proposal import. The operation identity is deterministic
/// across retries of the same immutable legacy record.
pub fn legacy_proposal_add_command(
    owner: FactOwnerV1,
    sidecar_digest: LocatorDigest,
    legacy_proposal_id: i64,
    request: AddFactRequest,
) -> Result<CompatibilityFactAddCommandV1, MemoryApplicationError> {
    owner.validate()?;
    let source_store_id =
        SourceStoreId::new(RUNTIME_MEMORY_COMPATIBILITY_SOURCE_STORE).map_err(|_| {
            MemoryApplicationError::InvalidCompatibilityInput {
                invariant: "runtime compatibility source store identity",
            }
        })?;
    sidecar_digest
        .validate()
        .map_err(|_| MemoryApplicationError::InvalidCompatibilityInput {
            invariant: "legacy proposal sidecar digest",
        })?;
    if legacy_proposal_id <= 0 {
        return Err(MemoryApplicationError::InvalidCompatibilityInput {
            invariant: "legacy proposal numeric identity",
        });
    }
    let request_id = format!(
        "{}:{}:{legacy_proposal_id}",
        source_store_id.as_str(),
        sidecar_digest.as_str()
    );
    let context = MemoryOperationContext::from_trusted_request_id(
        &owner,
        "legacy-proposal-import",
        &request_id,
        None,
    )?;
    let Some(request) = sanitize_add_fact_request(request)? else {
        return Err(MemoryApplicationError::InvalidCompatibilityInput {
            invariant: "legacy proposal rejected by memory privacy sanitizer",
        });
    };
    compatibility_add_command(owner, request, &context)
}

/// Converts a live automation proposal without manufacturing a legacy numeric
/// identity. The deterministic operation identity makes repeated processing of
/// the same run/proposal idempotent at the authority boundary.
pub fn automation_fact_proposal_add_command(
    owner: FactOwnerV1,
    request: AddFactRequest,
    run_id: &str,
    proposal_id: &str,
    actor: Option<ActorId>,
) -> Result<CompatibilityFactAddCommandV1, MemoryApplicationError> {
    owner.validate()?;
    validate_operation_component(run_id, "automation proposal run identity")?;
    validate_operation_component(proposal_id, "automation proposal identity")?;
    let context = MemoryOperationContext::from_trusted_request_id(
        &owner,
        "automation-fact-proposal",
        &format!("{run_id}:{proposal_id}"),
        actor,
    )?;
    let Some(request) = sanitize_add_fact_request(request)? else {
        return Err(MemoryApplicationError::InvalidCompatibilityInput {
            invariant: "automation proposal rejected by memory privacy sanitizer",
        });
    };
    with_automation_run_id(compatibility_add_command(owner, request, &context)?, run_id)
}

/// Binds the trusted run identity to command metadata after the payload has
/// been sanitized. It is never serialized into fact payload metadata.
pub fn with_automation_run_id(
    command: CompatibilityFactAddCommandV1,
    run_id: &str,
) -> Result<CompatibilityFactAddCommandV1, MemoryApplicationError> {
    validate_operation_component(run_id, "automation proposal run identity")?;
    command
        .with_automation_run_id(run_id.to_owned())
        .map_err(MemoryApplicationError::Store)
}

fn sanitize_add_fact_request(
    mut request: AddFactRequest,
) -> Result<Option<AddFactRequest>, MemoryApplicationError> {
    strip_reserved_automation_run_id(&mut request.metadata);
    // The canonical payload sorts labels before hashing; the sanitizer receipt
    // is computed over this wire, so it must see the same canonical order.
    request.tags.sort_unstable();
    request.entities.sort_unstable();
    if detect_secret_like(request.content.trim()).is_some() {
        return Ok(None);
    }
    let Some(source) = sanitize_optional_memory_text(request.source.clone()) else {
        return Ok(None);
    };
    let wire = serde_json::to_value(&request).map_err(|_| {
        MemoryApplicationError::InvalidCompatibilityInput {
            invariant: "legacy add request serialization",
        }
    })?;
    let MemoryFactSanitizationV1::Durable { payload, .. } = sanitize_memory_fact_payload(wire)
        .map_err(|_| MemoryApplicationError::InvalidCompatibilityInput {
            invariant: "legacy add request privacy sanitizer",
        })?
    else {
        return Ok(None);
    };
    let mut sanitized = serde_json::from_value::<AddFactRequest>(payload).map_err(|_| {
        MemoryApplicationError::InvalidCompatibilityInput {
            invariant: "sanitized legacy add request",
        }
    })?;
    sanitized.source = source;
    Ok(Some(sanitized))
}

fn sanitize_update_fact_request(
    mut request: UpdateFactRequest,
) -> Result<Option<UpdateFactRequest>, MemoryApplicationError> {
    if let Some(metadata) = request.metadata.as_mut() {
        strip_reserved_automation_run_id(metadata);
    }
    // Match the canonical payload's sorted label order (see the add path).
    if let Some(tags) = request.tags.as_mut() {
        tags.sort_unstable();
    }
    if let Some(entities) = request.entities.as_mut() {
        entities.sort_unstable();
    }
    if request
        .content
        .as_deref()
        .is_some_and(|content| detect_secret_like(content.trim()).is_some())
    {
        return Ok(None);
    }
    let Some(source) = sanitize_optional_memory_text(request.source.clone()) else {
        return Ok(None);
    };
    let wire = serde_json::to_value(&request).map_err(|_| {
        MemoryApplicationError::InvalidCompatibilityInput {
            invariant: "legacy update request serialization",
        }
    })?;
    let MemoryFactSanitizationV1::Durable { payload, .. } = sanitize_memory_fact_payload(wire)
        .map_err(|_| MemoryApplicationError::InvalidCompatibilityInput {
            invariant: "legacy update request privacy sanitizer",
        })?
    else {
        return Ok(None);
    };
    let mut sanitized = serde_json::from_value::<UpdateFactRequest>(payload).map_err(|_| {
        MemoryApplicationError::InvalidCompatibilityInput {
            invariant: "sanitized legacy update request",
        }
    })?;
    sanitized.source = source;
    Ok(Some(sanitized))
}

/// `automation_run_id` is typed command metadata. Never permit a caller to
/// smuggle it through a payload that will be persisted and privacy-scanned as
/// ordinary fact metadata.
fn strip_reserved_automation_run_id(metadata: &mut serde_json::Value) {
    if let serde_json::Value::Object(metadata) = metadata {
        metadata.remove("automation_run_id");
    }
}

fn sanitize_optional_memory_text(value: Option<String>) -> Option<Option<String>> {
    match value {
        Some(value) => sanitize_provider_metadata_text(&value).map(Some),
        None => Some(None),
    }
}

fn sanitize_curation_text(
    value: String,
    invariant: &'static str,
) -> Result<String, MemoryApplicationError> {
    sanitize_provider_metadata_text(&value)
        .ok_or(MemoryApplicationError::InvalidCompatibilityInput { invariant })
}

fn sanitize_curation_texts(
    values: Vec<String>,
    invariant: &'static str,
) -> Result<Vec<String>, MemoryApplicationError> {
    values
        .into_iter()
        .map(|value| sanitize_curation_text(value, invariant))
        .collect()
}

fn sanitize_curation_metadata(
    value: serde_json::Value,
) -> Result<serde_json::Value, MemoryApplicationError> {
    match sanitize_memory_fact_payload(value).map_err(|_| {
        MemoryApplicationError::InvalidCompatibilityInput {
            invariant: "dashboard curation metadata privacy sanitizer",
        }
    })? {
        MemoryFactSanitizationV1::Durable { payload, .. } => Ok(payload),
        MemoryFactSanitizationV1::Quarantined => {
            Err(MemoryApplicationError::InvalidCompatibilityInput {
                invariant: "dashboard curation metadata rejected by privacy sanitizer",
            })
        }
    }
}

fn compatibility_add_command(
    owner: FactOwnerV1,
    request: AddFactRequest,
    context: &MemoryOperationContext,
) -> Result<CompatibilityFactAddCommandV1, MemoryApplicationError> {
    let trust = Confidence::new(request.trust.unwrap_or(DEFAULT_TRUST)).map_err(|_| {
        MemoryApplicationError::InvalidCompatibilityInput {
            invariant: "legacy add trust",
        }
    })?;
    CompatibilityFactAddCommandV1::new(
        owner,
        context.operation_id().clone(),
        request.content,
        fact_category(request.category),
        request.source,
        request.tags,
        request.entities,
        request.metadata,
        trust,
        context.actor().cloned(),
    )
    .map_err(MemoryApplicationError::Store)
}

const fn fact_category(category: MemoryCategory) -> FactCategoryV1 {
    match category {
        MemoryCategory::General => FactCategoryV1::General,
        MemoryCategory::UserPref => FactCategoryV1::UserPref,
        MemoryCategory::Project => FactCategoryV1::Project,
        MemoryCategory::Tool => FactCategoryV1::Tool,
        MemoryCategory::Decision => FactCategoryV1::Decision,
        MemoryCategory::CodeArea => FactCategoryV1::CodeArea,
    }
}

const fn compatibility_relation(relation: FactRelationKind) -> CompatibilityFactRelationV1 {
    match relation {
        FactRelationKind::Supports => CompatibilityFactRelationV1::Supports,
        FactRelationKind::Contradicts => CompatibilityFactRelationV1::Contradicts,
        FactRelationKind::Supersedes => CompatibilityFactRelationV1::Supersedes,
        FactRelationKind::DerivedFrom => CompatibilityFactRelationV1::DerivedFrom,
    }
}

const fn memory_category(category: FactCategoryV1) -> MemoryCategory {
    match category {
        FactCategoryV1::General => MemoryCategory::General,
        FactCategoryV1::UserPref => MemoryCategory::UserPref,
        FactCategoryV1::Project => MemoryCategory::Project,
        FactCategoryV1::Tool => MemoryCategory::Tool,
        FactCategoryV1::Decision => MemoryCategory::Decision,
        FactCategoryV1::CodeArea => MemoryCategory::CodeArea,
    }
}

fn compatibility_confidence(
    value: Option<f64>,
) -> Result<Option<Confidence>, MemoryApplicationError> {
    value.map(Confidence::new).transpose().map_err(|_| {
        MemoryApplicationError::InvalidCompatibilityInput {
            invariant: "legacy memory confidence",
        }
    })
}

fn legacy_i64(value: u64, invariant: &'static str) -> Result<i64, MemoryApplicationError> {
    i64::try_from(value)
        .map_err(|_| MemoryApplicationError::IncompatibleLegacyProjection { invariant })
}

fn legacy_usize(value: u64, invariant: &'static str) -> Result<usize, MemoryApplicationError> {
    usize::try_from(value)
        .map_err(|_| MemoryApplicationError::IncompatibleLegacyProjection { invariant })
}

/// Projects one authoritative compatibility snapshot into the legacy status
/// shape. Keep this pure so callers cannot accidentally split status and
/// feedback-history repair across separate reads.
fn project_memory_status_v1(
    status: &CompatibilityMemoryStatusV1,
) -> Result<MemoryStatus, MemoryApplicationError> {
    let funnel = status.feedback_funnel();
    let repair = status.repair();
    Ok(MemoryStatus {
        fact_count: legacy_usize(status.fact_count(), "legacy memory fact count")?,
        entity_count: legacy_usize(status.entity_count(), "legacy memory entity count")?,
        bank_count: legacy_usize(status.bank_count(), "legacy memory bank count")?,
        algebra_name: status.algebra().name().to_owned(),
        hrr_dim: legacy_usize(status.algebra().hrr_dim(), "legacy memory hrr dimension")?,
        estimated_capacity: legacy_usize(
            status.algebra().estimated_capacity(),
            "legacy memory estimated capacity",
        )?,
        trust_0_025_count: legacy_usize(
            status.trust_0_025_count(),
            "legacy memory trust bucket 0-025",
        )?,
        trust_025_050_count: legacy_usize(
            status.trust_025_050_count(),
            "legacy memory trust bucket 025-050",
        )?,
        trust_050_075_count: legacy_usize(
            status.trust_050_075_count(),
            "legacy memory trust bucket 050-075",
        )?,
        trust_075_100_count: legacy_usize(
            status.trust_075_100_count(),
            "legacy memory trust bucket 075-100",
        )?,
        below_default_recall_threshold_count: legacy_usize(
            status.below_default_recall_threshold_count(),
            "legacy memory below recall threshold count",
        )?,
        helpful_count: legacy_usize(status.helpful_count(), "legacy memory helpful count")?,
        unhelpful_count: legacy_usize(status.unhelpful_count(), "legacy memory unhelpful count")?,
        missing_vector_count: legacy_usize(
            status.missing_vector_count(),
            "legacy memory missing vector count",
        )?,
        legacy_backfill_complete: status.legacy_backfill_complete(),
        repair: MemoryRepairStats {
            missing_vectors_repaired: legacy_usize(
                repair.missing_vectors_repaired(),
                "legacy memory repaired vectors",
            )?,
            banks_rebuilt: legacy_usize(repair.banks_rebuilt(), "legacy memory rebuilt banks")?,
        },
        feedback_funnel: MemoryFeedbackFunnel {
            retrieval_count_total: legacy_i64(
                funnel.retrieval_count_total(),
                "legacy memory retrieval count total",
            )?,
            access_count_total: legacy_i64(
                funnel.access_count_total(),
                "legacy memory access count total",
            )?,
            retrieved_fact_count: legacy_usize(
                funnel.retrieved_fact_count(),
                "legacy memory retrieved fact count",
            )?,
            rated_fact_count: legacy_usize(
                funnel.rated_fact_count(),
                "legacy memory rated fact count",
            )?,
            feedback_total: legacy_usize(funnel.feedback_total(), "legacy memory feedback total")?,
            seen_to_feedback_ratio: funnel
                .seen_to_feedback_ratio()
                .map(|value| legacy_i64(value, "legacy memory seen-to-feedback ratio"))
                .transpose()?,
        },
    })
}

fn compatibility_fact_record(
    scope: &MemoryCompatibilityScope,
    fact: &tracedecay_store::CompatibilityFactV1,
) -> Result<FactRecord, MemoryApplicationError> {
    if fact.owner() != scope.owner() {
        return Err(MemoryApplicationError::InvalidAuthorityResult {
            invariant: "legacy fact projection owner",
        });
    }
    let mapping = fact.mapping().legacy_mapping().ok_or(
        MemoryApplicationError::IncompatibleLegacyProjection {
            invariant: "legacy numeric fact mapping",
        },
    )?;
    if mapping.owner() != scope.owner() || mapping.source_store_id() != scope.source_store_id() {
        return Err(MemoryApplicationError::IncompatibleLegacyProjection {
            invariant: "legacy fact mapping source",
        });
    }
    let payload = fact
        .payload()
        .ok_or(MemoryApplicationError::IncompatibleLegacyProjection {
            invariant: "available legacy fact payload",
        })?;
    let telemetry = fact.telemetry();
    Ok(FactRecord {
        fact_id: mapping.legacy_fact_id(),
        content: payload.content().to_owned(),
        category: memory_category(payload.category()),
        tags: payload.tags().to_vec(),
        entities: payload.entities().to_vec(),
        trust_score: fact.fact().trust().as_f64(),
        source: fact.source_label().map(ToOwned::to_owned),
        retrieval_count: legacy_i64(telemetry.retrieval_count(), "legacy retrieval count")?,
        access_count: legacy_i64(telemetry.access_count(), "legacy access count")?,
        helpful_count: legacy_i64(telemetry.helpful_count(), "legacy helpful count")?,
        unhelpful_count: legacy_i64(telemetry.unhelpful_count(), "legacy unhelpful count")?,
        created_at: telemetry.created_at().0,
        updated_at: telemetry.updated_at().0,
        last_retrieved_at: telemetry.last_retrieved_at().map(|value| value.0),
        last_recalled_at: telemetry.last_recalled_at().map(|value| value.0),
        last_feedback_at: telemetry.last_feedback_at().map(|value| value.0),
        metadata: payload.metadata().clone(),
    })
}

fn compatibility_projection_record(
    scope: &MemoryCompatibilityScope,
    projection: &CompatibilityFactProjectionV1,
) -> Result<FactRecord, MemoryApplicationError> {
    match projection {
        CompatibilityFactProjectionV1::Available(fact) => compatibility_fact_record(scope, fact),
        CompatibilityFactProjectionV1::Unavailable(_) => {
            Err(MemoryApplicationError::IncompatibleLegacyProjection {
                invariant: "available legacy fact projection",
            })
        }
    }
}

/// Immutable daemon-authorized evidence record suitable for materialization in
/// a fact shard. It deliberately reuses the canonical retrieval-anchor model.
#[derive(Clone, Debug)]
pub struct ResolvedEvidenceAnchorV1 {
    record: RetrievalAnchorRecordV2,
}

impl ResolvedEvidenceAnchorV1 {
    pub fn new(record: RetrievalAnchorRecordV2) -> Result<Self, DomainError> {
        record.validate()?;
        Ok(Self { record })
    }

    pub fn anchor_id(&self) -> &RetrievalAnchorId {
        self.record.anchor_id()
    }

    pub fn record(&self) -> &RetrievalAnchorRecordV2 {
        &self.record
    }

    pub fn into_record(self) -> RetrievalAnchorRecordV2 {
        self.record
    }
}

#[derive(Debug, Error)]
pub enum EvidenceAnchorResolutionError {
    #[error("evidence anchor {anchor_id} is unavailable from the daemon authority")]
    Unavailable { anchor_id: RetrievalAnchorId },
    #[error("evidence anchor resolver operation {operation} failed")]
    Authority {
        operation: &'static str,
        #[source]
        source: Box<dyn StdError + Send + Sync>,
    },
}

/// Daemon/ingress-only boundary for resolving observation evidence that lives
/// outside the fact shard. Implementations must not expose a database handle.
pub trait EvidenceAnchorResolver: Send + Sync {
    fn resolve_evidence_anchor(
        &self,
        owner: FactOwnerV1,
        anchor_id: RetrievalAnchorId,
    ) -> impl Future<Output = Result<ResolvedEvidenceAnchorV1, EvidenceAnchorResolutionError>> + Send;
}

/// Owner-bound application service. Paths, connections, legacy integer IDs,
/// and transport payloads never enter this boundary.
pub struct MemoryApplication<A> {
    owner: FactOwnerV1,
    compatibility_scope: MemoryCompatibilityScope,
    authority: A,
}

impl<A: FactStore> MemoryApplication<A> {
    pub fn new(owner: FactOwnerV1, authority: A) -> Result<Self, MemoryApplicationError> {
        Self::new_with_compatibility_scope(MemoryCompatibilityScope::runtime(owner)?, authority)
    }

    /// Explicit construction path for a migrated V1 source with a typed,
    /// immutable source-store identity. Callers never derive this from a path
    /// or transport field.
    pub fn new_with_compatibility_scope(
        compatibility_scope: MemoryCompatibilityScope,
        authority: A,
    ) -> Result<Self, MemoryApplicationError> {
        compatibility_scope.owner().validate()?;
        Ok(Self {
            owner: compatibility_scope.owner().clone(),
            compatibility_scope,
            authority,
        })
    }

    pub fn owner(&self) -> &FactOwnerV1 {
        &self.owner
    }

    pub fn compatibility_scope(&self) -> &MemoryCompatibilityScope {
        &self.compatibility_scope
    }

    /// Resolves a daemon-authorized observation anchor before the caller
    /// materializes the returned record in `FactWriteBatch::new_anchors`.
    /// The fact shard never performs a cross-database anchor lookup itself.
    pub async fn resolve_evidence_anchor<R: EvidenceAnchorResolver>(
        &self,
        resolver: &R,
        anchor_id: RetrievalAnchorId,
    ) -> Result<RetrievalAnchorRecordV2, MemoryApplicationError> {
        anchor_id
            .validate()
            .map_err(MemoryApplicationError::InvalidEvidenceAnchor)?;
        let resolved = resolver
            .resolve_evidence_anchor(self.owner.clone(), anchor_id.clone())
            .await?;
        let record = resolved.into_record();
        if record.anchor_id() != &anchor_id
            || FactOwnerV1::from(record.owner().clone()) != self.owner
        {
            return Err(MemoryApplicationError::InvalidAuthorityResult {
                invariant: "resolved evidence anchor identity and owner",
            });
        }
        Ok(record)
    }

    /// Resolves a daemon-authorized observation anchor into its typed
    /// resolution report (state, coverage, watermark drift, and bounding
    /// authorization) before the caller materializes any returned record.
    /// The same owner and identity checks as `resolve_evidence_anchor` apply:
    /// a report never silently switches owner or anchor identity.
    pub async fn resolve_evidence_anchor_report<R: EvidenceAnchorReportResolver>(
        &self,
        resolver: &R,
        anchor_id: RetrievalAnchorId,
    ) -> Result<EvidenceAnchorResolutionReport, MemoryApplicationError> {
        anchor_id
            .validate()
            .map_err(MemoryApplicationError::InvalidEvidenceAnchor)?;
        let report = resolver
            .resolve_evidence_anchor_report(self.owner.clone(), anchor_id.clone())
            .await?;
        if report.anchor_id() != &anchor_id {
            return Err(MemoryApplicationError::InvalidAuthorityResult {
                invariant: "resolved evidence anchor report identity",
            });
        }
        if let Some(record) = report.record()
            && (record.anchor_id() != &anchor_id
                || FactOwnerV1::from(record.owner().clone()) != self.owner)
        {
            return Err(MemoryApplicationError::InvalidAuthorityResult {
                invariant: "resolved evidence anchor identity and owner",
            });
        }
        Ok(report)
    }

    pub async fn commit_fact(
        &self,
        batch: FactWriteBatch,
    ) -> Result<FactCommitOutcome, MemoryApplicationError> {
        self.ensure_owner(batch.owner())?;
        let expected_fact_id = batch.fact_id().clone();
        let outcome = self.authority.commit_fact(batch).await?;
        validate_commit_outcome(&self.owner, &expected_fact_id, &outcome)?;
        Ok(outcome)
    }

    pub async fn query_current_facts(
        &self,
        query: CurrentFactsQuery,
    ) -> Result<Vec<StoredFactV1>, MemoryApplicationError> {
        self.ensure_owner(query.owner())?;
        let after_fact_id = query.after_fact_id().cloned();
        let limit = query.limit();
        let facts = self.authority.query_current_facts(query).await?;
        validate_current_facts(&self.owner, after_fact_id.as_ref(), limit, &facts)?;
        Ok(facts)
    }

    pub async fn query_fact_as_of(
        &self,
        query: FactAsOfQuery,
    ) -> Result<Option<StoredFactV1>, MemoryApplicationError> {
        self.ensure_owner(query.owner())?;
        let fact_id = query.fact_id().clone();
        let as_of = query.as_of();
        let fact = self.authority.query_fact_as_of(query).await?;
        if let Some(fact) = &fact
            && (fact.owner() != &self.owner
                || fact.fact_id() != &fact_id
                || fact.projected_as_of() > as_of)
        {
            return Err(MemoryApplicationError::InvalidAuthorityResult {
                invariant: "as-of fact identity and timestamp",
            });
        }
        Ok(fact)
    }

    pub async fn query_fact_current(
        &self,
        query: FactCurrentQuery,
    ) -> Result<Option<StoredFactV1>, MemoryApplicationError> {
        self.ensure_owner(query.owner())?;
        let fact_id = query.fact_id().clone();
        let fact = self.authority.query_fact_current(query).await?;
        if let Some(fact) = &fact
            && (fact.owner() != &self.owner || fact.fact_id() != &fact_id)
        {
            return Err(MemoryApplicationError::InvalidAuthorityResult {
                invariant: "current fact identity",
            });
        }
        Ok(fact)
    }

    pub async fn query_fact_lineage(
        &self,
        query: FactLineageQuery,
    ) -> Result<Vec<FactLineageEventV1>, MemoryApplicationError> {
        self.ensure_owner(query.owner())?;
        let fact_id = query.fact_id().clone();
        let after = query.after().cloned();
        let limit = query.limit();
        let events = self.authority.query_fact_lineage(query).await?;
        validate_lineage(&self.owner, &fact_id, after.as_ref(), limit, &events)?;
        Ok(events)
    }

    pub async fn resolve_legacy_fact(
        &self,
        query: LegacyFactQuery,
    ) -> Result<Option<FactId>, MemoryApplicationError> {
        self.ensure_owner(query.owner())?;
        let fact_id = self.authority.resolve_legacy_fact(query).await?;
        if fact_id
            .as_ref()
            .is_some_and(|fact_id| fact_id.validate_owner(&self.owner).is_err())
        {
            return Err(MemoryApplicationError::InvalidAuthorityResult {
                invariant: "legacy fact owner",
            });
        }
        Ok(fact_id)
    }

    pub async fn get_retrieval_anchor(
        &self,
        query: RetrievalAnchorQuery,
    ) -> Result<Option<RetrievalAnchorRecordV2>, MemoryApplicationError> {
        self.ensure_owner(query.owner())?;
        let anchor_id = query.anchor_id().clone();
        let anchor = self.authority.get_retrieval_anchor(query).await?;
        if let Some(anchor) = &anchor
            && (anchor.anchor_id() != &anchor_id
                || FactOwnerV1::from(anchor.owner().clone()) != self.owner)
        {
            return Err(MemoryApplicationError::InvalidAuthorityResult {
                invariant: "retrieval anchor identity",
            });
        }
        Ok(anchor)
    }

    fn legacy_compatibility_target(
        &self,
        legacy_fact_id: i64,
    ) -> Result<CompatibilityFactTargetV1, MemoryApplicationError> {
        LegacyFactQuery::new(
            self.owner.clone(),
            self.compatibility_scope.source_store_id().clone(),
            legacy_fact_id,
        )
        .map(CompatibilityFactTargetV1::Legacy)
        .map_err(|_| MemoryApplicationError::InvalidCompatibilityInput {
            invariant: "legacy numeric fact target",
        })
    }

    fn ensure_owner(&self, request_owner: &FactOwnerV1) -> Result<(), MemoryApplicationError> {
        request_owner.validate()?;
        if request_owner != &self.owner {
            return Err(MemoryApplicationError::OwnerMismatch {
                scope: self.owner.clone(),
                request_owner: request_owner.clone(),
            });
        }
        Ok(())
    }
}

impl<A: FactProposalStore> MemoryApplication<A> {
    pub async fn promote_fact_proposal(
        &self,
        promotion: PromoteFactProposal,
    ) -> Result<PromoteFactProposalOutcome, MemoryApplicationError> {
        self.ensure_owner(promotion.owner())?;
        let proposal_id = promotion.proposal_id().clone();
        let previous_state = promotion.expected_state();
        let fact_id = promotion.batch().fact_id().clone();
        let outcome = self.authority.promote_fact_proposal(promotion).await?;
        if outcome.proposal_id() != &proposal_id || outcome.previous_state() != previous_state {
            return Err(MemoryApplicationError::InvalidAuthorityResult {
                invariant: "proposal CAS identity",
            });
        }
        validate_commit_outcome(&self.owner, &fact_id, outcome.commit())?;
        Ok(outcome)
    }
}

/// Typed compatibility use cases. Transport adapters translate legacy inputs
/// before this boundary; only the authority owns the corresponding mutation
/// transaction and compatibility projection.
impl<A: FactCompatibilityStore> MemoryApplication<A> {
    pub async fn list_compatibility_facts(
        &self,
        query: CompatibilityFactListQueryV1,
    ) -> Result<CompatibilityFactPageV1, MemoryApplicationError> {
        self.ensure_owner(query.owner())?;
        let after_fact_id = query.after_fact_id().cloned();
        let limit = query.limit();
        let page = self.authority.list_compatibility_facts(query).await?;
        validate_compatibility_page(&self.owner, after_fact_id.as_ref(), limit, &page)?;
        Ok(page)
    }

    pub async fn search_compatibility_facts(
        &self,
        query: CompatibilityFactSearchQuery,
    ) -> Result<CompatibilityFactSearchPageV1, MemoryApplicationError> {
        self.ensure_owner(query.owner())?;
        let after = query.after().cloned();
        let limit = query.limit();
        let page = self.authority.search_compatibility_facts(query).await?;
        validate_compatibility_search_page(&self.owner, after.as_ref(), limit, &page)?;
        Ok(page)
    }

    pub async fn probe_compatibility_facts(
        &self,
        query: CompatibilityFactSearchQuery,
    ) -> Result<CompatibilityFactSearchPageV1, MemoryApplicationError> {
        self.ensure_owner(query.owner())?;
        let after = query.after().cloned();
        let limit = query.limit();
        let page = self.authority.probe_compatibility_facts(query).await?;
        validate_compatibility_search_page(&self.owner, after.as_ref(), limit, &page)?;
        Ok(page)
    }

    pub async fn related_compatibility_facts(
        &self,
        query: CompatibilityFactSearchQuery,
    ) -> Result<CompatibilityFactSearchPageV1, MemoryApplicationError> {
        self.ensure_owner(query.owner())?;
        let after = query.after().cloned();
        let limit = query.limit();
        let page = self.authority.related_compatibility_facts(query).await?;
        validate_compatibility_search_page(&self.owner, after.as_ref(), limit, &page)?;
        Ok(page)
    }

    pub async fn reason_compatibility_facts(
        &self,
        query: CompatibilityFactSearchQuery,
    ) -> Result<CompatibilityFactSearchPageV1, MemoryApplicationError> {
        self.ensure_owner(query.owner())?;
        let after = query.after().cloned();
        let limit = query.limit();
        let page = self.authority.reason_compatibility_facts(query).await?;
        validate_compatibility_search_page(&self.owner, after.as_ref(), limit, &page)?;
        Ok(page)
    }

    pub async fn find_compatibility_contradictions(
        &self,
        query: CompatibilityFactContradictionQueryV1,
    ) -> Result<CompatibilityFactContradictionPageV1, MemoryApplicationError> {
        self.ensure_owner(query.owner())?;
        let limit = query.limit();
        let page = self
            .authority
            .find_compatibility_contradictions(query)
            .await?;
        if page.owner() != &self.owner
            || page.contradictions().len() > limit
            || page
                .contradictions()
                .iter()
                .any(|contradiction| contradiction.existing().owner() != &self.owner)
        {
            return Err(MemoryApplicationError::InvalidAuthorityResult {
                invariant: "compatibility contradiction bounds and owner",
            });
        }
        Ok(page)
    }

    pub async fn get_compatibility_fact(
        &self,
        target: CompatibilityFactTargetV1,
    ) -> Result<Option<CompatibilityFactProjectionV1>, MemoryApplicationError> {
        self.ensure_owner(target.owner())?;
        let result = self
            .authority
            .get_compatibility_fact(target.clone())
            .await?;
        if let Some(projection) = &result {
            validate_compatibility_projection(&self.owner, &target, projection)?;
        }
        Ok(result)
    }

    /// Owner-bound exact-content lookup for automation deduplication. The raw
    /// content is never forwarded to the authority: only its canonical SHA-256
    /// locator digest crosses this boundary. Legacy mappings remain part of an
    /// available projection, so callers can preserve the historical numeric id.
    pub async fn find_exact_fact_v1_by_content(
        &self,
        content: &str,
    ) -> Result<Option<CompatibilityFactProjectionV1>, MemoryApplicationError> {
        if content.trim().is_empty() || detect_secret_like(content.trim()).is_some() {
            return Ok(None);
        }
        let digest = LocatorDigest::new(format!(
            "sha256:{}",
            hex::encode(Sha256::digest(content.as_bytes()))
        ))
        .map_err(|_| MemoryApplicationError::InvalidCompatibilityInput {
            invariant: "exact fact content digest",
        })?;
        let result =
            self.authority
                .find_compatibility_fact_by_content_digest(
                    CompatibilityFactContentDigestQueryV1::new(self.owner.clone(), digest)?,
                )
                .await?;
        if let Some(projection) = &result
            && projection.owner() != &self.owner
        {
            return Err(MemoryApplicationError::InvalidAuthorityResult {
                invariant: "exact compatibility fact owner",
            });
        }
        Ok(result)
    }

    /// Finite dashboard overview; the dashboard never opens a memory database
    /// or constructs a store query itself.
    pub async fn dashboard_overview_v1(
        &self,
        fact_limit: usize,
        graph_limit: usize,
    ) -> Result<CompatibilityDashboardMemoryOverviewV1, MemoryApplicationError> {
        let overview = self
            .authority
            .dashboard_compatibility_memory_overview(
                CompatibilityDashboardMemoryOverviewQueryV1::new(
                    self.owner.clone(),
                    fact_limit,
                    graph_limit,
                )?,
            )
            .await?;
        if overview.owner != self.owner
            || overview.facts.len() > fact_limit
            || overview.entities.len() > graph_limit
            || overview.fact_entity_links.len() > graph_limit
            || overview
                .facts
                .iter()
                .any(|fact| fact.fact.owner() != &self.owner)
            || overview
                .entities
                .iter()
                .any(|entity| entity.target.owner() != &self.owner)
            || overview
                .fact_entity_links
                .iter()
                .any(|link| link.fact.owner() != &self.owner || link.entity.owner() != &self.owner)
        {
            return Err(MemoryApplicationError::InvalidAuthorityResult {
                invariant: "dashboard overview owner and bounds",
            });
        }
        Ok(overview)
    }

    /// Legacy numeric detail wrapper. The fixed compatibility source and owner
    /// are resolved here, never by a dashboard handler.
    pub async fn dashboard_fact_detail_v1(
        &self,
        fact_id: i64,
    ) -> Result<Option<CompatibilityDashboardFactDetailV1>, MemoryApplicationError> {
        let target = self.legacy_compatibility_target(fact_id)?;
        let detail = self
            .authority
            .dashboard_compatibility_fact_detail(CompatibilityDashboardFactDetailQueryV1::new(
                target.clone(),
            )?)
            .await?;
        if let Some(detail) = &detail
            && (detail.fact.owner() != &self.owner
                || detail
                    .entities
                    .iter()
                    .any(|entity| entity.target.owner() != &self.owner)
                || detail
                    .history
                    .as_ref()
                    .is_some_and(|history| history.owner() != &self.owner))
        {
            return Err(MemoryApplicationError::InvalidAuthorityResult {
                invariant: "dashboard detail owner",
            });
        }
        Ok(detail)
    }

    pub async fn dashboard_history_v1(
        &self,
        fact_id: i64,
        limit: usize,
    ) -> Result<CompatibilityFactHistoryV1, MemoryApplicationError> {
        self.get_compatibility_history(CompatibilityFactHistoryQueryV1::new(
            self.legacy_compatibility_target(fact_id)?,
            None,
            limit,
        )?)
        .await
    }

    /// Numeric dashboard trust-history route retaining typed repair progress.
    /// Callers that need an honest incomplete state must use this rather than
    /// the legacy lossy `fact_trust_history_v1` vector projection.
    pub async fn dashboard_feedback_history_v1(
        &self,
        fact_id: i64,
        limit: usize,
    ) -> Result<CompatibilityFactFeedbackHistoryV1, MemoryApplicationError> {
        self.get_compatibility_feedback_history(CompatibilityFactFeedbackHistoryQueryV1::new(
            self.legacy_compatibility_target(fact_id)?,
            None,
            limit,
        )?)
        .await
    }

    /// Typed dashboard status including feedback-history repair progress.
    pub async fn dashboard_memory_status_v1(
        &self,
    ) -> Result<CompatibilityMemoryStatusV1, MemoryApplicationError> {
        self.compatibility_memory_status().await
    }

    /// Capped vector inputs for dashboard-side PCA and similarity. Pair scoring
    /// remains client-side over this bounded response rather than a generic DB API.
    pub async fn dashboard_vector_points_v1(
        &self,
        search: Option<String>,
        limit: usize,
    ) -> Result<Vec<CompatibilityDashboardVectorPointV1>, MemoryApplicationError> {
        let points = self
            .authority
            .dashboard_compatibility_vector_points(CompatibilityDashboardVectorPointsQueryV1::new(
                self.owner.clone(),
                search,
                limit,
            )?)
            .await?;
        if points.len() > limit
            || points
                .iter()
                .any(|point| point.fact.fact.owner() != &self.owner)
        {
            return Err(MemoryApplicationError::InvalidAuthorityResult {
                invariant: "dashboard vector point owner and bounds",
            });
        }
        Ok(points)
    }

    pub async fn dashboard_oplog_v1(
        &self,
        limit: usize,
    ) -> Result<Vec<CompatibilityDashboardOplogEntryV1>, MemoryApplicationError> {
        let entries = self
            .authority
            .dashboard_compatibility_memory_oplog(CompatibilityDashboardOplogQueryV1::new(
                self.owner.clone(),
                limit,
            )?)
            .await?;
        if entries.len() > limit
            || entries.iter().any(|entry| {
                entry
                    .fact
                    .as_ref()
                    .is_some_and(|target| target.owner() != &self.owner)
            })
        {
            return Err(MemoryApplicationError::InvalidAuthorityResult {
                invariant: "dashboard oplog owner and bounds",
            });
        }
        Ok(entries)
    }

    pub async fn dashboard_curation_v1(
        &self,
        request: CompatibilityFactCurationBatchV1,
    ) -> Result<CompatibilityFactCurationReceiptV1, MemoryApplicationError> {
        self.ensure_owner(request.owner())?;
        let receipt = self
            .authority
            .apply_compatibility_fact_curation(request)
            .await?;
        if receipt.owner() != &self.owner {
            return Err(MemoryApplicationError::InvalidAuthorityResult {
                invariant: "dashboard curation receipt owner",
            });
        }
        Ok(receipt)
    }

    /// Dashboard-facing finite curation adapter. Numeric V1 identifiers are
    /// resolved only through the fixed compatibility scope at this boundary.
    pub async fn dashboard_apply_grooming_v1(
        &self,
        operations: Vec<MemoryGroomingOperation>,
        min_confidence: f64,
        context: MemoryOperationContext,
    ) -> Result<MemoryGroomingReport, MemoryApplicationError> {
        let minimum = Confidence::new(min_confidence).map_err(|_| {
            MemoryApplicationError::InvalidCompatibilityInput {
                invariant: "dashboard curation minimum confidence",
            }
        })?;
        let operations = operations
            .into_iter()
            .map(|operation| self.dashboard_curation_operation(operation))
            .collect::<Result<Vec<_>, _>>()?;
        let receipt = self
            .dashboard_curation_v1(CompatibilityFactCurationBatchV1::new(
                self.owner.clone(),
                context.operation_id().clone(),
                context.actor().cloned(),
                minimum,
                operations,
            )?)
            .await?;
        Ok(MemoryGroomingReport {
            normalized_tags: legacy_usize(receipt.normalized_tags(), "dashboard normalized tags")?,
            merged_entities: legacy_usize(receipt.merged_entities(), "dashboard merged entities")?,
            aliases_added: legacy_usize(receipt.aliases_added(), "dashboard aliases added")?,
            facts_linked: legacy_usize(receipt.facts_linked(), "dashboard facts linked")?,
            vectors_repaired: legacy_usize(
                receipt.vectors_repaired(),
                "dashboard vectors repaired",
            )?,
            derived_repair: MemoryRepairStats {
                missing_vectors_repaired: legacy_usize(
                    receipt.derived_repair().missing_vectors_repaired(),
                    "dashboard derived vectors repaired",
                )?,
                banks_rebuilt: legacy_usize(
                    receipt.derived_repair().banks_rebuilt(),
                    "dashboard derived banks rebuilt",
                )?,
            },
        })
    }

    fn dashboard_curation_operation(
        &self,
        operation: MemoryGroomingOperation,
    ) -> Result<CompatibilityFactCurationOperationV1, MemoryApplicationError> {
        let fact_targets = |fact_ids: Vec<i64>| {
            fact_ids
                .into_iter()
                .map(|fact_id| self.legacy_compatibility_target(fact_id))
                .collect::<Result<Vec<_>, _>>()
        };
        let confidence = |value: f64| {
            Confidence::new(value).map_err(|_| MemoryApplicationError::InvalidCompatibilityInput {
                invariant: "dashboard curation confidence",
            })
        };
        match operation {
            MemoryGroomingOperation::NormalizeTags {
                fact_id,
                tags,
                evidence_fact_ids,
                confidence: value,
            } => Ok(CompatibilityFactCurationOperationV1::NormalizeTags(
                CompatibilityFactNormalizeTagsV1::new(
                    self.legacy_compatibility_target(fact_id)?,
                    sanitize_curation_texts(tags, "dashboard curation tags")?,
                    fact_targets(evidence_fact_ids)?,
                    confidence(value)?,
                )?,
            )),
            MemoryGroomingOperation::MergeEntities {
                winner_entity_id,
                loser_entity_ids,
                evidence_fact_ids,
                confidence: value,
            } => Ok(CompatibilityFactCurationOperationV1::MergeEntities(
                CompatibilityFactMergeEntitiesV1::new(
                    CompatibilityLegacyEntityTargetV1::new(self.owner.clone(), winner_entity_id)?,
                    loser_entity_ids
                        .into_iter()
                        .map(|entity_id| {
                            CompatibilityLegacyEntityTargetV1::new(self.owner.clone(), entity_id)
                        })
                        .collect::<Result<Vec<_>, _>>()?,
                    fact_targets(evidence_fact_ids)?,
                    confidence(value)?,
                )?,
            )),
            MemoryGroomingOperation::AddAlias {
                entity_id,
                alias,
                evidence_fact_ids,
                confidence: value,
            } => Ok(CompatibilityFactCurationOperationV1::AddAlias(
                CompatibilityFactAddAliasV1::new(
                    CompatibilityLegacyEntityTargetV1::new(self.owner.clone(), entity_id)?,
                    sanitize_curation_text(alias, "dashboard curation alias")?,
                    fact_targets(evidence_fact_ids)?,
                    confidence(value)?,
                )?,
            )),
            MemoryGroomingOperation::LinkFacts {
                source_fact_id,
                target_fact_id,
                relation,
                evidence_fact_ids,
                confidence: value,
                source,
                metadata,
            } => Ok(CompatibilityFactCurationOperationV1::LinkFacts(
                CompatibilityFactLinkV1::new(
                    self.legacy_compatibility_target(source_fact_id)?,
                    self.legacy_compatibility_target(target_fact_id)?,
                    compatibility_relation(relation),
                    fact_targets(evidence_fact_ids)?,
                    confidence(value)?,
                    sanitize_curation_text(source, "dashboard curation relation source")?,
                    sanitize_curation_metadata(metadata)?,
                )?,
            )),
            MemoryGroomingOperation::RepairVector {
                fact_id,
                evidence_fact_ids,
                confidence: value,
            } => Ok(CompatibilityFactCurationOperationV1::RepairVector(
                CompatibilityFactRepairVectorV1::new(
                    self.legacy_compatibility_target(fact_id)?,
                    fact_targets(evidence_fact_ids)?,
                    confidence(value)?,
                ),
            )),
        }
    }

    pub async fn dashboard_merge_facts_v1(
        &self,
        request: CompatibilityFactMergeCommandV1,
    ) -> Result<CompatibilityFactMergeOutcomeV1, MemoryApplicationError> {
        self.ensure_owner(request.owner())?;
        let outcome = self.authority.merge_compatibility_facts(request).await?;
        if outcome.owner() != &self.owner {
            return Err(MemoryApplicationError::InvalidAuthorityResult {
                invariant: "dashboard merge outcome owner",
            });
        }
        Ok(outcome)
    }

    /// Legacy numeric merge route for the dashboard. The handler supplies only
    /// IDs and a trusted operation context; fixed source/owner resolution and
    /// content privacy gating stay in the application layer.
    pub async fn dashboard_merge_fact_ids_v1(
        &self,
        winner_id: i64,
        loser_ids: Vec<i64>,
        merged_content: Option<String>,
        context: MemoryOperationContext,
    ) -> Result<CompatibilityFactMergeOutcomeV1, MemoryApplicationError> {
        let merged_content = match merged_content {
            Some(content) => {
                if detect_secret_like(content.trim()).is_some() {
                    return Err(MemoryApplicationError::InvalidCompatibilityInput {
                        invariant: "dashboard merge content rejected by privacy sanitizer",
                    });
                }
                Some(sanitize_curation_text(
                    content,
                    "dashboard merge content rejected by privacy sanitizer",
                )?)
            }
            None => None,
        };
        let losers = loser_ids
            .into_iter()
            .map(|fact_id| self.legacy_compatibility_target(fact_id))
            .collect::<Result<Vec<_>, _>>()?;
        self.dashboard_merge_facts_v1(CompatibilityFactMergeCommandV1::new(
            self.owner.clone(),
            context.operation_id().clone(),
            self.legacy_compatibility_target(winner_id)?,
            losers,
            merged_content,
            context.actor().cloned(),
        )?)
        .await
    }

    /// One authority repair step only. Any incomplete feedback-history repair is
    /// surfaced through `memory_status_v1`/feedback history while the daemon resumes it.
    pub async fn dashboard_repair_v1(
        &self,
        context: MemoryOperationContext,
    ) -> Result<CompatibilityMemoryRepairStatsV1, MemoryApplicationError> {
        self.authority
            .repair_compatibility_memory(CompatibilityMemoryRepairCommandV1::new(
                self.owner.clone(),
                context.operation_id().clone(),
                context.actor().cloned(),
            )?)
            .await
            .map_err(Into::into)
    }

    /// Advances one persisted V1 raw-memory cutover batch. This daemon-only
    /// command is the sole raw legacy import boundary; reads and curation use
    /// typed canonical projections only.
    pub async fn daemon_legacy_memory_cutover_v1(
        &self,
        context: MemoryOperationContext,
    ) -> Result<CompatibilityLegacyMemoryCutoverProgressV1, MemoryApplicationError> {
        self.authority
            .advance_compatibility_legacy_memory_cutover(
                CompatibilityLegacyMemoryCutoverCommandV1::new(
                    self.owner.clone(),
                    context.operation_id().clone(),
                )?,
            )
            .await
            .map_err(Into::into)
    }

    pub async fn get_compatibility_history(
        &self,
        query: CompatibilityFactHistoryQueryV1,
    ) -> Result<CompatibilityFactHistoryV1, MemoryApplicationError> {
        self.ensure_owner(query.target().owner())?;
        let target = query.target().clone();
        let after = query.after().cloned();
        let limit = query.limit();
        let history = self.authority.compatibility_fact_history(query).await?;
        if history.owner() != &self.owner {
            return Err(MemoryApplicationError::InvalidAuthorityResult {
                invariant: "compatibility history owner",
            });
        }
        if let Some(fact_id) = target.canonical_fact_id()
            && history.fact_id() != fact_id
        {
            return Err(MemoryApplicationError::InvalidAuthorityResult {
                invariant: "compatibility history canonical identity",
            });
        }
        validate_lineage(
            &self.owner,
            history.fact_id(),
            after.as_ref(),
            limit,
            history.events(),
        )?;
        Ok(history)
    }

    /// Pure history snapshot. Incomplete repair is surfaced in the returned
    /// progress; callers must use an explicit repair command to advance it.
    pub async fn get_compatibility_feedback_history(
        &self,
        query: CompatibilityFactFeedbackHistoryQueryV1,
    ) -> Result<CompatibilityFactFeedbackHistoryV1, MemoryApplicationError> {
        self.ensure_owner(query.target().owner())?;
        let limit = query.limit();
        let history = self
            .authority
            .compatibility_fact_feedback_history(query)
            .await?;
        if history.owner() != &self.owner || history.events().len() > limit {
            return Err(MemoryApplicationError::InvalidAuthorityResult {
                invariant: "compatibility feedback history owner and bounds",
            });
        }
        Ok(history)
    }

    /// Pure status snapshot. It reports, but never advances, feedback repair.
    pub async fn compatibility_memory_status(
        &self,
    ) -> Result<CompatibilityMemoryStatusV1, MemoryApplicationError> {
        let status = self
            .authority
            .compatibility_memory_status(self.owner.clone())
            .await?;
        if status.owner() != &self.owner {
            return Err(MemoryApplicationError::InvalidAuthorityResult {
                invariant: "compatibility memory status owner",
            });
        }
        Ok(status)
    }

    pub async fn inspect_compatibility_fact(
        &self,
        target: CompatibilityFactTargetV1,
    ) -> Result<Option<CompatibilityFactInspectionV1>, MemoryApplicationError> {
        self.ensure_owner(target.owner())?;
        let inspection = self
            .authority
            .inspect_compatibility_fact(target.clone())
            .await?;
        if let Some(inspection) = &inspection {
            validate_compatibility_inspection(&self.owner, &target, inspection)?;
        }
        Ok(inspection)
    }

    pub async fn add_compatibility_fact(
        &self,
        request: CompatibilityFactAddCommandV1,
    ) -> Result<CompatibilityFactAddOutcomeV1, MemoryApplicationError> {
        self.ensure_owner(request.owner())?;
        let outcome = self.authority.add_compatibility_fact(request).await?;
        validate_compatibility_add_outcome(&self.owner, &outcome)?;
        Ok(outcome)
    }

    pub async fn update_compatibility_fact(
        &self,
        request: CompatibilityFactUpdateCommandV1,
    ) -> Result<CompatibilityFactUpdateOutcomeV1, MemoryApplicationError> {
        self.ensure_owner(request.target().owner())?;
        let target = request.target().clone();
        let outcome = self.authority.update_compatibility_fact(request).await?;
        validate_compatibility_projection(&self.owner, &target, outcome.fact())?;
        Ok(outcome)
    }

    pub async fn remove_compatibility_fact(
        &self,
        request: CompatibilityFactRemoveCommandV1,
    ) -> Result<CompatibilityFactRemoveOutcomeV1, MemoryApplicationError> {
        self.ensure_owner(request.target().owner())?;
        let target = request.target().clone();
        let outcome = self.authority.remove_compatibility_fact(request).await?;
        validate_compatibility_projection(&self.owner, &target, outcome.fact())?;
        Ok(outcome)
    }

    pub async fn record_compatibility_fact_feedback(
        &self,
        request: CompatibilityFactFeedbackCommandV1,
    ) -> Result<CompatibilityFactFeedbackOutcomeV1, MemoryApplicationError> {
        self.ensure_owner(request.target().owner())?;
        let target = request.target().clone();
        let outcome = self
            .authority
            .record_compatibility_fact_feedback(request)
            .await?;
        validate_compatibility_projection(&self.owner, &target, outcome.fact())?;
        Ok(outcome)
    }

    pub async fn record_compatibility_fact_retrieval(
        &self,
        request: CompatibilityFactRetrievalCommandV1,
    ) -> Result<Vec<CompatibilityFactProjectionV1>, MemoryApplicationError> {
        self.ensure_owner(request.owner())?;
        let targets = request.targets().to_vec();
        let projections = self
            .authority
            .record_compatibility_fact_retrieval(request)
            .await?;
        if projections
            .iter()
            .any(|projection| projection.owner() != &self.owner)
        {
            return Err(MemoryApplicationError::InvalidAuthorityResult {
                invariant: "compatibility retrieval projection owner",
            });
        }
        if targets
            .iter()
            .all(|target| target.canonical_fact_id().is_some())
            && projections.iter().any(|projection| {
                !targets
                    .iter()
                    .any(|target| target.canonical_fact_id() == Some(projection.fact_id()))
            })
        {
            return Err(MemoryApplicationError::InvalidAuthorityResult {
                invariant: "compatibility retrieval canonical target",
            });
        }
        Ok(projections)
    }

    pub async fn submit_compatibility_fact_proposal(
        &self,
        proposal_id: ProvenanceId,
        request: CompatibilityFactAddCommandV1,
        submitter: Option<ActorId>,
    ) -> Result<CompatibilityFactProposalRecordV1, MemoryApplicationError> {
        self.ensure_owner(request.owner())?;
        let proposal = self
            .authority
            .submit_compatibility_fact_proposal(proposal_id.clone(), request, submitter)
            .await?;
        validate_compatibility_proposal(&self.owner, &proposal_id, &proposal)?;
        Ok(proposal)
    }

    pub async fn get_compatibility_fact_proposal(
        &self,
        proposal_id: ProvenanceId,
    ) -> Result<Option<CompatibilityFactProposalRecordV1>, MemoryApplicationError> {
        let proposal = self
            .authority
            .get_compatibility_fact_proposal(self.owner.clone(), proposal_id.clone())
            .await?;
        if let Some(proposal) = &proposal {
            validate_compatibility_proposal(&self.owner, &proposal_id, proposal)?;
        }
        Ok(proposal)
    }

    pub async fn list_compatibility_fact_proposals(
        &self,
        state: Option<CompatibilityFactProposalStateV1>,
        after_proposal_id: Option<ProvenanceId>,
        limit: usize,
    ) -> Result<CompatibilityFactProposalPageV1, MemoryApplicationError> {
        let page = self
            .authority
            .list_compatibility_fact_proposals(
                self.owner.clone(),
                state,
                after_proposal_id.clone(),
                limit,
            )
            .await?;
        validate_compatibility_proposal_page(
            &self.owner,
            after_proposal_id.as_ref(),
            limit,
            &page,
        )?;
        Ok(page)
    }

    pub async fn count_pending_compatibility_fact_proposals(
        &self,
    ) -> Result<u64, MemoryApplicationError> {
        Ok(self
            .authority
            .count_pending_compatibility_fact_proposals(self.owner.clone())
            .await?)
    }

    pub async fn reject_compatibility_fact_proposal(
        &self,
        proposal_id: ProvenanceId,
        expected_revision: CompatibilityFactProposalRevisionV1,
        reviewer: ActorId,
        reason: String,
    ) -> Result<CompatibilityFactProposalRecordV1, MemoryApplicationError> {
        let proposal = self
            .authority
            .reject_compatibility_fact_proposal(
                self.owner.clone(),
                proposal_id.clone(),
                expected_revision,
                reviewer,
                reason,
            )
            .await?;
        validate_compatibility_proposal(&self.owner, &proposal_id, &proposal)?;
        if proposal.revision() <= expected_revision {
            return Err(MemoryApplicationError::InvalidAuthorityResult {
                invariant: "compatibility proposal rejection revision",
            });
        }
        Ok(proposal)
    }

    pub async fn import_legacy_compatibility_fact_proposals(
        &self,
        request: CompatibilityFactProposalImportV1,
    ) -> Result<CompatibilityFactProposalImportReceiptV1, MemoryApplicationError> {
        self.ensure_owner(request.owner())?;
        let source_store_id = request.source_store_id().clone();
        let sidecar_digest = request.sidecar_digest().clone();
        let receipt = self
            .authority
            .import_legacy_compatibility_fact_proposals(request)
            .await?;
        if receipt.owner() != &self.owner
            || receipt.source_store_id() != &source_store_id
            || receipt.sidecar_digest() != &sidecar_digest
        {
            return Err(MemoryApplicationError::InvalidAuthorityResult {
                invariant: "compatibility proposal import identity",
            });
        }
        Ok(receipt)
    }

    pub async fn promote_compatibility_fact_proposal(
        &self,
        request: CompatibilityFactProposalPromotionV1,
    ) -> Result<CompatibilityFactProposalRecordV1, MemoryApplicationError> {
        Ok(self
            .promote_compatibility_fact_proposal_with_disposition(request)
            .await?
            .proposal()
            .clone())
    }

    /// Atomic promotion result for automation callers. The disposition comes
    /// from the authority transaction/replay receipt, never a pre-read.
    pub async fn promote_compatibility_fact_proposal_with_disposition(
        &self,
        request: CompatibilityFactProposalPromotionV1,
    ) -> Result<CompatibilityFactProposalPromotionResultV1, MemoryApplicationError> {
        self.ensure_owner(request.owner())?;
        let proposal_id = request.proposal_id().clone();
        let expected_revision = request.expected_revision();
        let result = self
            .authority
            .promote_compatibility_fact_proposal_with_disposition(request)
            .await?;
        let proposal = result.proposal();
        validate_compatibility_proposal(&self.owner, &proposal_id, proposal)?;
        let revision_is_valid = match result.disposition() {
            CompatibilityFactProposalPromotionDispositionV1::NewlyPromoted
            | CompatibilityFactProposalPromotionDispositionV1::Quarantined => {
                proposal.revision() > expected_revision
            }
            CompatibilityFactProposalPromotionDispositionV1::AlreadyPromoted => {
                proposal.revision() >= expected_revision
            }
        };
        if !revision_is_valid {
            return Err(MemoryApplicationError::InvalidAuthorityResult {
                invariant: "compatibility proposal promotion revision",
            });
        }
        Ok(result)
    }

    /// V1-facing add route. The application owns conversion, sanitation, and
    /// portable operation construction; transports pass only the V1 request
    /// and trusted operation context.
    pub async fn add_fact_v1(
        &self,
        request: AddFactRequest,
        context: MemoryOperationContext,
    ) -> Result<AddFactOutcome, MemoryApplicationError> {
        let Some(request) = sanitize_add_fact_request(request)? else {
            return Ok(rejected_secret_add_outcome());
        };
        let outcome = self
            .add_compatibility_fact(compatibility_add_command(
                self.owner.clone(),
                request,
                &context,
            )?)
            .await?;
        self.project_add_fact_outcome_v1(outcome).await
    }

    pub async fn search_facts_v1(
        &self,
        request: SearchFactsRequest,
        context: MemoryOperationContext,
    ) -> Result<Vec<FactSearchResult>, MemoryApplicationError> {
        self.search_v1(
            CompatibilityFactSearchKindV1::Search,
            Some(request.query.clone()),
            request,
            Some(context),
            true,
        )
        .await
    }

    /// Background/context retrieval variant. It deliberately does not create
    /// a retrieval event or mutate recall/access counters.
    pub async fn search_facts_untracked_v1(
        &self,
        request: SearchFactsRequest,
    ) -> Result<Vec<FactSearchResult>, MemoryApplicationError> {
        self.search_v1(
            CompatibilityFactSearchKindV1::Search,
            Some(request.query.clone()),
            request,
            None,
            false,
        )
        .await
    }

    pub async fn probe_facts_v1(
        &self,
        request: SearchFactsRequest,
        context: MemoryOperationContext,
    ) -> Result<Vec<FactSearchResult>, MemoryApplicationError> {
        self.search_v1(
            CompatibilityFactSearchKindV1::Probe,
            Some(request.query.clone()),
            request,
            Some(context),
            false,
        )
        .await
    }

    pub async fn probe_facts_untracked_v1(
        &self,
        request: SearchFactsRequest,
    ) -> Result<Vec<FactSearchResult>, MemoryApplicationError> {
        self.search_v1(
            CompatibilityFactSearchKindV1::Probe,
            Some(request.query.clone()),
            request,
            None,
            false,
        )
        .await
    }

    pub async fn related_facts_v1(
        &self,
        request: SearchFactsRequest,
        context: MemoryOperationContext,
    ) -> Result<Vec<FactSearchResult>, MemoryApplicationError> {
        self.search_v1(
            CompatibilityFactSearchKindV1::Related {
                entity: request.query.clone(),
            },
            None,
            request,
            Some(context),
            false,
        )
        .await
    }

    pub async fn related_facts_untracked_v1(
        &self,
        request: SearchFactsRequest,
    ) -> Result<Vec<FactSearchResult>, MemoryApplicationError> {
        self.search_v1(
            CompatibilityFactSearchKindV1::Related {
                entity: request.query.clone(),
            },
            None,
            request,
            None,
            false,
        )
        .await
    }

    pub async fn reason_facts_v1(
        &self,
        mut entities: Vec<String>,
        category: Option<MemoryCategory>,
        min_trust: Option<f64>,
        limit: usize,
        context: MemoryOperationContext,
    ) -> Result<Vec<FactSearchResult>, MemoryApplicationError> {
        entities.sort_unstable();
        entities.dedup();
        self.search_v1(
            CompatibilityFactSearchKindV1::Reason { entities },
            None,
            SearchFactsRequest {
                query: String::new(),
                category,
                limit: Some(limit),
                min_trust,
                include_why: true,
            },
            Some(context),
            false,
        )
        .await
    }

    pub async fn reason_facts_untracked_v1(
        &self,
        mut entities: Vec<String>,
        category: Option<MemoryCategory>,
        min_trust: Option<f64>,
        limit: usize,
    ) -> Result<Vec<FactSearchResult>, MemoryApplicationError> {
        entities.sort_unstable();
        entities.dedup();
        self.search_v1(
            CompatibilityFactSearchKindV1::Reason { entities },
            None,
            SearchFactsRequest {
                query: String::new(),
                category,
                limit: Some(limit),
                min_trust,
                include_why: true,
            },
            None,
            false,
        )
        .await
    }

    pub async fn contradict_facts_v1(
        &self,
        category: Option<MemoryCategory>,
        threshold: f64,
        limit: usize,
    ) -> Result<Vec<ContradictionResult>, MemoryApplicationError> {
        let threshold = Confidence::new(threshold).map_err(|_| {
            MemoryApplicationError::InvalidCompatibilityInput {
                invariant: "legacy contradiction threshold",
            }
        })?;
        let page = self
            .find_compatibility_contradictions(CompatibilityFactContradictionQueryV1::new(
                self.owner.clone(),
                category.map(fact_category),
                (threshold.as_f64() * 1_000_000.0).round() as u32,
                limit,
            )?)
            .await?;
        page.contradictions()
            .iter()
            .map(|item| {
                Ok(ContradictionResult {
                    existing_fact: compatibility_fact_record(
                        &self.compatibility_scope,
                        item.existing(),
                    )?,
                    new_content: item.new_content().to_owned(),
                    score: f64::from(item.score_millionths()) / 1_000_000.0,
                    why: item.why().map(ToOwned::to_owned),
                })
            })
            .collect()
    }

    pub async fn list_facts_v1(
        &self,
        category: Option<MemoryCategory>,
        min_trust: Option<f64>,
        limit: usize,
        context: MemoryOperationContext,
    ) -> Result<Vec<FactRecord>, MemoryApplicationError> {
        self.list_facts_v1_inner(category, min_trust, limit, Some(context))
            .await
    }

    pub async fn list_facts_untracked_v1(
        &self,
        category: Option<MemoryCategory>,
        min_trust: Option<f64>,
        limit: usize,
    ) -> Result<Vec<FactRecord>, MemoryApplicationError> {
        self.list_facts_v1_inner(category, min_trust, limit, None)
            .await
    }

    async fn list_facts_v1_inner(
        &self,
        category: Option<MemoryCategory>,
        min_trust: Option<f64>,
        limit: usize,
        context: Option<MemoryOperationContext>,
    ) -> Result<Vec<FactRecord>, MemoryApplicationError> {
        let page = self
            .list_compatibility_facts(CompatibilityFactListQueryV1::new(
                self.owner.clone(),
                category.map(fact_category),
                compatibility_confidence(min_trust)?,
                None,
                limit,
            )?)
            .await?;
        let targets = compatibility_projection_targets(page.facts());
        // Unavailable projections (deleted, redacted, expired) read as absent
        // under the V1 contract — mirroring get_fact_v1 — so one tombstone
        // never makes the whole listing fail.
        let records = page
            .facts()
            .iter()
            .filter(|fact| matches!(fact, CompatibilityFactProjectionV1::Available(_)))
            .map(|fact| compatibility_projection_record(&self.compatibility_scope, fact))
            .collect::<Result<Vec<_>, _>>()?;
        if let Some(context) = context.as_ref() {
            self.record_v1_retrieval(targets, context, false).await?;
        }
        Ok(records)
    }

    pub async fn get_fact_v1(
        &self,
        fact_id: i64,
    ) -> Result<Option<FactRecord>, MemoryApplicationError> {
        let target = self.legacy_compatibility_target(fact_id)?;
        match self.get_compatibility_fact(target).await? {
            // A removed or otherwise unavailable fact reads as absent under
            // the V1 contract; only reachable payloads project to records.
            None | Some(CompatibilityFactProjectionV1::Unavailable(_)) => Ok(None),
            Some(projection) => {
                compatibility_projection_record(&self.compatibility_scope, &projection).map(Some)
            }
        }
    }

    pub async fn update_fact_v1(
        &self,
        request: UpdateFactRequest,
        context: MemoryOperationContext,
    ) -> Result<V1UpdateFactOutcome, MemoryApplicationError> {
        if let Some(content) = request.content.as_deref()
            && let Some(reason) = detect_secret_like(content.trim())
        {
            return Ok(V1UpdateFactOutcome::RejectedSecretLike {
                reason: format!(
                    "rejected_secret_like: content matched secret-likeness rule: {reason}"
                ),
            });
        }
        let Some(request) = sanitize_update_fact_request(request)? else {
            return Ok(V1UpdateFactOutcome::RejectedSecretLike {
                reason: "rejected_secret_like: content or structured payload was rejected by the privacy sanitizer".to_owned(),
            });
        };
        let target = self.legacy_compatibility_target(request.fact_id)?;
        let patch = CompatibilityFactUpdatePatchV1::new(
            request.content,
            request.category.map(fact_category),
            request.source.map(Some),
            request.tags,
            request.entities,
            request.metadata,
            compatibility_confidence(request.trust)?,
        )?;
        let outcome = self
            .update_compatibility_fact(CompatibilityFactUpdateCommandV1::new(
                target,
                context.operation_id().clone(),
                None,
                patch,
                context.actor().cloned(),
            )?)
            .await?;
        Ok(V1UpdateFactOutcome::Updated(Box::new(
            compatibility_projection_record(&self.compatibility_scope, outcome.fact())?,
        )))
    }

    pub async fn remove_fact_v1(
        &self,
        fact_id: i64,
        context: MemoryOperationContext,
    ) -> Result<bool, MemoryApplicationError> {
        let target = self.legacy_compatibility_target(fact_id)?;
        // Removing a fact that was never stored is an idempotent no-op, mirroring
        // the legacy MemoryStore contract. Callers (e.g. the dashboard curate
        // handler) surface this `false` as a per-op "fact not found" result
        // rather than an authority failure.
        if self.get_compatibility_fact(target.clone()).await?.is_none() {
            return Ok(false);
        }
        let outcome = self
            .remove_compatibility_fact(CompatibilityFactRemoveCommandV1::new(
                target,
                context.operation_id().clone(),
                None,
                context.actor().cloned(),
            )?)
            .await?;
        Ok(outcome.removed())
    }

    pub async fn record_fact_feedback_v1(
        &self,
        request: FeedbackRequest,
        context: MemoryOperationContext,
    ) -> Result<crate::memory::types::FeedbackResult, MemoryApplicationError> {
        let source_input = request
            .source
            .clone()
            .filter(|source| !source.trim().is_empty());
        let Some(source) = sanitize_optional_memory_text(source_input) else {
            return Err(MemoryApplicationError::InvalidCompatibilityInput {
                invariant: "legacy feedback source rejected by privacy sanitizer",
            });
        };
        // V1 feedback historically attributed omitted/blank transport sources
        // to MCP. Preserve that ordinary behavior without inventing a source for
        // redacted or unknown history rows returned by the authority.
        let source = source.unwrap_or_else(|| "mcp".to_owned());
        let Some(note) = sanitize_optional_memory_text(request.note.clone()) else {
            return Err(MemoryApplicationError::InvalidCompatibilityInput {
                invariant: "legacy feedback note rejected by privacy sanitizer",
            });
        };
        let action = match request.action {
            FeedbackAction::Helpful => CompatibilityFactFeedbackActionV1::Helpful,
            FeedbackAction::Unhelpful => CompatibilityFactFeedbackActionV1::Unhelpful,
        };
        let outcome = self
            .record_compatibility_fact_feedback(CompatibilityFactFeedbackCommandV1::new(
                self.legacy_compatibility_target(request.fact_id)?,
                context.operation_id().clone(),
                None,
                action,
                context.actor().cloned(),
                Some(source),
                note,
            )?)
            .await?;
        let event_id = outcome.legacy_feedback_event_id().ok_or(
            MemoryApplicationError::IncompatibleLegacyProjection {
                invariant: "legacy feedback event identity",
            },
        )?;
        let fact = compatibility_projection_record(&self.compatibility_scope, outcome.fact())?;
        Ok(crate::memory::types::FeedbackResult {
            event_id,
            fact_id: fact.fact_id,
            action: request.action,
            old_trust: outcome.old_trust().as_f64(),
            new_trust: outcome.new_trust().as_f64(),
            trust_delta: f64::from(outcome.trust_delta_millionths()) / 1_000_000.0,
            helpful_count: legacy_i64(outcome.helpful_count(), "legacy helpful count")?,
            unhelpful_count: legacy_i64(outcome.unhelpful_count(), "legacy unhelpful count")?,
        })
    }

    pub async fn fact_trust_history_v1(
        &self,
        fact_id: i64,
        limit: usize,
    ) -> Result<Vec<TrustHistoryEntry>, MemoryApplicationError> {
        let history = self
            .fact_trust_history_with_progress_v1(fact_id, limit)
            .await?;
        if !history.repair_progress.is_complete() {
            return Err(MemoryApplicationError::FeedbackHistoryUnavailable {
                progress: history.repair_progress,
            });
        }
        Ok(history.entries)
    }

    /// V1 trust-history entries plus explicit repair state. This is the only
    /// V1-compatible read for consumers that can represent partial history.
    pub async fn fact_trust_history_with_progress_v1(
        &self,
        fact_id: i64,
        limit: usize,
    ) -> Result<V1FactTrustHistoryV1, MemoryApplicationError> {
        let history = self
            .get_compatibility_feedback_history(CompatibilityFactFeedbackHistoryQueryV1::new(
                self.legacy_compatibility_target(fact_id)?,
                None,
                limit,
            )?)
            .await?;
        let entries = history
            .events()
            .iter()
            .filter(|event| {
                event.details_availability()
                    == CompatibilityFactFeedbackDetailsAvailabilityV1::Available
            })
            .filter_map(|event| {
                let source = event.source()?;
                Some(TrustHistoryEntry {
                    timestamp: event.occurred_at().0,
                    action: match event.action() {
                        CompatibilityFactFeedbackActionV1::Helpful => FeedbackAction::Helpful,
                        CompatibilityFactFeedbackActionV1::Unhelpful => FeedbackAction::Unhelpful,
                    },
                    old_trust: event.old_trust().as_f64(),
                    new_trust: event.new_trust().as_f64(),
                    delta: event.new_trust().as_f64() - event.old_trust().as_f64(),
                    source: source.to_owned(),
                    note: event.note().map(ToOwned::to_owned),
                })
            })
            .collect();
        Ok(V1FactTrustHistoryV1 {
            entries,
            repair_progress: history.repair_progress(),
        })
    }

    pub async fn memory_status_v1(&self) -> Result<MemoryStatus, MemoryApplicationError> {
        Ok(self.memory_status_with_repair_v1().await?.status)
    }

    /// One authority status read projected both into legacy fields and the
    /// finite feedback-history repair state.
    ///
    /// The legacy `memory_status` surface repaired derived vectors and rebuilt
    /// dirty banks as a side effect of reading, and reported the repair counts.
    /// Preserve that contract: run one bounded authoritative repair, then read
    /// the post-repair status so the projected counts reflect the repair.
    pub async fn memory_status_with_repair_v1(
        &self,
    ) -> Result<V1MemoryStatusWithRepairV1, MemoryApplicationError> {
        let context = MemoryOperationContext::generated(&self.owner, "memory-status-repair", None)?;
        let repair = self.dashboard_repair_v1(context).await?;
        let status = self.compatibility_memory_status().await?;
        let feedback_history_repair = status.feedback_history_repair();
        let mut projected = project_memory_status_v1(&status)?;
        projected.repair = MemoryRepairStats {
            missing_vectors_repaired: legacy_usize(
                repair.missing_vectors_repaired(),
                "legacy memory repaired vectors",
            )?,
            banks_rebuilt: legacy_usize(repair.banks_rebuilt(), "legacy memory rebuilt banks")?,
        };
        Ok(V1MemoryStatusWithRepairV1 {
            status: projected,
            feedback_history_repair,
        })
    }

    async fn search_v1(
        &self,
        kind: CompatibilityFactSearchKindV1,
        query: Option<String>,
        request: SearchFactsRequest,
        context: Option<MemoryOperationContext>,
        recall: bool,
    ) -> Result<Vec<FactSearchResult>, MemoryApplicationError> {
        let filter = CompatibilityFactSearchFilterV1::new(
            request.category.map(fact_category),
            compatibility_confidence(request.min_trust)?,
            None,
        )?;
        let query = CompatibilityFactSearchQuery::with_filter(
            self.owner.clone(),
            kind.clone(),
            query,
            filter,
            None,
            request.limit.unwrap_or(20),
        )?;
        let page = match kind {
            CompatibilityFactSearchKindV1::Search => self.search_compatibility_facts(query).await?,
            CompatibilityFactSearchKindV1::Probe => self.probe_compatibility_facts(query).await?,
            CompatibilityFactSearchKindV1::Related { .. } => {
                self.related_compatibility_facts(query).await?
            }
            CompatibilityFactSearchKindV1::Reason { .. } => {
                self.reason_compatibility_facts(query).await?
            }
        };
        let targets = page
            .hits()
            .iter()
            .map(|hit| {
                CompatibilityFactTargetV1::Canonical(
                    hit.fact().mapping().compatibility_id().clone(),
                )
            })
            .collect();
        let mut results = page
            .hits()
            .iter()
            .map(|hit| {
                let scores = hit.scores();
                Ok(FactSearchResult {
                    fact: compatibility_fact_record(&self.compatibility_scope, hit.fact())?,
                    score: f64::from(scores.score_millionths()) / 1_000_000.0,
                    fts_score: f64::from(scores.fts_score_millionths()) / 1_000_000.0,
                    jaccard_score: f64::from(scores.jaccard_score_millionths()) / 1_000_000.0,
                    holographic_score: f64::from(scores.holographic_score_millionths())
                        / 1_000_000.0,
                    trust_score: f64::from(scores.trust_score_millionths()) / 1_000_000.0,
                    why: request
                        .include_why
                        .then(|| hit.why().map(ToOwned::to_owned))
                        .flatten(),
                })
            })
            .collect::<Result<Vec<_>, MemoryApplicationError>>()?;
        if let Some(context) = context.as_ref() {
            self.record_v1_retrieval(targets, context, recall).await?;
        }
        if !request.include_why {
            for result in &mut results {
                result.why = None;
            }
        }
        Ok(results)
    }

    async fn record_v1_retrieval(
        &self,
        targets: Vec<CompatibilityFactTargetV1>,
        context: &MemoryOperationContext,
        recall: bool,
    ) -> Result<(), MemoryApplicationError> {
        if targets.is_empty() {
            return Ok(());
        }
        self.record_compatibility_fact_retrieval(CompatibilityFactRetrievalCommandV1::new(
            self.owner.clone(),
            context.operation_id().clone(),
            targets,
            recall,
        )?)
        .await?;
        Ok(())
    }

    async fn project_add_fact_outcome_v1(
        &self,
        outcome: CompatibilityFactAddOutcomeV1,
    ) -> Result<AddFactOutcome, MemoryApplicationError> {
        let fact = outcome
            .fact()
            .map(|fact| compatibility_projection_record(&self.compatibility_scope, fact))
            .transpose()?;
        let closest_fact_id = match outcome.closest_fact_id() {
            Some(id) => {
                let projection = self
                    .get_compatibility_fact(CompatibilityFactTargetV1::Canonical(id.clone()))
                    .await?
                    .ok_or(MemoryApplicationError::IncompatibleLegacyProjection {
                        invariant: "closest legacy fact mapping",
                    })?;
                Some(
                    compatibility_projection_record(&self.compatibility_scope, &projection)?
                        .fact_id,
                )
            }
            None => None,
        };
        Ok(AddFactOutcome {
            fact,
            diff: AddFactDiff {
                diff: match outcome.disposition() {
                    tracedecay_store::CompatibilityFactAddDispositionV1::Added => {
                        AddFactDiffKind::Add
                    }
                    tracedecay_store::CompatibilityFactAddDispositionV1::NearDuplicate => {
                        AddFactDiffKind::NearDuplicate
                    }
                    tracedecay_store::CompatibilityFactAddDispositionV1::PossibleConflict => {
                        AddFactDiffKind::PossibleConflict
                    }
                    tracedecay_store::CompatibilityFactAddDispositionV1::RejectedSecretLike => {
                        AddFactDiffKind::RejectedSecretLike
                    }
                },
                closest_fact_id,
                similarity: outcome
                    .similarity_millionths()
                    .map(|value| f64::from(value) / 1_000_000.0),
                reason: outcome.reason().map(ToOwned::to_owned),
            },
        })
    }
}

fn rejected_secret_add_outcome() -> AddFactOutcome {
    AddFactOutcome {
        fact: None,
        diff: AddFactDiff {
            diff: AddFactDiffKind::RejectedSecretLike,
            closest_fact_id: None,
            similarity: None,
            reason: Some(
                "content or structured payload was rejected by the privacy sanitizer".to_owned(),
            ),
        },
    }
}

fn compatibility_projection_targets(
    projections: &[CompatibilityFactProjectionV1],
) -> Vec<CompatibilityFactTargetV1> {
    projections
        .iter()
        .filter_map(|projection| match projection {
            CompatibilityFactProjectionV1::Available(fact) => Some(
                CompatibilityFactTargetV1::Canonical(fact.mapping().compatibility_id().clone()),
            ),
            CompatibilityFactProjectionV1::Unavailable(_) => None,
        })
        .collect()
}

fn validate_commit_outcome(
    owner: &FactOwnerV1,
    fact_id: &FactId,
    outcome: &FactCommitOutcome,
) -> Result<(), MemoryApplicationError> {
    let receipt = match outcome {
        FactCommitOutcome::Committed(receipt) | FactCommitOutcome::IdempotentReplay(receipt) => {
            Some(receipt)
        }
        FactCommitOutcome::Conflict(_) => None,
        _ => {
            return Err(MemoryApplicationError::InvalidAuthorityResult {
                invariant: "recognized fact commit outcome",
            });
        }
    };
    if receipt.is_some_and(|receipt| receipt.owner() != owner || receipt.fact_id() != fact_id) {
        return Err(MemoryApplicationError::InvalidAuthorityResult {
            invariant: "fact commit identity",
        });
    }
    Ok(())
}

fn validate_current_facts(
    owner: &FactOwnerV1,
    after_fact_id: Option<&FactId>,
    limit: usize,
    facts: &[StoredFactV1],
) -> Result<(), MemoryApplicationError> {
    if facts.len() > limit
        || facts.iter().any(|fact| fact.owner() != owner)
        || after_fact_id
            .is_some_and(|after_fact_id| facts.iter().any(|fact| fact.fact_id() <= after_fact_id))
        || facts
            .windows(2)
            .any(|pair| pair[0].fact_id() >= pair[1].fact_id())
    {
        return Err(MemoryApplicationError::InvalidAuthorityResult {
            invariant: "current fact bounds, owner, cursor, and ordering",
        });
    }
    Ok(())
}

fn validate_compatibility_page(
    owner: &FactOwnerV1,
    after_fact_id: Option<&FactId>,
    limit: usize,
    page: &CompatibilityFactPageV1,
) -> Result<(), MemoryApplicationError> {
    let facts = page.facts();
    let cursor_is_invalid = page.next_after_fact_id().is_some_and(|cursor| {
        cursor.validate_owner(owner).is_err()
            || after_fact_id.is_some_and(|after| cursor <= after)
            || facts.last().is_none_or(|last| cursor <= last.fact_id())
    });
    if page.owner() != owner
        || facts.len() > limit
        || facts.iter().any(|fact| fact.owner() != owner)
        || after_fact_id.is_some_and(|after| facts.iter().any(|fact| fact.fact_id() <= after))
        || facts
            .windows(2)
            .any(|pair| pair[0].fact_id() >= pair[1].fact_id())
        || cursor_is_invalid
    {
        return Err(MemoryApplicationError::InvalidAuthorityResult {
            invariant: "compatibility list bounds, owner, cursor, and ordering",
        });
    }
    Ok(())
}

fn validate_compatibility_search_page(
    owner: &FactOwnerV1,
    after: Option<&CompatibilityFactSearchCursorV1>,
    limit: usize,
    page: &CompatibilityFactSearchPageV1,
) -> Result<(), MemoryApplicationError> {
    let hits = page.hits();
    let cursor_is_invalid = page.next_after().is_some_and(|cursor| {
        cursor.fact_id().validate_owner(owner).is_err()
            || hits.last().is_none_or(|last| {
                cursor.score_millionths() != last.score_millionths()
                    || cursor.updated_at() != last.fact().telemetry().updated_at()
                    || cursor.fact_id() != last.fact().fact_id()
            })
    });
    if page.owner() != owner
        || hits.len() > limit
        || hits.iter().any(|hit| hit.fact().owner() != owner)
        || after.is_some_and(|after| {
            hits.iter()
                .any(|hit| !search_hit_follows_cursor(hit, after))
        })
        || hits.windows(2).any(|pair| {
            pair[0].score_millionths() < pair[1].score_millionths()
                || (pair[0].score_millionths() == pair[1].score_millionths()
                    && (pair[0].fact().telemetry().updated_at()
                        < pair[1].fact().telemetry().updated_at()
                        || (pair[0].fact().telemetry().updated_at()
                            == pair[1].fact().telemetry().updated_at()
                            && pair[0].fact().fact_id() >= pair[1].fact().fact_id())))
        })
        || cursor_is_invalid
    {
        return Err(MemoryApplicationError::InvalidAuthorityResult {
            invariant: "compatibility search bounds, owner, cursor, and ordering",
        });
    }
    Ok(())
}

fn search_hit_follows_cursor(
    hit: &tracedecay_store::CompatibilityFactSearchHitV1,
    after: &CompatibilityFactSearchCursorV1,
) -> bool {
    hit.score_millionths() < after.score_millionths()
        || (hit.score_millionths() == after.score_millionths()
            && (hit.fact().telemetry().updated_at() < after.updated_at()
                || (hit.fact().telemetry().updated_at() == after.updated_at()
                    && hit.fact().fact_id() > after.fact_id())))
}

fn validate_lineage(
    owner: &FactOwnerV1,
    fact_id: &FactId,
    after: Option<&tracedecay_store::FactLineageCursor>,
    limit: usize,
    events: &[FactLineageEventV1],
) -> Result<(), MemoryApplicationError> {
    if events.len() > limit
        || events
            .iter()
            .any(|event| event.owner() != owner || event.fact_id() != fact_id)
        || after.is_some_and(|after| {
            events.iter().any(|event| {
                (event.occurred_at(), event.event_id()) <= (after.occurred_at(), after.event_id())
            })
        })
        || events.windows(2).any(|pair| {
            (pair[0].occurred_at(), pair[0].event_id())
                >= (pair[1].occurred_at(), pair[1].event_id())
        })
    {
        return Err(MemoryApplicationError::InvalidAuthorityResult {
            invariant: "fact lineage bounds, owner, cursor, and ordering",
        });
    }
    Ok(())
}

fn validate_compatibility_projection(
    owner: &FactOwnerV1,
    target: &CompatibilityFactTargetV1,
    projection: &CompatibilityFactProjectionV1,
) -> Result<(), MemoryApplicationError> {
    if projection.owner() != owner {
        return Err(MemoryApplicationError::InvalidAuthorityResult {
            invariant: "compatibility projection owner",
        });
    }
    if let Some(fact_id) = target.canonical_fact_id() {
        if projection.fact_id() != fact_id {
            return Err(MemoryApplicationError::InvalidAuthorityResult {
                invariant: "compatibility projection canonical identity",
            });
        }
    } else if let (Some(query), CompatibilityFactProjectionV1::Available(fact)) =
        (target.legacy_query(), projection)
    {
        let mapping = fact.mapping().legacy_mapping();
        if mapping.is_none_or(|mapping| {
            mapping.owner() != owner
                || mapping.source_store_id() != query.source_store_id()
                || mapping.legacy_fact_id() != query.legacy_fact_id()
        }) {
            return Err(MemoryApplicationError::InvalidAuthorityResult {
                invariant: "compatibility projection legacy mapping",
            });
        }
    }
    Ok(())
}

fn validate_compatibility_inspection(
    owner: &FactOwnerV1,
    target: &CompatibilityFactTargetV1,
    inspection: &CompatibilityFactInspectionV1,
) -> Result<(), MemoryApplicationError> {
    if inspection.owner() != owner
        || inspection.history().owner() != owner
        || inspection.status().owner() != owner
        || inspection.history().fact_id() != inspection.fact().fact_id()
        || inspection
            .status()
            .fact_id()
            .is_some_and(|fact_id| fact_id != inspection.fact().fact_id())
        || inspection
            .anchors()
            .iter()
            .any(|anchor| FactOwnerV1::from(anchor.owner().clone()) != *owner)
        || inspection
            .anchors()
            .windows(2)
            .any(|pair| pair[0].anchor_id() >= pair[1].anchor_id())
    {
        return Err(MemoryApplicationError::InvalidAuthorityResult {
            invariant: "compatibility inspection owner and identity",
        });
    }
    match target {
        CompatibilityFactTargetV1::Canonical(target)
            if inspection.fact().fact_id() != target.fact_id() =>
        {
            Err(MemoryApplicationError::InvalidAuthorityResult {
                invariant: "compatibility inspection canonical identity",
            })
        }
        CompatibilityFactTargetV1::Legacy(query) => {
            let mapping = inspection.fact().mapping().legacy_mapping();
            if mapping.is_none_or(|mapping| {
                mapping.owner() != owner
                    || mapping.source_store_id() != query.source_store_id()
                    || mapping.legacy_fact_id() != query.legacy_fact_id()
            }) {
                return Err(MemoryApplicationError::InvalidAuthorityResult {
                    invariant: "compatibility inspection legacy mapping",
                });
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn validate_compatibility_add_outcome(
    owner: &FactOwnerV1,
    outcome: &CompatibilityFactAddOutcomeV1,
) -> Result<(), MemoryApplicationError> {
    if outcome
        .fact()
        .is_some_and(|projection| projection.owner() != owner)
        || outcome
            .closest_fact_id()
            .is_some_and(|fact_id| fact_id.owner() != owner)
    {
        return Err(MemoryApplicationError::InvalidAuthorityResult {
            invariant: "compatibility add outcome owner",
        });
    }
    Ok(())
}

fn validate_compatibility_proposal(
    owner: &FactOwnerV1,
    proposal_id: &ProvenanceId,
    proposal: &CompatibilityFactProposalRecordV1,
) -> Result<(), MemoryApplicationError> {
    if proposal.owner() != owner
        || proposal.proposal_id() != proposal_id
        || proposal.request().owner() != owner
    {
        return Err(MemoryApplicationError::InvalidAuthorityResult {
            invariant: "compatibility proposal owner and identity",
        });
    }
    Ok(())
}

fn validate_compatibility_proposal_page(
    owner: &FactOwnerV1,
    after_proposal_id: Option<&ProvenanceId>,
    limit: usize,
    page: &CompatibilityFactProposalPageV1,
) -> Result<(), MemoryApplicationError> {
    let proposals = page.proposals();
    let cursor_is_invalid = page.next_after_proposal_id().is_some_and(|cursor| {
        cursor.validate().is_err()
            || after_proposal_id.is_some_and(|after| cursor <= after)
            || proposals
                .last()
                .is_none_or(|proposal| cursor <= proposal.proposal_id())
    });
    if page.owner() != owner
        || proposals.len() > limit
        || proposals.iter().any(|proposal| proposal.owner() != owner)
        || after_proposal_id.is_some_and(|after| {
            proposals
                .iter()
                .any(|proposal| proposal.proposal_id() <= after)
        })
        || proposals
            .windows(2)
            .any(|pair| pair[0].proposal_id() >= pair[1].proposal_id())
        || cursor_is_invalid
    {
        return Err(MemoryApplicationError::InvalidAuthorityResult {
            invariant: "compatibility proposal page bounds, owner, cursor, and ordering",
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use tracedecay_domain::{
        AccessPolicyDigest, AnchorDurabilityClass, AnchorSourceGenerationV2, CapabilityId,
        Confidence, CoverageReportV1, EntityId, EntityKind, EntityRef, EvidenceClass,
        FactAssertionId, FactEventId, FactIdentityMaterialV1, FactIdentitySourceV1,
        FactLineageEventKindV1, ObservationScopeV1, PayloadAccessState,
        PrivacyDomainBoundLocatorDigest, PrivacyDomainId, ProjectId, ProjectionGenerationId,
        ResolutionAuthorizationV1, RetentionClass, RetrievalAnchorId, RetrievalAnchorRecordV2Parts,
        RetrievalAnchorTargetV2, ScopeResolutionId, SourceStoreId, UtcMicros, VectorWatermark,
    };
    use tracedecay_store::{
        FactCommitReceipt, FactLineageCursor, FactProposalPromotionStateV1, FactStoreResult,
    };

    use super::*;
    use crate::memory::types::{AddFactRequest, MemoryCategory};

    #[derive(Default)]
    struct FakeAuthority {
        committed: Mutex<Vec<FactWriteBatch>>,
        next_commit_outcome: Mutex<Option<FactCommitOutcome>>,
        promotions: Mutex<Vec<PromoteFactProposal>>,
        promotion_conflict: Mutex<Option<Option<FactProposalPromotionStateV1>>>,
        current_queries: Mutex<Vec<CurrentFactsQuery>>,
        current_results: Mutex<Vec<StoredFactV1>>,
        current_fact_queries: Mutex<Vec<FactCurrentQuery>>,
        current_fact_result: Mutex<Option<StoredFactV1>>,
        as_of_queries: Mutex<Vec<FactAsOfQuery>>,
        as_of_result: Mutex<Option<StoredFactV1>>,
        lineage_queries: Mutex<Vec<FactLineageQuery>>,
        lineage_results: Mutex<Vec<FactLineageEventV1>>,
        legacy_queries: Mutex<Vec<LegacyFactQuery>>,
        legacy_result: Mutex<Option<FactId>>,
        anchor_queries: Mutex<Vec<RetrievalAnchorId>>,
        feedback_history: Mutex<Option<CompatibilityFactFeedbackHistoryV1>>,
        feedback_requests: Mutex<Vec<CompatibilityFactFeedbackCommandV1>>,
        compatibility_calls: Mutex<Vec<&'static str>>,
    }

    #[derive(Default)]
    struct UnavailableEvidenceResolver {
        requests: Mutex<Vec<(FactOwnerV1, RetrievalAnchorId)>>,
    }

    impl EvidenceAnchorResolver for UnavailableEvidenceResolver {
        async fn resolve_evidence_anchor(
            &self,
            owner: FactOwnerV1,
            anchor_id: RetrievalAnchorId,
        ) -> Result<ResolvedEvidenceAnchorV1, EvidenceAnchorResolutionError> {
            self.requests
                .lock()
                .unwrap()
                .push((owner, anchor_id.clone()));
            Err(EvidenceAnchorResolutionError::Unavailable { anchor_id })
        }
    }

    struct StaticEvidenceResolver {
        record: ResolvedEvidenceAnchorV1,
    }

    impl EvidenceAnchorResolver for StaticEvidenceResolver {
        async fn resolve_evidence_anchor(
            &self,
            _owner: FactOwnerV1,
            _anchor_id: RetrievalAnchorId,
        ) -> Result<ResolvedEvidenceAnchorV1, EvidenceAnchorResolutionError> {
            Ok(self.record.clone())
        }
    }

    impl FactStore for FakeAuthority {
        async fn commit_fact(&self, batch: FactWriteBatch) -> FactStoreResult<FactCommitOutcome> {
            let outcome = self
                .next_commit_outcome
                .lock()
                .unwrap()
                .take()
                .unwrap_or_else(|| committed_outcome(&batch));
            self.committed.lock().unwrap().push(batch);
            Ok(outcome)
        }

        async fn query_current_facts(
            &self,
            query: CurrentFactsQuery,
        ) -> FactStoreResult<Vec<StoredFactV1>> {
            self.current_queries.lock().unwrap().push(query);
            Ok(self.current_results.lock().unwrap().clone())
        }

        async fn query_fact_as_of(
            &self,
            query: FactAsOfQuery,
        ) -> FactStoreResult<Option<StoredFactV1>> {
            self.as_of_queries.lock().unwrap().push(query);
            Ok(self.as_of_result.lock().unwrap().clone())
        }

        async fn query_fact_current(
            &self,
            query: FactCurrentQuery,
        ) -> FactStoreResult<Option<StoredFactV1>> {
            self.current_fact_queries.lock().unwrap().push(query);
            Ok(self.current_fact_result.lock().unwrap().clone())
        }

        async fn query_fact_lineage(
            &self,
            query: FactLineageQuery,
        ) -> FactStoreResult<Vec<FactLineageEventV1>> {
            self.lineage_queries.lock().unwrap().push(query);
            Ok(self.lineage_results.lock().unwrap().clone())
        }

        async fn resolve_legacy_fact(
            &self,
            query: LegacyFactQuery,
        ) -> FactStoreResult<Option<FactId>> {
            self.legacy_queries.lock().unwrap().push(query);
            Ok(self.legacy_result.lock().unwrap().clone())
        }

        async fn get_retrieval_anchor(
            &self,
            query: RetrievalAnchorQuery,
        ) -> FactStoreResult<Option<RetrievalAnchorRecordV2>> {
            self.anchor_queries
                .lock()
                .unwrap()
                .push(query.anchor_id().clone());
            Ok(None)
        }
    }

    impl FactProposalStore for FakeAuthority {
        async fn promote_fact_proposal(
            &self,
            promotion: PromoteFactProposal,
        ) -> Result<PromoteFactProposalOutcome, FactProposalStoreError> {
            if let Some(actual) = self.promotion_conflict.lock().unwrap().take() {
                return Err(FactProposalStoreError::ProposalStateConflict {
                    proposal_id: promotion.proposal_id().clone(),
                    expected: promotion.expected_state(),
                    actual,
                });
            }
            let outcome = committed_outcome(promotion.batch());
            let result = PromoteFactProposalOutcome::new(
                promotion.proposal_id().clone(),
                promotion.expected_state(),
                outcome,
            )
            .map_err(FactStoreError::from)?;
            self.promotions.lock().unwrap().push(promotion);
            Ok(result)
        }
    }

    impl FactCompatibilityStore for FakeAuthority {
        async fn list_compatibility_facts(
            &self,
            query: CompatibilityFactListQueryV1,
        ) -> Result<CompatibilityFactPageV1, FactCompatibilityStoreError> {
            self.compatibility_calls.lock().unwrap().push("list");
            Ok(CompatibilityFactPageV1::new(
                query.owner().clone(),
                vec![],
                None,
            )?)
        }

        async fn search_compatibility_facts(
            &self,
            query: CompatibilityFactSearchQuery,
        ) -> Result<CompatibilityFactSearchPageV1, FactCompatibilityStoreError> {
            self.compatibility_calls.lock().unwrap().push("search");
            Ok(CompatibilityFactSearchPageV1::new(
                query.owner().clone(),
                vec![],
                None,
            )?)
        }

        async fn probe_compatibility_facts(
            &self,
            query: CompatibilityFactSearchQuery,
        ) -> Result<CompatibilityFactSearchPageV1, FactCompatibilityStoreError> {
            self.compatibility_calls.lock().unwrap().push("probe");
            Ok(CompatibilityFactSearchPageV1::new(
                query.owner().clone(),
                vec![],
                None,
            )?)
        }

        async fn related_compatibility_facts(
            &self,
            query: CompatibilityFactSearchQuery,
        ) -> Result<CompatibilityFactSearchPageV1, FactCompatibilityStoreError> {
            self.compatibility_calls.lock().unwrap().push("related");
            Ok(CompatibilityFactSearchPageV1::new(
                query.owner().clone(),
                vec![],
                None,
            )?)
        }

        async fn reason_compatibility_facts(
            &self,
            query: CompatibilityFactSearchQuery,
        ) -> Result<CompatibilityFactSearchPageV1, FactCompatibilityStoreError> {
            self.compatibility_calls.lock().unwrap().push("reason");
            Ok(CompatibilityFactSearchPageV1::new(
                query.owner().clone(),
                vec![],
                None,
            )?)
        }

        async fn find_compatibility_contradictions(
            &self,
            query: CompatibilityFactContradictionQueryV1,
        ) -> Result<CompatibilityFactContradictionPageV1, FactCompatibilityStoreError> {
            self.compatibility_calls
                .lock()
                .unwrap()
                .push("contradictions");
            Ok(CompatibilityFactContradictionPageV1::new(
                query.owner().clone(),
                vec![],
            )?)
        }

        async fn get_compatibility_fact(
            &self,
            _target: CompatibilityFactTargetV1,
        ) -> Result<Option<CompatibilityFactProjectionV1>, FactCompatibilityStoreError> {
            self.compatibility_calls.lock().unwrap().push("get");
            Ok(None)
        }

        async fn compatibility_fact_history(
            &self,
            query: CompatibilityFactHistoryQueryV1,
        ) -> Result<CompatibilityFactHistoryV1, FactCompatibilityStoreError> {
            self.compatibility_calls.lock().unwrap().push("history");
            let Some(fact_id) = query.target().canonical_fact_id() else {
                return Err(compatibility_fixture_error());
            };
            Ok(CompatibilityFactHistoryV1::new(
                query.target().owner().clone(),
                fact_id.clone(),
                vec![],
                None,
            )?)
        }

        async fn compatibility_memory_status(
            &self,
            owner: FactOwnerV1,
        ) -> Result<CompatibilityMemoryStatusV1, FactCompatibilityStoreError> {
            self.compatibility_calls.lock().unwrap().push("status");
            compatibility_memory_status(owner)
        }

        async fn inspect_compatibility_fact(
            &self,
            _target: CompatibilityFactTargetV1,
        ) -> Result<Option<CompatibilityFactInspectionV1>, FactCompatibilityStoreError> {
            self.compatibility_calls.lock().unwrap().push("inspect");
            Ok(None)
        }

        async fn add_compatibility_fact(
            &self,
            _request: CompatibilityFactAddCommandV1,
        ) -> Result<CompatibilityFactAddOutcomeV1, FactCompatibilityStoreError> {
            self.compatibility_calls.lock().unwrap().push("add");
            Ok(CompatibilityFactAddOutcomeV1::new(
                None,
                tracedecay_store::CompatibilityFactAddDispositionV1::RejectedSecretLike,
                None,
                None,
                Some("fixture rejection".to_owned()),
            )?)
        }

        async fn update_compatibility_fact(
            &self,
            _request: CompatibilityFactUpdateCommandV1,
        ) -> Result<CompatibilityFactUpdateOutcomeV1, FactCompatibilityStoreError> {
            self.compatibility_calls.lock().unwrap().push("update");
            Err(compatibility_fixture_error())
        }

        async fn remove_compatibility_fact(
            &self,
            _request: CompatibilityFactRemoveCommandV1,
        ) -> Result<CompatibilityFactRemoveOutcomeV1, FactCompatibilityStoreError> {
            self.compatibility_calls.lock().unwrap().push("remove");
            Err(compatibility_fixture_error())
        }

        async fn record_compatibility_fact_feedback(
            &self,
            request: CompatibilityFactFeedbackCommandV1,
        ) -> Result<CompatibilityFactFeedbackOutcomeV1, FactCompatibilityStoreError> {
            self.compatibility_calls.lock().unwrap().push("feedback");
            self.feedback_requests.lock().unwrap().push(request);
            Err(compatibility_fixture_error())
        }

        async fn compatibility_fact_feedback_history(
            &self,
            _query: CompatibilityFactFeedbackHistoryQueryV1,
        ) -> Result<CompatibilityFactFeedbackHistoryV1, FactCompatibilityStoreError> {
            self.compatibility_calls
                .lock()
                .unwrap()
                .push("feedback-history");
            self.feedback_history
                .lock()
                .unwrap()
                .clone()
                .ok_or_else(compatibility_fixture_error)
        }

        async fn find_compatibility_fact_by_content_digest(
            &self,
            _query: CompatibilityFactContentDigestQueryV1,
        ) -> Result<Option<CompatibilityFactProjectionV1>, FactCompatibilityStoreError> {
            self.compatibility_calls
                .lock()
                .unwrap()
                .push("exact-content");
            Ok(None)
        }

        async fn apply_compatibility_fact_curation(
            &self,
            _request: CompatibilityFactCurationBatchV1,
        ) -> Result<CompatibilityFactCurationReceiptV1, FactCompatibilityStoreError> {
            self.compatibility_calls.lock().unwrap().push("curation");
            Err(compatibility_fixture_error())
        }

        async fn merge_compatibility_facts(
            &self,
            _request: CompatibilityFactMergeCommandV1,
        ) -> Result<CompatibilityFactMergeOutcomeV1, FactCompatibilityStoreError> {
            self.compatibility_calls.lock().unwrap().push("merge");
            Err(compatibility_fixture_error())
        }

        async fn repair_compatibility_memory(
            &self,
            _request: CompatibilityMemoryRepairCommandV1,
        ) -> Result<CompatibilityMemoryRepairStatsV1, FactCompatibilityStoreError> {
            self.compatibility_calls.lock().unwrap().push("repair");
            Ok(CompatibilityMemoryRepairStatsV1::default())
        }

        async fn advance_compatibility_legacy_memory_cutover(
            &self,
            _request: CompatibilityLegacyMemoryCutoverCommandV1,
        ) -> Result<CompatibilityLegacyMemoryCutoverProgressV1, FactCompatibilityStoreError>
        {
            self.compatibility_calls
                .lock()
                .unwrap()
                .push("legacy-cutover");
            Ok(CompatibilityLegacyMemoryCutoverProgressV1::Complete)
        }

        async fn dashboard_compatibility_memory_overview(
            &self,
            _query: CompatibilityDashboardMemoryOverviewQueryV1,
        ) -> Result<CompatibilityDashboardMemoryOverviewV1, FactCompatibilityStoreError> {
            self.compatibility_calls
                .lock()
                .unwrap()
                .push("dashboard-overview");
            Err(compatibility_fixture_error())
        }

        async fn dashboard_compatibility_fact_detail(
            &self,
            _query: CompatibilityDashboardFactDetailQueryV1,
        ) -> Result<Option<CompatibilityDashboardFactDetailV1>, FactCompatibilityStoreError>
        {
            self.compatibility_calls
                .lock()
                .unwrap()
                .push("dashboard-detail");
            Ok(None)
        }

        async fn dashboard_compatibility_vector_points(
            &self,
            _query: CompatibilityDashboardVectorPointsQueryV1,
        ) -> Result<Vec<CompatibilityDashboardVectorPointV1>, FactCompatibilityStoreError> {
            self.compatibility_calls
                .lock()
                .unwrap()
                .push("dashboard-vectors");
            Ok(vec![])
        }

        async fn dashboard_compatibility_memory_oplog(
            &self,
            _query: CompatibilityDashboardOplogQueryV1,
        ) -> Result<Vec<CompatibilityDashboardOplogEntryV1>, FactCompatibilityStoreError> {
            self.compatibility_calls
                .lock()
                .unwrap()
                .push("dashboard-oplog");
            Ok(vec![])
        }

        async fn record_compatibility_fact_retrieval(
            &self,
            _request: CompatibilityFactRetrievalCommandV1,
        ) -> Result<Vec<CompatibilityFactProjectionV1>, FactCompatibilityStoreError> {
            self.compatibility_calls.lock().unwrap().push("retrieval");
            Ok(vec![])
        }

        async fn submit_compatibility_fact_proposal(
            &self,
            _proposal_id: ProvenanceId,
            _request: CompatibilityFactAddCommandV1,
            _submitter: Option<ActorId>,
        ) -> Result<CompatibilityFactProposalRecordV1, FactCompatibilityStoreError> {
            self.compatibility_calls
                .lock()
                .unwrap()
                .push("proposal-submit");
            Err(compatibility_fixture_error())
        }

        async fn get_compatibility_fact_proposal(
            &self,
            _owner: FactOwnerV1,
            _proposal_id: ProvenanceId,
        ) -> Result<Option<CompatibilityFactProposalRecordV1>, FactCompatibilityStoreError>
        {
            self.compatibility_calls
                .lock()
                .unwrap()
                .push("proposal-get");
            Ok(None)
        }

        async fn list_compatibility_fact_proposals(
            &self,
            _owner: FactOwnerV1,
            _state: Option<CompatibilityFactProposalStateV1>,
            _after_proposal_id: Option<ProvenanceId>,
            _limit: usize,
        ) -> Result<CompatibilityFactProposalPageV1, FactCompatibilityStoreError> {
            self.compatibility_calls
                .lock()
                .unwrap()
                .push("proposal-list");
            Err(compatibility_fixture_error())
        }

        async fn count_pending_compatibility_fact_proposals(
            &self,
            _owner: FactOwnerV1,
        ) -> Result<u64, FactCompatibilityStoreError> {
            self.compatibility_calls
                .lock()
                .unwrap()
                .push("proposal-count-pending");
            Ok(0)
        }

        async fn reject_compatibility_fact_proposal(
            &self,
            _owner: FactOwnerV1,
            _proposal_id: ProvenanceId,
            _expected_revision: CompatibilityFactProposalRevisionV1,
            _reviewer: ActorId,
            _reason: String,
        ) -> Result<CompatibilityFactProposalRecordV1, FactCompatibilityStoreError> {
            self.compatibility_calls
                .lock()
                .unwrap()
                .push("proposal-reject");
            Err(compatibility_fixture_error())
        }

        async fn import_legacy_compatibility_fact_proposals(
            &self,
            _request: CompatibilityFactProposalImportV1,
        ) -> Result<CompatibilityFactProposalImportReceiptV1, FactCompatibilityStoreError> {
            self.compatibility_calls
                .lock()
                .unwrap()
                .push("proposal-import");
            Err(compatibility_fixture_error())
        }

        async fn promote_compatibility_fact_proposal(
            &self,
            _request: CompatibilityFactProposalPromotionV1,
        ) -> Result<CompatibilityFactProposalRecordV1, FactCompatibilityStoreError> {
            self.compatibility_calls
                .lock()
                .unwrap()
                .push("proposal-promote");
            Err(compatibility_fixture_error())
        }

        async fn promote_compatibility_fact_proposal_with_disposition(
            &self,
            _request: CompatibilityFactProposalPromotionV1,
        ) -> Result<CompatibilityFactProposalPromotionResultV1, FactCompatibilityStoreError>
        {
            self.compatibility_calls
                .lock()
                .unwrap()
                .push("proposal-promote-disposition");
            Err(compatibility_fixture_error())
        }
    }

    fn compatibility_fixture_error() -> FactCompatibilityStoreError {
        FactCompatibilityStoreError::Store(FactStoreError::Contract(DomainError::NonCanonical {
            field: "fake compatibility authority",
        }))
    }

    fn compatibility_memory_status(
        owner: FactOwnerV1,
    ) -> Result<CompatibilityMemoryStatusV1, FactCompatibilityStoreError> {
        Ok(CompatibilityMemoryStatusV1::new(
            owner,
            0,
            0,
            0,
            tracedecay_store::CompatibilityMemoryAlgebraV1::new("fixture".to_owned(), 1, 1)?,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            true,
            tracedecay_store::CompatibilityProjectionStateV1::Ready,
            tracedecay_store::CompatibilityMemoryRepairStatsV1::default(),
            tracedecay_store::CompatibilityMemoryFeedbackFunnelV1::new(0, 0, 0, 0, 0),
        )?)
    }

    fn owner() -> FactOwnerV1 {
        FactOwnerV1::Project {
            project_id: ProjectId::new("project.memory.application").unwrap(),
        }
    }

    fn id<T>(value: &str) -> T
    where
        T: TryFrom<String, Error = DomainError>,
    {
        T::try_from(value.to_owned()).unwrap()
    }

    fn fact_id(owner: FactOwnerV1, operation: &str) -> FactId {
        FactId::derive(
            &FactIdentityMaterialV1::new(
                owner,
                FactIdentitySourceV1::Application {
                    operation_id: id(operation),
                },
            )
            .unwrap(),
        )
        .unwrap()
    }

    fn batch(owner: FactOwnerV1, operation: &str) -> FactWriteBatch {
        let fact_id = fact_id(owner.clone(), operation);
        let event = FactLineageEventV1::new(
            fact_id.clone(),
            owner.clone(),
            FactLineageEventKindV1::PayloadAccessChanged {
                previous: PayloadAccessState::Eligible,
                current: PayloadAccessState::Deleted,
            },
            UtcMicros(1),
            None,
        )
        .unwrap();
        FactWriteBatch::new(
            fact_id,
            owner,
            None,
            vec![event],
            vec![],
            vec![],
            None,
            None,
        )
        .unwrap()
    }

    fn committed_outcome(batch: &FactWriteBatch) -> FactCommitOutcome {
        let event_ids: Vec<FactEventId> = batch
            .events()
            .iter()
            .map(|event| event.event_id().clone())
            .collect();
        let last_event_id = event_ids.last().unwrap().clone();
        let active_assertion_id: Option<FactAssertionId> = batch
            .assertion()
            .map(|assertion| assertion.assertion_id().clone());
        FactCommitOutcome::Committed(
            FactCommitReceipt::new(
                batch.fact_id().clone(),
                batch.owner().clone(),
                event_ids,
                last_event_id,
                active_assertion_id,
            )
            .unwrap(),
        )
    }

    fn stored_fact(
        owner: FactOwnerV1,
        operation: &str,
        projected_as_of: UtcMicros,
    ) -> StoredFactV1 {
        let fact_id = fact_id(owner.clone(), operation);
        StoredFactV1::new(
            fact_id,
            owner,
            None,
            PayloadAccessState::Deleted,
            Confidence::new(0.5).unwrap(),
            id(&format!("assertion.{operation}")),
            id(&format!("event.{operation}")),
            None,
            projected_as_of,
        )
        .unwrap()
    }

    fn profile_anchor() -> RetrievalAnchorRecordV2 {
        const DIGEST_A: &str =
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        const DIGEST_B: &str =
            "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        RetrievalAnchorRecordV2::new(RetrievalAnchorRecordV2Parts {
            target: RetrievalAnchorTargetV2::Entity(EntityRef {
                id: EntityId::new("entity.memory.external").unwrap(),
                kind: EntityKind::Document,
            }),
            owner: ObservationScopeV1::Profile,
            aliases: vec![],
            occurred_at: None,
            ingested_at: UtcMicros(1),
            evidence_class: EvidenceClass::Observed,
            source_generation: AnchorSourceGenerationV2::Unknown,
            projection_generation: ProjectionGenerationId::new("projection.memory.external")
                .unwrap(),
            projection_watermark: VectorWatermark::default(),
            coverage: CoverageReportV1::default(),
            source_observations: vec![],
            source_anchors: vec![],
            authorization: ResolutionAuthorizationV1 {
                resolved_scope_id: ScopeResolutionId::new("scope.memory.external").unwrap(),
                privacy_domain_id: PrivacyDomainId::new("privacy.memory.external").unwrap(),
                access_policy_digest: AccessPolicyDigest::new(DIGEST_A).unwrap(),
                capability_id: CapabilityId::new("capability.memory.external").unwrap(),
                canonical_request_digest: PrivacyDomainBoundLocatorDigest::new(DIGEST_B).unwrap(),
            },
            payload_access: PayloadAccessState::Eligible,
            retention_class: RetentionClass::new("retention.memory.external").unwrap(),
            durability: AnchorDurabilityClass::DurableEvidence,
        })
        .unwrap()
    }

    fn legacy_add_request() -> AddFactRequest {
        AddFactRequest {
            content: "legacy conversion fixture".to_owned(),
            category: MemoryCategory::Project,
            source: None,
            tags: vec![],
            entities: vec![],
            trust: None,
            metadata: serde_json::json!({}),
        }
    }

    #[tokio::test]
    async fn canonical_batch_is_the_single_write_boundary() {
        let application = MemoryApplication::new(owner(), FakeAuthority::default()).unwrap();
        let write = batch(owner(), "operation.memory.commit");
        let expected_fact_id = write.fact_id().clone();

        let outcome = application.commit_fact(write).await.unwrap();

        assert!(matches!(outcome, FactCommitOutcome::Committed(_)));
        let committed = application.authority.committed.lock().unwrap();
        assert_eq!(committed.len(), 1);
        assert_eq!(committed[0].fact_id(), &expected_fact_id);
    }

    #[tokio::test]
    async fn idempotent_replay_preserves_the_canonical_commit_identity() {
        let application = MemoryApplication::new(owner(), FakeAuthority::default()).unwrap();
        let write = batch(owner(), "operation.memory.replay");
        let replay = match committed_outcome(&write) {
            FactCommitOutcome::Committed(receipt) => FactCommitOutcome::IdempotentReplay(receipt),
            _ => unreachable!("fixture always commits"),
        };
        *application.authority.next_commit_outcome.lock().unwrap() = Some(replay);

        let outcome = application.commit_fact(write).await.unwrap();

        assert!(matches!(outcome, FactCommitOutcome::IdempotentReplay(_)));
        assert_eq!(application.authority.committed.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn evidence_resolution_is_owner_bound_at_the_daemon_boundary() {
        let application = MemoryApplication::new(owner(), FakeAuthority::default()).unwrap();
        let resolver = UnavailableEvidenceResolver::default();
        let anchor_id = id::<RetrievalAnchorId>("anchor.memory.external");

        let error = application
            .resolve_evidence_anchor(&resolver, anchor_id.clone())
            .await
            .unwrap_err();

        assert!(matches!(
            error,
            MemoryApplicationError::EvidenceAnchor(EvidenceAnchorResolutionError::Unavailable {
                anchor_id: actual,
            }) if actual == anchor_id
        ));
        assert_eq!(
            resolver.requests.lock().unwrap().as_slice(),
            &[(owner(), anchor_id)]
        );
    }

    #[tokio::test]
    async fn evidence_resolution_rejects_a_cross_owner_daemon_reply() {
        let application = MemoryApplication::new(owner(), FakeAuthority::default()).unwrap();
        let record = profile_anchor();
        let anchor_id = record.anchor_id().clone();
        let resolver = StaticEvidenceResolver {
            record: ResolvedEvidenceAnchorV1::new(record).unwrap(),
        };

        let error = application
            .resolve_evidence_anchor(&resolver, anchor_id)
            .await
            .unwrap_err();

        assert!(matches!(
            error,
            MemoryApplicationError::InvalidAuthorityResult {
                invariant: "resolved evidence anchor identity and owner"
            }
        ));
    }

    #[tokio::test]
    async fn owner_mismatch_is_rejected_before_authority_access() {
        let application = MemoryApplication::new(owner(), FakeAuthority::default()).unwrap();
        let error = application
            .commit_fact(batch(FactOwnerV1::Profile, "operation.profile.commit"))
            .await
            .unwrap_err();

        assert!(matches!(
            error,
            MemoryApplicationError::OwnerMismatch { .. }
        ));
        assert!(application.authority.committed.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn query_owner_mismatch_is_rejected_before_authority_access() {
        let application = MemoryApplication::new(owner(), FakeAuthority::default()).unwrap();
        let query = CurrentFactsQuery::new(FactOwnerV1::Profile, None, 10).unwrap();

        let error = application.query_current_facts(query).await.unwrap_err();

        assert!(matches!(
            error,
            MemoryApplicationError::OwnerMismatch { .. }
        ));
        assert!(
            application
                .authority
                .current_queries
                .lock()
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn compatibility_reads_use_finite_owner_bound_authority_methods() {
        let application = MemoryApplication::new(owner(), FakeAuthority::default()).unwrap();
        let fact_id = fact_id(owner(), "operation.compatibility.read");
        let target = CompatibilityFactTargetV1::Canonical(
            tracedecay_store::CompatibilityFactIdV1::new(owner(), fact_id).unwrap(),
        );
        let search = CompatibilityFactSearchQuery::new(
            owner(),
            tracedecay_store::CompatibilityFactSearchKindV1::Search,
            Some("compatibility fixture".to_owned()),
            None,
            10,
        )
        .unwrap();

        assert!(
            application
                .list_compatibility_facts(
                    CompatibilityFactListQueryV1::new(owner(), None, None, None, 10).unwrap(),
                )
                .await
                .unwrap()
                .facts()
                .is_empty()
        );
        assert!(
            application
                .search_compatibility_facts(search)
                .await
                .unwrap()
                .hits()
                .is_empty()
        );
        assert!(
            application
                .get_compatibility_fact(target.clone())
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            application
                .get_compatibility_history(
                    CompatibilityFactHistoryQueryV1::new(target.clone(), None, 10).unwrap(),
                )
                .await
                .unwrap()
                .events()
                .is_empty()
        );
        assert_eq!(
            application
                .compatibility_memory_status()
                .await
                .unwrap()
                .owner(),
            &owner()
        );
        assert!(
            application
                .inspect_compatibility_fact(target)
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(
            application
                .authority
                .compatibility_calls
                .lock()
                .unwrap()
                .as_slice(),
            ["list", "search", "get", "history", "status", "inspect"]
        );
    }

    #[tokio::test]
    async fn compatibility_read_owner_mismatch_never_reaches_authority() {
        let application = MemoryApplication::new(owner(), FakeAuthority::default()).unwrap();
        let error = application
            .list_compatibility_facts(
                CompatibilityFactListQueryV1::new(FactOwnerV1::Profile, None, None, None, 10)
                    .unwrap(),
            )
            .await
            .unwrap_err();

        assert!(matches!(
            error,
            MemoryApplicationError::OwnerMismatch { .. }
        ));
        assert!(
            application
                .authority
                .compatibility_calls
                .lock()
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn proposal_cas_and_batch_commit_are_one_authority_operation() {
        let application = MemoryApplication::new(owner(), FakeAuthority::default()).unwrap();
        let promotion = PromoteFactProposal::new(
            id("proposal.memory.1"),
            owner(),
            FactProposalPromotionStateV1::PendingApproval,
            Some(id("actor.reviewer")),
            batch(owner(), "operation.proposal.promote"),
        )
        .unwrap();

        let outcome = application.promote_fact_proposal(promotion).await.unwrap();

        assert!(matches!(outcome.commit(), FactCommitOutcome::Committed(_)));
        assert_eq!(application.authority.promotions.lock().unwrap().len(), 1);
        assert!(application.authority.committed.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn proposal_cas_conflict_is_typed_and_does_not_commit_a_batch() {
        let application = MemoryApplication::new(owner(), FakeAuthority::default()).unwrap();
        *application.authority.promotion_conflict.lock().unwrap() =
            Some(Some(FactProposalPromotionStateV1::Applying));
        let promotion = PromoteFactProposal::new(
            id("proposal.memory.conflict"),
            owner(),
            FactProposalPromotionStateV1::PendingApproval,
            Some(id("actor.reviewer")),
            batch(owner(), "operation.proposal.conflict"),
        )
        .unwrap();

        let error = application
            .promote_fact_proposal(promotion)
            .await
            .unwrap_err();

        assert!(matches!(
            error,
            MemoryApplicationError::Authority(FactProposalStoreError::ProposalStateConflict {
                expected: FactProposalPromotionStateV1::PendingApproval,
                actual: Some(FactProposalPromotionStateV1::Applying),
                ..
            })
        ));
        assert!(application.authority.promotions.lock().unwrap().is_empty());
        assert!(application.authority.committed.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn typed_queries_propagate_without_identity_loss() {
        let application = MemoryApplication::new(owner(), FakeAuthority::default()).unwrap();
        let fact_id = fact_id(owner(), "operation.memory.query");
        let current = CurrentFactsQuery::new(owner(), None, 10).unwrap();
        let current_fact = FactCurrentQuery::new(owner(), fact_id.clone()).unwrap();
        let as_of = FactAsOfQuery::new(owner(), fact_id.clone(), UtcMicros(5)).unwrap();
        let lineage = FactLineageQuery::new(owner(), fact_id, None, 10).unwrap();
        let legacy = LegacyFactQuery::new(owner(), id::<SourceStoreId>("store.legacy"), 7).unwrap();
        let anchor_id = id::<RetrievalAnchorId>("anchor.memory.query");
        let anchor_query = RetrievalAnchorQuery::new(owner(), anchor_id.clone()).unwrap();

        assert!(
            application
                .query_current_facts(current)
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            application
                .query_fact_current(current_fact)
                .await
                .unwrap()
                .is_none()
        );
        assert!(application.query_fact_as_of(as_of).await.unwrap().is_none());
        assert!(
            application
                .query_fact_lineage(lineage)
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            application
                .resolve_legacy_fact(legacy)
                .await
                .unwrap()
                .is_none()
        );
        let anchor: Option<RetrievalAnchorRecordV2> = application
            .get_retrieval_anchor(anchor_query)
            .await
            .unwrap();
        assert!(anchor.is_none());

        assert_eq!(
            application.authority.current_queries.lock().unwrap().len(),
            1
        );
        assert_eq!(
            application
                .authority
                .current_fact_queries
                .lock()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(application.authority.as_of_queries.lock().unwrap().len(), 1);
        assert_eq!(
            application.authority.lineage_queries.lock().unwrap().len(),
            1
        );
        assert_eq!(
            application.authority.legacy_queries.lock().unwrap().len(),
            1
        );
        assert_eq!(
            application
                .authority
                .anchor_queries
                .lock()
                .unwrap()
                .as_slice(),
            &[anchor_id]
        );
    }

    #[tokio::test]
    async fn current_page_must_advance_cursor_and_stay_bounded() {
        let application = MemoryApplication::new(owner(), FakeAuthority::default()).unwrap();
        let first = stored_fact(owner(), "operation.current.first", UtcMicros(1));
        *application.authority.current_results.lock().unwrap() = vec![first.clone()];

        let error = application
            .query_current_facts(
                CurrentFactsQuery::new(owner(), Some(first.fact_id().clone()), 1).unwrap(),
            )
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            MemoryApplicationError::InvalidAuthorityResult { .. }
        ));

        let second = stored_fact(owner(), "operation.current.second", UtcMicros(2));
        let mut results = vec![first, second];
        results.sort_by(|left, right| left.fact_id().cmp(right.fact_id()));
        *application.authority.current_results.lock().unwrap() = results;

        let error = application
            .query_current_facts(CurrentFactsQuery::new(owner(), None, 1).unwrap())
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            MemoryApplicationError::InvalidAuthorityResult { .. }
        ));
    }

    #[tokio::test]
    async fn as_of_result_cannot_project_after_requested_time() {
        let application = MemoryApplication::new(owner(), FakeAuthority::default()).unwrap();
        let fact = stored_fact(owner(), "operation.as-of.future", UtcMicros(6));
        *application.authority.as_of_result.lock().unwrap() = Some(fact.clone());

        let error = application
            .query_fact_as_of(
                FactAsOfQuery::new(owner(), fact.fact_id().clone(), UtcMicros(5)).unwrap(),
            )
            .await
            .unwrap_err();

        assert!(matches!(
            error,
            MemoryApplicationError::InvalidAuthorityResult { .. }
        ));
    }

    #[tokio::test]
    async fn lineage_page_must_advance_cursor_and_stay_bounded() {
        let application = MemoryApplication::new(owner(), FakeAuthority::default()).unwrap();
        let fact_id = fact_id(owner(), "operation.lineage.cursor");
        let event = FactLineageEventV1::new(
            fact_id.clone(),
            owner(),
            FactLineageEventKindV1::PayloadAccessChanged {
                previous: PayloadAccessState::Eligible,
                current: PayloadAccessState::Deleted,
            },
            UtcMicros(1),
            None,
        )
        .unwrap();
        let cursor = FactLineageCursor::new(event.occurred_at(), event.event_id().clone()).unwrap();
        *application.authority.lineage_results.lock().unwrap() = vec![event];

        let error = application
            .query_fact_lineage(FactLineageQuery::new(owner(), fact_id, Some(cursor), 1).unwrap())
            .await
            .unwrap_err();

        assert!(matches!(
            error,
            MemoryApplicationError::InvalidAuthorityResult { .. }
        ));
    }

    #[tokio::test]
    async fn legacy_resolution_cannot_cross_owner_boundary() {
        let application = MemoryApplication::new(owner(), FakeAuthority::default()).unwrap();
        *application.authority.legacy_result.lock().unwrap() = Some(fact_id(
            FactOwnerV1::Profile,
            "operation.legacy.cross-owner",
        ));

        let error = application
            .resolve_legacy_fact(
                LegacyFactQuery::new(owner(), id::<SourceStoreId>("store.legacy"), 7).unwrap(),
            )
            .await
            .unwrap_err();

        assert!(matches!(
            error,
            MemoryApplicationError::InvalidAuthorityResult { .. }
        ));
    }

    #[tokio::test]
    async fn v1_feedback_defaults_an_omitted_source_to_mcp() {
        let application = MemoryApplication::new(owner(), FakeAuthority::default()).unwrap();
        let context = MemoryOperationContext::from_trusted_request_id(
            &owner(),
            "feedback",
            "fixture-feedback-mcp",
            None,
        )
        .unwrap();

        let _ = application
            .record_fact_feedback_v1(
                FeedbackRequest {
                    fact_id: 1,
                    action: FeedbackAction::Helpful,
                    source: None,
                    note: None,
                },
                context,
            )
            .await;

        let requests = application.authority.feedback_requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].source(), Some("mcp"));
    }

    #[tokio::test]
    async fn v1_trust_history_never_claims_incomplete_repair_is_complete() {
        let application = MemoryApplication::new(owner(), FakeAuthority::default()).unwrap();
        *application.authority.feedback_history.lock().unwrap() = Some(
            CompatibilityFactFeedbackHistoryV1::new_with_repair_progress(
                owner(),
                vec![],
                None,
                CompatibilityFeedbackRepairProgressV1::Incomplete {
                    processed: 1,
                    remaining: Some(2),
                },
            )
            .unwrap(),
        );

        let typed = application
            .fact_trust_history_with_progress_v1(1, 10)
            .await
            .unwrap();
        assert!(typed.entries.is_empty());
        assert!(matches!(
            typed.repair_progress,
            CompatibilityFeedbackRepairProgressV1::Incomplete {
                processed: 1,
                remaining: Some(2)
            }
        ));

        let error = application.fact_trust_history_v1(1, 10).await.unwrap_err();
        assert!(matches!(
            error,
            MemoryApplicationError::FeedbackHistoryUnavailable {
                progress: CompatibilityFeedbackRepairProgressV1::Incomplete { .. }
            }
        ));
        assert_eq!(
            application
                .authority
                .compatibility_calls
                .lock()
                .unwrap()
                .as_slice(),
            ["feedback-history", "feedback-history"]
        );
    }

    #[test]
    fn automation_run_identity_is_typed_not_fact_payload_metadata() {
        let mut request = legacy_add_request();
        request.metadata = serde_json::json!({
            "automation_run_id": "caller-controlled-run-id",
            "fixture": "retained",
        });

        let command = automation_fact_proposal_add_command(
            owner(),
            request,
            "run_01J4A7P5MQ1X9DX2P9BQNQW75T",
            "proposal-typed-run-id",
            None,
        )
        .unwrap();

        assert_eq!(
            command.automation_run_id(),
            Some("run_01J4A7P5MQ1X9DX2P9BQNQW75T")
        );
        assert_eq!(
            command.metadata().get("fixture"),
            Some(&serde_json::Value::String("retained".to_owned()))
        );
        assert!(command.metadata().get("automation_run_id").is_none());
    }

    #[test]
    fn operation_identity_digest_matches_canonical_framed_sha256() {
        let context = MemoryOperationContext::from_trusted_request_id(
            &FactOwnerV1::Profile,
            "feedback",
            "fixture-feedback-mcp",
            None,
        )
        .unwrap();

        assert_eq!(
            context.operation_id().as_str(),
            "memory-operation.v1.178353d02133a655ee53c04806709a086671ac1e7a364969759cb3be8b810a4b"
        );
    }

    #[test]
    fn stored_fact_fixture_remains_canonical() {
        let stored = stored_fact(owner(), "operation.memory.fixture", UtcMicros(2));
        let fact_id = stored.fact_id().clone();
        assert_eq!(stored.fact_id(), &fact_id);
    }
}
