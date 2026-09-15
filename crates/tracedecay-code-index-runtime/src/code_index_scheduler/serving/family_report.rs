use std::path::{Component, Path};

use tracedecay_code_index::clones::{
    CloneExactKeyV1, CloneNormalizationClassV1, CodeIndexCloneBodyV1,
};
use tracedecay_code_index::production::CodeIndexExecutionControlV1;
use tracedecay_contracts::retrieval::{
    RedundancyCoverageV1, RedundancyFamilyV1, RedundancyPartialReasonV1, RedundancyRankingV1,
    RedundancyResultV1, SimilarFamilyV1, SimilarMatchClassV1, SimilarOccurrenceV1,
};
use tracedecay_query::code_search::CodeIndexRedundancyQueryV1;
use tracedecay_query::retrieval::lexical::{CloneArtifactCursorV1, CloneExactArtifactMemberV1};

use super::ProductionCodeIndexQueryOwnersV1;
use crate::query::retrieval::ports::RetrievalPortError;

impl ProductionCodeIndexQueryOwnersV1 {
    pub(crate) fn redundancy(
        &self,
        request: &CodeIndexRedundancyQueryV1,
        control: &dyn CodeIndexExecutionControlV1,
    ) -> Result<RedundancyResultV1, RetrievalPortError> {
        let family_page_limit = request.family_limit.min(request.work_limit / 3).max(1);
        let page = self
            .hydration
            .clone_exact_family_page(
                &request.project_id,
                &request.repository_id,
                &request.match_classes,
                request.path.as_deref(),
                request.include_generated_paths,
                request.cursor.as_deref(),
                family_page_limit,
                control,
            )
            .map_err(|error| RetrievalPortError::AuthorityUnavailable(error.to_string()))?;
        let page_continuation = page.next_cursor.clone();
        let mut families = Vec::with_capacity(page.families.len());
        let mut examined_families = 0usize;
        let mut examined_members = 0usize;
        let mut work_spent = 0usize;
        let mut work_exhausted = false;
        let mut report_continuation = request.cursor.clone();
        for candidate in page.families {
            if work_spent.saturating_add(3) > request.work_limit {
                work_exhausted = true;
                break;
            }
            examined_families = examined_families.saturating_add(1);
            work_spent = work_spent.saturating_add(1);
            let source = self
                .hydration
                .clone_body(&candidate.representative)
                .map_err(|error| RetrievalPortError::AuthorityUnavailable(error.to_string()))?
                .ok_or_else(|| {
                    RetrievalPortError::AuthorityUnavailable(
                        "clone family representative is unavailable".to_owned(),
                    )
                })?;
            if source.occurrence.project_id != request.project_id
                || source.occurrence.repository_id != request.repository_id
            {
                return Err(RetrievalPortError::AuthorityUnavailable(
                    "clone family representative is outside the authorized repository".to_owned(),
                ));
            }
            work_spent = work_spent.saturating_add(1);
            examined_members = examined_members.saturating_add(1);
            let remaining_work = request.work_limit.saturating_sub(work_spent);
            let read = self.verified_redundancy_members(
                &source,
                &candidate.key,
                request.path.as_deref(),
                request.include_generated_paths,
                request.member_limit.saturating_sub(1),
                remaining_work,
                control,
            )?;
            work_spent = work_spent.saturating_add(read.work_spent);
            examined_members = examined_members.saturating_add(read.work_spent);
            let mut members = Vec::with_capacity(read.members.len().saturating_add(1));
            members.push(similar_occurrence(&source.occurrence));
            members.extend(
                read.members
                    .iter()
                    .map(|member| similar_occurrence(&member.occurrence)),
            );
            work_exhausted |= read.work_exhausted;
            report_continuation = Some(candidate.continuation);
            let match_class = match candidate.key.class {
                CloneNormalizationClassV1::Conservative => SimilarMatchClassV1::ConservativeExact,
                CloneNormalizationClassV1::Rename => SimilarMatchClassV1::RenameNormalizedExact,
            };
            let next_cursor = read
                .next_cursor
                .as_ref()
                .map(CloneArtifactCursorV1::encode)
                .transpose()
                .map_err(|error| RetrievalPortError::AuthorityUnavailable(error.to_string()))?;
            let complete = read.complete && members.len() == candidate.member_count;
            families.push(RedundancyFamilyV1 {
                family: SimilarFamilyV1 {
                    match_class,
                    normalization_revision: candidate.key.normalization_revision,
                    family_digest: candidate.key.digest,
                    representative_payload_digest: source.payload.payload_digest,
                    member_count: members.len(),
                    members,
                    complete,
                    next_cursor,
                },
                total_member_count: candidate.member_count,
                reviewable_source_bytes: candidate.reviewable_source_bytes,
            });
            if work_exhausted {
                break;
            }
        }
        let source_generation = self.hydration.metadata().generation.clone();
        let (coverage, next_cursor) = redundancy_coverage(
            work_exhausted,
            family_page_limit < request.family_limit,
            page_continuation,
            report_continuation,
            examined_families,
            examined_members,
        );
        Ok(RedundancyResultV1 {
            source_generation,
            ranked_by: RedundancyRankingV1::ReviewableSourceBytes,
            families,
            coverage,
            next_cursor,
        })
    }

    fn verified_redundancy_members(
        &self,
        source: &CodeIndexCloneBodyV1,
        key: &CloneExactKeyV1,
        path: Option<&str>,
        include_generated_paths: bool,
        result_limit: usize,
        work_limit: usize,
        control: &dyn CodeIndexExecutionControlV1,
    ) -> Result<RedundancyMemberReadV1, RetrievalPortError> {
        let mut members = Vec::new();
        let mut cursor = None;
        let mut work_spent = 0usize;
        loop {
            if members.len() >= result_limit {
                return Ok(RedundancyMemberReadV1 {
                    members,
                    complete: false,
                    next_cursor: cursor,
                    work_spent,
                    work_exhausted: false,
                });
            }
            if work_spent >= work_limit {
                return Ok(RedundancyMemberReadV1 {
                    members,
                    complete: false,
                    next_cursor: cursor,
                    work_spent,
                    work_exhausted: true,
                });
            }
            let page_limit = result_limit
                .saturating_sub(members.len())
                .min(work_limit.saturating_sub(work_spent));
            let page = self
                .hydration
                .clone_exact_page(
                    &source.occurrence,
                    key,
                    cursor.as_ref(),
                    page_limit,
                    control,
                )
                .map_err(|error| RetrievalPortError::AuthorityUnavailable(error.to_string()))?;
            work_spent = work_spent.saturating_add(page.members.len());
            for member in page.members {
                if report_path_matches(&member.occurrence.path, path, include_generated_paths)
                    && tracedecay_code_index::clones::verify_exact_clone_payload(
                        &source.payload,
                        &member.payload,
                        key,
                    )
                {
                    members.push(member);
                }
            }
            match page.next_cursor {
                Some(next) => cursor = Some(next),
                None => {
                    return Ok(RedundancyMemberReadV1 {
                        members,
                        complete: true,
                        next_cursor: None,
                        work_spent,
                        work_exhausted: false,
                    });
                }
            }
        }
    }
}

struct RedundancyMemberReadV1 {
    members: Vec<CloneExactArtifactMemberV1>,
    complete: bool,
    next_cursor: Option<CloneArtifactCursorV1>,
    work_spent: usize,
    work_exhausted: bool,
}

fn report_path_matches(path: &str, scope: Option<&str>, include_generated_paths: bool) -> bool {
    tracedecay_domain::repository_path_matches_scope(path, scope)
        && (include_generated_paths
            || !Path::new(path).components().any(|component| {
                matches!(
                    component,
                    Component::Normal(segment)
                        if segment
                            .to_str()
                            .is_some_and(tracedecay_domain::is_generated_dir_segment)
                )
            }))
}

fn similar_occurrence(
    occurrence: &tracedecay_code_index::clones::CloneBodyOccurrenceV1,
) -> SimilarOccurrenceV1 {
    SimilarOccurrenceV1 {
        project_id: occurrence.project_id.clone(),
        repository_id: occurrence.repository_id.clone(),
        worktree_id: occurrence.worktree_id.clone(),
        source_generation: occurrence.source_generation.clone(),
        snapshot_digest: occurrence.snapshot_digest.clone(),
        symbol_occurrence_id: occurrence.symbol_occurrence_id.clone(),
        path: occurrence.path.clone(),
        body_span: occurrence.body_span,
    }
}

fn redundancy_coverage(
    work_exhausted: bool,
    family_budget_limited: bool,
    page_continuation: Option<String>,
    report_continuation: Option<String>,
    examined_families: usize,
    examined_members: usize,
) -> (RedundancyCoverageV1, Option<String>) {
    if work_exhausted || (family_budget_limited && page_continuation.is_some()) {
        (
            RedundancyCoverageV1::Partial {
                reason: RedundancyPartialReasonV1::WorkLimit,
                examined_families,
                examined_members,
            },
            report_continuation,
        )
    } else if page_continuation.is_some() {
        (
            RedundancyCoverageV1::Partial {
                reason: RedundancyPartialReasonV1::FamilyLimit,
                examined_families,
                examined_members,
            },
            page_continuation,
        )
    } else {
        (
            RedundancyCoverageV1::Complete {
                examined_families,
                examined_members,
            },
            None,
        )
    }
}
