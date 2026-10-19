use std::collections::BTreeMap;
use std::sync::Arc;

use tracedecay_configuration::config::scope_control::daemon_owned_project_source_binding;
use tracedecay_domain::configuration::{
    CONTEXT_SCOUT_SETTINGS_SETTING_KEY, ConfigurationLayerIdV1, ConfigurationRevisionId,
    ConfigurationValueV1, ContextScoutConfigurationStateV1, ContextScoutIntentSettingsV1,
    ContextScoutSettingsV1, DecisionEngineSettingsV1, IntentWindowSettingsV1,
    SOURCE_BINDINGS_SETTING_KEY, SettingKey,
};
use tracedecay_domain::{NativeHostIdentityV1, ProjectId};
use tracedecay_global_db::configuration::GlobalDbConfigurationControlStore;
use tracedecay_global_db::configuration::registry::ConfigurationRegistry;
use tracedecay_global_db::configuration::resolver::{ConfigurationLayerV1, resolve_configuration};
use tracedecay_hooks::{HookEventEnvelopeV2, HookEventV2, HookLifecyclePhaseV1, HookOrderingV1};
use tracedecay_project::project::TraceDecayOpenOptions;
use tracedecay_project::test_support::host_admission::HostAdmissionTestRuntimeV1;
use tracedecay_runtime_core::background_cpu::ProcessBackgroundCpuV1;
use tracedecay_sessions::admission::HostAdmissionScope;

use crate::handlers::hook_runtime::envelope::hook_now;
use tracedecay_global_db::RegisteredGlobalDb;
use tracedecay_hooks::NativeContextScoutLifecycleV1;
use tracedecay_project::project::TraceDecay;

pub(super) struct ToolDispatchFixture {
    _temporary: tempfile::TempDir,
    _runtime: Arc<HostAdmissionTestRuntimeV1>,
    pub(super) graph: TraceDecay,
    pub(super) sessions: tracedecay_global_db::RegisteredGlobalDbLeaseV1,
    pub(super) background_cpu: Arc<ProcessBackgroundCpuV1>,
    pub(super) envelope: HookEventEnvelopeV2,
    pub(super) lifecycle: NativeContextScoutLifecycleV1,
}

pub(super) async fn open() -> ToolDispatchFixture {
    let temporary = tempfile::tempdir().unwrap();
    let profile = temporary.path().join("profile");
    let root = temporary.path().join("project");
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/lib.rs"), "pub fn boundary() {}\n").unwrap();
    let project = ProjectId::new("project.tool-dispatch-cutoff").unwrap();
    let runtime = Arc::new(
        HostAdmissionTestRuntimeV1::project(&profile, &root, project.clone())
            .await
            .unwrap(),
    );
    let sessions = runtime
        .registered_database_arc(HostAdmissionScope::Project)
        .unwrap();
    initialize_configuration(&sessions, &project).await;
    let graph = runtime
        .initialize_project_graph_for_test(
            &root,
            TraceDecayOpenOptions {
                profile_root: Some(profile.clone()),
                global_db_path: None,
            },
        )
        .await
        .unwrap();
    let hook_runtime = tracedecay_project::runtime_ports::hook_runtime(
        tracedecay_runtime_core::config::ProfileRoot::new(profile),
    )
    .unwrap();
    let worktree = tracedecay_agent_hosts::hooks::hook_worktree_id_for_layout(
        &hook_runtime,
        graph.hook_store_layout(),
    )
    .unwrap();
    let binding = tracedecay_hooks::published_hook_scope_binding(
        &graph.hook_store_layout().data_root,
        worktree,
        NativeHostIdentityV1::ClaudeCode,
        hook_now(),
    )
    .unwrap();
    let scope = (hook_runtime.scope_resolver)(&root, &project).unwrap();
    assert_eq!(tracedecay_daemon_service::context_scout_lifecycle::register_context_scout_lifecycle_authority(binding.project_id, binding.worktree_id, scope.project_id, scope.worktree_id, &sessions), tracedecay_daemon_service::context_scout_lifecycle::AuthorityRegistrationV1::Registered);
    let lifecycle =
        NativeContextScoutLifecycleV1::new("session.tool-dispatch", "call.tool-dispatch", [73; 16])
            .unwrap();
    let envelope = HookEventEnvelopeV2 {
        schema_version: tracedecay_hooks::HOOK_EVENT_SCHEMA_VERSION,
        event_id: lifecycle.event_id,
        producer: binding.host,
        protected_session_id: tracedecay_agent_hosts::hooks::protected_native_session_id(
            lifecycle.session_id.as_str(),
        ),
        project_id: binding.project_id,
        repository_id: binding.repository_id,
        worktree_id: binding.worktree_id,
        worktree_epoch: binding.worktree_epoch,
        binding_token: binding.binding_token,
        ordering: HookOrderingV1::Unknown,
        observed_at: hook_now(),
        event: HookEventV2::ToolLifecycle {
            tool_id: [74; 16],
            phase: HookLifecyclePhaseV1::Completed,
            effect_receipt_id: None,
        },
    };
    let background_cpu = runtime.background_cpu();
    ToolDispatchFixture {
        _temporary: temporary,
        _runtime: runtime,
        graph,
        sessions,
        background_cpu,
        envelope,
        lifecycle,
    }
}

async fn initialize_configuration(sessions: &RegisteredGlobalDb, project: &ProjectId) {
    let intent = ContextScoutIntentSettingsV1 {
        enabled: true,
        engine: DecisionEngineSettingsV1 {
            base_url: "http://127.0.0.1:9".to_owned(),
            model: Some("kev-4b".to_owned()),
            timeout_ms: 1_000,
            max_concurrent: 1,
            max_state_bytes: 8_192,
        },
        window: IntentWindowSettingsV1 {
            max_user_turns: 3,
            max_messages: 12,
            max_bytes: 8_192,
            max_age_secs: 600,
            freshen_budget_ms: 0,
        },
        questions: ContextScoutIntentSettingsV1::reference_questions(),
        action_fact_score_permille: 700,
        ttl_secs: 600,
        min_delivery_gap_secs: 30,
        prompt_lookup_deadline_ms: 200,
    };
    let revision = ConfigurationRevisionId::new("configuration.tool-dispatch").unwrap();
    let resolution = resolve_configuration(
        &ConfigurationRegistry::core().unwrap(),
        &[ConfigurationLayerV1 {
            layer: ConfigurationLayerIdV1::Project {
                project_id: project.clone(),
            },
            revision_id: revision.clone(),
            entries: BTreeMap::from([
                (
                    SettingKey::new(SOURCE_BINDINGS_SETTING_KEY).unwrap(),
                    ConfigurationValueV1::SourceBindings(vec![
                        daemon_owned_project_source_binding(project).unwrap(),
                    ]),
                ),
                (
                    SettingKey::new(CONTEXT_SCOUT_SETTINGS_SETTING_KEY).unwrap(),
                    ConfigurationValueV1::ContextScoutSettings(ContextScoutSettingsV1 {
                        state: ContextScoutConfigurationStateV1::Active,
                        intent: Some(intent),
                        ..ContextScoutSettingsV1::disabled()
                    }),
                ),
            ]),
        }],
    )
    .unwrap();
    GlobalDbConfigurationControlStore::new_registered(sessions)
        .initialize_canonical(&revision, &resolution, hook_now())
        .await
        .unwrap();
}
