//! Typed SDK proof for provider-qualified Work-to-TaskSession availability.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tracedecay_application::{
    VerifiedWorkGraphVersionV1, WorkAttemptReceiptV1, WorkEvidenceContinuationV1,
    WorkEvidenceExpansionSelectorV1, WorkEvidenceOmissionReasonV1, WorkEvidenceRetrievalV1,
    WorkEvidenceRetrieveRequestV1, WorkEvidenceSourceV1, WorkProductSelectionScopeV1,
    WorkTaskSessionEvidenceV1, WorkTaskSessionHydrationStateV1,
};
use tracedecay_domain::configuration::{
    ConfigurationIdempotencyKey, ConfigurationLayerIdV1, ConfigurationValueV1,
    SEMANTIC_RUNTIME_SETTING_KEY, SettingKey,
};
use tracedecay_domain::{
    AdmittedEmbeddingProjectionKeyV1, CalibrationProfileId, ComponentRevision, ManifestDigest,
    ProjectId, SemanticSearchIndexProfileV1, TaskId, TemporalModeV1, UtcMicros,
    VectorGenerationIdV1, WorkAttemptIdentityV1, canonical_sha256,
};
use tracedecay_global_db::configuration::semantic::{SemanticConfig, SemanticProfileSelection};
use tracedecay_query::retrieval::semantic::SemanticCalibrationProfileV1;
use tracedecay_sdk::client::Client;
use tracedecay_sdk::operations::{
    ApplicationConfigurationObservedState, ApplicationConfigurationSet, WorkRetrieveEvidence,
};
use tracedecay_semantic::{
    DEFAULT_FASTEMBED_MODEL_ID, LoadedSemanticArtifactV1, SemanticModelLifecycleOwnerV1,
    SemanticModelLifecycleStateV1, SemanticResourceCeilings,
};
use tracedecay_usecases::config::retrieval::{
    RetrievalCompatibilityPinsV1, SemanticCompatibilityPinsV1, SemanticResourceRequirementV1,
};
use tracedecay_usecases::retention::code_index_generations::{
    DurablePublicationPointerV1, scoped_code_index_store_root,
};
use tracedecay_usecases::semantic_runtime::{
    SemanticEvaluationDiversityCandidateV1, SemanticEvaluationFusionCandidateV1,
    SemanticEvaluationProfileCandidateV1, SemanticFallbackReasonV1, SemanticRuntimeStateV1,
    SemanticRuntimeStatusV1,
};

use super::{
    PROVIDER_SESSION_ID, common,
    daemon_fixture::{
        sdk_client, spawn_project_daemon, wait_for_application_mount, wait_for_work_mount,
    },
    now,
};

const EVALUATED_PROFILE_ID: &str = "hybrid-conservative";
const JOURNEY_MODEL_LOAD_DEADLINE_MS: u64 = 180_000;

pub(super) struct InstalledSemanticFixture {
    artifact_digest: String,
    artifact_path: PathBuf,
}

pub(super) fn seed_semantic_source(project: &Path) {
    std::fs::create_dir_all(project.join("src")).expect("project source directory");
    std::fs::write(
        project.join("src/lib.rs"),
        "pub fn advanced_workflow_semantic_probe() -> &'static str { \"provider session\" }\n",
    )
    .expect("semantic fixture source");
}

pub(super) fn install_semantic_fixture(home: &Path) -> InstalledSemanticFixture {
    let fixture_root = std::env::var_os("TRACEDECAY_DISTRIBUTION_FASTEMBED_FIXTURE")
        .map(PathBuf::from)
        .filter(|path| path.is_dir())
        .expect(
            "advanced Work TaskSession journey requires the byte-pinned FastEmbed fixture in \
             TRACEDECAY_DISTRIBUTION_FASTEMBED_FIXTURE",
        );
    let profile = home.join(".tracedecay");
    tracedecay::storage::PrivateStoreIo::create_dir_all(&profile)
        .expect("private semantic fixture profile");
    let lifecycle_root = tracedecay_semantic::default_lifecycle_root_in(&profile);
    let owner = SemanticModelLifecycleOwnerV1::open_default(&lifecycle_root)
        .expect("isolated semantic lifecycle owner");
    seed_distribution_fixture(&lifecycle_root, &fixture_root, &owner);
    owner
        .select_model(Some(DEFAULT_FASTEMBED_MODEL_ID), true)
        .expect("select production semantic model");
    owner
        .acquire_blocking_for_tests()
        .expect("install verified distribution fixture");
    match owner.status().state.expect("installed model state") {
        SemanticModelLifecycleStateV1::Installed {
            artifact_digest,
            install_path,
            ..
        }
        | SemanticModelLifecycleStateV1::Ready {
            artifact_digest,
            install_path,
            ..
        } => InstalledSemanticFixture {
            artifact_digest,
            artifact_path: install_path,
        },
        state => panic!("expected installed production model, got {state:?}"),
    }
}

fn seed_distribution_fixture(
    lifecycle_root: &Path,
    fixture_root: &Path,
    owner: &SemanticModelLifecycleOwnerV1,
) {
    let model = owner
        .catalog()
        .get(DEFAULT_FASTEMBED_MODEL_ID)
        .expect("production catalog contains default model");
    let repository = format!("models--{}", model.model_code.replace('/', "--"));
    let repository_root = lifecycle_root.join("hf-hub-cache").join(repository);
    let snapshot = repository_root
        .join("snapshots")
        .join(&model.source.revision);
    for member in model.members.values() {
        let destination = snapshot.join(&member.upstream_path);
        std::fs::create_dir_all(destination.parent().expect("member parent"))
            .expect("create cached member parent");
        std::fs::copy(fixture_root.join(&member.path), &destination)
            .expect("copy byte-exact distribution fixture member");
    }
    let reference = repository_root.join("refs").join(&model.source.revision);
    std::fs::create_dir_all(reference.parent().expect("revision reference parent"))
        .expect("create revision reference parent");
    std::fs::write(reference, &model.source.revision).expect("write revision reference");
}

pub(super) fn assert_restored_provider_session_unavailable(
    client: &Client,
    selection: &WorkProductSelectionScopeV1,
    task_id: &tracedecay_domain::TaskId,
    verified_version: &VerifiedWorkGraphVersionV1,
    identity: &WorkAttemptIdentityV1,
) -> WorkAttemptReceiptV1 {
    let mut restored_receipt = None;
    for temporal in [
        TemporalModeV1::Current,
        TemporalModeV1::AsOf { cutoff: now() },
        TemporalModeV1::Evolution,
        TemporalModeV1::Forensic,
    ] {
        let (receipt, evidence, omissions) = retrieve(
            client,
            selection,
            task_id,
            verified_version,
            identity,
            temporal,
        )
        .unwrap_or_else(|error| panic!("typed SDK retrieval failed in {temporal:?}: {error}"));
        let receipt = receipt.unwrap_or_else(|| {
            panic!("typed SDK omitted attempt receipt in {temporal:?}: omissions={omissions:?}")
        });
        assert!(
            evidence.is_none(),
            "a missing evaluated query authority cannot hydrate TaskSession in {temporal:?}"
        );
        assert!(
            omissions.iter().any(|omission| {
                omission.relation == "task_session"
                    && omission.reason == WorkEvidenceOmissionReasonV1::Unavailable
            }),
            "TaskSession unavailability must remain typed in {temporal:?}: {omissions:?}"
        );
        let provider_session = receipt
            .evidence
            .as_ref()
            .and_then(|evidence| evidence.provider_session.as_ref())
            .expect("provider-qualified attempt receipt");
        assert_eq!(provider_session.provider().as_str(), "claude");
        assert_eq!(provider_session.session_id().as_str(), PROVIDER_SESSION_ID);
        if let Some(restored) = &restored_receipt {
            assert_eq!(
                restored, &receipt,
                "temporal modes must preserve the receipt"
            );
        } else {
            restored_receipt = Some(receipt);
        }
    }

    restored_receipt.expect("restored provider receipt")
}

#[allow(clippy::too_many_arguments)]
pub(super) fn configure_restart_and_activate_semantic_profile(
    home: &Path,
    project: &Path,
    client: &Client,
    project_id: &ProjectId,
    mut daemon: common::DaemonProcess,
    selection: &WorkProductSelectionScopeV1,
    task_id: &TaskId,
    verified_version: &VerifiedWorkGraphVersionV1,
    identity: &WorkAttemptIdentityV1,
    expected_receipt: &WorkAttemptReceiptV1,
    installed: &InstalledSemanticFixture,
) -> common::DaemonProcess {
    let receipt = assert_restored_provider_session_unavailable(
        client,
        selection,
        task_id,
        verified_version,
        identity,
    );
    assert_eq!(
        receipt, *expected_receipt,
        "the accepted-attempt receipt must survive restart exactly"
    );
    set_semantic_runtime_configuration(
        client,
        project_id,
        None,
        "configuration.advanced-workflow-semantic-preactivation",
        "configure the selected semantic model through typed SDK",
    );

    daemon
        .kill_and_wait()
        .expect("physically restart daemon after semantic model configuration");
    let restarted_daemon = spawn_project_daemon(home, project);
    let restarted_client = sdk_client(home, project_id.as_str());
    let _ = wait_for_application_mount(&restarted_client);
    wait_for_work_mount(&restarted_client);
    let configured_receipt = assert_restored_provider_session_unavailable(
        &restarted_client,
        selection,
        task_id,
        verified_version,
        identity,
    );
    assert_eq!(
        configured_receipt, *expected_receipt,
        "model selection without an evaluated profile must preserve the receipt exactly"
    );
    activate_evaluated_semantic_profile(home, project, &restarted_client, project_id, installed);
    restarted_daemon
}

fn activate_evaluated_semantic_profile(
    home: &Path,
    project: &Path,
    client: &Client,
    project_id: &ProjectId,
    installed: &InstalledSemanticFixture,
) -> ManifestDigest {
    let (code, vector_generation) = wait_for_semantic_generation(home, project);
    let lifecycle = SemanticModelLifecycleOwnerV1::open_default(
        tracedecay_semantic::default_lifecycle_root_in(&home.join(".tracedecay")),
    )
    .expect("reopen installed semantic lifecycle");
    let resources = journey_semantic_resources();
    let projection =
        LoadedSemanticArtifactV1::lifecycle_projection(&lifecycle, code.manifest(), resources)
            .expect("derive the installed model's admitted projection");
    let candidate = semantic_candidate(&code, &projection, vector_generation, resources);
    let candidate_path = home.join("semantic-evaluation-candidate.json");
    std::fs::write(
        &candidate_path,
        serde_json::to_vec_pretty(&candidate).expect("semantic candidate JSON"),
    )
    .expect("write semantic evaluation candidate");
    let mut evaluator =
        std::process::Command::new(env!("CARGO_BIN_EXE_tracedecay-search-eval-direct"));
    common::apply_tracedecay_home_env(&mut evaluator, home);
    let output = evaluator
        .args(["evaluate-and-publish", "--project-root"])
        .arg(project)
        .arg("--candidate")
        .arg(&candidate_path)
        .current_dir(project)
        .output()
        .expect("start direct semantic evaluator");
    assert!(
        output.status.success(),
        "direct semantic evaluator failed: {}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    let publication: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "direct semantic evaluator returned invalid JSON: {error}; stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    });
    assert_eq!(
        publication["report"]["status"], "pass",
        "only a native evaluator PASS may enter activation: {publication}"
    );
    let profile_digest = ManifestDigest::new(
        publication["profile_digest"]
            .as_str()
            .expect("published evaluated profile digest"),
    )
    .expect("valid evaluated profile digest");

    set_semantic_runtime_configuration(
        client,
        project_id,
        Some(SemanticProfileSelection {
            profile_id: EVALUATED_PROFILE_ID.to_owned(),
            accepted_profile_digest: profile_digest.clone(),
            artifact_digest: installed.artifact_digest.clone(),
            artifact_path: installed.artifact_path.clone(),
        }),
        "configuration.advanced-workflow-semantic-activation",
        "activate evaluated semantic profile through typed SDK",
    );
    profile_digest
}

fn journey_semantic_resources() -> SemanticResourceCeilings {
    SemanticResourceCeilings {
        load_deadline_ms: JOURNEY_MODEL_LOAD_DEADLINE_MS,
        ..SemanticResourceCeilings::default()
    }
}

fn set_semantic_runtime_configuration(
    client: &Client,
    project_id: &ProjectId,
    active_profile: Option<SemanticProfileSelection>,
    idempotency_key: &str,
    operation: &str,
) {
    let observed = client
        .execute::<ApplicationConfigurationObservedState>(
            &tracedecay_application::configuration::ConfigurationObservedStateRequestV1 {},
        )
        .expect("semantic configuration observed state")
        .result;
    let expected_revision = observed
        .first()
        .expect("configuration component")
        .desired_revision_id
        .clone();
    client
        .execute::<ApplicationConfigurationSet>(
            &tracedecay_application::configuration::ConfigurationSetRequestV1 {
                layer: ConfigurationLayerIdV1::Project {
                    project_id: project_id.clone(),
                },
                key: SettingKey::new(SEMANTIC_RUNTIME_SETTING_KEY)
                    .expect("semantic runtime setting key"),
                value: ConfigurationValueV1::Text(
                    serde_json::to_string(&SemanticConfig {
                        selected_model: Some(DEFAULT_FASTEMBED_MODEL_ID.to_owned()),
                        auto_download: false,
                        active_profile,
                        rollback_profile: None,
                        resources: journey_semantic_resources(),
                    })
                    .expect("semantic runtime configuration JSON"),
                ),
                expected_revision,
                idempotency_key: ConfigurationIdempotencyKey::new(idempotency_key.to_owned())
                    .expect("semantic configuration idempotency key"),
            },
        )
        .unwrap_or_else(|error| panic!("{operation}: {error}"));
}

fn wait_for_semantic_generation(
    home: &Path,
    project: &Path,
) -> (
    tracedecay_code_index::production::CodeIndexPublishedGenerationV1,
    VectorGenerationIdV1,
) {
    let deadline = Instant::now() + Duration::from_secs(180);
    loop {
        let code = read_active_code_generation(home, project);
        let status = semantic_runtime_status(home, project);
        let vector = status.as_ref().and_then(|status| match &status.state {
            SemanticRuntimeStateV1::Degraded {
                active_generation: Some(generation),
                ..
            } => Some(generation.clone()),
            SemanticRuntimeStateV1::Current { receipt } => {
                Some(receipt.activated_generation.clone())
            }
            _ => None,
        });
        if let (Some(code), Some(vector)) = (code, vector) {
            return (code, vector);
        }
        assert_semantic_lifecycle_not_failed(home, status.as_ref());
        assert!(
            Instant::now() < deadline,
            "timed out waiting for the real semantic vector generation: {status:?}"
        );
        std::thread::sleep(Duration::from_millis(250));
    }
}

fn assert_semantic_lifecycle_not_failed(home: &Path, status: Option<&SemanticRuntimeStatusV1>) {
    let Some(status) = status else {
        return;
    };
    if let SemanticRuntimeStateV1::Failed { detail, .. } = &status.state {
        panic!("semantic runtime failed while building the real vector generation: {detail}");
    }
    if !matches!(
        &status.state,
        SemanticRuntimeStateV1::Degraded {
            active_generation: None,
            reason: SemanticFallbackReasonV1::RuntimeFailure,
        }
    ) {
        return;
    }
    let lifecycle = SemanticModelLifecycleOwnerV1::open_default(
        tracedecay_semantic::default_lifecycle_root_in(&home.join(".tracedecay")),
    )
    .expect("reopen semantic lifecycle after runtime failure");
    if let Some(SemanticModelLifecycleStateV1::Failed { detail, .. }) = lifecycle.status().state {
        panic!(
            "semantic model lifecycle failed while building the real vector generation: {detail}; \
             runtime={status:?}"
        );
    }
}

fn read_active_code_generation(
    home: &Path,
    project: &Path,
) -> Option<tracedecay_code_index::production::CodeIndexPublishedGenerationV1> {
    let marker = tracedecay::storage::read_enrollment_marker(project).ok()??;
    let layout =
        tracedecay::storage::profile_sharded_layout(project, &home.join(".tracedecay"), &marker)
            .ok()?;
    let scope = scoped_code_index_store_root(&layout.data_root.join("code-index-v1"), project);
    let pointer = serde_json::from_slice::<DurablePublicationPointerV1>(
        &std::fs::read(scope.join("active-code-generation-v1.json")).ok()?,
    )
    .ok()?;
    tracedecay_code_index::production::CodeIndexPublishedGenerationV1::decode_sealed(
        &std::fs::read(
            scope
                .join("code-generations-v1")
                .join(pointer.generation_file),
        )
        .ok()?,
    )
    .ok()
}

fn semantic_candidate(
    code: &tracedecay_code_index::production::CodeIndexPublishedGenerationV1,
    projection: &AdmittedEmbeddingProjectionKeyV1,
    vector_generation_id: VectorGenerationIdV1,
    evaluation_limits: SemanticResourceCeilings,
) -> SemanticEvaluationProfileCandidateV1 {
    let material =
        tracedecay::search_eval::load_default_evaluated_profile_material(EVALUATED_PROFILE_ID)
            .expect("checked-in evaluated profile material");
    let embedding = projection.embedding_key();
    let runtime_compatibility_digest = canonical_sha256(&(
        "tracedecay.semantic-runtime-compatibility.v1",
        &embedding.runtime_backend,
        &embedding.runtime_build_revision,
        embedding.device_class,
        embedding.precision,
    ))
    .expect("runtime compatibility digest");
    let calibration = SemanticCalibrationProfileV1 {
        calibration_profile_id: CalibrationProfileId::new(
            "calibration.semantic.advanced-workflow-journey.v1",
        )
        .expect("calibration profile id"),
        cohort_digest: canonical_sha256(&(
            "tracedecay.semantic.advanced-workflow-journey.cohort.v1",
            code.manifest().generation_id.clone(),
            vector_generation_id.clone(),
            code.capability().manifest_digest.clone(),
        ))
        .expect("calibration cohort digest"),
        projection_key: projection.projection_key().clone(),
        vector_generation: vector_generation_id.clone(),
        capability_manifest_digest: code.capability().manifest_digest.clone(),
        maximum_distance_micros: i64::MAX,
        minimum_margin_micros: 0,
    };
    SemanticEvaluationProfileCandidateV1 {
        evaluated_profile_id: EVALUATED_PROFILE_ID.to_owned(),
        profile: SemanticEvaluationFusionCandidateV1 {
            profile_id: material.profile.profile_id.clone(),
            calibrations: material.profile.calibrations.clone(),
            score_domain_calibrations: material.profile.score_domain_calibrations.clone(),
            weights_micros: material.profile.weights_micros.clone(),
            diversity_policy_id: material.profile.diversity_policy_id.clone(),
            rerank_policy_id: material.profile.rerank_policy_id.clone(),
            retrieval_budget: material.profile.retrieval_budget,
        },
        diversity: SemanticEvaluationDiversityCandidateV1 {
            policy_id: material.diversity.policy_id.clone(),
            per_source_namespace: material.diversity.per_source_namespace,
            per_source_instance: material.diversity.per_source_instance,
            per_repository: material.diversity.per_repository,
            per_file: material.diversity.per_file,
            per_session_or_thread: material.diversity.per_session_or_thread,
            per_copy_cluster: material.diversity.per_copy_cluster,
            per_evidence_role: material.diversity.per_evidence_role,
        },
        rerank: None,
        compatibility: RetrievalCompatibilityPinsV1 {
            semantic: Some(SemanticCompatibilityPinsV1 {
                implementation_revision: ComponentRevision::new("semantic.fastembed.production.v1")
                    .expect("semantic implementation revision"),
                fusion_revision: ComponentRevision::new(
                    "fusion.semantic.advanced-workflow-journey.v1",
                )
                .expect("fusion revision"),
                artifact_manifest_digest: embedding.model_artifact_digest.clone(),
                runtime_compatibility_digest,
                projection: projection.clone(),
                search_index_key: SemanticSearchIndexProfileV1::exact_flat_v1()
                    .and_then(|profile| profile.index_key())
                    .expect("production exact-flat semantic index"),
                vector_generation_id,
                calibration,
                resources: SemanticResourceRequirementV1 {
                    model_bytes: evaluation_limits.max_model_bytes,
                    tokenizer_bytes: evaluation_limits.max_tokenizer_bytes,
                    resident_bytes: evaluation_limits.max_resident_bytes,
                    threads: evaluation_limits.max_threads,
                    max_concurrent_sessions: evaluation_limits.max_concurrent_sessions,
                    batch_size: evaluation_limits.max_batch_size,
                    sequence_length: evaluation_limits.max_sequence_length,
                    load_deadline_ms: evaluation_limits.load_deadline_ms,
                },
            }),
            rerank: None,
        },
    }
}

pub(super) fn wait_for_semantic_current(home: &Path, project: &Path) {
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        let status = semantic_runtime_status(home, project)
            .unwrap_or_else(|| panic!("runtime returned an invalid semantic status"));
        if matches!(status.state, SemanticRuntimeStateV1::Current { .. }) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for activated semantic runtime: {status:?}"
        );
        std::thread::sleep(Duration::from_millis(250));
    }
}

fn semantic_runtime_status(home: &Path, project: &Path) -> Option<SemanticRuntimeStatusV1> {
    let value = serve_tool_call(
        home,
        project,
        "tracedecay_runtime",
        json!({
            "format": "json",
            "authority_audit": true,
            "session_ingest_health": true,
            "doctor_report": false
        }),
    );
    serde_json::from_value(value["semantic_runtime"].clone()).ok()
}

pub(super) fn assert_available_over_sdk_and_mcp(
    home: &Path,
    project: &Path,
    client: &Client,
    selection: &WorkProductSelectionScopeV1,
    task_id: &TaskId,
    verified_version: &VerifiedWorkGraphVersionV1,
    identity: &WorkAttemptIdentityV1,
) -> WorkTaskSessionEvidenceV1 {
    let mut current = None;
    for temporal in [
        TemporalModeV1::Current,
        TemporalModeV1::AsOf { cutoff: now() },
        TemporalModeV1::Evolution,
        TemporalModeV1::Forensic,
    ] {
        let expansion = Some(WorkEvidenceExpansionSelectorV1::TaskSession {
            attempt: identity.clone(),
        });
        let first = retrieve_over_sdk_and_mcp(
            home,
            project,
            client,
            WorkEvidenceRetrieveRequestV1 {
                selection: selection.clone(),
                task_id: task_id.clone(),
                verified_version: verified_version.clone(),
                temporal,
                page_size: 1,
                expansion: expansion.clone(),
                continuation: None,
                observed_at: now(),
            },
        );
        let continuation = first
            .continuations
            .iter()
            .find_map(|continuation| match continuation {
                WorkEvidenceContinuationV1::TaskSession { continuation }
                    if continuation.attempt == *identity =>
                {
                    Some(continuation.clone())
                }
                _ => None,
            })
            .unwrap_or_else(|| {
                panic!("{temporal:?} first page must expose an exact TaskSession continuation")
            });
        assert!(
            continuation.temporal_cursor.is_some(),
            "{temporal:?} first page must expose the exact temporal TaskSession continuation"
        );
        let (first_receipt, first_evidence, omissions) = evidence_for_attempt(first, identity);
        let first_receipt = first_receipt
            .unwrap_or_else(|| panic!("{temporal:?} first page omitted the attempt receipt"));
        assert_available(&omissions, temporal);
        let first_evidence = first_evidence
            .unwrap_or_else(|| panic!("{temporal:?} first page omitted TaskSession evidence"));
        assert_eq!(first_evidence.continuation, Some(continuation.clone()));
        assert_eq!(first_evidence.ranked_anchors.len(), 1);
        assert_eq!(first_evidence.hydrated.len(), 1);
        assert_eq!(
            first_evidence.hydrated[0].state,
            WorkTaskSessionHydrationStateV1::Available
        );
        assert!(
            first_evidence.hydrated[0]
                .content
                .as_ref()
                .is_some_and(|content| !content.is_empty())
        );
        assert_eq!(first_evidence.source.provider().as_str(), "claude");
        assert_eq!(
            first_evidence.source.session_id().as_str(),
            PROVIDER_SESSION_ID
        );

        let second = retrieve_over_sdk_and_mcp(
            home,
            project,
            client,
            WorkEvidenceRetrieveRequestV1 {
                selection: selection.clone(),
                task_id: task_id.clone(),
                verified_version: verified_version.clone(),
                temporal,
                page_size: 1,
                expansion,
                continuation: Some(WorkEvidenceContinuationV1::TaskSession { continuation }),
                observed_at: now(),
            },
        );
        let (second_receipt, second_evidence, omissions) = evidence_for_attempt(second, identity);
        assert_eq!(second_receipt.as_ref(), Some(&first_receipt));
        assert_available(&omissions, temporal);
        let second_evidence = second_evidence
            .unwrap_or_else(|| panic!("{temporal:?} continuation omitted TaskSession evidence"));
        assert_eq!(
            second_evidence.participant_epoch,
            first_evidence.participant_epoch
        );
        assert_eq!(second_evidence.source, first_evidence.source);
        assert_eq!(second_evidence.ranked_anchors.len(), 1);
        assert_eq!(second_evidence.hydrated.len(), 1);
        assert_eq!(
            second_evidence.hydrated[0].state,
            WorkTaskSessionHydrationStateV1::Available
        );
        assert!(
            second_evidence.hydrated[0]
                .content
                .as_ref()
                .is_some_and(|content| !content.is_empty())
        );
        assert_ne!(
            second_evidence.hydrated[0].anchor_id, first_evidence.hydrated[0].anchor_id,
            "{temporal:?} continuation repeated a hydrated TaskSession anchor"
        );
        if temporal == TemporalModeV1::Current {
            current = Some(first_evidence);
        }
    }
    current.expect("Current TaskSession evidence")
}

fn assert_available(
    omissions: &[tracedecay_application::WorkEvidenceOmissionV1],
    temporal: TemporalModeV1,
) {
    assert!(
        !omissions
            .iter()
            .any(|omission| { omission.relation == "task_session" }),
        "an activated evaluated query authority must not omit TaskSession in {temporal:?}: \
         {omissions:?}"
    );
}

fn retrieve_over_sdk_and_mcp(
    home: &Path,
    project: &Path,
    client: &Client,
    request: WorkEvidenceRetrieveRequestV1,
) -> WorkEvidenceRetrievalV1 {
    let sdk = client
        .execute::<WorkRetrieveEvidence>(&request)
        .unwrap_or_else(|error| panic!("typed SDK TaskSession retrieval failed: {error}"))
        .result;
    let mcp_envelope = serve_tool_call(
        home,
        project,
        "tracedecay_work_retrieve_evidence",
        serde_json::to_value(&request).expect("MCP Work evidence request"),
    );
    assert_eq!(
        mcp_envelope["kind"], "success",
        "MCP Work retrieval must return the canonical success envelope: {mcp_envelope}"
    );
    let mcp = serde_json::from_value::<WorkEvidenceRetrievalV1>(
        mcp_envelope["value"]["outcome"]["value"]["payload"].clone(),
    )
    .expect("canonical MCP Work evidence payload");
    assert_eq!(
        mcp, sdk,
        "typed SDK and real tracedecay serve must expose the same Work payload"
    );
    sdk
}

fn serve_tool_call(home: &Path, project: &Path, tool_name: &str, arguments: Value) -> Value {
    let mut command = common::tracedecay_command_with_home(home);
    let child = command
        .args(["serve", "--path"])
        .arg(project)
        .current_dir(project)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("tracedecay serve should start");
    let mut child = common::TestChildProcess::new(child);
    {
        let stdin = child.stdin_mut().expect("serve stdin");
        writeln!(
            stdin,
            "{}",
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2024-11-05",
                    "capabilities": {},
                    "clientInfo": {"name": "advanced-workflow-journey", "version": "1"}
                }
            })
        )
        .expect("write MCP initialize");
        writeln!(
            stdin,
            "{}",
            json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/call",
                "params": {"name": tool_name, "arguments": arguments}
            })
        )
        .expect("write MCP tools/call");
    }
    let output = child
        .wait_with_output(Duration::from_secs(120))
        .expect("tracedecay serve should exit after stdin closes");
    assert!(
        output.status.success(),
        "tracedecay serve failed: {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("MCP stdout UTF-8");
    let response = stdout
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find(|response| response["id"] == 2)
        .unwrap_or_else(|| panic!("missing MCP tools/call response in stdout:\n{stdout}"));
    assert!(
        response.get("error").is_none(),
        "MCP tools/call failed: {response}"
    );
    let content = response["result"]["content"]
        .as_array()
        .expect("MCP tool content");
    content
        .iter()
        .filter_map(|item| item["text"].as_str())
        .find_map(|text| {
            let start = text.find('{').or_else(|| text.find('['))?;
            serde_json::from_str(&text[start..]).ok()
        })
        .unwrap_or_else(|| panic!("MCP tool response omitted JSON content: {response}"))
}

fn retrieve(
    client: &Client,
    selection: &WorkProductSelectionScopeV1,
    task_id: &tracedecay_domain::TaskId,
    verified_version: &VerifiedWorkGraphVersionV1,
    identity: &WorkAttemptIdentityV1,
    temporal: TemporalModeV1,
) -> Result<
    (
        Option<WorkAttemptReceiptV1>,
        Option<WorkTaskSessionEvidenceV1>,
        Vec<tracedecay_application::WorkEvidenceOmissionV1>,
    ),
    String,
> {
    let result = client
        .execute::<WorkRetrieveEvidence>(&WorkEvidenceRetrieveRequestV1 {
            selection: selection.clone(),
            task_id: task_id.clone(),
            verified_version: verified_version.clone(),
            temporal,
            page_size: 100,
            expansion: None,
            continuation: None,
            observed_at: UtcMicros(now().0),
        })
        .map_err(|error| error.to_string())?
        .result;
    Ok(evidence_for_attempt(result, identity))
}

fn evidence_for_attempt(
    result: WorkEvidenceRetrievalV1,
    identity: &WorkAttemptIdentityV1,
) -> (
    Option<WorkAttemptReceiptV1>,
    Option<WorkTaskSessionEvidenceV1>,
    Vec<tracedecay_application::WorkEvidenceOmissionV1>,
) {
    let omissions = result.omissions;
    let mut receipt = None;
    let mut task_session = None;
    for source in result.sources {
        match source {
            WorkEvidenceSourceV1::AttemptReceipt { receipt: candidate }
                if candidate.identity == *identity =>
            {
                receipt = Some(candidate);
            }
            WorkEvidenceSourceV1::TaskSession { attempt, evidence } if attempt == *identity => {
                task_session = Some(evidence)
            }
            _ => {}
        }
    }
    (receipt, task_session, omissions)
}
