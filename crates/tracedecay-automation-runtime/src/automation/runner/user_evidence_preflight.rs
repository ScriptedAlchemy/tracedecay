use std::sync::Arc;

use crate::automation::backend::AgentTaskKind;
use crate::automation::config::AutomationConfig;
use crate::automation::lifecycle::{
    AgentTaskRunContext, AutomationRunControl, AutomationRunResult, task_skip_reason,
};
use crate::errors::{Result, TraceDecayError};
use crate::ports::project_runtime::ProfileRuntime;
use crate::store::memory::DatabaseFactStore;
use tracedecay_domain::FactOwnerV1;
use tracedecay_domain::configuration::ConfigurationRevisionId;
use tracedecay_usecases::memory::MemoryApplication;

use super::AutomationTaskIo;
use super::evidence::{
    SessionReflectorEvidenceOutcome, SkillWriterEvidenceOutcome, build_session_reflector_evidence,
    build_skill_writer_evidence,
};
use super::retrieval::AutomationSessionRetrieval;
use super::session_reflector::{
    SessionReflectorAutomationOptions, SessionReflectorAutomationRun,
    rejected_session_reflector_run, run_session_reflector_for_store,
};
use super::skill_writer::SkillWriterAutomationOptions;

pub(super) async fn preflight_user_session_reflector_evidence(
    retrieval: &dyn AutomationSessionRetrieval,
    config: &AutomationConfig,
    options: &SessionReflectorAutomationOptions,
) -> Result<Option<SessionReflectorEvidenceOutcome>> {
    if !options.trigger.is_on_demand()
        || task_skip_reason(config, AgentTaskKind::SessionReflector).is_some()
    {
        return Ok(None);
    }
    build_session_reflector_evidence(retrieval, options)
        .await
        .map(Some)
}

pub(crate) async fn run_user_session_reflector_with_backend_and_retrieval(
    profile_root: &std::path::Path,
    session_registry: Arc<dyn ProfileRuntime>,
    config: &AutomationConfig,
    run_control: &AutomationRunControl,
    configuration_revision_id: &ConfigurationRevisionId,
    io: AutomationTaskIo<'_>,
    options: SessionReflectorAutomationOptions,
) -> AutomationRunResult<SessionReflectorAutomationRun> {
    let AutomationTaskIo { backend, retrieval } = io;
    let authority = super::profile_curation_authority(
        session_registry.as_ref(),
        "automation:session-reflector",
        configuration_revision_id,
    )?;
    let sessions_db = session_registry.profile_sessions().await?;
    let prebuilt_evidence =
        match preflight_user_session_reflector_evidence(retrieval, config, &options).await? {
            Some(SessionReflectorEvidenceOutcome::Ready(bundle)) => Some(bundle),
            Some(SessionReflectorEvidenceOutcome::Skipped {
                reason,
                evidence_hash,
            }) => {
                let run = AgentTaskRunContext::new(
                    super::user_automation_root(profile_root),
                    sessions_db.clone(),
                    options.run_id.clone(),
                    "session_reflector",
                    options.trigger,
                    config,
                    AgentTaskKind::SessionReflector,
                );
                return Ok(rejected_session_reflector_run(
                    &run,
                    config,
                    reason,
                    evidence_hash,
                ));
            }
            None => None,
        };
    let memory_db = session_registry.open_user_memory_db().await?;
    let memory = MemoryApplication::new(FactOwnerV1::Profile, DatabaseFactStore::new(&memory_db))
        .map_err(|error| TraceDecayError::Config {
        message: format!(
            "could not initialize profile session reflector memory authority: {error}"
        ),
    })?;
    run_session_reflector_for_store(
        super::user_automation_root(profile_root),
        sessions_db,
        retrieval,
        &memory,
        config,
        run_control,
        &authority,
        backend,
        options,
        prebuilt_evidence,
    )
    .await
}

pub(super) async fn preflight_user_skill_writer_evidence(
    retrieval: &dyn AutomationSessionRetrieval,
    config: &AutomationConfig,
    options: SkillWriterAutomationOptions,
) -> Result<Option<SkillWriterEvidenceOutcome>> {
    if !options.trigger.is_on_demand()
        || task_skip_reason(config, AgentTaskKind::SkillWriter).is_some()
    {
        return Ok(None);
    }
    build_skill_writer_evidence(retrieval, None, None, options)
        .await
        .map(Some)
}
