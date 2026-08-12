//! Canonical automation request projection.

use tracedecay_agent_hosts::automation::runner::{
    CombinedReviewAutomationOptions, SessionReflectorAutomationOptions,
    SkillWriterAutomationOptions,
};
use tracedecay_application::retained_surfaces::{
    AutomationRunRequestV1, AutomationTaskRequestV1, CombinedReviewRunInputV1, LcmGrepSortV1,
    LcmRoleV1, LcmSearchScopeV1, MemoryCuratorRunInputV1, SessionReflectorRunInputV1,
    SkillWriterRunInputV1, UserJobRunInputV1,
};
use tracedecay_domain::{RunId, UtcMicros};

use super::contract_error;
use crate::errors::Result;

pub(crate) fn memory_curator_run_request(
    run_id: &str,
    fact_review_limit: usize,
    min_confidence: f64,
) -> Result<AutomationRunRequestV1> {
    if !min_confidence.is_finite() || !(0.0..=1.0).contains(&min_confidence) {
        return Err(contract_error(
            "memory curator minimum confidence is outside the closed unit interval",
        ));
    }
    automation_run_request(
        run_id,
        AutomationTaskRequestV1::MemoryCurator(MemoryCuratorRunInputV1 {
            fact_review_limit: u32::try_from(fact_review_limit).map_err(contract_error)?,
            min_confidence_millionths: (min_confidence * 1_000_000.0).round() as u32,
        }),
    )
}

pub(crate) fn session_reflector_run_request(
    run_id: &str,
    options: &SessionReflectorAutomationOptions,
) -> Result<AutomationRunRequestV1> {
    automation_run_request(
        run_id,
        AutomationTaskRequestV1::SessionReflector(project_reflector_input(options)?),
    )
}

pub(crate) fn skill_writer_run_request(
    run_id: &str,
    options: &SkillWriterAutomationOptions,
) -> Result<AutomationRunRequestV1> {
    automation_run_request(
        run_id,
        AutomationTaskRequestV1::SkillWriter(project_skill_writer_input(options)?),
    )
}

pub(crate) fn combined_review_run_request(
    run_id: &str,
    options: &CombinedReviewAutomationOptions,
) -> Result<AutomationRunRequestV1> {
    automation_run_request(
        run_id,
        AutomationTaskRequestV1::CombinedReview(CombinedReviewRunInputV1 {
            session_reflector: project_reflector_input(&options.session_reflector)?,
            skill_writer: project_skill_writer_input(&options.skill_writer)?,
        }),
    )
}

pub(crate) fn user_job_run_request(run_id: &str, job_id: &str) -> Result<AutomationRunRequestV1> {
    automation_run_request(
        run_id,
        AutomationTaskRequestV1::UserJob(UserJobRunInputV1 {
            job_id: job_id.to_owned(),
        }),
    )
}

fn project_reflector_input(
    options: &SessionReflectorAutomationOptions,
) -> Result<SessionReflectorRunInputV1> {
    use tracedecay_agent_hosts::ports::session_evidence::{LcmGrepSort, LcmScope};

    Ok(SessionReflectorRunInputV1 {
        provider: options.provider.clone(),
        query: options.query.clone(),
        scope: match options.scope {
            LcmScope::Current => LcmSearchScopeV1::Current,
            LcmScope::Session => LcmSearchScopeV1::Session,
            LcmScope::All => LcmSearchScopeV1::All,
        },
        session_id: options.session_id.clone(),
        include_summaries: options.include_summaries,
        evidence_limit: u32::try_from(options.evidence_limit).map_err(contract_error)?,
        include_recent_sessions: options.include_recent_sessions,
        recent_sessions_limit: u32::try_from(options.recent_sessions_limit)
            .map_err(contract_error)?,
        sort: match options.sort {
            LcmGrepSort::Recency => LcmGrepSortV1::Recency,
            LcmGrepSort::Relevance => LcmGrepSortV1::Relevance,
            LcmGrepSort::Hybrid => LcmGrepSortV1::Hybrid,
        },
        source: options.source.clone(),
        role: options.role.as_deref().map(project_role).transpose()?,
        start_time: options.start_time.map(UtcMicros),
        end_time: options.end_time.map(UtcMicros),
    })
}

fn project_skill_writer_input(
    options: &SkillWriterAutomationOptions,
) -> Result<SkillWriterRunInputV1> {
    Ok(SkillWriterRunInputV1 {
        provider: options.provider.clone(),
        query: options.query.clone(),
        evidence_limit: u32::try_from(options.evidence_limit).map_err(contract_error)?,
        include_recent_sessions: options.include_recent_sessions,
        recent_sessions_limit: u32::try_from(options.recent_sessions_limit)
            .map_err(contract_error)?,
    })
}

fn project_role(role: &str) -> Result<LcmRoleV1> {
    match role {
        "system" => Ok(LcmRoleV1::System),
        "user" => Ok(LcmRoleV1::User),
        "assistant" => Ok(LcmRoleV1::Assistant),
        "tool" => Ok(LcmRoleV1::Tool),
        "unknown" => Ok(LcmRoleV1::Unknown),
        _ => Err(contract_error(format!(
            "session reflector role is not registered: {role}"
        ))),
    }
}

fn automation_run_request(
    run_id: &str,
    task: AutomationTaskRequestV1,
) -> Result<AutomationRunRequestV1> {
    let request = AutomationRunRequestV1 {
        run_id: RunId::new(run_id.to_owned()).map_err(contract_error)?,
        task,
    };
    if !request.validate() {
        return Err(contract_error(
            "automation run input is outside its registered bounds",
        ));
    }
    Ok(request)
}
