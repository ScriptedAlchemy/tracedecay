use std::path::{Component, Path};

use tracedecay_code_index::clones::{CloneExactKeyV1, CodeIndexCloneBodyV1};
use tracedecay_code_index::production::CodeIndexExecutionControlV1;
use tracedecay_query::code_search::{
    CodeIndexRedundancyCompletedV1, CodeIndexRedundancyFamilyV1, CodeIndexRedundancyOutcomeV1,
    CodeIndexRedundancyPartialReasonV1, CodeIndexRedundancyPartialV1, CodeIndexRedundancyQueryV1,
};
use tracedecay_query::retrieval::lexical::{
    CloneArtifactCursorV1, CloneExactArtifactMemberV1,
};

use super::ProductionCodeIndexQueryOwnersV1;
use crate::query::retrieval::ports::RetrievalPortError;

impl ProductionCodeIndexQueryOwnersV1 {
    pub(crate) fn redundancy(
        &self,
        request: &CodeIndexRedundancyQueryV1,
        control: &dyn CodeIndexExecutionControlV1,
    ) -> Result<CodeIndexRedundancyOutcomeV1, RetrievalPortError> {
        let family_page_limit = request.family_limit.min(request.work_limit / 3).max(1);
        let page = self
            .hydration
            .clone_exact_family_page(
                &request.project_id,
                &request.repository_id,
                &request.match_classes,
                request.path.as_deref(),
                request.include_generated_paths,
                request.cursor.as_ref(),
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
            members.push(CloneExactArtifactMemberV1 {
                payload: source.payload.clone(),
                occurrence: source.occurrence,
            });
            members.extend(read.members);
            work_exhausted |= read.work_exhausted;
            report_continuation = Some(candidate.continuation);
            families.push(CodeIndexRedundancyFamilyV1 {
                key: candidate.key,
                representative_payload_digest: source.payload.payload_digest,
                complete: read.complete && members.len() == candidate.member_count,
                next_cursor: read.next_cursor,
                members,
                total_member_count: candidate.member_count,
                reviewable_source_bytes: candidate.reviewable_source_bytes,
            });
            if work_exhausted {
                break;
            }
        }
        let source_generation = self.hydration.metadata().generation.clone();
        if work_exhausted
            || (family_page_limit < request.family_limit && page_continuation.is_some())
        {
            Ok(CodeIndexRedundancyOutcomeV1::Partial(Box::new(
                CodeIndexRedundancyPartialV1 {
                    source_generation,
                    reason: CodeIndexRedundancyPartialReasonV1::WorkLimit,
                    families,
                    examined_families,
                    examined_members,
                    next_cursor: report_continuation,
                },
            )))
        } else if page_continuation.is_some() {
            Ok(CodeIndexRedundancyOutcomeV1::Partial(Box::new(
                CodeIndexRedundancyPartialV1 {
                    source_generation,
                    reason: CodeIndexRedundancyPartialReasonV1::FamilyLimit,
                    families,
                    examined_families,
                    examined_members,
                    next_cursor: page_continuation,
                },
            )))
        } else {
            Ok(CodeIndexRedundancyOutcomeV1::Complete(Box::new(
                CodeIndexRedundancyCompletedV1 {
                    source_generation,
                    families,
                    examined_families,
                    examined_members,
                    next_cursor: None,
                },
            )))
        }
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
