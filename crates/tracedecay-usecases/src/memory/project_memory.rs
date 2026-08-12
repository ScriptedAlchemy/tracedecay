//! Typed use cases over the canonical project-memory authority.

use sha2::{Digest, Sha256};

use tracedecay_domain::RunId;
use tracedecay_domain::{FactId, FactLineageEventV1, FactOwnerV1, LocatorDigest, ProvenanceId};
use tracedecay_runtime_core::memory::hygiene::detect_secret_like;
use tracedecay_store::ProjectMemoryAutomationRunReceiptsV1;
use tracedecay_store::{
    FactReadControl, FactWriteControl, ProjectMemoryAutomaticFactApplyDispositionV1,
    ProjectMemoryAutomaticFactApplyResultV1, ProjectMemoryAutomaticFactEvidenceV1,
    ProjectMemoryAutomaticFactReceiptPageV1, ProjectMemoryAutomaticFactReceiptV1,
    ProjectMemoryAutomaticFactStateV1, ProjectMemoryFactAddCommandV1,
    ProjectMemoryFactAddDispositionV1, ProjectMemoryFactAddOutcomeV1,
    ProjectMemoryFactContentDigestQueryV1, ProjectMemoryFactContradictionPageV1,
    ProjectMemoryFactContradictionQueryV1, ProjectMemoryFactFeedbackCommandV1,
    ProjectMemoryFactFeedbackHistoryQueryV1, ProjectMemoryFactFeedbackHistoryV1,
    ProjectMemoryFactFeedbackOutcomeV1, ProjectMemoryFactHistoryQueryV1,
    ProjectMemoryFactHistoryV1, ProjectMemoryFactIdV1, ProjectMemoryFactInspectionV1,
    ProjectMemoryFactListQueryV1, ProjectMemoryFactPageV1, ProjectMemoryFactProjectionV1,
    ProjectMemoryFactRemoveCommandV1, ProjectMemoryFactRemoveOutcomeV1,
    ProjectMemoryFactRetrievalCommandV1, ProjectMemoryFactRetrievalOutcomeV1,
    ProjectMemoryFactSearchCursorV1, ProjectMemoryFactSearchPageV1, ProjectMemoryFactSearchQuery,
    ProjectMemoryFactStore, ProjectMemoryFactUpdateCommandV1, ProjectMemoryFactUpdateOutcomeV1,
    ProjectMemoryMemoryStatusV1,
};

use super::MemoryApplication;
use super::error::{MemoryApplicationError, MemoryMutationError, settle_authority_result};

mod add;
pub use add::{
    ProjectMemoryFactAddEffectMaterialV1, ProjectMemoryFactAddPreflight,
    ProjectMemoryFactAddRequest, ProjectMemoryFactAddRequestOutcome, automatic_fact_add_command,
};

/// Typed project-memory use cases. Only the authority owns each mutation
/// transaction and its durable projection.
impl<A: ProjectMemoryFactStore> MemoryApplication<A> {
    pub async fn list_project_memory_facts(
        &self,
        query: ProjectMemoryFactListQueryV1,
        read_control: &FactReadControl,
    ) -> Result<ProjectMemoryFactPageV1, MemoryApplicationError> {
        self.ensure_owner(query.owner())?;
        let after_fact_id = query.after_fact_id().cloned();
        let limit = query.limit();
        let page = self
            .authority
            .list_project_memory_facts(query, read_control)
            .await?;
        validate_project_memory_page(&self.owner, after_fact_id.as_ref(), limit, &page)?;
        Ok(page)
    }

    pub async fn search_project_memory_facts(
        &self,
        query: ProjectMemoryFactSearchQuery,
        read_control: &FactReadControl,
    ) -> Result<ProjectMemoryFactSearchPageV1, MemoryApplicationError> {
        self.ensure_owner(query.owner())?;
        let after = query.after().cloned();
        let limit = query.limit();
        let page = self
            .authority
            .search_project_memory_facts(query, read_control)
            .await?;
        validate_project_memory_search_page(&self.owner, after.as_ref(), limit, &page)?;
        Ok(page)
    }

    pub async fn probe_project_memory_facts(
        &self,
        query: ProjectMemoryFactSearchQuery,
        read_control: &FactReadControl,
    ) -> Result<ProjectMemoryFactSearchPageV1, MemoryApplicationError> {
        self.ensure_owner(query.owner())?;
        let after = query.after().cloned();
        let limit = query.limit();
        let page = self
            .authority
            .probe_project_memory_facts(query, read_control)
            .await?;
        validate_project_memory_search_page(&self.owner, after.as_ref(), limit, &page)?;
        Ok(page)
    }

    pub async fn related_project_memory_facts(
        &self,
        query: ProjectMemoryFactSearchQuery,
        read_control: &FactReadControl,
    ) -> Result<ProjectMemoryFactSearchPageV1, MemoryApplicationError> {
        self.ensure_owner(query.owner())?;
        let after = query.after().cloned();
        let limit = query.limit();
        let page = self
            .authority
            .related_project_memory_facts(query, read_control)
            .await?;
        validate_project_memory_search_page(&self.owner, after.as_ref(), limit, &page)?;
        Ok(page)
    }

    pub async fn reason_project_memory_facts(
        &self,
        query: ProjectMemoryFactSearchQuery,
        read_control: &FactReadControl,
    ) -> Result<ProjectMemoryFactSearchPageV1, MemoryApplicationError> {
        self.ensure_owner(query.owner())?;
        let after = query.after().cloned();
        let limit = query.limit();
        let page = self
            .authority
            .reason_project_memory_facts(query, read_control)
            .await?;
        validate_project_memory_search_page(&self.owner, after.as_ref(), limit, &page)?;
        Ok(page)
    }

    pub async fn find_project_memory_contradictions(
        &self,
        query: ProjectMemoryFactContradictionQueryV1,
        read_control: &FactReadControl,
    ) -> Result<ProjectMemoryFactContradictionPageV1, MemoryApplicationError> {
        self.ensure_owner(query.owner())?;
        let limit = query.limit();
        let page = self
            .authority
            .find_project_memory_contradictions(query, read_control)
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
        target: ProjectMemoryFactIdV1,
        read_control: &FactReadControl,
    ) -> Result<Option<ProjectMemoryFactProjectionV1>, MemoryApplicationError> {
        self.ensure_owner(target.owner())?;
        let result = self
            .authority
            .get_project_memory_fact(target.clone(), read_control)
            .await?;
        if let Some(projection) = &result {
            validate_project_memory_projection(&self.owner, &target, projection)?;
        }
        Ok(result)
    }

    /// Owner-bound exact-content lookup for automation deduplication. The raw
    /// content is never forwarded to the authority: only its canonical SHA-256
    /// locator digest crosses this boundary.
    pub async fn find_exact_fact_by_content(
        &self,
        content: &str,
        read_control: &FactReadControl,
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
        let result = self
            .authority
            .find_project_memory_fact_by_content_digest(
                ProjectMemoryFactContentDigestQueryV1::new(self.owner.clone(), digest)?,
                read_control,
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
        read_control: &FactReadControl,
    ) -> Result<ProjectMemoryFactHistoryV1, MemoryApplicationError> {
        self.ensure_owner(query.target().owner())?;
        let target = query.target().clone();
        let after = query.after().cloned();
        let limit = query.limit();
        let history = self
            .authority
            .project_memory_fact_history(query, read_control)
            .await?;
        if history.owner() != &self.owner {
            return Err(MemoryApplicationError::InvalidAuthorityResult {
                invariant: "project-memory history owner",
            });
        }
        if history.fact_id() != target.fact_id() {
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

    /// Pure owner-bound feedback history snapshot.
    pub async fn get_project_memory_feedback_history(
        &self,
        query: ProjectMemoryFactFeedbackHistoryQueryV1,
        read_control: &FactReadControl,
    ) -> Result<ProjectMemoryFactFeedbackHistoryV1, MemoryApplicationError> {
        self.ensure_owner(query.target().owner())?;
        let limit = query.limit();
        let history = self
            .authority
            .project_memory_fact_feedback_history(query, read_control)
            .await?;
        if history.owner() != &self.owner || history.events().len() > limit {
            return Err(MemoryApplicationError::InvalidAuthorityResult {
                invariant: "project-memory feedback history owner and bounds",
            });
        }
        Ok(history)
    }

    /// Pure status snapshot over canonical counters and memory algebra.
    pub async fn project_memory_status(
        &self,
        read_control: &FactReadControl,
    ) -> Result<ProjectMemoryMemoryStatusV1, MemoryApplicationError> {
        let status = self
            .authority
            .project_memory_status(self.owner.clone(), read_control)
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
        target: ProjectMemoryFactIdV1,
        read_control: &FactReadControl,
    ) -> Result<Option<ProjectMemoryFactInspectionV1>, MemoryApplicationError> {
        self.ensure_owner(target.owner())?;
        let inspection = self
            .authority
            .inspect_project_memory_fact(target.clone(), read_control)
            .await?;
        if let Some(inspection) = &inspection {
            validate_project_memory_inspection(&self.owner, &target, inspection)?;
        }
        Ok(inspection)
    }

    pub async fn update_project_memory_fact(
        &self,
        request: ProjectMemoryFactUpdateCommandV1,
        write_control: &FactWriteControl,
    ) -> Result<
        ProjectMemoryFactUpdateOutcomeV1,
        MemoryMutationError<ProjectMemoryFactUpdateOutcomeV1>,
    > {
        self.ensure_owner(request.target().owner())?;
        let target = request.target().clone();
        let outcome = self
            .authority
            .update_project_memory_fact(request, write_control)
            .await
            .map_err(MemoryApplicationError::from)?;
        settle_authority_result(outcome, |outcome| {
            validate_project_memory_projection(&self.owner, &target, outcome.fact())?;
            validate_commit_receipt(
                &self.owner,
                outcome.fact().fact_id(),
                Some(outcome.commit_receipt()),
                outcome.commit_replayed(),
                "project-memory update commit receipt",
            )
        })
    }

    pub async fn remove_project_memory_fact(
        &self,
        request: ProjectMemoryFactRemoveCommandV1,
        write_control: &FactWriteControl,
    ) -> Result<
        ProjectMemoryFactRemoveOutcomeV1,
        MemoryMutationError<ProjectMemoryFactRemoveOutcomeV1>,
    > {
        self.ensure_owner(request.target().owner())?;
        let target = request.target().clone();
        let outcome = self
            .authority
            .remove_project_memory_fact(request, write_control)
            .await
            .map_err(MemoryApplicationError::from)?;
        settle_authority_result(outcome, |outcome| {
            // A `None` fact is the idempotent no-op disposition for a target
            // that never resolved inside the authority transaction.
            if let Some(fact) = outcome.fact() {
                validate_project_memory_projection(&self.owner, &target, fact)?;
            }
            if let Some(receipt) = outcome.commit_receipt() {
                validate_commit_receipt(
                    &self.owner,
                    target.fact_id(),
                    Some(receipt),
                    outcome.commit_replayed(),
                    "project-memory remove commit receipt",
                )?;
            } else if outcome.commit_replayed() {
                return Err(MemoryApplicationError::InvalidAuthorityResult {
                    invariant: "project-memory remove replay without commit receipt",
                });
            }
            Ok(())
        })
    }

    pub async fn record_project_memory_fact_feedback(
        &self,
        request: ProjectMemoryFactFeedbackCommandV1,
        write_control: &FactWriteControl,
    ) -> Result<
        ProjectMemoryFactFeedbackOutcomeV1,
        MemoryMutationError<ProjectMemoryFactFeedbackOutcomeV1>,
    > {
        self.ensure_owner(request.target().owner())?;
        let target = request.target().clone();
        let outcome = self
            .authority
            .record_project_memory_fact_feedback(request, write_control)
            .await
            .map_err(MemoryApplicationError::from)?;
        settle_authority_result(outcome, |outcome| {
            validate_project_memory_projection(&self.owner, &target, outcome.fact())?;
            validate_commit_receipt(
                &self.owner,
                outcome.fact().fact_id(),
                Some(outcome.commit_receipt()),
                outcome.commit_replayed(),
                "project-memory feedback commit receipt",
            )?;
            if outcome.commit_receipt().last_event_id() != outcome.event_id() {
                return Err(MemoryApplicationError::InvalidAuthorityResult {
                    invariant: "project-memory feedback event receipt",
                });
            }
            Ok(())
        })
    }

    pub async fn record_project_memory_fact_retrieval(
        &self,
        request: ProjectMemoryFactRetrievalCommandV1,
        write_control: &FactWriteControl,
    ) -> Result<
        ProjectMemoryFactRetrievalOutcomeV1,
        MemoryMutationError<ProjectMemoryFactRetrievalOutcomeV1>,
    > {
        self.ensure_owner(request.owner())?;
        let operation_id = request.operation_id().clone();
        let targets = request.targets().to_vec();
        let recall = request.recall();
        let input_digest = request
            .input_digest()
            .map_err(MemoryApplicationError::from)?;
        let outcome = self
            .authority
            .record_project_memory_fact_retrieval(request, write_control)
            .await
            .map_err(MemoryApplicationError::from)?;
        settle_authority_result(outcome, |outcome| {
            validate_project_memory_retrieval_outcome(
                &self.owner,
                &operation_id,
                &input_digest,
                &targets,
                recall,
                outcome,
            )
        })
    }

    pub async fn apply_project_memory_automatic_fact(
        &self,
        apply_id: ProvenanceId,
        request: ProjectMemoryFactAddCommandV1,
        evidence: ProjectMemoryAutomaticFactEvidenceV1,
        write_control: &FactWriteControl,
    ) -> Result<
        ProjectMemoryAutomaticFactApplyResultV1,
        MemoryMutationError<ProjectMemoryAutomaticFactApplyResultV1>,
    > {
        self.ensure_owner(request.owner())?;
        let expected_request = request.clone();
        let expected_evidence = evidence.clone();
        let result = self
            .authority
            .apply_project_memory_automatic_fact(apply_id.clone(), request, evidence, write_control)
            .await
            .map_err(MemoryApplicationError::from)?;
        settle_authority_result(result, |result| {
            validate_project_memory_automatic_fact_apply_receipt(
                &self.owner,
                &apply_id,
                &expected_request,
                &expected_evidence,
                result.receipt(),
            )?;
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
            Ok(())
        })
    }

    pub async fn get_project_memory_automatic_fact_receipt(
        &self,
        apply_id: ProvenanceId,
        read_control: &FactReadControl,
    ) -> Result<Option<ProjectMemoryAutomaticFactReceiptV1>, MemoryApplicationError> {
        let receipt = self
            .authority
            .get_project_memory_automatic_fact_receipt(
                self.owner.clone(),
                apply_id.clone(),
                read_control,
            )
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
        read_control: &FactReadControl,
    ) -> Result<ProjectMemoryAutomaticFactReceiptPageV1, MemoryApplicationError> {
        let page = self
            .authority
            .list_project_memory_automatic_fact_receipts(
                self.owner.clone(),
                state,
                after_apply_id.clone(),
                limit,
                read_control,
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

    pub async fn project_memory_automation_run_receipts(
        &self,
        run_id: RunId,
        read_control: &FactReadControl,
    ) -> Result<ProjectMemoryAutomationRunReceiptsV1, MemoryApplicationError> {
        run_id
            .validate()
            .map_err(|_| MemoryApplicationError::InvalidInput {
                invariant: "memory automation run identity",
            })?;
        let receipts = self
            .authority
            .project_memory_automation_run_receipts(
                self.owner.clone(),
                run_id.clone(),
                read_control,
            )
            .await?;
        if receipts.owner() != &self.owner || receipts.run_id() != &run_id {
            return Err(MemoryApplicationError::InvalidAuthorityResult {
                invariant: "memory automation receipt recovery identity",
            });
        }
        Ok(receipts)
    }
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
    target: &ProjectMemoryFactIdV1,
    projection: &ProjectMemoryFactProjectionV1,
) -> Result<(), MemoryApplicationError> {
    if projection.owner() != owner {
        return Err(MemoryApplicationError::InvalidAuthorityResult {
            invariant: "project-memory projection owner",
        });
    }
    if projection.fact_id() != target.fact_id() {
        return Err(MemoryApplicationError::InvalidAuthorityResult {
            invariant: "project-memory projection canonical identity",
        });
    }
    Ok(())
}

fn validate_project_memory_inspection(
    owner: &FactOwnerV1,
    target: &ProjectMemoryFactIdV1,
    inspection: &ProjectMemoryFactInspectionV1,
) -> Result<(), MemoryApplicationError> {
    if inspection.owner() != owner
        || inspection.history().owner() != owner
        || inspection.status().owner() != owner
        || inspection.history().fact_id() != inspection.fact().fact_id()
        || inspection.status().fact_id() != inspection.fact().fact_id()
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
    if inspection.fact().fact_id() != target.fact_id() {
        return Err(MemoryApplicationError::InvalidAuthorityResult {
            invariant: "project-memory inspection canonical identity",
        });
    }
    Ok(())
}

pub(super) fn validate_project_memory_add_outcome(
    owner: &FactOwnerV1,
    outcome: &ProjectMemoryFactAddOutcomeV1,
) -> Result<(), MemoryApplicationError> {
    let invalid_owner = outcome.fact().owner() != owner
        || outcome
            .closest_fact_id()
            .is_some_and(|fact_id| fact_id.owner() != owner);
    let invalid_receipt = outcome.commit_receipt().is_some_and(|receipt| {
        receipt.owner() != owner || outcome.fact().fact_id() != receipt.fact_id()
    });
    let comparison_matches_fact = outcome
        .closest_fact_id()
        .is_some_and(|closest| closest.fact_id() == outcome.fact().fact_id());
    let invalid_disposition = match outcome.disposition() {
        ProjectMemoryFactAddDispositionV1::Added
        | ProjectMemoryFactAddDispositionV1::PossibleConflict => outcome.commit_receipt().is_none(),
        ProjectMemoryFactAddDispositionV1::NearDuplicate if comparison_matches_fact => {
            outcome.commit_receipt().is_some() || outcome.commit_replayed()
        }
        ProjectMemoryFactAddDispositionV1::NearDuplicate => outcome.commit_receipt().is_none(),
    };
    if invalid_owner || invalid_receipt || invalid_disposition {
        return Err(MemoryApplicationError::InvalidAuthorityResult {
            invariant: "project-memory add outcome identity and commit receipt",
        });
    }
    Ok(())
}

fn validate_commit_receipt(
    owner: &FactOwnerV1,
    fact_id: &FactId,
    receipt: Option<&tracedecay_store::FactCommitReceipt>,
    replayed: bool,
    invariant: &'static str,
) -> Result<(), MemoryApplicationError> {
    if receipt.is_some_and(|receipt| receipt.owner() != owner || receipt.fact_id() != fact_id)
        || replayed && receipt.is_none()
    {
        return Err(MemoryApplicationError::InvalidAuthorityResult { invariant });
    }
    Ok(())
}

fn validate_project_memory_retrieval_outcome(
    owner: &FactOwnerV1,
    operation_id: &ProvenanceId,
    input_digest: &str,
    targets: &[ProjectMemoryFactIdV1],
    recall: bool,
    outcome: &ProjectMemoryFactRetrievalOutcomeV1,
) -> Result<(), MemoryApplicationError> {
    let receipt = outcome.receipt();
    if receipt.owner() != owner {
        return Err(MemoryApplicationError::InvalidAuthorityResult {
            invariant: "project-memory retrieval receipt owner",
        });
    }
    if receipt.operation_id() != operation_id {
        return Err(MemoryApplicationError::InvalidAuthorityResult {
            invariant: "project-memory retrieval receipt operation",
        });
    }
    if receipt.input_digest() != input_digest {
        return Err(MemoryApplicationError::InvalidAuthorityResult {
            invariant: "project-memory retrieval receipt input",
        });
    }
    if receipt.fact_ids() != targets || receipt.recall() != recall {
        return Err(MemoryApplicationError::InvalidAuthorityResult {
            invariant: "project-memory retrieval receipt targets",
        });
    }
    if outcome.projections().len() != targets.len()
        || outcome
            .projections()
            .iter()
            .zip(targets)
            .any(|(projection, target)| {
                projection.owner() != owner
                    || projection.owner() != target.owner()
                    || projection.fact_id() != target.fact_id()
            })
    {
        return Err(MemoryApplicationError::InvalidAuthorityResult {
            invariant: "project-memory retrieval projection correspondence",
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

fn validate_project_memory_automatic_fact_apply_receipt(
    owner: &FactOwnerV1,
    apply_id: &ProvenanceId,
    request: &ProjectMemoryFactAddCommandV1,
    evidence: &ProjectMemoryAutomaticFactEvidenceV1,
    receipt: &ProjectMemoryAutomaticFactReceiptV1,
) -> Result<(), MemoryApplicationError> {
    validate_project_memory_automatic_fact_receipt(owner, apply_id, receipt)?;
    if receipt.request() != request || receipt.evidence() != evidence {
        return Err(MemoryApplicationError::InvalidAuthorityResult {
            invariant: "automatic fact receipt exact request and evidence identity",
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
