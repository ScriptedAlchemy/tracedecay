//! Typed compatibility use cases and projection conversions.

use sha2::{Digest, Sha256};

use tracedecay_domain::{
    ActorId, Confidence, FactCategoryV1, FactId, FactLineageEventV1, FactOwnerV1, LocatorDigest,
    ProvenanceId, SourceStoreId,
};
use tracedecay_store::{
    CompatibilityFactAddCommandV1, CompatibilityFactAddOutcomeV1,
    CompatibilityFactContentDigestQueryV1, CompatibilityFactContradictionPageV1,
    CompatibilityFactContradictionQueryV1, CompatibilityFactFeedbackCommandV1,
    CompatibilityFactFeedbackHistoryQueryV1, CompatibilityFactFeedbackHistoryV1,
    CompatibilityFactFeedbackOutcomeV1, CompatibilityFactHistoryQueryV1,
    CompatibilityFactHistoryV1, CompatibilityFactInspectionV1, CompatibilityFactListQueryV1,
    CompatibilityFactPageV1, CompatibilityFactProjectionV1,
    CompatibilityFactProposalImportReceiptV1, CompatibilityFactProposalImportV1,
    CompatibilityFactProposalPageV1, CompatibilityFactProposalPromotionDispositionV1,
    CompatibilityFactProposalPromotionResultV1, CompatibilityFactProposalPromotionV1,
    CompatibilityFactProposalRecordV1, CompatibilityFactProposalRevisionV1,
    CompatibilityFactProposalStateV1, CompatibilityFactRelationV1,
    CompatibilityFactRemoveCommandV1, CompatibilityFactRemoveOutcomeV1,
    CompatibilityFactRetrievalCommandV1, CompatibilityFactSearchCursorV1,
    CompatibilityFactSearchPageV1, CompatibilityFactSearchQuery, CompatibilityFactTargetV1,
    CompatibilityFactUpdateCommandV1, CompatibilityFactUpdateOutcomeV1,
    CompatibilityMemoryStatusV1, FactCompatibilityStore,
};

use tracedecay_runtime_core::memory::hygiene::detect_secret_like;
use tracedecay_runtime_core::memory::trust::DEFAULT_TRUST;
use tracedecay_runtime_core::memory::types::{
    AddFactRequest, FactRecord, FactRelationKind, MemoryCategory, MemoryFeedbackFunnel,
    MemoryRepairStats, MemoryStatus,
};

use super::MemoryApplication;
use super::context::{MemoryOperationContext, validate_operation_component};
use super::error::{
    MemoryApplicationError, MemoryCompatibilityScope, RUNTIME_MEMORY_COMPATIBILITY_SOURCE_STORE,
};
use super::sanitize::sanitize_add_fact_request;

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

pub(super) fn compatibility_add_command(
    owner: FactOwnerV1,
    request: AddFactRequest,
    context: &MemoryOperationContext,
) -> Result<CompatibilityFactAddCommandV1, MemoryApplicationError> {
    let trust = Confidence::new(request.trust.unwrap_or(DEFAULT_TRUST)).map_err(|_| {
        MemoryApplicationError::InvalidCompatibilityInput {
            invariant: "trust must be between 0.0 and 1.0",
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

pub(super) const fn fact_category(category: MemoryCategory) -> FactCategoryV1 {
    match category {
        MemoryCategory::General => FactCategoryV1::General,
        MemoryCategory::UserPref => FactCategoryV1::UserPref,
        MemoryCategory::Project => FactCategoryV1::Project,
        MemoryCategory::Tool => FactCategoryV1::Tool,
        MemoryCategory::Decision => FactCategoryV1::Decision,
        MemoryCategory::CodeArea => FactCategoryV1::CodeArea,
    }
}

pub(super) const fn compatibility_relation(
    relation: FactRelationKind,
) -> CompatibilityFactRelationV1 {
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

pub(super) fn compatibility_confidence(
    value: Option<f64>,
) -> Result<Option<Confidence>, MemoryApplicationError> {
    value.map(Confidence::new).transpose().map_err(|_| {
        MemoryApplicationError::InvalidCompatibilityInput {
            invariant: "confidence (trust/min_trust) must be between 0.0 and 1.0",
        }
    })
}

pub(super) fn legacy_i64(
    value: u64,
    invariant: &'static str,
) -> Result<i64, MemoryApplicationError> {
    i64::try_from(value)
        .map_err(|_| MemoryApplicationError::IncompatibleLegacyProjection { invariant })
}

pub(super) fn legacy_usize(
    value: u64,
    invariant: &'static str,
) -> Result<usize, MemoryApplicationError> {
    usize::try_from(value)
        .map_err(|_| MemoryApplicationError::IncompatibleLegacyProjection { invariant })
}

/// Projects one authoritative compatibility snapshot into the legacy status
/// shape. Keep this pure so callers cannot accidentally split status and
/// feedback-history repair across separate reads.
pub(super) fn project_memory_status_v1(
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

pub(super) fn compatibility_fact_record(
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

pub(super) fn compatibility_projection_record(
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
        // A `None` fact is the idempotent no-op disposition for a target that
        // never resolved within the authority's single remove transaction;
        // there is no projection to validate in that case.
        if let Some(fact) = outcome.fact() {
            validate_compatibility_projection(&self.owner, &target, fact)?;
        }
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
}

pub(super) fn compatibility_projection_targets(
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

fn validate_compatibility_page(
    owner: &FactOwnerV1,
    after_fact_id: Option<&FactId>,
    limit: usize,
    page: &CompatibilityFactPageV1,
) -> Result<(), MemoryApplicationError> {
    let facts = page.facts();
    // Resume is exclusive-start, so the canonical cursor for a full page is
    // exactly its last fact id — mirroring the search-page cursor convention
    // below, and matching what the authority's list producer emits.
    let cursor_is_invalid = page.next_after_fact_id().is_some_and(|cursor| {
        cursor.validate_owner(owner).is_err()
            || after_fact_id.is_some_and(|after| cursor <= after)
            || facts.last().is_none_or(|last| cursor != last.fact_id())
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

pub(super) fn validate_lineage(
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
        CompatibilityFactTargetV1::Canonical(_) => Ok(()),
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
