//! Legacy V1 memory API shims over the typed compatibility use cases.

use tracedecay_domain::Confidence;
use tracedecay_store::{
    ProjectMemoryFactAddOutcomeV1, ProjectMemoryFactContradictionQueryV1,
    ProjectMemoryFactFeedbackActionV1, ProjectMemoryFactFeedbackCommandV1,
    ProjectMemoryFactFeedbackDetailsAvailabilityV1, ProjectMemoryFactFeedbackHistoryQueryV1,
    ProjectMemoryFactListQueryV1, ProjectMemoryFactProjectionV1, ProjectMemoryFactRemoveCommandV1,
    ProjectMemoryFactRetrievalCommandV1, ProjectMemoryFactSearchFilterV1,
    ProjectMemoryFactSearchKindV1, ProjectMemoryFactSearchQuery, ProjectMemoryFactStore,
    ProjectMemoryFactTargetV1, ProjectMemoryFactUpdateCommandV1, ProjectMemoryFactUpdatePatchV1,
    ProjectMemoryFeedbackRepairProgressV1,
};

use tracedecay_runtime_core::memory::hygiene::detect_secret_like;
use tracedecay_runtime_core::memory::types::{
    AddFactDiff, AddFactDiffKind, AddFactOutcome, AddFactRequest, ContradictionResult, FactRecord,
    FactSearchResult, FeedbackAction, FeedbackRequest, MemoryCategory, MemoryStatus,
    SearchFactsRequest, TrustHistoryEntry, UpdateFactRequest,
};

use super::MemoryApplication;
use super::compatibility::{
    compatibility_add_command, compatibility_confidence, compatibility_projection_targets,
    fact_category, legacy_i64, project_memory_fact_record, project_memory_projection_record,
    project_memory_status_v1,
};
use super::context::MemoryOperationContext;
use super::error::MemoryApplicationError;
use super::sanitize::{
    prepare_tainted_update_fact_request, sanitize_add_fact_request, sanitize_optional_memory_text,
};

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
    pub repair_progress: ProjectMemoryFeedbackRepairProgressV1,
}

/// Legacy status fields and feedback-history repair state from one authority
/// snapshot. Consumers must use this instead of issuing two status reads.
#[derive(Clone, Debug, PartialEq)]
pub struct V1MemoryStatusWithRepairV1 {
    pub status: MemoryStatus,
    pub feedback_history_repair: ProjectMemoryFeedbackRepairProgressV1,
}

impl<A: ProjectMemoryFactStore> MemoryApplication<A> {
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
            .add_project_memory_fact(compatibility_add_command(
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
            ProjectMemoryFactSearchKindV1::Search,
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
            ProjectMemoryFactSearchKindV1::Search,
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
            ProjectMemoryFactSearchKindV1::Probe,
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
            ProjectMemoryFactSearchKindV1::Probe,
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
            ProjectMemoryFactSearchKindV1::Related {
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
            ProjectMemoryFactSearchKindV1::Related {
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
            ProjectMemoryFactSearchKindV1::Reason { entities },
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
            ProjectMemoryFactSearchKindV1::Reason { entities },
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
            .find_project_memory_contradictions(ProjectMemoryFactContradictionQueryV1::new(
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
                    existing_fact: project_memory_fact_record(
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
            .list_project_memory_facts(ProjectMemoryFactListQueryV1::new(
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
            .filter(|fact| matches!(fact, ProjectMemoryFactProjectionV1::Available(_)))
            .map(|fact| project_memory_projection_record(&self.compatibility_scope, fact))
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
        match self.get_project_memory_fact(target).await? {
            // A removed or otherwise unavailable fact reads as absent under
            // the V1 contract; only reachable payloads project to records.
            None | Some(ProjectMemoryFactProjectionV1::Unavailable(_)) => Ok(None),
            Some(projection) => {
                project_memory_projection_record(&self.compatibility_scope, &projection).map(Some)
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
        let Some(request) = prepare_tainted_update_fact_request(request)? else {
            return Ok(V1UpdateFactOutcome::RejectedSecretLike {
                reason: "rejected_secret_like: content or structured payload was rejected by the privacy sanitizer".to_owned(),
            });
        };
        let target = self.legacy_compatibility_target(request.fact_id)?;
        let patch = ProjectMemoryFactUpdatePatchV1::new(
            request.content,
            request.category.map(fact_category),
            request.source.map(Some),
            request.tags,
            request.entities,
            request.metadata,
            compatibility_confidence(request.trust)?,
        )?;
        let outcome = self
            .update_project_memory_fact(ProjectMemoryFactUpdateCommandV1::new(
                target,
                context.operation_id().clone(),
                None,
                patch,
                context.actor().cloned(),
            )?)
            .await?;
        Ok(V1UpdateFactOutcome::Updated(Box::new(
            project_memory_projection_record(&self.compatibility_scope, outcome.fact())?,
        )))
    }

    pub async fn remove_fact_v1(
        &self,
        fact_id: i64,
        context: MemoryOperationContext,
    ) -> Result<bool, MemoryApplicationError> {
        let target = self.legacy_compatibility_target(fact_id)?;
        // Removing a fact that was never stored (or was concurrently removed
        // just before this call) is an idempotent no-op, mirroring the legacy
        // MemoryStore contract. The authority resolves that disposition
        // inside its single remove transaction and reports it as
        // `removed() == false`; callers (e.g. the dashboard curate handler)
        // surface this as a per-op "fact not found" result rather than an
        // authority failure. This deliberately avoids a separate pre-read
        // transaction: two independent authority round trips would leave a
        // window where a concurrent remove between them could still surface
        // an authority error instead of the idempotent no-op.
        let outcome = self
            .remove_project_memory_fact(ProjectMemoryFactRemoveCommandV1::new(
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
    ) -> Result<tracedecay_runtime_core::memory::types::FeedbackResult, MemoryApplicationError>
    {
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
            FeedbackAction::Helpful => ProjectMemoryFactFeedbackActionV1::Helpful,
            FeedbackAction::Unhelpful => ProjectMemoryFactFeedbackActionV1::Unhelpful,
        };
        let outcome = self
            .record_project_memory_fact_feedback(ProjectMemoryFactFeedbackCommandV1::new(
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
        let fact = project_memory_projection_record(&self.compatibility_scope, outcome.fact())?;
        Ok(tracedecay_runtime_core::memory::types::FeedbackResult {
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
            .get_project_memory_feedback_history(ProjectMemoryFactFeedbackHistoryQueryV1::new(
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
                    == ProjectMemoryFactFeedbackDetailsAvailabilityV1::Available
            })
            .filter_map(|event| {
                let source = event.source()?;
                Some(TrustHistoryEntry {
                    timestamp: event.occurred_at().0,
                    action: match event.action() {
                        ProjectMemoryFactFeedbackActionV1::Helpful => FeedbackAction::Helpful,
                        ProjectMemoryFactFeedbackActionV1::Unhelpful => FeedbackAction::Unhelpful,
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
    /// This is a pure read: it reports the live backlog (missing vectors,
    /// projection and feedback repair state) and never triggers a repair pass
    /// as a side effect. Repair remains owned by the daemon's bounded memory-
    /// repair scheduler and the explicit [`Self::dashboard_repair_v1`] entry
    /// point; a status read must not race or duplicate that work. The legacy
    /// `MemoryStatus`/`V1MemoryStatusWithRepairV1` field shapes are
    /// unchanged, but `repair` counters are always zero here: they describe
    /// repairs performed by the reporting request, and a pure read performs
    /// none — explicit repair entry points return their own batch stats.
    pub async fn memory_status_with_repair_v1(
        &self,
    ) -> Result<V1MemoryStatusWithRepairV1, MemoryApplicationError> {
        let status = self.project_memory_status().await?;
        let feedback_history_repair = status.feedback_history_repair();
        let projected = project_memory_status_v1(&status)?;
        Ok(V1MemoryStatusWithRepairV1 {
            status: projected,
            feedback_history_repair,
        })
    }

    async fn search_v1(
        &self,
        kind: ProjectMemoryFactSearchKindV1,
        query: Option<String>,
        request: SearchFactsRequest,
        context: Option<MemoryOperationContext>,
        recall: bool,
    ) -> Result<Vec<FactSearchResult>, MemoryApplicationError> {
        let filter = ProjectMemoryFactSearchFilterV1::new(
            request.category.map(fact_category),
            compatibility_confidence(request.min_trust)?,
            None,
        )?;
        let query = ProjectMemoryFactSearchQuery::with_filter(
            self.owner.clone(),
            kind.clone(),
            query,
            filter,
            None,
            request.limit.unwrap_or(20),
        )?;
        let page = match kind {
            ProjectMemoryFactSearchKindV1::Search => {
                self.search_project_memory_facts(query).await?
            }
            ProjectMemoryFactSearchKindV1::Probe => self.probe_project_memory_facts(query).await?,
            ProjectMemoryFactSearchKindV1::Related { .. } => {
                self.related_project_memory_facts(query).await?
            }
            ProjectMemoryFactSearchKindV1::Reason { .. } => {
                self.reason_project_memory_facts(query).await?
            }
        };
        let targets = page
            .hits()
            .iter()
            .map(|hit| {
                ProjectMemoryFactTargetV1::Canonical(
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
                    fact: project_memory_fact_record(&self.compatibility_scope, hit.fact())?,
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
        targets: Vec<ProjectMemoryFactTargetV1>,
        context: &MemoryOperationContext,
        recall: bool,
    ) -> Result<(), MemoryApplicationError> {
        if targets.is_empty() {
            return Ok(());
        }
        self.record_project_memory_fact_retrieval(ProjectMemoryFactRetrievalCommandV1::new(
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
        outcome: ProjectMemoryFactAddOutcomeV1,
    ) -> Result<AddFactOutcome, MemoryApplicationError> {
        let fact = outcome
            .fact()
            .map(|fact| project_memory_projection_record(&self.compatibility_scope, fact))
            .transpose()?;
        let closest_fact_id = match outcome.closest_fact_id() {
            Some(id) => {
                let projection = self
                    .get_project_memory_fact(ProjectMemoryFactTargetV1::Canonical(id.clone()))
                    .await?
                    .ok_or(MemoryApplicationError::IncompatibleLegacyProjection {
                        invariant: "closest legacy fact mapping",
                    })?;
                Some(
                    project_memory_projection_record(&self.compatibility_scope, &projection)?
                        .fact_id,
                )
            }
            None => None,
        };
        Ok(AddFactOutcome {
            fact,
            diff: AddFactDiff {
                diff: match outcome.disposition() {
                    tracedecay_store::ProjectMemoryFactAddDispositionV1::Added => {
                        AddFactDiffKind::Add
                    }
                    tracedecay_store::ProjectMemoryFactAddDispositionV1::NearDuplicate => {
                        AddFactDiffKind::NearDuplicate
                    }
                    tracedecay_store::ProjectMemoryFactAddDispositionV1::PossibleConflict => {
                        AddFactDiffKind::PossibleConflict
                    }
                    tracedecay_store::ProjectMemoryFactAddDispositionV1::RejectedSecretLike => {
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
