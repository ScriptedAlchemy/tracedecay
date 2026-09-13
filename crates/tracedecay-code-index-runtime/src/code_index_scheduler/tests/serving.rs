use std::{
    collections::BTreeSet,
    fmt::Write as _,
    num::NonZeroU64,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tracedecay_code_index_retention::code_index_generations::code_text_artifacts_root;
use tracedecay_contracts::{
    CallableCodeOperationKind, CallableCodeQueryPort, CodeQueryScope, CodeRelationRequest,
    CodeSymbolSearchRequest, ExactOccurrenceRequest, OmissionReason, OpaqueCursor, PageRequest,
    PhraseSearchRequest, QualifiedNameRequest, ResolvedScope, ResultProjection, RetrievalOrder,
    RetrievalPortContext, RetrievalPortOutcome, RetrievalRequestMeta, SourceMetadataRequest,
    callable_code_operation,
    retrieval::{
        CodeFacetDimension, CodeFacetRequest, CodeHierarchyRequest, CodeImpactRequest,
        CodeImplementationsRequest, CodeNavigationRequest, CodeTimelineRequest,
        ImplementationSelector, ModuleApiRequest,
    },
};
use tracedecay_domain::{
    AuthorizationRevision, CodeGenerationId, ComponentRevision, EphemeralSanitizedQueryViewV1,
    ExactAdmissionRuleRevision, FreshnessVectorDigest, PrincipalId, PrivacyDomainId, ProjectId,
    ProviderEvaluationStateV1, PublicRetrieverStatus, QueryNormalizationRevision,
    RelationEdgeKindV1, RetrievalBudget, RetrievalRequest, RetrievalScope, RetrievalSnapshot,
    RetrieverKind, RetrieverOutcome, SanitizerRevision, ScoreDomainId, SingleRootScopeV1,
    TemporalModeV1, UtcMicros, VectorWatermark, encode_lowercase_hex, sha256_hex_suffix,
};
use tracedecay_query::retrieval::{
    exact::{CentralExactAdmissionAuthorityV1, ExactAdmissionAuthority, ExactLaneRequest},
    lexical::{
        CODE_LEXICAL_ARTIFACT_BUILD_MEMORY_BUDGET_BYTES_V1,
        CODE_LEXICAL_ARTIFACT_QUERY_CACHE_BUDGET_BYTES_V1, CodeLexicalArtifactBuilderV1,
        CodeLexicalArtifactFinalizationStepV1, CodeLexicalArtifactReaderV1, LexicalLaneRequest,
        LexicalRouteKindV1, LexicalRoutingV1,
    },
    semantic::{SemanticAbstentionV1, SemanticQueryModeV1},
};
use tracedecay_runtime_core::resident_memory::{
    DEFAULT_PROCESS_RESIDENT_MEMORY_LIMIT_V1, ProcessResidentMemoryV1, ResidentMemoryPressureV1,
    sampled_process_resident_bytes_v1,
};
use tracedecay_semantic_contracts::SemanticFallbackReasonV1;

use super::{
    CALLER_PAGE, GitFixture, ReadySemanticControlV1, active_text_artifact_path,
    application_context, build_progress_snapshot, caller_star_sources, callers_page_meta,
    core_search_request, decode_hex, git, install_verified_graph_store,
    install_verified_graph_store_on_text, mount_core_query_authority, mount_query_authority,
    mounted_core_query_worktree, mounted_core_query_worktree_with_one_permit,
    moved_reference_scope, progress_snapshot_for_generation, published, query_authority,
    query_authority_with_candidate_cap, query_meta, ranked_symbol_names, ranks_symbol,
    rewrite_active_text_artifact_format_revision, routed_core_search_request, scheduler,
    test_project_id, wait_for_live_complete_generation, wait_for_queryable_text_generation,
    wait_for_queryable_text_generation_change,
};
use crate::{
    code_index::production::{
        CodeIndexExecutionControlV1, UninterruptibleCodeIndexControlV1,
        VerifiedSealedLexicalPageReadV1,
    },
    code_index::provider::GenerationTestAttributionJoinReadPort,
    code_index_scheduler::{
        CodeIndexBuildProgressStateV1, CodeIndexCommittedProgressSampleV1,
        CodeIndexSchedulerRegistryV1, SharedCodeIndexBytePoolV1,
    },
};

#[test]
fn text_artifact_source_batches_scale_with_build_memory() {
    assert_eq!(
        super::super::text_artifact_source_batch_limits(
            CODE_LEXICAL_ARTIFACT_BUILD_MEMORY_BUDGET_BYTES_V1
        ),
        (64, 64 * 1024 * 1024, 128)
    );
    assert_eq!(
        super::super::text_artifact_source_batch_limits(
            8 * CODE_LEXICAL_ARTIFACT_BUILD_MEMORY_BUDGET_BYTES_V1
        ),
        (512, 512 * 1024 * 1024, 1024)
    );
}
#[test]
fn semantic_mcp_reasons_bind_runtime_state_and_exact_source_generation() {
    let latest =
        tracedecay_domain::CodeGenerationId::new("generation.latest").expect("latest generation");
    let stale =
        tracedecay_domain::CodeGenerationId::new("generation.stale").expect("stale generation");
    let _vector = tracedecay_domain::VectorGenerationIdV1::new(
        tracedecay_domain::canonical_sha256(&"semantic-mcp-vector").expect("vector digest"),
    );

    assert_eq!(
        super::super::queries::semantic_mcp_reason(None, &latest, None),
        "semantic_runtime_unavailable"
    );
    assert_eq!(
        super::super::queries::semantic_mcp_reason(Some(&stale), &latest, None),
        "semantic_generation_stale"
    );
    assert_eq!(
        super::super::queries::semantic_mcp_reason(Some(&latest), &latest, None),
        "calibration_unavailable"
    );
    for (state, reason) in [
        (
            tracedecay_application::semantic_runtime::SemanticRuntimeStateV1::Indexing {
                completed_units: 1,
                total_units: 2,
            },
            "semantic_indexing",
        ),
        (
            tracedecay_application::semantic_runtime::SemanticRuntimeStateV1::Degraded {
                active_generation: None,
                reason: SemanticFallbackReasonV1::RuntimeFailure,
            },
            "semantic_degraded",
        ),
        (
            tracedecay_application::semantic_runtime::SemanticRuntimeStateV1::Failed {
                model_id: "model.fixture".to_owned(),
                artifact_digest: format!("sha256:{}", "a".repeat(64)),
                detail: "fixture failure".to_owned(),
                retryable: true,
            },
            "semantic_failed",
        ),
    ] {
        assert_eq!(
            super::super::queries::semantic_mcp_reason(None, &latest, Some(&state)),
            reason
        );
    }
}

/// Foreground query admission never performs the O(store) text projection.
/// Only the scheduler-owned builder may advance it, one bounded document
/// window at a time, until the immutable owners become visible atomically.
#[test]
fn foreground_query_owner_read_stays_warming_until_background_projection_finishes() {
    let fixture = GitFixture::new(&[(
        "src/lib.rs",
        "pub fn caller() { callee(); }\npub fn callee() {}\n",
    )]);
    let store = TempDir::new().expect("store root");
    let bytes = Arc::new(SharedCodeIndexBytePoolV1::default());
    let mut scheduler = scheduler(&fixture, store.path().to_path_buf(), bytes);
    published(scheduler.reconcile_now().expect("publish"));
    let latest = scheduler.latest_complete().expect("latest generation");
    let budget = RetrievalBudget {
        max_candidates_per_lane: 1,
        max_fused_candidates: 1,
        max_hydrated_results: 1,
        max_hydration_bytes: 1,
        deadline_micros: None,
    };
    let refusal = latest.production_query_owners_with_budget(&budget);
    match refusal {
        Err(tracedecay_query::retrieval::RetrievalPortError::AuthorityUnavailable(_)) => {}
        Err(error) => {
            panic!("a cold foreground read must report typed warming: {error:?}")
        }
        Ok(_) => panic!("a cold foreground read must not build serving owners"),
    }
    assert!(
        matches!(
            &*latest.text_projection_build.lock_slot(),
            super::super::CodeTextProjectionSlotV1::Idle
        ),
        "foreground observation must not initialize the background builder"
    );
    while !latest
        .advance_text_serving(1)
        .expect("bounded background text projection")
    {}
    assert!(latest.query_owners_are_warm());
    assert!(
        matches!(
            &*latest.text_projection_build.lock_slot(),
            super::super::CodeTextProjectionSlotV1::Idle
        ),
        "completed projection must release its partial builder state"
    );
}

/// The production text-serving journey with a durable store: build the `SQLite`
/// lexical artifact in bounded page windows, publish it durably (pointer names
/// the content-addressed artifact file), then reopen the durable head after a
/// simulated restart in a single bounded pass — no rebuild — and serve exact
/// and lexical queries from it.
#[test]
fn production_text_serving_builds_publishes_and_reopens_the_artifact_head() {
    let fixture = GitFixture::new(&[(
        "src/lib.rs",
        "pub fn caller() { callee(); }\npub fn callee() {}\n",
    )]);
    let store = TempDir::new().expect("store root");
    let completed_before_restart = {
        let mut scheduler = scheduler(
            &fixture,
            store.path().to_path_buf(),
            Arc::new(SharedCodeIndexBytePoolV1::default()),
        );
        published(scheduler.reconcile_now().expect("publish"));
        let latest = scheduler.latest_complete().expect("latest generation");
        let mut passes = 0_usize;
        while !latest
            .advance_text_serving(64)
            .expect("advance durable text-artifact build")
        {
            passes += 1;
            assert!(
                passes < 10_000,
                "the bounded artifact build never completed"
            );
        }
        assert!(
            latest
                .production_query_owners()
                .expect("artifact-backed owners")
                .is_artifact_backed(),
            "a durable store must serve text queries from the published artifact"
        );
        let progress = build_progress_snapshot(&scheduler);
        assert_eq!(
            progress.phase,
            tracedecay_contracts::code_index_freshness::CodeIndexBuildPhaseV1::Ready
        );
        progress
    };

    // The durable pointer must name the published content-addressed artifact.
    let pointer: serde_json::Value = serde_json::from_slice(
        &std::fs::read(store.path().join("active-code-generation-v1.json"))
            .expect("read durable pointer"),
    )
    .expect("parse durable pointer");
    let active_generation = pointer["generation_id"]
        .as_str()
        .expect("active generation id")
        .to_owned();
    let entry = pointer["generation_index"]
        .as_array()
        .expect("durable generation index")
        .iter()
        .find(|entry| entry["generation_id"] == active_generation.as_str())
        .expect("active generation index entry");
    let artifact_file = entry["text_artifact"]["artifact_file"]
        .as_str()
        .expect("attached text artifact descriptor");
    assert!(
        artifact_file.starts_with("text-artifact-") && artifact_file.ends_with(".bin"),
        "the durable descriptor must use the content-addressed artifact naming rule"
    );
    let artifact_path = store
        .path()
        .join("code-text-artifacts-v1")
        .join(artifact_file);
    assert!(
        std::fs::metadata(&artifact_path)
            .expect("published artifact file")
            .len()
            > 0,
        "the published artifact file must exist and be non-empty"
    );

    // Simulated restart: a fresh scheduler over the same store must reopen
    // the durable head in ONE bounded pass instead of rebuilding.
    let scheduler = scheduler(
        &fixture,
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    );
    let latest = scheduler.latest_complete().expect("restored generation");
    assert!(
        latest
            .advance_text_serving(1)
            .expect("durable-head reopen pass"),
        "a published durable head must reopen without a rebuild"
    );
    let owners = latest
        .production_query_owners()
        .expect("reopened artifact owners");
    assert!(
        owners.is_artifact_backed(),
        "the reopened owners must serve from the durable artifact"
    );
    let completed_after_restart = build_progress_snapshot(&scheduler);
    assert_eq!(
        completed_after_restart.phase,
        tracedecay_contracts::code_index_freshness::CodeIndexBuildPhaseV1::Ready
    );
    assert_eq!(
        completed_after_restart.generation_id,
        completed_before_restart.generation_id
    );
    assert_eq!(
        completed_after_restart.sealed_source_digest,
        completed_before_restart.sealed_source_digest
    );
    assert_eq!(
        completed_after_restart.committed_pages,
        completed_before_restart.committed_pages
    );
    assert_eq!(
        completed_after_restart.committed_chunks,
        completed_before_restart.committed_chunks
    );
    assert_eq!(
        completed_after_restart.committed_imports,
        completed_before_restart.committed_imports
    );
    assert_eq!(
        completed_after_restart.committed_payload_bytes,
        completed_before_restart.committed_payload_bytes
    );
    assert_eq!(
        completed_after_restart.completed_files,
        completed_before_restart.completed_files
    );
    assert_eq!(
        completed_after_restart.total_files,
        completed_before_restart.total_files
    );
    assert_eq!(
        completed_after_restart.completed_lexical_units,
        completed_before_restart.completed_lexical_units
    );
    assert_eq!(
        completed_after_restart.total_lexical_units,
        completed_before_restart.total_lexical_units
    );
    assert_eq!(
        completed_after_restart.completed_files,
        completed_after_restart.total_files
    );
    assert_eq!(
        completed_after_restart.completed_lexical_units,
        completed_after_restart.total_lexical_units
    );
    assert_eq!(completed_after_restart.files_per_second, None);
    assert_eq!(completed_after_restart.lexical_units_per_second, None);
    assert_eq!(completed_after_restart.estimated_remaining_seconds, None);

    let generation = latest.generation().manifest().generation_id.clone();
    let base = RetrievalRequest {
        principal: PrincipalId::new("principal.artifact-head").expect("principal"),
        scope: RetrievalScope {
            privacy_domain: latest.generation().manifest().privacy_domain.clone(),
            root: SingleRootScopeV1 {
                repository: latest.generation().snapshot().repository.clone(),
                worktree: latest.generation().snapshot().worktree.clone(),
                reference: latest.generation().snapshot().reference.clone(),
            },
        },
        temporal_mode: TemporalModeV1::Current,
        snapshot: RetrievalSnapshot {
            watermarks: VectorWatermark::default(),
            freshness_digest: FreshnessVectorDigest::new(format!("sha256:{}", "f".repeat(64)))
                .expect("freshness digest"),
            authorization_revision: AuthorizationRevision::new("authorization.artifact-head.v1")
                .expect("authorization revision"),
            captured_at: UtcMicros(1),
        },
        profile_id: "profile.artifact-head.v1"
            .to_owned()
            .try_into()
            .expect("profile"),
        budget: RetrievalBudget {
            max_candidates_per_lane: 8,
            max_fused_candidates: 8,
            max_hydrated_results: 8,
            max_hydration_bytes: 65_536,
            deadline_micros: None,
        },
    };
    let query_view = EphemeralSanitizedQueryViewV1::sanitize(
        "callee",
        SanitizerRevision::new("sanitizer.artifact-head.v1").expect("sanitizer"),
        QueryNormalizationRevision::new("normalization.artifact-head.v1").expect("normalization"),
    )
    .expect("query view");
    let authority = CentralExactAdmissionAuthorityV1::new(
        ExactAdmissionRuleRevision::new(tracedecay_query::retrieval::QUERY_EXACT_RULE_REVISION_V1)
            .expect("exact rule revision"),
    );
    let exact = owners
        .retrieve_exact(&ExactLaneRequest {
            literals: authority.parse_literals(&query_view, &base),
            generation: generation.clone(),
            budget: base.budget,
            base: base.clone(),
            query_view: &query_view,
        })
        .expect("exact retrieval over the reopened artifact");
    let RetrieverOutcome::Complete(exact_batch) = exact else {
        panic!("the reopened artifact must complete the exact retrieval");
    };
    assert!(
        !exact_batch.candidates.is_empty(),
        "the exact lane must return results from the reopened artifact"
    );
    let lexical = owners
        .retrieve_lexical(&LexicalLaneRequest {
            query_view: &query_view,
            generation,
            whole_terms: vec!["callee".to_owned()],
            subtokens: vec!["callee".to_owned()],
            phrases: Vec::new(),
            field_filters: Vec::new(),
            fuzzy_budget: 0,
            lexical_profile_revision: ComponentRevision::new(
                tracedecay_query::retrieval::QUERY_LEXICAL_PROFILE_REVISION_V1,
            )
            .expect("lexical profile revision"),
            score_domain: ScoreDomainId::new(
                tracedecay_query::retrieval::QUERY_LEXICAL_SCORE_DOMAIN_V1,
            )
            .expect("lexical score domain"),
            budget: base.budget,
            base,
            control: &ReadySemanticControlV1,
        })
        .expect("lexical retrieval over the reopened artifact");
    let RetrieverOutcome::Complete(lexical_batch) = lexical else {
        panic!("the reopened artifact must complete the lexical retrieval");
    };
    assert!(
        !lexical_batch.candidates.is_empty(),
        "the lexical lane must return results from the reopened artifact"
    );
}

#[test]
fn retained_text_generation_reaches_query_owners_without_full_sealed_decode() {
    let fixture = GitFixture::new(&[(
        "src/lib.rs",
        "pub fn caller() { callee(); }\npub fn callee() {}\n",
    )]);
    let store = TempDir::new().expect("store root");
    {
        let mut scheduler = scheduler(
            &fixture,
            store.path().to_path_buf(),
            Arc::new(SharedCodeIndexBytePoolV1::default()),
        );
        published(scheduler.reconcile_now().expect("seed generation"));
    }

    let mut reopened = scheduler(
        &fixture,
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    );
    assert_eq!(reopened.sealed_decode_count(), 0);
    let text = reopened
        .servable_retained_text_generation()
        .expect("active durable text generation");
    let binding = text
        .publication_binding
        .clone()
        .expect("retained generation binding");
    let mut retained_history_update = reopened
        .publication
        .read_publication_pointer()
        .expect("read active pointer")
        .expect("active pointer");
    retained_history_update.generation_index.clear();
    retained_history_update.generation_index_truncated = true;
    retained_history_update.generation_index_digest = None;
    assert!(
        binding.matches(Some(&retained_history_update)),
        "retention-history changes must not supersede the same active seal"
    );
    let scan = build_progress_snapshot(&reopened);
    assert_eq!(
        scan.phase,
        tracedecay_contracts::code_index_freshness::CodeIndexBuildPhaseV1::SourceScan
    );
    assert!(scan.total_lexical_units > 0);
    assert_eq!(scan.completed_lexical_units, scan.total_lexical_units);
    assert_eq!(
        reopened.sealed_decode_count(),
        0,
        "binding authenticated text metadata must not decode the full generation"
    );
    while !text
        .advance_text_serving(64)
        .expect("advance retained text generation")
    {}
    assert!(text.query_owners_are_warm());
    assert_eq!(
        reopened.sealed_decode_count(),
        0,
        "exact and lexical owners must not require the full generation"
    );
    assert!(
        text.advance_text_serving(1)
            .expect("attached text descriptor preserves the active seal"),
        "a text-head pointer update must not retire its own ready owner"
    );

    fixture.edit(
        "src/lib.rs",
        "pub fn caller() { replacement(); }\npub fn replacement() {}\n",
    );
    published(
        reopened
            .reconcile_now()
            .expect("publish superseding generation"),
    );
    assert!(matches!(
        text.advance_text_serving(1),
        Err(tracedecay_query::retrieval::RetrievalPortError::Cancelled)
    ));
}

#[test]
fn text_artifact_publication_serializes_pointer_attachment_with_retention() {
    use std::sync::{Condvar, Mutex, mpsc};
    use std::thread;

    use tracedecay_code_index_retention::code_index_generations::{
        CodeGenerationRetentionModeV1, execute_code_generation_retention,
        plan_code_generation_retention,
    };

    struct PauseAfterExistingArtifactRead {
        checkpoints: std::sync::atomic::AtomicUsize,
        state: Mutex<(bool, bool)>,
        ready: Condvar,
    }

    impl PauseAfterExistingArtifactRead {
        fn wait_until_paused(&self) {
            let mut state = self.state.lock().expect("publication pause state");
            while !state.0 {
                state = self.ready.wait(state).expect("wait for publication pause");
            }
        }

        fn resume(&self) {
            let mut state = self.state.lock().expect("publication pause state");
            state.1 = true;
            self.ready.notify_all();
        }
    }

    impl CodeIndexExecutionControlV1 for PauseAfterExistingArtifactRead {
        fn is_cancelled(&self) -> bool {
            // One short staging file and one short existing artifact each
            // checkpoint before open and after their single bounded read. The
            // fourth checkpoint is therefore after the destination's bytes
            // were verified but before publication can attach its descriptor.
            if self
                .checkpoints
                .fetch_add(1, std::sync::atomic::Ordering::AcqRel)
                == 3
            {
                let mut state = self.state.lock().expect("publication pause state");
                state.0 = true;
                self.ready.notify_all();
                while !state.1 {
                    state = self.ready.wait(state).expect("wait to resume publication");
                }
            }
            false
        }

        fn is_deadline_exceeded(&self) -> bool {
            false
        }
    }

    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn retained_artifact() {}\n")]);
    let store = TempDir::new().expect("store root");
    let mut scheduler = scheduler(
        &fixture,
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    );
    published(scheduler.reconcile_now().expect("publish generation"));
    let latest = scheduler.latest_complete().expect("latest generation");
    let artifact_store = latest.text_artifact_store.clone();
    let generation = latest.generation().clone();
    let sealed_identity = artifact_store
        .sealed_identity(&generation.manifest().generation_id)
        .expect("sealed generation identity");
    let sealed_hex =
        sha256_hex_suffix(sealed_identity.digest.as_str()).expect("sealed SHA-256 digest");
    let artifacts_root = store.path().join("code-text-artifacts-v1");
    tracedecay_private_fs::create_private_directory(&artifacts_root)
        .expect("create private artifacts root");
    let staging = artifacts_root.join(format!(".text-artifact-{sealed_hex}.staging"));
    let artifact_bytes = b"already content-addressed artifact";
    let mut staging_file =
        tracedecay_private_fs::create_private_file(&staging).expect("create private staging file");
    std::io::Write::write_all(&mut staging_file, artifact_bytes).expect("write staging artifact");
    drop(staging_file);
    let artifact_hex = encode_lowercase_hex(&Sha256::digest(artifact_bytes));
    let artifact_file = format!("text-artifact-{artifact_hex}.bin");
    let artifact_path = artifacts_root.join(&artifact_file);
    let mut artifact_file_handle = tracedecay_private_fs::create_private_file(&artifact_path)
        .expect("create private orphan artifact");
    std::io::Write::write_all(&mut artifact_file_handle, artifact_bytes)
        .expect("write orphan artifact");
    drop(artifact_file_handle);

    let control = Arc::new(PauseAfterExistingArtifactRead {
        checkpoints: std::sync::atomic::AtomicUsize::new(0),
        state: Mutex::new((false, false)),
        ready: Condvar::new(),
    });
    let publish_control = Arc::clone(&control);
    let publisher = thread::spawn(move || {
        artifact_store.publish(
            &staging,
            &generation.manifest().generation_id,
            &sealed_identity,
            publish_control.as_ref(),
        )
    });
    control.wait_until_paused();

    let plan = plan_code_generation_retention(store.path(), &BTreeSet::new())
        .expect("plan the orphan artifact observed before attachment");
    let retention_root = store.path().to_path_buf();
    let (retention_done_tx, retention_done_rx) = mpsc::sync_channel(1);
    let retention = thread::spawn(move || {
        let result = execute_code_generation_retention(
            &retention_root,
            plan,
            CodeGenerationRetentionModeV1::Apply,
            UtcMicros(73),
            None,
        );
        retention_done_tx
            .send(result)
            .expect("report retention completion");
    });

    // Unfixed publication owns no store lock here, so retention completes and
    // unlinks the destination. Fixed publication holds the canonical lock and
    // keeps retention blocked until its pointer attachment is durable.
    let early_retention = retention_done_rx.recv_timeout(Duration::from_secs(2));
    control.resume();
    let descriptor = publisher
        .join()
        .expect("publication thread")
        .expect("publish text artifact");
    let retention_result = match early_retention {
        Ok(result) => result,
        Err(mpsc::RecvTimeoutError::Timeout) => retention_done_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("retention completes after publication releases the store lock"),
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            panic!("retention thread disconnected before reporting its outcome")
        }
    };
    retention.join().expect("retention thread");

    assert_eq!(descriptor.artifact_file, artifact_file);
    assert!(
        artifact_path.is_file(),
        "a successful descriptor must always retain its content-addressed bytes"
    );
    assert!(
        retention_result.is_err(),
        "the stale orphan plan must be rejected after pointer attachment"
    );
}

#[cfg(unix)]
#[test]
fn text_artifact_builder_creates_an_owner_private_artifacts_root() {
    use std::os::unix::fs::PermissionsExt;

    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn private_artifact() {}\n")]);
    let store = TempDir::new().expect("store root");
    let mut scheduler = scheduler(
        &fixture,
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    );
    published(scheduler.reconcile_now().expect("publish generation"));
    let latest = scheduler.latest_complete().expect("latest generation");

    let _ = latest
        .advance_text_serving(1)
        .expect("start text-artifact build");

    let mode = std::fs::symlink_metadata(store.path().join("code-text-artifacts-v1"))
        .expect("artifacts-root metadata")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o700, "the artifact namespace must be owner-private");
}

#[cfg(unix)]
#[test]
fn text_artifact_publish_rejects_a_permissive_artifacts_root() {
    use std::os::unix::fs::PermissionsExt;

    struct NeverCancelled;

    impl CodeIndexExecutionControlV1 for NeverCancelled {
        fn is_cancelled(&self) -> bool {
            false
        }

        fn is_deadline_exceeded(&self) -> bool {
            false
        }
    }

    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn private_publish() {}\n")]);
    let store = TempDir::new().expect("store root");
    let mut scheduler = scheduler(
        &fixture,
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    );
    published(scheduler.reconcile_now().expect("publish generation"));
    let latest = scheduler.latest_complete().expect("latest generation");
    let artifact_store = latest.text_artifact_store.clone();
    let generation = latest.generation();
    let sealed_identity = artifact_store
        .sealed_identity(&generation.manifest().generation_id)
        .expect("sealed generation identity");
    let artifacts_root = store.path().join("code-text-artifacts-v1");
    std::fs::create_dir(&artifacts_root).expect("create artifacts root");
    std::fs::set_permissions(&artifacts_root, std::fs::Permissions::from_mode(0o755))
        .expect("make artifacts root permissive");
    let staging = artifacts_root.join("artifact.staging");
    std::fs::write(&staging, b"private artifact bytes").expect("write staging artifact");

    assert!(
        matches!(
            artifact_store.publish(
                &staging,
                &generation.manifest().generation_id,
                &sealed_identity,
                &NeverCancelled,
            ),
            Err(tracedecay_query::retrieval::RetrievalPortError::Contract(_))
        ),
        "publication must fail closed instead of accepting a permissive artifact namespace"
    );
    assert!(staging.is_file(), "refusal must preserve staging evidence");
}

#[cfg(unix)]
#[test]
fn text_artifact_publish_rejects_a_symlink_artifacts_root() {
    use std::os::unix::fs::symlink;

    struct NeverCancelled;

    impl CodeIndexExecutionControlV1 for NeverCancelled {
        fn is_cancelled(&self) -> bool {
            false
        }

        fn is_deadline_exceeded(&self) -> bool {
            false
        }
    }

    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn symlink_publish() {}\n")]);
    let store = TempDir::new().expect("store root");
    let mut scheduler = scheduler(
        &fixture,
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    );
    published(scheduler.reconcile_now().expect("publish generation"));
    let latest = scheduler.latest_complete().expect("latest generation");
    let artifact_store = latest.text_artifact_store.clone();
    let generation = latest.generation();
    let sealed_identity = artifact_store
        .sealed_identity(&generation.manifest().generation_id)
        .expect("sealed generation identity");
    let foreign_root = TempDir::new().expect("foreign artifact root");
    let artifacts_root = store.path().join("code-text-artifacts-v1");
    symlink(foreign_root.path(), &artifacts_root).expect("create artifact-root symlink");
    let staging = artifacts_root.join("artifact.staging");
    std::fs::write(&staging, b"foreign artifact bytes").expect("write foreign staging artifact");

    assert!(
        matches!(
            artifact_store.publish(
                &staging,
                &generation.manifest().generation_id,
                &sealed_identity,
                &NeverCancelled,
            ),
            Err(tracedecay_query::retrieval::RetrievalPortError::Contract(_))
        ),
        "publication must not traverse a symlink artifact namespace"
    );
    assert!(
        staging.is_file(),
        "refusal must preserve foreign staging bytes"
    );
}

#[test]
fn missing_durable_text_artifact_is_withdrawn_and_rebuilt() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn rebuilt() {}\n")]);
    let store = TempDir::new().expect("store root");
    {
        let mut scheduler = scheduler(
            &fixture,
            store.path().to_path_buf(),
            Arc::new(SharedCodeIndexBytePoolV1::default()),
        );
        published(scheduler.reconcile_now().expect("publish"));
        let latest = scheduler.latest_complete().expect("latest generation");
        while !latest
            .advance_text_serving(64)
            .expect("build initial text artifact")
        {}
    }
    let artifact_path = active_text_artifact_path(store.path());
    std::fs::remove_file(&artifact_path).expect("remove derived artifact");

    let scheduler = scheduler(
        &fixture,
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    );
    let latest = scheduler.latest_complete().expect("restored generation");
    while !latest
        .advance_text_serving(64)
        .expect("withdraw and rebuild missing artifact")
    {}

    assert!(latest.query_owners_are_warm());
    assert!(
        !latest
            .text_projection_failed
            .load(std::sync::atomic::Ordering::Acquire)
    );
    assert!(active_text_artifact_path(store.path()).is_file());
}

/// A cold generation must finish text-serving owner warmup in one call.
///
/// One bounded advance never finalizes even a small generation, so
/// [`LatestCodeTextGenerationV1::production_query_owners`] keeps driving the
/// resumable build. Activation itself stays one bounded advance.
/// Assert owner readiness -- never elapsed time.
#[test]
fn cold_activation_completes_text_serving_in_one_call() {
    let fixture = GitFixture::new(&[
        ("src/lib.rs", "pub fn cold_activation() {}\n"),
        ("src/second.rs", "pub fn second_unit() -> usize { 2 }\n"),
        ("src/third.rs", "pub fn third_unit() -> usize { 3 }\n"),
    ]);
    let store = TempDir::new().expect("store root");
    let mut scheduler = scheduler(
        &fixture,
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    );
    published(scheduler.reconcile_now().expect("publish generation"));
    let latest = scheduler.latest_complete().expect("latest generation");

    assert!(
        !latest.text_serving_is_ready(),
        "a freshly published generation must start cold"
    );
    latest
        .production_query_owners()
        .expect("cold owner warmup must drive the artifact build to completion");
    assert!(
        latest.text_serving_is_ready(),
        "owner warmup must leave the text serving owners installed"
    );
    assert!(
        !latest.text_serving_needs_work(),
        "a completed warmup must leave no resumable build behind"
    );
}

#[test]
fn corrupt_durable_text_artifact_is_quarantined_and_rebuilt() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn repaired() {}\n")]);
    let store = TempDir::new().expect("store root");
    {
        let mut scheduler = scheduler(
            &fixture,
            store.path().to_path_buf(),
            Arc::new(SharedCodeIndexBytePoolV1::default()),
        );
        published(scheduler.reconcile_now().expect("publish"));
        let latest = scheduler.latest_complete().expect("latest generation");
        while !latest
            .advance_text_serving(64)
            .expect("build initial text artifact")
        {}
    }
    let artifact_path = active_text_artifact_path(store.path());
    let artifact_len = usize::try_from(
        std::fs::metadata(&artifact_path)
            .expect("artifact metadata")
            .len(),
    )
    .expect("artifact length fits usize");
    std::fs::write(&artifact_path, vec![0xa5; artifact_len])
        .expect("corrupt derived artifact bytes");

    let scheduler = scheduler(
        &fixture,
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    );
    let latest = scheduler.latest_complete().expect("restored generation");
    while !latest
        .advance_text_serving(64)
        .expect("withdraw and rebuild corrupt artifact")
    {}

    assert!(latest.query_owners_are_warm());
    let repaired =
        std::fs::read(active_text_artifact_path(store.path())).expect("repaired artifact bytes");
    assert_ne!(repaired, vec![0xa5; artifact_len]);
}

#[test]
fn incompatible_published_text_artifact_is_withdrawn_and_rebuilt() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn migrated() {}\n")]);
    let store = TempDir::new().expect("store root");
    {
        let mut scheduler = scheduler(
            &fixture,
            store.path().to_path_buf(),
            Arc::new(SharedCodeIndexBytePoolV1::default()),
        );
        published(scheduler.reconcile_now().expect("publish"));
        let latest = scheduler.latest_complete().expect("latest generation");
        while !latest
            .advance_text_serving(64)
            .expect("build current-format text artifact")
        {}
    }
    let incompatible_path = rewrite_active_text_artifact_format_revision(store.path(), 1);

    let scheduler = scheduler(
        &fixture,
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    );
    let latest = scheduler.latest_complete().expect("restored generation");
    let mut passes = 0_usize;
    while !latest
        .advance_text_serving(64)
        .expect("withdraw incompatible head and rebuild")
    {
        passes += 1;
        assert!(
            passes < 10_000,
            "incompatible artifact rebuild did not converge"
        );
    }

    assert!(latest.query_owners_are_warm());
    assert_ne!(active_text_artifact_path(store.path()), incompatible_path);
    assert!(
        incompatible_path.is_file(),
        "an incompatible immutable artifact remains bounded orphan evidence for retention"
    );
}

#[test]
fn published_text_artifact_with_stale_search_revision_is_rebuilt() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn migrated() {}\n")]);
    let store = TempDir::new().expect("store root");
    let control = UninterruptibleCodeIndexControlV1;
    let previous_path = {
        let mut scheduler = scheduler(
            &fixture,
            store.path().to_path_buf(),
            Arc::new(SharedCodeIndexBytePoolV1::default()),
        );
        published(scheduler.reconcile_now().expect("publish generation"));
        let latest = scheduler.latest_complete().expect("latest generation");
        let generation_id = latest.metadata.manifest().generation_id.clone();
        let sealed_identity = latest
            .text_artifact_store
            .sealed_identity(&generation_id)
            .expect("sealed identity");
        let mut source = latest
            .take_preopened_source_or_open(&sealed_identity, &control)
            .expect("verified source");
        let mut metadata = latest.text_projection_metadata().expect("current metadata");
        // Pre-qualified-name artifacts must be withdrawn before serving even
        // when the source generation itself has not changed.
        metadata.lexical_retriever_revision =
            ComponentRevision::new("retriever.lexical.daemon.v1").expect("previous revision");
        assert_ne!(
            metadata.lexical_retriever_revision.as_str(),
            tracedecay_query::retrieval::QUERY_LEXICAL_RETRIEVER_REVISION_V1,
            "the fixture must build an artifact under a revision the scheduler no longer serves"
        );
        let root = store.path().join("code-text-artifacts-v1");
        tracedecay_private_fs::create_private_directory(&root).expect("private artifacts root");
        let staging = root.join(".previous-search.staging");
        let mut builder = CodeLexicalArtifactBuilderV1::create(&staging, metadata)
            .expect("previous-revision artifact builder");
        let source_receipt = loop {
            match source.next_page(&control).expect("verified page") {
                VerifiedSealedLexicalPageReadV1::Page(page) => {
                    builder.append_page(&page, &control).expect("append page");
                }
                VerifiedSealedLexicalPageReadV1::Complete(receipt) => break receipt,
            }
        };
        let verified = loop {
            match builder
                .advance_finalization(&source_receipt, 4_096, &control)
                .expect("finalize previous-revision artifact")
            {
                CodeLexicalArtifactFinalizationStepV1::Pending { .. } => {}
                CodeLexicalArtifactFinalizationStepV1::Ready(receipt) => break receipt,
            }
        };
        drop(builder);
        // This is a valid artifact with stale search semantics, not corrupt
        // bytes or an unsupported container format.
        CodeLexicalArtifactReaderV1::open_with_control(
            &staging,
            &verified,
            CODE_LEXICAL_ARTIFACT_QUERY_CACHE_BUDGET_BYTES_V1,
            &control,
        )
        .expect("historical metadata remains readable");
        latest
            .text_artifact_store
            .publish(&staging, &generation_id, &sealed_identity, &control)
            .expect("publish previous-revision artifact");
        active_text_artifact_path(store.path())
    };
    let scheduler = scheduler(
        &fixture,
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    );
    let latest = scheduler.latest_complete().expect("restored generation");
    assert!(
        !latest
            .advance_text_serving(1)
            .expect("reject stale search revision"),
        "an incompatible search artifact must rebuild before serving"
    );
    let mut passes = 0;
    while !latest
        .advance_text_serving(64)
        .expect("rebuild current search fields")
    {
        passes += 1;
        assert!(passes < 10_000, "search-revision rebuild did not converge");
    }
    assert!(latest.query_owners_are_warm());
    assert_ne!(active_text_artifact_path(store.path()), previous_path);
    assert!(
        previous_path.is_file(),
        "retention owns the superseded artifact"
    );
}

/// A projection whose final source page fills exactly on its last file's last
/// record is convergeable, and the durable text lane must converge it.
///
/// A completed source mints one terminal read that emits no record and only
/// normalizes the exhausted file position, so its live cursor sits one file
/// rollover past the last durably accepted page exactly when that page filled
/// on a file boundary. Holding the builder's durable progress against that
/// normalized cursor — instead of against the completion receipt — reports a
/// deterministic contract violation, which parks the text projection as
/// unconvergeable and leaves the complete serving seat permanently empty.
#[test]
fn page_aligned_final_source_page_converges_the_text_projection() {
    // A whole-page multiple of symbols in one file keeps the last page filling
    // on the file's last record for any per-symbol chunk count.
    let source = (0..super::super::TEXT_ARTIFACT_PAGE_CHUNKS_V1 * 2).fold(
        String::new(),
        |mut source, index| {
            writeln!(
                &mut source,
                "pub fn aligned_{index}() -> usize {{ {index} }}"
            )
            .expect("write page-aligned source fixture");
            source
        },
    );
    let fixture = GitFixture::new(&[("src/lib.rs", source.as_str())]);
    let store = TempDir::new().expect("store root");
    let mut scheduler = scheduler(
        &fixture,
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    );
    published(scheduler.reconcile_now().expect("publish"));
    let latest = scheduler.latest_complete().expect("latest generation");
    let mut passes = 0_usize;
    while !latest
        .advance_text_serving(64)
        .expect("a page-aligned projection converges")
    {
        passes += 1;
        assert!(
            passes < 10_000,
            "page-aligned text projection did not converge"
        );
    }
    assert!(latest.query_owners_are_warm());
    let progress = build_progress_snapshot(&scheduler);
    // Every committed page must be chunk-full: an early commit from the page
    // byte bound or an import record would leave the final page partial, which
    // is exactly the shape that does not trip this invariant.
    assert_eq!(
        progress.committed_chunks,
        progress.committed_pages * super::super::TEXT_ARTIFACT_PAGE_CHUNKS_V1 as u64,
        "the fixture must keep every page chunk-full so the final page ends on the last record"
    );
    assert_eq!(
        progress.committed_imports, 0,
        "an import record would commit a partial page and break the alignment"
    );
}

#[test]
fn incompatible_partial_text_artifact_is_discarded_and_rebuilt() {
    let source = (0..256).fold(String::new(), |mut source, index| {
        writeln!(
            &mut source,
            "pub fn staged_{index}() -> usize {{ {index} }}"
        )
        .expect("write staged source fixture");
        source
    });
    let fixture = GitFixture::new(&[("src/lib.rs", source.as_str())]);
    let store = TempDir::new().expect("store root");
    {
        let mut scheduler = scheduler(
            &fixture,
            store.path().to_path_buf(),
            Arc::new(SharedCodeIndexBytePoolV1::default()),
        );
        published(scheduler.reconcile_now().expect("publish"));
        let latest = scheduler.latest_complete().expect("latest generation");
        assert!(
            !latest
                .advance_text_serving(1)
                .expect("start bounded text artifact build"),
            "one page must leave resumable staging state"
        );
    }
    let artifacts_root = store.path().join("code-text-artifacts-v1");
    let staging_path = std::fs::read_dir(&artifacts_root)
        .expect("read artifacts root")
        .map(|entry| entry.expect("artifact entry").path())
        .find(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with(".staging"))
        })
        .expect("partial staging database");
    {
        let connection =
            rusqlite::Connection::open(&staging_path).expect("open partial staging database");
        assert_eq!(
            connection
                .execute("UPDATE artifact_state SET format_revision = 1", [],)
                .expect("rewrite staging format revision"),
            1
        );
    }

    let scheduler = scheduler(
        &fixture,
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    );
    let latest = scheduler.latest_complete().expect("restored generation");
    let mut passes = 0_usize;
    while !latest
        .advance_text_serving(64)
        .expect("discard incompatible staging and rebuild")
    {
        passes += 1;
        assert!(
            passes < 10_000,
            "incompatible staging rebuild did not converge"
        );
    }

    assert!(latest.query_owners_are_warm());
    assert!(active_text_artifact_path(store.path()).is_file());
}

#[test]
fn invalid_partial_text_artifact_cursor_is_discarded_and_rebuilt() {
    let mut source = "import { readFileSync } from 'node:fs';\n".to_owned();
    for index in 0..256 {
        writeln!(
            &mut source,
            "export function staleCursor{index}(): number {{ return {index}; }}"
        )
        .expect("write staged source fixture");
    }
    let fixture = GitFixture::new(&[("src/lib.ts", source.as_str())]);
    let store = TempDir::new().expect("store root");
    let (generation_id, snapshot) = {
        let mut scheduler = scheduler(
            &fixture,
            store.path().to_path_buf(),
            Arc::new(SharedCodeIndexBytePoolV1::default()),
        );
        published(scheduler.reconcile_now().expect("publish"));
        let latest = scheduler.latest_complete().expect("latest generation");
        assert!(
            !latest
                .advance_text_serving(1)
                .expect("start bounded text artifact build"),
            "one page must leave resumable staging state"
        );
        (
            latest.metadata().manifest().generation_id.clone(),
            latest.metadata().snapshot().clone(),
        )
    };
    let artifacts_root = store.path().join("code-text-artifacts-v1");
    let staging_path = std::fs::read_dir(&artifacts_root)
        .expect("read artifacts root")
        .map(|entry| entry.expect("artifact entry").path())
        .find(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with(".staging"))
        })
        .expect("partial staging database");
    {
        let connection =
            rusqlite::Connection::open(&staging_path).expect("open partial staging database");
        let cursor_bytes: Vec<u8> = connection
            .query_row(
                "SELECT next_cursor FROM source_pages ORDER BY page_ordinal DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .expect("read persisted cursor");
        let mut cursor: Vec<serde_json::Value> =
            serde_json::from_slice(&cursor_bytes).expect("decode persisted cursor");
        assert_eq!(
            cursor[1].as_u64(),
            Some(0),
            "cursor must remain in its file"
        );
        assert!(
            cursor[3].as_u64().is_some_and(|ordinal| ordinal > 0),
            "first page must advance within the file's chunks"
        );
        cursor[3] = serde_json::Value::from(0_u64);
        cursor[7] = serde_json::Value::from(1_u64);

        let text = |index: usize| {
            cursor[index]
                .as_str()
                .expect("cursor digest field")
                .as_bytes()
        };
        let number = |index: usize| cursor[index].as_u64().expect("cursor numeric field");
        let mut hasher = Sha256::new();
        hasher.update(b"tracedecay.sealed-lexical-cursor.v1\0");
        hasher.update(
            u64::try_from(text(0).len())
                .expect("digest length")
                .to_le_bytes(),
        );
        hasher.update(text(0));
        for index in [1, 2, 3, 7, 4, 5, 6, 8, 9] {
            hasher.update(number(index).to_le_bytes());
        }
        for index in [10, 11] {
            hasher.update(
                u64::try_from(text(index).len())
                    .expect("digest length")
                    .to_le_bytes(),
            );
            hasher.update(text(index));
        }
        cursor[12] = serde_json::Value::from(format!(
            "sha256:{}",
            encode_lowercase_hex(&hasher.finalize())
        ));
        let cursor_bytes = serde_json::to_vec(&cursor).expect("encode invalid persisted cursor");
        connection
            .execute_batch("DROP TRIGGER immutable_source_pages_update")
            .expect("open immutable page fixture for corruption");
        assert_eq!(
            connection
                .execute(
                    "UPDATE source_pages SET next_cursor = ?1 WHERE page_ordinal = \
                     (SELECT MAX(page_ordinal) FROM source_pages)",
                    [cursor_bytes],
                )
                .expect("rewrite persisted cursor"),
            1
        );
        connection
            .execute_batch(
                "CREATE TRIGGER immutable_source_pages_update BEFORE UPDATE ON source_pages \
                 BEGIN SELECT RAISE(ABORT, 'immutable lexical source pages'); END",
            )
            .expect("restore immutable page contract");
    }

    let scheduler = scheduler(
        &fixture,
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    );
    let latest = scheduler.latest_complete().expect("restored generation");
    assert_eq!(latest.metadata().manifest().generation_id, generation_id);
    assert_eq!(latest.metadata().snapshot(), &snapshot);
    let mut passes = 0_usize;
    while !latest
        .advance_text_serving(64)
        .expect("discard invalid cursor and rebuild")
    {
        passes += 1;
        assert!(passes < 10_000, "invalid cursor rebuild did not converge");
    }

    assert!(latest.query_owners_are_warm());
    assert!(active_text_artifact_path(store.path()).is_file());
    assert!(
        !staging_path.exists(),
        "invalid resumable staging state must not survive repair"
    );
}

#[test]
fn reader_reservation_refusal_precedes_missing_artifact_access() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn reserved_first() {}\n")]);
    let store = TempDir::new().expect("store root");
    {
        let mut scheduler = scheduler(
            &fixture,
            store.path().to_path_buf(),
            Arc::new(SharedCodeIndexBytePoolV1::default()),
        );
        published(scheduler.reconcile_now().expect("publish"));
        let latest = scheduler.latest_complete().expect("latest generation");
        while !latest
            .advance_text_serving(64)
            .expect("build initial text artifact")
        {}
    }
    let artifact_path = active_text_artifact_path(store.path());
    std::fs::remove_file(&artifact_path).expect("remove derived artifact");

    let mut scheduler = scheduler(
        &fixture,
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    );
    scheduler.bind_resident_memory(Arc::new(ProcessResidentMemoryV1::new(
        std::num::NonZeroU64::new(1024 * 1024).expect("tight memory limit"),
    )));
    let latest = scheduler.latest_complete().expect("restored generation");
    assert_eq!(
        latest.advance_text_serving(1),
        Err(tracedecay_query::retrieval::RetrievalPortError::BudgetExceeded),
        "the reservation gate must win before the missing path is inspected"
    );
    assert!(
        !artifact_path.exists(),
        "reservation refusal must not touch or recreate the missing path"
    );
    assert!(latest.text_serving_needs_work());
}

#[test]
fn overlapping_text_builds_share_one_admission_watermark_headroom() {
    let limit_bytes = 1_000_u64;
    let watermark_headroom = 100_u64;
    let requested = NonZeroU64::new(200).expect("nonzero build request");
    let mut used_bytes = 0_u64;

    for observed_bytes in [300_u64, 500, 700] {
        let unmodeled_live_bytes = observed_bytes.saturating_sub(used_bytes);
        let (accounted, retained) = super::super::text_artifact_resident_memory_charges(
            requested,
            unmodeled_live_bytes,
            watermark_headroom,
        )
        .expect("bounded admission accounting");
        assert!(
            used_bytes + accounted.get() <= limit_bytes,
            "each overlapping build fits beneath the same 900-byte high watermark"
        );
        used_bytes += retained.get();
    }

    assert_eq!(
        used_bytes, 900,
        "the retained ledger owns one observed baseline plus three build ceilings"
    );
    for overflow in [
        super::super::text_artifact_resident_memory_charges(
            NonZeroU64::new(u64::MAX).expect("maximum nonzero request"),
            1,
            0,
        ),
        super::super::text_artifact_resident_memory_charges(requested, 0, u64::MAX),
    ] {
        assert!(
            matches!(
                overflow,
                Err(tracedecay_query::retrieval::RetrievalPortError::Contract(_))
            ),
            "overflow must remain a typed contract refusal: {overflow:?}"
        );
    }
}

/// The artifact build and reader ceilings must reserve through the process
/// resident-memory authority: an authority too small for the advertised
/// build ceiling refuses the build as a typed unavailability, and a serving
/// artifact holds its measured reader charge until its owners are dropped.
#[test]
fn text_artifact_ceilings_reserve_through_process_resident_memory() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn reserved() {}\n")]);
    let store = TempDir::new().expect("store root");

    // An authority below the advertised build ceiling refuses the build.
    {
        let mut scheduler = scheduler(
            &fixture,
            store.path().to_path_buf(),
            Arc::new(SharedCodeIndexBytePoolV1::default()),
        );
        published(scheduler.reconcile_now().expect("publish"));
        let tight = Arc::new(ProcessResidentMemoryV1::new(
            std::num::NonZeroU64::new(1024 * 1024).expect("tight limit"),
        ));
        scheduler.bind_resident_memory(Arc::clone(&tight));
        let latest = scheduler.latest_complete().expect("latest generation");
        let denied = latest.advance_text_serving(usize::MAX);
        assert!(
            matches!(
                denied,
                Err(tracedecay_query::retrieval::RetrievalPortError::BudgetExceeded)
            ),
            "an unreservable build ceiling must refuse as a typed budget state: {denied:?}"
        );
        assert_eq!(
            tight.snapshot().used_bytes,
            0,
            "a denied reservation must not leak a charge"
        );
    }

    // A request that fits the empty modeled ledger must still account the
    // process's freshly measured, unmodeled live set before allocating.
    if let Some(observed_bytes) = sampled_process_resident_bytes_v1() {
        let build_bytes = u64::try_from(CODE_LEXICAL_ARTIFACT_BUILD_MEMORY_BUDGET_BYTES_V1)
            .expect("build ceiling fits u64");
        let measured_limit = NonZeroU64::new(build_bytes.saturating_add(observed_bytes / 2))
            .expect("measured test limit");
        let measured = Arc::new(ProcessResidentMemoryV1::with_pressure(
            measured_limit,
            Arc::new(ResidentMemoryPressureV1::new(measured_limit)),
        ));
        let mut scheduler = scheduler(
            &fixture,
            store.path().to_path_buf(),
            Arc::new(SharedCodeIndexBytePoolV1::default()),
        );
        scheduler.bind_resident_memory(Arc::clone(&measured));
        let latest = scheduler
            .latest_complete()
            .expect("measured latest generation");
        assert_eq!(
            latest.advance_text_serving(1),
            Err(tracedecay_query::retrieval::RetrievalPortError::BudgetExceeded),
            "fresh RSS plus the requested build ceiling exceeds the process authority"
        );
        assert_eq!(
            measured.snapshot().used_bytes,
            0,
            "a measured-baseline refusal must not leak a charge"
        );
    }

    // An adequate authority admits the build and holds the reader charge
    // for as long as the artifact owners serve.
    let adequate = Arc::new(ProcessResidentMemoryV1::new(
        DEFAULT_PROCESS_RESIDENT_MEMORY_LIMIT_V1,
    ));
    {
        // A fresh scheduler restores the durable generation; the unchanged
        // fixture needs no republish.
        let mut scheduler = scheduler(
            &fixture,
            store.path().to_path_buf(),
            Arc::new(SharedCodeIndexBytePoolV1::default()),
        );
        scheduler.bind_resident_memory(Arc::clone(&adequate));
        let latest = scheduler
            .latest_complete()
            .expect("fresh latest generation");
        while !latest
            .advance_text_serving(64)
            .expect("advance artifact build under an adequate authority")
        {}
        let snapshot = adequate.snapshot();
        assert!(
            snapshot.charges.iter().any(|charge| {
                charge.key.component.as_str() == "code-text-artifact-reader" && charge.bytes > 0
            }),
            "serving artifact owners must hold the measured reader charge: {snapshot:?}"
        );
        assert!(
            !snapshot
                .charges
                .iter()
                .any(|charge| charge.key.component.as_str() == "code-text-artifact-build"),
            "the build ceiling must be released once the artifact serves: {snapshot:?}"
        );
    }
    assert_eq!(
        adequate.snapshot().used_bytes,
        0,
        "dropping the serving owners must release every artifact charge"
    );
}

#[test]
fn oversized_activation_hint_is_clamped_to_bounded_text_work() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn bounded_activation() {}\n")]);
    let store = TempDir::new().expect("store root");
    let mut scheduler = scheduler(
        &fixture,
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    );
    published(scheduler.reconcile_now().expect("publish generation"));
    let latest = scheduler.latest_complete().expect("latest generation");

    assert!(
        latest.text_serving_needs_work(),
        "typed warming must preserve the resumable artifact build"
    );
    let mut passes = 0_usize;
    while !latest
        .advance_text_serving(usize::MAX)
        .expect("oversized hints must remain bounded and resumable")
    {
        passes += 1;
        assert!(
            passes < 10_000,
            "bounded artifact activation did not converge"
        );
    }
    assert!(latest.text_serving_is_ready());
}

#[test]
fn text_artifact_hash_honors_cancellation_between_bounded_reads() {
    struct CancelAfterFirstRead {
        checkpoints: std::sync::atomic::AtomicUsize,
    }

    impl CodeIndexExecutionControlV1 for CancelAfterFirstRead {
        fn is_cancelled(&self) -> bool {
            self.checkpoints
                .fetch_add(1, std::sync::atomic::Ordering::AcqRel)
                >= 1
        }

        fn is_deadline_exceeded(&self) -> bool {
            false
        }
    }

    let directory = TempDir::new().expect("hash fixture root");
    let staging = directory.path().join("artifact.staging");
    let mut staging_file =
        tracedecay_private_fs::create_private_file(&staging).expect("create hash fixture");
    std::io::Write::write_all(&mut staging_file, &vec![7_u8; 3 * 64 * 1024])
        .expect("write hash fixture");
    drop(staging_file);
    let control = CancelAfterFirstRead {
        checkpoints: std::sync::atomic::AtomicUsize::new(0),
    };

    assert_eq!(
        super::super::sha256_private_file_and_size(&staging, &control).map(|(digest, _)| digest),
        Err(tracedecay_query::retrieval::RetrievalPortError::Cancelled)
    );
    assert!(
        staging.is_file(),
        "cancelled publication hashing must preserve resumable staging bytes"
    );
}

#[test]
fn text_artifact_hash_rejects_non_regular_staging_paths() {
    struct NeverCancelled;

    impl CodeIndexExecutionControlV1 for NeverCancelled {
        fn is_cancelled(&self) -> bool {
            false
        }

        fn is_deadline_exceeded(&self) -> bool {
            false
        }
    }

    let directory = TempDir::new().expect("hash fixture root");
    let staging_directory = directory.path().join("artifact.staging");
    std::fs::create_dir(&staging_directory).expect("create non-regular staging path");

    assert!(
        matches!(
            super::super::sha256_private_file_and_size(&staging_directory, &NeverCancelled),
            Err(tracedecay_query::retrieval::RetrievalPortError::Contract(_))
        ),
        "publication hashing must reject a non-regular staging path as unsafe input"
    );
}

#[cfg(unix)]
#[test]
fn text_artifact_hash_rejects_symlink_staging_paths() {
    use std::os::unix::fs::symlink;

    struct NeverCancelled;

    impl CodeIndexExecutionControlV1 for NeverCancelled {
        fn is_cancelled(&self) -> bool {
            false
        }

        fn is_deadline_exceeded(&self) -> bool {
            false
        }
    }

    let directory = TempDir::new().expect("hash fixture root");
    let target = directory.path().join("artifact-target.bin");
    let staging = directory.path().join("artifact.staging");
    std::fs::write(&target, b"artifact bytes").expect("write symlink target");
    symlink(&target, &staging).expect("create staging symlink");

    assert!(
        matches!(
            super::super::sha256_private_file_and_size(&staging, &NeverCancelled),
            Err(tracedecay_query::retrieval::RetrievalPortError::Contract(_))
        ),
        "publication hashing must not follow a staging symlink"
    );
    assert!(
        target.is_file(),
        "refusing the symlink preserves its target"
    );
}

#[test]
fn text_artifact_hash_rejects_a_named_file_replaced_during_hashing() {
    struct ReplaceAfterFirstRead {
        checkpoints: std::sync::atomic::AtomicUsize,
        path: PathBuf,
        replacement: PathBuf,
    }

    impl CodeIndexExecutionControlV1 for ReplaceAfterFirstRead {
        fn is_cancelled(&self) -> bool {
            if self
                .checkpoints
                .fetch_add(1, std::sync::atomic::Ordering::AcqRel)
                == 1
            {
                std::fs::rename(&self.replacement, &self.path)
                    .expect("atomically replace the named staging file");
            }
            false
        }

        fn is_deadline_exceeded(&self) -> bool {
            false
        }
    }

    let directory = TempDir::new().expect("hash fixture root");
    let staging = directory.path().join("artifact.staging");
    let replacement = directory.path().join("replacement.bin");
    let mut staging_file =
        tracedecay_private_fs::create_private_file(&staging).expect("create staging file");
    std::io::Write::write_all(&mut staging_file, &vec![7_u8; 2 * 64 * 1024])
        .expect("write original staging file");
    drop(staging_file);
    let mut replacement_file =
        tracedecay_private_fs::create_private_file(&replacement).expect("create replacement file");
    std::io::Write::write_all(&mut replacement_file, &vec![9_u8; 2 * 64 * 1024])
        .expect("write replacement file");
    drop(replacement_file);
    let control = ReplaceAfterFirstRead {
        checkpoints: std::sync::atomic::AtomicUsize::new(0),
        path: staging.clone(),
        replacement,
    };

    assert!(
        matches!(
            super::super::sha256_private_file_and_size(&staging, &control),
            Err(tracedecay_query::retrieval::RetrievalPortError::Contract(_))
        ),
        "the hashed handle must still be the regular file named by the staging path"
    );
}

#[test]
fn text_artifact_subdivision_yields_without_advancing_and_stops_at_one_chunk() {
    struct SubdivisionControl {
        cancelled: bool,
    }
    impl CodeIndexExecutionControlV1 for SubdivisionControl {
        fn is_cancelled(&self) -> bool {
            self.cancelled
        }
        fn is_deadline_exceeded(&self) -> bool {
            false
        }
    }
    let source = (0..256).fold(String::new(), |mut source, ordinal| {
        let _ = writeln!(
            source,
            "pub fn subdivision_{ordinal}() -> usize {{ {ordinal} }}"
        );
        source
    });
    let fixture = GitFixture::new(&[("src/lib.rs", source.as_str())]);
    let store = TempDir::new().unwrap();
    let mut scheduler = scheduler(
        &fixture,
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    );
    published(scheduler.reconcile_now().unwrap());
    let latest = scheduler.latest_complete().unwrap();
    assert!(!latest.advance_text_serving(1).unwrap());
    let before = {
        let mut slot = latest.text_projection_build.lock_slot();
        let super::super::CodeTextProjectionSlotV1::Building(build) = &mut *slot else {
            panic!("partial text build");
        };
        let progress = build.builder.progress().unwrap();
        assert!(progress.next_page_ordinal > 0);
        build.builder =
            CodeLexicalArtifactBuilderV1::open_or_resume_with_memory_budget_and_control(
                &build.staging_path,
                latest.text_projection_metadata().unwrap(),
                build.builder.fixed_ledger_charge_bytes() + 1,
                &SubdivisionControl { cancelled: false },
            )
            .unwrap();
        progress
    };
    let mut retries = 0;
    loop {
        match latest.advance_text_serving(1) {
            Ok(false) => {
                retries += 1;
                assert!(
                    retries <= 16,
                    "subdivision must terminate within the initial page bound"
                );
                assert_eq!(
                    latest
                        .advance_artifact_text_serving(1, &SubdivisionControl { cancelled: true }),
                    Err(tracedecay_query::retrieval::RetrievalPortError::Cancelled)
                );
            }
            Err(tracedecay_query::retrieval::RetrievalPortError::BudgetExceeded) => break,
            other => panic!("unexpected projection outcome: {other:?}"),
        }
        let slot = latest.text_projection_build.lock_slot();
        let super::super::CodeTextProjectionSlotV1::Building(build) = &*slot else {
            panic!("refusal keeps resumable build");
        };
        assert_eq!(build.builder.progress().unwrap(), before);
        assert_eq!(Some(build.source.cursor()), before.next_cursor.as_ref());
    }
    assert!(
        retries > 0,
        "oversized page must subdivide before terminal refusal"
    );
    let slot = latest.text_projection_build.lock_slot();
    let super::super::CodeTextProjectionSlotV1::Building(build) = &*slot else {
        panic!("indivisible refusal keeps durable prefix");
    };
    assert_eq!(build.builder.progress().unwrap(), before);
    assert_eq!(Some(build.source.cursor()), before.next_cursor.as_ref());
}

#[test]
fn source_window_and_builder_share_one_memory_reservation() {
    let ceiling =
        tracedecay_query::retrieval::lexical::CODE_LEXICAL_ARTIFACT_BUILD_MEMORY_BUDGET_BYTES_V1;
    assert_eq!(
        super::super::text_artifact_builder_budget(ceiling, ceiling - 1),
        Ok(1),
        "the source window must be subtracted from the builder's authority"
    );
    assert_eq!(
        super::super::text_artifact_builder_budget(ceiling, ceiling),
        Err(tracedecay_query::retrieval::RetrievalPortError::BudgetExceeded),
        "a source consuming the reservation must refuse before builder path access"
    );
}

#[test]
fn failed_background_text_task_is_terminal_typed_unavailable() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn indexed() {}\n")]);
    let store = TempDir::new().expect("store root");
    let mut scheduler = scheduler(
        &fixture,
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    );
    published(scheduler.reconcile_now().expect("publish"));
    let latest = scheduler.latest_complete().expect("latest generation");

    latest.mark_text_serving_failed();

    assert!(
        !latest.text_serving_needs_work(),
        "a failed task must not arm an unbounded self-wake loop"
    );
    match latest.production_query_owners_with_budget(&RetrievalBudget {
        max_candidates_per_lane: 1,
        max_fused_candidates: 1,
        max_hydrated_results: 1,
        max_hydration_bytes: 1,
        deadline_micros: None,
    }) {
        Err(tracedecay_query::retrieval::RetrievalPortError::AuthorityUnavailable(reason)) => {
            assert!(reason.contains("projection failed"));
        }
        Err(error) => panic!("failed projection returned the wrong typed state: {error:?}"),
        Ok(_) => panic!("failed projection must not fabricate serving owners"),
    }
}

#[test]
fn concurrent_background_wakes_share_one_generation_owned_text_builder() {
    let sources = (0..8)
        .map(|ordinal| {
            (
                format!("src/file_{ordinal}.rs"),
                format!("pub fn symbol_{ordinal}() -> usize {{ {ordinal} }}\n"),
            )
        })
        .collect::<Vec<_>>();
    let borrowed = sources
        .iter()
        .map(|(path, source)| (path.as_str(), source.as_str()))
        .collect::<Vec<_>>();
    let fixture = GitFixture::new(&borrowed);
    let store = TempDir::new().expect("store root");
    let mut scheduler = scheduler(
        &fixture,
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    );
    published(scheduler.reconcile_now().expect("publish"));
    let first = scheduler.latest_complete().expect("first latest handle");
    let second = scheduler.latest_complete().expect("second latest handle");
    assert!(Arc::ptr_eq(
        &first.text_projection_build,
        &second.text_projection_build
    ));

    let first_wake = std::thread::spawn(move || first.advance_text_serving(1));
    let second_wake = std::thread::spawn(move || second.advance_text_serving(1));
    assert!(first_wake.join().expect("first wake joined").is_ok());
    assert!(second_wake.join().expect("second wake joined").is_ok());

    let latest = scheduler.latest_complete().expect("latest generation");
    // The durable-artifact journey may complete within two bounded page
    // windows; until it does, both wakes must have fed one shared partial
    // build rather than each starting their own.
    assert!(
        latest.query_owners_are_warm()
            || matches!(
                &*latest.text_projection_build.lock_slot(),
                super::super::CodeTextProjectionSlotV1::Building(_)
            ),
        "bounded concurrent wakes must retain one shared partial projection"
    );
    while !latest
        .advance_text_serving(1)
        .expect("finish shared text projection")
    {}
    assert!(latest.query_owners_are_warm());
}

#[test]
fn text_progress_rate_and_eta_require_two_monotonic_committed_samples() {
    let mut state = CodeIndexBuildProgressStateV1::new();
    let first = Instant::now();
    state.observe_committed(CodeIndexCommittedProgressSampleV1 {
        observed_at: first,
        completed_files: 20,
        completed_lexical_units: 4_000_000,
    });
    assert_eq!(state.rates_and_eta(29_000_000), (None, None, None));

    state.observe_committed(CodeIndexCommittedProgressSampleV1 {
        observed_at: first + Duration::from_secs(2),
        completed_files: 21,
        completed_lexical_units: 14_000_000,
    });
    let (files_per_second, lexical_units_per_second, eta_seconds) = state.rates_and_eta(29_000_000);
    assert_eq!(files_per_second, Some(0.5));
    assert_eq!(lexical_units_per_second, Some(5_000_000.0));
    assert_eq!(
        eta_seconds,
        Some(3),
        "ETA must use the multi-page lexical span, not uneven completed-file boundaries"
    );
}

#[test]
fn progress_owner_epoch_rejects_original_a_after_a_b_a_replacement() {
    let generation_a = CodeGenerationId::new("generation.progress-a").expect("generation A");
    let generation_b = CodeGenerationId::new("generation.progress-b").expect("generation B");
    let mut slot = super::super::CodeIndexBuildProgressSlotStateV1::default();

    let original_a_owner = slot.replace_generation(generation_a.clone());
    assert!(slot.publish(
        &generation_a,
        original_a_owner,
        progress_snapshot_for_generation(&generation_a, 1),
    ));

    let generation_b_owner = slot.replace_generation(generation_b.clone());
    assert!(slot.publish(
        &generation_b,
        generation_b_owner,
        progress_snapshot_for_generation(&generation_b, 2),
    ));

    let replacement_a_owner = slot.replace_generation(generation_a.clone());
    assert_ne!(replacement_a_owner, original_a_owner);
    assert!(slot.publish(
        &generation_a,
        replacement_a_owner,
        progress_snapshot_for_generation(&generation_a, 3),
    ));
    let replacement_a = slot.snapshot().expect("replacement A snapshot");

    assert!(!slot.publish(
        &generation_a,
        original_a_owner,
        progress_snapshot_for_generation(&generation_a, 99),
    ));
    let after_stale_original = slot.snapshot().expect("current A snapshot");
    assert_eq!(after_stale_original.generation_id, generation_a.as_str());
    assert_eq!(after_stale_original.committed_pages, 3);
    assert_eq!(
        after_stale_original.progress_epoch, replacement_a.progress_epoch,
        "matching generation identity must not let the original A owner replace the newer A epoch"
    );
}

#[test]
fn dashboard_progress_advances_only_after_durable_batch_commit() {
    struct CancelAfterPreparation {
        progress: super::super::CodeIndexBuildProgressSlotV1,
        observed_bulk_commit: std::sync::atomic::AtomicBool,
    }

    impl CodeIndexExecutionControlV1 for CancelAfterPreparation {
        fn is_cancelled(&self) -> bool {
            let cancel = self
                .progress
                .read()
                .expect("cancellation progress slot")
                .snapshot()
                .is_some_and(|snapshot| {
                    snapshot.phase
                        == tracedecay_contracts::code_index_freshness::CodeIndexBuildPhaseV1::BulkCommit
                });
            if cancel {
                self.observed_bulk_commit
                    .store(true, std::sync::atomic::Ordering::Release);
            }
            cancel
        }

        fn is_deadline_exceeded(&self) -> bool {
            false
        }
    }

    let mut source = String::new();
    for ordinal in 0..600 {
        writeln!(
            source,
            "pub fn cancellation_symbol_{ordinal}() -> usize {{ {ordinal} }}"
        )
        .expect("write cancellation fixture");
    }
    let fixture = GitFixture::new(&[("src/lib.rs", source.as_str())]);
    let store = TempDir::new().expect("store root");
    let mut scheduler = scheduler(
        &fixture,
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    );
    published(scheduler.reconcile_now().expect("publish"));
    let latest = scheduler.latest_complete().expect("latest generation");
    assert!(
        !latest
            .advance_text_serving(1)
            .expect("start bounded text build")
    );
    let progress_before = {
        let slot = latest.text_projection_build.lock_slot();
        let super::super::CodeTextProjectionSlotV1::Building(build) = &*slot else {
            panic!("partial build");
        };
        build.builder.progress().expect("durable progress")
    };
    let dashboard_before = build_progress_snapshot(&scheduler);
    assert_eq!(
        dashboard_before.committed_pages,
        progress_before.next_page_ordinal
    );
    assert_eq!(dashboard_before.committed_pages, 1);
    assert!(
        dashboard_before.completed_files < dashboard_before.total_files,
        "the fixture must retain more authenticated source work after its first page"
    );

    let control = CancelAfterPreparation {
        progress: scheduler.build_progress_slot(),
        observed_bulk_commit: std::sync::atomic::AtomicBool::new(false),
    };
    let cancellation_started = Instant::now();
    let cancellation = latest.advance_artifact_text_serving(1, &control);
    assert_eq!(
        cancellation,
        Err(tracedecay_query::retrieval::RetrievalPortError::Cancelled)
    );
    assert!(
        cancellation_started.elapsed() < Duration::from_secs(1),
        "a cancelled scheduler batch must yield within the focused latency bound"
    );
    assert!(
        control
            .observed_bulk_commit
            .load(std::sync::atomic::Ordering::Acquire),
        "cancellation must occur after preparation publishes the bulk-commit boundary"
    );
    let progress_after = {
        let slot = latest.text_projection_build.lock_slot();
        let super::super::CodeTextProjectionSlotV1::Building(build) = &*slot else {
            panic!("cancelled build remains resumable");
        };
        build
            .builder
            .progress()
            .expect("durable progress after cancellation")
    };
    assert_eq!(progress_after, progress_before);
    let dashboard_after = build_progress_snapshot(&scheduler);
    assert_eq!(
        dashboard_after.phase,
        tracedecay_contracts::code_index_freshness::CodeIndexBuildPhaseV1::BulkCommit
    );
    assert_eq!(dashboard_after.current_batch_pages, 1);
    assert_eq!(
        dashboard_after.committed_pages, dashboard_before.committed_pages,
        "a prepared but cancelled batch must not publish staged pages as committed"
    );
    assert_eq!(
        dashboard_after.completed_lexical_units, dashboard_before.completed_lexical_units,
        "a cancelled batch must retain the prior exact sealed-source boundary"
    );
    assert!(latest.text_serving_needs_work());

    while !latest
        .advance_text_serving(8)
        .expect("resume durable text build")
    {}
    assert!(latest.query_owners_are_warm());
    let dashboard_ready = build_progress_snapshot(&scheduler);
    assert_eq!(
        dashboard_ready.phase,
        tracedecay_contracts::code_index_freshness::CodeIndexBuildPhaseV1::Ready
    );
    assert!(dashboard_ready.progress_epoch > dashboard_after.progress_epoch);
}

#[test]
fn reopen_reconstructs_exact_committed_progress_without_fabricating_rate() {
    let sources = (0..12)
        .map(|ordinal| {
            (
                format!("src/reopen_{ordinal}.rs"),
                format!("pub fn reopen_symbol_{ordinal}() -> usize {{ {ordinal} }}\n"),
            )
        })
        .collect::<Vec<_>>();
    let borrowed = sources
        .iter()
        .map(|(path, source)| (path.as_str(), source.as_str()))
        .collect::<Vec<_>>();
    let fixture = GitFixture::new(&borrowed);
    let store = TempDir::new().expect("store root");
    let bytes = Arc::new(SharedCodeIndexBytePoolV1::default());
    let committed = {
        let mut scheduler = scheduler(&fixture, store.path().to_path_buf(), Arc::clone(&bytes));
        published(scheduler.reconcile_now().expect("publish generation"));
        let latest = scheduler.latest_complete().expect("latest generation");
        assert!(
            !latest
                .advance_text_serving(1)
                .expect("commit one bounded source batch")
        );
        let progress = build_progress_snapshot(&scheduler);
        assert!(progress.committed_pages > 0);
        assert_eq!(progress.files_per_second, None);
        assert_eq!(progress.lexical_units_per_second, None);
        assert_eq!(progress.estimated_remaining_seconds, None);
        progress
    };

    let reopened = scheduler(&fixture, store.path().to_path_buf(), bytes);
    let latest = reopened.latest_complete().expect("reopened generation");
    assert!(
        !latest
            .advance_text_serving(0)
            .expect("reconstruct durable cursor snapshot")
    );
    let reconstructed = build_progress_snapshot(&reopened);
    assert_eq!(reconstructed.generation_id, committed.generation_id);
    assert_eq!(reconstructed.committed_pages, committed.committed_pages);
    assert_eq!(reconstructed.committed_chunks, committed.committed_chunks);
    assert_eq!(reconstructed.committed_imports, committed.committed_imports);
    assert_eq!(
        reconstructed.committed_payload_bytes,
        committed.committed_payload_bytes
    );
    assert_eq!(reconstructed.completed_files, committed.completed_files);
    assert_eq!(
        reconstructed.completed_lexical_units,
        committed.completed_lexical_units
    );
    assert_eq!(reconstructed.files_per_second, None);
    assert_eq!(reconstructed.lexical_units_per_second, None);
    assert_eq!(reconstructed.estimated_remaining_seconds, None);
}

#[test]
fn generation_replacement_drops_incomplete_text_projection_state() {
    let fixture = GitFixture::new(&[
        ("src/first.rs", "pub fn first() {}\n"),
        ("src/second.rs", "pub fn second() {}\n"),
        ("src/third.rs", "pub fn third() {}\n"),
    ]);
    let store = TempDir::new().expect("store root");
    let mut scheduler = scheduler(
        &fixture,
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    );
    published(scheduler.reconcile_now().expect("publish first generation"));
    let original = scheduler.latest_complete().expect("original generation");
    let original_control = original.text_execution_control();
    assert!(
        !original
            .advance_text_serving(1)
            .expect("start original text projection")
    );
    let original_progress = build_progress_snapshot(&scheduler);
    let original_state = Arc::downgrade(&original.text_projection_build);
    let original_generation = original.generation().manifest().generation_id.clone();

    fixture.edit("src/first.rs", "pub fn first_replaced() {}\n");
    published(
        scheduler
            .reconcile_now()
            .expect("publish replacement generation"),
    );
    let replacement = scheduler.latest_complete().expect("replacement generation");
    assert_ne!(
        replacement.generation().manifest().generation_id,
        original_generation
    );
    let superseded_result = original.advance_artifact_text_serving(1, &original_control);
    assert_eq!(
        superseded_result,
        Err(tracedecay_query::retrieval::RetrievalPortError::Cancelled),
        "a replacement serving generation must retire the prior text owner"
    );
    assert!(!Arc::ptr_eq(
        &original.text_projection_build,
        &replacement.text_projection_build
    ));
    assert!(
        scheduler
            .build_progress_slot()
            .read()
            .expect("replacement progress slot")
            .snapshot()
            .is_none(),
        "generation replacement clears the superseded snapshot before new work begins"
    );
    assert!(
        !replacement
            .advance_text_serving(0)
            .expect("publish replacement progress baseline")
    );
    let replacement_progress = build_progress_snapshot(&scheduler);
    assert_eq!(
        replacement_progress.generation_id,
        replacement.generation().manifest().generation_id.as_str()
    );
    assert!(replacement_progress.progress_epoch > original_progress.progress_epoch);
    original.publish_text_progress_phase(
        tracedecay_contracts::code_index_freshness::CodeIndexBuildPhaseV1::BulkCommit,
        99,
        99,
    );
    let progress_after_stale_publish = build_progress_snapshot(&scheduler);
    assert_eq!(
        progress_after_stale_publish.generation_id,
        replacement_progress.generation_id
    );
    assert_eq!(
        progress_after_stale_publish.progress_epoch, replacement_progress.progress_epoch,
        "the old generation/owner epoch must not replace a newer generation snapshot"
    );
    assert_ne!(progress_after_stale_publish.current_batch_pages, 99);
    drop(original);
    assert!(
        original_state.upgrade().is_none(),
        "generation replacement must drop the abandoned partial projection"
    );
    assert!(
        matches!(
            &*replacement.text_projection_build.lock_slot(),
            super::super::CodeTextProjectionSlotV1::Building(_)
        ),
        "publishing the replacement baseline initializes only its independent projection state"
    );
}

/// The generation record index must answer every point lookup exactly as the
/// linear `.iter().find(..)` scans it replaced, including misses, and must be
/// built once per generation rather than once per query.
#[test]
fn generation_record_index_matches_linear_scan_lookups() {
    use std::fmt::Write as _;

    let mut sources = Vec::new();
    for file in 0..24 {
        let mut body = String::new();
        for symbol in 0..6 {
            write!(
                body,
                "pub fn caller_{file}_{symbol}() {{ callee_{file}_{symbol}(); }}\n\
                 pub fn callee_{file}_{symbol}() {{}}\n"
            )
            .expect("write to a string never fails");
        }
        sources.push((format!("src/module_{file}.rs"), body));
    }
    let files = sources
        .iter()
        .map(|(path, body)| (path.as_str(), body.as_str()))
        .collect::<Vec<_>>();
    let fixture = GitFixture::new(&files);
    let store = TempDir::new().expect("store root");
    let bytes = Arc::new(SharedCodeIndexBytePoolV1::default());
    let mut scheduler = scheduler(&fixture, store.path().to_path_buf(), bytes);
    published(scheduler.reconcile_now().expect("publish"));
    let latest = scheduler.latest_complete().expect("latest generation");

    let generation = latest.generation();
    let snapshot_files = &generation.snapshot().files;
    let chunks = generation.chunks().chunks();
    let symbols = &generation.symbols().symbols;
    let edges = generation.edges();
    assert!(
        !snapshot_files.is_empty() && !chunks.is_empty() && !symbols.is_empty(),
        "fixture must publish files, chunks, and symbols to compare"
    );

    let index = latest.record_index();

    for file in snapshot_files {
        let expected = snapshot_files
            .iter()
            .position(|candidate| candidate.file_occurrence_id == file.file_occurrence_id);
        assert_eq!(
            index.file_position(&file.file_occurrence_id),
            expected,
            "indexed file lookup must match the linear scan"
        );
    }

    for chunk in chunks {
        let expected = chunks.iter().position(|candidate| candidate.id == chunk.id);
        assert_eq!(
            index.chunk_position(&chunk.id),
            expected,
            "indexed chunk lookup must match the linear scan"
        );
    }

    for record in symbols {
        let expected = symbols
            .iter()
            .position(|candidate| candidate.occurrence == record.occurrence);
        assert_eq!(
            index.symbol_position(&record.occurrence),
            expected,
            "indexed symbol lookup must match the linear scan"
        );
    }

    let facet_rows = index.kind_facet_rows();
    let expected_facet_rows = symbols
        .iter()
        .enumerate()
        .filter_map(|(symbol_position, symbol)| {
            let chunk = chunks.iter().find(|candidate| {
                candidate.anchor.symbol_occurrence_id.as_ref() == Some(&symbol.occurrence)
            })?;
            let file_position = snapshot_files.iter().position(|candidate| {
                candidate.file_occurrence_id == chunk.anchor.file_occurrence_id
            })?;
            Some((symbol_position, file_position))
        })
        .collect::<Vec<_>>();
    assert_eq!(
        facet_rows, expected_facet_rows,
        "indexed facet rows must match the resolvable linear symbol/file joins"
    );

    for selector in symbols
        .iter()
        .take(12)
        .map(|symbol| symbol.qualified_name.as_str())
    {
        let expected = symbols
            .iter()
            .enumerate()
            .filter(|(_, symbol)| symbol.qualified_name == selector)
            .map(|(position, _)| position)
            .collect::<Vec<_>>();
        assert_eq!(
            index.qualified_name_positions(symbols, selector),
            expected,
            "indexed qualified-name lookup must match the linear scan"
        );
    }

    for selector in symbols
        .iter()
        .take(12)
        .map(|symbol| symbol.qualified_name.rsplit("::").next().expect("segment"))
    {
        let expected = symbols
            .iter()
            .enumerate()
            .filter(|(_, symbol)| symbol.qualified_name.rsplit("::").next() == Some(selector))
            .map(|(position, _)| position)
            .collect::<Vec<_>>();
        assert_eq!(
            index.last_segment_positions(symbols, selector),
            expected,
            "indexed last-segment lookup must match the linear scan"
        );
    }

    for chunk in chunks {
        let Some(symbol) = chunk.anchor.symbol_occurrence_id.as_ref() else {
            continue;
        };
        let file = &chunk.anchor.file_occurrence_id;
        let expected_by_symbol = chunks
            .iter()
            .position(|candidate| candidate.anchor.symbol_occurrence_id.as_ref() == Some(symbol));
        assert_eq!(
            index.chunk_position_for_symbol(symbol),
            expected_by_symbol,
            "indexed symbol-anchored chunk lookup must match the linear scan"
        );
        let expected_by_pair = chunks.iter().position(|candidate| {
            &candidate.anchor.file_occurrence_id == file
                && candidate.anchor.symbol_occurrence_id.as_ref() == Some(symbol)
        });
        assert_eq!(
            index.chunk_position_for_file_symbol(file, symbol),
            expected_by_pair,
            "indexed file+symbol chunk lookup must match the linear scan"
        );
    }

    let incident_symbols = edges
        .iter()
        .flat_map(|edge| [edge.from_occurrence.clone(), edge.to_occurrence.clone()])
        .collect::<BTreeSet<_>>();
    for symbol in &incident_symbols {
        for reverse in [false, true] {
            let expected = edges
                .iter()
                .enumerate()
                .filter(|(_, edge)| {
                    if reverse {
                        &edge.to_occurrence == symbol
                    } else {
                        &edge.from_occurrence == symbol
                    }
                })
                .map(|(position, _)| position)
                .collect::<Vec<_>>();
            assert_eq!(
                index.incident_edge_positions(symbol, reverse),
                expected.as_slice(),
                "indexed adjacency must match the linear edge scan in order"
            );
        }
    }

    let missing_file = tracedecay_domain::FileOccurrenceId::new("absent-file-occurrence")
        .expect("valid file occurrence id");
    let missing_chunk =
        tracedecay_domain::CodeSearchChunkId::new("absent-chunk").expect("valid chunk id");
    let missing_symbol = tracedecay_domain::SymbolOccurrenceId::new("absent-symbol-occurrence")
        .expect("valid symbol occurrence id");
    assert_eq!(index.file_position(&missing_file), None);
    assert_eq!(index.chunk_position(&missing_chunk), None);
    assert_eq!(index.symbol_position(&missing_symbol), None);
    assert_eq!(index.chunk_position_for_symbol(&missing_symbol), None);
    assert_eq!(
        index.chunk_position_for_file_symbol(&missing_file, &missing_symbol),
        None
    );
    assert!(
        index
            .incident_edge_positions(&missing_symbol, false)
            .is_empty()
    );
    assert!(
        index
            .incident_edge_positions(&missing_symbol, true)
            .is_empty()
    );

    let same_generation = scheduler.latest_complete().expect("same latest generation");
    assert!(
        Arc::ptr_eq(&latest.record_index, &same_generation.record_index),
        "repeated queries must reuse the generation-bound record index"
    );
    assert!(
        same_generation.record_index.get().is_some(),
        "the shared record index must stay built across queries"
    );
}

#[tokio::test]
async fn core_query_profile_composes_live_code_index_lanes() {
    let fixture = GitFixture::new(&[("src/main.rs", "fn main() {}\n")]);
    let store = TempDir::new().expect("store root");
    let registry = CodeIndexSchedulerRegistryV1::new(1);
    registry
        .mount_worktree(
            test_project_id(),
            fixture.path(),
            store.path().to_path_buf(),
            None,
        )
        .await
        .expect("mount daemon-owned scheduler");
    let latest = wait_for_live_complete_generation(&registry, fixture.path()).await;
    let snapshot = latest.generation.snapshot();
    let scope = ResolvedScope::new(
        test_project_id(),
        snapshot.repository.clone(),
        snapshot.worktree.clone().expect("worktree id"),
        snapshot.reference.clone(),
    )
    .expect("resolved scope");
    let authority = query_authority(latest.generation.manifest().privacy_domain.clone());
    registry
        .mount_query_authority(fixture.path(), &scope, authority)
        .await
        .expect("mount core query authority");

    let request = super::super::query_runtime::QuerySearchExecutionRequestV1::new(
        "main",
        super::super::query_runtime::QuerySearchExecutionPolicyV1 {
            principal: PrincipalId::new("principal.core-query.fixture").expect("principal"),
            authorization_revision: AuthorizationRevision::new("authorization.core-query.fixture")
                .expect("authorization revision"),
            sanitizer_revision: SanitizerRevision::new(
                tracedecay_query::retrieval::QUERY_SANITIZER_REVISION_V1,
            )
            .expect("sanitizer revision"),
            normalization_revision: QueryNormalizationRevision::new(
                tracedecay_query::retrieval::QUERY_NORMALIZATION_REVISION_V1,
            )
            .expect("normalization revision"),
            exact_rule_revision: ExactAdmissionRuleRevision::new(
                tracedecay_query::retrieval::QUERY_EXACT_RULE_REVISION_V1,
            )
            .expect("exact rules revision"),
            lexical_profile_revision: ComponentRevision::new(
                tracedecay_query::retrieval::QUERY_LEXICAL_PROFILE_REVISION_V1,
            )
            .expect("lexical profile revision"),
            lexical_score_domain: ScoreDomainId::new(
                tracedecay_query::retrieval::QUERY_LEXICAL_SCORE_DOMAIN_V1,
            )
            .expect("lexical score domain"),
            fuzzy_budget: tracedecay_query::retrieval::lexical::MAX_FUZZY_TERM_EXPANSIONS_V1,
            graph_edge_kinds: vec![RelationEdgeKindV1::Calls],
            graph_max_depth: 1,
            page_size: 10,
            cursor: None,
            lexical_routing: LexicalRoutingV1::query_only(),
        },
    );
    let executed = registry
        .execute_query_search(&scope, request)
        .await
        .expect("core query composes live lanes");
    assert!(
        !executed.authorized.fallback.ordered_candidates.is_empty(),
        "live main symbol is returned"
    );
    assert!(
        !executed.served_stale,
        "a ready generation serves the fresh path and is never marked stale"
    );
    assert_eq!(
        executed.lexical_routes.routes,
        vec![LexicalRouteKindV1::Query],
        "a plain query runs exactly the query route"
    );
    assert!(executed.lexical_routes.matches_by_anchor.is_empty());
    registry.shutdown().await;
}

/// A lexical anchor is its own ranked route: a symbol the natural-language
/// query never reaches enters the composed page with evidence naming the
/// anchor, and the query-only page is unchanged.
#[tokio::test]
async fn lexical_anchor_route_ranks_the_anchored_symbol_with_named_evidence() {
    // The file path must not repeat the query term: path postings would
    // otherwise reach every symbol in the file and blur the anchor's effect.
    let fixture = GitFixture::new(&[(
        "src/lib.rs",
        "pub fn allocate_inventory() {}\n\npub fn reserve_stock() {}\n\npub fn ship_order() {}\n",
    )]);
    let store = TempDir::new().expect("store root");
    let (registry, scope) = mounted_core_query_worktree(&fixture, &store).await;
    let latest = wait_for_live_complete_generation(&registry, fixture.path()).await;

    // Whole query terms match whole indexed tokens and query subtokens match
    // the subtoken field, so this multi-token query reaches
    // `allocate_inventory` (whole term + subtokens) and nothing in
    // `reserve_stock`'s vocabulary.
    let query = "allocate_inventory callers";
    let query_only = registry
        .execute_query_search(&scope, core_search_request(query))
        .await
        .expect("query-only search composes");
    let query_only_names = ranked_symbol_names(&query_only, &latest);
    assert!(
        ranks_symbol(&query_only_names, "allocate_inventory"),
        "the natural-language query reaches allocate_inventory: {query_only_names:?}"
    );
    assert!(
        !ranks_symbol(&query_only_names, "reserve_stock"),
        "the natural-language query alone never reaches reserve_stock: {query_only_names:?}"
    );

    let routing =
        LexicalRoutingV1::new(vec!["reserve_stock".to_owned()], false).expect("one valid anchor");
    let anchored = registry
        .execute_query_search(&scope, routed_core_search_request(query, routing))
        .await
        .expect("anchored search composes");
    let anchored_names = ranked_symbol_names(&anchored, &latest);
    assert!(
        ranks_symbol(&anchored_names, "reserve_stock"),
        "the anchor route adds reserve_stock to the composed page: {anchored_names:?}"
    );
    assert!(
        ranks_symbol(&anchored_names, "allocate_inventory"),
        "the query route still contributes: {anchored_names:?}"
    );
    assert_eq!(anchored.lexical_routes.routes.len(), 2);
    let anchor_kind = anchored.lexical_routes.routes[1].clone();
    assert!(
        matches!(&anchor_kind, LexicalRouteKindV1::Anchor { anchor } if anchor.as_str() == "reserve_stock"),
        "{anchor_kind:?}"
    );
    let reserve = anchored
        .authorized
        .fallback
        .ordered_candidates
        .iter()
        .zip(&anchored_names)
        .find(|(_, name)| {
            name.as_deref()
                .is_some_and(|name| name.ends_with("reserve_stock"))
        })
        .map(|(ranked, _)| ranked)
        .expect("reserve_stock is ranked");
    let matches = anchored
        .lexical_routes
        .matches_by_anchor
        .get(&reserve.candidate.anchor_id)
        .expect("the anchored candidate carries route evidence");
    assert!(
        matches
            .iter()
            .any(|route_match| route_match.route == anchor_kind
                && route_match
                    .matched_terms
                    .iter()
                    .any(|term| term == "reserve_stock")),
        "the evidence names the anchor that ranked the hit: {matches:?}"
    );
    registry.shutdown().await;
}

/// `prefer_symbol` adds a deterministic symbol-name route built from the
/// query's identifier-shaped tokens; a query with no such token adds nothing.
#[tokio::test]
async fn preferred_symbol_route_is_derived_from_query_identifiers() {
    let fixture = GitFixture::new(&[(
        "src/lib.rs",
        "pub fn allocate_inventory() {}\n\npub fn reserve_stock() {}\n",
    )]);
    let store = TempDir::new().expect("store root");
    let (registry, scope) = mounted_core_query_worktree(&fixture, &store).await;
    let latest = wait_for_live_complete_generation(&registry, fixture.path()).await;

    let routing = LexicalRoutingV1::new(Vec::new(), true).expect("prefer_symbol routing");
    let preferred = registry
        .execute_query_search(
            &scope,
            routed_core_search_request("explain the function reserve_stock", routing.clone()),
        )
        .await
        .expect("preferred-symbol search composes");
    assert_eq!(
        preferred.lexical_routes.routes,
        vec![
            LexicalRouteKindV1::Query,
            LexicalRouteKindV1::PreferredSymbol {
                tokens: vec!["reserve_stock".to_owned()],
            },
        ],
        "stoplisted query words never become symbol tokens"
    );
    let names = ranked_symbol_names(&preferred, &latest);
    let reserve = preferred
        .authorized
        .fallback
        .ordered_candidates
        .iter()
        .zip(&names)
        .find(|(_, name)| {
            name.as_deref()
                .is_some_and(|name| name.ends_with("reserve_stock"))
        })
        .map(|(ranked, _)| ranked)
        .expect("reserve_stock is ranked");
    let matches = preferred
        .lexical_routes
        .matches_by_anchor
        .get(&reserve.candidate.anchor_id)
        .expect("the symbol-name hit carries route evidence");
    assert!(
        matches.iter().any(|route_match| matches!(
            route_match.route,
            LexicalRouteKindV1::PreferredSymbol { .. }
        )),
        "{matches:?}"
    );

    let no_identifiers = registry
        .execute_query_search(
            &scope,
            routed_core_search_request("what is the type of a", routing),
        )
        .await
        .expect("a query without identifiers still composes");
    assert_eq!(
        no_identifiers.lexical_routes.routes,
        vec![LexicalRouteKindV1::Query],
        "no identifier-shaped token means no preferred-symbol route"
    );
    registry.shutdown().await;
}

#[tokio::test]
async fn query_authority_lookup_preserves_real_mount_identity_isolation() {
    let primary = GitFixture::new(&[("src/main.rs", "fn main() {}\n")]);
    let linked_root = TempDir::new().expect("linked worktree root");
    let linked = linked_root.path().join("linked");
    let linked_arg = linked.to_str().expect("linked worktree path");
    git(
        primary.path(),
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "linked-query",
            linked_arg,
            "main",
        ],
    );
    let store = TempDir::new().expect("store root");
    let registry = CodeIndexSchedulerRegistryV1::new(2);
    for root in [primary.path(), linked.as_path()] {
        registry
            .mount_worktree(test_project_id(), root, store.path().to_path_buf(), None)
            .await
            .expect("mount real sibling worktree");
    }

    let scope_for = |root: &Path| {
        let identity = super::super::identity::IndexingIdentityV1::resolve(root)
            .expect("mounted worktree identity");
        ResolvedScope::new(
            test_project_id(),
            identity.repository_id().clone(),
            identity.worktree_id().clone(),
            identity.head_ref().cloned(),
        )
        .expect("resolved scope")
    };
    let primary_scope = scope_for(primary.path());
    let linked_scope = scope_for(&linked);
    assert_eq!(primary_scope.repository_id, linked_scope.repository_id);
    assert_ne!(primary_scope.worktree_id, linked_scope.worktree_id);

    registry
        .mount_query_authority(
            &linked,
            &linked_scope,
            query_authority(
                PrivacyDomainId::new("privacy.query-authority-linked")
                    .expect("linked privacy domain"),
            ),
        )
        .await
        .expect("mount linked query authority");
    assert!(!registry.has_query_authority_for_scope(&primary_scope).await);
    assert!(registry.has_query_authority_for_scope(&linked_scope).await);

    registry
        .mount_query_authority(
            primary.path(),
            &primary_scope,
            query_authority(
                PrivacyDomainId::new("privacy.query-authority-primary")
                    .expect("primary privacy domain"),
            ),
        )
        .await
        .expect("mount primary query authority");
    assert!(registry.has_query_authority_for_scope(&primary_scope).await);
    assert!(registry.has_query_authority_for_scope(&linked_scope).await);

    registry.shutdown().await;
}

/// The defect this covers: during any generation rebuild search used to
/// collapse into `GenerationUnavailable` for the whole window while
/// callers/grep/context kept serving. Holding the scheduler mutex reproduces
/// exactly that window — the background worker owns the scheduler — while the
/// last complete generation stays in `serving_generation`. A seat whose
/// currency witness still re-proves against the unchanged checkout serves as
/// current; once the checkout drifts under the held mutex the witness
/// disproves and the fallback serves the same complete generation reported
/// stale.
///
/// The ready gate itself no longer abstains for this window. Decoupling
/// freshness from publication work moved readiness onto the per-worktree
/// source-freshness state, so the gate reads it without the scheduler and an
/// unchanged checkout stays ready while a rebuild owns the mutex — which is
/// the point of the decoupling, and what the witnessed assertion below
/// already required of the query path.
// Holding the scheduler guard across the awaits is the scenario, not an
// oversight: it is how this test occupies the rebuild window that the fallback
// exists to serve through. The guard is released before shutdown.
#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn search_serves_the_last_complete_generation_while_the_scheduler_rebuilds() {
    let fixture = GitFixture::new(&[("src/main.rs", "fn main() {}\n")]);
    let store = TempDir::new().expect("store root");
    let (registry, scope) = mounted_core_query_worktree(&fixture, &store).await;

    // Baseline: the ready path, byte-for-byte, before anything is degraded.
    let fresh = registry
        .execute_query_search(&scope, core_search_request("main"))
        .await
        .expect("ready generation serves the fresh path");
    assert!(!fresh.served_stale);
    let fresh_generation = fresh.generation.clone();
    let fresh_candidates = fresh.authorized.fallback.ordered_candidates.clone();
    assert!(!fresh_candidates.is_empty(), "live main symbol is returned");

    // Enter the rebuild window: the scheduler is owned elsewhere, so the ready
    // gate cannot admit a current generation.
    let scheduler = {
        let mounted = registry.mounted.lock().await;
        Arc::clone(
            &mounted
                .get(&fixture.path().canonicalize().expect("canonical root"))
                .expect("mounted worktree")
                .scheduler,
        )
    };
    let held = scheduler
        .lock()
        .expect("hold the scheduler as a rebuild would");
    assert!(
        registry
            .latest_complete_ready_for_scope(&scope)
            .await
            .is_some(),
        "the ready gate reads source freshness without the scheduler, so a rebuild \
         window over an unchanged checkout does not make it abstain"
    );
    assert!(
        registry
            .latest_complete_serving_for_scope(&scope)
            .await
            .is_some(),
        "the last complete generation is still held and needs no re-read"
    );

    // An unchanged checkout re-proves the seat's currency witness without the
    // scheduler, so the rebuild window alone does not degrade the answer.
    let witnessed = registry
        .execute_query_search(&scope, core_search_request("main"))
        .await
        .expect("search keeps serving through the rebuild instead of failing");
    assert!(
        !witnessed.served_stale,
        "a seat re-proven current against the unchanged checkout serves as current"
    );

    // Drift the checkout while the rebuild still owns the scheduler: the
    // witness disproves, the ready gate abstains, and the fallback serves the
    // last complete generation reported stale.
    //
    // The drift is Git-mediated because that is what the decoupled freshness
    // authority observes without the scheduler. A bare worktree write is the
    // documented tier-2 case: it is proven only by the periodic
    // stat-signature ladder, and until that ladder runs the bounded-staleness
    // contract deliberately keeps serving the preceding generation as
    // current. Using it here would assert against that contract rather than
    // against this test's subject, which is what search reports once the seat
    // *is* disproved mid-rebuild.
    std::fs::write(
        fixture.path().join("src/main.rs"),
        "fn main() { drifted(); }\n",
    )
    .expect("drift the checkout under the held scheduler");
    git(fixture.path(), &["add", "src/main.rs"]);
    git(fixture.path(), &["commit", "-qm", "drift under rebuild"]);
    let stale = registry
        .execute_query_search(&scope, core_search_request("main"))
        .await
        .expect("search keeps serving through the rebuild instead of failing");
    assert!(
        stale.served_stale,
        "a fallback answer past the disproved witness must be reported stale, never as current"
    );
    assert_eq!(
        stale.generation, fresh_generation,
        "the stale answer names the complete generation that actually answered"
    );
    assert_eq!(
        stale.authorized.fallback.ordered_candidates, fresh_candidates,
        "serving stale changes only the coverage marker, not ranking identity"
    );

    // The coverage marker the executor derives from this flag.
    let coverage = tracedecay_query::code_search::CodeIndexSearchCoverageV1::fused_stale(
        stale.generation.as_str(),
        &tracedecay_query::code_search::CodeIndexSemanticStatusV1::Complete,
    );
    assert!(coverage.any_servable(), "a stale answer is still servable");
    assert!(
        coverage.is_degraded(),
        "a stale answer says recall is partial"
    );
    assert_eq!(
        coverage.exact,
        tracedecay_query::code_search::CodeIndexLaneStatusV1::Stale {
            generation: fresh_generation.as_str().to_owned(),
        }
    );

    drop(held);
    registry.shutdown().await;
}

/// The resolution-order defect: asking the graph-ready gate first made every
/// query queue on the single-flight decode of the generation being activated,
/// so a query with a current text owner still paid an O(store) sweep. Optional
/// graph enrichment must neither block nor mark that text owner stale.
///
/// This occupies the decode barrier exactly as activation of a new generation
/// does — pinned slot empty, one decode in flight — and deliberately leaves the
/// scheduler mutex FREE, so the ready gate is admitted and would park inside it.
#[tokio::test]
async fn search_never_awaits_an_in_flight_decode_while_a_generation_is_servable() {
    let fixture = GitFixture::new(&[("src/main.rs", "fn main() {}\n")]);
    let store = TempDir::new().expect("store root");
    let (registry, scope) = mounted_core_query_worktree(&fixture, &store).await;

    // Baseline: the ready path admits, byte-for-byte, before anything degrades.
    let fresh = registry
        .execute_query_search(&scope, core_search_request("main"))
        .await
        .expect("ready generation serves the fresh path");
    assert!(!fresh.served_stale);
    let fresh_generation = fresh.generation.clone();
    let fresh_candidates = fresh.authorized.fallback.ordered_candidates.clone();
    assert!(!fresh_candidates.is_empty(), "live main symbol is returned");

    let scheduler = {
        let mounted = registry.mounted.lock().await;
        Arc::clone(
            &mounted
                .get(&fixture.path().canonicalize().expect("canonical root"))
                .expect("mounted worktree")
                .scheduler,
        )
    };
    let (held_decode, decodes_before) = {
        let scheduler = scheduler
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        (
            scheduler.hold_active_decode(),
            scheduler.sealed_decode_count(),
        )
    };
    assert!(
        registry
            .latest_complete_serving_for_scope(&scope)
            .await
            .is_some(),
        "the last complete generation is still held and needs no decode"
    );

    let current = tokio::time::timeout(
        Duration::from_secs(30),
        registry.execute_query_search(&scope, core_search_request("main")),
    )
    .await
    .expect("a servable generation must never queue on the decode barrier")
    .expect("search serves through the activation window");
    assert!(
        !current.served_stale,
        "a current text authority must stay fresh while optional graph decode is in flight"
    );
    assert_eq!(
        current.generation, fresh_generation,
        "the current answer names the complete generation that actually answered"
    );
    assert_eq!(
        current.authorized.fallback.ordered_candidates, fresh_candidates,
        "optional graph decode cannot change exact and lexical ranking identity"
    );
    assert_eq!(
        scheduler
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .sealed_decode_count(),
        decodes_before,
        "the serving path must not enter the sealed decode at all"
    );

    drop(held_decode);
    registry.shutdown().await;
}

/// The cold-restore window: a sealed active generation is on disk and the
/// freshness fences pass, but the serving slot is empty because activation has
/// not seated anything yet. The typed refusal is already determined — the
/// activation gate can only ever admit the seated slot — so search resolution
/// must deliver that verdict without joining (or starting) the single-flight
/// O(store) decode. Before the reorder, a cold `search` against a rebuilding
/// generation parked on that decode for 76 s before returning the refusal the
/// daemon already knew; the same refusal is sub-second once delivered
/// decode-free.
#[tokio::test]
async fn search_refusal_with_nothing_servable_never_joins_the_decode() {
    let fixture = GitFixture::new(&[("src/main.rs", "fn main() {}\n")]);
    let store = TempDir::new().expect("store root");
    let (registry, scope) = mounted_core_query_worktree(&fixture, &store).await;

    // Baseline: the mounted worktree serves before the window opens.
    registry
        .execute_query_search(&scope, core_search_request("main"))
        .await
        .expect("ready generation serves the fresh path");

    // Enter the window: nothing seated, and the active-generation decode is
    // owned by an in-flight activation that has not completed.
    registry.clear_serving_generation_for_scope(&scope).await;
    let scheduler = {
        let mounted = registry.mounted.lock().await;
        Arc::clone(
            &mounted
                .get(&fixture.path().canonicalize().expect("canonical root"))
                .expect("mounted worktree")
                .scheduler,
        )
    };
    let held_decode = scheduler
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .hold_active_decode();

    let refusal = match tokio::time::timeout(
        Duration::from_secs(5),
        registry.execute_query_search(&scope, core_search_request("main")),
    )
    .await
    .expect("a refusal the scheduler already knows must not wait on the decode")
    {
        Err(refusal) => refusal,
        Ok(_) => panic!("nothing servable stays a typed refusal"),
    };
    assert!(
        matches!(
            refusal,
            super::super::query_runtime::QuerySearchExecutionErrorV1::GenerationUnavailable
                | super::super::query_runtime::QuerySearchExecutionErrorV1::GenerationUnverified
        ),
        "the verdict itself is unchanged: {refusal:?}"
    );
    assert_eq!(
        held_decode.waiter_count(),
        0,
        "the refusal path must not park on the publication decode flight"
    );

    drop(held_decode);
    registry.shutdown().await;
}

/// A seated graph generation is already the decoded serving authority. Root
/// graph/status reads must not ask the publication decoder cache to prove that
/// fact again: the cache may be temporarily claimed by unrelated activation
/// work while the immutable serving handle remains fully usable.
#[tokio::test]
async fn root_graph_ready_does_not_depend_on_the_publication_decode_cache() {
    let fixture = GitFixture::new(&[("src/main.rs", "fn main() {}\n")]);
    let store = TempDir::new().expect("store root");
    let (registry, scope) = mounted_core_query_worktree(&fixture, &store).await;
    let serving = registry
        .latest_complete_serving_for_scope(&scope)
        .await
        .expect("mounted graph generation serves");
    serving
        .production_graph_serving()
        .expect("the serving generation has activated its graph lane");
    let generation_id = serving.generation().manifest().generation_id.clone();

    let scheduler = {
        let mounted = registry.mounted.lock().await;
        Arc::clone(
            &mounted
                .get(&fixture.path().canonicalize().expect("canonical root"))
                .expect("mounted worktree")
                .scheduler,
        )
    };
    let held_decode = scheduler
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .hold_active_decode();

    let ready = tokio::time::timeout(
        Duration::from_secs(30),
        registry.latest_complete_ready_decoded_for_root_scope(fixture.path(), &scope),
    )
    .await
    .expect("root graph readiness must not wait for the decode cache")
    .expect("the seated graph generation remains ready");
    assert_eq!(
        ready.generation().manifest().generation_id,
        generation_id,
        "the exact seated generation remains authoritative"
    );
    assert_eq!(
        held_decode.waiter_count(),
        0,
        "root graph readiness must not join the publication decode flight"
    );

    let scope_ready = tokio::time::timeout(
        Duration::from_secs(30),
        registry.latest_complete_ready_decoded_for_scope(&scope),
    )
    .await
    .expect("scope query readiness must not wait for the decode cache")
    .expect("the seated graph generation remains ready for scoped queries");
    assert_eq!(
        scope_ready.generation().manifest().generation_id,
        generation_id,
        "scoped query admission must trust the exact seated generation"
    );
    assert_eq!(
        held_decode.waiter_count(),
        0,
        "scope query readiness must not join the publication decode flight"
    );

    drop(held_decode);
    registry.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mounted_map_contention_cannot_hide_query_authority() {
    let fixture = GitFixture::new(&[("src/main.rs", "fn main() {}\n")]);
    let store = TempDir::new().expect("store root");
    let (registry, scope) = mounted_core_query_worktree(&fixture, &store).await;
    let held = registry.mounted.lock().await;
    let lookup_registry = registry.clone();
    let lookup_scope = scope.clone();
    let lookup = tokio::spawn(async move {
        lookup_registry
            .query_authority_for_scope(&lookup_scope)
            .await
    });
    tokio::time::sleep(Duration::from_millis(25)).await;
    assert!(
        !lookup.is_finished(),
        "map contention waits for the micro-held map authority instead of fabricating unavailability"
    );

    drop(held);
    assert!(
        lookup
            .await
            .expect("query-authority lookup joins")
            .is_some(),
        "the mounted query authority remains visible after map contention"
    );
    registry.shutdown().await;
}

/// Fail-closed: the fallback serves a *retained complete* generation, never a
/// missing one. With no mounted worktree neither resolver can produce one, and
/// the typed fail-fast is preserved rather than degraded into an empty answer.
#[tokio::test]
async fn search_fails_fast_when_no_complete_generation_exists() {
    let fixture = GitFixture::new(&[("src/main.rs", "fn main() {}\n")]);
    let store = TempDir::new().expect("store root");
    // The mounted registry only lends this test a real scope; the queries
    // below run against an empty registry, so a text-current seat is all the
    // scope derivation needs and graph seating is not awaited.
    let registry = CodeIndexSchedulerRegistryV1::new(1);
    registry
        .mount_worktree(
            test_project_id(),
            fixture.path(),
            store.path().to_path_buf(),
            None,
        )
        .await
        .expect("mount daemon-owned scheduler");
    let text = wait_for_queryable_text_generation(&registry, fixture.path()).await;
    let snapshot = text.metadata().snapshot();
    let scope = ResolvedScope::new(
        test_project_id(),
        snapshot.repository.clone(),
        snapshot.worktree.clone().expect("worktree id"),
        snapshot.reference.clone(),
    )
    .expect("resolved scope");

    let empty = CodeIndexSchedulerRegistryV1::new(1);
    assert!(
        empty
            .latest_complete_serving_for_scope(&scope)
            .await
            .is_none(),
        "an unmounted scope has no retained generation to serve"
    );
    // `ExecutedQuerySearchV1` intentionally omits `Debug` (it carries the
    // sanitized query), so assert on the error arm directly.
    match empty
        .execute_query_search(&scope, core_search_request("main"))
        .await
    {
        Err(super::super::query_runtime::QuerySearchExecutionErrorV1::GenerationUnavailable) => {}
        Err(other) => panic!("expected the typed fail-fast, got {other:?}"),
        Ok(_) => panic!("absent generations must not be degraded into a stale answer"),
    }

    empty.shutdown().await;
    registry.shutdown().await;
}

/// The live outage this covers: a scope's branch label moves — a restored
/// generation was sealed before a `git switch`, or a retained route scope
/// pinned the label that was live at project open — while the worktree the
/// daemon is serving stays byte-identical. The label is not checkout
/// identity: the ready ladder has already verified the generation against
/// the live worktree, so the exact worktree's own graph must keep serving as
/// current instead of degrading to stale (queries) or `Unavailable` (graph
/// reads and the runtime census, which have no stale arm and were orphaned
/// until the route reopened).
#[tokio::test]
async fn moved_reference_label_still_serves_the_exact_worktree_as_current() {
    let fixture = GitFixture::new(&[("src/main.rs", "fn main() {}\n")]);
    let store = TempDir::new().expect("store root");
    let (registry, scope) = mounted_core_query_worktree(&fixture, &store).await;

    // Baseline under the sealed reference: the ready path, byte-for-byte.
    let fresh = registry
        .execute_query_search(&scope, core_search_request("main"))
        .await
        .expect("ready generation serves the fresh path");
    assert!(!fresh.served_stale);
    let fresh_generation = fresh.generation.clone();
    let fresh_candidates = fresh.authorized.fallback.ordered_candidates.clone();
    assert!(!fresh_candidates.is_empty(), "live main symbol is returned");

    // The scope's label moves. Nothing about the worktree changes, and the
    // daemon mounts the authority for the *new* scope exactly as project open
    // does.
    let moved = moved_reference_scope(&scope);
    let latest = registry
        .latest_complete_fresh(fixture.path())
        .await
        .expect("retained generation");
    mount_core_query_authority(&registry, fixture.path(), &moved, &latest).await;

    let ready = registry
        .latest_complete_ready_for_scope(&moved)
        .await
        .expect("a moved label must not orphan the exact worktree's ready generation");
    assert_eq!(
        ready.generation.manifest().generation_id,
        fresh_generation,
        "the ready gate serves the generation verified against this worktree"
    );
    // Attribution is generation-bound, not scope-bound: the served generation
    // still names its own sealed reference, so the answer is attributed to the
    // revision that produced it rather than to the scope that asked.
    assert_ne!(
        ready.generation.snapshot().reference,
        moved.reference,
        "the served generation keeps its own sealed reference"
    );

    let answered = registry
        .execute_query_search(&moved, core_search_request("main"))
        .await
        .expect("search survives a moved reference label instead of failing");
    assert!(
        !answered.served_stale,
        "a byte-identical worktree is current regardless of the label the scope carries"
    );
    assert_eq!(
        answered.generation, fresh_generation,
        "the answer names the complete generation that actually answered"
    );
    assert_eq!(
        answered.authorized.fallback.ordered_candidates, fresh_candidates,
        "the label move changes nothing about ranking identity"
    );

    // The grep/context/callers ladder survives the same move.
    let ladder = registry
        .latest_complete_fresh_for_scope(&moved)
        .await
        .expect("the callable-code ladder also serves through a moved reference");
    assert_eq!(ladder.generation.manifest().generation_id, fresh_generation);

    // The root-scope ready gate behind graph reads and the runtime census —
    // the arms with no stale fallback — must not be orphaned either. Seating
    // races the publication event, so the gate is polled bounded.
    let decoded = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(decoded) = registry
                .latest_complete_ready_decoded_for_root_scope(fixture.path(), &moved)
                .await
            {
                break decoded;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("graph reads and the census survive a moved reference label");
    assert_eq!(
        decoded.generation.manifest().generation_id,
        fresh_generation
    );

    registry.shutdown().await;
}

/// The second half of the outage: search resolves its generation without ever
/// running the freshness ladder, so when both arms came up empty nothing
/// requested the reconcile that would remedy it — the typed failure repeated
/// forever. Search must now ask for its own remedy, exactly once per due
/// window, and must still never reconcile inline or park.
// Holding the scheduler guard across the awaits is the scenario: it is how this
// test occupies the window where neither arm can resolve. Released before
// shutdown.
#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn search_requests_one_background_reconcile_when_nothing_is_servable() {
    let fixture = GitFixture::new(&[("src/main.rs", "fn main() {}\n")]);
    let store = TempDir::new().expect("store root");
    let (registry, scope) = mounted_core_query_worktree_with_one_permit(&fixture, &store).await;

    // Occupy the single background-reconcile permit so the worker parks at its
    // dequeue point and the pending wake stays observable for the whole test.
    let admission = registry
        .background_reconcile_admission()
        .acquire_owned()
        .await
        .expect("background reconcile admission");

    let scheduler = {
        let mounted = registry.mounted.lock().await;
        Arc::clone(
            &mounted
                .get(&fixture.path().canonicalize().expect("canonical root"))
                .expect("mounted worktree")
                .scheduler,
        )
    };
    // Nothing retained and the scheduler owned elsewhere: both arms come up
    // empty, which is the exact None/None admission that used to be silent.
    registry.clear_serving_generation_for_scope(&scope).await;
    let held = scheduler
        .lock()
        .expect("hold the scheduler as a rebuild would");
    registry.clear_pending_wake_for_scope(&scope).await;

    match registry
        .execute_query_search(&scope, core_search_request("main"))
        .await
    {
        Err(super::super::query_runtime::QuerySearchExecutionErrorV1::GenerationUnverified) => {}
        Err(other) => panic!("expected the typed unverified state, got {other:?}"),
        Ok(_) => panic!("absent generations must not be degraded into an answer"),
    }
    let stamped = registry
        .pending_wake_micros_for_scope(&scope)
        .await
        .expect("mounted worktree");
    assert_ne!(
        stamped, 0,
        "a search that resolved to nothing must request the rebuild that remedies it"
    );

    // Repeated failing searches inside the same due window must not storm the
    // worker: the outstanding wake already is the remedy they would ask for.
    for _ in 0..4 {
        let _ = registry
            .execute_query_search(&scope, core_search_request("main"))
            .await;
        assert!(
            !registry.request_query_background_reconcile(&scope).await,
            "an outstanding wake must debounce every further admission"
        );
        assert_eq!(
            registry
                .pending_wake_micros_for_scope(&scope)
                .await
                .expect("mounted worktree"),
            stamped,
            "the pending arrival must keep the first admission's instant"
        );
    }

    // A fresh due window (the worker claimed the wake) admits exactly one more.
    registry.clear_pending_wake_for_scope(&scope).await;
    assert!(
        registry.request_query_background_reconcile(&scope).await,
        "a new due window admits one request"
    );
    assert!(
        !registry.request_query_background_reconcile(&scope).await,
        "and only one"
    );

    drop(held);
    // Release the worker before shutdown: it is parked on this permit, and
    // `shutdown` joins its task.
    drop(admission);
    registry.shutdown().await;
}

#[test]
fn layout_scan_progress_publish_does_not_wait_for_status_reader() {
    let generation = CodeGenerationId::new("generation.progress-scan").expect("generation");
    let slot = Arc::new(std::sync::RwLock::new(
        super::super::CodeIndexBuildProgressSlotStateV1::default(),
    ));
    let owner = slot
        .write()
        .expect("progress owner")
        .replace_generation(generation.clone());
    let status_reader = slot.read().expect("status progress reader");

    let started = Instant::now();
    let published = super::super::try_publish_build_progress(
        &slot,
        &generation,
        owner,
        progress_snapshot_for_generation(&generation, 1),
    );
    assert!(
        started.elapsed() < Duration::from_millis(100),
        "observational progress must not block the authenticated layout scan"
    );
    assert!(!published, "a busy observational slot must skip the sample");
    drop(status_reader);

    assert!(super::super::try_publish_build_progress(
        &slot,
        &generation,
        owner,
        progress_snapshot_for_generation(&generation, 2),
    ));
    assert_eq!(
        slot.read()
            .expect("published progress")
            .snapshot()
            .expect("progress snapshot")
            .committed_pages,
        2
    );
}

#[tokio::test]
async fn callable_application_operations_consume_exact_lexical_and_graph_owners() {
    let fixture = GitFixture::new(&[(
        "src/lib.rs",
        "pub trait Processor { fn process(&self, input: u32) -> u32; }\n\
         pub struct Doubler;\n\
         impl Processor for Doubler {\n\
             fn process(&self, input: u32) -> u32 { input * 2 }\n\
         }\n\
         pub struct Tripler;\n\
         impl Processor for Tripler {\n\
             fn process(&self, input: u32) -> u32 { input * 3 }\n\
         }\n\
         pub fn via_trait(processor: &Doubler, input: u32) -> u32 {\n\
             Processor::process(processor, input)\n\
         }\n\
         pub fn caller() { callee(); }\n\
         pub fn callee() {}\n",
    )]);
    let store = TempDir::new().expect("store root");
    let registry = CodeIndexSchedulerRegistryV1::new(1);
    registry
        .mount_worktree(
            test_project_id(),
            fixture.path(),
            store.path().to_path_buf(),
            None,
        )
        .await
        .expect("mount daemon-owned scheduler");
    let latest = wait_for_live_complete_generation(&registry, fixture.path()).await;
    let generation = latest.generation.manifest().generation_id.clone();
    let cancellation =
        tracedecay_contracts::CancellationSignal::active("cancel.callable-graph-projection")
            .expect("graph cancellation");
    let publisher =
        tracedecay_code_index::graph_projection::HermeticCodeGraphProjectionStore::memory(
            &cancellation,
        )
        .expect("graph publisher");
    publisher
        .publish_indexed_with_cancellation(
            &generation,
            latest.generation.edges(),
            latest.generation.chunks().chunks(),
            &latest.generation.snapshot().files,
            latest.generation.symbols(),
            Arc::new(tracedecay_graph_db::NeverCancelled),
        )
        .expect("publish indexed graph");
    let graph_store = Arc::new(
        publisher
            .verified_store(&generation)
            .expect("verified graph"),
    );
    graph_store
        .warm_interactive_catalog_with_cancellation(Arc::new(tracedecay_graph_db::NeverCancelled))
        .expect("warm graph catalog");
    let graph_reader = graph_store
        .evidence_reader_with_cancellation(
            &generation,
            Some(latest.generation.snapshot().repository.clone()),
            latest.source_freshness().expect("source freshness"),
            Arc::new(tracedecay_graph_db::NeverCancelled),
        )
        .expect("graph reader");
    latest
        .install_graph_serving(
            graph_reader,
            Some(graph_store),
            super::super::CodeGraphServingAuthorityV1::Memory,
        )
        .expect("install interactive graph serving");
    assert_eq!(latest.generation.manifest().generation_id, generation);
    let repository = latest.generation.snapshot().repository.clone();
    let worktree = latest
        .generation
        .snapshot()
        .worktree
        .clone()
        .expect("worktree identity");
    let scope = CodeQueryScope::new(generation.clone(), None).expect("query scope");

    let exact_operation =
        callable_code_operation(CallableCodeOperationKind::ExactOccurrence).expect("operation");
    let exact_context = application_context(&exact_operation, repository.clone(), worktree.clone());
    mount_query_authority(
        &registry,
        fixture.path(),
        &exact_context,
        latest.generation.manifest().privacy_domain.clone(),
    )
    .await;
    let exact_request =
        ExactOccurrenceRequest::new("caller", None, scope.clone(), query_meta()).expect("exact");
    let exact = registry
        .exact_occurrence(
            RetrievalPortContext {
                request: &exact_context,
                operation: &exact_operation,
            },
            &exact_request,
        )
        .await;
    let exact_repeat = registry
        .exact_occurrence(
            RetrievalPortContext {
                request: &exact_context,
                operation: &exact_operation,
            },
            &exact_request,
        )
        .await;
    assert!(
        exact.evidence().finished_at > latest.generation.manifest().seal.sealed_at,
        "query completion time must not reuse the generation seal time"
    );
    assert_eq!(
        serde_json::to_vec(&exact.evidence().payload).expect("serialize exact payload"),
        serde_json::to_vec(&exact_repeat.evidence().payload)
            .expect("serialize repeated exact payload"),
        "same generation and request produce byte-stable production query payload"
    );
    match exact {
        RetrievalPortOutcome::Completed(evidence) => {
            let page = evidence.payload.expect("exact page");
            assert_eq!(page.generation, generation);
            assert!(
                !page.items.is_empty(),
                "exact operation must return production lane evidence"
            );
        }
        outcome => panic!("expected completed exact operation, got {outcome:?}"),
    }

    let lexical_operation =
        callable_code_operation(CallableCodeOperationKind::PhraseSearch).expect("operation");
    let lexical_context =
        application_context(&lexical_operation, repository.clone(), worktree.clone());
    let query = EphemeralSanitizedQueryViewV1::sanitize(
        "callee",
        SanitizerRevision::new("sanitizer.query.fixture").expect("sanitizer"),
        QueryNormalizationRevision::new("normalization.query.fixture").expect("normalization"),
    )
    .expect("query");
    let lexical_request = PhraseSearchRequest::new(
        query,
        vec!["callee".to_owned()],
        Vec::new(),
        0,
        scope.clone(),
        query_meta(),
    )
    .expect("lexical");
    let lexical = registry
        .phrase_search(
            RetrievalPortContext {
                request: &lexical_context,
                operation: &lexical_operation,
            },
            &lexical_request,
        )
        .await;
    match lexical {
        RetrievalPortOutcome::Completed(evidence) => {
            let page = evidence.payload.expect("lexical page");
            assert_eq!(page.generation, generation);
            assert!(
                !page.items.is_empty(),
                "lexical operation must return production lane evidence"
            );
        }
        outcome => panic!("expected completed lexical operation, got {outcome:?}"),
    }

    let caller = latest
        .generation
        .symbols()
        .symbols
        .iter()
        .find(|record| record.qualified_name.ends_with("caller"))
        .expect("caller symbol")
        .occurrence
        .as_str()
        .to_owned();
    let graph_operation =
        callable_code_operation(CallableCodeOperationKind::Callees).expect("operation");
    let graph_context = application_context(&graph_operation, repository.clone(), worktree.clone());
    let graph_request = CodeRelationRequest {
        node_id: caller,
        maximum_depth: 2,
        resolve_trait_dispatch: false,
        scope: scope.clone(),
        meta: query_meta(),
    };
    let graph = registry
        .callees(
            RetrievalPortContext {
                request: &graph_context,
                operation: &graph_operation,
            },
            &graph_request,
        )
        .await;
    match graph {
        RetrievalPortOutcome::Completed(evidence) => {
            let page = evidence.payload.expect("graph page");
            assert_eq!(page.generation, generation);
            assert_eq!(page.items.len(), 1);
            let callee = &page.items[0];
            assert_eq!(callee.edge_kind, "calls");
            assert_eq!(callee.symbol.name, "callee");
            assert_eq!(callee.symbol.file, "src/lib.rs");
            assert_eq!(callee.symbol.start_line_zero_based, 13);
            assert_eq!(callee.symbol.end_line_zero_based, 13);
            assert_eq!(callee.symbol.line, 14);
            assert_eq!(callee.symbol.end_line, 14);
        }
        outcome => panic!("expected completed graph operation, got {outcome:?}"),
    }

    let via_trait = latest
        .generation
        .symbols()
        .symbols
        .iter()
        .find(|record| record.qualified_name.ends_with("via_trait"))
        .expect("trait caller symbol")
        .occurrence
        .as_str()
        .to_owned();
    let trait_method = latest
        .generation
        .symbols()
        .symbols
        .iter()
        .find(|record| {
            record.simple_name == "process"
                && record.qualified_name.contains("Processor")
                && record.kind == "method"
        })
        .expect("trait method symbol")
        .occurrence
        .as_str()
        .to_owned();
    let implementation_method = latest
        .generation
        .symbols()
        .symbols
        .iter()
        .find(|record| {
            record.simple_name == "process"
                && record.qualified_name.contains("Doubler")
                && record.kind == "method"
        })
        .expect("implementation method symbol")
        .occurrence
        .as_str()
        .to_owned();
    let second_implementation_method = latest
        .generation
        .symbols()
        .symbols
        .iter()
        .find(|record| {
            record.simple_name == "process"
                && record.qualified_name.contains("Tripler")
                && record.kind == "method"
        })
        .expect("second implementation method symbol")
        .occurrence
        .as_str()
        .to_owned();
    let direct_dispatch_request = CodeRelationRequest {
        node_id: via_trait.clone(),
        maximum_depth: 1,
        resolve_trait_dispatch: false,
        scope: scope.clone(),
        meta: query_meta(),
    };
    let direct_dispatch = registry
        .callees(
            RetrievalPortContext {
                request: &graph_context,
                operation: &graph_operation,
            },
            &direct_dispatch_request,
        )
        .await;
    let RetrievalPortOutcome::Completed(direct_dispatch) = direct_dispatch else {
        panic!("expected completed direct trait call");
    };
    let direct_dispatch = direct_dispatch.payload.expect("direct trait call page");
    assert!(
        direct_dispatch
            .items
            .iter()
            .any(|record| record.symbol.node_id == trait_method)
    );
    assert!(
        direct_dispatch
            .items
            .iter()
            .all(|record| record.symbol.node_id != implementation_method)
    );

    let mut resolved_meta = query_meta();
    resolved_meta.page = PageRequest::first(1).expect("dispatch page size");
    let resolved_dispatch_request = CodeRelationRequest {
        node_id: via_trait.clone(),
        maximum_depth: 1,
        resolve_trait_dispatch: true,
        scope: scope.clone(),
        meta: resolved_meta,
    };
    let resolved_dispatch = registry
        .callees(
            RetrievalPortContext {
                request: &graph_context,
                operation: &graph_operation,
            },
            &resolved_dispatch_request,
        )
        .await;
    let RetrievalPortOutcome::Completed(resolved_dispatch) = resolved_dispatch else {
        panic!("expected completed resolved trait call");
    };
    let resolved_dispatch = resolved_dispatch.payload.expect("resolved trait call page");
    assert_eq!(resolved_dispatch.total, Some(3));
    assert_eq!(resolved_dispatch.items.len(), 1);
    assert_eq!(resolved_dispatch.items[0].symbol.node_id, trait_method);
    assert!(!resolved_dispatch.items[0].dispatch_via_trait);
    let cursor = resolved_dispatch
        .next_cursor
        .expect("resolved dispatch continuation");
    let mut continuation_meta = query_meta();
    continuation_meta.page = PageRequest::new(1, Some(cursor)).expect("dispatch continuation");
    let continuation_request = CodeRelationRequest {
        node_id: via_trait,
        maximum_depth: 1,
        resolve_trait_dispatch: true,
        scope: scope.clone(),
        meta: continuation_meta,
    };
    let continuation = registry
        .callees(
            RetrievalPortContext {
                request: &graph_context,
                operation: &graph_operation,
            },
            &continuation_request,
        )
        .await;
    let RetrievalPortOutcome::Completed(continuation) = continuation else {
        panic!("expected completed resolved trait continuation");
    };
    let continuation = continuation
        .payload
        .expect("resolved trait continuation page");
    assert_eq!(continuation.items.len(), 1);
    let second_cursor = continuation
        .next_cursor
        .clone()
        .expect("second resolved dispatch continuation");
    let mut second_continuation_meta = query_meta();
    second_continuation_meta.page =
        PageRequest::new(1, Some(second_cursor)).expect("second dispatch continuation");
    let second_continuation_request = CodeRelationRequest {
        node_id: continuation_request.node_id.clone(),
        maximum_depth: 1,
        resolve_trait_dispatch: true,
        scope: scope.clone(),
        meta: second_continuation_meta,
    };
    let second_continuation = registry
        .callees(
            RetrievalPortContext {
                request: &graph_context,
                operation: &graph_operation,
            },
            &second_continuation_request,
        )
        .await;
    let RetrievalPortOutcome::Completed(second_continuation) = second_continuation else {
        panic!("expected second completed resolved trait continuation");
    };
    let second_continuation = second_continuation
        .payload
        .expect("second resolved trait continuation page");
    assert_eq!(second_continuation.items.len(), 1);
    assert_eq!(
        continuation
            .items
            .iter()
            .chain(&second_continuation.items)
            .map(|item| item.symbol.node_id.as_str())
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([
            implementation_method.as_str(),
            second_implementation_method.as_str(),
        ])
    );
    assert!(
        continuation
            .items
            .iter()
            .chain(&second_continuation.items)
            .all(|implementation| {
                implementation.dispatch_via_trait
                    && implementation.dispatch_from.as_deref() == Some(trait_method.as_str())
                    && implementation.depth == Some(1)
            })
    );
    assert!(second_continuation.next_cursor.is_none());

    registry
        .mount_query_authority(
            fixture.path(),
            graph_context.scope(),
            query_authority_with_candidate_cap(
                latest.generation.manifest().privacy_domain.clone(),
                2,
            ),
        )
        .await
        .expect("mount candidate-capped query authority");
    let capped_dispatch_request = CodeRelationRequest {
        node_id: continuation_request.node_id.clone(),
        maximum_depth: 1,
        resolve_trait_dispatch: true,
        scope: scope.clone(),
        meta: query_meta(),
    };
    let capped_dispatch = registry
        .callees(
            RetrievalPortContext {
                request: &graph_context,
                operation: &graph_operation,
            },
            &capped_dispatch_request,
        )
        .await;
    let RetrievalPortOutcome::Partial(capped_dispatch) = capped_dispatch else {
        panic!("candidate-capped trait dispatch must report partial coverage");
    };
    let capped_page = capped_dispatch
        .payload
        .expect("candidate-capped trait dispatch page");
    assert_eq!(capped_page.items.len(), 1);
    assert_eq!(capped_page.items[0].symbol.node_id, trait_method);
    assert!(!capped_page.items[0].dispatch_via_trait);
    assert!(
        capped_dispatch
            .omissions
            .iter()
            .any(|omission| omission.reason == OmissionReason::Budget)
    );
    mount_query_authority(
        &registry,
        fixture.path(),
        &graph_context,
        latest.generation.manifest().privacy_domain.clone(),
    )
    .await;

    let qualified_name = latest
        .generation
        .symbols()
        .symbols
        .iter()
        .find(|record| record.qualified_name.ends_with("callee"))
        .expect("callee symbol")
        .qualified_name
        .clone();
    let qualified_operation =
        callable_code_operation(CallableCodeOperationKind::QualifiedName).expect("operation");
    let qualified_context =
        application_context(&qualified_operation, repository.clone(), worktree.clone());
    let qualified_request = QualifiedNameRequest {
        qualified_name,
        scope: graph_request.scope.clone(),
        meta: query_meta(),
    };
    let qualified = registry
        .qualified_name(
            RetrievalPortContext {
                request: &qualified_context,
                operation: &qualified_operation,
            },
            &qualified_request,
        )
        .await;
    assert_eq!(
        qualified
            .evidence()
            .payload
            .as_ref()
            .expect("qualified page")
            .items
            .len(),
        1
    );

    let file = latest.generation.snapshot().files[0]
        .file_occurrence_id
        .clone();
    let metadata_operation =
        callable_code_operation(CallableCodeOperationKind::SourceMetadata).expect("operation");
    let metadata_context = application_context(&metadata_operation, repository, worktree);
    let metadata_request =
        SourceMetadataRequest::new(vec![file], graph_request.scope.clone(), query_meta())
            .expect("metadata request");
    let metadata = registry
        .source_metadata(
            RetrievalPortContext {
                request: &metadata_context,
                operation: &metadata_operation,
            },
            &metadata_request,
        )
        .await;
    let metadata_page = metadata.evidence().payload.as_ref().expect("metadata page");
    assert_eq!(metadata_page.items[0].path, "src/lib.rs");
    assert_eq!(metadata_page.items[0].language.as_deref(), Some("rust"));

    let facets_operation =
        callable_code_operation(CallableCodeOperationKind::Facets).expect("operation");
    let facets_context = application_context(
        &facets_operation,
        latest.generation.snapshot().repository.clone(),
        latest
            .generation
            .snapshot()
            .worktree
            .clone()
            .expect("worktree"),
    );
    let facets = registry
        .facets(
            RetrievalPortContext {
                request: &facets_context,
                operation: &facets_operation,
            },
            &CodeFacetRequest {
                dimension: CodeFacetDimension::Kind,
                scope: graph_request.scope.clone(),
                meta: query_meta(),
            },
        )
        .await;
    assert!(
        !facets
            .evidence()
            .payload
            .as_ref()
            .expect("facet page")
            .items
            .is_empty()
    );

    let timeline_operation =
        callable_code_operation(CallableCodeOperationKind::Timeline).expect("operation");
    let timeline_context = application_context(
        &timeline_operation,
        latest.generation.snapshot().repository.clone(),
        latest
            .generation
            .snapshot()
            .worktree
            .clone()
            .expect("worktree"),
    );
    let timeline = registry
        .timeline(
            RetrievalPortContext {
                request: &timeline_context,
                operation: &timeline_operation,
            },
            &CodeTimelineRequest {
                scope: graph_request.scope.clone(),
                meta: query_meta(),
            },
        )
        .await;
    assert_eq!(
        timeline
            .evidence()
            .payload
            .as_ref()
            .expect("timeline page")
            .items
            .len(),
        1
    );

    let callee = latest
        .generation
        .symbols()
        .symbols
        .iter()
        .find(|record| record.qualified_name.ends_with("callee"))
        .expect("callee")
        .occurrence
        .as_str()
        .to_owned();
    let processor = latest
        .generation
        .symbols()
        .symbols
        .iter()
        .find(|record| record.qualified_name.ends_with("Processor"))
        .expect("processor trait")
        .occurrence
        .as_str()
        .to_owned();
    let doubler = latest
        .generation
        .symbols()
        .symbols
        .iter()
        .find(|record| record.qualified_name.ends_with("Doubler"))
        .expect("doubler type")
        .occurrence
        .as_str()
        .to_owned();
    let references_operation =
        callable_code_operation(CallableCodeOperationKind::References).expect("operation");
    let references_context = application_context(
        &references_operation,
        latest.generation.snapshot().repository.clone(),
        latest
            .generation
            .snapshot()
            .worktree
            .clone()
            .expect("worktree"),
    );
    let references = registry
        .references(
            RetrievalPortContext {
                request: &references_context,
                operation: &references_operation,
            },
            &CodeNavigationRequest {
                node_id: callee.clone(),
                scope: graph_request.scope.clone(),
                meta: query_meta(),
            },
        )
        .await;
    assert!(
        !references
            .evidence()
            .payload
            .as_ref()
            .expect("references page")
            .items
            .is_empty()
    );

    let warming_text = {
        let mounted = registry.mounted.lock().await;
        mounted
            .get(&fixture.path().canonicalize().expect("canonical root"))
            .expect("mounted worktree")
            .historical_generation_owner
            .published_text_generation(&generation)
            .expect("read retained generation metadata")
            .expect("partitioned retained generation")
    };
    install_verified_graph_store_on_text(&warming_text, &latest);
    assert!(
        !warming_text.text_serving_is_ready(),
        "the callable graph check must use an owner whose lexical projection is still warming"
    );

    let cold_repository = latest.generation.snapshot().repository.clone();
    let cold_worktree = latest
        .generation
        .snapshot()
        .worktree
        .clone()
        .expect("worktree identity");
    let scheduler = {
        let mounted = registry.mounted.lock().await;
        let worktree = mounted
            .get(&fixture.path().canonicalize().expect("canonical root"))
            .expect("mounted worktree");
        *worktree
            .serving_generation
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        *worktree
            .text_generation
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(warming_text);
        assert!(
            worktree
                .text_generation
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_some(),
            "the verified graph remains seated on its lightweight generation owner"
        );
        Arc::clone(&worktree.scheduler)
    };
    drop(latest);
    let decodes_before = scheduler
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .sealed_decode_count();
    let held_decode = scheduler
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .hold_active_decode();
    let cursor_profile = TempDir::new().expect("cursor profile");
    let cursor_runtime =
        tracedecay_global_db::tests::harness::RegisteredGlobalDbTestRuntime::profile(
            cursor_profile.path(),
        )
        .await
        .expect("cursor key runtime");
    let cursor_keys = cursor_runtime
        .profile_database()
        .load_session_cursor_key_provider_result()
        .await
        .expect("cursor keys");
    tokio::time::timeout(
        Duration::from_secs(2),
        super::super::query_runtime::mount_core_query_authority_on_project_open(
            &registry,
            fixture.path(),
            graph_context.scope(),
            &cursor_keys,
        ),
    )
    .await
    .expect("core query authority mount must not wait on lexical decode")
    .expect("mount core query authority from retained metadata");
    macro_rules! assert_graph_only_query {
        ($kind:expr, $method:ident, $request:expr) => {{
            let operation = callable_code_operation($kind).expect("operation");
            let context =
                application_context(&operation, cold_repository.clone(), cold_worktree.clone());
            let outcome = tokio::time::timeout(
                Duration::from_secs(2),
                registry.$method(
                    RetrievalPortContext {
                        request: &context,
                        operation: &operation,
                    },
                    &$request,
                ),
            )
            .await
            .expect("graph-only query must not wait on the lexical decode");
            assert!(
                matches!(outcome, RetrievalPortOutcome::Completed(_)),
                "{} must serve from the retained verified graph: {outcome:?}",
                $kind.as_str(),
            );
            assert_eq!(
                scheduler
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .sealed_decode_count(),
                decodes_before,
                "{} must not decode the sealed lexical generation",
                $kind.as_str(),
            );
        }};
    }
    assert_graph_only_query!(
        CallableCodeOperationKind::Implementations,
        implementations,
        CodeImplementationsRequest {
            selector: ImplementationSelector::Trait {
                name: "Processor".to_owned(),
            },
            scope: graph_request.scope.clone(),
            meta: query_meta(),
        }
    );
    assert_graph_only_query!(
        CallableCodeOperationKind::TypeHierarchy,
        type_hierarchy,
        CodeHierarchyRequest {
            node_id: doubler.clone(),
            maximum_depth: 2,
            scope: graph_request.scope.clone(),
            meta: query_meta(),
        }
    );
    assert_graph_only_query!(
        CallableCodeOperationKind::Callers,
        callers,
        CodeRelationRequest {
            node_id: callee.clone(),
            maximum_depth: 2,
            resolve_trait_dispatch: false,
            scope: graph_request.scope.clone(),
            meta: query_meta(),
        }
    );
    assert_graph_only_query!(
        CallableCodeOperationKind::Impact,
        impact,
        CodeImpactRequest {
            node_id: callee.clone(),
            maximum_depth: 2,
            scope: graph_request.scope.clone(),
            meta: query_meta(),
        }
    );
    assert_graph_only_query!(
        CallableCodeOperationKind::Declaration,
        declaration,
        CodeNavigationRequest {
            node_id: processor,
            scope: graph_request.scope.clone(),
            meta: query_meta(),
        }
    );
    assert_graph_only_query!(
        CallableCodeOperationKind::TypeDefinition,
        type_definition,
        CodeNavigationRequest {
            node_id: doubler,
            scope: graph_request.scope.clone(),
            meta: query_meta(),
        }
    );
    assert_graph_only_query!(
        CallableCodeOperationKind::References,
        references,
        CodeNavigationRequest {
            node_id: callee.clone(),
            scope: graph_request.scope.clone(),
            meta: query_meta(),
        }
    );
    assert_graph_only_query!(
        CallableCodeOperationKind::ModuleApi,
        module_api,
        ModuleApiRequest {
            path: "src/lib.rs".to_owned(),
            scope: graph_request.scope.clone(),
            meta: query_meta(),
        }
    );
    let graph = registry
        .callees(
            RetrievalPortContext {
                request: &graph_context,
                operation: &graph_operation,
            },
            &graph_request,
        )
        .await;
    assert!(
        matches!(graph, RetrievalPortOutcome::Completed(_)),
        "a generation-pinned graph query must use the retained verified graph: {graph:?}"
    );
    assert_eq!(
        scheduler
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .sealed_decode_count(),
        decodes_before,
        "graph-only query admission must not decode the sealed lexical generation"
    );

    drop(held_decode);
    registry.shutdown().await;
}

#[tokio::test]
async fn callers_page_reports_candidate_cap_and_hydrates_only_the_requested_slice() {
    let sources = caller_star_sources();
    let files = sources
        .iter()
        .map(|(path, source)| (path.as_str(), source.as_str()))
        .collect::<Vec<_>>();
    let fixture = GitFixture::new(&files);
    let store = TempDir::new().expect("store root");
    let registry = CodeIndexSchedulerRegistryV1::new(1);
    registry
        .mount_worktree(
            test_project_id(),
            fixture.path(),
            store.path().to_path_buf(),
            None,
        )
        .await
        .expect("mount daemon-owned scheduler");
    let latest = wait_for_live_complete_generation(&registry, fixture.path()).await;
    install_verified_graph_store(&latest);
    let generation = latest.generation.manifest().generation_id.clone();
    let repository = latest.generation.snapshot().repository.clone();
    let worktree = latest
        .generation
        .snapshot()
        .worktree
        .clone()
        .expect("worktree identity");
    let scope = CodeQueryScope::new(generation.clone(), None).expect("query scope");
    let hub = latest
        .generation
        .symbols()
        .symbols
        .iter()
        .find(|record| record.qualified_name.ends_with("hub"))
        .expect("hub symbol");
    let operation = callable_code_operation(CallableCodeOperationKind::Callers).expect("operation");
    let context = application_context(&operation, repository, worktree);
    mount_query_authority(
        &registry,
        fixture.path(),
        &context,
        latest.generation.manifest().privacy_domain.clone(),
    )
    .await;
    let request = CodeRelationRequest {
        node_id: hub.occurrence.as_str().to_owned(),
        maximum_depth: 1,
        resolve_trait_dispatch: false,
        scope: scope.clone(),
        meta: callers_page_meta(CALLER_PAGE, None),
    };
    let first = registry
        .callers(
            RetrievalPortContext {
                request: &context,
                operation: &operation,
            },
            &request,
        )
        .await;
    let first_page = match first {
        RetrievalPortOutcome::Partial(evidence) => {
            assert_eq!(evidence.coverage.eligible, Some(33));
            assert_eq!(evidence.omissions.len(), 1);
            assert_eq!(evidence.omissions[0].reason, OmissionReason::Budget);
            evidence.payload.expect("first callers page")
        }
        other => panic!("expected capped callers page, got {other:?}"),
    };
    assert_eq!(first_page.items.len(), CALLER_PAGE as usize);
    assert_eq!(first_page.total, Some(32));
    let page1_hydrations = registry.take_relation_symbol_hydrations();
    assert_eq!(
        page1_hydrations,
        u64::from(CALLER_PAGE),
        "page 1 must hydrate only the returned slice; observed {page1_hydrations}"
    );

    let cursor = first_page.next_cursor.clone().expect("page 2 cursor");
    let second_request = CodeRelationRequest {
        node_id: hub.occurrence.as_str().to_owned(),
        maximum_depth: 1,
        resolve_trait_dispatch: false,
        scope: scope.clone(),
        meta: callers_page_meta(CALLER_PAGE, Some(cursor)),
    };
    let second = registry
        .callers(
            RetrievalPortContext {
                request: &context,
                operation: &operation,
            },
            &second_request,
        )
        .await;
    let second_page = match second {
        RetrievalPortOutcome::Partial(evidence) => evidence.payload.expect("second callers page"),
        other => panic!("expected capped callers continuation, got {other:?}"),
    };
    assert_eq!(second_page.items.len(), CALLER_PAGE as usize);
    assert!(
        second_page
            .items
            .iter()
            .all(|item| !first_page.items.contains(item)),
        "page 2 must return a disjoint slice"
    );
    let page2_hydrations = registry.take_relation_symbol_hydrations();
    assert_eq!(
        page2_hydrations,
        u64::from(CALLER_PAGE),
        "page 2 must hydrate only the returned slice; observed {page2_hydrations}"
    );

    let mut collected = first_page.items.clone();
    collected.extend(second_page.items);
    let mut cursor = second_page.next_cursor;
    while let Some(next) = cursor {
        let page_request = CodeRelationRequest {
            node_id: hub.occurrence.as_str().to_owned(),
            maximum_depth: 1,
            resolve_trait_dispatch: false,
            scope: scope.clone(),
            meta: callers_page_meta(CALLER_PAGE, Some(next)),
        };
        let page = match registry
            .callers(
                RetrievalPortContext {
                    request: &context,
                    operation: &operation,
                },
                &page_request,
            )
            .await
        {
            RetrievalPortOutcome::Partial(evidence) => evidence.payload.expect("callers page"),
            other => panic!("expected capped callers page, got {other:?}"),
        };
        collected.extend(page.items);
        cursor = page.next_cursor;
    }
    let _ = registry.take_relation_symbol_hydrations();
    assert_eq!(
        collected.len(),
        32,
        "the declared candidate cap is enforced"
    );
    let identities = collected
        .iter()
        .map(|record| record.symbol.node_id.as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(identities.len(), collected.len(), "capped rows stay unique");
    registry.shutdown().await;
}

// ---------------------------------------------------------------------------
// Worktree-aware incremental indexing: identity, gix classification, the
// hook-driven + lazy-reconcile freshness ladder.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn unpinned_query_resolves_exact_admitted_worktree_scope() {
    let left = GitFixture::new(&[("src/lib.rs", "pub fn left_only() {}\n")]);
    let right = GitFixture::new(&[("src/lib.rs", "pub fn right_only() {}\n")]);
    let (first, target, target_literal) = if left.path().canonicalize().expect("left root")
        < right.path().canonicalize().expect("right root")
    {
        (&left, &right, "right_only")
    } else {
        (&right, &left, "left_only")
    };
    let store = TempDir::new().expect("store root");
    let registry = CodeIndexSchedulerRegistryV1::new(2);
    registry
        .mount_worktree(
            ProjectId::new("project.unpinned.first").expect("valid project"),
            first.path(),
            store.path().to_path_buf(),
            None,
        )
        .await
        .expect("mount first worktree");
    registry
        .mount_worktree(
            test_project_id(),
            target.path(),
            store.path().to_path_buf(),
            None,
        )
        .await
        .expect("mount target worktree");
    // Unpinned exact queries resolve through the text owner, so the target
    // must be text-current; neither worktree's graph seat is exercised here.
    wait_for_queryable_text_generation(&registry, first.path()).await;
    let target_latest = wait_for_queryable_text_generation(&registry, target.path()).await;
    let target_generation = target_latest.metadata().manifest().generation_id.clone();
    let repository = target_latest.metadata().snapshot().repository.clone();
    let worktree = target_latest
        .metadata()
        .snapshot()
        .worktree
        .clone()
        .expect("target worktree identity");
    let operation =
        callable_code_operation(CallableCodeOperationKind::ExactOccurrence).expect("operation");
    let context = application_context(&operation, repository, worktree);
    mount_query_authority(
        &registry,
        target.path(),
        &context,
        target_latest.metadata().manifest().privacy_domain.clone(),
    )
    .await;
    let scope = CodeQueryScope::new(super::super::queries::unpinned_latest_generation(), None)
        .expect("scope");
    let request = ExactOccurrenceRequest::new(target_literal, None, scope, query_meta())
        .expect("exact request");

    let outcome = registry
        .exact_occurrence(
            RetrievalPortContext {
                request: &context,
                operation: &operation,
            },
            &request,
        )
        .await;
    let served = match outcome {
        RetrievalPortOutcome::Completed(evidence) => {
            let page = evidence.payload.expect("exact page");
            assert!(!page.items.is_empty(), "target-only symbol is returned");
            page.generation
        }
        other => panic!("expected completed scoped query, got {other:?}"),
    };
    assert_eq!(served, target_generation);
    registry.shutdown().await;
}

#[tokio::test]
async fn unpinned_cursor_continues_on_its_immutable_generation() {
    let fixture = GitFixture::new(&[(
        "src/lib.rs",
        "mod a { pub fn shared() {} }\nmod b { pub fn shared() {} }\nmod c { pub fn shared() {} }\n",
    )]);
    let store = TempDir::new().expect("store root");
    let registry = CodeIndexSchedulerRegistryV1::new(1);
    registry
        .mount_worktree(
            test_project_id(),
            fixture.path(),
            store.path().to_path_buf(),
            None,
        )
        .await
        .expect("mount worktree");
    let initial = wait_for_live_complete_generation(&registry, fixture.path()).await;
    let operation =
        callable_code_operation(CallableCodeOperationKind::ExactOccurrence).expect("operation");
    let context = application_context(
        &operation,
        initial.generation.snapshot().repository.clone(),
        initial
            .generation
            .snapshot()
            .worktree
            .clone()
            .expect("worktree identity"),
    );
    mount_query_authority(
        &registry,
        fixture.path(),
        &context,
        initial.generation.manifest().privacy_domain.clone(),
    )
    .await;
    let scope = CodeQueryScope::new(super::super::queries::unpinned_latest_generation(), None)
        .expect("scope");
    let first_request = ExactOccurrenceRequest::new(
        "shared",
        None,
        scope.clone(),
        RetrievalRequestMeta::current(
            PageRequest::first(1).expect("first page"),
            ResultProjection::Evidence,
            RetrievalOrder::Relevance,
        ),
    )
    .expect("first request");
    let first = registry
        .exact_occurrence(
            RetrievalPortContext {
                request: &context,
                operation: &operation,
            },
            &first_request,
        )
        .await;
    let first_page = match first {
        RetrievalPortOutcome::Completed(evidence) => evidence.payload.expect("first page"),
        other => panic!("expected first page, got {other:?}"),
    };
    let cursor = first_page.next_cursor.clone().expect("continuation cursor");
    let original_generation = first_page.generation.clone();

    // The envelope prefix names the cursor wire revision and is bumped whenever
    // that contract changes (it is `ccq2.` today). Take it from the cursor the
    // production path just minted rather than pinning a literal here: this test
    // is about expiry tampering, not about which revision is current.
    let (prefix, encoded) = cursor
        .as_str()
        .split_once('.')
        .expect("callable cursor revision prefix");
    let mut tampered: serde_json::Value =
        serde_json::from_slice(&decode_hex(encoded)).expect("cursor JSON");
    tampered["payload"]["expires_at"] = serde_json::json!(0);
    let tampered = OpaqueCursor::new(format!(
        "{prefix}.{}",
        encode_lowercase_hex(&serde_json::to_vec(&tampered).expect("tampered cursor JSON"))
    ))
    .expect("tampered cursor");
    let tampered_request = ExactOccurrenceRequest::new(
        "shared",
        None,
        scope.clone(),
        RetrievalRequestMeta::current(
            PageRequest::new(1, Some(tampered)).expect("tampered continuation page"),
            ResultProjection::Evidence,
            RetrievalOrder::Relevance,
        ),
    )
    .expect("tampered continuation request");
    let tampered_outcome = registry
        .exact_occurrence(
            RetrievalPortContext {
                request: &context,
                operation: &operation,
            },
            &tampered_request,
        )
        .await;
    let RetrievalPortOutcome::Unavailable(tampered_evidence) = tampered_outcome else {
        panic!("tampered cursor must be rejected");
    };
    assert_eq!(
        tampered_evidence.omissions[0].reason,
        OmissionReason::Failed,
        "MAC verification must precede expiry and other binding diagnostics"
    );

    fixture.edit(
        "src/lib.rs",
        "mod a { pub fn shared() {} }\nmod b { pub fn shared() {} }\nmod c { pub fn shared() {} }\npub fn unrelated() {}\n",
    );
    git(fixture.path(), &["commit", "-qam", "refresh"]);
    // Serve-old-first: the freshness ladder answers from the retained
    // generation and only *requests* the rebuild, so the new generation
    // arrives from the background worker rather than from this call.
    let _requested = registry
        .latest_complete_fresh(fixture.path())
        .await
        .expect("retained generation stays servable while the rebuild runs");
    // The cursor's immutability is tested against a *text-current* successor:
    // an unpinned exact query now resolves the new generation while the
    // continuation must stay on the one the cursor was minted for. Graph
    // seating of that successor is irrelevant to exact serving.
    let refreshed =
        wait_for_queryable_text_generation_change(&registry, fixture.path(), &original_generation)
            .await
            .metadata()
            .manifest()
            .generation_id
            .clone();
    assert_ne!(refreshed, original_generation);

    let continuation_request = ExactOccurrenceRequest::new(
        "shared",
        None,
        scope,
        RetrievalRequestMeta::current(
            PageRequest::new(1, Some(cursor)).expect("continuation page"),
            ResultProjection::Evidence,
            RetrievalOrder::Relevance,
        ),
    )
    .expect("continuation request");
    let continuation = registry
        .exact_occurrence(
            RetrievalPortContext {
                request: &context,
                operation: &operation,
            },
            &continuation_request,
        )
        .await;
    let continuation_page = match continuation {
        RetrievalPortOutcome::Completed(evidence) => evidence.payload.expect("continuation page"),
        other => panic!("expected continuation page, got {other:?}"),
    };
    assert_eq!(continuation_page.generation, original_generation);
    assert_eq!(continuation_page.items.len(), 1);
    registry.shutdown().await;
}

#[tokio::test]
async fn pinned_generation_from_another_worktree_is_unavailable() {
    let owner = GitFixture::new(&[("src/lib.rs", "pub fn owner_only() {}\n")]);
    let requester = GitFixture::new(&[("src/lib.rs", "pub fn requester_only() {}\n")]);
    let store = TempDir::new().expect("store root");
    let registry = CodeIndexSchedulerRegistryV1::new(2);
    registry
        .mount_worktree(
            ProjectId::new("project.pinned.owner").expect("valid project"),
            owner.path(),
            store.path().to_path_buf(),
            None,
        )
        .await
        .expect("mount owner worktree");
    registry
        .mount_worktree(
            test_project_id(),
            requester.path(),
            store.path().to_path_buf(),
            None,
        )
        .await
        .expect("mount requester worktree");
    let owner_generation = wait_for_live_complete_generation(&registry, owner.path())
        .await
        .generation
        .manifest()
        .generation_id
        .clone();
    let requester_latest = wait_for_live_complete_generation(&registry, requester.path()).await;
    let operation =
        callable_code_operation(CallableCodeOperationKind::ExactOccurrence).expect("operation");
    let context = application_context(
        &operation,
        requester_latest.generation.snapshot().repository.clone(),
        requester_latest
            .generation
            .snapshot()
            .worktree
            .clone()
            .expect("requester worktree identity"),
    );
    let scope = CodeQueryScope::new(owner_generation, None).expect("scope");
    let request =
        ExactOccurrenceRequest::new("owner_only", None, scope, query_meta()).expect("request");

    assert!(matches!(
        registry
            .exact_occurrence(
                RetrievalPortContext {
                    request: &context,
                    operation: &operation,
                },
                &request,
            )
            .await,
        RetrievalPortOutcome::Unavailable(_)
    ));
    registry.shutdown().await;
}

#[tokio::test]
async fn symbol_search_is_generation_bound_and_uses_mounted_authority() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn alpha() {}\n")]);
    let store = TempDir::new().expect("store root");
    let registry = CodeIndexSchedulerRegistryV1::new(1);
    registry
        .mount_worktree(
            test_project_id(),
            fixture.path(),
            store.path().to_path_buf(),
            None,
        )
        .await
        .expect("mount worktree");
    let latest = wait_for_live_complete_generation(&registry, fixture.path()).await;
    let generation = latest.generation.manifest().generation_id.clone();
    let operation =
        callable_code_operation(CallableCodeOperationKind::SymbolSearch).expect("operation");
    let context = application_context(
        &operation,
        latest.generation.snapshot().repository.clone(),
        latest
            .generation
            .snapshot()
            .worktree
            .clone()
            .expect("worktree identity"),
    );
    mount_query_authority(
        &registry,
        fixture.path(),
        &context,
        latest.generation.manifest().privacy_domain.clone(),
    )
    .await;
    let query = EphemeralSanitizedQueryViewV1::sanitize(
        "alpha",
        SanitizerRevision::new("sanitizer.query.fixture").expect("sanitizer"),
        QueryNormalizationRevision::new("normalization.query.fixture").expect("normalization"),
    )
    .expect("query");
    let request = CodeSymbolSearchRequest {
        query,
        scope: CodeQueryScope::new(generation.clone(), None).expect("scope"),
        meta: query_meta(),
    };

    let outcome = registry
        .symbol_search(
            RetrievalPortContext {
                request: &context,
                operation: &operation,
            },
            &request,
        )
        .await;
    let RetrievalPortOutcome::Completed(evidence) = outcome else {
        panic!("mounted symbol-search authority must complete, got {outcome:?}");
    };
    let page = evidence.payload.expect("symbol-search page");
    assert_eq!(page.generation, generation);
    assert!(
        page.items.iter().any(|symbol| symbol.name == "alpha"),
        "implemented symbol search must return the indexed symbol"
    );
    registry.shutdown().await;
}

/// A query that pins no explicit generation (the reserved unpinned sentinel)
/// resolves its serving generation through the three-tier freshness ladder. An
/// out-of-band git commit after indexing is caught by the tier-1 `.git`
/// metadata check at query admission, so the unpinned query serves the freshly
/// reconciled latest generation rather than the stale one indexed at mount.
#[tokio::test]
async fn unpinned_query_serves_freshness_resolved_latest_generation() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn alpha() -> u32 { 1 }\n")]);
    let store = TempDir::new().expect("store root");
    let registry = CodeIndexSchedulerRegistryV1::new(1);
    registry
        .mount_worktree(
            test_project_id(),
            fixture.path(),
            store.path().to_path_buf(),
            None,
        )
        .await
        .expect("mount daemon-owned scheduler");
    // Exact/lexical unpinned resolution reads the text owner. Waiting on the
    // post-graph publication bus (or `latest_complete_fresh`, which abstains
    // while only text is seated) races optional graph seating.
    let initial_text = wait_for_queryable_text_generation(&registry, fixture.path()).await;
    let initial = initial_text.metadata().manifest().generation_id.clone();
    let snapshot = initial_text.metadata().snapshot();
    let repository = snapshot.repository.clone();
    let worktree = snapshot.worktree.clone().expect("worktree identity");
    let resolved = ResolvedScope::new(
        test_project_id(),
        repository.clone(),
        worktree.clone(),
        snapshot.reference.clone(),
    )
    .expect("resolved scope");

    // Another process commits a change. Nothing notifies the scheduler, and no
    // filesystem watcher exists, so the seated text owner still reports `initial`
    // until a freshness check runs at query admission.
    fixture.edit("src/lib.rs", "pub fn alpha() -> u32 { 2 }\n");
    git(fixture.path(), &["commit", "-qam", "external"]);
    assert_eq!(
        registry
            .latest_text_serving_for_root(fixture.path())
            .await
            .map(|text| text.metadata().manifest().generation_id.clone()),
        Some(initial.clone()),
        "an out-of-band commit is not reflected until the freshness ladder runs"
    );

    // The unpinned query's ladder checks inline and hands the rebuild to the
    // background worker. Join the text-owner receipt that exact resolution
    // actually serves — not the later graph-bearing serving swap.
    let _ = registry.latest_text_fresh_for_scope(&resolved).await;
    let next = wait_for_queryable_text_generation_change(&registry, fixture.path(), &initial).await;
    let expected = next.metadata().manifest().generation_id.clone();

    let operation =
        callable_code_operation(CallableCodeOperationKind::ExactOccurrence).expect("operation");
    let context = application_context(&operation, repository, worktree);
    mount_query_authority(
        &registry,
        fixture.path(),
        &context,
        next.metadata().manifest().privacy_domain.clone(),
    )
    .await;
    let scope = CodeQueryScope::new(super::super::queries::unpinned_latest_generation(), None)
        .expect("scope");
    let request =
        ExactOccurrenceRequest::new("alpha", None, scope, query_meta()).expect("exact request");
    let outcome = registry
        .exact_occurrence(
            RetrievalPortContext {
                request: &context,
                operation: &operation,
            },
            &request,
        )
        .await;

    let served = match outcome {
        RetrievalPortOutcome::Completed(evidence) => {
            evidence.payload.expect("exact page").generation
        }
        other => panic!("expected a completed unpinned query, got {other:?}"),
    };
    assert_ne!(
        served, initial,
        "the unpinned query serves the freshness-resolved latest generation, not the stale one"
    );
    assert_eq!(
        served, expected,
        "query admission reconciled the out-of-band commit into the served text generation"
    );

    registry.shutdown().await;
}

/// An explicit caller-pinned generation is served generation-bound and
/// read-only: the freshness ladder is bypassed, so an out-of-band commit after
/// indexing never mutates the served generation and never triggers a reconcile.
#[tokio::test]
async fn pinned_query_bypasses_freshness_resolution() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn alpha() -> u32 { 1 }\n")]);
    let store = TempDir::new().expect("store root");
    let registry = CodeIndexSchedulerRegistryV1::new(1);
    registry
        .mount_worktree(
            test_project_id(),
            fixture.path(),
            store.path().to_path_buf(),
            None,
        )
        .await
        .expect("mount daemon-owned scheduler");
    let latest = wait_for_live_complete_generation(&registry, fixture.path()).await;
    let initial = latest.generation.manifest().generation_id.clone();
    assert_eq!(latest.generation.manifest().generation_id, initial);
    let repository = latest.generation.snapshot().repository.clone();
    let worktree = latest
        .generation
        .snapshot()
        .worktree
        .clone()
        .expect("worktree identity");

    // The same out-of-band commit as the unpinned case: it would be caught by
    // the tier-1 metadata check *if* the freshness ladder ran.
    fixture.edit("src/lib.rs", "pub fn alpha() -> u32 { 2 }\n");
    git(fixture.path(), &["commit", "-qam", "external"]);

    // The caller pins the exact generation indexed at mount.
    let operation =
        callable_code_operation(CallableCodeOperationKind::ExactOccurrence).expect("operation");
    let context = application_context(&operation, repository, worktree);
    mount_query_authority(
        &registry,
        fixture.path(),
        &context,
        latest.generation.manifest().privacy_domain.clone(),
    )
    .await;
    let scope = CodeQueryScope::new(initial.clone(), None).expect("scope");
    let request =
        ExactOccurrenceRequest::new("alpha", None, scope, query_meta()).expect("exact request");
    let outcome = registry
        .exact_occurrence(
            RetrievalPortContext {
                request: &context,
                operation: &operation,
            },
            &request,
        )
        .await;

    let served = match outcome {
        RetrievalPortOutcome::Completed(evidence) => {
            evidence.payload.expect("exact page").generation
        }
        other => panic!("expected a completed pinned query, got {other:?}"),
    };
    assert_eq!(
        served, initial,
        "a pinned query serves exactly the requested generation, bypassing freshness"
    );
    assert_eq!(
        registry.latest_generation_id(fixture.path()).await,
        Some(initial),
        "the pinned read never triggers a reconcile of the out-of-band commit"
    );

    registry.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn generation_read_callers_install_exact_affected_test_attribution() {
    let fixture = GitFixture::new(&[(
        "tests/production.rs",
        "fn helper() {}\n#[test]\nfn verifies_helper() { helper(); }\n",
    )]);
    let store = TempDir::new().expect("store root");
    let registry = CodeIndexSchedulerRegistryV1::new(1);
    registry
        .mount_worktree(
            test_project_id(),
            fixture.path(),
            store.path().to_path_buf(),
            None,
        )
        .await
        .expect("mount fixture");
    let latest = wait_for_live_complete_generation(&registry, fixture.path()).await;
    let generation_id = latest.generation().manifest().generation_id.clone();
    let snapshot = latest.generation().snapshot();
    let scope = ResolvedScope::new(
        test_project_id(),
        snapshot.repository.clone(),
        snapshot.worktree.clone().expect("worktree id"),
        snapshot.reference.clone(),
    )
    .expect("resolved scope");
    let occurrence = |name: &str| {
        latest
            .generation()
            .symbols()
            .symbols
            .iter()
            .find(|symbol| symbol.simple_name == name)
            .unwrap_or_else(|| panic!("fixture symbol {name}"))
            .occurrence
            .clone()
    };
    let helper = occurrence("helper");
    let test = occurrence("verifies_helper");

    let assert_attribution = || {
        let read = registry.read_test_attribution(&generation_id);
        assert_eq!(
            read.provider_state,
            ProviderEvaluationStateV1::Partial,
            "the real graph reports its honest partial attribution coverage"
        );
        let join = read.evidence.expect("generation attribution evidence");
        let record = join
            .records
            .iter()
            .find(|record| record.attribution.test_occurrence == test)
            .expect("exact test attribution");
        assert!(record.attribution.covered_occurrences.contains(&helper));
        assert_eq!(
            record
                .test_occurrence
                .as_ref()
                .map(|occurrence| &occurrence.occurrence_id),
            Some(&test)
        );
    };

    registry.remove_test_attribution_authority(fixture.path());
    assert_eq!(
        registry
            .read_test_attribution(&generation_id)
            .provider_state,
        ProviderEvaluationStateV1::Unavailable,
        "an uninstalled generation stays typed unavailable"
    );
    let fresh = registry
        .latest_complete_fresh(fixture.path())
        .await
        .expect("fresh caller resolves generation");
    assert_eq!(fresh.generation().manifest().generation_id, generation_id);
    assert_attribution();

    registry.remove_test_attribution_authority(fixture.path());
    let ready = registry
        .latest_complete_ready_for_scope(&scope)
        .await
        .expect("ready caller resolves generation");
    assert_eq!(ready.generation().manifest().generation_id, generation_id);
    assert_attribution();

    registry.remove_test_attribution_authority(fixture.path());
    let decoded = registry
        .latest_complete_ready_decoded_for_root_scope(fixture.path(), &scope)
        .await
        .expect("ready-decoded caller resolves generation");
    assert_eq!(decoded.generation().manifest().generation_id, generation_id);
    assert_attribution();

    registry.shutdown().await;
}

/// A graph-off cold mount must keep the authenticated lightweight text owner
/// authoritative when an overflow reconcile arrives between bounded text
/// slices. Rebinding the same sealed generation through the full-generation
/// cache clears the live progress slot, invalidates the surviving owner's
/// epoch, and decodes gigabytes that graph-off serving never needs.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn graph_off_overflow_preserves_text_owner_progress_without_full_decode() {
    let sources = (0..512)
        .map(|index| {
            (
                format!("src/file_{index:04}.rs"),
                format!("pub fn alpha_{index:04}() -> usize {{ {index} }}\n"),
            )
        })
        .collect::<Vec<_>>();
    let source_refs = sources
        .iter()
        .map(|(path, source)| (path.as_str(), source.as_str()))
        .collect::<Vec<_>>();
    let fixture = GitFixture::new(&source_refs);
    let store = TempDir::new().expect("store root");
    let scoped_store = super::super::scoped_code_index_store_root(
        store.path(),
        &fixture.path().canonicalize().expect("canonical fixture"),
    );
    let (scope, privacy_domain) = {
        let mut scheduler = scheduler(
            &fixture,
            scoped_store.clone(),
            Arc::new(SharedCodeIndexBytePoolV1::default()),
        );
        published(scheduler.reconcile_now().expect("seed retained generation"));
        let latest = scheduler.latest_complete().expect("seeded generation");
        let snapshot = latest.generation.snapshot();
        (
            ResolvedScope::new(
                test_project_id(),
                snapshot.repository.clone(),
                snapshot.worktree.clone().expect("worktree id"),
                snapshot.reference.clone(),
            )
            .expect("resolved scope"),
            latest.generation.manifest().privacy_domain.clone(),
        )
    };
    let freshness_witness = scoped_store.join("freshness_witness.v1");
    assert!(
        freshness_witness.is_file(),
        "seeding the preserved profile persists its restore witness"
    );
    std::fs::remove_file(freshness_witness)
        .expect("remove restore witness to reproduce a cold preserved-profile mount");

    let registry = CodeIndexSchedulerRegistryV1::with_background_reconcile_permits(1, 1);
    registry
        .mount_worktree_with_graph_policy(
            test_project_id(),
            fixture.path(),
            store.path().to_path_buf(),
            None,
            super::super::CodeGraphActivationPolicyV1::RefusedByConfiguration,
        )
        .await
        .expect("mount graph-off scheduler");
    registry
        .mount_query_authority(fixture.path(), &scope, query_authority(privacy_domain))
        .await
        .expect("mount query authority");
    let scheduler = {
        let mounted = registry.mounted.lock().await;
        Arc::clone(
            &mounted
                .get(&fixture.path().canonicalize().expect("canonical root"))
                .expect("mounted worktree")
                .scheduler,
        )
    };

    let progress_deadline = std::time::Instant::now() + Duration::from_secs(10);
    let (owner_epoch_before_overflow, progress_before_overflow) = loop {
        let observed = {
            let scheduler = scheduler
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let progress = scheduler.build_progress_slot();
            let progress = progress
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            (progress.owner_epoch, progress.snapshot())
        };
        if let (owner_epoch, Some(progress)) = observed
            && progress.committed_pages > 0
            && progress.phase
                != tracedecay_contracts::code_index_freshness::CodeIndexBuildPhaseV1::Ready
        {
            break (owner_epoch, progress);
        }
        assert!(
            std::time::Instant::now() <= progress_deadline,
            "text projection completed before exposing bounded live progress"
        );
        tokio::time::sleep(Duration::from_millis(2)).await;
    };
    let status_poll_admission = registry
        .background_reconcile_admission()
        .acquire_owned()
        .await
        .expect("pause text projection after a bounded committed slice");
    let (last_reconciled_before_status_polls, original_staleness_threshold) = {
        let mut scheduler = scheduler
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let original_staleness_threshold = scheduler.policy.staleness_threshold;
        scheduler.policy.staleness_threshold = Duration::ZERO;
        (
            scheduler.last_reconciled_at_micros(),
            original_staleness_threshold,
        )
    };
    for _ in 0..8 {
        assert!(
            !registry.has_current_ready_decoded_for_root_scope(fixture.path(), &scope),
            "graph-off status census has no fully decoded generation"
        );
    }
    {
        let scheduler = scheduler
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(
            scheduler.pending_hint_count(),
            Some(0),
            "stat-equal status polling must not fabricate an overflow reconcile"
        );
        assert_eq!(
            scheduler.last_reconciled_at_micros(),
            last_reconciled_before_status_polls,
            "stat-equal status polling must not run a capture pass"
        );
        assert_eq!(
            scheduler.sealed_decode_count(),
            0,
            "graph-off status polling must not decode the full generation"
        );
        let progress = scheduler.build_progress_slot();
        let progress = progress
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(progress.owner_epoch, owner_epoch_before_overflow);
        let progress = progress
            .snapshot()
            .expect("status polling preserves live text progress");
        assert_eq!(
            progress.daemon_incarnation,
            progress_before_overflow.daemon_incarnation
        );
        assert_eq!(
            progress.producer_incarnation,
            progress_before_overflow.producer_incarnation
        );
        assert!(progress.progress_epoch >= progress_before_overflow.progress_epoch);
        assert!(progress.committed_pages >= progress_before_overflow.committed_pages);
    }
    scheduler
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .policy
        .staleness_threshold = original_staleness_threshold;
    drop(status_poll_admission);
    let last_reconciled_before_overflow = scheduler
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .last_reconciled_at_micros();
    assert!(
        registry.notify_hook_overflow(fixture.path()).await,
        "mounted graph-off worktree accepts the overflow reconcile"
    );

    let query_deadline = std::time::Instant::now() + Duration::from_secs(10);
    let executed = loop {
        match registry
            .execute_query_search(&scope, core_search_request("alpha_0000"))
            .await
        {
            Ok(executed) => break executed,
            Err(error) => {
                assert!(
                    std::time::Instant::now() <= query_deadline,
                    "graph-off text projection never became queryable: {error}"
                );
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        }
    };
    assert!(
        executed.served_stale,
        "an explicit overflow keeps the currently served text generation stale until reconcile settles"
    );
    let overflow_deadline = std::time::Instant::now() + Duration::from_secs(10);
    let dashboard = loop {
        let overflow_settled = {
            let scheduler = scheduler
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            scheduler.pending_hint_count() == Some(0)
                && scheduler.last_reconciled_at_micros() != last_reconciled_before_overflow
        };
        if overflow_settled
            && !registry
                .reconcile_in_progress_for_test(fixture.path())
                .await
            && let Some(freshness) = registry.dashboard_freshness(fixture.path()).await
            && freshness.staleness_state.as_deref() == Some("fresh")
        {
            // A completed owner pass can have another queued wake. Capture the
            // settled public snapshot instead of racing a later status read.
            break freshness;
        }
        assert!(
            std::time::Instant::now() <= overflow_deadline,
            "graph-off overflow did not settle through a real no-op reconcile"
        );
        tokio::time::sleep(Duration::from_millis(2)).await;
    };
    let (owner_epoch_after_overflow, progress_after_overflow, decode_count) = {
        let scheduler = scheduler
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let decode_count = scheduler.sealed_decode_count();
        let progress = scheduler.build_progress_slot();
        let progress = progress
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        (
            progress.owner_epoch,
            progress
                .snapshot()
                .expect("ready text progress remains visible"),
            decode_count,
        )
    };
    assert_eq!(owner_epoch_after_overflow, owner_epoch_before_overflow);
    assert_eq!(
        progress_after_overflow.generation_id,
        progress_before_overflow.generation_id
    );
    assert_eq!(
        progress_after_overflow.daemon_incarnation,
        progress_before_overflow.daemon_incarnation
    );
    assert_eq!(
        progress_after_overflow.producer_incarnation,
        progress_before_overflow.producer_incarnation
    );
    assert!(progress_after_overflow.progress_epoch > progress_before_overflow.progress_epoch);
    assert!(progress_after_overflow.committed_pages >= progress_before_overflow.committed_pages);
    assert_eq!(
        progress_after_overflow.phase,
        tracedecay_contracts::code_index_freshness::CodeIndexBuildPhaseV1::Ready
    );
    assert_eq!(
        decode_count, 0,
        "graph-off text serving must not decode the full generation"
    );
    let artifact_names = std::fs::read_dir(code_text_artifacts_root(&scoped_store))
        .expect("read text artifact root")
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    assert_eq!(
        artifact_names
            .iter()
            .filter(|name| name.starts_with("text-artifact-") && name.ends_with(".bin"))
            .count(),
        1,
        "one durable artifact owns ready text serving"
    );
    assert_eq!(
        artifact_names
            .iter()
            .filter(|name| name.starts_with(".text-artifact-") && name.ends_with(".staging"))
            .count(),
        0,
        "ready text serving leaves no abandoned staging owner"
    );
    assert_eq!(
        executed
            .authorized
            .fallback
            .public_fallback_lane_coverage
            .get(&RetrieverKind::ExactLiteral),
        Some(&PublicRetrieverStatus::Complete)
    );
    assert_eq!(
        executed
            .authorized
            .fallback
            .public_fallback_lane_coverage
            .get(&RetrieverKind::Lexical),
        Some(&PublicRetrieverStatus::Complete)
    );
    assert_eq!(
        executed
            .authorized
            .fallback
            .public_fallback_lane_coverage
            .get(&RetrieverKind::Graph),
        Some(&PublicRetrieverStatus::Unavailable)
    );

    assert_eq!(dashboard.staleness_state.as_deref(), Some("fresh"));
    assert_eq!(dashboard.coverage, "complete");
    assert_eq!(
        dashboard
            .progress
            .as_ref()
            .expect("ready progress stays observable")
            .phase,
        tracedecay_contracts::code_index_freshness::CodeIndexBuildPhaseV1::Ready
    );
    let current = registry
        .execute_query_search(&scope, core_search_request("alpha_0000"))
        .await
        .expect("settled graph-off text query");
    assert!(
        !current.served_stale,
        "a reconciled graph-off text owner must report current exact and lexical coverage"
    );
    let semantic = registry
        .execute_query_with_semantic(
            fixture.path(),
            &scope,
            core_search_request("alpha_0000"),
            Arc::new(ReadySemanticControlV1),
            SemanticQueryModeV1::FallbackAllowed,
        )
        .await
        .expect("graph-off fallback semantic query");
    assert_eq!(semantic.query.generation, current.generation);
    assert!(matches!(
        semantic.semantic,
        super::super::semantic_query_runtime::SemanticAugmentationOutcomeV1::Fallback {
            abstention: SemanticAbstentionV1::CalibrationUnavailable,
            ..
        }
    ));
    let strict = registry
        .execute_query_with_semantic(
            fixture.path(),
            &scope,
            core_search_request("alpha_0000"),
            Arc::new(ReadySemanticControlV1),
            SemanticQueryModeV1::StrictSemantic,
        )
        .await;
    assert!(matches!(
        strict,
        Err(
            super::super::semantic_query_runtime::QuerySemanticSearchExecutionErrorV1::StrictSemanticUnavailable {
                generation,
                abstention: SemanticAbstentionV1::CalibrationUnavailable,
            }
        ) if generation == current.generation
    ));
    assert_eq!(
        scheduler
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .sealed_decode_count(),
        0,
        "semantic fallback without an activated profile must not decode the sealed generation"
    );
    let text = registry
        .latest_text_serving_for_scope(&scope)
        .await
        .expect("ready graph-off text owner");
    let candidate = current
        .authorized
        .fallback
        .ordered_candidates
        .first()
        .expect("artifact-backed ranked candidate");
    let (display, _) = crate::code_index_executor::code_index_text_search_display_binding(
        &text,
        current.sanitized.request(),
        candidate,
    )
    .expect("artifact-backed result display");
    assert_eq!(display.name, "alpha_0000");
    assert_eq!(display.kind, "function");
    assert_eq!(display.path, "src/file_0000.rs");
    assert_eq!(
        scheduler
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .sealed_decode_count(),
        0,
        "artifact-backed display hydration must not decode the sealed generation"
    );

    let query_scope = CodeQueryScope::new(executed.generation.clone(), None).expect("query scope");
    let exact_operation =
        callable_code_operation(CallableCodeOperationKind::ExactOccurrence).expect("operation");
    let exact_context = application_context(
        &exact_operation,
        scope.repository_id.clone(),
        scope.worktree_id.clone(),
    );
    let exact_request =
        ExactOccurrenceRequest::new("alpha_0000", None, query_scope.clone(), query_meta())
            .expect("exact request");
    let exact = registry
        .exact_occurrence(
            RetrievalPortContext {
                request: &exact_context,
                operation: &exact_operation,
            },
            &exact_request,
        )
        .await;
    let exact_page = match exact {
        RetrievalPortOutcome::Completed(evidence) => evidence.payload.expect("exact payload"),
        outcome => panic!("ready graph-off exact owner was unavailable: {outcome:?}"),
    };
    assert_eq!(exact_page.generation, executed.generation);
    assert!(
        exact_page
            .items
            .iter()
            .any(|record| record.occurrence.path == "src/file_0000.rs"),
        "artifact-backed exact hydration returns the canonical source path"
    );

    let phrase_operation =
        callable_code_operation(CallableCodeOperationKind::PhraseSearch).expect("operation");
    let phrase_context = application_context(
        &phrase_operation,
        scope.repository_id.clone(),
        scope.worktree_id.clone(),
    );
    let query = EphemeralSanitizedQueryViewV1::sanitize(
        "alpha_0000",
        SanitizerRevision::new("sanitizer.query.fixture").expect("sanitizer"),
        QueryNormalizationRevision::new("normalization.query.fixture").expect("normalization"),
    )
    .expect("query");
    let phrase_request = PhraseSearchRequest::new(
        query,
        vec!["alpha_0000".to_owned()],
        Vec::new(),
        0,
        query_scope,
        query_meta(),
    )
    .expect("phrase request");
    let phrase = registry
        .phrase_search(
            RetrievalPortContext {
                request: &phrase_context,
                operation: &phrase_operation,
            },
            &phrase_request,
        )
        .await;
    let phrase_page = match phrase {
        RetrievalPortOutcome::Completed(evidence) => evidence.payload.expect("phrase payload"),
        outcome => panic!("ready graph-off phrase owner was unavailable: {outcome:?}"),
    };
    assert_eq!(phrase_page.generation, executed.generation);
    assert!(
        phrase_page
            .items
            .iter()
            .any(|record| record.occurrence.path == "src/file_0000.rs"),
        "artifact-backed lexical hydration returns the canonical source path"
    );
    registry.shutdown().await;
}
