//! Typed project-memory use cases and persisted fact-id projections.

use sha2::{Digest, Sha256};

use tracedecay_domain::{
    ActorId, Confidence, FactCategoryV1, FactId, FactLineageEventV1, FactOwnerV1, LocatorDigest,
    ProvenanceId,
};
use tracedecay_store::{
    ProjectMemoryAutomaticFactApplyDispositionV1, ProjectMemoryAutomaticFactApplyResultV1,
    ProjectMemoryAutomaticFactEvidenceV1, ProjectMemoryAutomaticFactReceiptPageV1,
    ProjectMemoryAutomaticFactReceiptV1, ProjectMemoryAutomaticFactStateV1,
    ProjectMemoryFactAddCommandV1, ProjectMemoryFactAddOutcomeV1,
    ProjectMemoryFactContentDigestQueryV1, ProjectMemoryFactContradictionPageV1,
    ProjectMemoryFactContradictionQueryV1, ProjectMemoryFactFeedbackCommandV1,
    ProjectMemoryFactFeedbackHistoryQueryV1, ProjectMemoryFactFeedbackHistoryV1,
    ProjectMemoryFactFeedbackOutcomeV1, ProjectMemoryFactHistoryQueryV1,
    ProjectMemoryFactHistoryV1, ProjectMemoryFactInspectionV1, ProjectMemoryFactListQueryV1,
    ProjectMemoryFactPageV1, ProjectMemoryFactProjectionV1, ProjectMemoryFactRelationV1,
    ProjectMemoryFactRemoveCommandV1, ProjectMemoryFactRemoveOutcomeV1,
    ProjectMemoryFactRetrievalCommandV1, ProjectMemoryFactSearchCursorV1,
    ProjectMemoryFactSearchPageV1, ProjectMemoryFactSearchQuery, ProjectMemoryFactStore,
    ProjectMemoryFactTargetV1, ProjectMemoryFactUpdateCommandV1, ProjectMemoryFactUpdateOutcomeV1,
    ProjectMemoryMemoryStatusV1,
};

use tracedecay_runtime_core::memory::hygiene::detect_secret_like;
use tracedecay_runtime_core::memory::trust::DEFAULT_TRUST;
use tracedecay_runtime_core::memory::types::{
    AddFactRequest, FactRecord, FactRelationKind, MemoryCategory, MemoryFeedbackFunnel,
    MemoryRepairStats, MemoryStatus,
};

use super::MemoryApplication;
use super::context::{MemoryOperationContext, validate_operation_component};
use super::error::{MemoryApplicationError, PersistedFactIdScope};
use super::sanitize::{SanitizedAddFactRequest, sanitize_add_fact_request};

/// Converts an automation item without manufacturing a legacy numeric
/// identity. The deterministic operation identity makes repeated processing of
/// the same run/apply identity idempotent at the authority boundary.
pub fn automatic_fact_add_command(
    owner: FactOwnerV1,
    request: AddFactRequest,
    run_id: &str,
    apply_id: &str,
    actor: Option<ActorId>,
) -> Result<ProjectMemoryFactAddCommandV1, MemoryApplicationError> {
    owner.validate()?;
    validate_operation_component(run_id, "automatic fact run identity")?;
    validate_operation_component(apply_id, "automatic fact apply identity")?;
    let context = MemoryOperationContext::from_request_id(
        &owner,
        "automatic-fact",
        &format!("{run_id}:{apply_id}"),
        actor,
    )?;
    let Some(request) = sanitize_add_fact_request(request)? else {
        return Err(MemoryApplicationError::InvalidInput {
            invariant: "automatic fact declined by memory privacy sanitizer",
        });
    };
    with_automation_run_id(fact_add_command(owner, request, &context)?, run_id)
}

/// Binds the trusted run identity to command metadata after the payload has
/// been sanitized. It is never serialized into fact payload metadata.
pub fn with_automation_run_id(
    command: ProjectMemoryFactAddCommandV1,
    run_id: &str,
) -> Result<ProjectMemoryFactAddCommandV1, MemoryApplicationError> {
    validate_operation_component(run_id, "automatic fact run identity")?;
    command
        .with_automation_run_id(run_id.to_owned())
        .map_err(MemoryApplicationError::Store)
}

pub(super) fn fact_add_command(
    owner: FactOwnerV1,
    request: SanitizedAddFactRequest,
    context: &MemoryOperationContext,
) -> Result<ProjectMemoryFactAddCommandV1, MemoryApplicationError> {
    let (request, sanitization_receipt) = request.into_parts();
    let trust = Confidence::new(request.trust.unwrap_or(DEFAULT_TRUST)).map_err(|_| {
        MemoryApplicationError::InvalidInput {
            invariant: "trust must be between 0.0 and 1.0",
        }
    })?;
    ProjectMemoryFactAddCommandV1::new(
        owner,
        context.operation_id().clone(),
        request.content,
        fact_category(request.category),
        request.source,
        request.tags,
        request.entities,
        request.metadata,
        sanitization_receipt,
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

pub(super) const fn memory_relation(relation: FactRelationKind) -> ProjectMemoryFactRelationV1 {
    match relation {
        FactRelationKind::Supports => ProjectMemoryFactRelationV1::Supports,
        FactRelationKind::Contradicts => ProjectMemoryFactRelationV1::Contradicts,
        FactRelationKind::Supersedes => ProjectMemoryFactRelationV1::Supersedes,
        FactRelationKind::DerivedFrom => ProjectMemoryFactRelationV1::DerivedFrom,
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

pub(super) fn memory_confidence(
    value: Option<f64>,
) -> Result<Option<Confidence>, MemoryApplicationError> {
    value
        .map(Confidence::new)
        .transpose()
        .map_err(|_| MemoryApplicationError::InvalidInput {
            invariant: "confidence (trust/min_trust) must be between 0.0 and 1.0",
        })
}

pub(super) fn legacy_i64(
    value: u64,
    invariant: &'static str,
) -> Result<i64, MemoryApplicationError> {
    i64::try_from(value)
        .map_err(|_| MemoryApplicationError::UnrepresentablePersistedFact { invariant })
}

pub(super) fn legacy_usize(
    value: u64,
    invariant: &'static str,
) -> Result<usize, MemoryApplicationError> {
    usize::try_from(value)
        .map_err(|_| MemoryApplicationError::UnrepresentablePersistedFact { invariant })
}

/// Projects one authoritative snapshot into the persisted numeric status
/// shape. Keep this pure so callers cannot accidentally split status and
/// feedback-history repair across separate reads.
pub(super) fn project_memory_status(
    status: &ProjectMemoryMemoryStatusV1,
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

pub(super) fn project_memory_fact_record(
    scope: &PersistedFactIdScope,
    fact: &tracedecay_store::ProjectMemoryFactV1,
) -> Result<FactRecord, MemoryApplicationError> {
    if fact.owner() != scope.owner() {
        return Err(MemoryApplicationError::InvalidAuthorityResult {
            invariant: "legacy fact projection owner",
        });
    }
    let mapping = fact.mapping().legacy_mapping().ok_or(
        MemoryApplicationError::UnrepresentablePersistedFact {
            invariant: "legacy numeric fact mapping",
        },
    )?;
    if mapping.owner() != scope.owner() || mapping.source_store_id() != scope.source_store_id() {
        return Err(MemoryApplicationError::UnrepresentablePersistedFact {
            invariant: "legacy fact mapping source",
        });
    }
    let payload = fact
        .payload()
        .ok_or(MemoryApplicationError::UnrepresentablePersistedFact {
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

pub(super) fn project_memory_projection_record(
    scope: &PersistedFactIdScope,
    projection: &ProjectMemoryFactProjectionV1,
) -> Result<FactRecord, MemoryApplicationError> {
    match projection {
        ProjectMemoryFactProjectionV1::Available(fact) => project_memory_fact_record(scope, fact),
        ProjectMemoryFactProjectionV1::Unavailable(_) => {
            Err(MemoryApplicationError::UnrepresentablePersistedFact {
                invariant: "available legacy fact projection",
            })
        }
    }
}

/// Typed project-memory use cases. Transport adapters translate persisted
/// numeric inputs before this boundary; only the authority owns the
/// corresponding mutation transaction and projection.
impl<A: ProjectMemoryFactStore> MemoryApplication<A> {
    pub async fn list_project_memory_facts(
        &self,
        query: ProjectMemoryFactListQueryV1,
    ) -> Result<ProjectMemoryFactPageV1, MemoryApplicationError> {
        self.ensure_owner(query.owner())?;
        let after_fact_id = query.after_fact_id().cloned();
        let limit = query.limit();
        let page = self.authority.list_project_memory_facts(query).await?;
        validate_project_memory_page(&self.owner, after_fact_id.as_ref(), limit, &page)?;
        Ok(page)
    }

    pub async fn search_project_memory_facts(
        &self,
        query: ProjectMemoryFactSearchQuery,
    ) -> Result<ProjectMemoryFactSearchPageV1, MemoryApplicationError> {
        self.ensure_owner(query.owner())?;
        let after = query.after().cloned();
        let limit = query.limit();
        let page = self.authority.search_project_memory_facts(query).await?;
        validate_project_memory_search_page(&self.owner, after.as_ref(), limit, &page)?;
        Ok(page)
    }

    pub async fn probe_project_memory_facts(
        &self,
        query: ProjectMemoryFactSearchQuery,
    ) -> Result<ProjectMemoryFactSearchPageV1, MemoryApplicationError> {
        self.ensure_owner(query.owner())?;
        let after = query.after().cloned();
        let limit = query.limit();
        let page = self.authority.probe_project_memory_facts(query).await?;
        validate_project_memory_search_page(&self.owner, after.as_ref(), limit, &page)?;
        Ok(page)
    }

    pub async fn related_project_memory_facts(
        &self,
        query: ProjectMemoryFactSearchQuery,
    ) -> Result<ProjectMemoryFactSearchPageV1, MemoryApplicationError> {
        self.ensure_owner(query.owner())?;
        let after = query.after().cloned();
        let limit = query.limit();
        let page = self.authority.related_project_memory_facts(query).await?;
        validate_project_memory_search_page(&self.owner, after.as_ref(), limit, &page)?;
        Ok(page)
    }

    pub async fn reason_project_memory_facts(
        &self,
        query: ProjectMemoryFactSearchQuery,
    ) -> Result<ProjectMemoryFactSearchPageV1, MemoryApplicationError> {
        self.ensure_owner(query.owner())?;
        let after = query.after().cloned();
        let limit = query.limit();
        let page = self.authority.reason_project_memory_facts(query).await?;
        validate_project_memory_search_page(&self.owner, after.as_ref(), limit, &page)?;
        Ok(page)
    }

    pub async fn find_project_memory_contradictions(
        &self,
        query: ProjectMemoryFactContradictionQueryV1,
    ) -> Result<ProjectMemoryFactContradictionPageV1, MemoryApplicationError> {
        self.ensure_owner(query.owner())?;
        let limit = query.limit();
        let page = self
            .authority
            .find_project_memory_contradictions(query)
            .await?;
        if page.owner() != &self.owner
            || page.contradictions().len() > limit
            || page
                .contradictions()
                .iter()
                .any(|contradiction| contradiction.existing().owner() != &self.owner)
        {
            return Err(MemoryApplicationError::InvalidAuthorityResult {
                invariant: "project-memory contradiction bounds and owner",
            });
        }
        Ok(page)
    }

    pub async fn get_project_memory_fact(
        &self,
        target: ProjectMemoryFactTargetV1,
    ) -> Result<Option<ProjectMemoryFactProjectionV1>, MemoryApplicationError> {
        self.ensure_owner(target.owner())?;
        let result = self
            .authority
            .get_project_memory_fact(target.clone())
            .await?;
        if let Some(projection) = &result {
            validate_project_memory_projection(&self.owner, &target, projection)?;
        }
        Ok(result)
    }

    /// Owner-bound exact-content lookup for automation deduplication. The raw
    /// content is never forwarded to the authority: only its canonical SHA-256
    /// locator digest crosses this boundary. Legacy mappings remain part of an
    /// available projection, so callers can preserve the historical numeric id.
    pub async fn find_exact_fact_by_content(
        &self,
        content: &str,
    ) -> Result<Option<ProjectMemoryFactProjectionV1>, MemoryApplicationError> {
        if content.trim().is_empty() || detect_secret_like(content.trim()).is_some() {
            return Ok(None);
        }
        let digest = LocatorDigest::new(format!(
            "sha256:{}",
            hex::encode(Sha256::digest(content.as_bytes()))
        ))
        .map_err(|_| MemoryApplicationError::InvalidInput {
            invariant: "exact fact content digest",
        })?;
        let result =
            self.authority
                .find_project_memory_fact_by_content_digest(
                    ProjectMemoryFactContentDigestQueryV1::new(self.owner.clone(), digest)?,
                )
                .await?;
        if let Some(projection) = &result
            && projection.owner() != &self.owner
        {
            return Err(MemoryApplicationError::InvalidAuthorityResult {
                invariant: "exact project-memory fact owner",
            });
        }
        Ok(result)
    }

    pub async fn get_project_memory_history(
        &self,
        query: ProjectMemoryFactHistoryQueryV1,
    ) -> Result<ProjectMemoryFactHistoryV1, MemoryApplicationError> {
        self.ensure_owner(query.target().owner())?;
        let target = query.target().clone();
        let after = query.after().cloned();
        let limit = query.limit();
        let history = self.authority.project_memory_fact_history(query).await?;
        if history.owner() != &self.owner {
            return Err(MemoryApplicationError::InvalidAuthorityResult {
                invariant: "project-memory history owner",
            });
        }
        if let Some(fact_id) = target.canonical_fact_id()
            && history.fact_id() != fact_id
        {
            return Err(MemoryApplicationError::InvalidAuthorityResult {
                invariant: "project-memory history canonical identity",
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
    pub async fn get_project_memory_feedback_history(
        &self,
        query: ProjectMemoryFactFeedbackHistoryQueryV1,
    ) -> Result<ProjectMemoryFactFeedbackHistoryV1, MemoryApplicationError> {
        self.ensure_owner(query.target().owner())?;
        let limit = query.limit();
        let history = self
            .authority
            .project_memory_fact_feedback_history(query)
            .await?;
        if history.owner() != &self.owner || history.events().len() > limit {
            return Err(MemoryApplicationError::InvalidAuthorityResult {
                invariant: "project-memory feedback history owner and bounds",
            });
        }
        Ok(history)
    }

    /// Pure status snapshot. It reports, but never advances, feedback repair.
    pub async fn project_memory_status(
        &self,
    ) -> Result<ProjectMemoryMemoryStatusV1, MemoryApplicationError> {
        let status = self
            .authority
            .project_memory_status(self.owner.clone())
            .await?;
        if status.owner() != &self.owner {
            return Err(MemoryApplicationError::InvalidAuthorityResult {
                invariant: "project-memory status owner",
            });
        }
        Ok(status)
    }

    pub async fn inspect_project_memory_fact(
        &self,
        target: ProjectMemoryFactTargetV1,
    ) -> Result<Option<ProjectMemoryFactInspectionV1>, MemoryApplicationError> {
        self.ensure_owner(target.owner())?;
        let inspection = self
            .authority
            .inspect_project_memory_fact(target.clone())
            .await?;
        if let Some(inspection) = &inspection {
            validate_project_memory_inspection(&self.owner, &target, inspection)?;
        }
        Ok(inspection)
    }

    pub async fn add_project_memory_fact(
        &self,
        request: ProjectMemoryFactAddCommandV1,
    ) -> Result<ProjectMemoryFactAddOutcomeV1, MemoryApplicationError> {
        self.ensure_owner(request.owner())?;
        let outcome = self.authority.add_project_memory_fact(request).await?;
        validate_project_memory_add_outcome(&self.owner, &outcome)?;
        Ok(outcome)
    }

    pub async fn update_project_memory_fact(
        &self,
        request: ProjectMemoryFactUpdateCommandV1,
    ) -> Result<ProjectMemoryFactUpdateOutcomeV1, MemoryApplicationError> {
        self.ensure_owner(request.target().owner())?;
        let target = request.target().clone();
        let outcome = self.authority.update_project_memory_fact(request).await?;
        validate_project_memory_projection(&self.owner, &target, outcome.fact())?;
        Ok(outcome)
    }

    pub async fn remove_project_memory_fact(
        &self,
        request: ProjectMemoryFactRemoveCommandV1,
    ) -> Result<ProjectMemoryFactRemoveOutcomeV1, MemoryApplicationError> {
        self.ensure_owner(request.target().owner())?;
        let target = request.target().clone();
        let outcome = self.authority.remove_project_memory_fact(request).await?;
        // A `None` fact is the idempotent no-op disposition for a target that
        // never resolved within the authority's single remove transaction;
        // there is no projection to validate in that case.
        if let Some(fact) = outcome.fact() {
            validate_project_memory_projection(&self.owner, &target, fact)?;
        }
        Ok(outcome)
    }

    pub async fn record_project_memory_fact_feedback(
        &self,
        request: ProjectMemoryFactFeedbackCommandV1,
    ) -> Result<ProjectMemoryFactFeedbackOutcomeV1, MemoryApplicationError> {
        self.ensure_owner(request.target().owner())?;
        let target = request.target().clone();
        let outcome = self
            .authority
            .record_project_memory_fact_feedback(request)
            .await?;
        validate_project_memory_projection(&self.owner, &target, outcome.fact())?;
        Ok(outcome)
    }

    pub async fn record_project_memory_fact_retrieval(
        &self,
        request: ProjectMemoryFactRetrievalCommandV1,
    ) -> Result<Vec<ProjectMemoryFactProjectionV1>, MemoryApplicationError> {
        self.ensure_owner(request.owner())?;
        let targets = request.targets().to_vec();
        let projections = self
            .authority
            .record_project_memory_fact_retrieval(request)
            .await?;
        if projections
            .iter()
            .any(|projection| projection.owner() != &self.owner)
        {
            return Err(MemoryApplicationError::InvalidAuthorityResult {
                invariant: "project-memory retrieval projection owner",
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
                invariant: "project-memory retrieval canonical target",
            });
        }
        Ok(projections)
    }

    pub async fn apply_project_memory_automatic_fact(
        &self,
        apply_id: ProvenanceId,
        request: ProjectMemoryFactAddCommandV1,
        evidence: ProjectMemoryAutomaticFactEvidenceV1,
    ) -> Result<ProjectMemoryAutomaticFactApplyResultV1, MemoryApplicationError> {
        self.ensure_owner(request.owner())?;
        let result = self
            .authority
            .apply_project_memory_automatic_fact(apply_id.clone(), request, evidence)
            .await?;
        validate_project_memory_automatic_fact_receipt(&self.owner, &apply_id, result.receipt())?;
        let valid_disposition = matches!(
            (result.receipt().state(), result.disposition()),
            (
                ProjectMemoryAutomaticFactStateV1::Applied,
                ProjectMemoryAutomaticFactApplyDispositionV1::Applied
                    | ProjectMemoryAutomaticFactApplyDispositionV1::AlreadyApplied,
            ) | (
                ProjectMemoryAutomaticFactStateV1::Quarantined,
                ProjectMemoryAutomaticFactApplyDispositionV1::Quarantined,
            )
        );
        if !valid_disposition {
            return Err(MemoryApplicationError::InvalidAuthorityResult {
                invariant: "automatic fact receipt disposition",
            });
        }
        Ok(result)
    }

    pub async fn get_project_memory_automatic_fact_receipt(
        &self,
        apply_id: ProvenanceId,
    ) -> Result<Option<ProjectMemoryAutomaticFactReceiptV1>, MemoryApplicationError> {
        let receipt = self
            .authority
            .get_project_memory_automatic_fact_receipt(self.owner.clone(), apply_id.clone())
            .await?;
        if let Some(receipt) = &receipt {
            validate_project_memory_automatic_fact_receipt(&self.owner, &apply_id, receipt)?;
        }
        Ok(receipt)
    }

    pub async fn list_project_memory_automatic_fact_receipts(
        &self,
        state: Option<ProjectMemoryAutomaticFactStateV1>,
        after_apply_id: Option<ProvenanceId>,
        limit: usize,
    ) -> Result<ProjectMemoryAutomaticFactReceiptPageV1, MemoryApplicationError> {
        let page = self
            .authority
            .list_project_memory_automatic_fact_receipts(
                self.owner.clone(),
                state,
                after_apply_id.clone(),
                limit,
            )
            .await?;
        validate_project_memory_automatic_fact_receipt_page(
            &self.owner,
            after_apply_id.as_ref(),
            limit,
            &page,
        )?;
        Ok(page)
    }
}

pub(super) fn projection_targets(
    projections: &[ProjectMemoryFactProjectionV1],
) -> Vec<ProjectMemoryFactTargetV1> {
    projections
        .iter()
        .filter_map(|projection| match projection {
            ProjectMemoryFactProjectionV1::Available(fact) => Some(
                ProjectMemoryFactTargetV1::Canonical(fact.mapping().compatibility_id().clone()),
            ),
            ProjectMemoryFactProjectionV1::Unavailable(_) => None,
        })
        .collect()
}

fn validate_project_memory_page(
    owner: &FactOwnerV1,
    after_fact_id: Option<&FactId>,
    limit: usize,
    page: &ProjectMemoryFactPageV1,
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
            invariant: "project-memory list bounds, owner, cursor, and ordering",
        });
    }
    Ok(())
}

fn validate_project_memory_search_page(
    owner: &FactOwnerV1,
    after: Option<&ProjectMemoryFactSearchCursorV1>,
    limit: usize,
    page: &ProjectMemoryFactSearchPageV1,
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
            invariant: "project-memory search bounds, owner, cursor, and ordering",
        });
    }
    Ok(())
}

fn search_hit_follows_cursor(
    hit: &tracedecay_store::ProjectMemoryFactSearchHitV1,
    after: &ProjectMemoryFactSearchCursorV1,
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

fn validate_project_memory_projection(
    owner: &FactOwnerV1,
    target: &ProjectMemoryFactTargetV1,
    projection: &ProjectMemoryFactProjectionV1,
) -> Result<(), MemoryApplicationError> {
    if projection.owner() != owner {
        return Err(MemoryApplicationError::InvalidAuthorityResult {
            invariant: "project-memory projection owner",
        });
    }
    if let Some(fact_id) = target.canonical_fact_id() {
        if projection.fact_id() != fact_id {
            return Err(MemoryApplicationError::InvalidAuthorityResult {
                invariant: "project-memory projection canonical identity",
            });
        }
    } else if let (Some(query), ProjectMemoryFactProjectionV1::Available(fact)) =
        (target.legacy_query(), projection)
    {
        let mapping = fact.mapping().legacy_mapping();
        if mapping.is_none_or(|mapping| {
            mapping.owner() != owner
                || mapping.source_store_id() != query.source_store_id()
                || mapping.legacy_fact_id() != query.legacy_fact_id()
        }) {
            return Err(MemoryApplicationError::InvalidAuthorityResult {
                invariant: "persisted fact-id projection mapping",
            });
        }
    }
    Ok(())
}

fn validate_project_memory_inspection(
    owner: &FactOwnerV1,
    target: &ProjectMemoryFactTargetV1,
    inspection: &ProjectMemoryFactInspectionV1,
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
            invariant: "project-memory inspection owner and identity",
        });
    }
    match target {
        ProjectMemoryFactTargetV1::Canonical(target)
            if inspection.fact().fact_id() != target.fact_id() =>
        {
            Err(MemoryApplicationError::InvalidAuthorityResult {
                invariant: "project-memory inspection canonical identity",
            })
        }
        ProjectMemoryFactTargetV1::Legacy(query) => {
            let mapping = inspection.fact().mapping().legacy_mapping();
            if mapping.is_none_or(|mapping| {
                mapping.owner() != owner
                    || mapping.source_store_id() != query.source_store_id()
                    || mapping.legacy_fact_id() != query.legacy_fact_id()
            }) {
                return Err(MemoryApplicationError::InvalidAuthorityResult {
                    invariant: "persisted fact-id inspection mapping",
                });
            }
            Ok(())
        }
        ProjectMemoryFactTargetV1::Canonical(_) => Ok(()),
    }
}

fn validate_project_memory_add_outcome(
    owner: &FactOwnerV1,
    outcome: &ProjectMemoryFactAddOutcomeV1,
) -> Result<(), MemoryApplicationError> {
    if outcome
        .fact()
        .is_some_and(|projection| projection.owner() != owner)
        || outcome
            .closest_fact_id()
            .is_some_and(|fact_id| fact_id.owner() != owner)
    {
        return Err(MemoryApplicationError::InvalidAuthorityResult {
            invariant: "project-memory add outcome owner",
        });
    }
    Ok(())
}

fn validate_project_memory_automatic_fact_receipt(
    owner: &FactOwnerV1,
    apply_id: &ProvenanceId,
    receipt: &ProjectMemoryAutomaticFactReceiptV1,
) -> Result<(), MemoryApplicationError> {
    if receipt.owner() != owner
        || receipt.apply_id() != apply_id
        || receipt.request().owner() != owner
    {
        return Err(MemoryApplicationError::InvalidAuthorityResult {
            invariant: "automatic fact receipt owner and identity",
        });
    }
    Ok(())
}

fn validate_project_memory_automatic_fact_receipt_page(
    owner: &FactOwnerV1,
    after_apply_id: Option<&ProvenanceId>,
    limit: usize,
    page: &ProjectMemoryAutomaticFactReceiptPageV1,
) -> Result<(), MemoryApplicationError> {
    let receipts = page.receipts();
    let cursor_is_invalid = page.next_after_apply_id().is_some_and(|cursor| {
        cursor.validate().is_err()
            || after_apply_id.is_some_and(|after| cursor <= after)
            || receipts
                .last()
                .is_none_or(|receipt| cursor <= receipt.apply_id())
    });
    if page.owner() != owner
        || receipts.len() > limit
        || receipts.iter().any(|receipt| receipt.owner() != owner)
        || after_apply_id
            .is_some_and(|after| receipts.iter().any(|receipt| receipt.apply_id() <= after))
        || receipts
            .windows(2)
            .any(|pair| pair[0].apply_id() >= pair[1].apply_id())
        || cursor_is_invalid
    {
        return Err(MemoryApplicationError::InvalidAuthorityResult {
            invariant: "automatic fact receipt page bounds, owner, cursor, and ordering",
        });
    }
    Ok(())
}
