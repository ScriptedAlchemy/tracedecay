use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use tracedecay_domain::configuration::{ConfigurationRevisionId, UserProfileId};
use tracedecay_domain::{ActorId, FactOwnerV1, SessionId};
use tracedecay_global_db::tests::harness::RegisteredGlobalDbTestRuntime;
use tracedecay_policy::CurationApplyAuthorityV1;

use super::super::session_reflector::run_session_reflector_for_store;
use super::super::skill_writer::{SkillWriterStoreRuntime, run_skill_writer_for_store};
use super::super::*;
use crate::automation::backend::{AgentTaskBackend, AgentTaskRequest, AgentTaskResponse};
use crate::automation::config::{
    AutomationBackend, AutomationHostMode, AutomationTaskConfig, AutomationTaskSet,
};
use crate::automation::run_ledger::AutomationRunStatus;
use crate::db::{Database, DatabaseAuthority, TestDatabaseRuntimeMode};
use tracedecay_usecases::session::SessionTemporalQuery;

struct RecordingRetrieval {
    anchor_session_id: SessionId,
    calls: AtomicUsize,
}

impl RecordingRetrieval {
    fn new() -> Self {
        Self {
            anchor_session_id: SessionId::new("session.early-gate").expect("session id"),
            calls: AtomicUsize::new(0),
        }
    }
}

impl AutomationSessionRetrieval for RecordingRetrieval {
    fn anchor_session_id(&self) -> &SessionId {
        &self.anchor_session_id
    }

    fn retrieve(&self, _query: SessionTemporalQuery) -> AutomationSessionRetrievalFuture<'_> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { AutomationTemporalRetrieval::CompleteZero })
    }
}

struct RecordingBackend {
    calls: AtomicUsize,
}

impl RecordingBackend {
    fn new() -> Self {
        Self {
            calls: AtomicUsize::new(0),
        }
    }
}

impl AgentTaskBackend for RecordingBackend {
    fn run_task(
        &self,
        _request: &AgentTaskRequest,
    ) -> std::result::Result<AgentTaskResponse, tracedecay_automation::backend::AgentTaskError>
    {
        self.calls.fetch_add(1, Ordering::SeqCst);
        panic!("scheduled-disabled automation must not invoke its backend")
    }
}

fn scheduled_disabled_config() -> AutomationConfig {
    AutomationConfig {
        enabled: false,
        backend: AutomationBackend::CodexAppServer,
        host_mode: AutomationHostMode::Standalone,
        tasks: AutomationTaskSet {
            session_reflector: AutomationTaskConfig {
                enabled: false,
                ..AutomationTaskConfig::default()
            },
            skill_writer: AutomationTaskConfig {
                enabled: false,
                ..AutomationTaskConfig::default()
            },
            ..AutomationTaskSet::default()
        },
        ..AutomationConfig::default()
    }
}

fn curation_authority() -> CurationApplyAuthorityV1 {
    CurationApplyAuthorityV1 {
        actor_id: ActorId::new("automation:early-gate-test").expect("actor id"),
        project_id: None,
        profile_id: UserProfileId::new("profile.early-gate-test").expect("profile id"),
        configuration_revision_id: ConfigurationRevisionId::new("config.early-gate-test.v1")
            .expect("configuration revision"),
    }
}

fn run_control() -> AutomationRunControl {
    AutomationRunControl::from_interrupted(Arc::new(|| false))
}

async fn memory_database(root: &std::path::Path) -> Database {
    let path = root.join("memory.db");
    let authority = DatabaseAuthority::acquire_test(&path, "automation early gate memory fixture")
        .expect("memory authority");
    Database::publish_test_runtime(&path, &authority, TestDatabaseRuntimeMode::Initialize)
        .await
        .expect("memory database")
        .0
}

#[tokio::test]
async fn scheduled_disabled_session_reflector_reads_no_evidence_and_runs_no_backend() {
    let directory = tempfile::tempdir().expect("session reflector early gate directory");
    let sessions = RegisteredGlobalDbTestRuntime::profile(directory.path())
        .await
        .expect("registered session runtime");
    let database = memory_database(directory.path()).await;
    let memory = MemoryApplication::new(FactOwnerV1::Profile, DatabaseFactStore::new(&database))
        .expect("memory application");
    let retrieval = RecordingRetrieval::new();
    let backend = RecordingBackend::new();
    let config = scheduled_disabled_config();
    let control = run_control();
    let authority = curation_authority();

    let run = run_session_reflector_for_store(
        directory.path().join("automation"),
        sessions.profile_database_arc(),
        &retrieval,
        &memory,
        &config,
        &control,
        &authority,
        &backend,
        SessionReflectorAutomationOptions {
            trigger: AutomationTrigger::Scheduler,
            run_id: Some("run.early-gate.session-reflector".to_owned()),
            ..SessionReflectorAutomationOptions::default()
        },
    )
    .await
    .expect("scheduled-disabled reflector skip");

    assert_eq!(run.ledger_record.status, AutomationRunStatus::Skipped);
    assert_eq!(run.report["reason"], "automation_disabled");
    assert_eq!(retrieval.calls.load(Ordering::SeqCst), 0);
    assert_eq!(backend.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn scheduled_disabled_skill_writer_reads_no_evidence_and_runs_no_backend() {
    let directory = tempfile::tempdir().expect("skill writer early gate directory");
    let sessions = RegisteredGlobalDbTestRuntime::profile(directory.path())
        .await
        .expect("registered session runtime");
    let retrieval = RecordingRetrieval::new();
    let backend = RecordingBackend::new();
    let config = scheduled_disabled_config();

    let run = run_skill_writer_for_store(
        SkillWriterStoreRuntime {
            dashboard_root: directory.path().join("automation"),
            sessions_db: sessions.profile_database_arc(),
            analytics_project_root: None,
            analytics_db: None,
            authority: curation_authority(),
        },
        &retrieval,
        &config,
        &backend,
        SkillWriterAutomationOptions {
            trigger: AutomationTrigger::Scheduler,
            run_id: Some("run.early-gate.skill-writer".to_owned()),
            profile_root: Some(directory.path().join("profile")),
            ..SkillWriterAutomationOptions::default()
        },
    )
    .await
    .expect("scheduled-disabled skill writer skip");

    assert_eq!(run.ledger_record.status, AutomationRunStatus::Skipped);
    assert_eq!(run.report["reason"], "automation_disabled");
    assert_eq!(retrieval.calls.load(Ordering::SeqCst), 0);
    assert_eq!(backend.calls.load(Ordering::SeqCst), 0);
}
