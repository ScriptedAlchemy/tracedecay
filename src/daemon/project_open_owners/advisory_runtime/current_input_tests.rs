//! Current-input coverage for the advisory/Scout producer path.
//!
//! Hook cycles and advisory cycles resolve their file census through
//! [`current_indexed_files`] and their feedback-cycle input through
//! [`current_feedback_lsp_input`] on every invocation. These tests prove the
//! census tracks the *current* sealed code-index generation (files sealed
//! after project open become eligible without a reopen, and a root without a
//! sealed generation stays the typed `None` state) and that the cycle input
//! re-pins the *current* configuration revision, so a settings PATCH landed
//! after project open remounts the producer path instead of rejecting every
//! cycle as `feedback-cycle-configuration-drift` until reopen.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use tempfile::TempDir;
use tracedecay_application::ResolvedScope;
use tracedecay_domain::configuration::{
    CONTEXT_SCOUT_SETTINGS_SETTING_KEY, ConfigurationGrantId, ConfigurationGrantReceiptId,
    ConfigurationIdempotencyKey, ConfigurationLayerIdV1, ConfigurationMutationEffectV1,
    ConfigurationMutationGrantReceiptV1, ConfigurationMutationOperationV1,
    ConfigurationMutationSinkV1, ConfigurationValueV1, ContextScoutConfigurationLimitsV1,
    ContextScoutConfigurationModeV1, ContextScoutConfigurationStateV1, ContextScoutSettingsV1,
    SettingKey,
};
use tracedecay_domain::{AccessPolicyDigest, ActorId, ProjectId, UtcMicros};
use tracedecay_lsp::analyzer::broker::DiagnosticBroker;
use tracedecay_lsp::{DiagnosticTrigger, FeedbackCycleRequest};
use tracedecay_usecases::configuration::{
    ConfigurationControlStore, ConfigurationMutationAuthority, DirectConfigurationMutation,
};

use super::super::DAEMON_REQUESTER;
use super::super::code_index_reads::project_code_graph_projection_read_port;
use super::{ProjectOpenCycleInputsV1, current_feedback_lsp_input, current_indexed_files};
use crate::daemon::code_index_scheduler::CodeIndexSchedulerRegistryV1;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn census_resolves_files_from_the_current_indexed_generation() {
    let root = TempDir::new().expect("fixture root");
    git(root.path(), &["init", "-q", "-b", "main"]);
    git(root.path(), &["config", "user.name", "TraceDecay Test"]);
    git(
        root.path(),
        &["config", "user.email", "tracedecay@example.invalid"],
    );
    write(
        root.path(),
        "src/lib.rs",
        b"pub fn census_anchor() -> u32 { 7 }\n",
    );
    git(root.path(), &["add", "."]);
    git(root.path(), &["commit", "-qm", "fixture"]);

    let store = TempDir::new().expect("store root");
    let registry = CodeIndexSchedulerRegistryV1::new(1);
    registry
        .mount_worktree(
            ProjectId::new("project.advisory-census").expect("project id"),
            root.path(),
            store.path().to_path_buf(),
            None,
        )
        .await
        .expect("mount code-index scheduler");
    let scope = wait_for_ready_scope(&registry, root.path()).await;

    assert!(
        current_indexed_files(&registry, store.path(), &scope)
            .await
            .is_none(),
        "a root without a sealed generation must stay the typed None state"
    );

    let baseline = current_indexed_files(&registry, root.path(), &scope)
        .await
        .expect("sealed generation census");
    assert!(
        baseline.iter().any(|path| path == "src/lib.rs"),
        "the sealed census must carry the indexed file: {baseline:?}"
    );
    assert!(
        !baseline.iter().any(|path| path == "src/later.rs"),
        "the baseline census must not anticipate unindexed files"
    );

    // A file sealed by a *later* generation: the census consumed by the next
    // hook or advisory cycle must include it without a project reopen.
    write(
        root.path(),
        "src/later.rs",
        b"pub fn census_late_arrival() -> u32 { 9 }\n",
    );
    git(root.path(), &["add", "."]);
    assert!(
        registry.request_authoritative_reconcile(root.path()).await,
        "the mounted root must accept a reconcile request"
    );
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if let Some(census) = current_indexed_files(&registry, root.path(), &scope).await
                && census.iter().any(|path| path == "src/later.rs")
            {
                assert!(
                    census.iter().any(|path| path == "src/lib.rs"),
                    "the newer generation census must retain untouched files: {census:?}"
                );
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("a later sealed generation's files must enter the census without a reopen");

    registry.shutdown().await;
}

/// The user enables the Context Scout checkbox after the project is already
/// open: the PATCH commits a new configuration revision. A cycle input pinned
/// at the project-open revision rejects every later cycle as
/// `feedback-cycle-configuration-drift`; the producer path must instead
/// resolve its input from the current revision on each cycle, so the next
/// cycle after the PATCH remounts and proceeds.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cycle_input_re_pins_the_current_configuration_revision_after_a_patch() {
    let _profile = crate::config::PinnedUserDataDir::new();
    let root = TempDir::new().expect("fixture root");
    git(root.path(), &["init", "-q", "-b", "main"]);
    git(root.path(), &["config", "user.name", "TraceDecay Test"]);
    git(
        root.path(),
        &["config", "user.email", "tracedecay@example.invalid"],
    );
    write(
        root.path(),
        "src/lib.rs",
        b"pub fn remount_anchor() -> u32 { 7 }\n",
    );
    git(root.path(), &["add", "."]);
    git(root.path(), &["commit", "-qm", "fixture"]);

    let graph = Arc::new(
        crate::tracedecay::TraceDecay::open(root.path())
            .await
            .expect("open project graph"),
    );
    let project_id = graph
        .configuration_runtime()
        .configuration_target()
        .project_id
        .clone();
    let project_root = root.path().canonicalize().expect("canonical fixture root");

    let store = TempDir::new().expect("store root");
    let registry = CodeIndexSchedulerRegistryV1::new(1);
    registry
        .mount_worktree(
            project_id.clone(),
            root.path(),
            store.path().to_path_buf(),
            None,
        )
        .await
        .expect("mount code-index scheduler");
    let scope = wait_for_ready_scope(&registry, root.path()).await;

    let session_db = graph
        .store_runtime_registry()
        .project_sessions(project_id.clone(), [project_root.clone()])
        .await
        .expect("project sessions lease");
    let inputs = ProjectOpenCycleInputsV1 {
        graph: Arc::clone(&graph),
        project_root: project_root.clone(),
        scope: scope.clone(),
        code_index_schedulers: registry.clone(),
        session_db,
        code_graph: project_code_graph_projection_read_port(
            registry.clone(),
            project_root.clone(),
            scope.clone(),
        ),
        requester: ActorId::new(DAEMON_REQUESTER).expect("daemon requester"),
        diagnostic_broker: Arc::new(tokio::sync::Mutex::new(DiagnosticBroker::new_for_test(
            project_root.clone(),
            Vec::new(),
        ))),
    };
    let census = current_indexed_files(&registry, root.path(), &scope)
        .await
        .expect("sealed generation census");
    let request = FeedbackCycleRequest {
        root_uri: url::Url::from_directory_path(&project_root)
            .expect("root URI")
            .into(),
        document_uri: url::Url::from_file_path(project_root.join("src/lib.rs"))
            .expect("document URI")
            .into(),
        trigger: DiagnosticTrigger::DocumentSave,
    };

    // The revision live at "project open".
    let opened = graph
        .configuration_runtime()
        .client()
        .current()
        .await
        .expect("project-open configuration");
    let project_open_input = current_feedback_lsp_input(&inputs, &census)
        .await
        .expect("cycle input for the project-open revision");
    project_open_input(request.clone())
        .await
        .expect("a cycle under the project-open revision must build its invocation");

    // The PATCH: enabling the Context Scout checkbox commits a new revision.
    let settings = ContextScoutSettingsV1 {
        schema_version: ContextScoutSettingsV1::SCHEMA_VERSION,
        state: ContextScoutConfigurationStateV1::Active,
        mode: ContextScoutConfigurationModeV1::Deterministic,
        limits: ContextScoutConfigurationLimitsV1::bounded_defaults(),
        model_path: None,
        model_id: None,
        model_timeout_secs: None,
    };
    settings.validate().expect("enabled Scout settings");
    let mutation = DirectConfigurationMutation::Set {
        layer: ConfigurationLayerIdV1::Project {
            project_id: project_id.clone(),
        },
        key: SettingKey::new(CONTEXT_SCOUT_SETTINGS_SETTING_KEY).expect("Scout setting key"),
        value: Box::new(ConfigurationValueV1::ContextScoutSettings(settings)),
    };
    let authority = ConfigurationMutationAuthority {
        receipt: ConfigurationMutationGrantReceiptV1::issue(
            ConfigurationGrantReceiptId::new("configuration.grant-receipt.scout-remount")
                .expect("grant receipt id"),
            ConfigurationGrantId::new("configuration.grant.scout-remount").expect("grant id"),
            ActorId::new("actor.scout-remount-test").expect("actor id"),
            ConfigurationMutationOperationV1::DirectMutation,
            mutation.target_scope_digest().expect("mutation scope"),
            opened.revision_id.clone(),
            1,
            AccessPolicyDigest::new(format!("sha256:{}", "a".repeat(64))).expect("policy digest"),
            ConfigurationMutationSinkV1::ConfigurationStore,
            ConfigurationMutationEffectV1::CommitConfigurationRevision,
            Some(
                ConfigurationIdempotencyKey::new("configuration.idempotency.scout-remount")
                    .expect("idempotency key"),
            ),
            UtcMicros(1),
            UtcMicros(100),
        )
        .expect("mutation grant receipt"),
    };
    let configuration_store = graph.configuration_runtime().configuration_store();
    ConfigurationControlStore::commit_direct(
        &configuration_store,
        &authority,
        &mutation,
        &opened.revision_id,
    )
    .await
    .expect("commit the Context Scout PATCH");
    let patched = graph
        .configuration_runtime()
        .client()
        .current()
        .await
        .expect("patched configuration");
    assert_ne!(
        patched.revision_id, opened.revision_id,
        "the PATCH must commit a new configuration revision"
    );

    // The project-open-era input is now the exact hazard the remount removes.
    let Err(stale) = project_open_input(request.clone()).await else {
        panic!("a project-open-pinned input must reject the patched revision");
    };
    assert_eq!(
        stale.class(),
        "feedback-cycle-configuration-drift",
        "the stale input's rejection must be the typed drift state"
    );

    // The producer path resolves a fresh input per cycle: the next cycle
    // after the PATCH re-pins the current revision and proceeds.
    let remounted_input = current_feedback_lsp_input(&inputs, &census)
        .await
        .expect("cycle input for the patched revision");
    remounted_input(request)
        .await
        .expect("a cycle resolved after the PATCH must accept the current revision");

    registry.shutdown().await;
}

async fn wait_for_ready_scope(
    registry: &CodeIndexSchedulerRegistryV1,
    project_root: &Path,
) -> ResolvedScope {
    let scope = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(latest) = registry.latest_complete_fresh(project_root).await {
                let generation = latest.generation();
                let snapshot = generation.snapshot();
                break ResolvedScope::new(
                    generation.manifest().project_id.clone(),
                    snapshot.repository.clone(),
                    snapshot.worktree.clone().expect("worktree identity"),
                    snapshot.reference.clone(),
                )
                .expect("resolved scope");
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("initial sealed generation");
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if registry
                .latest_complete_ready_decoded_for_root_scope(project_root, &scope)
                .await
                .is_some()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("the sealed generation becomes ready and serving for its exact root");
    scope
}

fn git(root: &Path, arguments: &[&str]) {
    let status = std::process::Command::new(crate::git::git_program())
        .current_dir(root)
        .args(arguments)
        .status()
        .expect("run git fixture command");
    assert!(
        status.success(),
        "git fixture command failed: {arguments:?}"
    );
}

fn write(root: &Path, logical_path: &str, contents: &[u8]) {
    let path = root.join(logical_path);
    std::fs::create_dir_all(path.parent().expect("fixture parent")).expect("create fixture parent");
    std::fs::write(path, contents).expect("write fixture file");
}
