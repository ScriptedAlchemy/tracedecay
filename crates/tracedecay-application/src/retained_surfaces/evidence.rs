//! Truth-preserving evidence facts derived from exact retained results.
//!
//! This projection deliberately leaves coverage unknown when a lower
//! authority did not report visited or eligible counts. Transport adapters
//! use it to build the common application envelope without upgrading a
//! bounded result into fabricated complete evidence.

use crate::{
    CoverageCompleteness, EvidenceDomain, FreshnessState, OmissionReason, OpaqueCursor, PageCursor,
};

use super::{
    HydrationStateResultV1, LcmRetrievalOutcomeV1, LcmTemporalFieldsV1, RetainedOutcomeStatusV1,
    RetainedSurfaceResultV1, SessionCoverageModeV1, SessionSourceCoverageV1, TemporalFreshnessV1,
    TemporalMetadataV1, TemporalWatermarksV1,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RetainedSurfaceEvidenceTerminalV1 {
    Effect,
    Busy,
    Cancelled,
    Conflict,
    CursorManifestLimitExceeded,
    Denied,
    Failed,
    InvalidOutput,
    NotFoundOrNotAuthorized,
    TimedOut,
    Unavailable,
    Unsupported,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RetainedSurfaceEvidenceOmissionV1 {
    pub reason: OmissionReason,
    pub count: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RetainedSurfaceTemporalRequestV1 {
    pub source_id: String,
    pub mode: SessionCoverageModeV1,
}

/// Exact temporal authority carried by retained session results.
///
/// The source watermarks are intentionally retained as fields rather than
/// replaced with an adapter-created timestamp. Per-source request modes stay
/// distinct because a multi-source response does not prove one global mode.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RetainedSurfaceTemporalFactsV1 {
    pub watermarks: TemporalWatermarksV1,
    pub requests: Vec<RetainedSurfaceTemporalRequestV1>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RetainedSurfaceEvidenceFactsV1 {
    pub domain: EvidenceDomain,
    pub returned: u64,
    pub visited: Option<u64>,
    pub eligible: Option<u64>,
    pub total: Option<u64>,
    pub next_cursor: Option<PageCursor>,
    pub completeness: CoverageCompleteness,
    pub freshness: FreshnessState,
    pub omissions: Vec<RetainedSurfaceEvidenceOmissionV1>,
    /// Lower authorities sometimes report an omitted count without assigning
    /// a safe reason. Keep it explicit instead of inventing one.
    pub unattributed_omitted: Option<u64>,
    pub temporal: Option<RetainedSurfaceTemporalFactsV1>,
}

impl RetainedSurfaceEvidenceFactsV1 {
    fn unknown(
        domain: EvidenceDomain,
        returned: usize,
    ) -> Result<Self, RetainedSurfaceEvidenceTerminalV1> {
        Ok(Self {
            domain,
            returned: count(returned)?,
            visited: None,
            eligible: None,
            total: None,
            next_cursor: None,
            completeness: CoverageCompleteness::Unknown,
            freshness: FreshnessState::Unknown,
            omissions: Vec::new(),
            unattributed_omitted: None,
            temporal: None,
        })
    }

    fn unknown_singleton(
        domain: EvidenceDomain,
        present: bool,
    ) -> Result<Self, RetainedSurfaceEvidenceTerminalV1> {
        Self::unknown(domain, usize::from(present))
    }

    fn apply_status(
        &mut self,
        status: RetainedOutcomeStatusV1,
    ) -> Result<(), RetainedSurfaceEvidenceTerminalV1> {
        match status {
            RetainedOutcomeStatusV1::Ok
            | RetainedOutcomeStatusV1::Complete
            | RetainedOutcomeStatusV1::CompleteZero
            | RetainedOutcomeStatusV1::Recorded
            | RetainedOutcomeStatusV1::Running
            | RetainedOutcomeStatusV1::Started
            | RetainedOutcomeStatusV1::Joined => Ok(()),
            RetainedOutcomeStatusV1::Partial => {
                self.completeness = CoverageCompleteness::Partial;
                Ok(())
            }
            RetainedOutcomeStatusV1::Stale => {
                self.completeness = CoverageCompleteness::Partial;
                self.freshness = FreshnessState::Stale;
                Ok(())
            }
            RetainedOutcomeStatusV1::Redacted => {
                self.completeness = CoverageCompleteness::Partial;
                self.omissions.push(RetainedSurfaceEvidenceOmissionV1 {
                    reason: OmissionReason::Redacted,
                    count: 1,
                });
                Ok(())
            }
            RetainedOutcomeStatusV1::Deleted => {
                self.completeness = CoverageCompleteness::Partial;
                self.omissions.push(RetainedSurfaceEvidenceOmissionV1 {
                    reason: OmissionReason::Unavailable,
                    count: 1,
                });
                Ok(())
            }
            RetainedOutcomeStatusV1::Busy => Err(RetainedSurfaceEvidenceTerminalV1::Busy),
            RetainedOutcomeStatusV1::Cancelled | RetainedOutcomeStatusV1::Aborted => {
                Err(RetainedSurfaceEvidenceTerminalV1::Cancelled)
            }
            RetainedOutcomeStatusV1::DeadlineExceeded => {
                Err(RetainedSurfaceEvidenceTerminalV1::TimedOut)
            }
            RetainedOutcomeStatusV1::Denied | RetainedOutcomeStatusV1::WrongScope => {
                Err(RetainedSurfaceEvidenceTerminalV1::Denied)
            }
            RetainedOutcomeStatusV1::NotFound => {
                Err(RetainedSurfaceEvidenceTerminalV1::NotFoundOrNotAuthorized)
            }
            RetainedOutcomeStatusV1::Unavailable | RetainedOutcomeStatusV1::Locked => {
                Err(RetainedSurfaceEvidenceTerminalV1::Unavailable)
            }
            RetainedOutcomeStatusV1::UnsupportedFilter => {
                Err(RetainedSurfaceEvidenceTerminalV1::Unsupported)
            }
            RetainedOutcomeStatusV1::CursorManifestLimitExceeded => {
                Err(RetainedSurfaceEvidenceTerminalV1::CursorManifestLimitExceeded)
            }
            RetainedOutcomeStatusV1::BudgetExhausted => {
                Err(RetainedSurfaceEvidenceTerminalV1::Unavailable)
            }
            RetainedOutcomeStatusV1::Error | RetainedOutcomeStatusV1::Failed => {
                Err(RetainedSurfaceEvidenceTerminalV1::Failed)
            }
        }
    }

    fn apply_temporal(
        &mut self,
        temporal: &TemporalMetadataV1,
    ) -> Result<(), RetainedSurfaceEvidenceTerminalV1> {
        self.next_cursor = opaque_page_cursor(temporal.next_cursor.as_deref())?;
        self.visited = Some(temporal_visited(&temporal.coverage)?);
        self.temporal = Some(temporal_facts(
            &temporal.watermarks,
            &temporal.source_coverage,
        ));
        match temporal.freshness.as_ref().map(freshness_state) {
            Some(FreshnessState::Stale) => self.freshness = FreshnessState::Stale,
            Some(FreshnessState::Current) if self.freshness == FreshnessState::Unknown => {
                self.freshness = FreshnessState::Current;
            }
            Some(FreshnessState::Current | FreshnessState::Unknown) | None => {}
        }
        if temporal.coverage.hidden > 0
            || temporal.coverage.unknown > 0
            || temporal.coverage.redacted > 0
            || !temporal.omissions.is_empty()
        {
            self.completeness = CoverageCompleteness::Partial;
        }
        self.omissions
            .extend(temporal.omissions.iter().filter_map(|omission| {
                omission_reason(omission.reason)
                    .map(|reason| RetainedSurfaceEvidenceOmissionV1 { reason, count: 1 })
            }));
        Ok(())
    }

    fn apply_lcm_temporal(
        &mut self,
        temporal: &LcmTemporalFieldsV1,
    ) -> Result<(), RetainedSurfaceEvidenceTerminalV1> {
        self.next_cursor = opaque_page_cursor(temporal.next_cursor.as_deref())?;
        self.visited = Some(temporal_visited(&temporal.coverage)?);
        self.temporal = Some(temporal_facts(
            &temporal.watermarks,
            &temporal.source_coverage,
        ));
        if temporal.coverage.hidden > 0
            || temporal.coverage.unknown > 0
            || temporal.coverage.redacted > 0
            || !temporal.omissions.is_empty()
        {
            self.completeness = CoverageCompleteness::Partial;
        }
        self.omissions
            .extend(temporal.omissions.iter().filter_map(|omission| {
                omission_reason(omission.reason)
                    .map(|reason| RetainedSurfaceEvidenceOmissionV1 { reason, count: 1 })
            }));
        Ok(())
    }

    fn apply_lcm_retrieval(
        &mut self,
        retrieval: &LcmRetrievalOutcomeV1,
    ) -> Result<(), RetainedSurfaceEvidenceTerminalV1> {
        let (completeness, freshness, omitted) = match retrieval {
            LcmRetrievalOutcomeV1::Complete { freshness } => (
                CoverageCompleteness::Complete,
                freshness_state(freshness),
                Some(0),
            ),
            LcmRetrievalOutcomeV1::Partial { freshness, omitted } => (
                CoverageCompleteness::Partial,
                freshness_state(freshness),
                Some(*omitted),
            ),
            LcmRetrievalOutcomeV1::Stale { freshness } => (
                CoverageCompleteness::Partial,
                freshness_state(freshness),
                Some(0),
            ),
        };
        if let (Some(reported), Some(authoritative)) = (self.unattributed_omitted, omitted)
            && reported != authoritative
        {
            return Err(RetainedSurfaceEvidenceTerminalV1::InvalidOutput);
        }
        self.unattributed_omitted = self.unattributed_omitted.or(omitted);
        if completeness == CoverageCompleteness::Partial {
            self.completeness = CoverageCompleteness::Partial;
        } else if self.completeness == CoverageCompleteness::Unknown {
            self.completeness = CoverageCompleteness::Complete;
        }
        if freshness == FreshnessState::Stale {
            self.freshness = FreshnessState::Stale;
        } else if self.freshness == FreshnessState::Unknown {
            self.freshness = freshness;
        }
        Ok(())
    }

    fn apply_unattributed_omitted(&mut self, omitted: Option<u64>) {
        if omitted.is_some_and(|count| count > 0) {
            self.completeness = CoverageCompleteness::Partial;
        }
        self.unattributed_omitted = omitted;
    }

    /// Supplies the counts the evidence contract demands of complete coverage.
    ///
    /// `EvidenceCoverage::validate` rejects `Complete` unless both `visited`
    /// and `eligible` are present, so a lower authority that reports a
    /// complete retrieval without them would be projected into an envelope the
    /// transport refuses — the answer is lost as
    /// `application.retained.authority-unavailable`. Nothing is invented here:
    /// "complete" is that authority's own claim that every eligible item was
    /// returned and none omitted, which fixes `eligible` at `returned`, and a
    /// retrieval that returned `n` items visited at least `n`. Counts a real
    /// authority did report are never overwritten.
    fn settle_complete_coverage(&mut self) {
        if self.completeness != CoverageCompleteness::Complete {
            return;
        }
        if self.eligible.is_none() {
            self.eligible = Some(self.returned);
        }
        if self.visited.is_none() {
            self.visited = self.eligible;
        }
    }
}

impl RetainedSurfaceResultV1 {
    pub fn evidence_facts(
        &self,
    ) -> Result<RetainedSurfaceEvidenceFactsV1, RetainedSurfaceEvidenceTerminalV1> {
        match self {
            Self::FactStoreSearch(value) => {
                fact_search_collection(value.hits.len(), value.next_after.as_ref())
            }
            Self::FactStoreProbe(value) => {
                fact_search_collection(value.hits.len(), value.next_after.as_ref())
            }
            Self::FactStoreRelated(value) => {
                fact_search_collection(value.hits.len(), value.next_after.as_ref())
            }
            Self::FactStoreReason(value) => {
                fact_search_collection(value.hits.len(), value.next_after.as_ref())
            }
            Self::FactStoreContradict(value) => fact_collection(value.contradictions.len()),
            Self::FactStoreGet(_) => {
                RetainedSurfaceEvidenceFactsV1::unknown(EvidenceDomain::Operational, 1)
            }
            Self::FactStoreList(value) => {
                fact_list_collection(value.facts.len(), value.next_after_fact_id.as_ref())
            }
            Self::MemoryStatus(_) => {
                RetainedSurfaceEvidenceFactsV1::unknown_singleton(EvidenceDomain::Operational, true)
            }
            Self::SessionRefreshStatus(value) => {
                let mut facts = RetainedSurfaceEvidenceFactsV1::unknown_singleton(
                    EvidenceDomain::Temporal,
                    true,
                )?;
                facts.apply_status(value.outcome)?;
                Ok(facts)
            }
            Self::MessageSearch(value) => {
                let mut facts =
                    RetainedSurfaceEvidenceFactsV1::unknown(EvidenceDomain::Temporal, 0)?;
                facts.apply_status(value.status)?;
                facts.returned = count(message_search_returned(value)?)?;
                facts.apply_unattributed_omitted(value.omitted);
                if let Some(temporal) = &value.temporal {
                    facts.apply_temporal(temporal)?;
                }
                Ok(facts)
            }
            Self::SessionsFor(value) => {
                let mut facts =
                    RetainedSurfaceEvidenceFactsV1::unknown(EvidenceDomain::Temporal, value.count)?;
                facts.apply_status(value.status)?;
                Ok(facts)
            }
            Self::Workflows(value) => {
                let returned = value
                    .count
                    .or(value.agents_returned)
                    .unwrap_or_else(|| usize::from(value.found == Some(true)));
                let mut facts =
                    RetainedSurfaceEvidenceFactsV1::unknown(EvidenceDomain::Temporal, returned)?;
                facts.apply_status(value.status)?;
                Ok(facts)
            }
            Self::LcmStatus(value) => {
                let mut facts = RetainedSurfaceEvidenceFactsV1::unknown_singleton(
                    EvidenceDomain::Temporal,
                    value.lcm.is_some(),
                )?;
                facts.apply_status(value.status)?;
                Ok(facts)
            }
            Self::LcmDoctor(value) => {
                let mut facts = RetainedSurfaceEvidenceFactsV1::unknown_singleton(
                    EvidenceDomain::Diagnostic,
                    value.health.is_some(),
                )?;
                facts.apply_status(value.status)?;
                Ok(facts)
            }
            Self::LcmLoadSession(value) => lcm_facts(
                value.status,
                value.messages.len(),
                value.omitted,
                value.temporal.as_ref(),
                None,
            ),
            Self::LcmGrep(value) => lcm_facts(
                value.status,
                value.hits.len(),
                value.omitted,
                value.temporal.as_ref(),
                None,
            ),
            Self::LcmDescribe(value) => lcm_facts(
                value.status,
                usize::from(value.description.is_some()),
                value.omitted,
                value.temporal.as_ref(),
                value.retrieval.as_ref(),
            ),
            Self::LcmExpand(value) => lcm_facts(
                value.status,
                usize::from(value.expansion.is_some()),
                value.omitted,
                value.temporal.as_ref(),
                value.retrieval.as_ref(),
            ),
            Self::LcmExpandQuery(value) => lcm_facts(
                value.status,
                value.context_blocks.len(),
                value.omitted,
                value.temporal.as_ref(),
                None,
            ),
            Self::FactStoreCurate(_)
            | Self::FactStoreAdd(_)
            | Self::FactStoreUpdate(_)
            | Self::FactStoreRemove(_)
            | Self::FactFeedback(_)
            | Self::SessionRefreshCancel(_)
            | Self::SessionRefreshBegin(_) => Err(RetainedSurfaceEvidenceTerminalV1::Effect),
        }
    }
}

fn fact_collection(
    returned: usize,
) -> Result<RetainedSurfaceEvidenceFactsV1, RetainedSurfaceEvidenceTerminalV1> {
    RetainedSurfaceEvidenceFactsV1::unknown(EvidenceDomain::Operational, returned)
}

fn fact_search_collection(
    returned: usize,
    next_after: Option<&crate::memory::FactSearchCursorV1>,
) -> Result<RetainedSurfaceEvidenceFactsV1, RetainedSurfaceEvidenceTerminalV1> {
    let mut facts = fact_collection(returned)?;
    facts.next_cursor = next_after
        .cloned()
        .map(|cursor| PageCursor::FactSearch { cursor });
    Ok(facts)
}

fn fact_list_collection(
    returned: usize,
    next_after_fact_id: Option<&tracedecay_domain::FactId>,
) -> Result<RetainedSurfaceEvidenceFactsV1, RetainedSurfaceEvidenceTerminalV1> {
    let mut facts = fact_collection(returned)?;
    facts.next_cursor = next_after_fact_id
        .cloned()
        .map(|fact_id| PageCursor::FactListAfter { fact_id });
    Ok(facts)
}

fn opaque_page_cursor(
    cursor: Option<&str>,
) -> Result<Option<PageCursor>, RetainedSurfaceEvidenceTerminalV1> {
    cursor
        .map(|cursor| {
            OpaqueCursor::new(cursor.to_owned())
                .map(PageCursor::from)
                .map_err(|_| RetainedSurfaceEvidenceTerminalV1::InvalidOutput)
        })
        .transpose()
}

fn lcm_facts(
    status: RetainedOutcomeStatusV1,
    returned: usize,
    omitted: Option<u64>,
    temporal: Option<&LcmTemporalFieldsV1>,
    retrieval: Option<&LcmRetrievalOutcomeV1>,
) -> Result<RetainedSurfaceEvidenceFactsV1, RetainedSurfaceEvidenceTerminalV1> {
    let mut facts = RetainedSurfaceEvidenceFactsV1::unknown(EvidenceDomain::Temporal, returned)?;
    facts.apply_status(status)?;
    facts.apply_unattributed_omitted(omitted);
    if let Some(retrieval) = retrieval {
        facts.apply_lcm_retrieval(retrieval)?;
    }
    if let Some(temporal) = temporal {
        facts.apply_lcm_temporal(temporal)?;
    }
    facts.settle_complete_coverage();
    Ok(facts)
}

fn message_search_returned(
    value: &super::MessageSearchResultV1,
) -> Result<usize, RetainedSurfaceEvidenceTerminalV1> {
    let result_count = value.results.as_ref().map(Vec::len);
    match (value.count, result_count) {
        (Some(reported), Some(actual)) if reported != actual => {
            Err(RetainedSurfaceEvidenceTerminalV1::InvalidOutput)
        }
        (Some(reported), _) => Ok(reported),
        (None, Some(actual)) => Ok(actual),
        (None, None) if value.status == RetainedOutcomeStatusV1::CompleteZero => Ok(0),
        (None, None) => Err(RetainedSurfaceEvidenceTerminalV1::InvalidOutput),
    }
}

fn count(value: usize) -> Result<u64, RetainedSurfaceEvidenceTerminalV1> {
    u64::try_from(value).map_err(|_| RetainedSurfaceEvidenceTerminalV1::InvalidOutput)
}

fn temporal_visited(
    coverage: &super::TemporalCoverageV1,
) -> Result<u64, RetainedSurfaceEvidenceTerminalV1> {
    coverage
        .visible
        .checked_add(coverage.hidden)
        .and_then(|total| total.checked_add(coverage.unknown))
        .and_then(|total| total.checked_add(coverage.redacted))
        .ok_or(RetainedSurfaceEvidenceTerminalV1::InvalidOutput)
}

fn temporal_facts(
    watermarks: &TemporalWatermarksV1,
    source_coverage: &[SessionSourceCoverageV1],
) -> RetainedSurfaceTemporalFactsV1 {
    RetainedSurfaceTemporalFactsV1 {
        watermarks: watermarks.clone(),
        requests: source_coverage
            .iter()
            .map(|source| RetainedSurfaceTemporalRequestV1 {
                source_id: source.source_id.clone(),
                mode: source.request.mode,
            })
            .collect(),
    }
}

const fn freshness_state(value: &TemporalFreshnessV1) -> FreshnessState {
    match value {
        TemporalFreshnessV1::Fresh => FreshnessState::Current,
        TemporalFreshnessV1::Stored { .. } | TemporalFreshnessV1::Partial { .. } => {
            FreshnessState::Stale
        }
    }
}

const fn omission_reason(value: HydrationStateResultV1) -> Option<OmissionReason> {
    match value {
        HydrationStateResultV1::Available => None,
        HydrationStateResultV1::Redacted | HydrationStateResultV1::Unauthorized => {
            Some(OmissionReason::Redacted)
        }
        HydrationStateResultV1::RetainedButUnavailable
        | HydrationStateResultV1::Deleted
        | HydrationStateResultV1::RetentionExpired
        | HydrationStateResultV1::Locked
        | HydrationStateResultV1::UnverifiableLegacy => Some(OmissionReason::Unavailable),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::{FactSearchCursorV1, FactSearchGraphCoverageV1};
    use crate::result::PageCursor;
    use crate::retained_surfaces::{
        FactCommitOwnerV1, FactStoreContradictResultV1, FactStoreListResultV1,
        FactStoreSearchResultV1, MessageSearchHitV1, MessageSearchResultV1, RetainedNextActionV1,
        RetrievalWorkerStatusV1,
    };
    use tracedecay_domain::{FactId, UtcMicros};

    fn fact_id(identity_byte: char) -> FactId {
        FactId::new(format!(
            "fact.v1.{}.{}",
            "0".repeat(64),
            identity_byte.to_string().repeat(64)
        ))
        .expect("canonical fact id")
    }

    fn search_result(next_after: Option<FactSearchCursorV1>) -> FactStoreSearchResultV1 {
        FactStoreSearchResultV1 {
            owner: FactCommitOwnerV1::Profile,
            hits: Vec::new(),
            next_after,
            graph_coverage: FactSearchGraphCoverageV1::NotMounted,
        }
    }

    fn message_search_result(
        status: RetainedOutcomeStatusV1,
        count: Option<usize>,
        results: Option<Vec<MessageSearchHitV1>>,
    ) -> MessageSearchResultV1 {
        MessageSearchResultV1 {
            catch_up: false,
            catch_up_failures: Vec::new(),
            catch_up_performed: false,
            catch_up_provider: "all".to_owned(),
            count,
            goals: false,
            include_subagents: true,
            message_type: "all".to_owned(),
            next_action: None::<RetainedNextActionV1>,
            outcome: status,
            parent_session_id: None,
            project_key: None,
            provider: "all".to_owned(),
            query: None,
            refresh_required: false,
            requested_provider: None,
            results,
            scope: "all".to_owned(),
            since: None,
            status,
            until: None,
            error: None,
            git_filter: None,
            git_filter_applied: None,
            message: None,
            omitted: None,
            project_scope: None,
            registry_truncated: None,
            roots: None,
            searched_project_count: None,
            selected_project_root: None,
            service_status: None::<RetrievalWorkerStatusV1>,
            skipped: None,
            skipped_project_count: None,
            store_scope: None,
            temporal: None,
            workflow_agent: None,
            workflow_filter_applied: None,
            workflow_run: None,
            workflow_run_parent_session: None,
        }
    }

    #[test]
    fn fact_collection_keeps_unproved_coverage_unknown() {
        let facts = fact_collection(3).expect("bounded fact collection");
        assert_eq!(facts.returned, 3);
        assert_eq!(facts.visited, None);
        assert_eq!(facts.eligible, None);
        assert_eq!(facts.total, None);
        assert_eq!(facts.completeness, CoverageCompleteness::Unknown);
    }

    #[test]
    fn fact_search_evidence_preserves_structural_cursor() {
        let cursor = FactSearchCursorV1 {
            score_millionths: 750_000,
            updated_at: UtcMicros(42),
            fact_id: fact_id('1'),
        };
        let facts = RetainedSurfaceResultV1::FactStoreSearch(search_result(Some(cursor.clone())))
            .evidence_facts()
            .expect("search evidence");

        assert_eq!(facts.next_cursor, Some(PageCursor::FactSearch { cursor }));
    }

    #[test]
    fn fact_search_evidence_omits_absent_cursor() {
        let facts = RetainedSurfaceResultV1::FactStoreSearch(search_result(None))
            .evidence_facts()
            .expect("final search page evidence");

        assert_eq!(facts.next_cursor, None);
    }

    #[test]
    fn fact_list_evidence_preserves_structural_cursor() {
        let fact_id = fact_id('2');
        let result = FactStoreListResultV1 {
            owner: FactCommitOwnerV1::Profile,
            facts: Vec::new(),
            next_after_fact_id: Some(fact_id.clone()),
        };
        let facts = RetainedSurfaceResultV1::FactStoreList(result)
            .evidence_facts()
            .expect("list evidence");

        assert_eq!(
            facts.next_cursor,
            Some(PageCursor::FactListAfter { fact_id })
        );
    }

    #[test]
    fn fact_list_evidence_omits_absent_cursor() {
        let result = FactStoreListResultV1 {
            owner: FactCommitOwnerV1::Profile,
            facts: Vec::new(),
            next_after_fact_id: None,
        };
        let facts = RetainedSurfaceResultV1::FactStoreList(result)
            .evidence_facts()
            .expect("final list page evidence");

        assert_eq!(facts.next_cursor, None);
    }

    #[test]
    fn fact_contradiction_evidence_is_intentionally_nonpaginated() {
        let result = FactStoreContradictResultV1 {
            owner: FactCommitOwnerV1::Profile,
            contradictions: Vec::new(),
        };
        let facts = RetainedSurfaceResultV1::FactStoreContradict(result)
            .evidence_facts()
            .expect("contradiction evidence");

        assert_eq!(facts.next_cursor, None);
    }

    #[test]
    fn denied_message_search_is_terminal_not_empty_evidence() {
        let result = RetainedSurfaceResultV1::MessageSearch(message_search_result(
            RetainedOutcomeStatusV1::Denied,
            None,
            None,
        ));
        assert_eq!(
            result.evidence_facts(),
            Err(RetainedSurfaceEvidenceTerminalV1::Denied)
        );
    }

    #[test]
    fn message_search_uses_actual_results_when_count_is_absent() {
        let result = message_search_result(RetainedOutcomeStatusV1::Ok, None, Some(Vec::new()));
        let facts = RetainedSurfaceResultV1::MessageSearch(result)
            .evidence_facts()
            .expect("empty result vector proves zero returned items");
        assert_eq!(facts.returned, 0);
        assert_eq!(facts.unattributed_omitted, None);
    }

    #[test]
    fn message_search_rejects_inconsistent_reported_count() {
        let result = message_search_result(RetainedOutcomeStatusV1::Ok, Some(1), Some(Vec::new()));
        assert_eq!(
            RetainedSurfaceResultV1::MessageSearch(result).evidence_facts(),
            Err(RetainedSurfaceEvidenceTerminalV1::InvalidOutput)
        );
    }

    #[test]
    fn temporal_coverage_overflow_is_invalid_output() {
        let coverage = super::super::TemporalCoverageV1 {
            visible: u64::MAX,
            hidden: 1,
            unknown: 0,
            redacted: 0,
        };
        assert_eq!(
            temporal_visited(&coverage),
            Err(RetainedSurfaceEvidenceTerminalV1::InvalidOutput)
        );
    }

    #[test]
    fn lcm_retrieval_preserves_exact_partial_state() {
        let mut facts = RetainedSurfaceEvidenceFactsV1::unknown(EvidenceDomain::Temporal, 1)
            .expect("bounded count");
        facts
            .apply_lcm_retrieval(&LcmRetrievalOutcomeV1::Partial {
                freshness: TemporalFreshnessV1::Stored { generation_lag: 2 },
                omitted: 3,
            })
            .expect("consistent retrieval proof");
        assert_eq!(facts.completeness, CoverageCompleteness::Partial);
        assert_eq!(facts.freshness, FreshnessState::Stale);
        assert_eq!(facts.unattributed_omitted, Some(3));
    }

    #[test]
    fn positive_unattributed_omission_downgrades_coverage_without_inventing_reason() {
        let mut facts = RetainedSurfaceEvidenceFactsV1::unknown(EvidenceDomain::Temporal, 1)
            .expect("bounded count");
        facts.apply_unattributed_omitted(Some(2));
        assert_eq!(facts.completeness, CoverageCompleteness::Partial);
        assert!(facts.omissions.is_empty());
        assert_eq!(facts.unattributed_omitted, Some(2));
    }
}
