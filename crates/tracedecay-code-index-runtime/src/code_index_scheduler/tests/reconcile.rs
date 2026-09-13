use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write as _,
    num::NonZeroU64,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use tempfile::TempDir;
#[cfg(all(feature = "semantic-fastembed", not(windows)))]
use tracedecay_application::semantic_runtime::{
    ProductionSemanticRuntimeV1, SemanticVectorGraphProviderV1,
};
use tracedecay_contracts::{
    CallableCodeOperationKind, CallableCodeQueryPort, CodeQueryScope, Deadline,
    ExactOccurrenceRequest, ResolvedScope, RetrievalPortContext, RetrievalPortOutcome,
    callable_code_operation,
};
use tracedecay_domain::{
    AuthorizationRevision, CodeGenerationId, CommitId, ComponentRevision,
    EphemeralSanitizedQueryViewV1, ExactClass, FreshnessVectorDigest, FusedCandidate,
    LogicalEvidenceId, ManifestDigest, OptionalStagePublicStatus, PrincipalId, ProjectId,
    PublicRetrieverStatus, QueryNormalizationRevision, RankedCandidate, RefId, RerankPolicy,
    RetrievalAnchorId, RetrievalBudget, RetrievalRequest, RetrievalScope, RetrievalSnapshot,
    RetrieverKind, SanitizerRevision, SensitivityLevelV1, SingleRootScopeV1, TemporalModeV1,
    UtcMicros, VectorWatermark, WorktreeId,
};
#[cfg(all(feature = "semantic-fastembed", not(windows)))]
use tracedecay_graph_db::NeverCancelled;
use tracedecay_query::retrieval::{
    rerank::{AdmittedNativeRerankExecutorV1, BoundedRerankRuntimeV1},
    semantic::apply_bounded_rerank_outcome,
};
#[cfg(all(feature = "semantic-fastembed", not(windows)))]
use tracedecay_runtime_core::db::{Database, DatabaseAuthority, TestDatabaseRuntimeMode};
use tracedecay_runtime_core::resident_memory::{
    DEFAULT_PROCESS_RESIDENT_MEMORY_LIMIT_V1, ProcessResidentMemoryV1,
    sampled_process_resident_bytes_v1,
};
use tracedecay_semantic_contracts::RerankCompatibilityPinsV1;
#[cfg(all(feature = "semantic-fastembed", not(windows)))]
use tracedecay_semantic_contracts::{DEFAULT_FASTEMBED_MODEL_ID, SemanticResourceCeilings};

#[cfg(all(feature = "semantic-fastembed", not(windows)))]
use super::IsolatedSemanticVectorGraphProviderV1;
use super::{
    ALPHA_LIB_V1, CancelledRerankControlV1, GitFixture, MixedAnchorReverseRerankExecutorV1,
    RETAINED_REVISION_0, ReadyRerankControlV1, SERVING_SEAT_FAILURE_CEILING,
    advance_pointer_to_unseated_successor, application_context, committed_capture_corpus_files,
    core_search_request, git, git_stdout, mounted_core_query_worktree,
    mounted_core_query_worktree_with_one_permit, published, query_authority, query_meta,
    quiesced_background_reconcile_admission, replace_scheduler_chunker_revision,
    replace_scheduler_policy_revision, rewrite_active_rust_extractor_revision,
    rewrite_preserving_stat, scheduler, scheduler_with_policy, served_lexical_texts,
    test_project_id, wait_for_dashboard_ready, wait_for_event_to_ready, wait_for_generation_change,
    wait_for_initial_generation, wait_for_live_complete_generation,
    wait_for_live_complete_generation_by_polling, wait_for_queryable_text_generation,
    wait_for_queryable_text_generation_change, wait_for_queryable_text_generation_id,
    wait_for_quiescent_owner_pass, wait_until_serving_seat, write,
};
#[cfg(all(feature = "semantic-fastembed", not(windows)))]
use crate::semantic_code::{
    CatalogedFastEmbedModelV1, DaemonSemanticRuntimeHandleV1, FastEmbedModelCatalogV1,
    ModelLifecycleErrorV1, ModelMemberSourceV1, SemanticModelLifecycleOwnerV1,
    production_fastembed_catalog,
};
use crate::{
    code_index::{
        chunks::content_digest,
        production::{
            CodeIndexExecutionControlV1, DAEMON_CODE_INDEX_CHUNKER_REVISION,
            UninterruptibleCodeIndexControlV1,
        },
    },
    code_index_scheduler::{
        CodeIndexCadenceOutcomeV1, CodeIndexCadenceTriggerV1, CodeIndexHintPolicyV1,
        CodeIndexIgnoredDependencyRequestV1, CodeIndexReconcileOutcomeV1,
        CodeIndexSchedulerRegistryV1, CodeIndexWorktreeSchedulerV1, GenerationDecodeAdmissionV1,
        SharedCodeIndexBytePoolV1,
        classification::{WorktreeChangeClassV1, WorktreeChangeClassificationV1},
        feedback_document_identity_from_generation,
        freshness_witness::RestoreFreshnessWitnessV1,
        registry::{
            ColdMountOpenEventV1, ServingGenerationInstallationOutcomeV1,
            ServingGenerationRollbackOutcomeV1, dashboard_code_graph_serving,
        },
    },
    semantic_code::rerank_adapter::{
        GenerationBoundCodeRerankViewsV1, ProductionCodeRerankAuthorityV1,
    },
};

#[test]
fn one_file_increment_captures_only_edited_bytes_with_one_thousand_unchanged_files() {
    let mut owned_sources = (0..1_000)
        .map(|index| {
            (
                format!("src/unchanged_{index:04}.rs"),
                format!("pub fn unchanged_{index:04}() -> usize {{ {index} }}\n"),
            )
        })
        .collect::<Vec<_>>();
    owned_sources.push((
        "src/edited.rs".to_owned(),
        "pub fn edited() -> usize { 1 }\n".to_owned(),
    ));
    let borrowed_sources = owned_sources
        .iter()
        .map(|(path, source)| (path.as_str(), source.as_str()))
        .collect::<Vec<_>>();
    let fixture = GitFixture::new(&borrowed_sources);
    let store = TempDir::new().expect("store root");
    let mut scheduler = scheduler(
        &fixture,
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    );
    published(
        scheduler
            .reconcile_now()
            .expect("publish initial large generation"),
    );

    let edited_source = "pub fn edited() -> usize { 2 }\n";
    fixture.edit("src/edited.rs", edited_source);
    scheduler.notify_hook_paths([PathBuf::from("src/edited.rs")]);

    let captured = scheduler
        .capture_authoritative_snapshot(None)
        .expect("capture one-file increment");
    assert_eq!(
        captured.snapshot.files.len(),
        1_001,
        "the complete snapshot must retain every unchanged row"
    );
    assert_eq!(
        captured.captured_files.len(),
        1,
        "only the classified changed file may be read and sanitized"
    );
    assert_eq!(
        captured
            .captured_files
            .iter()
            .map(|file| file.sanitized_bytes.len())
            .sum::<usize>(),
        edited_source.len(),
        "captured bytes must be proportional to the one edited file"
    );
    let full_capture = scheduler
        .capture_authoritative_snapshot_without_active_generation_reuse(None)
        .expect("capture full comparison snapshot");
    assert_eq!(
        captured.snapshot.content_identity, full_capture.snapshot.content_identity,
        "active-row reuse must preserve the full capture's byte-exact snapshot identity"
    );
    let untouched_row = captured
        .snapshot
        .files
        .iter()
        .find(|file| file.logical_path == "src/unchanged_0001.rs")
        .expect("untouched snapshot row")
        .clone();

    published(
        scheduler
            .reconcile_now()
            .expect("publish first dirty generation"),
    );
    fixture.edit(
        "src/unchanged_0000.rs",
        "pub fn unchanged_0000() -> usize { 10_000 }\n",
    );
    scheduler.notify_hook_paths([PathBuf::from("src/unchanged_0000.rs")]);

    let second = scheduler
        .capture_authoritative_snapshot(None)
        .expect("capture second consecutive edit");
    assert_eq!(
        second.snapshot.files.len(),
        1_001,
        "the second complete snapshot must retain every file row"
    );
    assert_eq!(
        second.captured_files.len(),
        2,
        "the previously dirty file and newly edited file must both be captured"
    );
    assert_eq!(
        second
            .snapshot
            .files
            .iter()
            .find(|file| file.logical_path == "src/unchanged_0001.rs"),
        Some(&untouched_row),
        "an untouched file row must still be reused from the dirty active generation"
    );
}

#[test]
fn committed_one_file_edit_reuses_unchanged_rows_across_the_new_head_tree() {
    let unchanged_files = committed_capture_corpus_files();
    let mut owned_sources = (0..unchanged_files)
        .map(|index| {
            (
                format!("src/unchanged_{index:04}.rs"),
                format!("pub fn unchanged_{index:04}() -> usize {{ {index} }}\n"),
            )
        })
        .collect::<Vec<_>>();
    owned_sources.push((
        "src/edited.rs".to_owned(),
        "pub fn committed_edit() -> usize { 1 }\n".to_owned(),
    ));
    let borrowed_sources = owned_sources
        .iter()
        .map(|(path, source)| (path.as_str(), source.as_str()))
        .collect::<Vec<_>>();
    let fixture = GitFixture::new(&borrowed_sources);
    let store = TempDir::new().expect("store root");
    let mut scheduler = scheduler(
        &fixture,
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    );
    published(
        scheduler
            .reconcile_now()
            .expect("publish initial large generation"),
    );

    fixture.edit("src/edited.rs", "pub fn committed_edit() -> usize { 2 }\n");
    fixture.commit_all("commit one-file edit");
    scheduler.identity = super::super::identity::IndexingIdentityV1::resolve(fixture.path())
        .expect("refresh HEAD identity");

    let capture_started = Instant::now();
    let captured = scheduler
        .capture_authoritative_snapshot(None)
        .expect("capture committed one-file delta");
    let capture_elapsed = capture_started.elapsed();
    assert_eq!(captured.captured_files.len(), 1);
    assert_eq!(captured.snapshot.files.len(), unchanged_files + 1);
    assert!(captured.changed_paths.contains("src/edited.rs"));
    let captured_bytes = captured
        .captured_files
        .iter()
        .map(|file| file.sanitized_bytes.len())
        .sum::<usize>();
    println!(
        "committed one-file edit over {unchanged_files} unchanged files: captured_files=1 \
         captured_bytes={captured_bytes} capture_ms={}",
        capture_elapsed.as_millis()
    );
    let full_started = Instant::now();
    let full = scheduler
        .capture_authoritative_snapshot_without_active_generation_reuse(None)
        .expect("capture full comparison snapshot");
    println!(
        "full capture over {unchanged_files} unchanged files: captured_files={} \
         captured_bytes={} capture_ms={}",
        full.captured_files.len(),
        full.captured_files
            .iter()
            .map(|file| file.sanitized_bytes.len())
            .sum::<usize>(),
        full_started.elapsed().as_millis()
    );
    assert_eq!(
        captured.snapshot.content_identity, full.snapshot.content_identity,
        "clean-tip reuse must preserve the full capture identity"
    );

    published(
        scheduler
            .reconcile_now()
            .expect("publish committed one-file delta"),
    );
    let chunks = scheduler
        .latest_complete()
        .expect("committed generation")
        .lexical()
        .iter()
        .filter(|chunk| chunk.sanitized_text.as_str().contains("fn committed_edit"))
        .map(|chunk| chunk.sanitized_text.as_str().to_owned())
        .collect::<Vec<_>>();
    assert!(!chunks.is_empty());
    assert!(chunks.iter().all(|text| text.contains("{ 2 }")));
}

#[test]
fn committed_revert_recaptures_the_reverted_file() {
    let committed = "pub fn committed_revert() -> u32 { 1 }\n";
    let fixture = GitFixture::new(&[
        ("src/lib.rs", committed),
        ("src/other.rs", "pub fn other() -> u32 { 2 }\n"),
    ]);
    let store = TempDir::new().expect("store root");
    let mut scheduler = scheduler(
        &fixture,
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    );
    published(
        scheduler
            .reconcile_now()
            .expect("publish initial generation"),
    );
    let original_digest = scheduler
        .latest_complete()
        .expect("initial generation")
        .generation
        .snapshot()
        .files
        .iter()
        .find(|file| file.logical_path == "src/lib.rs")
        .expect("initial lib row")
        .content_digest
        .clone();

    fixture.edit("src/lib.rs", "pub fn committed_revert() -> u32 { 99 }\n");
    fixture.commit_all("commit edit");
    published(scheduler.reconcile_now().expect("publish committed edit"));

    fixture.edit("src/lib.rs", committed);
    fixture.commit_all("commit revert");
    scheduler.identity = super::super::identity::IndexingIdentityV1::resolve(fixture.path())
        .expect("refresh HEAD identity");
    let captured = scheduler
        .capture_authoritative_snapshot(None)
        .expect("capture committed revert");
    assert_eq!(captured.captured_files.len(), 1);
    assert!(captured.changed_paths.contains("src/lib.rs"));
    assert_eq!(
        captured
            .snapshot
            .files
            .iter()
            .find(|file| file.logical_path == "src/lib.rs")
            .expect("reverted lib row")
            .content_digest,
        original_digest
    );

    published(scheduler.reconcile_now().expect("publish committed revert"));
    let chunks = scheduler
        .latest_complete()
        .expect("reverted generation")
        .lexical()
        .iter()
        .filter(|chunk| {
            chunk
                .sanitized_text
                .as_str()
                .contains("fn committed_revert")
        })
        .map(|chunk| chunk.sanitized_text.as_str().to_owned())
        .collect::<Vec<_>>();
    assert!(!chunks.is_empty());
    assert!(chunks.iter().all(|text| text.contains("{ 1 }")));
    assert!(chunks.iter().all(|text| !text.contains("{ 99 }")));
}

#[test]
fn committed_deletion_drops_the_row_and_committed_addition_captures_it() {
    let owned_sources = (0..1_001)
        .map(|index| {
            (
                format!("src/original_{index:04}.rs"),
                format!("pub fn original_{index:04}() -> usize {{ {index} }}\n"),
            )
        })
        .collect::<Vec<_>>();
    let borrowed_sources = owned_sources
        .iter()
        .map(|(path, source)| (path.as_str(), source.as_str()))
        .collect::<Vec<_>>();
    let fixture = GitFixture::new(&borrowed_sources);
    let store = TempDir::new().expect("store root");
    let mut scheduler = scheduler(
        &fixture,
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    );
    published(
        scheduler
            .reconcile_now()
            .expect("publish initial large generation"),
    );

    fixture.remove("src/original_0000.rs");
    fixture.edit(
        "src/added.rs",
        "pub fn committed_addition() -> usize { 7 }\n",
    );
    fixture.commit_all("replace one committed file");
    scheduler.identity = super::super::identity::IndexingIdentityV1::resolve(fixture.path())
        .expect("refresh HEAD identity");
    let captured = scheduler
        .capture_authoritative_snapshot(None)
        .expect("capture committed deletion and addition");

    assert_eq!(captured.snapshot.files.len(), 1_001);
    assert_eq!(captured.captured_files.len(), 1);
    assert!(captured.changed_paths.contains("src/original_0000.rs"));
    assert!(captured.changed_paths.contains("src/added.rs"));
    assert!(
        captured
            .snapshot
            .files
            .iter()
            .any(|file| file.logical_path == "src/added.rs")
    );
    assert!(
        captured
            .snapshot
            .files
            .iter()
            .all(|file| file.logical_path != "src/original_0000.rs")
    );
    let full = scheduler
        .capture_authoritative_snapshot_without_active_generation_reuse(None)
        .expect("capture full comparison snapshot");
    assert_eq!(
        captured.snapshot.content_identity,
        full.snapshot.content_identity
    );
}

#[test]
fn unresolvable_active_tree_falls_back_to_full_capture() {
    let fixture = GitFixture::new(&[
        ("src/lib.rs", "pub fn old_tree() -> u32 { 1 }\n"),
        ("src/other.rs", "pub fn other() -> u32 { 2 }\n"),
    ]);
    let store = TempDir::new().expect("store root");
    let mut scheduler = scheduler(
        &fixture,
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    );
    published(
        scheduler
            .reconcile_now()
            .expect("publish initial generation"),
    );
    let active_tree = scheduler
        .latest_complete()
        .expect("initial generation")
        .generation
        .repository_parse_identity()
        .tree
        .as_ref()
        .expect("active tree")
        .as_str()
        .to_owned();

    fixture.edit("src/lib.rs", "pub fn old_tree() -> u32 { 3 }\n");
    fixture.commit_all("move HEAD tree");
    let object_path = fixture
        .path()
        .join(".git")
        .join("objects")
        .join(&active_tree[..2])
        .join(&active_tree[2..]);
    assert!(object_path.is_file(), "active tree must be a loose object");
    std::fs::remove_file(object_path).expect("remove active tree object");
    scheduler.identity = super::super::identity::IndexingIdentityV1::resolve(fixture.path())
        .expect("refresh HEAD identity");

    let captured = scheduler
        .capture_authoritative_snapshot(None)
        .expect("fall back to full capture");
    assert_eq!(
        captured.captured_files.len(),
        2,
        "an unresolvable active tree must disable row reuse"
    );
    let full = scheduler
        .capture_authoritative_snapshot_without_active_generation_reuse(None)
        .expect("capture full comparison snapshot");
    assert_eq!(
        captured.snapshot.content_identity,
        full.snapshot.content_identity
    );
}

#[test]
fn reverted_dirty_file_is_recaptured_from_clean_content() {
    let committed = "pub fn alpha() -> u32 { 1 }\n";
    let fixture = GitFixture::new(&[
        ("src/lib.rs", committed),
        ("src/other.rs", "pub fn other() -> u32 { 2 }\n"),
    ]);
    let store = TempDir::new().expect("store root");
    let mut scheduler = scheduler(
        &fixture,
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    );
    let clean_row = |captured: &super::super::CapturedSnapshotV1| {
        captured
            .snapshot
            .files
            .iter()
            .find(|file| file.logical_path == "src/lib.rs")
            .expect("lib.rs snapshot row")
            .content_digest
            .clone()
    };
    let clean_digest = clean_row(
        &scheduler
            .capture_authoritative_snapshot(None)
            .expect("capture clean tree"),
    );

    fixture.edit("src/lib.rs", "pub fn alpha() -> u32 { 99 }\n");
    scheduler.notify_hook_paths([PathBuf::from("src/lib.rs")]);
    published(scheduler.reconcile_now().expect("publish dirty generation"));

    // Revert to the committed bytes: the path is git-clean again, but the
    // active generation still carries the dirty row.
    fixture.edit("src/lib.rs", committed);
    scheduler.notify_hook_paths([PathBuf::from("src/lib.rs")]);
    let captured = scheduler
        .capture_authoritative_snapshot(None)
        .expect("capture reverted tree");
    assert_eq!(
        clean_row(&captured),
        clean_digest,
        "a file reverted to its committed content must be recaptured, not carried from the dirty active generation"
    );
    assert!(
        captured
            .captured_files
            .iter()
            .any(|file| file.sanitized_bytes.as_ref() == committed.as_bytes()),
        "the reverted file must be re-read from disk"
    );

    // The served index must reflect the revert: the dirty body is gone and
    // the committed body is back in the lexical chunks.
    published(
        scheduler
            .reconcile_now()
            .expect("publish reverted generation"),
    );
    let latest = scheduler.latest_complete().expect("reverted generation");
    let lib_chunks = latest
        .lexical()
        .iter()
        .filter(|chunk| chunk.sanitized_text.as_str().contains("fn alpha"))
        .map(|chunk| chunk.sanitized_text.as_str().to_owned())
        .collect::<Vec<_>>();
    assert!(
        !lib_chunks.is_empty(),
        "the reverted file must still be indexed"
    );
    assert!(
        lib_chunks.iter().all(|text| text.contains("{ 1 }")),
        "queries must serve the committed body after the revert, got {lib_chunks:?}"
    );
    assert!(
        lib_chunks.iter().all(|text| !text.contains("{ 99 }")),
        "queries must not serve the reverted dirty body, got {lib_chunks:?}"
    );
}

#[test]
fn capture_sanitizes_code_and_propagates_scan_evidence() {
    let secret = ["sk", "-test-", "1234567890abcdef"].concat();
    let source = format!("pub const TOKEN: &str = \"{secret}\";\n");
    let fixture = GitFixture::new(&[("src/lib.rs", &source)]);
    let store = TempDir::new().expect("store root");
    let mut first_scheduler = scheduler(
        &fixture,
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    );

    published(
        first_scheduler
            .reconcile_now()
            .expect("publish sanitized code"),
    );
    let latest = first_scheduler
        .latest_complete()
        .expect("latest generation");
    let snapshot = latest.generation.snapshot();

    assert_eq!(
        snapshot.sanitizer_revision.as_str(),
        tracedecay_privacy::CODE_SOURCE_SANITIZER_VERSION_V1
    );
    assert!(
        snapshot
            .sanitization_receipts
            .iter()
            .all(|receipt| { receipt.as_str().starts_with("privacy.code-source.v1.") })
    );
    assert!(
        latest
            .generation
            .chunks()
            .chunks()
            .iter()
            .all(|chunk| { !chunk.sanitized_text.as_str().contains(&secret) })
    );
    assert!(
        latest
            .generation
            .chunks()
            .chunks()
            .iter()
            .any(|chunk| chunk.sensitivity.level == SensitivityLevelV1::Redacted)
    );

    drop(latest);
    drop(first_scheduler);
    let restarted = scheduler(
        &fixture,
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    );
    let restored = restarted
        .latest_complete()
        .expect("restart restores sanitized generation");
    assert!(
        restored
            .generation
            .chunks()
            .chunks()
            .iter()
            .all(|chunk| !chunk.sanitized_text.as_str().contains(&secret))
    );
    assert!(
        restored
            .generation
            .snapshot()
            .sanitization_receipts
            .iter()
            .all(|receipt| receipt.as_str().starts_with("privacy.code-source.v1."))
    );
}

#[tokio::test]
async fn registry_feeds_publications_and_bounded_freshness_reads() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn alpha() -> u32 { 1 }\n")]);
    let store = TempDir::new().expect("store root");
    let registry = CodeIndexSchedulerRegistryV1::new(1);
    let mut publications = registry.subscribe_generation_publications();

    registry
        .mount_worktree(
            test_project_id(),
            fixture.path(),
            store.path().to_path_buf(),
            None,
        )
        .await
        .expect("mount worktree");
    let initial = tokio::time::timeout(Duration::from_secs(2), publications.recv())
        .await
        .expect("initial publication timeout")
        .expect("initial publication event");
    assert_eq!(
        initial.project_root,
        fixture.path().canonicalize().expect("canonical fixture")
    );
    // Publication is not the seated dashboard identity. Wait for the seat
    // before asserting the projected generation id.
    wait_for_live_complete_generation(&registry, fixture.path()).await;
    wait_for_dashboard_ready(&registry, fixture.path()).await;

    let freshness = registry
        .dashboard_freshness(fixture.path())
        .await
        .expect("dashboard freshness");
    assert_eq!(
        freshness.latest_generation_id.as_deref(),
        Some(initial.generation_id.as_str())
    );
    assert!(freshness.last_reconcile_micros.is_some());
    assert_eq!(freshness.staleness_state.as_deref(), Some("fresh"));
    assert_eq!(freshness.hook_hint_count, Some(0));

    fixture.edit("src/lib.rs", "pub fn alpha() -> u32 { 2 }\n");
    assert!(
        registry
            .notify_hook_paths(fixture.path(), &["src/lib.rs".to_owned()])
            .await
    );
    let changed = tokio::time::timeout(Duration::from_secs(2), publications.recv())
        .await
        .expect("changed publication timeout")
        .expect("changed publication event");
    assert_ne!(changed.generation_id, initial.generation_id);
}

/// The generation-publication broadcast carries only verified publishes —
/// generations that crossed the durable publication compare-and-swap, the
/// verified graph snapshot publish, and the serving swap. A restart that
/// restores a retained generation is a `Noop` apply and must reach the
/// serving slot silently: the post-mount query-authority waiter re-reads the
/// serving slot for restores and trusts this bus only for verified publishes.
#[tokio::test]
async fn restart_remount_serves_the_retained_generation_without_republishing() {
    let fixture = GitFixture::new(ALPHA_LIB_V1);
    let store = TempDir::new().expect("store root");
    let first = CodeIndexSchedulerRegistryV1::new(1);
    first
        .mount_worktree(
            test_project_id(),
            fixture.path(),
            store.path().to_path_buf(),
            None,
        )
        .await
        .expect("mount worktree");
    let sealed = wait_for_initial_generation(&first, fixture.path()).await;
    first.shutdown().await;

    let restarted = CodeIndexSchedulerRegistryV1::new(1);
    // Subscribed before the remount. The per-worktree worker is serial, so a
    // broadcast wrongly emitted by the restore-era passes would sit in this
    // receiver ahead of the edit-triggered publication received below.
    let mut publications = restarted.subscribe_generation_publications();
    restarted
        .mount_worktree(
            test_project_id(),
            fixture.path(),
            store.path().to_path_buf(),
            None,
        )
        .await
        .expect("remount worktree over the retained store");
    let restored = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(generation) = restarted.latest_generation_id(fixture.path()).await {
                break generation;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("restart serves a generation");
    assert_eq!(
        restored, sealed,
        "the restart serves the retained generation, not a rebuilt one"
    );

    fixture.edit("src/lib.rs", "pub fn alpha() -> u32 { 2 }\n");
    assert!(
        restarted
            .notify_hook_paths(fixture.path(), &["src/lib.rs".to_owned()])
            .await
    );
    let first_broadcast = tokio::time::timeout(Duration::from_secs(5), publications.recv())
        .await
        .expect("post-restart publication timeout")
        .expect("post-restart publication event");
    assert_ne!(
        first_broadcast.generation_id, sealed,
        "the retained restore stays silent; the first broadcast is the rebuilt generation"
    );
    restarted.shutdown().await;
}

#[test]
fn retained_v3_rust_extractor_generation_is_refused_and_rebuilt_by_v5() {
    let fixture = GitFixture::new(ALPHA_LIB_V1);
    let store = TempDir::new().expect("store root");
    let mut seed = scheduler(
        &fixture,
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    );
    let stale = published(seed.reconcile_now().expect("publish retained generation"));
    drop(seed);

    rewrite_active_rust_extractor_revision(store.path(), "extractor.rust.v3");
    let mut restarted = scheduler(
        &fixture,
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    );
    assert!(
        restarted.servable_retained_text_generation().is_none(),
        "a retained v3 Rust extraction must not enter a v5 serving slot"
    );

    let rebuilt = published(
        restarted
            .reconcile_now()
            .expect("rebuild generation under the current extractor"),
    );
    assert_ne!(rebuilt.generation_id, stale.generation_id);
    assert_eq!(
        restarted
            .latest_complete()
            .expect("rebuilt generation")
            .generation
            .manifest()
            .extractor_revisions
            .iter()
            .find(|(language, _)| language.as_str() == "rust")
            .map(|(_, revision)| revision.as_str()),
        Some("extractor.rust.v5")
    );
}

/// A restart over a dirty checkout must seat the retained complete generation
/// before the successor rebuild finishes. Waiting for that rebuild left remount
/// serving empty (`last_reconcile_micros` unset, no seated publication) while
/// a sealed artifact was already on disk.
#[tokio::test]
async fn restart_remount_seats_the_retained_generation_before_a_dirty_rebuild() {
    let fixture = GitFixture::new(ALPHA_LIB_V1);
    let store = TempDir::new().expect("store root");
    let first = CodeIndexSchedulerRegistryV1::new(1);
    first
        .mount_worktree(
            test_project_id(),
            fixture.path(),
            store.path().to_path_buf(),
            None,
        )
        .await
        .expect("mount worktree");
    let sealed = wait_for_live_complete_generation(&first, fixture.path()).await;
    let sealed_id = sealed.generation().manifest().generation_id.clone();
    first.shutdown().await;

    fixture.edit("src/lib.rs", "pub fn alpha() -> u32 { 2 }\n");

    let restarted = CodeIndexSchedulerRegistryV1::new(1);
    restarted
        .mount_worktree(
            test_project_id(),
            fixture.path(),
            store.path().to_path_buf(),
            None,
        )
        .await
        .expect("remount worktree over a dirty checkout");
    let seated = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(latest) = restarted
                .latest_complete_serving_for_test(fixture.path())
                .await
            {
                break latest;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("dirty remount must seat the retained generation before the successor rebuild");
    assert_eq!(
        seated.generation().manifest().generation_id,
        sealed_id,
        "the empty serving slot takes the retained generation, not the in-flight rebuild"
    );

    let rebuilt = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(latest) = restarted
                .latest_complete_serving_for_test(fixture.path())
                .await
                && latest.generation().manifest().generation_id != sealed_id
            {
                break latest.generation().manifest().generation_id.clone();
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("dirty remount must still publish the successor generation");
    assert_ne!(rebuilt, sealed_id, "the dirty checkout must rebuild");
    restarted.shutdown().await;
}

/// Dirty remount seating must not park on the publication decode barrier
/// while holding the scheduler lock. Activation may already own that cache;
/// joining it left remount warming with no seated generation.
#[tokio::test]
async fn dirty_retained_seat_does_not_join_the_publication_decode_cache() {
    let fixture = GitFixture::new(ALPHA_LIB_V1);
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
    wait_for_live_complete_generation(&registry, fixture.path()).await;

    fixture.edit("src/lib.rs", "pub fn alpha() -> u32 { 2 }\n");
    let scheduler = registry
        .scheduler_handle(fixture.path())
        .await
        .expect("scheduler handle");
    let held_decode = scheduler
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .hold_active_decode();
    let seating = scheduler.clone();
    let outcome = tokio::time::timeout(
        Duration::from_secs(1),
        tokio::task::spawn_blocking(move || {
            seating
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .seat_retained_generation_on_empty_serving_for_test()
        }),
    )
    .await
    .expect("dirty retained seat must not wait for the decode cache")
    .expect("seat task")
    .expect("seat result");
    assert!(
        matches!(outcome, Some(CodeIndexReconcileOutcomeV1::Noop(_))),
        "dirty remount must still emit a retained-seat Noop without decoding"
    );
    assert_eq!(
        held_decode.waiter_count(),
        0,
        "retained seating must not join the publication decode flight"
    );

    drop(held_decode);
    registry.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scheduler_notifications_remain_nonblocking_while_reconcile_is_busy() {
    let fixture = GitFixture::new(ALPHA_LIB_V1);
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
    wait_for_initial_generation(&registry, fixture.path()).await;

    let scheduler = registry
        .scheduler_handle(fixture.path())
        .await
        .expect("scheduler handle");
    let (locked_tx, locked_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let blocker = std::thread::spawn(move || {
        let _scheduler = scheduler
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        locked_tx.send(()).expect("announce scheduler lock");
        release_rx.recv().expect("release scheduler lock");
    });
    locked_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("scheduler lock acquisition");

    let notification_registry = registry.clone();
    let project_root = fixture.path().to_path_buf();
    let notification = tokio::spawn(async move {
        notification_registry
            .notify_hook_paths(&project_root, &["src/lib.rs".to_owned()])
            .await
    });
    let notified = tokio::time::timeout(Duration::from_millis(100), notification)
        .await
        .expect("scheduler notification must not wait for the reconcile lock")
        .expect("notification task");
    assert!(notified);
    assert!(
        registry
            .latest_complete_ready(fixture.path())
            .await
            .is_none(),
        "a hook epoch invalidates the memoized freshness proof before reconcile"
    );

    let registry_read = tokio::time::timeout(
        Duration::from_millis(100),
        registry.scheduler_handle(fixture.path()),
    )
    .await;
    assert!(
        registry_read.is_ok(),
        "scheduler notification must not retain the mounted-worktree registry lock"
    );

    release_tx.send(()).expect("release scheduler");
    blocker.join().expect("scheduler blocker");
    registry.shutdown().await;
}

/// Full pre-seat memory journey: a fresh scheduler decodes the retained active
/// generation, publishes a one-file increment through the partitioned sealer,
/// and drains that successor into the durable text artifact. The default is
/// intentionally substantial and can be raised to the preserved operator
/// shape without changing the exercised production path.
#[test]
#[ignore = "scale memory regression; run explicitly with one exact test process"]
fn retained_decode_incremental_seal_and_text_publish_stay_below_the_high_watermark() {
    let file_count = std::env::var("TRACEDECAY_PRESEAT_SCALE_FILES").map_or(2_600, |value| {
        value.parse::<usize>().expect("positive scale file count")
    });
    let functions_per_file =
        std::env::var("TRACEDECAY_PRESEAT_FUNCTIONS_PER_FILE").map_or(64, |value| {
            value
                .parse::<usize>()
                .expect("positive functions-per-file count")
        });
    assert!(file_count > 1);
    assert!(functions_per_file > 0);

    let owned_sources = (0..file_count)
        .map(|file| {
            let mut source = String::new();
            for function in 0..functions_per_file {
                writeln!(
                    source,
                    "pub fn scale_{file:04}_{function:04}() -> usize {{ {} }}",
                    file + function
                )
                .expect("write scale source");
            }
            (format!("src/scale_{file:04}.rs"), source)
        })
        .collect::<Vec<_>>();
    let source_bytes = owned_sources
        .iter()
        .map(|(_, source)| source.len())
        .sum::<usize>();
    let sources = owned_sources
        .iter()
        .map(|(path, source)| (path.as_str(), source.as_str()))
        .collect::<Vec<_>>();
    let fixture = GitFixture::new(&sources);
    let store = TempDir::new().expect("store root");

    {
        let mut seed = scheduler(
            &fixture,
            store.path().to_path_buf(),
            Arc::new(SharedCodeIndexBytePoolV1::default()),
        );
        published(seed.reconcile_now().expect("seed retained generation"));
    }

    #[cfg(feature = "hotpath-alloc")]
    let hotpath_output = std::env::temp_dir().join(format!(
        "tracedecay-preseat-scale-{}.json",
        std::process::id()
    ));
    #[cfg(feature = "hotpath-alloc")]
    let hotpath_guard = hotpath::HotpathGuardBuilder::new("preseat-code-index-scale")
        .format(hotpath::Format::Json)
        .output_path(hotpath_output.clone())
        .build();
    #[cfg(target_os = "linux")]
    std::fs::write("/proc/self/clear_refs", b"5\n").expect("reset process peak RSS");
    let baseline_rss_bytes = sampled_process_resident_bytes_v1();

    let mut scheduler = scheduler(
        &fixture,
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    );
    assert_eq!(scheduler.publication.sealed_decode_count(), 0);
    assert!(matches!(
        scheduler
            .reconcile_now()
            .expect("restore retained generation"),
        CodeIndexReconcileOutcomeV1::Noop(_)
    ));
    assert_eq!(
        scheduler.publication.sealed_decode_count(),
        1,
        "fresh activation must decode the retained generation exactly once"
    );

    let mut edited = String::new();
    for function in 0..functions_per_file {
        writeln!(
            edited,
            "pub fn scale_{:04}_{function:04}() -> usize {{ {} }}",
            file_count - 1,
            file_count + function
        )
        .expect("write edited scale source");
    }
    let edited_path = format!("src/scale_{:04}.rs", file_count - 1);
    fixture.edit(&edited_path, &edited);
    scheduler.notify_path(fixture.path().join(&edited_path));
    let increment = published(
        scheduler
            .reconcile_now()
            .expect("publish one-file incremental generation"),
    );
    assert_eq!(increment.reextracted_files, 1);
    assert!(
        scheduler
            .publication
            .seal_encoded_segment_bytes
            .load(std::sync::atomic::Ordering::Relaxed)
            > 0
    );
    assert_eq!(
        scheduler
            .publication
            .seal_existing_segment_bytes_read
            .load(std::sync::atomic::Ordering::Relaxed),
        0,
        "unchanged parent file segments must stay content-address reused"
    );

    let latest = scheduler.latest_complete().expect("incremental generation");
    while !latest
        .advance_text_serving(super::super::TEXT_ARTIFACT_MAXIMUM_WORK_PER_ADVANCE_V1)
        .expect("drain successor through durable text publication")
    {}
    assert!(latest.query_owners_are_warm());
    let settled_rss_bytes = sampled_process_resident_bytes_v1();
    #[cfg(target_os = "linux")]
    let peak_rss_bytes = {
        let status = std::fs::read_to_string("/proc/self/status").expect("read process status");
        status
            .lines()
            .find_map(|line| line.strip_prefix("VmHWM:"))
            .and_then(|value| value.split_whitespace().next())
            .and_then(|value| value.parse::<u64>().ok())
            .and_then(|kib| kib.checked_mul(1_024))
            .expect("process peak RSS")
    };
    #[cfg(not(target_os = "linux"))]
    let peak_rss_bytes = settled_rss_bytes.unwrap_or(0);
    let existing_high_watermark_bytes = 18_u64
        .saturating_mul(1024 * 1024 * 1024)
        .saturating_add(84_u64.saturating_mul(1024 * 1024 * 1024) / 100);
    println!(
        "{}",
        serde_json::json!({
            "files": file_count,
            "functions_per_file": functions_per_file,
            "source_bytes": source_bytes,
            "active_decode_count": scheduler.publication.sealed_decode_count(),
            "baseline_rss_bytes": baseline_rss_bytes,
            "peak_rss_bytes": peak_rss_bytes,
            "settled_rss_bytes": settled_rss_bytes,
            "existing_high_watermark_bytes": existing_high_watermark_bytes,
        })
    );
    assert!(
        peak_rss_bytes < existing_high_watermark_bytes,
        "pre-seat pipeline peak {peak_rss_bytes} exceeded the existing {existing_high_watermark_bytes}-byte high watermark"
    );
    #[cfg(feature = "hotpath-alloc")]
    {
        drop(hotpath_guard);
        println!("hotpath_output={}", hotpath_output.display());
    }
}

/// Committing content the dirty index already serves re-seals for provenance
/// and recaptures the HEAD-tree delta, while byte identity still proves that
/// every chunk can be reused.
#[test]
fn provenance_only_reseal_recaptures_the_tree_delta_without_changing_chunks() {
    let fixture = GitFixture::new(&[
        ("src/lib.rs", "pub fn alpha() -> u32 { 1 }\n"),
        ("src/other.rs", "pub fn gamma() -> u32 { 3 }\n"),
    ]);
    let store = TempDir::new().expect("store root");
    let bytes = Arc::new(SharedCodeIndexBytePoolV1::default());
    let mut scheduler = scheduler(&fixture, store.path().to_path_buf(), bytes);
    published(scheduler.reconcile_now().expect("initial publish"));

    fixture.edit("src/lib.rs", "pub fn alpha() -> u32 { 2 }\n");
    scheduler.notify_path(fixture.path().join("src/lib.rs"));
    let dirty = published(
        scheduler
            .reconcile_now()
            .expect("dirty incremental publish"),
    );
    assert!(
        scheduler
            .latest_complete()
            .expect("dirty generation serves")
            .generation()
            .snapshot()
            .source_revision
            .is_none(),
        "a dirty capture seals without an exact source revision"
    );

    git(fixture.path(), &["add", "."]);
    git(fixture.path(), &["commit", "-qm", "commit indexed content"]);
    let resealed = published(
        scheduler
            .reconcile_now()
            .expect("provenance-only reseal publishes"),
    );
    assert_ne!(dirty.generation_id, resealed.generation_id);
    assert_eq!(
        resealed.snapshot_content_identity, dirty.snapshot_content_identity,
        "committing indexed content must not change the content identity"
    );
    assert_eq!(
        resealed.reextracted_files, 1,
        "the moved HEAD tree must declare its one changed path for recapture"
    );
    assert_eq!(
        resealed.changed_chunks, 0,
        "a provenance-only reseal produces no added, changed, or deleted chunks"
    );
    assert!(
        resealed.reused_chunks > 0,
        "a provenance-only reseal reuses every sealed chunk"
    );
    assert_eq!(
        scheduler
            .latest_complete()
            .expect("resealed generation serves")
            .generation()
            .snapshot()
            .source_revision
            .clone()
            .expect("the reseal carries the committed revision")
            .as_str(),
        git_stdout(fixture.path(), &["rev-parse", "HEAD"]),
        "the reseal records the moved tip as its exact source revision"
    );
}

#[test]
fn unchanged_policy_transition_refuses_unsafe_serving_and_rebuilds_once() {
    let fixture = GitFixture::new(ALPHA_LIB_V1);
    let store = TempDir::new().expect("store root");
    let bytes = Arc::new(SharedCodeIndexBytePoolV1::default());
    let mut config_a = scheduler(&fixture, store.path().to_path_buf(), Arc::clone(&bytes));
    let generation_a = published(
        config_a
            .reconcile_now()
            .expect("publish configuration A generation"),
    )
    .generation_id;
    drop(config_a);

    let mut config_b = scheduler(&fixture, store.path().to_path_buf(), Arc::clone(&bytes));
    replace_scheduler_policy_revision(&mut config_b, "policy.daemon.v2");
    assert!(
        config_b.latest_complete().is_none(),
        "a generation sealed under a foreign policy must not serve while B rebuilds"
    );
    let recovery = config_b
        .generation_recovery()
        .read()
        .expect("generation recovery status")
        .clone()
        .expect("policy recovery status");
    assert_eq!(recovery.incompatible_generation_id, generation_a.as_str());
    assert_eq!(recovery.incompatibilities, ["policy_revision"]);
    assert_eq!(
        recovery.serving,
        tracedecay_contracts::code_index_freshness::CodeIndexGenerationRecoveryServingV1::Refused
    );
    let generation_b = published(
        config_b
            .reconcile_now()
            .expect("unchanged source still requires a configuration B generation"),
    )
    .generation_id;
    assert_ne!(generation_b, generation_a);
    let pointer = config_b
        .publication
        .read_publication_pointer()
        .expect("read configuration B publication")
        .expect("configuration B publication pointer");
    assert_eq!(pointer.generation_id, generation_b.as_str());
    assert_eq!(
        pointer
            .generation_index
            .iter()
            .filter(|entry| entry.generation_id == generation_a.as_str())
            .count(),
        0,
        "the replacement is the sole durable candidate for this exact Git evidence"
    );
    assert!(
        config_b
            .generation_recovery()
            .read()
            .expect("generation recovery status")
            .is_none(),
        "the replacement publication clears recovery status"
    );
    assert!(matches!(
        config_b
            .reconcile_now()
            .expect("configuration B must settle after one rebuild"),
        CodeIndexReconcileOutcomeV1::Noop(_)
    ));
    drop(config_b);

    let mut restarted = scheduler(&fixture, store.path().to_path_buf(), bytes);
    replace_scheduler_policy_revision(&mut restarted, "policy.daemon.v2");
    assert!(matches!(
        restarted
            .reconcile_now()
            .expect("restart must adopt the persisted configuration B generation"),
        CodeIndexReconcileOutcomeV1::Noop(_)
    ));
    assert_eq!(
        restarted
            .latest_complete()
            .expect("configuration B survives restart")
            .generation()
            .manifest()
            .generation_id,
        generation_b
    );
}

#[test]
fn chunker_transition_preserves_safe_serving_until_replacement() {
    let fixture = GitFixture::new(ALPHA_LIB_V1);
    let store = TempDir::new().expect("store root");
    let bytes = Arc::new(SharedCodeIndexBytePoolV1::default());
    let mut config_a = scheduler(&fixture, store.path().to_path_buf(), Arc::clone(&bytes));
    let generation_a = published(
        config_a
            .reconcile_now()
            .expect("publish configuration A generation"),
    )
    .generation_id;
    drop(config_a);

    let mut config_b = scheduler(&fixture, store.path().to_path_buf(), bytes);
    let foreign_revision = format!("{DAEMON_CODE_INDEX_CHUNKER_REVISION}-foreign");
    replace_scheduler_chunker_revision(&mut config_b, &foreign_revision);
    assert_eq!(
        config_b
            .latest_complete()
            .expect("chunker-only incompatibility may preserve serving")
            .generation()
            .manifest()
            .generation_id,
        generation_a
    );
    let recovery = config_b
        .generation_recovery()
        .read()
        .expect("generation recovery status")
        .clone()
        .expect("chunker recovery status");
    assert_eq!(recovery.incompatibilities, ["chunker_revision"]);
    assert_eq!(
        recovery.serving,
        tracedecay_contracts::code_index_freshness::CodeIndexGenerationRecoveryServingV1::Preserved
    );
    let generation_b = published(
        config_b
            .reconcile_now()
            .expect("chunker transition must publish a replacement"),
    )
    .generation_id;
    assert_ne!(generation_b, generation_a);
    assert_eq!(
        config_b
            .latest_complete()
            .expect("replacement generation")
            .generation()
            .manifest()
            .chunker_revision
            .as_str(),
        foreign_revision.as_str()
    );
}

#[test]
fn occurrence_graph_store_is_available_before_catalog_warm() {
    let fixture = GitFixture::new(ALPHA_LIB_V1);
    let store_root = TempDir::new().expect("store root");
    let mut scheduler = scheduler(
        &fixture,
        store_root.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    );
    published(scheduler.reconcile_now().expect("seed generation"));
    let latest = scheduler.latest_complete().expect("latest generation");
    let generation_id = latest.generation().manifest().generation_id.clone();
    let projector_revision = tracedecay_graph_db::GraphProjectorRevision::try_from(
        crate::code_index::graph_projection::CODE_GRAPH_PROJECTOR_REVISION.to_owned(),
    )
    .expect("projector revision");
    let projection = crate::code_index::graph_projection::code_graph_projection_identity(
        tracedecay_graph_db::GraphNamespace::new("code-graph").expect("graph namespace"),
    )
    .expect("projection identity");
    let manifest =
        crate::code_index::graph_projection::build_published_code_graph_manifest_checked(
            projection,
            latest.generation(),
            &projector_revision,
            &|| Ok(()),
        )
        .expect("code graph manifest");
    let snapshot = tracedecay_graph_db::VerifiedGraphSnapshot::memory(
        manifest.as_ref().clone(),
        Arc::new(tracedecay_graph_db::NeverCancelled),
    )
    .expect("verified graph snapshot");
    let graph_store = Arc::new(
        crate::code_index::graph_projection::CodeGraphProjectionStore::from_verified_snapshot(
            snapshot,
            generation_id.clone(),
        )
        .expect("graph projection store"),
    );
    let reader = graph_store
        .evidence_reader_with_cancellation(
            &generation_id,
            Some(latest.generation().snapshot().repository.clone()),
            latest.source_freshness().expect("source freshness"),
            Arc::new(tracedecay_graph_db::NeverCancelled),
        )
        .expect("graph evidence reader");
    graph_store
        .mark_interactive_catalog_warming()
        .expect("mark background catalog warm before serving");
    latest
        .install_graph_serving(
            reader,
            Some(Arc::clone(&graph_store)),
            super::super::CodeGraphServingAuthorityV1::Memory,
        )
        .expect("install occurrence graph serving");

    assert_eq!(graph_store.interactive_catalog_is_warm(), Ok(false));
    assert!(
        Arc::ptr_eq(
            &latest
                .interactive_graph_store()
                .expect("occurrence graph serving is independent of catalog warm"),
            &graph_store,
        ),
        "the installed generation-pinned graph store is immediately available"
    );
}

#[test]
fn generation_bound_rerank_authorizes_mixed_symbol_and_chunk_anchors() {
    let fixture = GitFixture::new(&[(
        "src/lib.rs",
        "pub fn alpha() -> u32 { 1 }\npub fn beta() -> u32 { alpha() }\n",
    )]);
    let store = TempDir::new().expect("store root");
    let mut scheduler = scheduler(
        &fixture,
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    );
    published(scheduler.reconcile_now().expect("initial publish"));
    let latest = scheduler.latest_complete().expect("latest generation");
    let symbol_chunk = latest
        .generation
        .chunks()
        .chunks()
        .iter()
        .find(|chunk| chunk.anchor.symbol_occurrence_id.is_some())
        .expect("symbol chunk");
    let symbol = symbol_chunk
        .anchor
        .symbol_occurrence_id
        .as_ref()
        .expect("symbol occurrence");
    let chunk = latest
        .generation
        .chunks()
        .chunks()
        .iter()
        .find(|chunk| chunk.id != symbol_chunk.id)
        .unwrap_or(symbol_chunk);
    let anchors = [
        RetrievalAnchorId::new(format!("code-symbol:{}", symbol.as_str())).expect("symbol anchor"),
        RetrievalAnchorId::new(format!("code-chunk:{}", chunk.id.as_str())).expect("chunk anchor"),
    ];
    let candidates = anchors
        .iter()
        .enumerate()
        .map(|(ordinal, anchor)| RankedCandidate {
            candidate: FusedCandidate {
                anchor_id: anchor.clone(),
                logical_evidence_id: LogicalEvidenceId::new(anchor.as_str().to_owned())
                    .expect("logical evidence"),
                occurrences: Vec::new(),
                exact_class: ExactClass::Approximate,
                utility_micros: 2 - ordinal as u64,
                contributions: Vec::new(),
                freshness: Vec::new(),
                decisions: Vec::new(),
            },
            final_ordinal: ordinal as u32,
        })
        .collect::<Vec<_>>();
    let request = RetrievalRequest {
        principal: PrincipalId::new("principal.rerank-mixed").expect("principal"),
        scope: RetrievalScope {
            privacy_domain: latest.generation.manifest().privacy_domain.clone(),
            root: SingleRootScopeV1 {
                repository: latest.generation.snapshot().repository.clone(),
                worktree: latest.generation.snapshot().worktree.clone(),
                reference: latest.generation.snapshot().reference.clone(),
            },
        },
        temporal_mode: TemporalModeV1::Current,
        snapshot: RetrievalSnapshot {
            watermarks: VectorWatermark::default(),
            freshness_digest: FreshnessVectorDigest::new(format!("sha256:{}", "f".repeat(64)))
                .expect("freshness digest"),
            authorization_revision: AuthorizationRevision::new("authorization.rerank-mixed.v1")
                .expect("authorization revision"),
            captured_at: UtcMicros(1),
        },
        profile_id: "profile.rerank-mixed.v1"
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
    let query = EphemeralSanitizedQueryViewV1::sanitize(
        "alpha",
        SanitizerRevision::new("sanitizer.rerank-mixed.v1").expect("sanitizer"),
        QueryNormalizationRevision::new("normalization.rerank-mixed.v1").expect("normalization"),
    )
    .expect("query");
    let policy = RerankPolicy {
        policy_id: "rerank.mixed.v1".to_owned().try_into().expect("policy"),
        evaluation_result_anchor: RetrievalAnchorId::new("evaluation.rerank-mixed.v1")
            .expect("evaluation"),
        max_candidates: 2,
        max_input_bytes: u64::MAX,
        max_input_tokens: u64::MAX,
        max_work_units: 2,
        max_model_invocations: 1,
        deadline_micros: None,
    };
    let mut views = GenerationBoundCodeRerankViewsV1::new(&latest.generation, &query);
    let runtime_outcome = BoundedRerankRuntimeV1::new(
        &mut views,
        &MixedAnchorReverseRerankExecutorV1,
    )
    .rerank(&request, &policy, &candidates, &ReadyRerankControlV1);
    let pins = RerankCompatibilityPinsV1 {
        implementation_revision: ComponentRevision::new("rerank.fastembed.production.v1")
            .expect("implementation revision"),
        artifact_manifest_digest: MixedAnchorReverseRerankExecutorV1
            .artifact_manifest_digest()
            .clone(),
        runtime_compatibility_digest: ManifestDigest::new(format!("sha256:{}", "b".repeat(64)))
            .expect("runtime digest"),
    };
    let authority = ProductionCodeRerankAuthorityV1::from_executor_for_test(
        pins,
        Arc::new(MixedAnchorReverseRerankExecutorV1),
    );
    let execute_outcome = authority.execute(
        &latest.generation,
        &query,
        &request,
        &policy,
        &candidates,
        &ReadyRerankControlV1,
    );

    assert_eq!(execute_outcome, runtime_outcome);
    assert_eq!(
        execute_outcome.public_status,
        OptionalStagePublicStatus::Complete
    );
    assert_eq!(
        execute_outcome
            .ordered_candidates
            .iter()
            .map(|candidate| candidate.candidate.anchor_id.clone())
            .collect::<Vec<_>>(),
        anchors.into_iter().rev().collect::<Vec<_>>()
    );

    let cancelled = authority.execute(
        &latest.generation,
        &query,
        &request,
        &policy,
        &candidates,
        &CancelledRerankControlV1,
    );
    assert_eq!(
        cancelled.public_status,
        OptionalStagePublicStatus::Cancelled
    );
    assert_eq!(cancelled.ordered_candidates, candidates);
    let mut composition = tracedecay_query::retrieval::fusion::CompositionOutputV1 {
        profile_id: request.profile_id.clone(),
        ranked_candidates: candidates.clone(),
        comparator_records: Vec::new(),
        internal_lane_outcomes: BTreeMap::new(),
        public_lane_statuses: BTreeMap::new(),
        freshness: Vec::new(),
        lane_checkpoints: Vec::new(),
        dedupe_decisions: Vec::new(),
        diversity_decisions: Vec::new(),
    };
    let status = apply_bounded_rerank_outcome(&mut composition, cancelled);
    assert_eq!(status, OptionalStagePublicStatus::Cancelled);
    assert_eq!(composition.ranked_candidates, candidates);
}

#[test]
fn cross_worktree_byte_reuse_without_identity_alias() {
    let first = GitFixture::new(&[("src/lib.rs", "pub fn shared() -> u32 { 7 }\n")]);
    let linked_root = TempDir::new().expect("linked worktree root");
    let linked = linked_root.path().join("linked");
    let linked_arg = linked.to_str().expect("linked worktree path");
    git(
        first.path(),
        &["worktree", "add", "-q", "-b", "linked", linked_arg, "main"],
    );
    let store = TempDir::new().expect("store root");
    let registry = CodeIndexSchedulerRegistryV1::new(2);
    let project_id = ProjectId::new("project.linked-worktrees").expect("valid project");

    let mut first_scheduler = registry
        .open_worktree(project_id.clone(), first.path(), store.path().join("first"))
        .expect("first scheduler");
    let mut second_scheduler = registry
        .open_worktree(project_id.clone(), &linked, store.path().join("second"))
        .expect("second scheduler");
    let first_publish = published(first_scheduler.reconcile_now().expect("first publish"));
    let second_publish = published(second_scheduler.reconcile_now().expect("second publish"));
    let first_generation = first_scheduler
        .latest_complete()
        .expect("first generation")
        .generation;
    let second_generation = second_scheduler
        .latest_complete()
        .expect("second generation")
        .generation;
    let reuse = registry.byte_pool_stats();

    assert!(reuse.reused >= 1, "sanitized source bytes must be shared");
    assert!(
        reuse.parse_chunk_reused >= 1,
        "matching parse/chunk artifacts must be physically shared"
    );
    assert_eq!(first_publish.repository_id, second_publish.repository_id);
    assert_eq!(first_generation.manifest().project_id, project_id);
    assert_eq!(second_generation.manifest().project_id, project_id);
    assert_ne!(
        first_generation.snapshot().worktree,
        second_generation.snapshot().worktree
    );
    assert_eq!(
        first_publish.snapshot_content_identity,
        second_publish.snapshot_content_identity
    );
    assert_ne!(
        first_publish.file_occurrence_ids, second_publish.file_occurrence_ids,
        "shared artifacts must never alias worktree occurrence identity"
    );
    assert_ne!(first_publish.generation_id, second_publish.generation_id);
    assert_ne!(
        first_generation.manifest().snapshot_digest,
        second_generation.manifest().snapshot_digest
    );
    assert_eq!(
        first_generation.capability().manifest_digest,
        second_generation.capability().manifest_digest,
        "byte-identical capability evidence is generation-free; generation, occurrence, \
         snapshot, and publication identities remain worktree-local above and below"
    );
    assert_ne!(
        first_generation.projection().publication_digest(),
        second_generation.projection().publication_digest(),
        "publication identity remains generation-local"
    );

    git(&linked, &["mv", "src/lib.rs", "src/renamed.rs"]);
    second_scheduler.notify_path(linked.join("src/lib.rs"));
    second_scheduler.notify_path(linked.join("src/renamed.rs"));
    published(
        second_scheduler
            .reconcile_now()
            .expect("renamed linked-worktree publish"),
    );
    let after_rename = registry.byte_pool_stats();
    assert_eq!(
        after_rename.parse_chunk_reused, reuse.parse_chunk_reused,
        "same content at a new logical path must not reuse path-bound parse/chunk artifacts"
    );

    write(&linked, "src/renamed.rs", "pub fn shared() -> u32 { 8 }\n");
    second_scheduler.notify_path(linked.join("src/renamed.rs"));
    published(
        second_scheduler
            .reconcile_now()
            .expect("edited linked-worktree publish"),
    );
    let after_edit = registry.byte_pool_stats();
    assert_eq!(
        after_edit.parse_chunk_reused, after_rename.parse_chunk_reused,
        "changed source content must not reuse the prior parse/chunk artifact"
    );
    assert_eq!(
        first_scheduler
            .latest_complete()
            .expect("first worktree remains current")
            .generation
            .manifest()
            .generation_id,
        first_publish.generation_id,
        "editing one linked worktree must not invalidate its sibling"
    );
}

#[tokio::test]
async fn existing_path_remount_rejects_foreign_project_identity() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn owned() {}\n")]);
    let store = TempDir::new().expect("store root");
    let registry = CodeIndexSchedulerRegistryV1::new(1);
    registry
        .mount_worktree(
            ProjectId::new("project.remount.owner").expect("valid owner project"),
            fixture.path(),
            store.path().to_path_buf(),
            None,
        )
        .await
        .expect("mount owning project");

    let error = registry
        .mount_worktree(
            ProjectId::new("project.remount.foreign").expect("valid foreign project"),
            fixture.path(),
            store.path().to_path_buf(),
            None,
        )
        .await
        .expect_err("same path must reject a foreign project");

    assert!(matches!(
        error,
        super::super::CodeIndexSchedulerErrorV1::Identity(message)
            if message.contains("different project identity")
    ));
    registry.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_same_root_mounts_keep_one_canonical_owner() {
    let fixture = GitFixture::new(&[("src/main.rs", "fn main() {}\n")]);
    let store = TempDir::new().expect("store root");
    let registry = CodeIndexSchedulerRegistryV1::new(4);
    let barrier = Arc::new(tokio::sync::Barrier::new(4));
    let mut tasks = Vec::new();
    for _ in 0..4 {
        let registry = registry.clone();
        let root = fixture.path().to_path_buf();
        let store_root = store.path().to_path_buf();
        let barrier = Arc::clone(&barrier);
        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            registry
                .mount_worktree(test_project_id(), &root, store_root, None)
                .await
        }));
    }

    let mut mounted = 0;
    let mut reused = 0;
    for task in tasks {
        if task
            .await
            .expect("mount task joins")
            .expect("mount succeeds")
        {
            mounted += 1;
        } else {
            reused += 1;
        }
    }
    assert_eq!(mounted, 1, "one caller must install the canonical owner");
    assert_eq!(reused, 3, "same-root racers must reuse the canonical owner");
    assert_eq!(
        registry.mounted.lock().await.len(),
        1,
        "the registry must retain exactly one owner after the mount race"
    );

    registry.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn paused_cold_mount_rejects_a_root_retiring_before_final_commit() {
    let fixture = GitFixture::new(&[("src/main.rs", "fn main() {}\n")]);
    let store = TempDir::new().expect("store root");
    let registry = CodeIndexSchedulerRegistryV1::new(2);
    let root = fixture.path().canonicalize().expect("canonical root");
    let (cold_commit_entered, release_cold_commit) = registry
        .pause_next_cold_mount_before_final_commit(root.clone())
        .await;
    let cold_registry = registry.clone();
    let cold_root = fixture.path().to_path_buf();
    let cold_store = store.path().to_path_buf();
    let cold_mount = tokio::spawn(async move {
        cold_registry
            .mount_worktree(test_project_id(), &cold_root, cold_store, None)
            .await
    });

    cold_commit_entered
        .await
        .expect("first cold mount must pause before its final owner commit");

    let roots = BTreeSet::from([root.clone()]);
    assert!(
        !registry
            .retire_project_roots_with_deadline(&roots, Duration::from_millis(25))
            .await,
        "retirement must wait for the paused exact cold reservation"
    );
    assert_eq!(registry.retiring_owner_count().await, 0);

    release_cold_commit
        .send(())
        .expect("release paused cold mount final commit");
    let cold_error = cold_mount
        .await
        .expect("paused cold mount joins")
        .expect_err("a retiring root must reject a stale cold mount final commit");
    assert!(matches!(
        cold_error,
        super::super::CodeIndexSchedulerErrorV1::Identity(message)
            if message.contains("still retiring")
    ));
    let retiring = registry.retiring.lock().await;
    let mounted = registry.mounted.lock().await;
    assert!(!retiring.contains_key(&root));
    assert!(!mounted.contains_key(&root));
    drop(mounted);
    drop(retiring);

    assert!(
        registry
            .retire_project_roots_with_deadline(&roots, Duration::from_secs(2))
            .await,
        "the completed retired cold reservation must release"
    );
    assert_eq!(registry.retiring_owner_count().await, 0);
    assert!(
        !registry.mounted.lock().await.contains_key(&root),
        "the rejected cold mount must not leave a replacement worker"
    );

    registry.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn retirement_parks_the_incumbent_while_a_same_root_remount_waits_on_its_scheduler() {
    let fixture = GitFixture::new(&[("src/main.rs", "fn main() {}\n")]);
    let store = TempDir::new().expect("store root");
    let registry = CodeIndexSchedulerRegistryV1::new(2);
    let root = fixture.path().canonicalize().expect("canonical root");
    assert!(
        registry
            .mount_worktree(
                test_project_id(),
                fixture.path(),
                store.path().to_path_buf(),
                None,
            )
            .await
            .expect("incumbent mount succeeds")
    );
    let scheduler = registry
        .scheduler_handle(&root)
        .await
        .expect("incumbent scheduler");
    let (held_tx, held_rx) = std::sync::mpsc::channel();
    let (release_scheduler_tx, release_scheduler_rx) = std::sync::mpsc::channel();
    let held_scheduler = Arc::clone(&scheduler);
    let lock_thread = std::thread::spawn(move || {
        let scheduler = held_scheduler
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let wake = Arc::clone(&scheduler.wake);
        held_tx.send(wake).expect("signal held scheduler");
        release_scheduler_rx.recv().expect("release held scheduler");
    });
    let wake = held_rx.recv().expect("scheduler lock must be held");
    wake.notify_one();
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let reconciling = registry.reconcile_in_progress_for_test(&root).await;
            if reconciling {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .expect("incumbent worker blocks in its reconcile pass");

    let replacement_entered = registry
        .observe_next_existing_semantic_schedule_replacement(root.clone())
        .await;
    let remount_registry = registry.clone();
    let remount_root = fixture.path().to_path_buf();
    let remount_store = store.path().to_path_buf();
    let remount = tokio::spawn(async move {
        remount_registry
            .mount_worktree(test_project_id(), &remount_root, remount_store, None)
            .await
    });
    replacement_entered
        .await
        .expect("same-root remount reaches its semantic replacement");

    let roots = BTreeSet::from([root.clone()]);
    let _retirement = registry
        .retire_project_roots_with_deadline(&roots, Duration::from_millis(25))
        .await;
    let _parked_before_retry = {
        let retiring = registry.retiring.lock().await;
        let mounted = registry.mounted.lock().await;
        (retiring.contains_key(&root), mounted.contains_key(&root))
    };

    release_scheduler_tx
        .send(())
        .expect("release incumbent scheduler");
    lock_thread.join().expect("held scheduler thread joins");
    let remount = remount.await.expect("same-root remount joins");
    let drained = registry
        .retire_project_roots_with_deadline(&roots, Duration::from_secs(2))
        .await;
    let no_owner_remains = {
        let retiring = registry.retiring.lock().await;
        let mounted = registry.mounted.lock().await;
        !retiring.contains_key(&root) && !mounted.contains_key(&root)
    };
    registry.shutdown().await;

    // 24b3c81c4d superseded lock-park: the worker polls try_lock and cancels
    // when `shutting_down` is set, so the 25ms deadline may complete. Remount
    // must still observe retirement (not "owner changed") and must not install.
    assert!(matches!(
        remount,
        Err(super::super::CodeIndexSchedulerErrorV1::Identity(message))
            if message.contains("retired while semantic schedule update waited")
    ));
    assert!(
        drained,
        "the incumbent must drain after its scheduler releases"
    );
    assert!(
        no_owner_remains,
        "the refused remount must not leave a mounted or retiring orphan"
    );
}

#[test]
fn empty_generation_restart_preserves_project_identity() {
    // A file with a compiled language descriptor (so the snapshot has something
    // extractable and the reconcile reaches a publish) whose content yields no
    // symbols, so the sealed generation is chunk-empty. `# fixture` used to be
    // that file, but the markdown extractor now chunks headings, which made
    // this fixture produce a non-empty generation and stopped exercising the
    // empty-generation restore this test exists for.
    let fixture = GitFixture::new(&[("README.md", "")]);
    let store = TempDir::new().expect("store root");
    let project_id = ProjectId::new("project.empty-restart").expect("valid project");
    let mut scheduler = CodeIndexWorktreeSchedulerV1::open(
        project_id.clone(),
        fixture.path(),
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    )
    .expect("open scheduler");
    published(scheduler.reconcile_now().expect("publish empty generation"));
    let generation = scheduler.latest_complete().expect("published generation");
    assert!(generation.generation().chunks().chunks().is_empty());
    assert_eq!(generation.generation().manifest().project_id, project_id);
    drop(generation);
    drop(scheduler);

    let reopened = CodeIndexWorktreeSchedulerV1::open(
        project_id.clone(),
        fixture.path(),
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    )
    .expect("reopen scheduler");
    assert_eq!(
        reopened
            .latest_complete()
            .expect("restored generation")
            .generation()
            .manifest()
            .project_id,
        project_id
    );
    drop(reopened);

    let mut foreign = CodeIndexWorktreeSchedulerV1::open(
        ProjectId::new("project.empty-restart.foreign").expect("valid foreign project"),
        fixture.path(),
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    )
    .expect("foreground open defers sealed identity validation");
    let error = foreign
        .activate_or_reconcile()
        .expect_err("persisted generation must reject a foreign project");
    assert!(matches!(
        error,
        super::super::CodeIndexSchedulerErrorV1::Identity(message)
            if message.contains("different project/worktree identity")
    ));
}

#[test]
fn one_symbol_unrelated_work_skip() {
    let fixture = GitFixture::new(&[(
        "src/lib.rs",
        "pub fn alpha() -> u32 { 1 }\n\npub fn unrelated() -> u32 { 99 }\n",
    )]);
    let store = TempDir::new().expect("store root");
    let bytes = Arc::new(SharedCodeIndexBytePoolV1::default());
    let mut incremental = scheduler(
        &fixture,
        store.path().join("incremental"),
        Arc::clone(&bytes),
    );
    published(incremental.reconcile_now().expect("baseline"));

    fixture.edit(
        "src/lib.rs",
        "pub fn alpha() -> u32 { 2 }\n\npub fn unrelated() -> u32 { 99 }\n",
    );
    incremental.notify_path(fixture.path().join("src/lib.rs"));
    let changed = published(incremental.reconcile_now().expect("one-symbol publish"));

    assert_eq!(changed.reextracted_files, 1);
    assert!(changed.changed_chunks > 0);
    assert!(
        changed.reused_chunks > 0,
        "unrelated symbol chunks must skip projection work"
    );
    let mut clean = scheduler(&fixture, store.path().join("clean"), bytes);
    let rebuilt = published(clean.reconcile_now().expect("clean rebuild"));
    assert_eq!(
        changed.snapshot_content_identity, rebuilt.snapshot_content_identity,
        "one-file incremental capture must equal a clean capture of the same source"
    );
    assert_eq!(
        changed.lane_digest, rebuilt.lane_digest,
        "one-file incremental projection must equal a clean projection"
    );
}

#[test]
fn superseding_notifies_publish_only_latest_content() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn alpha() -> u32 { 1 }\n")]);
    let store = TempDir::new().expect("store root");
    let bytes = Arc::new(SharedCodeIndexBytePoolV1::default());
    let mut live = scheduler(&fixture, store.path().join("live"), Arc::clone(&bytes));
    let mut clean = scheduler(&fixture, store.path().join("clean"), bytes);
    published(live.reconcile_now().expect("live baseline"));
    published(clean.reconcile_now().expect("clean baseline"));

    fixture.edit("src/lib.rs", "pub fn alpha() -> u32 { 2 }\n");
    live.notify_path(fixture.path().join("src/lib.rs"));
    fixture.edit("src/lib.rs", "pub fn alpha() -> u32 { 3 }\n");
    live.notify_path(fixture.path().join("src/lib.rs"));
    live.notify_overflow();

    let superseded = published(live.reconcile_now().expect("superseded reconcile"));
    let expected = published(clean.reconcile_now().expect("clean latest reconcile"));
    assert_eq!(
        superseded.snapshot_content_identity, expected.snapshot_content_identity,
        "fair supersession must publish only the latest reconciled content"
    );
    assert_eq!(superseded.lane_digest, expected.lane_digest);
    assert!(superseded.overflow_reconciled);
}

#[tokio::test]
async fn graph_activation_does_not_wait_for_bounded_text_projection() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn independently_activated_graph() {}\n")]);
    let store = TempDir::new().expect("store root");
    let mut scheduler = scheduler(
        &fixture,
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    );
    published(scheduler.reconcile_now().expect("publish generation"));
    let latest = scheduler.latest_complete().expect("latest generation");
    let replay_binding = scheduler
        .code_graph_replay_binding(&latest.generation().manifest().generation_id)
        .expect("sealed graph replay binding");
    let activation = super::super::graph_activation::CodeGraphActivationAuthorityV1::Memory {
        policy: Arc::new(std::sync::atomic::AtomicBool::new(true)),
    };

    activation
        .activate(
            &scheduler.project_id,
            &scheduler.repository_id,
            &scheduler.worktree_id,
            latest.clone(),
            replay_binding,
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
        )
        .await
        .expect("graph activation must not wait for the independently bounded text projection");

    let _ = latest
        .production_graph_serving()
        .expect("graph seating must succeed without waiting on the bounded text ladder");
}

#[test]
fn ordinary_background_reconcile_does_not_supersede_in_flight_text_work() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn background_wake() {}\n")]);
    let store = TempDir::new().expect("store root");
    let scheduler = scheduler(
        &fixture,
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    );
    let control = tracedecay_application::code_index::DaemonCodeIndexControlV1::new(
        Arc::clone(&scheduler.epoch),
        Arc::clone(&scheduler.shutting_down),
    );

    scheduler.request_background_reconcile();

    assert!(
        !control.is_cancelled(),
        "a source-neutral background wake must not cancel text work already admitted for the current generation"
    );
}

#[test]
fn observed_change_after_neutral_wake_supersedes_in_flight_text_work() {
    let fixture = GitFixture::new(&[(
        "src/lib.rs",
        "pub fn neutral_then_observed_change() -> u32 { 1 }\n",
    )]);
    let store = TempDir::new().expect("store root");
    let scheduler = scheduler(
        &fixture,
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    );
    let control = tracedecay_application::code_index::DaemonCodeIndexControlV1::new(
        Arc::clone(&scheduler.epoch),
        Arc::clone(&scheduler.shutting_down),
    );

    scheduler.request_background_reconcile();
    assert!(
        !control.is_cancelled(),
        "the neutral wake must preserve work admitted for unchanged source state"
    );

    fixture.edit(
        "src/lib.rs",
        "pub fn neutral_then_observed_change() -> u32 { 2 }\n",
    );
    scheduler.request_background_reconcile_for_observed_change();

    assert!(
        control.is_cancelled(),
        "a source change observed before the neutral wake drains must still supersede stale work"
    );
    let observed_change_epoch = scheduler.epoch.load(std::sync::atomic::Ordering::Acquire);

    scheduler.request_background_reconcile_for_observed_change();

    assert_eq!(
        scheduler.epoch.load(std::sync::atomic::Ordering::Acquire),
        observed_change_epoch,
        "repeated reads of the same pending drift must keep the cancellation epoch stable"
    );
}

#[test]
fn observed_change_during_capture_retries_before_publication() {
    let fixture = GitFixture::new(&[(
        "src/lib.rs",
        "pub fn changed_during_capture() -> u32 { 1 }\n",
    )]);
    let store = TempDir::new().expect("store root");
    let mut scheduler = scheduler(
        &fixture,
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    );
    published(scheduler.reconcile_now().expect("publish initial source"));
    scheduler.request_background_reconcile();
    let mut capture_attempts = 0;

    let outcome = scheduler
        .reconcile_now_with_capture(|scheduler, control| {
            let captured = scheduler.capture_authoritative_snapshot(Some(control))?;
            capture_attempts += 1;
            if capture_attempts == 1 {
                fixture.edit(
                    "src/lib.rs",
                    "pub fn changed_during_capture() -> u32 { 2 }\n",
                );
                scheduler.request_background_reconcile_for_observed_change();
            }
            Ok(captured)
        })
        .expect("reconcile source changed during capture");
    let published = published(outcome);
    let current = scheduler
        .capture_authoritative_snapshot(None)
        .expect("capture current source after publication");

    assert_eq!(
        capture_attempts, 2,
        "the capture superseded by the observed change must be retried exactly once"
    );
    assert_eq!(
        published.snapshot_content_identity, current.snapshot.content_identity,
        "the published generation must describe the source captured after the observed change"
    );
}

#[test]
fn same_daemon_scheduler_retire_remount_mints_new_progress_producer() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn producer_epoch() {}\n")]);
    let store = TempDir::new().expect("store root");
    let registry = CodeIndexSchedulerRegistryV1::new(1);
    let first = registry
        .open_worktree(
            test_project_id(),
            fixture.path(),
            store.path().to_path_buf(),
        )
        .expect("first scheduler owner");
    let (first_daemon, first_producer) = first.progress_incarnations_for_test();
    drop(first);
    let second = registry
        .open_worktree(
            test_project_id(),
            fixture.path(),
            store.path().to_path_buf(),
        )
        .expect("replacement scheduler owner");
    let (second_daemon, second_producer) = second.progress_incarnations_for_test();

    assert_eq!(second_daemon, first_daemon);
    assert!(
        second_producer > first_producer,
        "same-daemon scheduler replacement must outrank delayed low-epoch progress"
    );
}

#[test]
fn source_epoch_advance_does_not_discard_immutable_text_progress() {
    let mut source = String::new();
    for ordinal in 0..600 {
        writeln!(
            source,
            "pub fn immutable_symbol_{ordinal}() -> usize {{ {ordinal} }}"
        )
        .expect("write immutable source fixture");
    }
    let fixture = GitFixture::new(&[("src/lib.rs", source.as_str())]);
    let store = TempDir::new().expect("store root");
    let mut scheduler = scheduler(
        &fixture,
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    );
    published(scheduler.reconcile_now().expect("publish generation"));
    let latest = scheduler.latest_complete().expect("latest generation");
    let admitted = latest.text_execution_control();

    // Reproduce a hook arriving after a text pass captures its control but
    // before the sealed source's first cancellation checkpoint.
    scheduler.notify_path(fixture.path().join("src/lib.rs"));
    let result = latest.advance_artifact_text_serving(1, &admitted);

    assert!(
        matches!(result, Ok(false | true)),
        "a worktree freshness epoch must not cancel immutable generation work: {result:?}"
    );
    assert!(
        matches!(
            &*latest.text_projection_build.lock_slot(),
            super::super::CodeTextProjectionSlotV1::Building(_)
        ) || latest.query_owners_are_warm(),
        "the bounded pass must retain or complete its generation-owned progress"
    );
}

#[test]
fn shutdown_cancels_generation_owned_text_work() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn shutdown_text_work() {}\n")]);
    let store = TempDir::new().expect("store root");
    let mut scheduler = scheduler(
        &fixture,
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    );
    published(scheduler.reconcile_now().expect("publish generation"));
    let latest = scheduler.latest_complete().expect("latest generation");
    scheduler
        .shutting_down
        .store(true, std::sync::atomic::Ordering::Release);

    assert_eq!(
        latest.advance_text_serving(1),
        Err(tracedecay_query::retrieval::RetrievalPortError::Cancelled),
        "daemon shutdown must remain a cancellation fence for immutable text work"
    );
}

/// A healthy Git-watcher backstop is a freshness probe, not evidence that the
/// checkout changed. When the exact stat signature still matches, routing that
/// probe must leave both the source-hint authority and worker queue empty.
#[tokio::test]
async fn unchanged_git_watcher_probe_does_not_enqueue_authoritative_capture() {
    let fixture = GitFixture::new(&[("src/main.rs", "fn main() {}\n")]);
    let store = TempDir::new().expect("store root");
    let scoped_store = super::super::scoped_code_index_store_root(store.path(), fixture.path());
    let scope = {
        let mut scheduler = scheduler(
            &fixture,
            scoped_store,
            Arc::new(SharedCodeIndexBytePoolV1::default()),
        );
        published(scheduler.reconcile_now().expect("seed retained generation"));
        let latest = scheduler.latest_complete().expect("seeded generation");
        let snapshot = latest.generation.snapshot();
        ResolvedScope::new(
            test_project_id(),
            snapshot.repository.clone(),
            snapshot.worktree.clone().expect("worktree id"),
            snapshot.reference.clone(),
        )
        .expect("resolved scope")
    };
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
        .expect("mount graph-off retained generation");

    let canonical_root = fixture.path().canonicalize().expect("canonical fixture");
    let scheduler = {
        let mounted = registry.mounted.lock().await;
        Arc::clone(
            &mounted
                .get(&canonical_root)
                .expect("mounted worktree")
                .scheduler,
        )
    };
    let settled_deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let settled = {
            let scheduler = scheduler
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            scheduler.verified_against_source() && scheduler.pending_hint_count() == Some(0)
        };
        if settled
            && !registry
                .reconcile_in_progress_for_test(fixture.path())
                .await
        {
            break;
        }
        assert!(
            Instant::now() <= settled_deadline,
            "retained graph-off owner never established initial freshness"
        );
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    let admission = registry
        .background_reconcile_admission()
        .acquire_owned()
        .await
        .expect("hold background reconcile admission");
    registry.clear_pending_wake_for_scope(&scope).await;
    assert_eq!(
        scheduler
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pending_hint_count(),
        Some(0),
        "settled fixture starts without source-change evidence"
    );

    let identity = tracedecay_runtime_core::git_discovery::GitRepositoryIdentity {
        worktree_root: canonical_root.clone(),
        git_dir: canonical_root.join(".git"),
        common_dir: canonical_root.join(".git"),
    };
    assert_eq!(
        registry.request_for_root(&identity).await,
        super::super::GitStateChangeRequestV1::Accepted
    );
    assert_eq!(
        scheduler
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pending_hint_count(),
        Some(0),
        "an unchanged watcher probe must not fabricate overflow evidence"
    );
    assert_eq!(
        registry.pending_wake_micros_for_scope(&scope).await,
        Some(0),
        "an unchanged watcher probe must not queue a capture pass"
    );

    drop(admission);
    registry.shutdown().await;
}

/// The live outage this covers: a background reconcile owns the scheduler
/// mutex for its whole pass — sealing a production-scale corpus holds it for
/// minutes per generation — while the seated serving generation stays fully
/// decoded, activated, and proven current from before the pass began.
/// Verified graph reads (redundancy, diagnose, `dead_code`, callers, impact)
/// resolve through `latest_complete_ready_decoded_for_root_scope`; refusing
/// them "not ready" for the whole pass turned bounded background work into a
/// tool outage that outlived exact/lexical retrieval by 25+ minutes.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn proven_seated_generation_serves_verified_reads_while_reconcile_owns_the_scheduler() {
    let fixture = GitFixture::new(&[("src/main.rs", "fn main() {}\n")]);
    let store = TempDir::new().expect("store root");
    let (registry, scope) = mounted_core_query_worktree(&fixture, &store).await;

    // Seating races the publication event; poll the ready gate bounded until
    // the quiet probe proves the seated generation current (arming the
    // busy-read witness).
    let ready = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(ready) = registry
                .latest_complete_ready_decoded_for_root_scope(fixture.path(), &scope)
                .await
            {
                break ready;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("the mounted generation becomes ready-decoded");
    let generation_id = ready.generation().manifest().generation_id.clone();

    let scheduler = {
        let mounted = registry.mounted.lock().await;
        Arc::clone(
            &mounted
                .get(&fixture.path().canonicalize().expect("canonical root"))
                .expect("mounted worktree")
                .scheduler,
        )
    };
    // Occupy the scheduler mutex exactly as an in-flight reconcile pass does:
    // a std mutex held on a blocking thread until released.
    let (locked_tx, locked_rx) = tokio::sync::oneshot::channel::<()>();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let holder = std::thread::spawn(move || {
        let _guard = scheduler
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _ = locked_tx.send(());
        let _ = release_rx.recv();
    });
    locked_rx.await.expect("scheduler mutex held");

    let busy = tokio::time::timeout(
        Duration::from_secs(30),
        registry.latest_complete_ready_decoded_for_root_scope(fixture.path(), &scope),
    )
    .await
    .expect("verified reads must not join the in-flight reconcile")
    .expect("the proven seated generation keeps serving while the scheduler is busy");
    assert_eq!(
        busy.generation().manifest().generation_id,
        generation_id,
        "the busy read serves the exact generation the quiet probe proved current"
    );
    assert!(
        registry.has_current_ready_decoded_for_root_scope(fixture.path(), &scope),
        "the readiness census reports the proven seated generation while the scheduler is busy"
    );

    release_tx.send(()).expect("release the held scheduler");
    holder.join().expect("scheduler holder thread");
    registry.shutdown().await;
}

#[tokio::test]
async fn selected_generation_mints_feedback_identity_after_registry_lookup_closes() {
    let fixture = GitFixture::new(&[("src/main.rs", "fn main() {}\n")]);
    let store = TempDir::new().expect("store root");
    let (registry, scope) = mounted_core_query_worktree(&fixture, &store).await;
    let selected = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(ready) = registry
                .latest_complete_ready_decoded_for_root_scope(fixture.path(), &scope)
                .await
            {
                break ready.text_generation_handle();
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("selected generation");
    let expected_generation = selected.metadata().manifest().generation_id.clone();
    let foreign_root = TempDir::new().expect("foreign root");
    assert!(
        registry
            .latest_feedback_generation_for_scope(foreign_root.path(), &scope)
            .await
            .is_none(),
        "a matching scope must not select a generation for a different root"
    );

    registry.shutdown().await;
    assert!(
        registry
            .latest_complete_ready(fixture.path())
            .await
            .is_none(),
        "the root-level current lookup must be unavailable after shutdown"
    );
    let identity = feedback_document_identity_from_generation(selected, fixture.path(), None)
        .expect("the already-selected generation remains an identity authority");
    assert_eq!(identity.generation_id, expected_generation);
}

/// Fail-closed half of the busy-read witness: a seated generation whose
/// currency was never proven (a boot-restored seat, a stale seat, or a
/// withdrawn proof) must stay a typed abstention while the scheduler mutex is
/// busy, exactly as before. Only a reconcile pass may arm busy serving: it is
/// the only thing that compares the checkout against the sealed digests, so it
/// is the only thing whose verdict can bind a seat to proven source. Reads no
/// longer walk the tree and so can never arm the witness themselves.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn busy_scheduler_still_refuses_a_seated_generation_without_a_currency_witness() {
    let fixture = GitFixture::new(&[("src/main.rs", "fn main() {}\n")]);
    let store = TempDir::new().expect("store root");
    let (registry, scope) = mounted_core_query_worktree(&fixture, &store).await;

    let ready = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(ready) = registry
                .latest_complete_ready_decoded_for_root_scope(fixture.path(), &scope)
                .await
            {
                break ready;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("the mounted generation becomes ready-decoded");
    let generation_id = ready.generation().manifest().generation_id.clone();

    let scheduler = {
        let mounted = registry.mounted.lock().await;
        Arc::clone(
            &mounted
                .get(&fixture.path().canonicalize().expect("canonical root"))
                .expect("mounted worktree")
                .scheduler,
        )
    };
    let witness = registry
        .serving_source_witness_for_root(fixture.path())
        .await
        .expect("mounted worktree witness");
    // Hold the scheduler first so no reconcile pass can re-prove the seat,
    // then withdraw the proof — the exact state a restart-restored seat is in
    // before its first passing probe.
    let (locked_tx, locked_rx) = tokio::sync::oneshot::channel::<()>();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let holder = std::thread::spawn(move || {
        let _guard = scheduler
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _ = locked_tx.send(());
        let _ = release_rx.recv();
    });
    locked_rx.await.expect("scheduler mutex held");
    *witness
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = None;

    assert!(
        registry
            .latest_complete_ready_decoded_for_root_scope(fixture.path(), &scope)
            .await
            .is_none(),
        "an unproven seat must stay a typed abstention while the scheduler is busy"
    );
    assert!(
        !registry.has_current_ready_decoded_for_root_scope(fixture.path(), &scope),
        "the readiness census must not report an unproven seat while the scheduler is busy"
    );

    release_tx.send(()).expect("release the held scheduler");
    holder.join().expect("scheduler holder thread");

    // With the scheduler quiet again the exact-source probe re-proves the
    // unchanged checkout and re-arms the witness.
    let reproved = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(ready) = registry
                .latest_complete_ready_decoded_for_root_scope(fixture.path(), &scope)
                .await
            {
                break ready;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("the quiet probe re-proves the unchanged seat");
    assert_eq!(
        reproved.generation().manifest().generation_id,
        generation_id
    );

    registry.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn source_currency_witness_refuses_a_stale_generation() {
    let fixture = GitFixture::new(&[("src/main.rs", "fn main() {}\n")]);
    let store = TempDir::new().expect("store root");
    let (registry, _) = mounted_core_query_worktree(&fixture, &store).await;
    let stale = wait_for_live_complete_generation(&registry, fixture.path()).await;
    let stale_generation = stale.generation().manifest().generation_id.clone();
    let stale_content = stale.generation().snapshot().content_identity.clone();

    fixture.edit("src/main.rs", "fn main() { changed(); }\n");
    git(fixture.path(), &["commit", "-qam", "publish successor"]);
    assert!(
        registry
            .notify_path(fixture.path(), fixture.path().join("src/main.rs"))
            .await,
        "the changed source is admitted to the retained worker"
    );
    let successor = wait_for_generation_change(&registry, fixture.path(), &stale_generation).await;
    let source_freshness = registry
        .source_freshness_for_root(fixture.path())
        .await
        .expect("mounted worktree source fence");

    assert!(
        source_freshness
            .source_currency_witness_for(&stale_generation, &stale_content)
            .is_none(),
        "a generation whose sealed content predates the freshness proof cannot obtain a witness"
    );
    assert!(
        source_freshness
            .source_currency_witness_for(
                &successor,
                &wait_for_live_complete_generation(&registry, fixture.path())
                    .await
                    .generation()
                    .snapshot()
                    .content_identity,
            )
            .is_some(),
        "the exact generation proved by the freshness fence obtains a witness"
    );

    registry.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn graph_read_during_reconcile_records_a_busy_follow_up() {
    let fixture = GitFixture::new(&[("src/main.rs", "fn main() {}\n")]);
    let store = TempDir::new().expect("store root");
    let (registry, scope) = mounted_core_query_worktree_with_one_permit(&fixture, &store).await;
    let _ = wait_for_live_complete_generation(&registry, fixture.path()).await;
    let admission = quiesced_background_reconcile_admission(&registry, fixture.path()).await;
    registry.clear_pending_wake_for_scope(&scope).await;
    let receipts_before = registry.event_to_ready_receipts();
    let owner_pass = registry
        .hold_reconcile_pass_for_test(fixture.path())
        .await
        .expect("mounted worktree");
    let source_freshness = registry
        .source_freshness_for_root(fixture.path())
        .await
        .expect("mounted worktree source fence");
    {
        let mut state = source_freshness
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.last_reconciled_at = Instant::now()
            .checked_sub(state.staleness_threshold + Duration::from_secs(1))
            .expect("age the source proof");
    }

    assert!(
        registry
            .latest_complete_ready_decoded_for_root_scope(fixture.path(), &scope)
            .await
            .is_none(),
        "an expired graph proof abstains while the owner pass is in flight"
    );
    drop(owner_pass);
    drop(admission);

    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let receipts = registry.event_to_ready_receipts();
            if receipts.len() > receipts_before.len()
                && receipts
                    .iter()
                    .skip(receipts_before.len())
                    .any(|receipt| receipt.trigger == CodeIndexCadenceTriggerV1::BusyFollowUp)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .expect("busy graph follow-up records a completed cadence receipt");
    assert!(
        registry
            .event_to_ready_receipts()
            .iter()
            .skip(receipts_before.len())
            .all(|receipt| receipt.trigger != CodeIndexCadenceTriggerV1::QueryAdmission),
        "the graph wake owned by an in-flight reconcile is not query admission"
    );

    registry.shutdown().await;
}

/// A publication can finish source capture long before its text artifact is
/// ready. The serving swap must reverify after that projection, otherwise the
/// exact active generation seats after its bounded proof expires and every
/// graph readiness probe keeps an unchanged-source Noop loop alive.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn long_text_projection_renews_source_before_seating_and_noop_follow_up_settles() {
    let fixture = GitFixture::new(ALPHA_LIB_V1);
    let store = TempDir::new().expect("store root");
    let registry = CodeIndexSchedulerRegistryV1::with_background_reconcile_permits(1, 1);
    let canonical_root = fixture.path().canonicalize().expect("canonical fixture");
    let (projection_started, release_projection) = registry
        .pause_next_published_text_projection(canonical_root)
        .await;
    registry
        .mount_worktree(
            test_project_id(),
            fixture.path(),
            store.path().to_path_buf(),
            None,
        )
        .await
        .expect("mount worktree");
    tokio::time::timeout(Duration::from_secs(10), projection_started)
        .await
        .expect("publication did not reach text projection")
        .expect("publication projection gate stays armed");

    let identity = super::super::identity::IndexingIdentityV1::resolve(fixture.path())
        .expect("mounted worktree identity");
    let scope = ResolvedScope::new(
        test_project_id(),
        identity.repository_id().clone(),
        identity.worktree_id().clone(),
        identity.head_ref().cloned(),
    )
    .expect("resolved scope");
    let source_freshness = registry
        .source_freshness_for_root(fixture.path())
        .await
        .expect("mounted source fence");
    {
        let mut state = source_freshness
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.last_reconciled_at = Instant::now()
            .checked_sub(state.staleness_threshold + Duration::from_secs(1))
            .expect("age the pre-projection proof");
    }
    release_projection
        .send(())
        .expect("release publication projection");

    let ready = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(ready) = registry
                .latest_complete_ready_decoded_for_root_scope(fixture.path(), &scope)
                .await
            {
                break ready;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .expect("post-projection source proof never admitted the exact active generation");
    let generation = ready.generation().manifest().generation_id.clone();

    // Exercise the ordinary expiry path too: one readiness request starts a
    // real Noop, and a read during that owner pass records one BusyFollowUp.
    // Both passes must settle because the existing seat keeps its exact
    // witness while the source proof is renewed.
    {
        let mut state = source_freshness
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.last_reconciled_at = Instant::now()
            .checked_sub(state.staleness_threshold + Duration::from_secs(1))
            .expect("age the seated proof");
    }
    registry.clear_pending_wake_for_scope(&scope).await;
    let receipts_before = registry.event_to_ready_receipts().len();
    assert!(
        registry
            .latest_complete_ready_decoded_for_root_scope(fixture.path(), &scope)
            .await
            .is_none(),
        "the expired proof declines before the worker renews it"
    );
    tokio::time::timeout(Duration::from_secs(10), async {
        while !registry
            .reconcile_in_progress_for_test(fixture.path())
            .await
        {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .expect("readiness did not start a source-verification pass");
    assert!(
        registry
            .latest_complete_ready_decoded_for_root_scope(fixture.path(), &scope)
            .await
            .is_none(),
        "readiness stays fail-closed while the Noop owns verification"
    );
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let receipts = registry.event_to_ready_receipts();
            let settled = !registry
                .reconcile_in_progress_for_test(fixture.path())
                .await
                && registry.pending_wake_micros_for_scope(&scope).await == Some(0);
            let new = &receipts[receipts_before.min(receipts.len())..];
            if settled
                && new.iter().any(|receipt| {
                    receipt.trigger == CodeIndexCadenceTriggerV1::BusyFollowUp && receipt.is_noop()
                })
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .expect("the real Noop and its single busy follow-up did not settle");
    assert_eq!(
        registry
            .latest_complete_ready_decoded_for_root_scope(fixture.path(), &scope)
            .await
            .expect("renewed seat is ready")
            .generation()
            .manifest()
            .generation_id,
        generation
    );

    fixture.edit("src/lib.rs", "pub fn changed_after_seat() {}\n");
    assert!(
        registry
            .notify_path(fixture.path(), fixture.path().join("src/lib.rs"))
            .await,
        "changed source reaches the mounted owner"
    );
    assert!(
        registry
            .latest_complete_ready_decoded_for_root_scope(fixture.path(), &scope)
            .await
            .is_none(),
        "a real source change still refuses the old seat"
    );

    registry.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn verified_empty_source_remains_observable_while_scheduler_is_busy() {
    let fixture = GitFixture::new(&[("assets/blob.bin", "not source\n")]);
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
    let identity = super::super::identity::IndexingIdentityV1::resolve(fixture.path())
        .expect("mounted worktree identity");
    let scope = ResolvedScope::new(
        test_project_id(),
        identity.repository_id().clone(),
        identity.worktree_id().clone(),
        identity.head_ref().cloned(),
    )
    .expect("resolved scope");
    tokio::time::timeout(Duration::from_secs(10), async {
        while !registry
            .reconciled_without_generation_for_scope(&scope)
            .await
        {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .expect("empty source becomes verified");

    let scheduler = registry
        .scheduler_handle(fixture.path())
        .await
        .expect("scheduler handle");
    let (locked_tx, locked_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let holder = std::thread::spawn(move || {
        let _guard = scheduler
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _ = locked_tx.send(());
        let _ = release_rx.recv();
    });
    locked_rx.await.expect("scheduler lock held");

    assert!(
        tokio::time::timeout(
            Duration::from_millis(100),
            registry.reconciled_without_generation_for_scope(&scope),
        )
        .await
        .expect("freshness probe does not wait for scheduler metadata"),
        "verified empty source is not reclassified as warming during a build"
    );

    release_tx.send(()).expect("release scheduler lock");
    holder.join().expect("scheduler holder joins");
    registry.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn background_worker_waits_for_global_admission_before_publication_gate() {
    let fixture = GitFixture::new(&[("src/main.rs", "fn main() {}\n")]);
    let store = TempDir::new().expect("store root");
    let registry = CodeIndexSchedulerRegistryV1::with_background_reconcile_permits(1, 0);
    registry
        .mount_worktree(
            test_project_id(),
            fixture.path(),
            store.path().to_path_buf(),
            None,
        )
        .await
        .expect("mount worktree");
    let publication_gate = {
        let mounted = registry.mounted.lock().await;
        Arc::clone(
            &mounted
                .get(&fixture.path().canonicalize().expect("canonical root"))
                .expect("mounted worktree")
                .semantic_evaluation_publication_gate,
        )
    };

    tokio::time::sleep(Duration::from_millis(25)).await;
    let publication = tokio::time::timeout(Duration::from_millis(100), publication_gate.lock())
        .await
        .expect("global admission wait must not hold the per-worktree publication gate");
    drop(publication);
    registry.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ignored_dependency_waits_for_global_admission_before_publication_gate() {
    struct ActiveControl;

    impl CodeIndexExecutionControlV1 for ActiveControl {
        fn is_cancelled(&self) -> bool {
            false
        }

        fn is_deadline_exceeded(&self) -> bool {
            false
        }
    }

    let fixture = GitFixture::new(&[
        (".gitignore", "node_modules/\n"),
        (
            "src/app.ts",
            "import type { PublicWidget } from \"pkg\";\nexport const anchor = 1;\n",
        ),
    ]);
    write(
        fixture.path(),
        "node_modules/pkg/index.d.ts",
        "export interface PublicWidget { value: string }\n",
    );
    let store = TempDir::new().expect("store root");
    let (registry, _) = mounted_core_query_worktree_with_one_permit(&fixture, &store).await;
    let latest = wait_for_live_complete_generation(&registry, fixture.path()).await;
    let generation = latest.generation();
    let verified_import = generation
        .imports()
        .iter()
        .find(|import| import.module_specifier == "pkg")
        .expect("verified package import")
        .clone();
    let snapshot = generation.snapshot();
    let scope = ResolvedScope::new(
        generation.manifest().project_id.clone(),
        snapshot.repository.clone(),
        snapshot.worktree.clone().expect("worktree identity"),
        snapshot.reference.clone(),
    )
    .expect("resolved scope");
    let request = CodeIndexIgnoredDependencyRequestV1 {
        scope: scope.clone(),
        expected_generation: generation.manifest().generation_id.clone(),
        verified_imports: vec![verified_import],
    };
    let publication_gate = {
        let mounted = registry.mounted.lock().await;
        Arc::clone(
            &mounted
                .get(&fixture.path().canonicalize().expect("canonical root"))
                .expect("mounted worktree")
                .semantic_evaluation_publication_gate,
        )
    };
    let global_admission = registry.background_reconcile_admission();
    registry.clear_pending_wake_for_scope(&scope).await;
    let publication = publication_gate.lock().await;
    assert_eq!(
        global_admission.available_permits(),
        1,
        "test setup requires an idle global admission"
    );

    let request_registry = registry.clone();
    let project_root = fixture.path().to_path_buf();
    let request_task = tokio::spawn(async move {
        request_registry
            .index_verified_ignored_dependency(&project_root, request, Arc::new(ActiveControl))
            .await
    });
    tokio::time::timeout(Duration::from_millis(100), async {
        while global_admission.available_permits() != 0 {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .expect("ignored dependency must acquire global admission before waiting on publication");
    drop(publication);
    request_task
        .await
        .expect("ignored-dependency task joins")
        .expect("ignored dependency publishes after admission");
    registry.shutdown().await;
}

/// Busy-read proof reuse is explicitly bounded, but the bound belongs to the
/// freshness fence, not to a per-read worktree sweep: an ordinary read never
/// walks the checkout. An out-of-band write that moves no Git metadata and
/// reaches no hint authority is therefore served from the live proof until
/// that proof expires. The first read after it does declines the seat and
/// hands the exact stat-plus-sealed-digest comparison to the retained worker,
/// whose pass is the only authority that may withdraw the witness from the
/// disproved generation.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_disproving_exact_source_probe_withdraws_the_busy_read_witness() {
    let fixture = GitFixture::new(&[("src/main.rs", "fn main() {}\n")]);
    let store = TempDir::new().expect("store root");
    let (registry, scope) = mounted_core_query_worktree_with_one_permit(&fixture, &store).await;

    let ready = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(ready) = registry
                .latest_complete_ready_decoded_for_root_scope(fixture.path(), &scope)
                .await
            {
                break ready;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("the mounted generation becomes ready-decoded");
    let disproved_generation_id = ready.generation().manifest().generation_id.clone();

    let witness = registry
        .serving_source_witness_for_root(fixture.path())
        .await
        .expect("mounted worktree witness");
    let source_freshness = registry
        .source_freshness_for_root(fixture.path())
        .await
        .expect("mounted worktree source fence");
    // Hold the worker at its dequeue point so every observation below is the
    // read path's own answer and never a pass that raced it.
    let admission = quiesced_background_reconcile_admission(&registry, fixture.path()).await;

    std::fs::write(
        fixture.path().join("src/main.rs"),
        "fn main() { drifted(); }\n",
    )
    .expect("drift the worktree");

    assert!(
        registry
            .latest_complete_ready_decoded_for_root_scope(fixture.path(), &scope)
            .await
            .is_some(),
        "an unhinted raw write reuses the live proof; a read never walks the checkout"
    );

    // Expire that proof exactly as its own bound does, without waiting it out.
    {
        let mut state = source_freshness
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.last_reconciled_at = Instant::now()
            .checked_sub(state.staleness_threshold + Duration::from_secs(1))
            .expect("age the source proof past its own bound");
    }
    registry.clear_pending_wake_for_scope(&scope).await;
    assert!(
        registry
            .latest_complete_ready_decoded_for_root_scope(fixture.path(), &scope)
            .await
            .is_none(),
        "an expired proof disproves the seated generation's currency"
    );
    assert!(
        registry
            .pending_wake_micros_for_scope(&scope)
            .await
            .is_some_and(|pending| pending != 0),
        "the declining read hands the exact source proof to the retained worker"
    );
    assert_eq!(
        witness
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .map(|witness| witness.generation_id.clone()),
        Some(disproved_generation_id.clone()),
        "a read that cannot verify source may not fabricate the disproof itself"
    );

    // Release the worker: its pass re-derives the sealed digests, observes the
    // drift, and the witness stops naming the disproved generation.
    drop(admission);
    let deadline = Instant::now() + SERVING_SEAT_FAILURE_CEILING;
    while witness
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_ref()
        .map(|witness| witness.generation_id.clone())
        == Some(disproved_generation_id.clone())
    {
        assert!(
            Instant::now() <= deadline,
            "the disproving reconcile pass never withdrew the busy-read witness"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    registry.shutdown().await;
}

/// The pointer-supersession half of the verified-read outage: a reconcile
/// pass publishes a successor generation and flips the durable pointer
/// minutes before the successor's O(store) decode + native activation seats
/// it. When that successor sealed the SAME source content — a convergence or
/// repair republication, not an edit — the seated predecessor still describes
/// exactly the bytes on disk, so verified reads must keep serving it through
/// the successor's activation window instead of refusing "not ready".
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_same_content_successor_pointer_keeps_the_seated_generation_serving() {
    let fixture = GitFixture::new(&[("src/main.rs", "fn main() {}\n")]);
    let store = TempDir::new().expect("store root");
    let (registry, scope) = mounted_core_query_worktree(&fixture, &store).await;

    let ready = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(ready) = registry
                .latest_complete_ready_decoded_for_root_scope(fixture.path(), &scope)
                .await
            {
                break ready;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("the mounted generation becomes ready-decoded");
    let generation_id = ready.generation().manifest().generation_id.clone();

    // Flip the durable pointer to an unseated successor sealed from the same
    // source content — the exact durable state between a convergence
    // republication's publish and its seat.
    advance_pointer_to_unseated_successor(
        &super::super::scoped_code_index_store_root(
            store.path(),
            &fixture
                .path()
                .canonicalize()
                .expect("canonical fixture root"),
        ),
        false,
    );

    let served = registry
        .latest_complete_ready_decoded_for_root_scope(fixture.path(), &scope)
        .await
        .expect("the same-content predecessor keeps serving while its successor activates");
    assert_eq!(
        served.generation().manifest().generation_id,
        generation_id,
        "the read serves the seated predecessor, not a phantom of the unseated successor"
    );
    assert!(
        registry.has_current_ready_decoded_for_root_scope(fixture.path(), &scope),
        "the readiness census reports the same-content predecessor through the activation window"
    );

    registry.shutdown().await;
}

/// Fail-closed half of pointer supersession: a successor sealed from
/// DIFFERENT source content means the seat lags the reconciled truth. The
/// read must refuse and withdraw the busy-read witness so neither the quiet
/// nor the busy path serves the stale seat.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_different_content_successor_pointer_refuses_the_stale_seat() {
    let fixture = GitFixture::new(&[("src/main.rs", "fn main() {}\n")]);
    let store = TempDir::new().expect("store root");
    let (registry, scope) = mounted_core_query_worktree(&fixture, &store).await;

    let ready = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(ready) = registry
                .latest_complete_ready_decoded_for_root_scope(fixture.path(), &scope)
                .await
            {
                break ready;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("the mounted generation becomes ready-decoded");
    let stale_generation_id = ready.generation().manifest().generation_id.clone();

    let witness = registry
        .serving_source_witness_for_root(fixture.path())
        .await
        .expect("mounted worktree witness");
    advance_pointer_to_unseated_successor(
        &super::super::scoped_code_index_store_root(
            store.path(),
            &fixture
                .path()
                .canonicalize()
                .expect("canonical fixture root"),
        ),
        true,
    );

    assert!(
        registry
            .latest_complete_ready_decoded_for_root_scope(fixture.path(), &scope)
            .await
            .is_none(),
        "a different-content successor pointer disproves the seat"
    );
    assert_ne!(
        witness
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .map(|witness| witness.generation_id.clone()),
        Some(stale_generation_id),
        "the disproving probe withdraws the busy-read witness"
    );

    registry.shutdown().await;
}

/// A graph publication `Conflict` is a lifecycle or compare-and-swap race
/// (a runtime mid-close/retire, a concurrent publisher, a superseded head),
/// never evidence about the sealed payload. Classifying it terminal turned
/// one race into a permanent outage: the seat pass gave up stale serving,
/// every later reconcile hit the same race, and the route answered
/// `generation_unverified` until the daemon restarted.
#[test]
fn graph_publication_conflict_re_arms_activation_instead_of_orphaning_serving() {
    use crate::code_index::graph_projection::CodeGraphProjectionError;

    assert!(
        super::super::CodeIndexSchedulerErrorV1::GraphProjection(
            tracedecay_graph_db::GraphDbError::conflict("test.publication_conflict").into()
        )
        .is_retryable_activation(),
        "a publication conflict leaves the sealed artifact intact and must retry with backoff"
    );
    assert!(
        super::super::CodeIndexSchedulerErrorV1::GraphProjection(
            CodeGraphProjectionError::Cancelled
        )
        .is_retryable_activation(),
        "cancellation mid-publication is typed and must resume from the journaled replay"
    );
    assert!(
        super::super::CodeIndexSchedulerErrorV1::GraphProjection(
            CodeGraphProjectionError::DeadlineExceeded
        )
        .is_retryable_activation(),
        "a deadline mid-publication is typed and must resume from the journaled replay"
    );
    assert!(
        !super::super::CodeIndexSchedulerErrorV1::GraphProjection(
            CodeGraphProjectionError::Corrupt("sealed payload mismatch".to_owned())
        )
        .is_retryable_activation(),
        "payload corruption stays terminal so reconcile can rebuild"
    );
}

/// A conflict verdict identical to the previous seat attempt's — same guard
/// site, same compared evidence, same sealed generation — is deterministic:
/// the sealed inputs are immutable, so replaying activation reproduces the
/// exact refusal forever. The seat loop must recognize the repeat and take
/// the terminal typed-refusal arm instead of looping at the backoff ceiling
/// (issue #765). Anything short of an exact repeat stays a retry: a first
/// conflict, a different guard site, different compared evidence, another
/// generation, or a non-conflict failure in between.
#[test]
fn repeated_identical_conflict_verdict_is_terminal_not_retryable() {
    use tracedecay_graph_db::GraphDbError;

    let generation = CodeGenerationId::new("gen.sealed-1").expect("generation id");
    let other_generation = CodeGenerationId::new("gen.sealed-2").expect("generation id");
    let conflict_error = |site: &'static str| {
        super::super::CodeIndexSchedulerErrorV1::GraphProjection(
            GraphDbError::conflict(site).into(),
        )
    };
    let context_of = |site: &'static str| {
        let error = conflict_error(site);
        error
            .activation_conflict_context()
            .expect("conflict error carries its context")
            .clone()
    };

    let error = conflict_error("publication.prepare.expected_prior_head");
    let prior = (
        generation.clone(),
        context_of("publication.prepare.expected_prior_head"),
    );

    assert!(
        super::super::registry::is_repeated_conflict_verdict(&error, &generation, Some(&prior)),
        "an identical verdict for the same generation is deterministic and terminal"
    );
    assert!(
        !super::super::registry::is_repeated_conflict_verdict(&error, &generation, None),
        "the first conflict retries; it may be a concurrent-publisher race"
    );
    assert!(
        !super::super::registry::is_repeated_conflict_verdict(
            &error,
            &other_generation,
            Some(&prior)
        ),
        "a different sealed generation is a fresh attempt, not a repeat"
    );
    let different_site = (
        generation.clone(),
        context_of("publication.complete.cas_prior_head"),
    );
    assert!(
        !super::super::registry::is_repeated_conflict_verdict(
            &error,
            &generation,
            Some(&different_site)
        ),
        "a different guard site is a different verdict"
    );
    let different_evidence = (
        generation.clone(),
        match GraphDbError::conflict_observed(
            "publication.prepare.expected_prior_head",
            "head seq 1",
            "head seq 2",
        ) {
            GraphDbError::Conflict { context } => context,
            _ => unreachable!("conflict constructor produces the conflict variant"),
        },
    );
    assert!(
        !super::super::registry::is_repeated_conflict_verdict(
            &error,
            &generation,
            Some(&different_evidence)
        ),
        "different compared evidence is a different verdict"
    );
    let non_conflict = super::super::CodeIndexSchedulerErrorV1::GraphProjection(
        crate::code_index::graph_projection::CodeGraphProjectionError::DeadlineExceeded,
    );
    assert!(
        !super::super::registry::is_repeated_conflict_verdict(
            &non_conflict,
            &generation,
            Some(&prior)
        ),
        "only a conflict verdict can repeat a conflict verdict"
    );
}

/// A first publication conflict is a concurrent-publisher race, not a
/// deterministic refusal. The seat loop must schedule exactly one retry and
/// seat the sealed generation when that retry succeeds (issue #765). A later
/// conflict at a different guard site stays retryable — only an identical
/// repeat is terminal.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn first_activation_conflict_retries_once_and_then_seats() {
    use tracedecay_graph_db::GraphDbError;

    let fixture = GitFixture::new(ALPHA_LIB_V1);
    let store = TempDir::new().expect("store root");
    let scoped_store = super::super::scoped_code_index_store_root(
        store.path(),
        &fixture.path().canonicalize().expect("canonical fixture"),
    );
    let (scope, worktree_id, sealed_generation_id) = {
        let mut scheduler = scheduler(
            &fixture,
            scoped_store,
            Arc::new(SharedCodeIndexBytePoolV1::default()),
        );
        published(scheduler.reconcile_now().expect("seed generation"));
        let latest = scheduler.latest_complete().expect("seeded generation");
        let snapshot = latest.generation.snapshot();
        let worktree_id = snapshot.worktree.clone().expect("worktree id");
        (
            ResolvedScope::new(
                test_project_id(),
                snapshot.repository.clone(),
                worktree_id.clone(),
                snapshot.reference.clone(),
            )
            .expect("resolved scope"),
            worktree_id,
            latest.generation.manifest().generation_id.clone(),
        )
    };

    let conflict_error = |site: &'static str| {
        super::super::CodeIndexSchedulerErrorV1::GraphProjection(
            GraphDbError::conflict(site).into(),
        )
    };
    let context_of = |site: &'static str| {
        conflict_error(site)
            .activation_conflict_context()
            .expect("conflict error carries its context")
            .clone()
    };
    let first = conflict_error("publication.prepare.expected_prior_head");
    let different_site = (
        sealed_generation_id.clone(),
        context_of("publication.complete.cas_prior_head"),
    );
    assert!(
        !super::super::registry::is_repeated_conflict_verdict(&first, &sealed_generation_id, None),
        "the first conflict retries; it may be a concurrent-publisher race"
    );
    assert!(
        !super::super::registry::is_repeated_conflict_verdict(
            &first,
            &sealed_generation_id,
            Some(&different_site)
        ),
        "a second different conflict is not classified terminal"
    );

    super::super::graph_activation::set_injected_activation_conflicts(&worktree_id, 1);
    let registry = CodeIndexSchedulerRegistryV1::with_background_reconcile_permits(1, 1);
    registry
        .mount_worktree(
            test_project_id(),
            fixture.path(),
            store.path().to_path_buf(),
            None,
        )
        .await
        .expect("mount retained generation");

    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let freshness = registry
            .dashboard_freshness(fixture.path())
            .await
            .expect("mounted dashboard freshness");
        if let Some(
            tracedecay_contracts::code_index_freshness::CodeGraphServingReadinessV1::Unavailable {
                ref reason,
            },
        ) = freshness.code_graph_serving
            && reason != "generation_unavailable"
        {
            panic!("first conflict became a terminal graph refusal: {reason}");
        }
        let seated = registry
            .latest_complete_serving_for_scope(&scope)
            .await
            .is_some_and(|latest| {
                latest.generation().manifest().generation_id == sealed_generation_id
                    && latest.code_graph_serving_readiness()
                        == tracedecay_contracts::code_index_freshness::CodeGraphServingReadinessV1::Ready
            });
        if seated
            && matches!(
                freshness.code_graph_serving,
                Some(
                    tracedecay_contracts::code_index_freshness::CodeGraphServingReadinessV1::Ready
                )
            )
        {
            break;
        }
        assert!(
            Instant::now() <= deadline,
            "the first-conflict retry did not seat the sealed generation: {freshness:?}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    assert_eq!(
        super::super::graph_activation::injected_activation_attempt_count(&worktree_id),
        2,
        "one injected conflict must schedule exactly one retry that then seats"
    );
    registry.shutdown().await;
}

/// The serving gates are relaxed on `reference` only. A different repository
/// or a different worktree is a different checkout identity and must stay
/// unservable, or an answer would be mis-attributed rather than merely old.
#[tokio::test]
async fn serving_arms_still_refuse_a_different_worktree_identity() {
    let fixture = GitFixture::new(&[("src/main.rs", "fn main() {}\n")]);
    let store = TempDir::new().expect("store root");
    let (registry, scope) = mounted_core_query_worktree(&fixture, &store).await;

    let foreign = ResolvedScope::new(
        scope.project_id.clone(),
        scope.repository_id.clone(),
        WorktreeId::new("worktree.some-other-checkout").expect("worktree id"),
        scope.reference.clone(),
    )
    .expect("foreign scope");
    assert!(
        registry
            .latest_complete_serving_for_scope(&foreign)
            .await
            .is_none(),
        "a different worktree identity never inherits a retained generation"
    );
    assert!(
        registry
            .latest_complete_fresh_for_scope(&foreign)
            .await
            .is_none(),
        "the callable-code ladder refuses a different worktree identity too"
    );
    assert!(
        registry
            .latest_complete_ready_for_scope(&foreign)
            .await
            .is_none(),
        "the ready gate refuses a different worktree identity"
    );
    assert!(
        registry
            .latest_complete_ready_decoded_for_root_scope(fixture.path(), &foreign)
            .await
            .is_none(),
        "graph reads and the census refuse a different worktree identity truthfully"
    );

    registry.shutdown().await;
}

#[tokio::test]
async fn foreign_serving_generation_replacement_rejects_stale_rollback_token() {
    let fixture = GitFixture::new(&[("src/main.rs", "fn main() {}\n")]);
    let store = TempDir::new().expect("store root");
    let (registry, _scope) = mounted_core_query_worktree(&fixture, &store).await;
    let original = registry
        .serving_code_scope(fixture.path())
        .await
        .and_then(|scope| scope.serving_generation)
        .expect("initial retained generation");
    let original_id = original.manifest().generation_id.clone();
    let ServingGenerationInstallationOutcomeV1::Installed(original_installation) = registry
        .install_exact_serving_generation(fixture.path(), &original)
        .await
    else {
        panic!("the initial serving generation must admit one exact owner")
    };

    fixture.edit(
        "src/main.rs",
        "fn main() { refreshed(); }\nfn refreshed() {}\n",
    );
    git(fixture.path(), &["add", "src/main.rs"]);
    git(
        fixture.path(),
        &["commit", "-qm", "refresh retained generation"],
    );
    assert!(
        registry
            .notify_path(fixture.path(), fixture.path().join("src/main.rs"))
            .await,
        "the mounted worktree must accept a refresh hint"
    );
    let newer = wait_for_generation_change(&registry, fixture.path(), &original_id).await;
    let serving_deadline = Instant::now() + Duration::from_secs(5);
    let newer_generation = loop {
        if let Some(generation) = registry
            .serving_code_scope(fixture.path())
            .await
            .and_then(|scope| scope.serving_generation)
            && generation.manifest().generation_id == newer
        {
            break generation;
        }
        assert!(
            Instant::now() <= serving_deadline,
            "foreign replacement must seat the newer serving generation"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    };

    assert_eq!(
        registry
            .retire_owned_serving_generation(fixture.path(), original_installation)
            .await,
        ServingGenerationRollbackOutcomeV1::NoMatch,
        "a foreign replacement must invalidate the original installation token"
    );
    assert_eq!(
        registry.latest_generation_id(fixture.path()).await,
        Some(newer.clone())
    );
    let ServingGenerationInstallationOutcomeV1::Installed(newer_installation) = registry
        .install_exact_serving_generation(fixture.path(), &newer_generation)
        .await
    else {
        panic!("the newer serving generation must admit a fresh owner")
    };
    assert_eq!(
        registry
            .retire_owned_serving_generation(fixture.path(), newer_installation)
            .await,
        ServingGenerationRollbackOutcomeV1::Cleared
    );
    assert!(
        registry
            .serving_code_scope(fixture.path())
            .await
            .and_then(|scope| scope.serving_generation)
            .is_none(),
        "the exact failed generation must be retired from the serving slot"
    );
    assert_ne!(
        registry.latest_generation_id(fixture.path()).await.as_ref(),
        Some(&newer),
        "a matching rollback token must also withdraw the text fallback latest_generation_id would keep serving"
    );

    registry.shutdown().await;
}

#[tokio::test]
async fn abandoned_serving_generation_installation_releases_the_exact_replay_claim() {
    let fixture = GitFixture::new(&[("src/main.rs", "fn main() {}\n")]);
    let store = TempDir::new().expect("store root");
    let (registry, _scope) = mounted_core_query_worktree(&fixture, &store).await;
    let generation = registry
        .serving_code_scope(fixture.path())
        .await
        .and_then(|scope| scope.serving_generation)
        .expect("initial retained generation");
    let generation_id = generation.manifest().generation_id.clone();
    let ServingGenerationInstallationOutcomeV1::Installed(abandoned) = registry
        .install_exact_serving_generation(fixture.path(), &generation)
        .await
    else {
        panic!("the initial serving generation must admit one exact owner")
    };

    drop(abandoned);

    let ServingGenerationInstallationOutcomeV1::Installed(replay) = registry
        .install_exact_serving_generation(fixture.path(), &generation)
        .await
    else {
        panic!("dropping an unfinished installation must release the replay claim")
    };
    assert_eq!(
        registry
            .commit_serving_generation_installation(fixture.path(), replay)
            .await,
        ServingGenerationRollbackOutcomeV1::Cleared
    );
    assert_eq!(
        registry.latest_generation_id(fixture.path()).await,
        Some(generation_id),
        "releasing an abandoned claim must never clear the serving generation"
    );

    registry.shutdown().await;
}

#[tokio::test]
async fn cancelled_serving_generation_installation_releases_the_exact_replay_claim() {
    let fixture = GitFixture::new(&[("src/main.rs", "fn main() {}\n")]);
    let store = TempDir::new().expect("store root");
    let (registry, _scope) = mounted_core_query_worktree(&fixture, &store).await;
    let generation = registry
        .serving_code_scope(fixture.path())
        .await
        .and_then(|scope| scope.serving_generation)
        .expect("initial retained generation");
    let generation_id = generation.manifest().generation_id.clone();
    let task_registry = registry.clone();
    let task_root = fixture.path().to_path_buf();
    let task_generation = Arc::clone(&generation);
    let (claimed, claimed_observed) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        let ServingGenerationInstallationOutcomeV1::Installed(_installation) = task_registry
            .install_exact_serving_generation(&task_root, &task_generation)
            .await
        else {
            panic!("the initial serving generation must admit one exact owner")
        };
        claimed.send(()).expect("publish exact installation claim");
        std::future::pending::<()>().await;
        drop(_installation);
    });
    claimed_observed
        .await
        .expect("installation task must hold the claim before cancellation");
    task.abort();
    let _ = task.await;

    let ServingGenerationInstallationOutcomeV1::Installed(replay) = registry
        .install_exact_serving_generation(fixture.path(), &generation)
        .await
    else {
        panic!("cancelling an installation task must release the exact replay claim")
    };
    assert_eq!(
        registry
            .commit_serving_generation_installation(fixture.path(), replay)
            .await,
        ServingGenerationRollbackOutcomeV1::Cleared
    );
    assert_eq!(
        registry.latest_generation_id(fixture.path()).await,
        Some(generation_id),
        "cancelling the claim owner must leave its serving generation intact"
    );

    registry.shutdown().await;
}

#[tokio::test]
async fn dashboard_progress_does_not_wait_for_the_scheduler_mutex() {
    let sources = (0..12)
        .map(|ordinal| {
            (
                format!("src/dashboard_{ordinal}.rs"),
                format!("pub fn dashboard_symbol_{ordinal}() -> usize {{ {ordinal} }}\n"),
            )
        })
        .collect::<Vec<_>>();
    let borrowed = sources
        .iter()
        .map(|(path, source)| (path.as_str(), source.as_str()))
        .collect::<Vec<_>>();
    let fixture = GitFixture::new(&borrowed);
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
    wait_for_initial_generation(&registry, fixture.path()).await;
    wait_for_dashboard_ready(&registry, fixture.path()).await;
    let canonical_root = fixture
        .path()
        .canonicalize()
        .expect("canonical fixture root");
    let (scheduler, progress_slot) = {
        let mounted = registry.mounted.lock().await;
        let worktree = mounted.get(&canonical_root).expect("mounted worktree");
        (
            Arc::clone(&worktree.scheduler),
            Arc::clone(&worktree.build_progress),
        )
    };
    let expected = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(progress) = progress_slot.read().expect("progress slot").snapshot() {
                break progress;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .expect("background text build publishes progress");

    let (locked_tx, locked_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let scheduler_holder = tokio::task::spawn_blocking(move || {
        let _scheduler_guard = scheduler.lock().expect("hold scheduler mutex");
        let _ = locked_tx.send(());
        let _ = release_rx.blocking_recv();
    });
    locked_rx.await.expect("scheduler mutex holder started");
    let projected = tokio::time::timeout(
        Duration::from_secs(1),
        registry.dashboard_freshness(fixture.path()),
    )
    .await
    .expect("dashboard projection must not wait for scheduler")
    .expect("mounted freshness projection");
    let projected_progress = projected.progress.expect("projected progress snapshot");
    assert_eq!(projected_progress.generation_id, expected.generation_id);
    assert!(projected_progress.progress_epoch >= expected.progress_epoch);
    assert_eq!(
        projected.staleness_state.as_deref(),
        Some("fresh"),
        "an unrelated scheduler-mutex holder is not a source refresh"
    );
    assert_eq!(projected.coverage, "complete");
    assert_eq!(projected.hook_hint_count, Some(0));
    let _ = release_tx.send(());
    scheduler_holder
        .await
        .expect("scheduler mutex holder joined");
    registry.shutdown().await;
}

// Two workers so the timeout timer stays live if a regression parks one
// runtime worker on the scheduler mutex: the test then fails instead of
// deadlocking against its own release channel.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn replay_binding_does_not_wait_for_the_scheduler_mutex() {
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
    let generation_id = wait_for_initial_generation(&registry, fixture.path()).await;
    let canonical_root = fixture
        .path()
        .canonicalize()
        .expect("canonical fixture root");
    let scheduler = {
        let mounted = registry.mounted.lock().await;
        Arc::clone(
            &mounted
                .get(&canonical_root)
                .expect("mounted worktree")
                .scheduler,
        )
    };
    // Model a background reconcile that owns the scheduler mutex for its
    // whole pass; the sealed replay binding must stay answerable through it.
    let (locked_tx, locked_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let scheduler_holder = tokio::task::spawn_blocking(move || {
        let _scheduler_guard = scheduler.lock().expect("hold scheduler mutex");
        let _ = locked_tx.send(());
        let _ = release_rx.blocking_recv();
    });
    locked_rx.await.expect("scheduler mutex holder started");
    // Spawn the probe so a regression parks only its own task: the timeout
    // then observes the parked join handle and fails cleanly instead of
    // hanging this test task inside the probe's poll.
    let probe_registry = registry.clone();
    let probe_root = fixture.path().to_path_buf();
    let probe_generation = generation_id.clone();
    let probe = tokio::spawn(async move {
        probe_registry
            .code_graph_replay_binding(&probe_root, &probe_generation)
            .await
    });
    let binding = tokio::time::timeout(Duration::from_secs(1), probe)
        .await
        .expect("replay binding must not wait for the scheduler mutex")
        .expect("replay binding probe task joined")
        .expect("mounted worktree resolves a replay binding")
        .expect("sealed replay binding");
    assert!(
        binding.generations_root.starts_with(store.path()),
        "replay binding must name this worktree's scoped generations root"
    );
    let _ = release_tx.send(());
    scheduler_holder
        .await
        .expect("scheduler mutex holder joined");
    registry.shutdown().await;
}

#[tokio::test]
async fn unchanged_background_freshness_probe_posts_no_overflow_wake() {
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
    wait_for_initial_generation(&registry, fixture.path()).await;
    // The seat is published mid-pass, so the mount's own reconcile receipt can
    // still be outstanding. Sample the baseline only once that pass is done,
    // or its receipt is charged to the probe below.
    wait_for_quiescent_owner_pass(&registry, fixture.path()).await;
    let canonical = fixture.path().canonicalize().expect("canonical fixture");
    {
        let mounted = registry.mounted.lock().await;
        let scheduler = &mounted.get(&canonical).expect("mounted worktree").scheduler;
        scheduler
            .lock()
            .expect("scheduler")
            .policy
            .staleness_threshold = Duration::ZERO;
    }
    let receipts_before = registry.event_to_ready_receipts().len();

    assert!(registry.probe_freshness(fixture.path()).await);
    tokio::time::sleep(Duration::from_millis(50)).await;

    let mounted = registry.mounted.lock().await;
    let scheduler = mounted.get(&canonical).expect("mounted worktree");
    assert_eq!(
        scheduler
            .scheduler
            .lock()
            .expect("scheduler")
            .pending_hint_count(),
        Some(0),
        "matching Git/stat evidence must not become an overflow hint"
    );
    assert_eq!(
        registry.event_to_ready_receipts().len(),
        receipts_before,
        "a suppressed probe must not fabricate a reconcile receipt"
    );
    drop(mounted);
    registry.shutdown().await;
}

#[tokio::test]
async fn diagnostics_change_generation_is_stable_until_a_sibling_edit_hint() {
    let fixture = GitFixture::new(&[
        ("src/lib.rs", "pub fn primary() {}\n"),
        ("src/sibling.rs", "pub fn sibling() {}\n"),
    ]);
    let store = TempDir::new().expect("store root");
    let (registry, scope) = mounted_core_query_worktree_with_one_permit(&fixture, &store).await;
    let admission = registry
        .background_reconcile_admission()
        .acquire_owned()
        .await
        .expect("hold background reconcile admission");
    registry.clear_pending_wake_for_scope(&scope).await;

    let first = registry
        .diagnostics_change_generation(fixture.path())
        .await
        .expect("mounted diagnostics generation");
    let unchanged = registry
        .diagnostics_change_generation(fixture.path())
        .await
        .expect("stable diagnostics generation");
    assert_eq!(unchanged, first);

    fixture.edit(
        "src/sibling.rs",
        "pub fn sibling() { println!(\"changed\"); }\n",
    );
    assert!(
        registry
            .notify_hook_paths(fixture.path(), &["src/sibling.rs".to_owned()])
            .await
    );
    let changed = registry
        .diagnostics_change_generation(fixture.path())
        .await
        .expect("changed diagnostics generation");
    assert!(changed > first);
    let still_pending = registry
        .diagnostics_change_generation(fixture.path())
        .await
        .expect("pending diagnostics generation");
    assert_eq!(
        still_pending, changed,
        "a coalesced pending reconcile must not mint another generation"
    );

    drop(admission);
    registry.shutdown().await;
}

#[tokio::test]
async fn diagnostics_change_generation_advances_for_out_of_band_git_drift() {
    let fixture = GitFixture::new(&[
        ("src/lib.rs", "pub fn primary() {}\n"),
        ("src/sibling.rs", "pub fn sibling() {}\n"),
    ]);
    let store = TempDir::new().expect("store root");
    let (registry, scope) = mounted_core_query_worktree_with_one_permit(&fixture, &store).await;
    let admission = registry
        .background_reconcile_admission()
        .acquire_owned()
        .await
        .expect("hold background reconcile admission");
    registry.clear_pending_wake_for_scope(&scope).await;

    let first = registry
        .diagnostics_change_generation(fixture.path())
        .await
        .expect("mounted diagnostics generation");
    fixture.edit(
        "src/sibling.rs",
        "pub fn sibling() { println!(\"out of band\"); }\n",
    );
    git(fixture.path(), &["add", "src/sibling.rs"]);
    git(fixture.path(), &["commit", "-qm", "out-of-band drift"]);

    let changed = registry
        .diagnostics_change_generation(fixture.path())
        .await
        .expect("drifted diagnostics generation");
    assert!(
        changed > first,
        "Git metadata drift must advance the canonical change generation"
    );
    let unchanged = registry
        .diagnostics_change_generation(fixture.path())
        .await
        .expect("stable drifted diagnostics generation");
    assert_eq!(
        unchanged, changed,
        "repeated reads of the same pending drift must stay stable"
    );

    drop(admission);
    wait_for_quiescent_owner_pass(&registry, fixture.path()).await;
    let reconciled = registry
        .diagnostics_change_generation(fixture.path())
        .await
        .expect("reconciled diagnostics generation");
    assert_eq!(
        reconciled, changed,
        "settling the observed drift must not mint another generation"
    );
    registry.shutdown().await;
}

#[tokio::test]
async fn elapsed_freshness_window_alone_does_not_make_dashboard_state_stale() {
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
    wait_for_initial_generation(&registry, fixture.path()).await;
    wait_for_dashboard_ready(&registry, fixture.path()).await;
    let canonical = fixture.path().canonicalize().expect("canonical fixture");
    {
        let mounted = registry.mounted.lock().await;
        mounted
            .get(&canonical)
            .expect("mounted worktree")
            .scheduler
            .lock()
            .expect("scheduler")
            .policy
            .staleness_threshold = Duration::ZERO;
    }

    let projected = registry
        .dashboard_freshness(fixture.path())
        .await
        .expect("dashboard freshness");
    assert_eq!(projected.staleness_state.as_deref(), Some("fresh"));
    assert_eq!(projected.coverage, "complete");
    registry.shutdown().await;
}

#[tokio::test]
async fn dashboard_freshness_reports_pending_rebuild_liveness() {
    let fixture = GitFixture::new(&[("src/main.rs", "fn main() {}\n")]);
    let store = TempDir::new().expect("store root");
    let registry = CodeIndexSchedulerRegistryV1::with_background_reconcile_permits(1, 1);
    registry
        .mount_worktree(
            test_project_id(),
            fixture.path(),
            store.path().to_path_buf(),
            None,
        )
        .await
        .expect("mount daemon-owned scheduler");
    wait_for_initial_generation(&registry, fixture.path()).await;
    wait_for_dashboard_ready(&registry, fixture.path()).await;

    let admission = registry
        .background_reconcile_admission()
        .acquire_owned()
        .await
        .expect("hold background reconcile admission");
    fixture.edit("src/main.rs", "fn main() { println!(\"changed\"); }\n");
    assert!(
        registry
            .notify_hook_paths(fixture.path(), &["src/main.rs".to_owned()])
            .await,
        "the source change must publish a pending scheduler wake"
    );
    let projected = registry
        .dashboard_freshness(fixture.path())
        .await
        .expect("dashboard freshness");

    assert!(
        projected.rebuild_in_flight,
        "a pending scheduler wake must keep stale serving typed as rebuilding"
    );
    drop(admission);
    registry.shutdown().await;
}

/// A running source-verification pass is not itself a reason to decline query
/// admission. What declines here is the freshness gate: the seat is servable
/// and its proof is unexpired, so the query already has its answer and there is
/// no remedy to schedule. The pass is held only to put the worktree in the
/// verifying state whose dashboard projection this also pins.
#[tokio::test]
async fn a_fresh_seat_declines_query_admission_during_source_verification() {
    let fixture = GitFixture::new(&[("src/main.rs", "fn main() {}\n")]);
    let store = TempDir::new().expect("store root");
    let (registry, scope) = mounted_core_query_worktree(&fixture, &store).await;
    wait_for_dashboard_ready(&registry, fixture.path()).await;
    registry.clear_pending_wake_for_scope(&scope).await;

    let pass = registry
        .hold_reconcile_pass_for_test(fixture.path())
        .await
        .expect("mounted reconcile owner");
    assert!(
        !registry.request_query_background_reconcile(&scope).await,
        "a servable seat under an unexpired proof is already the query's answer"
    );
    assert_eq!(
        registry.pending_wake_micros_for_scope(&scope).await,
        Some(0),
        "a declined admission must leave the pending wake unclaimed"
    );

    let projected = registry
        .dashboard_freshness(fixture.path())
        .await
        .expect("dashboard freshness");
    assert_eq!(projected.staleness_state.as_deref(), Some("verifying"));
    assert_eq!(projected.coverage, "partial_source_verification");
    assert!(!projected.rebuild_in_flight);

    drop(pass);
    registry.shutdown().await;
}

/// A dashboard status view reports the last execution-owned scheduler state; it
/// must not run the freshness ladder, wake a worker, or publish an out-of-band
/// source change merely because an operator opened the view.
#[tokio::test]
async fn dashboard_freshness_does_not_reconcile_an_out_of_band_change() {
    let fixture = GitFixture::new(&[("src/main.rs", "fn main() { println!(\"v1\"); }\n")]);
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
    let initial = wait_for_initial_generation(&registry, fixture.path()).await;

    fixture.edit("src/main.rs", "fn main() { println!(\"v2\"); }\n");
    git(fixture.path(), &["commit", "-qam", "out-of-band"]);

    let projected = registry
        .dashboard_freshness(fixture.path())
        .await
        .expect("mounted scheduler projection");

    assert_eq!(
        projected.latest_generation_id.as_deref(),
        Some(initial.as_str()),
        "a status read must not publish the changed source generation"
    );
    registry.shutdown().await;
}

#[test]
fn restart_restores_complete_generation_and_content_noop() {
    let fixture = GitFixture::new(&[(
        "src/lib.rs",
        "pub fn caller() { callee(); }\npub fn callee() {}\n",
    )]);
    let store = TempDir::new().expect("store root");
    let bytes = Arc::new(SharedCodeIndexBytePoolV1::default());
    let first = {
        let mut scheduler = scheduler(&fixture, store.path().to_path_buf(), Arc::clone(&bytes));
        published(scheduler.reconcile_now().expect("initial publish"))
    };

    let mut restarted = scheduler(&fixture, store.path().to_path_buf(), bytes);
    let restored = restarted
        .latest_complete()
        .expect("restart restores active generation");
    assert_eq!(
        restored.generation.manifest().generation_id,
        first.generation_id
    );
    match restarted.reconcile_now().expect("restart reconciliation") {
        CodeIndexReconcileOutcomeV1::Noop(evidence) => {
            assert_eq!(
                evidence.snapshot_content_identity,
                first.snapshot_content_identity
            );
        }
        CodeIndexReconcileOutcomeV1::Published(_) => {
            panic!("restart with identical content must not republish")
        }
    }
}

#[test]
fn restored_generation_abstains_and_schedules_background_truth() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn alpha() -> u32 { 1 }\n")]);
    let store = TempDir::new().expect("store root");
    let bytes = Arc::new(SharedCodeIndexBytePoolV1::default());
    {
        let mut scheduler = scheduler(&fixture, store.path().to_path_buf(), Arc::clone(&bytes));
        published(scheduler.reconcile_now().expect("initial publish"));
    }

    // Simulate a generation sealed WITHOUT a restore-time freshness witness (an
    // older daemon, or a witness that never landed). With no witness the restore
    // must fail closed: unproven bytes are not request-admissible until the
    // background worker reconciles against gix truth.
    std::fs::remove_file(store.path().join("freshness_witness.v1"))
        .expect("remove restore-time freshness witness");

    let mut restarted = scheduler(&fixture, store.path().to_path_buf(), bytes);
    assert!(
        restarted
            .latest_complete_ready_for_query()
            .expect("ready check")
            .is_none(),
        "restored bytes are not request-admissible before current truth is proven"
    );
    assert_eq!(
        restarted.pending_hint_count(),
        None,
        "the ready check schedules one overflow reconcile for the background worker"
    );
    let first_wake_epoch = restarted.epoch.load(std::sync::atomic::Ordering::Acquire);
    assert!(
        restarted
            .latest_complete_ready_for_query()
            .expect("repeat ready check")
            .is_none()
    );
    assert_eq!(
        restarted.epoch.load(std::sync::atomic::Ordering::Acquire),
        first_wake_epoch,
        "a repeated read wake must not cancel in-flight generation work"
    );
    assert_eq!(
        restarted.pending_hint_count(),
        None,
        "the retained overflow marker remains pending for the refreshed wake"
    );

    let outcome = restarted.reconcile_now().expect("background truth");
    assert!(matches!(outcome, CodeIndexReconcileOutcomeV1::Noop(_)));
    assert!(
        restarted
            .latest_complete_ready_for_query()
            .expect("ready check")
            .is_some(),
        "the unchanged restored generation becomes request-admissible after reconciliation"
    );
}

#[test]
fn unchanged_ready_query_refreshes_its_stat_witness_without_reconcile() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn alpha() -> u32 { 1 }\n")]);
    let store = TempDir::new().expect("store root");
    let mut scheduler = scheduler(
        &fixture,
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    );
    let published = published(scheduler.reconcile_now().expect("initial publish"));
    scheduler.policy.staleness_threshold = Duration::ZERO;

    // Ready admission abstains on elapsed without scanning (July 29
    // `perf(index): defer stale query probes`). The August 26 scan-and-admit
    // contract lives on the query/background ladder: a matching stat witness
    // resets the clock and must not overflow into a capture.
    assert!(
        !scheduler.request_fresh_for_query_background(),
        "elapsed time alone must not enqueue authoritative capture on an unchanged tree"
    );
    assert_eq!(
        scheduler.latest_complete().map(|latest| latest
            .generation
            .manifest()
            .generation_id
            .clone()),
        Some(published.generation_id),
        "the sealed generation stays current after a matching stat witness"
    );
    assert_eq!(
        scheduler.pending_hint_count(),
        Some(0),
        "a matching Git/stat witness must not enqueue authoritative capture"
    );
}

#[test]
fn exact_source_readiness_abstains_when_a_file_is_added_inside_freshness_window() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn alpha() -> u32 { 1 }\n")]);
    let store = TempDir::new().expect("store root");
    let mut scheduler = scheduler(
        &fixture,
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    );
    published(scheduler.reconcile_now().expect("initial publish"));
    assert!(
        scheduler
            .exact_source_is_ready()
            .expect("exact source readiness")
    );
    assert!(
        scheduler
            .latest_complete_with(GenerationDecodeAdmissionV1::AlreadyDecoded)
            .is_some()
    );

    fixture.edit("src/added.rs", "pub fn added() {}\n");

    assert!(
        !scheduler
            .exact_source_is_ready()
            .expect("exact source readiness after file add"),
        "workspace completeness must abstain before the added file is published"
    );
}

#[test]
fn unchanged_reopen_with_witness_skips_full_reconcile() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn alpha() -> u32 { 1 }\n")]);
    let store = TempDir::new().expect("store root");
    let bytes = Arc::new(SharedCodeIndexBytePoolV1::default());
    let baseline = {
        let mut scheduler = scheduler(&fixture, store.path().to_path_buf(), Arc::clone(&bytes));
        published(scheduler.reconcile_now().expect("initial publish"))
    };
    assert!(
        store.path().join("freshness_witness.v1").is_file(),
        "a successful reconcile persists the restore-time freshness witness"
    );

    // Reopen against the same store with the worktree unchanged. Foreground
    // open remains unverified and decode-free; the retained owner then uses the
    // witness to activate the exact sealed generation without a source read.
    let mut reopened = scheduler(&fixture, store.path().to_path_buf(), bytes);
    assert!(
        !reopened.verified_against_source(),
        "foreground open cannot claim freshness before background proof"
    );
    assert_eq!(
        reopened.sealed_decode_count(),
        0,
        "foreground open must not decode sealed bytes"
    );
    assert!(
        matches!(
            reopened
                .activate_or_reconcile()
                .expect("retained activation"),
            CodeIndexReconcileOutcomeV1::Noop(_)
        ),
        "the matching frontier activates the sealed generation without rebuilding"
    );
    assert!(
        reopened
            .ensure_fresh_for_query()
            .expect("freshness ladder runs")
            .is_none(),
        "background frontier verification establishes the ordinary freshness clocks"
    );
    let served = reopened
        .latest_complete_ready_for_query()
        .expect("ready check")
        .expect("witness-verified restore serves immediately");
    assert_eq!(
        served.generation.manifest().generation_id,
        baseline.generation_id,
        "the witness-verified reopen serves the sealed generation"
    );
}

#[test]
fn edited_reopen_forces_full_reconcile_when_witness_mismatches() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn alpha() -> u32 { 1 }\n")]);
    let store = TempDir::new().expect("store root");
    let bytes = Arc::new(SharedCodeIndexBytePoolV1::default());
    let baseline = {
        let mut scheduler = scheduler(&fixture, store.path().to_path_buf(), Arc::clone(&bytes));
        published(scheduler.reconcile_now().expect("initial publish"))
    };

    // A working-tree edit changes the tier-2 stat signature, so the witness no
    // longer matches. The reopen must fail closed and fully reconcile the change
    // rather than serve the now-stale sealed generation.
    fixture.edit("src/lib.rs", "pub fn alpha() -> u32 { 2 }\n");

    let mut reopened = scheduler(&fixture, store.path().to_path_buf(), bytes);
    assert!(
        !reopened.verified_against_source(),
        "a changed worktree must never be adopted as verified from a stale witness"
    );
    let outcome = reopened
        .ensure_fresh_for_query()
        .expect("freshness ladder runs")
        .expect("a witness mismatch forces a reconcile");
    assert_ne!(
        published(outcome).generation_id,
        baseline.generation_id,
        "the edited source is captured in a freshly published generation"
    );
}

/// Equal stat metadata is a negative cache, never proof of currency: a
/// same-length rewrite whose mtime is preserved leaves every
/// `(path, len, mtime)` tuple unchanged. The live freshness ladder must still
/// compare the current bytes against the generation's sealed file digests and
/// reconcile the change instead of serving the retained generation forever.
#[test]
fn same_length_rewrite_with_preserved_mtime_reconciles_on_the_live_ladder() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn alpha() -> u32 { 1 }\n")]);
    let store = TempDir::new().expect("store root");
    let mut scheduler = scheduler(
        &fixture,
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    );
    let baseline = published(scheduler.reconcile_now().expect("initial publish"));
    assert!(
        scheduler
            .exact_source_is_ready()
            .expect("exact source readiness")
    );
    let stat_before = scheduler
        .worktree_stat_signature()
        .expect("stat signature before rewrite");

    rewrite_preserving_stat(&fixture, "src/lib.rs", "pub fn alpha() -> u32 { 2 }\n");

    assert_eq!(
        scheduler
            .worktree_stat_signature()
            .expect("stat signature after rewrite"),
        stat_before,
        "the rewrite is invisible to stat metadata by construction"
    );
    assert!(
        !scheduler
            .exact_source_is_ready()
            .expect("exact source readiness after rewrite"),
        "equal length and mtime must not prove the retained generation current"
    );
    // Inside the bounded-staleness window the ladder trusts the last
    // reconcile by design; once it elapses, the probe must not let an equal
    // stat signature reset the clock over changed bytes.
    scheduler.policy.staleness_threshold = Duration::ZERO;
    assert!(
        scheduler.request_fresh_for_query_background(),
        "the query probe must schedule a reconcile for changed bytes under equal metadata"
    );
    let outcome = scheduler
        .ensure_fresh_for_query()
        .expect("freshness ladder runs")
        .expect("changed bytes under equal stat metadata must reconcile");
    assert_ne!(
        published(outcome).generation_id,
        baseline.generation_id,
        "the rewritten source is captured in a freshly published generation"
    );
    let served = served_lexical_texts(&scheduler, "fn alpha");
    assert!(!served.is_empty(), "the rewritten file is still served");
    assert!(
        served.iter().all(|text| text.contains("{ 2 }")),
        "the served generation carries the rewritten bytes: {served:?}"
    );
}

/// The retained-generation path across a daemon restart: the persisted
/// witness still matches Git metadata and the stat signature, but the bytes
/// on disk changed. The remount must verify content against the retained
/// generation's file digests, rebuild, and serve the new content.
#[tokio::test]
async fn restart_over_same_length_preserved_mtime_rewrite_rebuilds_the_retained_generation() {
    let fixture = GitFixture::new(ALPHA_LIB_V1);
    let store = TempDir::new().expect("store root");
    let first = CodeIndexSchedulerRegistryV1::new(1);
    first
        .mount_worktree(
            test_project_id(),
            fixture.path(),
            store.path().to_path_buf(),
            None,
        )
        .await
        .expect("mount worktree");
    let sealed = wait_for_live_complete_generation(&first, fixture.path()).await;
    let sealed_id = sealed.generation().manifest().generation_id.clone();
    first.shutdown().await;

    rewrite_preserving_stat(&fixture, "src/lib.rs", "pub fn alpha() -> u32 { 2 }\n");

    let restarted = CodeIndexSchedulerRegistryV1::new(1);
    restarted
        .mount_worktree(
            test_project_id(),
            fixture.path(),
            store.path().to_path_buf(),
            None,
        )
        .await
        .expect("remount worktree over the retained store");
    let rebuilt = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(latest) = restarted.latest_complete_fresh(fixture.path()).await
                && latest.generation().manifest().generation_id != sealed_id
            {
                break latest;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("a restart over changed bytes with equal stat metadata must rebuild");
    let served = rebuilt
        .lexical()
        .iter()
        .filter(|chunk| chunk.sanitized_text.as_str().contains("fn alpha"))
        .map(|chunk| chunk.sanitized_text.as_str().to_owned())
        .collect::<Vec<_>>();
    assert!(!served.is_empty(), "the rewritten file is still served");
    assert!(
        served.iter().all(|text| text.contains("{ 2 }")),
        "the restarted daemon serves the rewritten bytes: {served:?}"
    );
    restarted.shutdown().await;
}

/// An explicitly admitted ignored source joins the stat signature and the
/// generation's file manifest like any ordinary candidate, so its same-length
/// preserved-mtime rewrite must be caught by the same content comparison.
#[test]
fn same_length_rewrite_of_an_admitted_ignored_source_reconciles() {
    let fixture = GitFixture::new(&[
        (".gitignore", "node_modules/\n"),
        (
            "src/app.ts",
            "import type { PublicWidget } from \"pkg\";\nexport const anchor = 1;\n",
        ),
    ]);
    write(
        fixture.path(),
        "node_modules/pkg/index.d.ts",
        "export interface PublicWidget { value: string }\n",
    );
    let store = TempDir::new().expect("store root");
    let mut scheduler = scheduler(
        &fixture,
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    );
    published(scheduler.reconcile_now().expect("initial publish"));
    let serving = scheduler.latest_complete().expect("serving generation");
    let generation = serving.generation();
    let verified_import = generation
        .imports()
        .iter()
        .find(|import| import.module_specifier == "pkg")
        .expect("verified package import")
        .clone();
    let snapshot = generation.snapshot();
    let scope = ResolvedScope::new(
        generation.manifest().project_id.clone(),
        snapshot.repository.clone(),
        snapshot.worktree.clone().expect("worktree identity"),
        snapshot.reference.clone(),
    )
    .expect("resolved scope");
    let admitted = scheduler
        .index_verified_ignored_dependency(
            &serving,
            CodeIndexIgnoredDependencyRequestV1 {
                scope,
                expected_generation: generation.manifest().generation_id.clone(),
                verified_imports: vec![verified_import],
            },
            &UninterruptibleCodeIndexControlV1,
        )
        .expect("admit the verified ignored dependency");
    assert_eq!(
        admitted.outcome.admission.logical_path,
        "node_modules/pkg/index.d.ts"
    );
    assert!(
        scheduler
            .exact_source_is_ready()
            .expect("exact source readiness")
    );
    let stat_before = scheduler
        .worktree_stat_signature()
        .expect("stat signature before rewrite");

    rewrite_preserving_stat(
        &fixture,
        "node_modules/pkg/index.d.ts",
        "export interface PublicWidget { value: number }\n",
    );

    assert_eq!(
        scheduler
            .worktree_stat_signature()
            .expect("stat signature after rewrite"),
        stat_before,
        "the admitted source rewrite is invisible to stat metadata by construction"
    );
    assert!(
        !scheduler
            .exact_source_is_ready()
            .expect("exact source readiness after rewrite"),
        "equal length and mtime must not prove the admitted ignored source current"
    );
    scheduler.policy.staleness_threshold = Duration::ZERO;
    let outcome = scheduler
        .ensure_fresh_for_query()
        .expect("freshness ladder runs")
        .expect("changed admitted bytes under equal stat metadata must reconcile");
    assert_ne!(
        published(outcome).generation_id,
        admitted.outcome.generation_id,
        "the rewritten admitted source is captured in a freshly published generation"
    );
    let served = served_lexical_texts(&scheduler, "PublicWidget");
    assert!(!served.is_empty(), "the admitted source is still served");
    assert!(
        served.iter().any(|text| text.contains("value: number")),
        "the served generation carries the rewritten admitted bytes: {served:?}"
    );
    assert!(
        served.iter().all(|text| !text.contains("value: string")),
        "the stale admitted bytes are no longer served: {served:?}"
    );
}

/// Swapping the contents of two same-length files and handing each its own
/// mtime back leaves the aggregate stat signature byte-identical, while every
/// per-file content digest moved. Only the file manifest comparison can tell.
#[test]
fn swapping_two_same_length_files_with_preserved_mtimes_reconciles() {
    let alpha = "pub fn alpha() -> u32 { 1 }\n";
    let bravo = "pub fn bravo() -> u32 { 2 }\n";
    assert_eq!(
        alpha.len(),
        bravo.len(),
        "the swap must preserve both lengths"
    );
    let fixture = GitFixture::new(&[("src/alpha.rs", alpha), ("src/bravo.rs", bravo)]);
    let store = TempDir::new().expect("store root");
    let mut scheduler = scheduler(
        &fixture,
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    );
    let baseline = published(scheduler.reconcile_now().expect("initial publish"));
    let stat_before = scheduler
        .worktree_stat_signature()
        .expect("stat signature before swap");

    rewrite_preserving_stat(&fixture, "src/alpha.rs", bravo);
    rewrite_preserving_stat(&fixture, "src/bravo.rs", alpha);

    assert_eq!(
        scheduler
            .worktree_stat_signature()
            .expect("stat signature after swap"),
        stat_before,
        "the swap leaves the aggregate stat signature unchanged by construction"
    );
    assert!(
        !scheduler
            .exact_source_is_ready()
            .expect("exact source readiness after swap"),
        "an unchanged aggregate stat signature must not prove the swapped files current"
    );
    scheduler.policy.staleness_threshold = Duration::ZERO;
    let outcome = scheduler
        .ensure_fresh_for_query()
        .expect("freshness ladder runs")
        .expect("swapped bytes under an equal aggregate stat signature must reconcile");
    assert_ne!(
        published(outcome).generation_id,
        baseline.generation_id,
        "the swapped sources are captured in a freshly published generation"
    );
    let latest = scheduler.latest_complete().expect("served generation");
    let path_of = |needle: &str| {
        let chunk = latest
            .lexical()
            .iter()
            .find(|chunk| chunk.sanitized_text.as_str().contains(needle))
            .unwrap_or_else(|| panic!("served chunk containing {needle}"));
        latest
            .generation()
            .snapshot()
            .files
            .iter()
            .find(|file| file.file_occurrence_id == chunk.anchor.file_occurrence_id)
            .map(|file| file.logical_path.clone())
            .expect("served chunk names a snapshot file")
    };
    assert_eq!(
        path_of("fn bravo"),
        "src/alpha.rs",
        "alpha.rs now carries bravo's bytes"
    );
    assert_eq!(
        path_of("fn alpha"),
        "src/bravo.rs",
        "bravo.rs now carries alpha's bytes"
    );
}

/// A checkout whose clean filters separate the bytes on disk from HEAD's blobs
/// (`core.autocrlf=true` over CRLF files) seals LF blob digests from the exact
/// HEAD tree. The content comparison must recognise the unchanged checkout as
/// current through the repository's own filter pipeline — never looping into a
/// reconcile every staleness window — while a same-length preserved-mtime
/// rewrite of the same file is still disproved.
#[test]
fn clean_filtered_checkout_verifies_current_and_still_disproves_a_rewrite() {
    let crlf_source = "pub fn alpha() -> u32 { 1 }\r\n";
    let fixture = GitFixture::build_fresh(&[("src/lib.rs", crlf_source)]);
    git(fixture.path(), &["config", "core.autocrlf", "true"]);
    git(fixture.path(), &["add", "--renormalize", "."]);
    git(fixture.path(), &["commit", "-qm", "normalise line endings"]);
    assert_eq!(
        std::fs::read(fixture.path().join("src/lib.rs")).expect("checkout bytes"),
        crlf_source.as_bytes(),
        "the checkout keeps CRLF while HEAD holds the normalised blob"
    );
    let store = TempDir::new().expect("store root");
    let mut scheduler = scheduler(
        &fixture,
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    );
    let baseline = published(scheduler.reconcile_now().expect("initial publish"));
    let sealed_digest = scheduler
        .latest_complete()
        .expect("served generation")
        .generation()
        .snapshot()
        .files
        .iter()
        .find(|file| file.logical_path == "src/lib.rs")
        .expect("sealed lib.rs")
        .content_digest
        .clone();
    assert_eq!(
        sealed_digest,
        content_digest(b"pub fn alpha() -> u32 { 1 }\n"),
        "the clean tree seals HEAD's LF blob, not the CRLF checkout bytes"
    );

    assert!(
        scheduler
            .exact_source_is_ready()
            .expect("exact source readiness"),
        "an unchanged filtered checkout is current once its bytes pass the clean filters"
    );
    scheduler.policy.staleness_threshold = Duration::ZERO;
    assert!(
        !scheduler.request_fresh_for_query_background(),
        "an unchanged filtered checkout must not reconcile on every elapsed window"
    );

    rewrite_preserving_stat(&fixture, "src/lib.rs", "pub fn alpha() -> u32 { 2 }\r\n");

    assert!(
        !scheduler
            .exact_source_is_ready()
            .expect("exact source readiness after rewrite"),
        "the clean filters must not hide a real rewrite under equal metadata"
    );
    let outcome = scheduler
        .ensure_fresh_for_query()
        .expect("freshness ladder runs")
        .expect("changed bytes under equal stat metadata must reconcile");
    assert_ne!(published(outcome).generation_id, baseline.generation_id);
    let served = served_lexical_texts(&scheduler, "fn alpha");
    assert!(!served.is_empty(), "the rewritten file is still served");
    assert!(
        served.iter().all(|text| text.contains("{ 2 }")),
        "the served generation carries the rewritten bytes: {served:?}"
    );
}

/// A slow freshness reconcile in one worktree must not serialize another
/// worktree's query. `latest_complete_fresh` clones the per-worktree handle
/// under a short map lock and drops the registry guard before reconciling, so
/// holding one scheduler's lock never blocks the registry map for others.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn worktree_queries_do_not_serialize_on_slow_reconcile() {
    let slow = GitFixture::new(&[("src/lib.rs", "pub fn slow() -> u32 { 1 }\n")]);
    let fast = GitFixture::new(&[("src/lib.rs", "pub fn fast() -> u32 { 2 }\n")]);
    let store = TempDir::new().expect("store root");
    let registry = CodeIndexSchedulerRegistryV1::new(2);
    for fixture in [&slow, &fast] {
        assert!(
            registry
                .mount_worktree(
                    test_project_id(),
                    fixture.path(),
                    store.path().to_path_buf(),
                    None,
                )
                .await
                .expect("mount worktree")
        );
    }
    // Let both workers seat their complete serving generation so neither
    // scheduler is mid-reconcile when the test grabs a lock. Publication now
    // broadcasts before graph seating; `latest_complete_fresh` needs the seat.
    for fixture in [&slow, &fast] {
        let _ = wait_for_live_complete_generation(&registry, fixture.path()).await;
    }

    // Hold the slow worktree's scheduler lock on a dedicated thread to model a
    // long in-flight reconcile that cannot complete until the test releases it.
    let slow_handle = registry
        .scheduler_handle(slow.path())
        .await
        .expect("slow scheduler handle");
    let (held_tx, held_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let lock_thread = std::thread::spawn(move || {
        let _guard = slow_handle
            .lock()
            .unwrap_or_else(|_| panic!("slow scheduler lock"));
        held_tx.send(()).expect("signal slow lock held");
        let _ = release_rx.recv();
    });
    held_rx.recv().expect("slow scheduler lock acquired");

    // A freshness query on the slow worktree now blocks on its scheduler lock.
    // Under the old design it would hold the registry map lock while blocked,
    // starving every other worktree's query.
    let slow_registry = registry.clone();
    let slow_path = slow.path().to_path_buf();
    let slow_query =
        tokio::spawn(async move { slow_registry.latest_complete_fresh(&slow_path).await });
    // Let the slow query enter its blocking reconcile section (acquire and drop
    // the map lock, then park on the scheduler lock) before the fast query runs.
    tokio::time::sleep(Duration::from_millis(100)).await;

    // The fast worktree's query must complete within a bounded time even while
    // the slow worktree's reconcile is stuck holding its scheduler lock.
    let fast_result = tokio::time::timeout(
        Duration::from_secs(2),
        registry.latest_complete_fresh(fast.path()),
    )
    .await
    .expect("fast worktree query is not serialized behind the slow reconcile");
    assert!(
        fast_result.is_some(),
        "fast worktree serves its generation while the slow worktree reconcile is in flight"
    );

    // Release the slow lock and let its query drain before shutting down.
    release_tx.send(()).expect("release slow lock");
    lock_thread.join().expect("slow lock thread joins");
    let _ = slow_query.await;
    registry.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn busy_worktree_serves_last_complete_generation_without_waiting() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn ready() -> u32 { 1 }\n")]);
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
    let expected = wait_for_live_complete_generation(&registry, fixture.path())
        .await
        .generation
        .manifest()
        .generation_id
        .clone();
    let scheduler = registry
        .scheduler_handle(fixture.path())
        .await
        .expect("scheduler handle");
    let (held_tx, held_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let lock_thread = std::thread::spawn(move || {
        let _guard = scheduler
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        held_tx.send(()).expect("signal scheduler lock held");
        let _ = release_rx.recv();
    });
    held_rx.recv().expect("scheduler lock acquired");

    let result = tokio::time::timeout(
        Duration::from_millis(250),
        registry.latest_complete_fresh(fixture.path()),
    )
    .await;
    release_tx.send(()).expect("release scheduler lock");
    lock_thread.join().expect("scheduler lock thread joins");

    let latest = result
        .expect("foreground query must not wait for an in-flight refresh")
        .expect("last complete generation remains queryable");
    assert_eq!(
        latest.generation.manifest().generation_id,
        expected,
        "foreground query must preserve the unchanged complete generation"
    );
    registry.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_releases_indexed_generation_and_scheduler_owners() {
    #[cfg(feature = "hotpath")]
    let _measurement = hotpath::HotpathGuardBuilder::new("indexed-registry-shutdown").build();
    let sources = (0..128)
        .map(|file| {
            let source = (0..16).fold(String::new(), |mut source, symbol| {
                let _ = writeln!(
                    source,
                    "pub fn item_{file}_{symbol}() -> u32 {{ {symbol} }}"
                );
                source
            });
            (format!("src/module_{file}.rs"), source)
        })
        .collect::<Vec<_>>();
    let files = sources
        .iter()
        .map(|(path, source)| (path.as_str(), source.as_str()))
        .collect::<Vec<_>>();
    let fixture = GitFixture::new(&files);
    let store = TempDir::new().expect("store root");
    let registry = CodeIndexSchedulerRegistryV1::new(2);
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
    assert!(latest.generation.symbols().symbols.len() >= 128 * 16);
    let generation = Arc::downgrade(&latest.generation);
    drop(latest);
    let scheduler = registry
        .scheduler_handle(fixture.path())
        .await
        .expect("mounted scheduler");
    let scheduler_owner = Arc::downgrade(&scheduler);
    drop(scheduler);

    let started = Instant::now();
    registry.shutdown().await;
    println!(
        "indexed_registry_shutdown_elapsed_us={}",
        started.elapsed().as_micros()
    );
    assert!(
        scheduler_owner.upgrade().is_none(),
        "scheduler was not released"
    );
    assert!(
        generation.upgrade().is_none(),
        "decoded generation was not released"
    );
    assert!(registry.scheduler_handle(fixture.path()).await.is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_signals_code_index_worker_without_taking_busy_scheduler_lock() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn busy() -> u32 { 1 }\n")]);
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
    // Let the mount-time reconcile finish first. Until it does, the background
    // worker owns the scheduler lock itself, and shutdown joining a worker that
    // is *already* blocked acquiring that lock is a different wait than the one
    // under test — this test is about shutdown never taking the lock on its own
    // behalf.
    wait_for_live_complete_generation(&registry, fixture.path()).await;
    let scheduler = registry
        .scheduler_handle(fixture.path())
        .await
        .expect("scheduler handle");
    let (held_tx, held_rx) = std::sync::mpsc::channel();
    let lock_thread = std::thread::spawn(move || {
        let _guard = scheduler
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        held_tx.send(()).expect("signal scheduler lock held");
        std::thread::sleep(Duration::from_millis(750));
    });
    held_rx.recv().expect("scheduler lock acquired");

    let started = std::time::Instant::now();
    registry.shutdown().await;
    let elapsed = started.elapsed();
    lock_thread.join().expect("scheduler lock holder joins");

    assert!(
        elapsed < Duration::from_millis(250),
        "shutdown waited {elapsed:?} for a synchronous scheduler lock instead of signalling its cooperative cancellation token"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shutdown_timeout_retains_blocked_worker_owner_until_retry_joins_it() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn busy() -> u32 { 1 }\n")]);
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
    wait_for_initial_generation(&registry, fixture.path()).await;
    let scheduler = registry
        .scheduler_handle(fixture.path())
        .await
        .expect("scheduler handle");
    struct ResumeOnDrop(Arc<super::super::reconcile_panic_guard::ReconcileFaultInjectionV1>);
    impl Drop for ResumeOnDrop {
        fn drop(&mut self) {
            self.0.resume();
        }
    }

    let admitted =
        Arc::new(super::super::reconcile_panic_guard::ReconcileFaultInjectionV1::paused());
    let release = ResumeOnDrop(Arc::clone(&admitted));
    let wake = {
        let mut scheduler = scheduler
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        scheduler.install_reconcile_fault_for_test(Arc::clone(&admitted));
        Arc::clone(&scheduler.wake)
    };
    fixture.edit("src/lib.rs", "pub fn busy() -> u32 { 2 }\n");
    wake.notify_one();
    tokio::time::timeout(Duration::from_secs(2), async {
        while admitted.attempts() == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("worker admits a reconcile pass before retirement");
    let drained = tokio::time::timeout(Duration::from_millis(25), registry.shutdown()).await;
    let retained = registry.retiring_owner_count().await;
    drop(release);
    assert!(drained.is_err(), "blocked writer must report settling");
    assert_eq!(retained, 1);
    assert!(
        tokio::time::timeout(Duration::from_secs(2), registry.shutdown())
            .await
            .is_ok(),
        "retry must join the retained owner"
    );
    assert_eq!(registry.retiring_owner_count().await, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn project_retirement_retains_blocked_worker_owner_until_retry_joins_it() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn busy() -> u32 { 1 }\n")]);
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
    wait_for_initial_generation(&registry, fixture.path()).await;
    let scheduler = registry
        .scheduler_handle(fixture.path())
        .await
        .expect("scheduler handle");
    struct ResumeOnDrop(Arc<super::super::reconcile_panic_guard::ReconcileFaultInjectionV1>);
    impl Drop for ResumeOnDrop {
        fn drop(&mut self) {
            self.0.resume();
        }
    }

    let admitted =
        Arc::new(super::super::reconcile_panic_guard::ReconcileFaultInjectionV1::paused());
    let release = ResumeOnDrop(Arc::clone(&admitted));
    let wake = {
        let mut scheduler = scheduler
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        scheduler.install_reconcile_fault_for_test(Arc::clone(&admitted));
        Arc::clone(&scheduler.wake)
    };
    fixture.edit("src/lib.rs", "pub fn busy() -> u32 { 2 }\n");
    wake.notify_one();
    tokio::time::timeout(Duration::from_secs(2), async {
        while admitted.attempts() == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("worker admits a reconcile pass before retirement");
    let roots = [fixture.path().canonicalize().expect("canonical root")]
        .into_iter()
        .collect();

    let drained = registry
        .retire_project_roots_with_deadline(&roots, Duration::from_millis(25))
        .await;
    let retained = registry.retiring_owner_count().await;
    drop(release);
    assert!(!drained, "blocked writer must report settling");
    assert_eq!(retained, 1);
    assert!(
        registry
            .retire_project_roots_with_deadline(&roots, Duration::from_secs(2))
            .await,
        "retry must join the retained owner"
    );
    assert_eq!(registry.retiring_owner_count().await, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn simultaneous_cold_mounts_admit_exactly_one_worktree_owner() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn race() -> u32 { 1 }\n")]);
    let store = TempDir::new().expect("store root");
    // Keep both newly-created workers parked at their dequeue point. The test
    // counts mount admissions before either worker can coalesce its initial
    // wake, so a duplicate cold owner cannot hide behind worker scheduling.
    let registry = CodeIndexSchedulerRegistryV1::with_background_reconcile_permits(2, 0);
    let callers = 2;
    registry.install_cold_mount_admission_barrier(fixture.path(), callers);
    let start = Arc::new(tokio::sync::Barrier::new(callers + 1));
    let mounts = (0..callers)
        .map(|_| {
            let registry = registry.clone();
            let project_root = fixture.path().to_path_buf();
            let store_root = store.path().to_path_buf();
            let start = Arc::clone(&start);
            tokio::spawn(async move {
                start.wait().await;
                registry
                    .mount_worktree(test_project_id(), &project_root, store_root, None)
                    .await
                    .expect("cold mount")
            })
        })
        .collect::<Vec<_>>();
    start.wait().await;

    let mut created_owners = 0;
    for mount in mounts {
        if mount.await.expect("mount task joins") {
            created_owners += 1;
        }
    }
    let mounted_worktrees = registry.memory_stats().await.mounted_worktrees;

    // Close the shared admission and join the retained owner before asserting,
    // so a failing run cannot leave a detached worker behind the test.
    registry.shutdown().await;

    assert_eq!(
        created_owners, 1,
        "one root must admit one cold owner; a second true result creates a detached runtime"
    );
    assert_eq!(
        mounted_worktrees, 1,
        "the retained mount table must contain the one owner that was admitted"
    );
}

// Each caller begins from the same empty pending slot. Holding the scheduler
// lock forces every request onto the BusyFollowUp path, where the registry—not
// the worker's later wake coalescing—is solely responsible for one admission.
#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn concurrent_query_admissions_claim_one_pending_wake_before_worker_coalescing() {
    let fixture = GitFixture::new(&[("src/main.rs", "fn main() {}\n")]);
    let store = TempDir::new().expect("store root");
    let (registry, scope) = mounted_core_query_worktree_with_one_permit(&fixture, &store).await;
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
    registry.clear_serving_generation_for_scope(&scope).await;
    registry.clear_pending_wake_for_scope(&scope).await;
    let held = scheduler
        .lock()
        .expect("hold the scheduler as a rebuild would");

    let callers = 8;
    registry.install_query_admission_barrier(&scope, callers);
    let start = Arc::new(tokio::sync::Barrier::new(callers + 1));
    let requests = (0..callers)
        .map(|_| {
            let registry = registry.clone();
            let scope = scope.clone();
            let start = Arc::clone(&start);
            tokio::spawn(async move {
                start.wait().await;
                registry.request_query_background_reconcile(&scope).await
            })
        })
        .collect::<Vec<_>>();
    start.wait().await;

    let mut admitted = 0;
    for request in requests {
        if request.await.expect("query admission task joins") {
            admitted += 1;
        }
    }
    let stamped = registry
        .pending_wake_micros_for_scope(&scope)
        .await
        .expect("mounted worktree");

    drop(held);
    registry.shutdown().await;
    drop(admission);

    assert_ne!(
        stamped, 0,
        "the one winning admission records its pending wake"
    );
    assert_eq!(
        admitted, 1,
        "the registry must atomically claim one query admission before worker wake coalescing"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancelled_cold_mount_holds_its_reservation_until_blocking_open_finishes() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn cancel() {}\n")]);
    let store = TempDir::new().expect("store root");
    let registry = CodeIndexSchedulerRegistryV1::with_background_reconcile_permits(1, 0);
    registry.install_cold_mount_open_gate(fixture.path());

    let leader = {
        let registry = registry.clone();
        let project_root = fixture.path().to_path_buf();
        let store_root = store.path().to_path_buf();
        tokio::spawn(async move {
            registry
                .mount_worktree(test_project_id(), &project_root, store_root, None)
                .await
        })
    };
    registry
        .wait_for_cold_mount_open_events(fixture.path(), 1)
        .await;

    let follower = {
        let registry = registry.clone();
        let project_root = fixture.path().to_path_buf();
        let store_root = store.path().to_path_buf();
        tokio::spawn(async move {
            registry
                .mount_worktree(test_project_id(), &project_root, store_root, None)
                .await
        })
    };
    registry.wait_for_cold_mount_follower(fixture.path()).await;
    leader.abort();
    assert!(
        leader.await.is_err(),
        "caller cancellation joins as cancelled"
    );

    registry.release_cold_mount_open_gate(fixture.path());
    assert!(
        follower
            .await
            .expect("follower mount task joins")
            .expect("follower retries after the detached open settles"),
        "the follower becomes the one canonical owner after the cancelled caller's open ends"
    );
    let events = registry.cold_mount_open_events(fixture.path());
    let first_finished = events
        .iter()
        .position(|event| *event == ColdMountOpenEventV1::Finished)
        .expect("first blocking open finished");
    let second_started = events
        .iter()
        .enumerate()
        .filter(|(_, event)| **event == ColdMountOpenEventV1::Started)
        .nth(1)
        .map(|(index, _)| index)
        .expect("retry opens after the detached open settles");

    registry.shutdown().await;

    assert!(
        first_finished < second_started,
        "a cancelled caller must retain its reservation until its detached blocking open has finished"
    );
}

#[tokio::test]
async fn failed_cold_mount_releases_its_reservation_for_retry() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn retry() {}\n")]);
    let registry = CodeIndexSchedulerRegistryV1::new(1);
    let bad_store_root = TempDir::new().expect("bad store root");
    let bad_store = bad_store_root.path().join("not-a-directory");
    std::fs::write(&bad_store, "not a directory").expect("write failing store path");

    assert!(
        registry
            .mount_worktree(test_project_id(), fixture.path(), bad_store, None)
            .await
            .is_err(),
        "the first cold open fails through the typed scheduler error"
    );
    let retry_store = TempDir::new().expect("retry store root");
    assert!(
        registry
            .mount_worktree(
                test_project_id(),
                fixture.path(),
                retry_store.path().to_path_buf(),
                None,
            )
            .await
            .expect("failed cold mount must release its exact reservation"),
        "retry owns the root after the failed open finishes"
    );

    registry.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn distinct_cold_mounts_respect_capacity_before_opening() {
    let first = GitFixture::new(&[("src/lib.rs", "pub fn first() {}\n")]);
    let second = GitFixture::new(&[("src/lib.rs", "pub fn second() {}\n")]);
    let third = GitFixture::new(&[("src/lib.rs", "pub fn third() {}\n")]);
    let store = TempDir::new().expect("store root");
    let registry = CodeIndexSchedulerRegistryV1::with_background_reconcile_permits(2, 0);
    registry.install_cold_mount_open_gate(first.path());
    registry.install_cold_mount_open_gate(second.path());
    registry.install_cold_mount_open_observer(third.path());

    let first_mount = {
        let registry = registry.clone();
        let project_root = first.path().to_path_buf();
        let store_root = store.path().to_path_buf();
        tokio::spawn(async move {
            registry
                .mount_worktree(test_project_id(), &project_root, store_root, None)
                .await
        })
    };
    registry
        .wait_for_cold_mount_open_events(first.path(), 1)
        .await;
    let second_mount = {
        let registry = registry.clone();
        let project_root = second.path().to_path_buf();
        let store_root = store.path().to_path_buf();
        tokio::spawn(async move {
            registry
                .mount_worktree(test_project_id(), &project_root, store_root, None)
                .await
        })
    };
    registry
        .wait_for_cold_mount_open_events(second.path(), 1)
        .await;

    let capacity = registry
        .mount_worktree(
            test_project_id(),
            third.path(),
            store.path().to_path_buf(),
            None,
        )
        .await
        .expect_err("the N+1 distinct root must be refused before opening");
    registry.release_cold_mount_open_gate(first.path());
    registry.release_cold_mount_open_gate(second.path());
    assert!(
        first_mount
            .await
            .expect("first mount task joins")
            .expect("first mount"),
        "first reserved root mounts"
    );
    assert!(
        second_mount
            .await
            .expect("second mount task joins")
            .expect("second mount"),
        "second reserved root mounts"
    );

    registry.shutdown().await;

    assert!(matches!(
        capacity,
        super::super::CodeIndexSchedulerErrorV1::Identity(message)
            if message.contains("capacity is exhausted")
    ));
    assert!(
        registry.cold_mount_open_events(third.path()).is_empty(),
        "the rejected N+1 root never starts its expensive blocking open"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn foreign_wake_keeps_pending_arrival_when_query_claim_is_released() {
    let fixture = GitFixture::new(&[("src/main.rs", "fn main() {}\n")]);
    let store = TempDir::new().expect("store root");
    let (registry, scope) = mounted_core_query_worktree_with_one_permit(&fixture, &store).await;
    let admission = quiesced_background_reconcile_admission(&registry, fixture.path()).await;
    registry.clear_pending_wake_for_scope(&scope).await;
    registry.install_query_claim_gate(&scope);

    let request = {
        let registry = registry.clone();
        let scope = scope.clone();
        tokio::spawn(async move { registry.request_query_background_reconcile(&scope).await })
    };
    registry.wait_for_query_claim(&scope).await;
    assert!(
        registry
            .notify_path(fixture.path(), fixture.path().join("src/main.rs"))
            .await,
        "foreign hint wake is accepted for the mounted root"
    );
    registry.release_query_claim(&scope);
    assert!(
        !request.await.expect("query admission task joins"),
        "a query whose claim lost its owner to a foreign wake must not restamp \
         QueryAdmission over that arrival"
    );
    let stamped = registry
        .pending_wake_micros_for_scope(&scope)
        .await
        .expect("mounted worktree");

    drop(admission);
    registry.shutdown().await;

    assert_ne!(
        stamped, 0,
        "dropping a query-owned claim must not erase a foreign coalesced wake"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn foreign_wake_arriving_during_query_claim_drop_is_retained() {
    let fixture = GitFixture::new(&[("src/main.rs", "fn main() {}\n")]);
    let store = TempDir::new().expect("store root");
    let (registry, scope) = mounted_core_query_worktree_with_one_permit(&fixture, &store).await;
    let admission = quiesced_background_reconcile_admission(&registry, fixture.path()).await;
    registry.clear_pending_wake_for_scope(&scope).await;
    registry.install_query_claim_gate(&scope);
    registry.install_pending_wake_drop_gate(&scope).await;

    let request = {
        let registry = registry.clone();
        let scope = scope.clone();
        tokio::spawn(async move { registry.request_query_background_reconcile(&scope).await })
    };
    registry.wait_for_query_claim(&scope).await;
    registry.release_query_claim(&scope);
    registry.wait_for_pending_wake_claim_drop(&scope).await;

    let foreign_wake = {
        let registry = registry.clone();
        let project_root = fixture.path().to_path_buf();
        let changed_path = fixture.path().join("src/main.rs");
        tokio::spawn(async move { registry.notify_path(&project_root, changed_path).await })
    };
    registry.wait_for_foreign_pending_wake_attempt(&scope).await;
    registry.release_pending_wake_claim_drop(&scope).await;

    assert!(
        !request.await.expect("query admission task joins"),
        "the rejected query releases its own claimed marker"
    );
    assert!(
        foreign_wake.await.expect("foreign wake task joins"),
        "foreign hint wake is accepted after the claim release"
    );
    let stamped = registry
        .pending_wake_micros_for_scope(&scope)
        .await
        .expect("mounted worktree");

    drop(admission);
    registry.shutdown().await;

    assert_ne!(
        stamped, 0,
        "a foreign wake that contended at the former owner/marker gap remains pending"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shutdown_after_outer_mount_check_refuses_reservation_before_open() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn shutdown_race() {}\n")]);
    let store = TempDir::new().expect("store root");
    let registry = CodeIndexSchedulerRegistryV1::with_background_reconcile_permits(1, 0);
    registry.install_cold_mount_post_check_gate(fixture.path());
    registry.install_cold_mount_open_observer(fixture.path());

    let mount = {
        let registry = registry.clone();
        let project_root = fixture.path().to_path_buf();
        let store_root = store.path().to_path_buf();
        tokio::spawn(async move {
            registry
                .mount_worktree(test_project_id(), &project_root, store_root, None)
                .await
        })
    };
    registry
        .wait_for_cold_mount_post_check(fixture.path())
        .await;
    registry.shutdown().await;
    registry.release_cold_mount_post_check(fixture.path());

    let error = mount
        .await
        .expect("mount task joins")
        .expect_err("shutdown closes cold-mount admission before reservation");
    assert!(matches!(
        error,
        super::super::CodeIndexSchedulerErrorV1::Identity(message)
            if message.contains("shutting down")
    ));
    assert!(
        registry.cold_mount_open_events(fixture.path()).is_empty(),
        "a caller paused before reservation never starts an open after shutdown closes admission"
    );
    assert_eq!(registry.memory_stats().await.mounted_worktrees, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shutdown_waits_for_and_fences_a_cold_mount_open() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn shutdown() {}\n")]);
    let store = TempDir::new().expect("store root");
    let registry = CodeIndexSchedulerRegistryV1::with_background_reconcile_permits(1, 0);
    registry.install_cold_mount_open_gate(fixture.path());
    let mount = {
        let registry = registry.clone();
        let project_root = fixture.path().to_path_buf();
        let store_root = store.path().to_path_buf();
        tokio::spawn(async move {
            registry
                .mount_worktree(test_project_id(), &project_root, store_root, None)
                .await
        })
    };
    registry
        .wait_for_cold_mount_open_events(fixture.path(), 1)
        .await;
    let mut cancelled = registry
        .subscribe_cold_mount_cancellation(fixture.path())
        .expect("cold mount reservation");
    let shutdown = {
        let registry = registry.clone();
        tokio::spawn(async move { registry.shutdown().await })
    };
    let _ = cancelled.changed().await;
    assert!(
        !shutdown.is_finished(),
        "shutdown waits for the in-flight blocking open after fencing it"
    );
    registry.release_cold_mount_open_gate(fixture.path());
    let mount_error = mount
        .await
        .expect("mount task joins")
        .expect_err("shutdown fences publication after the open ends");
    shutdown.await.expect("shutdown task joins");

    assert!(matches!(
        mount_error,
        super::super::CodeIndexSchedulerErrorV1::Identity(message)
            if message.contains("shutting down")
    ));
    assert_eq!(registry.memory_stats().await.mounted_worktrees, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn retirement_waits_for_and_fences_an_exact_cold_mount_open() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn retire() {}\n")]);
    let store = TempDir::new().expect("store root");
    let registry = CodeIndexSchedulerRegistryV1::with_background_reconcile_permits(1, 0);
    registry.install_cold_mount_open_gate(fixture.path());
    let mount = {
        let registry = registry.clone();
        let project_root = fixture.path().to_path_buf();
        let store_root = store.path().to_path_buf();
        tokio::spawn(async move {
            registry
                .mount_worktree(test_project_id(), &project_root, store_root, None)
                .await
        })
    };
    registry
        .wait_for_cold_mount_open_events(fixture.path(), 1)
        .await;
    let mut cancelled = registry
        .subscribe_cold_mount_cancellation(fixture.path())
        .expect("cold mount reservation");
    let roots = BTreeSet::from([fixture.path().canonicalize().expect("canonical root")]);
    let retirement = {
        let registry = registry.clone();
        tokio::spawn(async move {
            registry
                .retire_project_roots_with_deadline(&roots, Duration::from_secs(2))
                .await
        })
    };
    let _ = cancelled.changed().await;
    registry.release_cold_mount_open_gate(fixture.path());
    let mount_error = mount
        .await
        .expect("mount task joins")
        .expect_err("retirement fences publication after the open ends");
    assert!(
        retirement.await.expect("retirement task joins"),
        "retirement joins the cancelled cold-open reservation"
    );
    assert_eq!(registry.memory_stats().await.mounted_worktrees, 0);
    assert!(
        registry
            .mount_worktree(
                test_project_id(),
                fixture.path(),
                store.path().to_path_buf(),
                None,
            )
            .await
            .expect("retired reservation is released after its open joins"),
        "a new owner may mount only after retirement completes"
    );
    registry.shutdown().await;

    assert!(matches!(
        mount_error,
        super::super::CodeIndexSchedulerErrorV1::Identity(message)
            if message.contains("retiring")
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn background_reconciles_respect_a_single_admission_permit() {
    // A bound of ONE serializes all worktrees: while the first worker holds the
    // sole permit (blocked on its scheduler lock), the second cannot start.
    let first = GitFixture::new(&[("src/lib.rs", "pub fn first() -> u32 { 1 }\n")]);
    let second = GitFixture::new(&[("src/lib.rs", "pub fn second() -> u32 { 1 }\n")]);
    let store = TempDir::new().expect("store root");
    let registry = CodeIndexSchedulerRegistryV1::with_background_reconcile_permits(2, 1);
    for fixture in [&first, &second] {
        registry
            .mount_worktree(
                test_project_id(),
                fixture.path(),
                store.path().to_path_buf(),
                None,
            )
            .await
            .expect("mount worktree");
    }
    // Publication broadcasts at publish time, before the pass's deliberately
    // admission-free tail (graph prepare, activation, serving seat) has run.
    // Wait for both serving seats — the tail's last scheduler-lock step — so
    // each worker is parked on its wake. Holding the first scheduler's lock
    // any earlier wedges that worker inside its tail, where it holds no
    // permit, and the second worktree would overtake through the free permit
    // before the first worker's edit pass is ever admitted.
    let first_generation = wait_for_live_complete_generation(&registry, first.path())
        .await
        .generation()
        .manifest()
        .generation_id
        .clone();
    let second_generation = wait_for_live_complete_generation(&registry, second.path())
        .await
        .generation()
        .manifest()
        .generation_id
        .clone();

    let first_handle = registry
        .scheduler_handle(first.path())
        .await
        .expect("first scheduler");
    let first_wake = {
        let scheduler = first_handle
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Arc::clone(&scheduler.wake)
    };
    let (held_tx, held_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let lock_thread = std::thread::spawn(move || {
        let _guard = first_handle
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        held_tx.send(()).expect("signal first lock held");
        let _ = release_rx.recv();
    });
    held_rx.recv().expect("first scheduler lock acquired");

    first.edit("src/lib.rs", "pub fn first() -> u32 { 2 }\n");
    first_wake.notify_one();
    tokio::time::sleep(Duration::from_millis(100)).await;

    second.edit("src/lib.rs", "pub fn second() -> u32 { 2 }\n");
    assert!(
        registry
            .notify_path(second.path(), second.path().join("src/lib.rs"))
            .await
    );
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(
        registry.latest_generation_id(second.path()).await,
        Some(second_generation.clone()),
        "with a single permit a second worktree must wait behind the first"
    );

    release_tx.send(()).expect("release first scheduler");
    lock_thread.join().expect("first lock thread joins");
    let _ = wait_for_generation_change(&registry, first.path(), &first_generation).await;
    let _ = wait_for_generation_change(&registry, second.path(), &second_generation).await;
    registry.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn build_publication_lock_serializes_source_reconcile() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn source() -> u32 { 1 }\n")]);
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
    let initial = wait_for_initial_generation(&registry, fixture.path()).await;
    let build_lock = registry
        .build_publication_lock_handle(fixture.path())
        .await
        .expect("build/publication lock");
    let held = build_lock.lock_owned().await;

    fixture.edit("src/lib.rs", "pub fn source() -> u32 { 2 }\n");
    assert!(
        registry
            .notify_hook_paths(fixture.path(), &["src/lib.rs".to_owned()])
            .await
    );
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        registry.latest_generation_id(fixture.path()).await,
        Some(initial.clone()),
        "the source reconcile cannot publish while another same-store build owns exclusivity"
    );

    drop(held);
    let advanced = wait_for_generation_change(&registry, fixture.path(), &initial).await;
    assert_ne!(advanced, initial);
    registry.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn distinct_stores_reconcile_in_parallel_under_bounded_admission() {
    // With two permits, hold the FIRST worktree's scheduler lock so its worker
    // takes one permit and blocks mid-reconcile (an in-flight reconcile analog).
    // The SECOND worktree, writing to a different path-scoped store, must still
    // acquire the remaining permit and publish — proving distinct stores are NOT
    // serialized behind one another. (Same-store exclusion — that one worktree
    // never runs two overlapping reconciles — is structural, from its single
    // worker plus per-scheduler `Mutex`, and is covered by
    // `scheduler_notifications_release_registry_while_reconcile_is_busy`.)
    let first = GitFixture::new(&[("src/lib.rs", "pub fn first() -> u32 { 1 }\n")]);
    let second = GitFixture::new(&[("src/lib.rs", "pub fn second() -> u32 { 1 }\n")]);
    let store = TempDir::new().expect("store root");
    let registry = CodeIndexSchedulerRegistryV1::with_background_reconcile_permits(2, 2);
    for fixture in [&first, &second] {
        registry
            .mount_worktree(
                test_project_id(),
                fixture.path(),
                store.path().to_path_buf(),
                None,
            )
            .await
            .expect("mount worktree");
    }
    let first_generation = wait_for_initial_generation(&registry, first.path()).await;
    let second_generation = wait_for_initial_generation(&registry, second.path()).await;

    let first_handle = registry
        .scheduler_handle(first.path())
        .await
        .expect("first scheduler");
    let first_wake = {
        let scheduler = first_handle
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Arc::clone(&scheduler.wake)
    };
    let (held_tx, held_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let lock_thread = std::thread::spawn(move || {
        let _guard = first_handle
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        held_tx.send(()).expect("signal first lock held");
        let _ = release_rx.recv();
    });
    held_rx.recv().expect("first scheduler lock acquired");

    // Wake the first worker: it takes one of the two permits, then blocks on the
    // held scheduler lock. Its own store cannot advance while blocked, but it
    // occupies exactly one permit.
    first.edit("src/lib.rs", "pub fn first() -> u32 { 2 }\n");
    first_wake.notify_one();
    tokio::time::sleep(Duration::from_millis(100)).await;

    // The second worktree — a distinct path-scoped store — must proceed on the
    // remaining permit and publish a new generation without the first releasing.
    // (Note: the first scheduler lock is deliberately held here, so we must NOT
    // query the first worktree via `latest_generation_id`, which would block on
    // that lock while holding the registry map lock.)
    second.edit("src/lib.rs", "pub fn second() -> u32 { 2 }\n");
    assert!(
        registry
            .notify_path(second.path(), second.path().join("src/lib.rs"))
            .await
    );
    let advanced_second =
        wait_for_generation_change(&registry, second.path(), &second_generation).await;
    assert_ne!(
        advanced_second, second_generation,
        "a distinct store must reconcile in parallel, not serialize behind the first"
    );

    // Release the first worktree and confirm it, too, reconciles the pending edit
    // once its lock frees — it was blocked, never starved.
    release_tx.send(()).expect("release first scheduler");
    lock_thread.join().expect("first lock thread joins");
    let advanced_first =
        wait_for_generation_change(&registry, first.path(), &first_generation).await;
    assert_ne!(
        advanced_first, first_generation,
        "the first worktree reconciles its pending edit once its lock is released"
    );
    registry.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn poisoned_scheduler_lock_does_not_retire_the_background_worker() {
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
        .expect("mount worktree");
    let initial = wait_for_queryable_text_generation_id(&registry, fixture.path()).await;
    let scheduler = registry
        .scheduler_handle(fixture.path())
        .await
        .expect("scheduler");
    let poison = Arc::clone(&scheduler);
    assert!(
        std::thread::spawn(move || {
            let _guard = poison.lock().expect("unpoisoned scheduler");
            panic!("fixture poison");
        })
        .join()
        .is_err()
    );

    fixture.edit("src/lib.rs", "pub fn alpha() -> u32 { 2 }\n");
    scheduler
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .notify_path(fixture.path().join("src/lib.rs"));
    // The worker survived the poison when the edit becomes text-current.
    let _ = wait_for_queryable_text_generation_change(&registry, fixture.path(), &initial).await;
    registry.shutdown().await;
}

#[cfg(all(feature = "semantic-fastembed", not(windows)))]
#[tokio::test(flavor = "multi_thread")]
async fn configured_jina_lifecycle_publishes_and_restores_semantic_generation() {
    struct PreparedJinaFixture {
        root: PathBuf,
    }

    impl ModelMemberSourceV1 for PreparedJinaFixture {
        fn fetch_member(
            &self,
            model: &CatalogedFastEmbedModelV1,
            upstream_path: &str,
            destination: &Path,
        ) -> Result<(), ModelLifecycleErrorV1> {
            let member = model
                .members
                .values()
                .find(|member| member.upstream_path == upstream_path)
                .ok_or(ModelLifecycleErrorV1::DownloadFailed)?;
            std::fs::copy(self.root.join(&member.path), destination)
                .map(|_| ())
                .map_err(|_| ModelLifecycleErrorV1::DownloadFailed)
        }
    }

    let Some(fixture_root) = std::env::var_os("TRACEDECAY_DISTRIBUTION_FASTEMBED_FIXTURE")
        .map(PathBuf::from)
        .filter(|path| path.is_dir())
    else {
        eprintln!(
            "skipping configured Jina integration; prepare fixture and set \
             TRACEDECAY_DISTRIBUTION_FASTEMBED_FIXTURE"
        );
        return;
    };

    let lifecycle_root = TempDir::new().expect("lifecycle root");
    let catalog: FastEmbedModelCatalogV1 = production_fastembed_catalog();
    let lifecycle = Arc::new(
        SemanticModelLifecycleOwnerV1::open(
            lifecycle_root.path(),
            catalog,
            Arc::new(PreparedJinaFixture { root: fixture_root }),
        )
        .expect("Jina lifecycle"),
    );
    lifecycle
        .select_model(Some(DEFAULT_FASTEMBED_MODEL_ID), true)
        .expect("select configured Jina model");
    lifecycle
        .acquire_blocking_for_tests()
        .expect("install configured Jina fixture");

    let project = GitFixture::new(&[(
        "src/lib.rs",
        "pub fn semantic_bridge() -> &'static str { \"ready\" }\n",
    )]);
    let code_store = TempDir::new().expect("code store");
    let mut scheduler = scheduler(
        &project,
        code_store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    );
    published(scheduler.reconcile_now().expect("publish code generation"));
    let latest = scheduler.latest_complete().expect("latest code generation");

    let database_root = TempDir::new().expect("database root");
    let database_path = database_root.path().join("project.db");
    let authority =
        DatabaseAuthority::acquire_test(&database_path, "Jina semantic bridge integration")
            .expect("database authority");
    let database = Arc::new(
        Database::publish_test_runtime(
            &database_path,
            &authority,
            TestDatabaseRuntimeMode::Initialize,
        )
        .await
        .expect("project database")
        .0,
    );
    let handle = DaemonSemanticRuntimeHandleV1::new(1, 64, 2 << 30).expect("semantic handle");
    let vector_graph = IsolatedSemanticVectorGraphProviderV1::new(&latest.generation);
    let runtime = ProductionSemanticRuntimeV1::new(
        handle.clone(),
        Arc::clone(&database),
        Arc::clone(&vector_graph) as Arc<dyn SemanticVectorGraphProviderV1>,
        Arc::clone(&lifecycle),
        SemanticResourceCeilings {
            max_model_bytes: 1024 * 1024 * 1024,
            max_tokenizer_bytes: 64 * 1024 * 1024,
            max_resident_bytes: Some(2 * 1024 * 1024 * 1024),
            max_threads: 1,
            max_concurrent_sessions: 1,
            max_batch_size: 4,
            max_sequence_length: 4096,
            load_deadline_ms: 180_000,
        },
        tracedecay_domain::EmbeddingDocumentCompositionV1::SanitizedText,
    );

    assert!(runtime.schedule_saved_generation(Arc::clone(&latest.generation)));
    latest
        .production_query_owners()
        .expect("ordinary lanes remain callable during Jina startup");
    tokio::time::timeout(Duration::from_mins(3), async {
        while handle.current().is_none() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("Jina projection became atomically current");
    let current = handle.current().expect("current semantic pointer");
    assert!(
        handle
            .query_factory(
                &current.source_generation,
                &current.generation,
                &current.projection_key,
            )
            .is_some()
    );

    let restarted_handle =
        DaemonSemanticRuntimeHandleV1::new(1, 64, 2 << 30).expect("restarted handle");
    let restarted = ProductionSemanticRuntimeV1::new(
        restarted_handle.clone(),
        database,
        Arc::clone(&vector_graph) as Arc<dyn SemanticVectorGraphProviderV1>,
        lifecycle,
        SemanticResourceCeilings {
            max_model_bytes: 1024 * 1024 * 1024,
            max_tokenizer_bytes: 64 * 1024 * 1024,
            max_resident_bytes: Some(2 * 1024 * 1024 * 1024),
            max_threads: 1,
            max_concurrent_sessions: 1,
            max_batch_size: 4,
            max_sequence_length: 4096,
            load_deadline_ms: 180_000,
        },
        tracedecay_domain::EmbeddingDocumentCompositionV1::SanitizedText,
    );
    let generation_reads_before_restore = vector_graph.generation_reads();
    assert!(
        restarted
            .restore_current(latest.metadata().manifest(), &current.generation)
            .await
            .expect("restore current generation")
    );
    assert_eq!(
        vector_graph.generation_reads(),
        generation_reads_before_restore,
        "restart restore must read vector provenance through current metadata identity, not a decoded generation"
    );
    assert_eq!(restarted_handle.current(), Some(current.clone()));
    assert!(
        restarted_handle
            .query_factory(
                &current.source_generation,
                &current.generation,
                &current.projection_key,
            )
            .is_some()
    );
}

/// gix status classification keeps committed/staged/unstaged/untracked/deleted
/// dispositions distinct and truthful.
#[test]
fn classification_distinguishes_staged_unstaged_untracked_and_deleted() {
    let fixture = GitFixture::new(&[
        ("src/a.rs", "pub fn a() -> u32 { 1 }\n"),
        ("src/b.rs", "pub fn b() -> u32 { 2 }\n"),
        ("src/d.rs", "pub fn d() -> u32 { 4 }\n"),
    ]);
    // Staged modification.
    fixture.edit("src/a.rs", "pub fn a() -> u32 { 10 }\n");
    git(fixture.path(), &["add", "src/a.rs"]);
    // Unstaged modification.
    fixture.edit("src/b.rs", "pub fn b() -> u32 { 20 }\n");
    // Untracked new file.
    write(fixture.path(), "src/c.rs", "pub fn c() -> u32 { 3 }\n");
    // Unstaged deletion.
    std::fs::remove_file(fixture.path().join("src/d.rs")).expect("remove d");

    let repository = gix::open(fixture.path()).expect("open gix");
    let classification = WorktreeChangeClassificationV1::classify(&repository).expect("classify");

    assert_eq!(
        classification.class_of("src/a.rs"),
        Some(WorktreeChangeClassV1::StagedModified)
    );
    assert_eq!(
        classification.class_of("src/b.rs"),
        Some(WorktreeChangeClassV1::UnstagedModified)
    );
    assert_eq!(
        classification.class_of("src/c.rs"),
        Some(WorktreeChangeClassV1::Untracked)
    );
    assert_eq!(
        classification.class_of("src/d.rs"),
        Some(WorktreeChangeClassV1::UnstagedDeleted)
    );

    let deleted = classification.deleted_paths();
    assert!(
        deleted.contains("src/d.rs"),
        "deletion is a tombstone candidate"
    );

    let candidates = classification.candidate_paths();
    assert!(candidates.contains("src/a.rs"));
    assert!(candidates.contains("src/b.rs"));
    assert!(candidates.contains("src/c.rs"));
    assert!(
        !candidates.contains("src/d.rs"),
        "a deleted path is never a present-content candidate"
    );
}

/// A filesystem rename is reconciled as the truthful delete-plus-add pair and
/// produces the same final code lanes as a fresh scan of the renamed tree.
#[test]
fn rename_reconciliation_matches_clean_scan() {
    let fixture = GitFixture::new(&[
        ("src/old.rs", "pub fn renamed_symbol() -> u32 { 7 }\n"),
        ("src/keep.rs", "pub fn keep_symbol() -> u32 { 1 }\n"),
    ]);
    let store = TempDir::new().expect("store root");
    let bytes = Arc::new(SharedCodeIndexBytePoolV1::default());
    let mut incremental = scheduler(
        &fixture,
        store.path().join("incremental"),
        Arc::clone(&bytes),
    );
    published(incremental.reconcile_now().expect("baseline publish"));

    std::fs::rename(
        fixture.path().join("src/old.rs"),
        fixture.path().join("src/new.rs"),
    )
    .expect("rename source file");
    let classification =
        WorktreeChangeClassificationV1::classify(&gix::open(fixture.path()).expect("open gix"))
            .expect("classify rename");
    assert_eq!(
        classification.class_of("src/old.rs"),
        Some(WorktreeChangeClassV1::UnstagedDeleted)
    );
    assert_eq!(
        classification.class_of("src/new.rs"),
        Some(WorktreeChangeClassV1::Untracked)
    );

    incremental.notify_path(fixture.path().join("src/old.rs"));
    incremental.notify_path(fixture.path().join("src/new.rs"));
    let renamed = published(
        incremental
            .reconcile_now()
            .expect("incremental rename reconcile"),
    );
    let mut clean = scheduler(&fixture, store.path().join("clean"), bytes);
    let clean = published(clean.reconcile_now().expect("clean renamed-tree scan"));

    assert_eq!(
        renamed.snapshot_content_identity, clean.snapshot_content_identity,
        "rename reconciliation must capture the same final tree as a clean scan"
    );
    assert_eq!(
        renamed.lane_digest, clean.lane_digest,
        "rename reconciliation must publish byte-identical code lanes"
    );
    let latest = incremental.latest_complete().expect("renamed generation");
    assert!(
        latest
            .generation()
            .snapshot()
            .files
            .iter()
            .any(|file| file.logical_path == "src/new.rs")
    );
    assert!(
        latest
            .generation()
            .snapshot()
            .files
            .iter()
            .all(|file| file.logical_path != "src/old.rs")
    );
}

/// A staged-only edit (index differs from HEAD while the worktree matches the
/// staged bytes) is real indexing work and converges with a fresh scan.
#[test]
fn index_only_reconciliation_matches_clean_scan() {
    let fixture = GitFixture::new(&[
        ("src/lib.rs", "pub fn staged_symbol() -> u32 { 1 }\n"),
        ("src/keep.rs", "pub fn keep_symbol() -> u32 { 2 }\n"),
    ]);
    let store = TempDir::new().expect("store root");
    let bytes = Arc::new(SharedCodeIndexBytePoolV1::default());
    let mut incremental = scheduler(
        &fixture,
        store.path().join("incremental"),
        Arc::clone(&bytes),
    );
    published(incremental.reconcile_now().expect("baseline publish"));

    fixture.edit("src/lib.rs", "pub fn staged_symbol() -> u32 { 10 }\n");
    git(fixture.path(), &["add", "src/lib.rs"]);
    let classification =
        WorktreeChangeClassificationV1::classify(&gix::open(fixture.path()).expect("open gix"))
            .expect("classify staged-only edit");
    assert_eq!(
        classification.class_of("src/lib.rs"),
        Some(WorktreeChangeClassV1::StagedModified)
    );
    assert_eq!(classification.changes().len(), 1);

    let staged = published(
        incremental
            .reconcile_now()
            .expect("incremental staged-only reconcile"),
    );
    let mut clean = scheduler(&fixture, store.path().join("clean"), bytes);
    let clean = published(clean.reconcile_now().expect("clean staged-tree scan"));

    assert_eq!(staged.reextracted_files, 1);
    assert_eq!(
        staged.snapshot_content_identity, clean.snapshot_content_identity,
        "staged-only reconciliation must capture the same final tree as a clean scan"
    );
    assert_eq!(
        staged.lane_digest, clean.lane_digest,
        "staged-only reconciliation must publish byte-identical code lanes"
    );
}

/// Deleting a tracked file tombstones its prior chunks: the next published
/// generation must not carry any chunk anchored to the removed file, while an
/// untouched sibling's chunks survive unchanged.
#[test]
fn deleting_a_file_tombstones_its_prior_chunks() {
    let fixture = GitFixture::new(&[
        ("src/keep.rs", "pub fn keep_marker_symbol() -> u32 { 1 }\n"),
        ("src/gone.rs", "pub fn gone_marker_symbol() -> u32 { 2 }\n"),
    ]);
    let store = TempDir::new().expect("store root");
    let bytes = Arc::new(SharedCodeIndexBytePoolV1::default());
    let mut scheduler = scheduler(&fixture, store.path().to_path_buf(), bytes);

    let baseline = published(scheduler.reconcile_now().expect("baseline publish"));
    let gone_occurrences: BTreeSet<_> = {
        let latest = scheduler.latest_complete().expect("baseline generation");
        latest
            .lexical()
            .iter()
            .filter(|chunk| chunk.sanitized_text.as_str().contains("gone_marker_symbol"))
            .map(|chunk| chunk.anchor.file_occurrence_id.clone())
            .collect()
    };
    assert!(
        !gone_occurrences.is_empty(),
        "baseline must index the file that will be deleted"
    );

    // Delete the tracked file out of band (an unstaged deletion) and reconcile.
    std::fs::remove_file(fixture.path().join("src/gone.rs")).expect("remove gone.rs");
    scheduler.notify_path(fixture.path().join("src/gone.rs"));
    let after = published(scheduler.reconcile_now().expect("post-deletion publish"));

    assert_ne!(
        baseline.generation_id, after.generation_id,
        "removing indexed content must publish a new generation"
    );
    assert!(
        after.changed_chunks > 0,
        "a deletion must register as changed (tombstoned) chunk work"
    );
    assert!(
        !after
            .file_occurrence_ids
            .iter()
            .any(|occurrence| gone_occurrences.contains(occurrence)),
        "the deleted file's occurrence must be absent from the new generation"
    );

    let latest = scheduler
        .latest_complete()
        .expect("post-deletion generation");
    assert!(
        latest
            .lexical()
            .iter()
            .all(|chunk| !chunk.sanitized_text.as_str().contains("gone_marker_symbol")),
        "no surviving chunk may carry the deleted file's content"
    );
    assert!(
        latest
            .lexical()
            .iter()
            .any(|chunk| chunk.sanitized_text.as_str().contains("keep_marker_symbol")),
        "an untouched sibling's chunks must survive the deletion"
    );
}

/// A host after-file-edit hook delivers its exact touched paths into the
/// incremental queue and the subsequent reconcile publishes the edit.
#[test]
fn hook_hint_delivers_exact_paths_and_schedules_batch() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn alpha() -> u32 { 1 }\n")]);
    let store = TempDir::new().expect("store root");
    let bytes = Arc::new(SharedCodeIndexBytePoolV1::default());
    let mut scheduler = scheduler(&fixture, store.path().to_path_buf(), bytes);
    published(scheduler.reconcile_now().expect("baseline"));

    fixture.edit("src/lib.rs", "pub fn alpha() -> u32 { 2 }\n");
    let edited = fixture.path().join("src/lib.rs");
    scheduler.notify_hook_paths([edited.clone()]);

    assert!(
        scheduler.pending_hint_paths().contains(&edited),
        "the exact hook path is enqueued as a hint"
    );

    let publish = published(scheduler.reconcile_now().expect("hook-scheduled batch"));
    assert!(publish.changed_chunks > 0, "the hinted edit is indexed");
    assert!(
        scheduler.pending_hint_paths().is_empty(),
        "reconciliation drains the hint queue"
    );
}

/// With no filesystem watcher, a raw out-of-band file write is still caught by
/// the tier-2 bounded-staleness reconcile at query admission.
#[test]
fn threshold_expiry_reconciles_out_of_band_write_without_watcher() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn alpha() -> u32 { 1 }\n")]);
    let store = TempDir::new().expect("store root");
    let bytes = Arc::new(SharedCodeIndexBytePoolV1::default());
    let policy = CodeIndexHintPolicyV1 {
        staleness_threshold: Duration::ZERO,
    };
    let mut scheduler = scheduler_with_policy(&fixture, store.path().to_path_buf(), bytes, policy);
    let baseline = published(scheduler.reconcile_now().expect("baseline"));

    // No hook, no watcher: a raw editor/rsync write.
    fixture.edit("src/lib.rs", "pub fn alpha() -> u32 { 2 }\n");

    let reconciled = scheduler
        .ensure_fresh_for_query()
        .expect("freshness ladder runs");
    assert!(
        reconciled.is_some(),
        "an elapsed staleness bound reconciles at admission"
    );

    let served = scheduler
        .latest_complete()
        .expect("served generation")
        .generation
        .snapshot()
        .content_identity
        .clone();
    assert_ne!(
        served, baseline.snapshot_content_identity,
        "the out-of-band write is reflected in the served generation"
    );
}

/// Tier-2 cheapness: when the staleness bound has elapsed but nothing on disk
/// changed, the query-path freshness check must NOT run a full read+hash
/// reconcile. The stat-level prefilter resets the clock and reports no work, so
/// a quiet repository is never re-hashed every threshold on the query path.
#[test]
fn threshold_expiry_without_change_skips_full_reconcile() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn alpha() -> u32 { 1 }\n")]);
    let store = TempDir::new().expect("store root");
    let bytes = Arc::new(SharedCodeIndexBytePoolV1::default());
    let policy = CodeIndexHintPolicyV1 {
        staleness_threshold: Duration::ZERO,
    };
    let mut scheduler = scheduler_with_policy(&fixture, store.path().to_path_buf(), bytes, policy);
    let baseline = published(scheduler.reconcile_now().expect("baseline"));

    // The staleness bound has elapsed (ZERO), but with no disk change the stat
    // prefilter must short-circuit before any capture and report no reconcile.
    let reconciled = scheduler
        .ensure_fresh_for_query()
        .expect("freshness ladder runs");
    assert!(
        reconciled.is_none(),
        "an unchanged tree past the staleness bound must not reconcile"
    );

    let served = scheduler
        .latest_complete()
        .expect("served generation")
        .generation
        .manifest()
        .generation_id
        .clone();
    assert_eq!(
        served, baseline.generation_id,
        "no new generation is published when nothing changed on disk"
    );
}

#[test]
fn ready_query_expiry_defers_even_an_unchanged_tree() {
    let fixture = GitFixture::new(ALPHA_LIB_V1);
    let store = TempDir::new().expect("store root");
    let bytes = Arc::new(SharedCodeIndexBytePoolV1::default());
    let policy = CodeIndexHintPolicyV1 {
        staleness_threshold: Duration::ZERO,
    };
    let mut scheduler = scheduler_with_policy(&fixture, store.path().to_path_buf(), bytes, policy);
    published(scheduler.reconcile_now().expect("baseline"));

    assert!(
        scheduler
            .latest_complete_ready_for_query()
            .expect("ready query")
            .is_none(),
        "latency-sensitive admission must abstain instead of scanning the worktree"
    );
    assert_eq!(
        scheduler.pending_hint_count(),
        None,
        "ready admission schedules an overflow reconcile in the background"
    );
}

/// A git operation from another process (commit here stands in for pull/rebase)
/// is detected instantly by the tier-1 .git-metadata check, without waiting for
/// the staleness bound and without any watcher.
#[test]
fn git_op_in_another_process_detected_via_metadata() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn alpha() -> u32 { 1 }\n")]);
    let store = TempDir::new().expect("store root");
    let bytes = Arc::new(SharedCodeIndexBytePoolV1::default());
    // Long staleness bound so only tier-1 (git metadata) can fire.
    let policy = CodeIndexHintPolicyV1 {
        staleness_threshold: Duration::from_hours(1),
    };
    let mut scheduler = scheduler_with_policy(&fixture, store.path().to_path_buf(), bytes, policy);
    let baseline = published(scheduler.reconcile_now().expect("baseline"));

    // Another process commits a change.
    fixture.edit("src/lib.rs", "pub fn alpha() -> u32 { 2 }\n");
    git(fixture.path(), &["commit", "-qam", "external"]);

    let reconciled = scheduler
        .ensure_fresh_for_query()
        .expect("freshness ladder runs");
    assert!(
        reconciled.is_some(),
        "a .git-metadata change reconciles before the bound"
    );

    let served = scheduler
        .latest_complete()
        .expect("served generation")
        .generation
        .snapshot()
        .content_identity
        .clone();
    assert_ne!(
        served, baseline.snapshot_content_identity,
        "the external git change is reflected in the served generation"
    );
}

/// When HEAD moves between indexing and query, the served generation is
/// refreshed to the new revision while its repository/worktree identity is never
/// mixed with another checkout's.
#[test]
fn identity_move_reconciles_and_never_mixes_identity() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn alpha() -> u32 { 1 }\n")]);
    let store = TempDir::new().expect("store root");
    let bytes = Arc::new(SharedCodeIndexBytePoolV1::default());
    let policy = CodeIndexHintPolicyV1 {
        staleness_threshold: Duration::from_hours(1),
    };
    let mut scheduler = scheduler_with_policy(&fixture, store.path().to_path_buf(), bytes, policy);
    published(scheduler.reconcile_now().expect("baseline"));

    let repo_before = scheduler.identity().repository_id().clone();
    let worktree_before = scheduler.identity().worktree_id().clone();
    let commit_before = scheduler.identity().head_commit().cloned();

    // HEAD moves under the same worktree.
    fixture.edit("src/lib.rs", "pub fn alpha() -> u32 { 2 }\n");
    git(fixture.path(), &["commit", "-qam", "move-head"]);

    let reconciled = scheduler
        .ensure_fresh_for_query()
        .expect("freshness ladder runs");
    assert!(reconciled.is_some(), "a HEAD move reconciles at admission");

    // Tier-3 backstop: identity re-resolved to the new revision.
    let commit_after = scheduler.identity().head_commit().cloned();
    assert_ne!(
        commit_before, commit_after,
        "the resolved source revision advances with HEAD"
    );
    // Structural identity is never mixed across the move.
    assert_eq!(scheduler.identity().repository_id(), &repo_before);
    assert_eq!(scheduler.identity().worktree_id(), &worktree_before);

    let served = scheduler.latest_complete().expect("served generation");
    assert_eq!(
        &served.generation.snapshot().repository,
        &repo_before,
        "the served generation is attributed to its exact repository identity"
    );
    assert_eq!(
        served.generation.snapshot().worktree.as_ref(),
        Some(&worktree_before),
        "the served generation is attributed to its exact worktree identity"
    );
}

/// Re-reconciling identical final content in a fresh store (same worktree)
/// yields the byte-identical published chunk lane, proving publication output
/// is a pure function of content identity and independent of edit history.
#[test]
fn reparse_matches_full_parse_chunks() {
    let fixture = GitFixture::new(&[(
        "src/lib.rs",
        "pub fn alpha() -> u32 { 1 }\npub fn beta() -> u32 { 2 }\n",
    )]);
    let store = TempDir::new().expect("store root");
    let bytes = Arc::new(SharedCodeIndexBytePoolV1::default());

    // Sequential-edit scheduler: baseline then two edits, each reconciled.
    let mut sequential = scheduler(
        &fixture,
        store.path().join("sequential"),
        Arc::clone(&bytes),
    );
    published(sequential.reconcile_now().expect("sequential baseline"));

    fixture.edit(
        "src/lib.rs",
        "pub fn alpha() -> u32 { 10 }\npub fn beta() -> u32 { 2 }\n",
    );
    published(sequential.reconcile_now().expect("sequential edit 1"));

    let final_source = "pub fn alpha() -> u32 { 10 }\npub fn beta() -> u32 { 20 }\n";
    fixture.edit("src/lib.rs", final_source);
    let second = published(sequential.reconcile_now().expect("sequential edit 2"));

    // Fresh-store scheduler over the identical final content in the SAME
    // worktree, so chunk identity (repository/worktree-bound) still matches.
    let mut full = scheduler(&fixture, store.path().join("full"), bytes);
    let full_publish = published(full.reconcile_now().expect("full parse"));

    assert_eq!(
        second.snapshot_content_identity, full_publish.snapshot_content_identity,
        "identical final content yields identical snapshot identity"
    );
    assert_eq!(
        second.lane_digest, full_publish.lane_digest,
        "sequential-edit and fresh-store reconcile produce byte-identical chunk lanes"
    );
}

// ---------------------------------------------------------------------------
// Query-admission serving generation: an unpinned query resolves through the
// freshness ladder to the latest complete generation; an explicit caller pin
// is served generation-bound and read-only, bypassing freshness entirely.
// ---------------------------------------------------------------------------

/// Publication is broadcast when reconcile seals, before the sealed generation
/// takes the serving slot, so `branch_add`'s exact-branch wait had no event for
/// the seat and polled the slot every 10ms for up to thirty minutes instead.
/// The seating counter replaces that poll, and it only can if a wake means the
/// slot already holds the generation.
#[tokio::test]
async fn serving_seat_wake_arrives_only_after_the_slot_is_seated() {
    let fixture = GitFixture::new(ALPHA_LIB_V1);
    let store = TempDir::new().expect("store root");
    let registry = CodeIndexSchedulerRegistryV1::new(1);
    let mut seats = registry.subscribe_serving_seats();
    registry
        .mount_worktree(
            test_project_id(),
            fixture.path(),
            store.path().to_path_buf(),
            None,
        )
        .await
        .expect("mount scheduler");
    tokio::time::timeout(Duration::from_secs(30), seats.changed())
        .await
        .expect("seating wakes its waiters instead of leaving them to poll")
        .expect("the seating channel stays open while the registry lives");
    assert!(
        registry
            .latest_complete_serving_for_test(fixture.path())
            .await
            .is_some(),
        "a seat wake must not fire before the serving slot holds the generation"
    );
    registry.shutdown().await;
}

/// A seat that lands after the historical 5s / 10ms poll deadline must still
/// be observed. The old helper (`wait_for_live_complete_generation` before
/// #1206) asserted `"live generation"` once `Instant` passed that bound;
/// the registry seating signal has no such wall-clock cut-off.
#[tokio::test(flavor = "multi_thread")]
async fn serving_seat_signal_observes_a_seat_that_misses_the_poll_deadline() {
    let fixture = GitFixture::new(ALPHA_LIB_V1);
    let store = TempDir::new().expect("store root");
    let registry = CodeIndexSchedulerRegistryV1::new(1);
    let path = fixture.path().to_path_buf();

    let poll_registry = registry.clone();
    let poll_path = path.clone();
    let poll = tokio::spawn(async move {
        wait_for_live_complete_generation_by_polling(&poll_registry, &poll_path).await
    });

    let signal_registry = registry.clone();
    let signal_path = path.clone();
    let signal = tokio::spawn(async move {
        wait_for_live_complete_generation(&signal_registry, &signal_path).await
    });

    // Publish after the old poller's 5s deadline so the poll is already dead.
    tokio::time::sleep(Duration::from_secs(5) + Duration::from_millis(50)).await;
    registry
        .mount_worktree(
            test_project_id(),
            fixture.path(),
            store.path().to_path_buf(),
            None,
        )
        .await
        .expect("mount scheduler");

    let poll_result = poll.await;
    assert!(
        poll_result
            .as_ref()
            .err()
            .is_some_and(tokio::task::JoinError::is_panic),
        "polling helper must miss a seat that arrives after its 5s deadline"
    );

    let latest = signal.await.expect("signal waiter joined");
    assert_eq!(
        latest.generation.manifest().generation_id,
        registry
            .latest_complete_serving_for_test(fixture.path())
            .await
            .expect("mounted seat")
            .generation
            .manifest()
            .generation_id
    );
    registry.shutdown().await;
}

/// A worktree that never seats must fail with the serving-seat diagnostic
/// instead of hanging on the signal. The short ceiling is local to this
/// assertion; production waits keep [`SERVING_SEAT_FAILURE_CEILING`].
#[tokio::test]
#[should_panic(expected = "serving seat never arrived")]
async fn serving_seat_signal_fails_when_a_seat_never_arrives() {
    let registry = CodeIndexSchedulerRegistryV1::new(1);
    let path = Path::new("/no-such-tracedecay-worktree-for-seat-ceiling");
    let _ = wait_until_serving_seat(&registry, path, Duration::from_millis(250), || {
        registry.latest_complete_serving_for_test(path)
    })
    .await;
}

#[tokio::test]
async fn semantic_mcp_abstention_uses_freshest_sealed_generation() {
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
        .expect("mount scheduler");
    let initial = wait_for_live_complete_generation(&registry, fixture.path())
        .await
        .generation
        .manifest()
        .generation_id
        .clone();

    let first = registry.semantic_mcp_abstention(fixture.path()).await;
    assert_eq!(first.code_generation.as_deref(), Some(initial.as_str()));
    assert_eq!(first.reason, "semantic_runtime_unavailable");

    fixture.edit("src/lib.rs", "pub fn alpha() -> u32 { 2 }\n");
    git(fixture.path(), &["commit", "-qam", "external"]);
    // Query admission schedules the reconcile instead of running it inline, so
    // the out-of-band commit lands on the background worker. The abstention
    // still reports the freshest *sealed* generation; it just no longer forces
    // the rebuild that seals it onto whichever request arrived first.
    let _ = registry.semantic_mcp_abstention(fixture.path()).await;
    wait_for_generation_change(&registry, fixture.path(), &initial).await;
    // Publication is not the sealed serving seat. Abstention reports the
    // seated generation, so wait for that seat to advance.
    wait_until_serving_seat(
        &registry,
        fixture.path(),
        SERVING_SEAT_FAILURE_CEILING,
        || async {
            registry
                .latest_complete_serving_for_test(fixture.path())
                .await
                .filter(|latest| latest.generation.manifest().generation_id != initial)
        },
    )
    .await;
    let refreshed = registry.semantic_mcp_abstention(fixture.path()).await;
    assert_ne!(refreshed.code_generation.as_deref(), Some(initial.as_str()));
    assert_eq!(
        refreshed.code_generation.as_deref(),
        registry
            .latest_generation_id(fixture.path())
            .await
            .as_ref()
            .map(tracedecay_domain::CodeGenerationId::as_str)
    );
    assert_eq!(refreshed.reason, "semantic_runtime_unavailable");
    registry.shutdown().await;
}

#[tokio::test]
async fn freshness_failure_does_not_serve_a_stale_complete_generation() {
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
        .expect("mount scheduler");
    wait_for_live_complete_generation(&registry, fixture.path()).await;
    // A busy owner pass serves the seated generation without re-proving
    // source (serve-old-first); the fail-closed assertion below is about the
    // freshness ladder, so the probe must reach it.
    wait_for_quiescent_owner_pass(&registry, fixture.path()).await;

    let git_dir = fixture.path().join(".git");
    let unavailable_git_dir = fixture.path().join(".git-unavailable");
    std::fs::rename(&git_dir, &unavailable_git_dir).expect("hide git authority");
    let latest = registry.latest_complete_fresh(fixture.path()).await;
    std::fs::rename(&unavailable_git_dir, &git_dir).expect("restore git authority");

    assert!(
        latest.is_none(),
        "failed freshness resolution must fail closed instead of serving stale data"
    );
    registry.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn expired_query_does_not_wait_for_a_busy_scheduler() {
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
        .expect("mount scheduler");
    let latest = wait_for_live_complete_generation(&registry, fixture.path()).await;
    let generation = latest.generation.manifest().generation_id.clone();
    let operation =
        callable_code_operation(CallableCodeOperationKind::ExactOccurrence).expect("operation");
    let context = application_context(
        &operation,
        latest.generation.snapshot().repository.clone(),
        latest
            .generation
            .snapshot()
            .worktree
            .clone()
            .expect("worktree identity"),
    )
    .with_deadline(Deadline::new(UtcMicros(1)).expect("expired deadline"));
    let request = ExactOccurrenceRequest::new(
        "alpha",
        None,
        CodeQueryScope::new(generation, None).expect("scope"),
        query_meta(),
    )
    .expect("request");

    let scheduler = registry
        .scheduler_handle(fixture.path())
        .await
        .expect("scheduler");
    let (held_tx, held_rx) = std::sync::mpsc::channel();
    let lock_thread = std::thread::spawn(move || {
        let _guard = scheduler
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        held_tx.send(()).expect("signal scheduler lock held");
        std::thread::sleep(Duration::from_millis(300));
    });
    held_rx.recv().expect("scheduler lock acquired");

    let started = std::time::Instant::now();
    let outcome = registry
        .exact_occurrence(
            RetrievalPortContext {
                request: &context,
                operation: &operation,
            },
            &request,
        )
        .await;
    let elapsed = started.elapsed();
    lock_thread.join().expect("scheduler lock thread joins");

    assert!(matches!(outcome, RetrievalPortOutcome::Unavailable(_)));
    assert!(
        elapsed < Duration::from_millis(100),
        "expired query waited {elapsed:?} for scheduler work"
    );
    registry.shutdown().await;
}

#[test]
fn same_content_head_move_publishes_new_source_identity() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn alpha() -> u32 { 1 }\n")]);
    let store = TempDir::new().expect("store root");
    let mut scheduler = scheduler(
        &fixture,
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    );
    let initial = published(scheduler.reconcile_now().expect("initial reconcile"));
    let initial_generation = scheduler.latest_complete().expect("initial generation");
    assert_eq!(
        initial_generation.generation.snapshot().source_revision,
        Some(
            CommitId::new(git_stdout(fixture.path(), &["rev-parse", "HEAD"]))
                .expect("initial commit id")
        )
    );

    git(
        fixture.path(),
        &["commit", "--allow-empty", "-qm", "same tree"],
    );
    let moved_head =
        CommitId::new(git_stdout(fixture.path(), &["rev-parse", "HEAD"])).expect("moved HEAD");
    let refreshed = published(scheduler.reconcile_now().expect("HEAD reconcile"));
    let served = scheduler.latest_complete().expect("refreshed generation");

    assert_ne!(refreshed.generation_id, initial.generation_id);
    assert_eq!(
        refreshed.snapshot_content_identity, initial.snapshot_content_identity,
        "same tree content remains physically reusable"
    );
    assert_eq!(
        served.generation.snapshot().source_revision,
        Some(moved_head)
    );
    assert_eq!(
        served
            .generation
            .snapshot()
            .reference
            .as_ref()
            .map(RefId::as_str),
        Some("refs/heads/main")
    );
}

/// A text freshness query that arrives while the worker still owns a pass
/// cannot run the ladder itself, and the in-flight pass observed the source
/// when *it* started — after publication it is still projecting text or
/// seating the graph of the previous source state. Answering stale without
/// leaving a wake stranded the remedy until an unrelated hint arrived; the
/// out-of-band commit stayed unserved (issue #917, the flaky tail of
/// `unpinned_query_serves_freshness_resolved_latest_generation`).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn text_freshness_query_during_owner_work_schedules_a_follow_up_pass() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn alpha() -> u32 { 1 }\n")]);
    let store = TempDir::new().expect("store root");
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
        .expect("mount graph-off worktree");
    let initial_text = wait_for_queryable_text_generation(&registry, fixture.path()).await;
    let initial = initial_text.metadata().manifest().generation_id.clone();
    let snapshot = initial_text.metadata().snapshot();
    let scope = ResolvedScope::new(
        test_project_id(),
        snapshot.repository.clone(),
        snapshot.worktree.clone().expect("worktree identity"),
        snapshot.reference.clone(),
    )
    .expect("resolved scope");

    // Let the mount pass finish, then keep the worker from starting another
    // one so the wake this query leaves behind stays observable.
    let settled_deadline = Instant::now() + Duration::from_secs(10);
    while registry
        .reconcile_in_progress_for_test(fixture.path())
        .await
    {
        assert!(
            Instant::now() <= settled_deadline,
            "initial graph-off mount never released its owner pass"
        );
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    let admission = registry
        .background_reconcile_admission()
        .acquire_owned()
        .await
        .expect("hold background reconcile admission");
    registry.clear_pending_wake_for_scope(&scope).await;
    // Stand in for the worker's own pass: in-progress, scheduler mutex free.
    let owner_pass = registry
        .hold_reconcile_pass_for_test(fixture.path())
        .await
        .expect("mounted worktree");
    let scheduler = registry
        .scheduler_handle(fixture.path())
        .await
        .expect("mounted scheduler");
    let reconcile_control = {
        let scheduler = scheduler
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        tracedecay_application::code_index::DaemonCodeIndexControlV1::new(
            Arc::clone(&scheduler.epoch),
            Arc::clone(&scheduler.shutting_down),
        )
    };

    fixture.edit("src/lib.rs", "pub fn alpha() -> u32 { 2 }\n");
    git(fixture.path(), &["commit", "-qam", "external"]);

    let callers = 32;
    let start = Arc::new(tokio::sync::Barrier::new(callers + 1));
    let requests = (0..callers)
        .map(|_| {
            let registry = registry.clone();
            let scope = scope.clone();
            let start = Arc::clone(&start);
            tokio::spawn(async move {
                start.wait().await;
                registry
                    .latest_text_serving_freshness_for_scope(&scope)
                    .await
            })
        })
        .collect::<Vec<_>>();
    start.wait().await;
    for request in requests {
        let (latest, current) = request
            .await
            .expect("freshness query joins")
            .expect("the seated text owner keeps serving during owner work");
        assert_eq!(
            latest.metadata().manifest().generation_id,
            initial,
            "every query is answered from the retained text generation"
        );
        assert!(
            !current,
            "a source the queries could not verify is reported stale, never current"
        );
    }
    let first_follow_up = registry
        .pending_wake_micros_for_scope(&scope)
        .await
        .filter(|pending| *pending != 0)
        .expect("the concurrent queries leave one coalesced follow-up");
    let _ = registry
        .latest_text_serving_freshness_for_scope(&scope)
        .await;
    assert_eq!(
        registry.pending_wake_micros_for_scope(&scope).await,
        Some(first_follow_up),
        "repeated reads preserve the one pending follow-up instead of restamping it"
    );
    assert!(
        !reconcile_control.is_cancelled(),
        "freshness reads do not cancel or restart the owner pass"
    );

    // Model the next worker pass claiming that wake while its owner authority
    // remains held. A source edit observed during this pass must leave another
    // coalesced follow-up, not disappear with the claimed arrival.
    drop(owner_pass);
    let next_owner_pass = registry
        .hold_reconcile_pass_for_test(fixture.path())
        .await
        .expect("mounted worktree");
    registry.clear_pending_wake_for_scope(&scope).await;
    fixture.edit("src/lib.rs", "pub fn alpha() -> u32 { 3 }\n");
    git(
        fixture.path(),
        &["commit", "-qam", "external during follow-up"],
    );
    let (_, current) = registry
        .latest_text_serving_freshness_for_scope(&scope)
        .await
        .expect("retained text remains available during the follow-up pass");
    assert!(!current);
    assert!(
        registry
            .pending_wake_micros_for_scope(&scope)
            .await
            .is_some_and(|pending| pending != 0),
        "the source edit during the follow-up pass leaves the next necessary wake"
    );

    // Release the worker: one BusyFollowUp pass must reconcile the latest
    // source directly, without publishing or retrying the superseded edit.
    drop(next_owner_pass);
    drop(admission);
    let next = wait_for_queryable_text_generation_change(&registry, fixture.path(), &initial).await;
    assert_ne!(next.metadata().manifest().generation_id, initial);
    let current = scheduler
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .capture_authoritative_snapshot_without_active_generation_reuse(None)
        .expect("capture current source");
    assert_eq!(
        next.metadata().snapshot().content_identity,
        current.snapshot.content_identity,
        "the pass serves the edit that arrived during its predecessor"
    );
    let busy_follow_ups = registry
        .event_to_ready_receipts()
        .into_iter()
        .filter(|receipt| receipt.trigger == CodeIndexCadenceTriggerV1::BusyFollowUp)
        .count();
    assert_eq!(
        busy_follow_ups, 1,
        "coalesced reads and superseding edits produce one completed follow-up pass"
    );
    registry.shutdown().await;
}

/// A text owner whose sealed source the fence has verified against the live
/// tree is current even while the worker owns a pass: the read answers from
/// source truth and leaves no follow-up wake. Treating every in-flight pass as
/// staleness made a polling reader and the worker livelock — each read during
/// a `Noop` pass posted a follow-up, the follow-up was another pass, and the
/// owner was never called current although nothing moved (issue #1103). The
/// same read during the same pass must still report a genuine edit stale.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn text_freshness_query_during_owner_work_is_current_when_source_is_unchanged() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn alpha() -> u32 { 1 }\n")]);
    let store = TempDir::new().expect("store root");
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
        .expect("mount graph-off worktree");
    let initial_text = wait_for_queryable_text_generation(&registry, fixture.path()).await;
    let initial = initial_text.metadata().manifest().generation_id.clone();
    let snapshot = initial_text.metadata().snapshot();
    let scope = ResolvedScope::new(
        test_project_id(),
        snapshot.repository.clone(),
        snapshot.worktree.clone().expect("worktree identity"),
        snapshot.reference.clone(),
    )
    .expect("resolved scope");

    let settled_deadline = Instant::now() + Duration::from_secs(10);
    while registry
        .reconcile_in_progress_for_test(fixture.path())
        .await
    {
        assert!(
            Instant::now() <= settled_deadline,
            "initial graph-off mount never released its owner pass"
        );
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    let admission = registry
        .background_reconcile_admission()
        .acquire_owned()
        .await
        .expect("hold background reconcile admission");
    registry.clear_pending_wake_for_scope(&scope).await;
    // Stand in for a worker pass re-observing an unchanged tree: in-progress,
    // scheduler mutex free, nothing moved on disk or in git.
    let owner_pass = registry
        .hold_reconcile_pass_for_test(fixture.path())
        .await
        .expect("mounted worktree");

    let (latest, current) = registry
        .latest_text_serving_freshness_for_scope(&scope)
        .await
        .expect("the seated text owner keeps serving during owner work");
    assert_eq!(latest.metadata().manifest().generation_id, initial);
    assert!(
        current,
        "an unchanged source verified by the fence is current even while a pass is in flight"
    );
    assert_eq!(
        registry.pending_wake_micros_for_scope(&scope).await,
        Some(0),
        "a read the fence proved current leaves no follow-up wake behind"
    );

    // The same in-flight pass, now with a real edit the fence cannot vouch
    // for: the read reports stale and leaves the coalesced follow-up.
    fixture.edit("src/lib.rs", "pub fn alpha() -> u32 { 2 }\n");
    git(fixture.path(), &["commit", "-qam", "external"]);
    let (latest, current) = registry
        .latest_text_serving_freshness_for_scope(&scope)
        .await
        .expect("the retained text owner still serves the stale read");
    assert_eq!(latest.metadata().manifest().generation_id, initial);
    assert!(!current, "a moved source is stale, never current");
    assert!(
        registry
            .pending_wake_micros_for_scope(&scope)
            .await
            .is_some_and(|pending| pending != 0),
        "a stale read during owner work leaves the follow-up wake the pass needs"
    );
    drop(owner_pass);
    drop(admission);
    let next = wait_for_queryable_text_generation_change(&registry, fixture.path(), &initial).await;
    assert_ne!(next.metadata().manifest().generation_id, initial);
    registry.shutdown().await;
}

/// End-to-end proof that the diagnostics identity split is closed.
///
/// The compiler pillar publishes through its real production entry point
/// (`publish_compiler_diagnostics_through_code_index_v1`, the exact call the
/// `tracedecay_diagnose` handler makes) with identity resolved from the real
/// mounted `CodeIndexSchedulerRegistryV1`. The published records are then driven
/// through the real `DiagnosticsStoreLspFeedbackProjection` against a feedback
/// cycle whose impact target carries the same registry-minted identity.
///
/// Before the identity was unified, the producer minted
/// `FileOccurrenceId::new("src/lib.rs")` and its own
/// `generation.diagnostics.compiler.<digest>`, so the projection refused every
/// record with `ImpactTargetFileMismatch` / `GenerationMismatch` and LSP
/// Problems stayed empty. This test asserts admission, not refusal.
#[tokio::test]
async fn compiler_diagnostics_published_under_registry_identity_are_admitted_by_the_lsp_projection()
{
    use std::collections::{BTreeMap, BTreeSet};

    use tracedecay_application::diagnostics_publication::{
        CodeIndexPublicationIdentityPortV1, CompilerDiagnosticPublicationOutcomeV1,
        publish_compiler_diagnostics_through_code_index_v1,
    };
    use tracedecay_application::diagnostics_store::DiagnosticsStore;
    use tracedecay_application::lsp_runtime::{
        DiagnosticsStoreLspFeedbackProjection, LspCodeIndexProjectionIdentityPort,
        LspFeedbackDiagnosticProjectionPort, LspFeedbackDocumentSnapshot,
        LspFeedbackDocumentSnapshotPort, LspFeedbackProjectionScope,
    };
    use tracedecay_domain::feedback::{
        FeedbackCycleId, FeedbackCycleResultV1, FeedbackCycleTerminationV1,
        FeedbackDiagnosticClassificationV1, FeedbackDiagnosticProducerV1,
        FeedbackDiagnosticProjectionV1, FeedbackDurabilityV1, FeedbackFindingId,
        FeedbackFindingLifecycleV1, FeedbackFindingV1, FeedbackImpactStateV1, FeedbackImpactV1,
        FeedbackResultId, FeedbackScopeV1, FeedbackTargetV1, ProviderEvaluationStateV1,
    };
    use tracedecay_domain::{ContentDigest, DiagnosticSeverityV1, SourceSpan, UtcMicros};
    use tracedecay_lsp::{AdmittedRoot, DiagnosticSource, LspRuntimeFailure, LspRuntimeFuture};

    struct FixedDocument(String);

    impl LspFeedbackDocumentSnapshotPort for FixedDocument {
        fn snapshot(
            &self,
            _root: AdmittedRoot,
            _document_uri: String,
        ) -> LspRuntimeFuture<Result<LspFeedbackDocumentSnapshot, LspRuntimeFailure>> {
            let text = self.0.clone();
            Box::pin(async move { Ok(LspFeedbackDocumentSnapshot { text }) })
        }
    }

    fn id<T>(value: &str) -> T
    where
        T: TryFrom<String>,
        <T as TryFrom<String>>::Error: std::fmt::Debug,
    {
        T::try_from(value.to_owned()).expect("valid fixture identity")
    }

    let source = "pub fn alpha() -> u32 {\n    let value: u32 = \"nope\";\n    value\n}\n";
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn alpha() -> u32 { 1 }\n")]);
    fixture.edit("src/lib.rs", source);
    let store_root = TempDir::new().expect("store root");
    let registry = CodeIndexSchedulerRegistryV1::new(1);
    assert!(
        registry
            .mount_worktree(
                test_project_id(),
                fixture.path(),
                store_root.path().to_path_buf(),
                None,
            )
            .await
            .expect("mount daemon-owned scheduler")
    );
    // Publication broadcasts before graph seating. Identity resolve reads the
    // seated snapshot, so wait for that seat rather than the publish event.
    wait_for_live_complete_generation(&registry, fixture.path()).await;

    // The one mint: file identity and generation both come from the registry.
    let identity =
        CodeIndexPublicationIdentityPortV1::resolve(&registry, fixture.path().to_path_buf())
            .await
            .expect("code-index generation identity");
    let (indexed_file, indexed_digest) = identity
        .file("src/lib.rs")
        .expect("code index contains the fixture source");
    let indexed_file = indexed_file.clone();
    let indexed_digest = indexed_digest.clone();
    assert!(
        indexed_file.as_str().starts_with("file.daemon."),
        "registry must mint daemon file identity, got {}",
        indexed_file.as_str()
    );
    let generation = identity.generation_id().clone();

    let database_root = TempDir::new().expect("database root");
    let database_path = database_root.path().join("diagnostics.db");
    let authority = tracedecay_runtime_core::db::DatabaseAuthority::acquire_test(
        &database_path,
        "diagnostics identity admission test",
    )
    .expect("database authority");
    crate::register_test_schema_installer();
    let (database, _guard) = tracedecay_runtime_core::db::Database::publish_test_runtime(
        &database_path,
        &authority,
        tracedecay_runtime_core::db::TestDatabaseRuntimeMode::Initialize,
    )
    .await
    .expect("open diagnostics database");

    let parsed = tracedecay_application::diagnose::parse_cargo_output(
        "error[E0308]: mismatched types\n  --> src/lib.rs:2:22\n",
    );
    assert_eq!(parsed.len(), 1, "fixture cargo output must parse");

    let outcome = {
        let store = DiagnosticsStore::new(database.clone());
        publish_compiler_diagnostics_through_code_index_v1(
            fixture.path(),
            Some(&registry as &dyn CodeIndexPublicationIdentityPortV1),
            &store,
            &parsed,
            tracedecay_application::diagnostics_publication::compiler_diagnostic_analyzer_revision_v1()
                .expect("analyzer revision"),
            tracedecay_application::diagnostics_publication::compiler_diagnostic_configuration_revision_v1()
                .expect("configuration revision"),
            UtcMicros(1_700_000_000_000_000),
        )
        .await
    };
    let CompilerDiagnosticPublicationOutcomeV1::Published {
        generation: published_generation,
        report,
        unresolved,
    } = outcome
    else {
        panic!("compiler publication did not reach the store: {outcome:?}");
    };
    assert!(unresolved.is_empty(), "unexpected skips: {unresolved:?}");
    assert_eq!(report.inserted, 1);
    assert!(report.rejected.is_empty());
    assert_eq!(
        published_generation, generation,
        "records must publish under the code-index generation"
    );

    let record = {
        let store = DiagnosticsStore::new(database.clone());
        store
            .current_records(&generation)
            .await
            .expect("read published records")
            .pop()
            .expect("one published record")
    };
    assert_eq!(record.file_occurrence_id, indexed_file);
    assert_eq!(record.content_digest, indexed_digest);
    assert_eq!(
        record.source_revision, None,
        "a dirty-worktree generation must not be mislabeled as HEAD"
    );

    // The saved-edit cycle's impact target is minted by the same authority, so
    // the projection's identity comparison can succeed.
    let head_commit = git_stdout(fixture.path(), &["rev-parse", "HEAD"]);
    let projection_identity = LspCodeIndexProjectionIdentityPort::current_identity(
        &registry,
        fixture.path().to_path_buf(),
        Some("src/lib.rs".to_owned()),
    )
    .await
    .expect("lsp code-index projection identity");
    let document_file_occurrence_id = projection_identity
        .document_file_occurrence_id
        .clone()
        .expect("document file occurrence identity");
    let document_content_digest: ContentDigest = projection_identity
        .document_content_digest
        .clone()
        .expect("document content digest");
    assert_eq!(document_file_occurrence_id, indexed_file);
    assert_eq!(document_content_digest, indexed_digest);

    let finding = FeedbackFindingV1 {
        finding_id: id::<FeedbackFindingId>("finding.diagnostics.admission"),
        classification: FeedbackDiagnosticClassificationV1::New,
        lifecycle: FeedbackFindingLifecycleV1::Active,
        retrieval_anchor_id: Some(record.diagnostic_anchor.clone()),
        provider_state: ProviderEvaluationStateV1::SupportedCompletedComplete,
        safe_bounded_preview: None,
        diagnostic_projection: None,
    };
    let cycle = FeedbackCycleResultV1 {
        result_id: id::<FeedbackResultId>("result.diagnostics.admission"),
        cycle_id: id::<FeedbackCycleId>("cycle.diagnostics.admission"),
        scope: FeedbackScopeV1 {
            project_id: test_project_id(),
            repository_id: id(record.repository.as_str()),
            worktree_id: id(record
                .worktree
                .as_ref()
                .expect("worktree identity")
                .as_str()),
            branch_ref: "refs/heads/main".to_owned(),
            head_commit_id: id(&head_commit),
        },
        content_identity: None,
        durability: FeedbackDurabilityV1::Durable,
        policy_digest: projection_identity.snapshot_digest.clone(),
        configuration_digest: projection_identity.invalidation_digest.clone(),
        termination: FeedbackCycleTerminationV1::Clean,
        provider_states: vec![ProviderEvaluationStateV1::SupportedCompletedComplete],
        advisory_provider_states: Vec::new(),
        baseline_states: Vec::new(),
        impact: Some(FeedbackImpactV1 {
            target: FeedbackTargetV1 {
                file: indexed_file.clone(),
                span: None,
                symbol: None,
                generation_id: Some(generation.clone()),
            },
            affected_files: vec![indexed_file.clone()],
            affected_callers: Vec::new(),
            affected_tests: Vec::new(),
            evidence_anchors: Vec::new(),
            state: FeedbackImpactStateV1::Partial,
            affected_tests_state: FeedbackImpactStateV1::Partial,
        }),
        impact_state: Some(FeedbackImpactStateV1::Partial),
        affected_tests_state: Some(FeedbackImpactStateV1::Partial),
        findings: vec![finding],
        total_findings: 1,
        returned_findings: 1,
        omitted_findings: 0,
        advisory_only: false,
    };
    let advisory_cycle = FeedbackCycleResultV1 {
        advisory_provider_states: vec![
            tracedecay_domain::feedback::FeedbackAdvisoryProviderStateV1 {
                producer: FeedbackDiagnosticProducerV1::GitHubReview,
                state: ProviderEvaluationStateV1::SupportedCompletedComplete,
            },
        ],
        findings: vec![FeedbackFindingV1 {
            finding_id: id("finding.github.direct-projection"),
            classification: FeedbackDiagnosticClassificationV1::Unknown,
            lifecycle: FeedbackFindingLifecycleV1::Active,
            retrieval_anchor_id: Some(id("anchor.github.evidence-only")),
            provider_state: ProviderEvaluationStateV1::SupportedCompletedComplete,
            safe_bounded_preview: None,
            diagnostic_projection: Some(FeedbackDiagnosticProjectionV1 {
                file: indexed_file.clone(),
                span: SourceSpan {
                    start_byte: 0,
                    end_byte: 2,
                },
                symbol: None,
                code: "github-review".to_owned(),
                severity: DiagnosticSeverityV1::Information,
                safe_bounded_message: "Unresolved GitHub review comment".to_owned(),
                producer: FeedbackDiagnosticProducerV1::GitHubReview,
                code_description_uri: Some(
                    "https://github.com/ScriptedAlchemy/tracedecay/pull/13#discussion_r1"
                        .to_owned(),
                ),
            }),
        }],
        total_findings: 1,
        returned_findings: 1,
        omitted_findings: 0,
        ..cycle.clone()
    };

    let document_uri = url::Url::from_file_path(fixture.path().join("src/lib.rs"))
        .expect("document uri")
        .to_string();
    let root_uri = url::Url::from_file_path(fixture.path())
        .expect("root uri")
        .to_string();
    let projection = DiagnosticsStoreLspFeedbackProjection::new(
        Arc::new(
            tracedecay_application::feedback::diagnostics::DatabaseDiagnosticStore::new(
                database.clone(),
            ),
        ),
        Arc::new(FixedDocument(source.to_owned())),
    );
    let published = projection
        .project(
            AdmittedRoot::new(root_uri.clone()),
            document_uri.clone(),
            LspFeedbackProjectionScope {
                head_commit_id: id(&head_commit),
                code_generation_id: generation.clone(),
                snapshot_digest: projection_identity.snapshot_digest.clone(),
                invalidation_digest: projection_identity.invalidation_digest.clone(),
                snapshot_content_digest: projection_identity.snapshot_content_digest.clone(),
                document_file_occurrence_id: Some(document_file_occurrence_id.clone()),
                document_content_digest: Some(document_content_digest.clone()),
                document_relative_path: Some("src/lib.rs".to_owned()),
                generation: 1,
            },
            cycle,
            BTreeMap::new(),
        )
        .await
        .expect("projection succeeds");

    assert_eq!(
        published.len(),
        1,
        "the published compiler record must be admitted, not skipped"
    );
    assert_eq!(published[0].uri, document_uri);
    assert_eq!(published[0].code.as_deref(), Some("E0308"));
    assert_eq!(
        published[0].source,
        DiagnosticSource::TraceDecay,
        "the compiler pillar must name its own producer source"
    );
    let sources: BTreeSet<_> = published.iter().map(|entry| entry.source).collect();
    assert!(sources.iter().all(|source| source.is_tracedecay()));

    let advisory = projection
        .project(
            AdmittedRoot::new(root_uri),
            document_uri,
            LspFeedbackProjectionScope {
                head_commit_id: id(&head_commit),
                code_generation_id: generation,
                snapshot_digest: projection_identity.snapshot_digest,
                invalidation_digest: projection_identity.invalidation_digest,
                snapshot_content_digest: projection_identity.snapshot_content_digest,
                document_file_occurrence_id: Some(document_file_occurrence_id),
                document_content_digest: Some(document_content_digest),
                document_relative_path: Some("src/lib.rs".to_owned()),
                generation: 2,
            },
            advisory_cycle,
            BTreeMap::new(),
        )
        .await
        .expect("advisory projection succeeds");
    assert_eq!(
        advisory.len(),
        1,
        "a bounded advisory code projection must not require its evidence anchor in the diagnostic store"
    );
    assert_eq!(advisory[0].source, DiagnosticSource::TraceDecayGitHub);
    assert_eq!(advisory[0].code.as_deref(), Some("github-review"));

    registry.shutdown().await;
}

/// A producer with no reachable code-index authority publishes nothing and says
/// so. The former behaviour minted a repository-relative file identity, which
/// the LSP projection could only refuse.
#[tokio::test]
async fn compiler_publication_without_a_resolver_is_named_not_guessed() {
    use tracedecay_application::diagnostics_publication::{
        CompilerDiagnosticPublicationOutcomeV1, publish_compiler_diagnostics_through_code_index_v1,
    };
    use tracedecay_application::diagnostics_store::DiagnosticsStore;
    use tracedecay_domain::{ComponentVersion, UtcMicros};

    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn alpha() -> u32 { 1 }\n")]);
    let database_root = TempDir::new().expect("database root");
    let database_path = database_root.path().join("diagnostics.db");
    crate::register_test_schema_installer();
    let authority = tracedecay_runtime_core::db::DatabaseAuthority::acquire_test(
        &database_path,
        "diagnostics absent resolver",
    )
    .expect("database authority");
    let (database, _guard) = tracedecay_runtime_core::db::Database::publish_test_runtime(
        &database_path,
        &authority,
        tracedecay_runtime_core::db::TestDatabaseRuntimeMode::Initialize,
    )
    .await
    .expect("open diagnostics database");
    let store = DiagnosticsStore::new(database.clone());

    let parsed = tracedecay_application::diagnose::parse_cargo_output(
        "error[E0308]: mismatched types\n  --> src/lib.rs:1:1\n",
    );
    let outcome = publish_compiler_diagnostics_through_code_index_v1(
        fixture.path(),
        None,
        &store,
        &parsed,
        ComponentVersion::new("analyzer.tracedecay-diagnose.test".to_owned())
            .expect("analyzer revision"),
        ComponentVersion::new("configuration.tracedecay-diagnose.v1".to_owned())
            .expect("configuration revision"),
        UtcMicros(1_700_000_000_000_000),
    )
    .await;
    assert_eq!(
        outcome,
        CompilerDiagnosticPublicationOutcomeV1::CodeIndexIdentityUnavailable
    );
}

/// A real branch switch under one worktree reconciles to the same immutable
/// publication inputs and code lanes as a clean scan of the switched-to branch.
#[test]
fn real_branch_switch_reconcile_matches_clean_scan() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn alpha() -> u32 { 1 }\n")]);
    git(fixture.path(), &["switch", "-q", "-c", "feature"]);
    fixture.edit("src/lib.rs", "pub fn alpha() -> u32 { 2 }\n");
    git(fixture.path(), &["commit", "-qam", "feature"]);
    git(fixture.path(), &["switch", "-q", "main"]);

    let store = TempDir::new().expect("store root");
    let bytes = Arc::new(SharedCodeIndexBytePoolV1::default());
    let mut live = scheduler(&fixture, store.path().join("live"), Arc::clone(&bytes));
    published(live.reconcile_now().expect("main baseline"));
    let main_snapshot = live
        .latest_complete()
        .expect("main generation")
        .generation()
        .snapshot()
        .clone();

    git(fixture.path(), &["switch", "-q", "feature"]);
    let switched = published(live.reconcile_now().expect("branch-switch reconcile"));
    let switched_generation = live.latest_complete().expect("switched generation");

    let mut clean = scheduler(&fixture, store.path().join("clean"), bytes);
    let clean_publish = published(clean.reconcile_now().expect("clean feature scan"));
    let clean_generation = clean.latest_complete().expect("clean generation");

    assert_eq!(
        switched.snapshot_content_identity,
        clean_publish.snapshot_content_identity
    );
    assert_eq!(switched.lane_digest, clean_publish.lane_digest);
    assert_eq!(
        main_snapshot.reference.as_ref().map(RefId::as_str),
        Some("refs/heads/main")
    );
    assert_eq!(
        switched_generation
            .generation()
            .snapshot()
            .reference
            .as_ref()
            .map(RefId::as_str),
        Some("refs/heads/feature")
    );
    assert_ne!(
        switched_generation.generation().snapshot().source_revision,
        main_snapshot.source_revision
    );
    assert_eq!(
        switched_generation.generation().snapshot().reference,
        clean_generation.generation().snapshot().reference
    );
    assert_eq!(
        switched_generation.generation().snapshot().source_revision,
        clean_generation.generation().snapshot().source_revision
    );
}

/// A real Git rebase reconciles the rewritten branch tip to the same immutable
/// publication inputs and code lanes as a clean scan of the rebased checkout.
#[test]
fn real_rebase_reconcile_matches_clean_scan() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn alpha() -> u32 { 1 }\n")]);
    git(fixture.path(), &["switch", "-q", "-c", "feature"]);
    write(
        fixture.path(),
        "src/feature.rs",
        "pub fn feature() -> u32 { 2 }\n",
    );
    git(fixture.path(), &["add", "."]);
    git(fixture.path(), &["commit", "-qm", "feature"]);

    let store = TempDir::new().expect("store root");
    let bytes = Arc::new(SharedCodeIndexBytePoolV1::default());
    let mut live = scheduler(&fixture, store.path().join("live"), Arc::clone(&bytes));
    published(live.reconcile_now().expect("pre-rebase baseline"));
    let pre_rebase_revision = live
        .latest_complete()
        .expect("pre-rebase generation")
        .generation()
        .snapshot()
        .source_revision
        .clone();

    git(fixture.path(), &["switch", "-q", "main"]);
    write(
        fixture.path(),
        "src/main_only.rs",
        "pub fn main_only() -> u32 { 3 }\n",
    );
    git(fixture.path(), &["add", "."]);
    git(fixture.path(), &["commit", "-qm", "advance main"]);
    git(fixture.path(), &["switch", "-q", "feature"]);
    git(fixture.path(), &["rebase", "-q", "main"]);

    let rebased = published(live.reconcile_now().expect("rebase reconcile"));
    let rebased_generation = live.latest_complete().expect("rebased generation");

    let mut clean = scheduler(&fixture, store.path().join("clean"), bytes);
    let clean_publish = published(clean.reconcile_now().expect("clean rebased scan"));
    let clean_generation = clean.latest_complete().expect("clean generation");

    assert_eq!(
        rebased.snapshot_content_identity,
        clean_publish.snapshot_content_identity
    );
    assert_eq!(rebased.lane_digest, clean_publish.lane_digest);
    assert_eq!(
        rebased_generation
            .generation()
            .snapshot()
            .reference
            .as_ref()
            .map(RefId::as_str),
        Some("refs/heads/feature")
    );
    assert_ne!(
        rebased_generation.generation().snapshot().source_revision,
        pre_rebase_revision
    );
    assert_eq!(
        rebased_generation.generation().snapshot().reference,
        clean_generation.generation().snapshot().reference
    );
    assert_eq!(
        rebased_generation.generation().snapshot().source_revision,
        clean_generation.generation().snapshot().source_revision
    );
}

/// Restoring a sealed generation must keep queries non-blocking AND schedule a
/// verification reconcile. Open-time clocks alone must not suppress cadence.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mount_with_retained_generation_verifies_cadence_promptly() {
    let fixture = GitFixture::new(RETAINED_REVISION_0);
    let store = TempDir::new().expect("store root");
    let bytes = Arc::new(SharedCodeIndexBytePoolV1::default());
    let scoped_store = super::super::scoped_code_index_store_root(
        store.path(),
        &fixture.path().canonicalize().expect("canonical fixture"),
    );
    let first_generation = {
        let mut scheduler = scheduler(&fixture, scoped_store, Arc::clone(&bytes));
        published(scheduler.reconcile_now().expect("seed generation")).generation_id
    };

    // Out-of-band edit after the sealed generation was written.
    fixture.edit("src/lib.rs", "pub fn retained_revision() -> usize { 1 }\n");
    git(fixture.path(), &["commit", "-qam", "stale-after-seal"]);

    let registry = CodeIndexSchedulerRegistryV1::new(1);
    registry
        .mount_worktree(
            test_project_id(),
            fixture.path(),
            store.path().to_path_buf(),
            None,
        )
        .await
        .expect("mount with retained generation");

    // The retained generation is not queryable until its freshness frontier is
    // proved. The mount wake must verify against gix and publish the new
    // content. "Published and queryable" is the text seat: the retained stale
    // generation may take the graph-bearing serving slot first, and the
    // successor's graph seating is not what this cadence test measures.
    let refreshed =
        wait_for_queryable_text_generation_change(&registry, fixture.path(), &first_generation)
            .await
            .metadata()
            .manifest()
            .generation_id
            .clone();
    assert_ne!(refreshed, first_generation);

    // Early publish records the Published receipt on the source pass; a
    // later graph/verify Noop can become `latest`. Wait for a Published
    // receipt in the set, not only the newest one.
    let published_deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if registry
            .event_to_ready_receipts()
            .iter()
            .any(|receipt| matches!(receipt.outcome, CodeIndexCadenceOutcomeV1::Published { .. }))
        {
            break;
        }
        assert!(
            Instant::now() <= published_deadline,
            "stale retained generation must publish a refreshed generation"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    registry.shutdown().await;
}

/// Content-identical reconcile after mount verification emits a no-op
/// event-to-ready receipt (zero projection work) rather than a silent discard.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mount_verification_noop_emits_event_to_ready_receipt() {
    let fixture = GitFixture::new(ALPHA_LIB_V1);
    let store = TempDir::new().expect("store root");
    let bytes = Arc::new(SharedCodeIndexBytePoolV1::default());
    let scoped_store = super::super::scoped_code_index_store_root(
        store.path(),
        &fixture.path().canonicalize().expect("canonical fixture"),
    );
    {
        let mut scheduler = scheduler(&fixture, scoped_store.clone(), Arc::clone(&bytes));
        published(scheduler.reconcile_now().expect("seed generation"));
    }
    // Exercise the mount-time verification of a restored generation that carries
    // NO freshness witness (an older seal, or a witness that never landed): the
    // mount must still schedule a verification pass that emits a no-op receipt.
    // The witness-present fast path (mount skips the reconcile) is covered by
    // `witness_verified_mount_skips_reconcile`.
    std::fs::remove_file(scoped_store.join("freshness_witness.v1"))
        .expect("remove restore-time freshness witness");

    let registry = CodeIndexSchedulerRegistryV1::new(1);
    registry
        .mount_worktree(
            test_project_id(),
            fixture.path(),
            store.path().to_path_buf(),
            None,
        )
        .await
        .expect("mount retained");
    let receipt = wait_for_event_to_ready(&registry).await;
    assert!(
        receipt.is_noop(),
        "unchanged retained content must emit a no-op event-to-ready receipt"
    );
    // The mount wake is an observed arrival, so queue wait and total latency are
    // both measurable, and the dequeue stamp keeps them distinct measurements.
    let queue_delay = receipt
        .queue_delay_micros()
        .expect("mount wake arrival is observed");
    let event_to_ready = receipt
        .event_to_ready_micros()
        .expect("mount wake arrival is observed");
    assert!(
        queue_delay <= event_to_ready,
        "queue wait ({queue_delay}) cannot exceed event-to-ready ({event_to_ready})"
    );
    assert_eq!(
        event_to_ready,
        queue_delay + receipt.service_micros(),
        "event-to-ready must decompose into queue wait plus service time"
    );
    assert_eq!(receipt.trigger, CodeIndexCadenceTriggerV1::Mount);

    // The read model is reachable from the production registry surface.
    let read_model = registry.cadence_read_model();
    assert!(read_model.retained_count >= 1);
    assert!(
        read_model.capacity >= super::super::cadence::P99_MINIMUM_SAMPLES,
        "ring must be able to hold a p99-eligible population"
    );
    assert_eq!(read_model.latency_sample_count, read_model.retained_count);
    assert_eq!(read_model.arrival_unavailable_count, 0);
    assert!(
        !read_model.event_to_ready_micros.p99.is_available(),
        "p99 must stay unavailable until 100 samples are retained"
    );
    assert!(
        registry
            .latest_generation_id(fixture.path())
            .await
            .is_some(),
        "the verified generation becomes serving state"
    );
    registry.shutdown().await;
}

/// A mount whose restored generation is proved current by its freshness witness
/// activates that generation without rebuilding it.
#[tokio::test(flavor = "multi_thread")]
async fn witness_verified_mount_activates_without_rebuild() {
    let fixture = GitFixture::new(ALPHA_LIB_V1);
    let store = TempDir::new().expect("store root");
    let bytes = Arc::new(SharedCodeIndexBytePoolV1::default());
    let scoped_store = super::super::scoped_code_index_store_root(
        store.path(),
        &fixture.path().canonicalize().expect("canonical fixture"),
    );
    let seeded = {
        let mut scheduler = scheduler(&fixture, scoped_store.clone(), Arc::clone(&bytes));
        published(scheduler.reconcile_now().expect("seed generation")).generation_id
    };
    assert!(
        scoped_store.join("freshness_witness.v1").is_file(),
        "seeding a generation persists its restore-time freshness witness"
    );

    let registry = CodeIndexSchedulerRegistryV1::new(1);
    registry
        .mount_worktree(
            test_project_id(),
            fixture.path(),
            store.path().to_path_buf(),
            None,
        )
        .await
        .expect("mount retained");

    // The retained owner decodes and proves the frontier in background. A no-op
    // receipt makes that activation observable while the generation identity
    // proves no replacement was published.
    let receipt = wait_for_event_to_ready(&registry).await;
    assert!(
        receipt.is_noop(),
        "a matching witness activates the retained generation without rebuilding"
    );
    assert_eq!(
        registry.latest_generation_id(fixture.path()).await,
        Some(seeded),
        "the witness-verified mount serves the sealed generation without rebuilding"
    );
    registry.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reopened_current_text_generation_resolves_publication_identity_without_graph_seat() {
    let fixture = GitFixture::new(ALPHA_LIB_V1);
    let store = TempDir::new().expect("store root");
    let scoped_store = super::super::scoped_code_index_store_root(
        store.path(),
        &fixture.path().canonicalize().expect("canonical fixture"),
    );
    let (generation, scope) = {
        let mut scheduler = scheduler(
            &fixture,
            scoped_store,
            Arc::new(SharedCodeIndexBytePoolV1::default()),
        );
        let generation =
            published(scheduler.reconcile_now().expect("seed generation")).generation_id;
        let latest = scheduler.latest_complete().expect("seeded generation");
        let snapshot = latest.generation.snapshot();
        let scope = ResolvedScope::new(
            test_project_id(),
            snapshot.repository.clone(),
            snapshot.worktree.clone().expect("worktree id"),
            snapshot.reference.clone(),
        )
        .expect("resolved scope");
        (generation, scope)
    };

    let registry = CodeIndexSchedulerRegistryV1::new(1);
    registry
        .mount_worktree_with_graph_policy(
            test_project_id(),
            fixture.path(),
            store.path().to_path_buf(),
            None,
            super::super::CodeGraphActivationPolicyV1::RefusedByConfiguration,
        )
        .await
        .expect("reopen retained generation");
    let mut serving_changes = registry
        .subscribe_serving_generation_changes(fixture.path())
        .await
        .expect("subscribe to retained serving changes");
    assert!(
        registry.request_complete_generation(fixture.path()).await,
        "mounted worktree admits complete-generation demand"
    );

    let deadline = Instant::now() + Duration::from_secs(5);
    let current = loop {
        if let Some((current, true)) = registry
            .latest_text_serving_freshness_for_scope(&scope)
            .await
            && current.query_owners_are_warm()
        {
            break current;
        }
        assert!(
            Instant::now() <= deadline,
            "reopened text generation did not become current"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    assert!(
        current.uses_partitioned_manifest(),
        "the retained text owner is the partitioned generation authority"
    );
    assert!(
        registry
            .latest_complete_serving_for_scope(&scope)
            .await
            .is_none(),
        "configured graph refusal must leave the full generation unavailable"
    );
    tokio::time::timeout(Duration::from_secs(5), serving_changes.changed())
        .await
        .expect("the current retained text owner wakes deferred consumers")
        .expect("the serving-change channel stays open while mounted");
    let selected = registry
        .latest_feedback_generation_for_scope(fixture.path(), &scope)
        .await
        .expect("the woken consumer selects current retained text authority");
    assert_eq!(selected.metadata().manifest().generation_id, generation);

    let root_identity = tracedecay_application::diagnostics_publication::CodeIndexPublicationIdentityPortV1::resolve(
        &registry,
        fixture.path().to_path_buf(),
    )
    .await
    .expect("current reopened text generation resolves by root");
    let scoped_identity = tracedecay_application::diagnostics_publication::CodeIndexPublicationIdentityPortV1::resolve_current_for_scope(
        &registry,
        fixture.path().to_path_buf(),
        scope,
    )
    .await
    .expect("current reopened text generation resolves by scope");
    assert_eq!(root_identity.generation_id(), &generation);
    assert_eq!(scoped_identity.generation_id(), &generation);
    registry.shutdown().await;
}

/// A retained seal is only a candidate for activation. If source-authority
/// verification fails after the seal decodes, the worker must not copy that
/// unverified generation into the serving slot. The failed arrival remains
/// pending for a later hint, while queries fail fast with the typed unverified
/// state.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_retained_activation_never_installs_unverified_serving_state() {
    let fixture = GitFixture::new(ALPHA_LIB_V1);
    let store = TempDir::new().expect("store root");
    let bytes = Arc::new(SharedCodeIndexBytePoolV1::default());
    let scoped_store = super::super::scoped_code_index_store_root(
        store.path(),
        &fixture.path().canonicalize().expect("canonical fixture"),
    );
    let scope = {
        let mut scheduler = scheduler(&fixture, scoped_store, bytes);
        published(scheduler.reconcile_now().expect("seed generation"));
        let latest = scheduler.latest_complete().expect("seeded generation");
        let snapshot = latest.generation.snapshot();
        ResolvedScope::new(
            test_project_id(),
            snapshot.repository.clone(),
            snapshot.worktree.clone().expect("worktree id"),
            snapshot.reference.clone(),
        )
        .expect("resolved scope")
    };

    let registry = CodeIndexSchedulerRegistryV1::with_background_reconcile_permits(1, 1);
    let admission = registry
        .background_reconcile_admission()
        .acquire_owned()
        .await
        .expect("hold background activation");
    registry
        .mount_worktree(
            test_project_id(),
            fixture.path(),
            store.path().to_path_buf(),
            None,
        )
        .await
        .expect("mount retained generation");

    // Hold the scheduler after the worker enters the reconcile pass. The
    // canonical in-progress authority proves the worker owns admission while
    // the lock keeps the fallible retained activation from completing.
    let scheduler = {
        let mounted = registry.mounted.lock().await;
        Arc::clone(
            &mounted
                .get(&fixture.path().canonicalize().expect("canonical root"))
                .expect("mounted worktree")
                .scheduler,
        )
    };
    let (held_tx, held_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let lock_thread = std::thread::spawn(move || {
        let _guard = scheduler
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        held_tx.send(()).expect("signal scheduler held");
        let _ = release_rx.recv();
    });
    held_rx.recv().expect("scheduler lock acquired");

    let git_dir = fixture.path().join(".git");
    let unavailable_git_dir = fixture.path().join(".git.activation-unavailable");
    std::fs::rename(&git_dir, &unavailable_git_dir).expect("make Git authority unavailable");
    drop(admission);

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        if registry
            .reconcile_in_progress_for_test(fixture.path())
            .await
        {
            break;
        }
        assert!(
            std::time::Instant::now() <= deadline,
            "worker did not enter the retained activation pass"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    release_tx.send(()).expect("release scheduler");
    lock_thread.join().expect("join scheduler holder");

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        if registry
            .pending_wake_micros_for_scope(&scope)
            .await
            .is_some_and(|pending| pending != 0)
        {
            break;
        }
        assert!(
            std::time::Instant::now() <= deadline,
            "failed activation did not restore its pending retry arrival"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    assert_eq!(
        registry.latest_generation_id(fixture.path()).await,
        None,
        "a retained generation cannot serve after its activation fails"
    );
    match tokio::time::timeout(
        Duration::from_millis(250),
        registry.execute_query_search(&scope, core_search_request("alpha")),
    )
    .await
    .expect("an unavailable generation must not block its query")
    {
        Err(super::super::query_runtime::QuerySearchExecutionErrorV1::GenerationUnverified) => {}
        Err(other) => panic!("expected the typed unverified state, got {other:?}"),
        Ok(_) => panic!("failed activation must not degrade into a stale answer"),
    }

    // A later real hint retries the retained owner. Restoring Git truth lets
    // the retry prove and activate the exact retained generation.
    std::fs::rename(&unavailable_git_dir, &git_dir).expect("restore Git authority");
    assert!(
        registry.notify_hook_overflow(fixture.path()).await,
        "restored worktree accepts a retry hint"
    );
    let receipts_before = registry.event_to_ready_receipts().len();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let seated = registry
            .latest_generation_id(fixture.path())
            .await
            .is_some();
        let receipts_after = registry.event_to_ready_receipts().len();
        if seated && receipts_after > receipts_before {
            break;
        }
        assert!(
            std::time::Instant::now() <= deadline,
            "successful retry did not seat the verified retained generation with a cadence receipt; seated={seated} before={receipts_before} after={receipts_after}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    registry.shutdown().await;
}

/// A verified sealed generation is independently useful to the exact and
/// lexical lanes. Refusing the optional native graph under the process memory
/// ceiling must therefore degrade only graph capability instead of withholding
/// the generation from every query surface.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resident_memory_graph_refusal_seats_text_serving_without_graph() {
    let fixture = GitFixture::new(ALPHA_LIB_V1);
    let store = TempDir::new().expect("store root");
    let scoped_store = super::super::scoped_code_index_store_root(
        store.path(),
        &fixture.path().canonicalize().expect("canonical fixture"),
    );
    let (scope, worktree_id) = {
        let mut scheduler = scheduler(
            &fixture,
            scoped_store,
            Arc::new(SharedCodeIndexBytePoolV1::default()),
        );
        published(scheduler.reconcile_now().expect("seed generation"));
        let latest = scheduler.latest_complete().expect("seeded generation");
        let snapshot = latest.generation.snapshot();
        let worktree_id = snapshot.worktree.clone().expect("worktree id");
        (
            ResolvedScope::new(
                test_project_id(),
                snapshot.repository.clone(),
                worktree_id.clone(),
                snapshot.reference.clone(),
            )
            .expect("resolved scope"),
            worktree_id,
        )
    };
    super::super::graph_activation::set_injected_resident_memory_refusal(&worktree_id, true);

    let registry = CodeIndexSchedulerRegistryV1::with_background_reconcile_permits(1, 1);
    registry
        .mount_worktree(
            test_project_id(),
            fixture.path(),
            store.path().to_path_buf(),
            None,
        )
        .await
        .expect("mount retained generation");

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let latest = loop {
        if let Some(latest) = registry.latest_complete_serving_for_scope(&scope).await
            && latest.query_owners_are_warm()
        {
            break latest;
        }
        assert!(
            std::time::Instant::now() <= deadline,
            "resident graph refusal withheld the text-serving generation"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    assert!(
        latest.production_query_owners().is_ok(),
        "exact and lexical owners remain serving under graph refusal"
    );
    assert!(
        latest.interactive_graph_store().is_err(),
        "a budget-refused native graph must not gain a substitute store"
    );
    let freshness = registry
        .dashboard_freshness(fixture.path())
        .await
        .expect("mounted worktree freshness");
    match freshness.code_graph_serving {
        Some(
            tracedecay_contracts::code_index_freshness::CodeGraphServingReadinessV1::Refused {
                reason,
            },
        ) => assert_eq!(
            reason,
            super::super::graph_activation::RESIDENT_MEMORY_GRAPH_REFUSAL_REASON,
            "the graph refusal keeps the canonical resident-memory reason"
        ),
        other => panic!(
            "text serving must not turn the refused graph into strict graph readiness: {other:?}"
        ),
    }

    super::super::graph_activation::set_injected_resident_memory_refusal(&worktree_id, false);
    registry.shutdown().await;
}

/// A benign Git metadata rewrite after a clean graph-off seal must trigger one
/// authoritative text-only capture, not fall through to the graph-bearing
/// reconcile path. The captured source identity is unchanged, so the retained
/// generation becomes current without decoding its full sealed payload.
#[test]
fn graph_off_stale_witness_reconciles_unchanged_source_without_full_decode() {
    let fixture = GitFixture::new(ALPHA_LIB_V1);
    let store = TempDir::new().expect("store root");
    let bytes = Arc::new(SharedCodeIndexBytePoolV1::default());
    let seeded = {
        let mut scheduler = scheduler(&fixture, store.path().to_path_buf(), Arc::clone(&bytes));
        published(scheduler.reconcile_now().expect("seed retained generation"))
    };
    let index_path = fixture.path().join(".git/index");
    let index_mtime = std::fs::metadata(&index_path)
        .expect("git index metadata")
        .modified()
        .expect("git index mtime");
    filetime::set_file_mtime(
        &index_path,
        filetime::FileTime::from_system_time(index_mtime + Duration::from_secs(2)),
    )
    .expect("advance only the git index mtime");

    let mut reopened = scheduler(&fixture, store.path().to_path_buf(), bytes);
    assert_eq!(
        reopened.sealed_decode_count(),
        0,
        "foreground reopen must remain decode-free"
    );
    let metadata = reopened
        .servable_retained_text_generation()
        .expect("authenticated retained text generation")
        .metadata()
        .clone();
    assert_eq!(
        reopened.sealed_decode_count(),
        0,
        "partitioned text metadata must not enter the full-generation decoder"
    );
    let outcome = reopened
        .reconcile_retained_text_generation_with(&metadata, true)
        .expect("graph-off retained reconcile")
        .expect("stale witness must be verified by text-only capture");
    let CodeIndexReconcileOutcomeV1::Noop(evidence) = outcome else {
        panic!("metadata-only Git change must not publish a generation");
    };
    assert_eq!(
        evidence.snapshot_content_identity, seeded.snapshot_content_identity,
        "authoritative capture proves the sealed source identity is unchanged"
    );
    assert_eq!(
        reopened.sealed_decode_count(),
        0,
        "graph-off freshness verification must not decode the full generation"
    );
    assert!(
        reopened.verified_against_source(),
        "successful text-only capture establishes current source truth"
    );
}

/// A query freshness probe against a restored owner that no pass has verified
/// yet must report "not current" — the restart's first pass is still the
/// remedy — without minting an observed source change: no overflow hint and no
/// cancellation epoch, because nothing was observed to move. The fabricated
/// overflow made the graph-on restart's own verifying pass skip the
/// sealed-digest witness a quiet tree satisfies and fall into the full sealed
/// replay (`sealed_decode_count` 1) that the revision-7 verified-head recovery
/// exists to avoid. Proven movement still posts the observed change.
#[test]
fn unverified_restart_probe_requests_a_pass_without_fabricating_an_observed_change() {
    let fixture = GitFixture::new(ALPHA_LIB_V1);
    let store = TempDir::new().expect("store root");
    let bytes = Arc::new(SharedCodeIndexBytePoolV1::default());
    {
        let mut scheduler = scheduler(&fixture, store.path().to_path_buf(), Arc::clone(&bytes));
        published(scheduler.reconcile_now().expect("seed retained generation"));
    }

    let mut reopened = scheduler(&fixture, store.path().to_path_buf(), bytes);
    let metadata = reopened
        .servable_retained_text_generation()
        .expect("authenticated retained text generation")
        .metadata()
        .clone();
    let epoch_before = reopened.epoch.load(std::sync::atomic::Ordering::Acquire);
    assert!(
        reopened.request_fresh_for_query_background(),
        "an owner nothing has verified yet is not current"
    );
    assert_eq!(
        reopened.pending_hint_count(),
        Some(0),
        "an unverified probe observed no source change and must not post an overflow hint"
    );
    assert_eq!(
        reopened.epoch.load(std::sync::atomic::Ordering::Acquire),
        epoch_before,
        "an unverified probe must not mint a cancellation epoch"
    );

    let outcome = reopened
        .reconcile_retained_text_generation_with(&metadata, false)
        .expect("graph-on retained reconcile")
        .expect("a quiet tree must be proven by the sealed-digest witness, not replayed");
    assert!(
        matches!(outcome, CodeIndexReconcileOutcomeV1::Noop(_)),
        "the unchanged restart must reconcile as a Noop: {outcome:?}"
    );
    assert_eq!(
        reopened.sealed_decode_count(),
        0,
        "the witness path must not decode the sealed generation"
    );
    assert!(
        !reopened.request_fresh_for_query_background(),
        "the verified owner is current"
    );

    // Proven movement is still an observed source change.
    let index_path = fixture.path().join(".git/index");
    let index_mtime = std::fs::metadata(&index_path)
        .expect("git index metadata")
        .modified()
        .expect("git index mtime");
    filetime::set_file_mtime(
        &index_path,
        filetime::FileTime::from_system_time(index_mtime + Duration::from_secs(2)),
    )
    .expect("advance only the git index mtime");
    assert!(
        reopened.request_fresh_for_query_background(),
        "moved git metadata must request a reconcile"
    );
    assert_eq!(
        reopened.pending_hint_count(),
        None,
        "proven movement posts the overflow hint"
    );
    assert_ne!(
        reopened.epoch.load(std::sync::atomic::Ordering::Acquire),
        epoch_before,
        "proven movement mints the observed-change epoch"
    );
}

#[test]
fn graph_off_change_after_capture_refuses_stale_publication() {
    let fixture = GitFixture::new(&[(
        "src/lib.rs",
        "pub fn retained_capture_race() -> u32 { 1 }\n",
    )]);
    let store = TempDir::new().expect("store root");
    let mut scheduler = scheduler(
        &fixture,
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    );
    published(scheduler.reconcile_now().expect("publish generation A"));
    let pointer_a = scheduler
        .publication
        .read_publication_pointer()
        .expect("read generation A pointer")
        .expect("generation A pointer");
    let metadata_a = scheduler
        .servable_retained_text_generation()
        .expect("verified generation A text handle")
        .metadata()
        .clone();

    fixture.edit(
        "src/lib.rs",
        "pub fn retained_capture_race() -> u32 { 2 }\n",
    );
    scheduler.request_background_reconcile_for_observed_change();
    let stale_capture = scheduler
        .capture_retained_reconcile_attempt()
        .expect("capture generation B source")
        .expect("generation B capture remains current");

    fixture.edit(
        "src/lib.rs",
        "pub fn retained_capture_race() -> u32 { 3 }\n",
    );
    scheduler.request_background_reconcile_for_observed_change();
    let refused = scheduler
        .finish_retained_reconcile(
            &metadata_a,
            RestoreFreshnessWitnessV1::load(store.path()),
            stale_capture,
        )
        .expect("refuse superseded retained capture");

    assert!(
        refused.is_none(),
        "a retained capture superseded after hint drain must not publish"
    );
    assert_eq!(
        scheduler
            .publication
            .read_publication_pointer()
            .expect("read pointer after refusal")
            .expect("active pointer after refusal"),
        pointer_a,
        "refusing the stale capture must preserve generation A"
    );

    let outcome = scheduler
        .reconcile_retained_text_generation_with(&metadata_a, true)
        .expect("retry retained reconcile")
        .expect("retry outcome");
    let published = published(outcome);
    let current = scheduler
        .capture_authoritative_snapshot_without_active_generation_reuse(None)
        .expect("capture current generation C source");
    assert_eq!(
        published.snapshot_content_identity, current.snapshot.content_identity,
        "the retry must publish the source state that superseded generation B"
    );
}

/// A graph-off changed-source rebuild must use the same canonical worker-memory
/// admission as the complete reconcile path. A denial occurs before capture,
/// leaves the durable pointer and hint authority intact, and releases its RAII
/// reservation so an adequately funded retry can publish without decoding A.
#[test]
fn graph_off_changed_source_worker_memory_denial_retries_without_decode() {
    let fixture = GitFixture::new(&[(
        "src/lib.rs",
        "pub fn graph_off_memory_alpha() -> usize { 1 }\n",
    )]);
    let store = TempDir::new().expect("store root");
    let mut scheduler = scheduler(
        &fixture,
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    );
    published(scheduler.reconcile_now().expect("seed generation A"));
    let generation_a = scheduler
        .publication
        .read_publication_pointer()
        .expect("read generation A pointer")
        .expect("generation A pointer");
    let metadata_a = scheduler
        .servable_retained_text_generation()
        .expect("verified generation A text handle")
        .metadata()
        .clone();

    fixture.edit(
        "src/lib.rs",
        "pub fn graph_off_memory_beta() -> usize { 2 }\n",
    );
    git(
        fixture.path(),
        &["commit", "-qam", "publish memory generation B"],
    );
    scheduler.notify_path(fixture.path().join("src/lib.rs"));

    let tight_limit = NonZeroU64::new(1024 * 1024).expect("tight canonical limit");
    assert!(
        tracedecay_code_index::parallelism::worker_reservation_bytes(
            tracedecay_code_index::parallelism::indexing_workers(),
        ) > tight_limit.get(),
        "the fixture limit must deny the installed worker plan"
    );
    let tight = Arc::new(ProcessResidentMemoryV1::new(tight_limit));
    scheduler.bind_resident_memory(Arc::clone(&tight));
    let denied = scheduler.reconcile_retained_text_generation_with(&metadata_a, true);
    assert!(
        matches!(
            denied,
            Err(super::super::CodeIndexSchedulerErrorV1::WorkerMemoryAdmission(_))
        ),
        "graph-off capture must be refused by canonical worker admission: {denied:?}"
    );
    assert_eq!(
        scheduler
            .publication
            .read_publication_pointer()
            .expect("read pointer after denial")
            .expect("active pointer after denial"),
        generation_a,
        "denied capture must leave generation A durable"
    );
    assert_eq!(
        tight.snapshot().used_bytes,
        0,
        "a denied worker reservation must not leak a charge"
    );

    let adequate = Arc::new(ProcessResidentMemoryV1::new(
        DEFAULT_PROCESS_RESIDENT_MEMORY_LIMIT_V1,
    ));
    scheduler.bind_resident_memory(Arc::clone(&adequate));
    let outcome = scheduler
        .reconcile_retained_text_generation_with(&metadata_a, true)
        .expect("adequately funded graph-off retry")
        .expect("graph-off retry outcome");
    let generation_b = published(outcome);
    assert_ne!(
        generation_b.generation_id.as_str(),
        generation_a.generation_id
    );
    assert_eq!(
        scheduler
            .publication
            .read_publication_pointer()
            .expect("read generation B pointer")
            .expect("generation B pointer")
            .generation_id,
        generation_b.generation_id.as_str()
    );
    assert_eq!(scheduler.sealed_decode_count(), 0);
    assert!(
        adequate.snapshot().charges.iter().all(|charge| {
            charge.key.component.as_str() != super::super::CODE_INDEX_WORKER_RESIDENT_COMPONENT_V1
        }),
        "the successful retry must release its worker reservation"
    );
}

/// Publishing a changed graph-off generation must hand text authority from the
/// ready prior generation to the new durable pointer. Otherwise the worker's
/// graph-only `latest` slot stays empty, neither serving swap runs, and queries
/// can serve the old generation forever even though the pointer names newer
/// source.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn graph_off_changed_source_advances_text_authority_without_full_decode() {
    let mut sources = (0..256)
        .map(|index| {
            (
                format!("src/file_{index:04}.rs"),
                format!("pub fn alpha_{index:04}() -> usize {{ {index} }}\n"),
            )
        })
        .collect::<Vec<_>>();
    sources.push(("revision.marker".to_owned(), "generation A\n".to_owned()));
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
            scoped_store,
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

    let ready_deadline = std::time::Instant::now() + Duration::from_secs(10);
    let generation_a = loop {
        match registry
            .execute_query_search(&scope, core_search_request("alpha_0000"))
            .await
        {
            Ok(executed) => break executed.generation,
            Err(error) => {
                assert!(
                    std::time::Instant::now() <= ready_deadline,
                    "generation A never became text-queryable: {error}"
                );
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        }
    };
    // The successful query can land while the owner is finishing generation
    // A and legitimately leave one coalesced BusyFollowUp. Let that pass
    // settle before this test injects generation B's publication failure, so
    // the assertion below observes only the failed pass's restored hint.
    let settled_deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let in_progress = registry
            .reconcile_in_progress_for_test(fixture.path())
            .await;
        let pending_wake = registry.pending_wake_micros_for_scope(&scope).await;
        if !in_progress && pending_wake == Some(0) {
            break;
        }
        assert!(
            std::time::Instant::now() <= settled_deadline,
            "generation A follow-up wake did not settle"
        );
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    let owner_epoch_a = {
        let scheduler = scheduler
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(scheduler.sealed_decode_count(), 0);
        let progress = scheduler.build_progress_slot();
        let progress = progress
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(
            progress
                .snapshot()
                .expect("generation A ready progress")
                .generation_id,
            generation_a.as_str()
        );
        progress.owner_epoch
    };

    let durable_generations_root = {
        let mut scheduler = scheduler
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let durable = scheduler.publication.generations_root.clone();
        let blocker = store.path().join("publication-root-blocker");
        std::fs::write(&blocker, b"not a directory").expect("write publication blocker");
        scheduler.publication.generations_root = blocker;
        durable
    };
    fixture.edit(
        "src/file_0000.rs",
        "pub fn beta_0000() -> usize { 10_000 }\n",
    );
    git(fixture.path(), &["commit", "-qam", "publish generation B"]);
    let reconcile_in_progress = {
        Arc::clone(
            &scheduler
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .reconcile_in_progress,
        )
    };
    assert!(
        registry
            .notify_hook_paths(fixture.path(), &["src/file_0000.rs".to_owned()])
            .await,
        "changed source wakes the mounted graph-off owner"
    );

    let attempt_deadline = std::time::Instant::now() + Duration::from_secs(10);
    while reconcile_in_progress.load(std::sync::atomic::Ordering::Acquire) == 0 {
        assert!(
            std::time::Instant::now() <= attempt_deadline,
            "transient publication failure was never attempted"
        );
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    let restore_deadline = std::time::Instant::now() + Duration::from_secs(10);
    while reconcile_in_progress.load(std::sync::atomic::Ordering::Acquire) != 0 {
        assert!(
            std::time::Instant::now() <= restore_deadline,
            "transient publication failure did not terminate"
        );
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    let unpublished_b_generation = {
        let scheduler = scheduler
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert!(
            !scheduler
                .hints
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .paths
                .is_empty(),
            "transient publication failure lost the drained source hint"
        );
        assert_eq!(
            scheduler
                .publication
                .read_publication_pointer()
                .expect("read pointer after transient failure")
                .expect("generation A remains durable")
                .generation_id,
            generation_a.as_str()
        );
        assert_eq!(scheduler.sealed_decode_count(), 0);
        scheduler
            .publication
            .unpublished_candidate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .expect("generation B remains available for a publication-only retry")
            .manifest()
            .generation_id
            .clone()
    };

    {
        let mut scheduler = scheduler
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        // Keep B unpublished until C is committed and its stale retry is
        // checked; the background owner must not publish between those steps.
        fixture.edit(
            "revision.marker",
            "generation C leaves indexed source unchanged\n",
        );
        git(
            fixture.path(),
            &["commit", "-qam", "advance to generation C"],
        );
        scheduler.publication.generations_root = durable_generations_root;
        scheduler.request_background_reconcile_for_observed_change();
        assert!(
            scheduler
                .republish_unpublished_retained_generation()
                .expect("validate publication-only retry against generation C")
                .is_none(),
            "the unpublished generation B must not publish after source advances to C"
        );
        assert_eq!(
            scheduler
                .publication
                .read_publication_pointer()
                .expect("read pointer after stale candidate refusal")
                .expect("generation A remains durable")
                .generation_id,
            generation_a.as_str(),
            "refusing unpublished B must preserve generation A"
        );
    }
    assert!(
        registry
            .notify_hook_paths(fixture.path(), &["src/file_0000.rs".to_owned()])
            .await,
        "retry wake reaches the restored source hint"
    );

    let publication_deadline = std::time::Instant::now() + Duration::from_secs(10);
    let generation_c = loop {
        let durable = match scheduler.try_lock() {
            Ok(scheduler) => scheduler
                .publication
                .read_publication_pointer()
                .expect("read durable active pointer")
                .map(|pointer| pointer.generation_id),
            Err(std::sync::TryLockError::WouldBlock) => None,
            Err(std::sync::TryLockError::Poisoned(poisoned)) => poisoned
                .into_inner()
                .publication
                .read_publication_pointer()
                .expect("read durable active pointer")
                .map(|pointer| pointer.generation_id),
        };
        if let Some(durable) = durable
            && durable != generation_a.as_str()
        {
            break CodeGenerationId::new(durable).expect("generation C id");
        }
        assert!(
            std::time::Instant::now() <= publication_deadline,
            "changed graph-off source never published durable generation C"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    };

    let progress_deadline = std::time::Instant::now() + Duration::from_secs(10);
    let progress_c = loop {
        let progress = match scheduler.try_lock() {
            Ok(scheduler) => {
                let progress = scheduler.build_progress_slot();
                let progress = progress
                    .read()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                Some((progress.owner_epoch, progress.snapshot()))
            }
            Err(std::sync::TryLockError::WouldBlock) => None,
            Err(std::sync::TryLockError::Poisoned(poisoned)) => {
                let scheduler = poisoned.into_inner();
                let progress = scheduler.build_progress_slot();
                let progress = progress
                    .read()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                Some((progress.owner_epoch, progress.snapshot()))
            }
        };
        if let Some((owner_epoch, Some(progress))) = progress
            && progress.generation_id == generation_c.as_str()
            && progress.committed_pages > 0
        {
            assert!(owner_epoch > owner_epoch_a);
            break progress;
        }
        assert!(
            std::time::Instant::now() <= progress_deadline,
            "durable generation C never acquired advancing text authority"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    assert!(progress_c.progress_epoch > 0);
    assert_eq!(
        registry.latest_generation_id(fixture.path()).await,
        Some(generation_c.clone()),
        "generation A stops owning the graph-off query route"
    );
    if let Ok(executed) = registry
        .execute_query_search(&scope, core_search_request("alpha_0000"))
        .await
    {
        assert_ne!(
            executed.generation, generation_a,
            "generation A must not serve after C owns text progress"
        );
    }

    let query_deadline = std::time::Instant::now() + Duration::from_secs(10);
    let executed_c = loop {
        match registry
            .execute_query_search(&scope, core_search_request("beta_0000"))
            .await
        {
            Ok(executed) if executed.generation == generation_c => break executed,
            Ok(executed) => panic!(
                "stale generation {} served after durable C",
                executed.generation
            ),
            Err(error) => {
                assert!(
                    std::time::Instant::now() <= query_deadline,
                    "generation C never became text-queryable: {error}"
                );
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        }
    };
    assert_eq!(
        executed_c
            .authorized
            .fallback
            .public_fallback_lane_coverage
            .get(&RetrieverKind::ExactLiteral),
        Some(&PublicRetrieverStatus::Complete)
    );
    assert_eq!(
        executed_c
            .authorized
            .fallback
            .public_fallback_lane_coverage
            .get(&RetrieverKind::Lexical),
        Some(&PublicRetrieverStatus::Complete)
    );
    assert_eq!(
        executed_c
            .authorized
            .fallback
            .public_fallback_lane_coverage
            .get(&RetrieverKind::Graph),
        Some(&PublicRetrieverStatus::Unavailable)
    );
    {
        let scheduler = scheduler
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let active = scheduler
            .publication
            .load_active_shared()
            .expect("load generation C from the in-memory publication authority")
            .expect("generation C is active");
        let current = scheduler
            .capture_authoritative_snapshot_without_active_generation_reuse(None)
            .expect("capture current generation C revision");
        assert_ne!(
            active.manifest().generation_id,
            unpublished_b_generation,
            "generation B must never become active after generation C is observed"
        );
        assert_eq!(
            active.snapshot().source_revision,
            current.snapshot.source_revision,
            "the successor must record the allow-empty generation C revision"
        );
        assert_eq!(
            scheduler.sealed_decode_count(),
            0,
            "graph-off A-to-C publication must not decode a sealed generation"
        );
    }
    registry.shutdown().await;
}

/// The canonical project setting must reach the scheduler's production policy
/// boundary. A configured refusal still verifies and seats the sealed text
/// generation, while the graph authority is never activated.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pinned_configuration_refuses_native_graph_before_text_serving_swap() {
    let fixture = GitFixture::new(ALPHA_LIB_V1);
    let store = TempDir::new().expect("store root");
    let scoped_store = super::super::scoped_code_index_store_root(
        store.path(),
        &fixture.path().canonicalize().expect("canonical fixture"),
    );
    let scope = {
        let mut scheduler = scheduler(
            &fixture,
            scoped_store,
            Arc::new(SharedCodeIndexBytePoolV1::default()),
        );
        published(scheduler.reconcile_now().expect("seed generation"));
        let latest = scheduler.latest_complete().expect("seeded generation");
        let snapshot = latest.generation.snapshot();
        ResolvedScope::new(
            test_project_id(),
            snapshot.repository.clone(),
            snapshot.worktree.clone().expect("worktree id"),
            snapshot.reference.clone(),
        )
        .expect("resolved scope")
    };

    let configuration_registry =
        crate::config::registry::ConfigurationRegistry::core().expect("configuration registry");
    let setting = tracedecay_domain::configuration::SettingKey::new(
        tracedecay_domain::configuration::INDEX_NATIVE_GRAPH_ACTIVATION_SETTING_KEY,
    )
    .expect("native graph setting key");
    let layer = crate::config::resolver::ConfigurationLayerV1 {
        layer: tracedecay_domain::configuration::ConfigurationLayerIdV1::Project {
            project_id: test_project_id(),
        },
        revision_id: tracedecay_domain::configuration::ConfigurationRevisionId::new(
            "revision.native-graph-refusal.1",
        )
        .expect("configuration revision"),
        entries: std::collections::BTreeMap::from([(
            setting,
            tracedecay_domain::configuration::ConfigurationValueV1::Boolean(false),
        )]),
    };
    let snapshot =
        crate::config::resolver::resolve_configuration(&configuration_registry, &[layer])
            .expect("resolve configured native graph refusal")
            .snapshot;
    let config = tracedecay_configuration::PinnedRuntimeConfiguration::new(
        tracedecay_configuration::RuntimeConfigurationTarget {
            project_id: test_project_id(),
            project_root: fixture.path().to_path_buf(),
        },
        tracedecay_domain::configuration::ConfigurationRevisionId::new(
            "revision.native-graph-refusal.1",
        )
        .expect("pinned configuration revision"),
        snapshot,
    )
    .expect("materialize pinned runtime configuration");
    assert!(!config.config().native_graph_activation);

    let registry = CodeIndexSchedulerRegistryV1::with_background_reconcile_permits(1, 1);
    registry
        .mount_worktree_with_graph_policy(
            test_project_id(),
            fixture.path(),
            store.path().to_path_buf(),
            None,
            super::super::CodeGraphActivationPolicyV1::from_enabled(
                config.config().native_graph_activation,
            ),
        )
        .await
        .expect("mount scheduler under configured graph policy");

    // The identity port answers only for a text owner the fence still proves
    // current, so it must be resolved in the same wait that admits the owner:
    // the fence's bounded proof expires on its own clock, and a pass renewing
    // it republishes the owner. Sampling the owner once and resolving its
    // identity afterwards turned either boundary into a red.
    let deadline = Instant::now() + SERVING_SEAT_FAILURE_CEILING;
    let (latest, identity) = loop {
        if let Some((latest, current)) = registry
            .latest_text_serving_freshness_for_scope(&scope)
            .await
            && latest.query_owners_are_warm()
            && current
            && let Some(identity) = <CodeIndexSchedulerRegistryV1 as tracedecay_application::diagnostics_publication::CodeIndexPublicationIdentityPortV1>::resolve_current_for_scope(
                &registry,
                fixture.path().to_path_buf(),
                scope.clone(),
            )
            .await
        {
            break (latest, identity);
        }
        assert!(
            Instant::now() <= deadline,
            "configured graph refusal withheld the ready text owner's publication identity"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    assert!(
        registry
            .latest_complete_serving_for_scope(&scope)
            .await
            .is_none(),
        "graph-off must leave the complete serving slot empty"
    );
    let indexed_file = latest
        .metadata()
        .snapshot()
        .files
        .first()
        .expect("indexed file");
    assert_eq!(
        identity.generation_id(),
        &latest.metadata().manifest().generation_id
    );
    assert_eq!(
        identity.logical_path(&indexed_file.file_occurrence_id),
        Some(indexed_file.logical_path.as_str())
    );
    registry
        .mount_query_authority(
            fixture.path(),
            &scope,
            query_authority(latest.metadata().manifest().privacy_domain.clone()),
        )
        .await
        .expect("mount retained query authority");
    let executed = registry
        .execute_query_search(&scope, core_search_request("alpha"))
        .await
        .expect("configured refusal preserves exact and lexical query serving");
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
    assert!(latest.query_owners_are_warm());
    assert!(latest.production_query_owners().is_ok());
    assert!(
        registry
            .latest_complete_serving_for_scope(&scope)
            .await
            .is_none(),
        "graph-off must still leave the complete serving slot empty after query"
    );
    registry.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn same_root_remount_updates_retained_graph_policy_before_worker_activation() {
    let fixture = GitFixture::new(ALPHA_LIB_V1);
    let store = TempDir::new().expect("store root");
    let scoped_store = super::super::scoped_code_index_store_root(
        store.path(),
        &fixture.path().canonicalize().expect("canonical fixture"),
    );
    let scope = {
        let mut scheduler = scheduler(
            &fixture,
            scoped_store,
            Arc::new(SharedCodeIndexBytePoolV1::default()),
        );
        published(scheduler.reconcile_now().expect("seed retained generation"));
        let latest = scheduler.latest_complete().expect("retained generation");
        let snapshot = latest.generation.snapshot();
        ResolvedScope::new(
            test_project_id(),
            snapshot.repository.clone(),
            snapshot.worktree.clone().expect("worktree id"),
            snapshot.reference.clone(),
        )
        .expect("resolved scope")
    };

    let registry = CodeIndexSchedulerRegistryV1::with_background_reconcile_permits(1, 1);
    let activation = registry
        .background_reconcile_admission()
        .acquire_owned()
        .await
        .expect("hold retained activation");
    assert!(
        registry
            .mount_worktree_with_graph_policy(
                test_project_id(),
                fixture.path(),
                store.path().to_path_buf(),
                None,
                super::super::CodeGraphActivationPolicyV1::Enabled,
            )
            .await
            .expect("mount initial owner")
    );
    assert!(
        !registry
            .mount_worktree_with_graph_policy(
                test_project_id(),
                fixture.path(),
                store.path().to_path_buf(),
                None,
                super::super::CodeGraphActivationPolicyV1::RefusedByConfiguration,
            )
            .await
            .expect("remount existing owner with configured refusal"),
        "same-root remount must reuse the existing owner"
    );
    drop(activation);

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let latest = loop {
        if let Some((latest, _)) = registry
            .latest_text_serving_freshness_for_scope(&scope)
            .await
            && latest.query_owners_are_warm()
        {
            break latest;
        }
        assert!(
            std::time::Instant::now() <= deadline,
            "updated same-root policy did not seat the retained text-serving owner"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    assert!(
        registry
            .latest_complete_serving_for_scope(&scope)
            .await
            .is_none(),
        "graph-off remount must leave the complete serving slot empty"
    );
    assert!(
        latest.query_owners_are_warm(),
        "same-root graph refusal must preserve retained text hydration"
    );
    assert!(latest.production_query_owners().is_ok());
    registry.shutdown().await;
}

/// A same-root remount is a control-plane reconciliation request, not an
/// expendable text-projection follow-up. If an unhinted edit races the remount,
/// the remount wake must enter the canonical pending-work authority so the
/// graph-off settled-text fast path cannot discard the only source probe.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn graph_off_remount_preserves_an_unhinted_source_reconcile() {
    let fixture = GitFixture::new(ALPHA_LIB_V1);
    let store = TempDir::new().expect("store root");
    let scoped_store = super::super::scoped_code_index_store_root(
        store.path(),
        &fixture.path().canonicalize().expect("canonical fixture"),
    );
    let (scope, generation_a) = {
        let mut scheduler = scheduler(
            &fixture,
            scoped_store,
            Arc::new(SharedCodeIndexBytePoolV1::default()),
        );
        published(scheduler.reconcile_now().expect("seed retained generation"));
        let latest = scheduler.latest_complete().expect("retained generation");
        let snapshot = latest.generation.snapshot();
        (
            ResolvedScope::new(
                test_project_id(),
                snapshot.repository.clone(),
                snapshot.worktree.clone().expect("worktree id"),
                snapshot.reference.clone(),
            )
            .expect("resolved scope"),
            latest.generation.manifest().generation_id.clone(),
        )
    };

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
        .expect("mount graph-off retained generation");
    let settled_deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let text_ready = registry
            .latest_text_serving_freshness_for_scope(&scope)
            .await
            .is_some_and(|(latest, _)| latest.query_owners_are_warm());
        if text_ready
            && !registry
                .reconcile_in_progress_for_test(fixture.path())
                .await
        {
            break;
        }
        assert!(
            Instant::now() <= settled_deadline,
            "graph-off retained generation never settled"
        );
        tokio::time::sleep(Duration::from_millis(2)).await;
    }

    let admission = registry
        .background_reconcile_admission()
        .acquire_owned()
        .await
        .expect("hold worker after remount dequeue");
    registry.clear_pending_wake_for_scope(&scope).await;
    fixture.edit("src/lib.rs", "pub fn beta() -> usize { 2 }\n");
    git(fixture.path(), &["commit", "-qam", "unhinted remount edit"]);

    assert!(
        !registry
            .mount_worktree_with_graph_policy(
                test_project_id(),
                fixture.path(),
                store.path().to_path_buf(),
                None,
                super::super::CodeGraphActivationPolicyV1::RefusedByConfiguration,
            )
            .await
            .expect("remount existing graph-off owner"),
        "same-root remount must reuse the mounted owner"
    );
    assert!(
        registry
            .pending_wake_micros_for_scope(&scope)
            .await
            .is_some_and(|micros| micros != 0),
        "the remount must publish authoritative pending work"
    );
    drop(admission);

    let reconcile_deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let generation = registry.latest_generation_id(fixture.path()).await;
        if generation
            .as_ref()
            .is_some_and(|generation| generation != &generation_a)
        {
            break;
        }
        assert!(
            Instant::now() <= reconcile_deadline,
            "the remount wake never reconciled the unhinted edit"
        );
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    registry.shutdown().await;
}

/// A retryable graph-activation failure may delay only native graph serving.
/// Exact and lexical refresh must still publish a changed worktree generation
/// while activation retries remain isolated to the immutable graph artifact.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retryable_graph_activation_does_not_block_changed_text_generation() {
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
    let bytes = Arc::new(SharedCodeIndexBytePoolV1::default());
    let scoped_store = super::super::scoped_code_index_store_root(
        store.path(),
        &fixture.path().canonicalize().expect("canonical fixture"),
    );
    let (scope, sealed_worktree_id, sealed_generation_id) = {
        let mut scheduler = scheduler(&fixture, scoped_store.clone(), bytes);
        published(scheduler.reconcile_now().expect("seed generation"));
        let latest = scheduler.latest_complete().expect("seeded generation");
        let snapshot = latest.generation.snapshot();
        let worktree_id = snapshot.worktree.clone().expect("seeded worktree id");
        (
            ResolvedScope::new(
                test_project_id(),
                snapshot.repository.clone(),
                worktree_id.clone(),
                snapshot.reference.clone(),
            )
            .expect("resolved scope"),
            worktree_id,
            latest.generation.manifest().generation_id.clone(),
        )
    };
    let generation_files = |scoped_store: &Path| -> usize {
        std::fs::read_dir(scoped_store.join("code-generations-v1")).map_or(0, |entries| {
            entries
                .filter_map(Result::ok)
                .filter(|entry| {
                    entry
                        .file_name()
                        .to_string_lossy()
                        .starts_with("generation-")
                })
                .count()
        })
    };
    assert_eq!(generation_files(&scoped_store), 1);

    super::super::graph_activation::set_injected_activation_failures(
        &sealed_worktree_id,
        usize::MAX,
    );
    let registry = CodeIndexSchedulerRegistryV1::with_background_reconcile_permits(1, 1);
    registry
        .mount_worktree(
            test_project_id(),
            fixture.path(),
            store.path().to_path_buf(),
            None,
        )
        .await
        .expect("mount retained generation");

    let scheduler = registry
        .scheduler_handle(fixture.path())
        .await
        .expect("mounted scheduler");
    // One budget covers both pre-edit observations: they are two stages of
    // the same retained-mount journey. Text owners deliberately install only
    // when the artifact build finishes and the Ready snapshot publishes in
    // the same step, so live build progress and ready text are sequential
    // states, never a simultaneous one.
    let progress_deadline = std::time::Instant::now() + Duration::from_secs(10);
    let progress_mid_build = loop {
        let observed = {
            let scheduler = scheduler
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let progress = scheduler.build_progress_slot();
            let progress = progress
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            progress.snapshot()
        };
        if let Some(progress) = observed
            && progress.committed_pages > 0
            && progress.phase
                != tracedecay_contracts::code_index_freshness::CodeIndexBuildPhaseV1::Ready
        {
            break progress;
        }
        assert!(
            std::time::Instant::now() <= progress_deadline,
            "graph-on text projection never exposed bounded live progress"
        );
        tokio::time::sleep(Duration::from_millis(2)).await;
    };
    assert_eq!(
        progress_mid_build.generation_id,
        sealed_generation_id.as_str(),
        "the live build progress must belong to the retained generation"
    );
    // Exact and lexical owners must finish and serve while native graph
    // activation keeps failing; this ready owner is the pre-edit baseline
    // the changed generation must replace.
    let (text_owner_before_retry, owner_epoch_before_retry, progress_before_retry) = loop {
        let text = registry.latest_text_serving_for_scope(&scope).await;
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
        if let (Some(text), (owner_epoch, Some(progress))) = (text, observed) {
            break (text, owner_epoch, progress);
        }
        assert!(
            std::time::Instant::now() <= progress_deadline,
            "graph retries withheld retained exact and lexical readiness"
        );
        tokio::time::sleep(Duration::from_millis(2)).await;
    };
    assert_eq!(
        text_owner_before_retry.metadata().manifest().generation_id,
        sealed_generation_id,
        "the pre-edit observation must own the retained generation"
    );
    assert_eq!(
        progress_before_retry.generation_id,
        sealed_generation_id.as_str(),
        "the pre-edit progress must belong to the retained generation"
    );

    // Change the worktree only after the retained text owner is observably
    // advancing. The retry journey must replace that exact owner with the new
    // source generation while native graph activation keeps failing.
    fixture.edit("src/extra.rs", "pub fn extra() -> u32 { 2 }\n");

    // Keep waking the worker while activation stays failing. The graph owner
    // remains unavailable, but source refresh must not be held behind it.
    for _ in 0..5 {
        let _ = registry.notify_hook_overflow(fixture.path()).await;
        tokio::time::sleep(Duration::from_millis(120)).await;
    }
    let text_deadline = std::time::Instant::now() + Duration::from_secs(10);
    let (text_owner_after_refresh, refreshed_generation_id) = loop {
        let generation_id = registry.latest_generation_id(fixture.path()).await;
        if let (Some(text), Some(generation_id)) = (
            registry.latest_text_serving_for_scope(&scope).await,
            generation_id,
        ) && generation_id != sealed_generation_id
            && text.query_owners_are_warm()
        {
            break (text, generation_id);
        }
        assert!(
            std::time::Instant::now() <= text_deadline,
            "graph retry backoff withheld the changed exact and lexical generation"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    assert!(
        !text_owner_after_refresh.same_text_owner(&text_owner_before_retry),
        "the changed generation must replace the retained text owner"
    );
    assert_eq!(
        generation_files(&scoped_store),
        2,
        "source refresh must publish exactly one changed generation"
    );
    {
        let scheduler = scheduler
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let progress = scheduler.build_progress_slot();
        let progress = progress
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert!(
            progress.owner_epoch > owner_epoch_before_retry,
            "the changed generation must advance text progress authority"
        );
        let progress = progress
            .snapshot()
            .expect("changed text progress remains visible during graph retry");
        assert_eq!(progress.generation_id, refreshed_generation_id.as_str());
        assert_ne!(progress.generation_id, progress_before_retry.generation_id);
        assert_eq!(
            progress.phase,
            tracedecay_contracts::code_index_freshness::CodeIndexBuildPhaseV1::Ready
        );
    }
    assert!(
        registry
            .latest_complete_serving_for_scope(&scope)
            .await
            .is_none(),
        "retryable graph activation must not expose an unactivated graph owner"
    );

    // Clearing the injected failure lets the scheduled backoff activate the
    // changed generation without resealing it.
    super::super::graph_activation::set_injected_activation_failures(&sealed_worktree_id, 0);
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        if registry
            .latest_complete_serving_for_scope(&scope)
            .await
            .is_some_and(|latest| {
                latest.generation().manifest().generation_id == refreshed_generation_id
            })
        {
            break;
        }
        assert!(
            std::time::Instant::now() <= deadline,
            "the backoff retry did not activate the sealed generation"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    registry.shutdown().await;
}

/// A retryable graph seat owns one scheduled wake. A retained `Noop` pass may
/// consume a notification queued before backoff was armed, but that skip must
/// not enqueue another pass and recursively poll until the retry deadline.
#[test]
fn graph_activation_retry_window_emits_one_self_driven_seat_skip() {
    let mut queued_wake = true;
    let mut seat_skips = 0_usize;
    while queued_wake && seat_skips < 1_000 {
        seat_skips += 1;
        queued_wake = super::super::registry::retained_noop_requires_follow_up_wake(
            true, // no graph generation is seated
            true, // activation retry deadline is still pending
            true, // this pass consumed a real external arrival
            true, // retained source reconciliation completed as Noop
        );
    }

    assert_eq!(
        seat_skips, 1,
        "one deferred pass may report one skip, but must wait for the scheduled retry afterward"
    );
    assert!(
        super::super::registry::retained_noop_requires_follow_up_wake(true, false, true, true),
        "without activation backoff the empty retained Noop still needs its follow-up pass"
    );
    assert!(
        !super::super::registry::retained_noop_requires_follow_up_wake(true, false, false, true),
        "a self-woken pass with no consumed arrival reproduces the identical Noop and must not \
         re-arm itself"
    );
}

/// Dashboard graph readiness belongs to the current sealed text generation,
/// even while an older generation still owns a fully ready graph seat.
#[test]
fn dashboard_graph_readiness_follows_the_current_text_generation() {
    let fixture = GitFixture::new(ALPHA_LIB_V1);
    let store = TempDir::new().expect("store root");
    let scoped_store = super::super::scoped_code_index_store_root(
        store.path(),
        &fixture.path().canonicalize().expect("canonical fixture"),
    );
    let mut scheduler = scheduler(
        &fixture,
        scoped_store,
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    );
    published(scheduler.reconcile_now().expect("seed generation"));
    let old_ready = scheduler.latest_complete().expect("seeded generation");
    old_ready.warm_serving_caches();
    assert_eq!(
        old_ready.code_graph_serving_readiness(),
        tracedecay_contracts::code_index_freshness::CodeGraphServingReadinessV1::Ready
    );

    fixture.edit("src/current.rs", "pub fn current() -> u32 { 2 }\n");
    published(
        scheduler
            .reconcile_now()
            .expect("publish changed generation"),
    );
    let current = scheduler.latest_complete().expect("changed generation");
    assert_eq!(
        dashboard_code_graph_serving(
            Some(&old_ready),
            Some(&current.text_generation_handle()),
            true,
        ),
        Some(tracedecay_contracts::code_index_freshness::CodeGraphServingReadinessV1::Pending),
        "an older Ready graph must not mask the current text generation's Pending state"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn terminal_graph_activation_failure_is_typed_for_current_text_generation() {
    let fixture = GitFixture::new(ALPHA_LIB_V1);
    let store = TempDir::new().expect("store root");
    let scoped_store = super::super::scoped_code_index_store_root(
        store.path(),
        &fixture.path().canonicalize().expect("canonical fixture"),
    );
    let (scope, worktree_id) = {
        let mut scheduler = scheduler(
            &fixture,
            scoped_store,
            Arc::new(SharedCodeIndexBytePoolV1::default()),
        );
        published(scheduler.reconcile_now().expect("seed generation"));
        let latest = scheduler.latest_complete().expect("seeded generation");
        let snapshot = latest.generation.snapshot();
        let worktree_id = snapshot.worktree.clone().expect("worktree id");
        (
            ResolvedScope::new(
                test_project_id(),
                snapshot.repository.clone(),
                worktree_id.clone(),
                snapshot.reference.clone(),
            )
            .expect("resolved scope"),
            worktree_id,
        )
    };
    let activation_gate =
        super::super::graph_activation::install_injected_activation_gate(&worktree_id);

    let registry = CodeIndexSchedulerRegistryV1::with_background_reconcile_permits(1, 1);
    registry
        .mount_worktree(
            test_project_id(),
            fixture.path(),
            store.path().to_path_buf(),
            None,
        )
        .await
        .expect("mount retained generation");
    tokio::time::timeout(
        Duration::from_secs(10),
        activation_gate.wait_until_started(),
    )
    .await
    .expect("graph activation did not reach the terminal-failure boundary");
    super::super::graph_activation::set_injected_terminal_activation_failure(&worktree_id, true);
    activation_gate.release();

    let deadline = Instant::now() + Duration::from_secs(5);
    let reason = loop {
        let freshness = registry
            .dashboard_freshness(fixture.path())
            .await
            .expect("mounted dashboard freshness");
        if let Some(
            tracedecay_contracts::code_index_freshness::CodeGraphServingReadinessV1::Unavailable {
                ref reason,
            },
        ) = freshness.code_graph_serving
            && reason != "generation_unavailable"
        {
            break reason.clone();
        }
        assert!(
            Instant::now() <= deadline,
            "terminal graph activation failure remained pending: {freshness:?}"
        );
        tokio::time::sleep(Duration::from_millis(2)).await;
    };
    assert!(
        reason.contains("injected terminal graph activation failure"),
        "typed failure must retain its terminal cause: {reason}"
    );
    // The owner's projection runs on its own task now, so the terminal graph
    // failure above can be observed before it finishes. Withdrawal is still
    // falsified: a withdrawn owner never becomes warm and this deadline fires.
    let serving_deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if registry
            .latest_text_serving_for_scope(&scope)
            .await
            .is_some_and(|text| text.query_owners_are_warm())
        {
            break;
        }
        assert!(
            Instant::now() <= serving_deadline,
            "terminal native graph failure must not withdraw exact/lexical serving"
        );
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    registry.shutdown().await;
}

/// Decoding the immutable sealed generation for optional graph activation may
/// take tens of seconds on a cold large repository. That decode must not hold
/// the mutable scheduler mutex: exact/lexical freshness still needs its cheap
/// witness check while graph preparation is parked or reading the seal.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn graph_decode_does_not_block_text_freshness() {
    let fixture = GitFixture::new(ALPHA_LIB_V1);
    let store = TempDir::new().expect("store root");
    let scoped_store = super::super::scoped_code_index_store_root(
        store.path(),
        &fixture.path().canonicalize().expect("canonical fixture"),
    );
    let scope = {
        let mut scheduler = scheduler(
            &fixture,
            scoped_store,
            Arc::new(SharedCodeIndexBytePoolV1::default()),
        );
        published(scheduler.reconcile_now().expect("seed generation"));
        let latest = scheduler.latest_complete().expect("seeded generation");
        let snapshot = latest.generation.snapshot();
        ResolvedScope::new(
            test_project_id(),
            snapshot.repository.clone(),
            snapshot.worktree.clone().expect("worktree identity"),
            snapshot.reference.clone(),
        )
        .expect("resolved scope")
    };

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
        .expect("mount text-only retained generation");
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if registry
                .latest_text_serving_freshness_for_scope(&scope)
                .await
                .is_some_and(|(text, current)| text.query_owners_are_warm() && current)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .expect("text generation did not become ready and current");

    let scheduler = registry
        .scheduler_handle(fixture.path())
        .await
        .expect("mounted scheduler");
    let held_decode = scheduler
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .hold_active_decode();
    assert!(
        !registry
            .mount_worktree_with_graph_policy(
                test_project_id(),
                fixture.path(),
                store.path().to_path_buf(),
                None,
                super::super::CodeGraphActivationPolicyV1::Enabled,
            )
            .await
            .expect("enable graph activation on the retained owner"),
        "same-root policy update must retain the mounted owner"
    );
    // The retained policy update posts its own worker wake and claims it as
    // that pass's arrival, so the pass which parks on the decode barrier below
    // is provably the one this update started. A second `notify_hook_overflow`
    // here posted an arrival that raced the graph seat gate: when it landed
    // after the gate it stayed unclaimed for the whole parked-decode window,
    // and the freshness witness reported that outstanding authoritative rescan
    // -- not the decode -- as the reason the text owner was not current.
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if held_decode.waiter_count() > 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .expect("graph preparation did not park on the held decode barrier");
    assert!(
        !registry
            .reconcile_in_progress_for_test(fixture.path())
            .await,
        "the source pass must finish before the optional graph decode"
    );
    let (_, current) = registry
        .latest_text_serving_freshness_for_scope(&scope)
        .await
        .expect("ready text remains queryable during graph decode");
    assert!(
        current,
        "optional graph decode must not block the exact/lexical freshness witness"
    );

    drop(held_decode);
    registry.shutdown().await;
}

/// Busy admission preserves the prior generation and schedules a follow-up wake
/// so serve-during-refresh cannot leave the index stale indefinitely.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn busy_admission_schedules_follow_up_cadence_wake() {
    let fixture = GitFixture::new(ALPHA_LIB_V1);
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
        .expect("mount");
    let expected = wait_for_live_complete_generation(&registry, fixture.path())
        .await
        .generation
        .manifest()
        .generation_id
        .clone();
    let before_receipts = registry.event_to_ready_receipts().len();

    let scheduler = registry
        .scheduler_handle(fixture.path())
        .await
        .expect("scheduler handle");
    let (held_tx, held_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let lock_thread = std::thread::spawn(move || {
        let _guard = scheduler
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        held_tx.send(()).expect("signal held");
        let _ = release_rx.recv();
    });
    held_rx.recv().expect("lock acquired");

    let latest = tokio::time::timeout(
        Duration::from_millis(250),
        registry.latest_complete_fresh(fixture.path()),
    )
    .await
    .expect("must not wait on busy lock")
    .expect("prior generation served");
    assert_eq!(latest.generation.manifest().generation_id, expected);

    release_tx.send(()).expect("release");
    lock_thread.join().expect("join");

    // Follow-up wake must produce another cadence receipt after the lock frees.
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    loop {
        let receipts = registry.event_to_ready_receipts();
        if receipts.len() > before_receipts
            && receipts.iter().any(|receipt| {
                receipt.trigger == CodeIndexCadenceTriggerV1::BusyFollowUp
                    || receipt.trigger == CodeIndexCadenceTriggerV1::Mount
                    || receipt.trigger == CodeIndexCadenceTriggerV1::QueryAdmission
            })
        {
            break;
        }
        assert!(
            std::time::Instant::now() <= deadline,
            "busy follow-up wake did not produce a cadence receipt"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    registry.shutdown().await;
}

/// A successful reconcile is product state; optional lifecycle telemetry may
/// not keep its in-progress guard or freshness response open while the
/// observability store is busy.
#[tokio::test]
async fn blocked_observability_store_does_not_hold_reconcile_readiness() {
    let _pin = tracedecay_runtime_core::config::PinnedUserDataDir::new();
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn alpha() -> u32 { 1 }\n")]);
    let store = TempDir::new().expect("store root");
    let (registry, scope) = mounted_core_query_worktree(&fixture, &store).await;
    let runtime = tracedecay_global_db::tests::harness::RegisteredGlobalDbTestRuntime::project(
        tracedecay_runtime_core::storage::default_profile_root().expect("profile root"),
        fixture.path(),
        scope.project_id.clone(),
    )
    .await
    .expect("registered runtime");
    let database = runtime.project_database_arc().expect("project database");
    let producer = Arc::new(
        tracedecay_application::observability::BoundedObservabilityProducerV1::start(
            database.clone(),
            tracedecay_application::observability::ObservabilityProducerIdentityV1 {
                authorized_scope_ref: scope.project_id.as_str().to_owned(),
                process_boot_id: "boot:code-index-readiness".to_owned(),
                producer_revision: "code-index-readiness-test.v1".to_owned(),
                configuration_revision: "code-index-readiness-config.v1".to_owned(),
                policy_revision: "code-index-readiness-policy.v1".to_owned(),
            },
            8,
        )
        .expect("bounded producer"),
    );
    registry
        .install_index_observability(
            fixture.path(),
            super::super::observability::CodeIndexObservabilityV1::new(Arc::clone(&producer)),
        )
        .await
        .expect("install observability lane");

    let blocked_writer = database
        .begin_write_transaction()
        .await
        .expect("hold observability writer");
    let initial = registry
        .latest_generation_id(fixture.path())
        .await
        .expect("initial generation");
    fixture.edit("src/lib.rs", "pub fn alpha() -> u32 { 2 }\n");
    assert!(
        registry
            .notify_path(fixture.path(), fixture.path().join("src/lib.rs"))
            .await
    );
    let _ = wait_for_generation_change(&registry, fixture.path(), &initial).await;

    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    loop {
        let freshness = registry
            .dashboard_freshness(fixture.path())
            .await
            .expect("dashboard freshness");
        if freshness.staleness_state.as_deref() == Some("fresh") && freshness.coverage == "complete"
        {
            break;
        }
        assert!(
            std::time::Instant::now() <= deadline,
            "optional telemetry held successful reconcile readiness: {freshness:?}"
        );
        tokio::time::sleep(Duration::from_millis(2)).await;
    }

    blocked_writer
        .commit()
        .await
        .expect("release observability writer");
    registry.shutdown().await;
    producer.shutdown().await.expect("flush producer");
}

/// The installed observability lane must persist one canonical index
/// lifecycle observation when a reconcile publishes a generation, and the
/// retrieval-pipeline families when a query composition completes, all in the
/// one project observation store.
#[tokio::test]
async fn installed_observability_lane_records_index_and_retrieval_observations() {
    let _pin = tracedecay_runtime_core::config::PinnedUserDataDir::new();
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
    // Join the text owner that exact/lexical composition serves. Waiting for
    // `latest_complete_fresh` after `wait_for_initial_generation` races the
    // optional graph seat: the wait can return on text while complete-fresh
    // still abstains, or time out on the post-graph publication bus.
    let initial_text = wait_for_queryable_text_generation(&registry, fixture.path()).await;
    let snapshot = initial_text.metadata().snapshot();
    let scope = ResolvedScope::new(
        test_project_id(),
        snapshot.repository.clone(),
        snapshot.worktree.clone().expect("worktree identity"),
        snapshot.reference.clone(),
    )
    .expect("resolved scope");
    registry
        .mount_query_authority(
            fixture.path(),
            &scope,
            query_authority(initial_text.metadata().manifest().privacy_domain.clone()),
        )
        .await
        .expect("mount core query authority");

    let runtime = tracedecay_global_db::tests::harness::RegisteredGlobalDbTestRuntime::project(
        tracedecay_runtime_core::storage::default_profile_root().expect("profile root"),
        fixture.path(),
        scope.project_id.clone(),
    )
    .await
    .expect("registered runtime");
    let database = runtime.project_database_arc().expect("project database");
    let producer = Arc::new(
        tracedecay_application::observability::BoundedObservabilityProducerV1::start(
            database.clone(),
            tracedecay_application::observability::ObservabilityProducerIdentityV1 {
                authorized_scope_ref: scope.project_id.as_str().to_owned(),
                process_boot_id: "boot:code-index-observability".to_owned(),
                producer_revision: "code-index-observability-test.v1".to_owned(),
                configuration_revision: "code-index-observability-config.v1".to_owned(),
                policy_revision: "code-index-observability-policy.v1".to_owned(),
            },
            64,
        )
        .expect("bounded producer"),
    );
    registry
        .install_index_observability(
            fixture.path(),
            super::super::observability::CodeIndexObservabilityV1::new(Arc::clone(&producer)),
        )
        .await
        .expect("install observability lane");

    // A reconcile after installation publishes a new generation and must leave
    // one canonical index lifecycle observation when the source pointer moves,
    // not after optional graph seating.
    let initial = initial_text.metadata().manifest().generation_id.clone();
    fixture.edit("src/lib.rs", "pub fn alpha() -> u32 { 2 }\n");
    assert!(
        registry
            .notify_path(fixture.path(), fixture.path().join("src/lib.rs"))
            .await
    );
    let _ = wait_for_queryable_text_generation_change(&registry, fixture.path(), &initial).await;

    // One real query composition through the mounted authority carries the
    // retrieval-pipeline families through the bounded producer.
    let _executed = registry
        .execute_query_search(&scope, core_search_request("alpha"))
        .await
        .expect("query search");

    registry.shutdown().await;
    producer.shutdown().await.expect("flush producer");

    let port = tracedecay_application::observability::RegisteredObservabilityPortV1::new(
        database.as_ref(),
    );
    let observability_query =
        |event_kinds: Vec<String>| tracedecay_contracts::ObservabilityQueryV1 {
            authorized_scope_ref: scope.project_id.as_str().to_owned(),
            event_kinds,
            horizon: tracedecay_contracts::ObservabilityHorizonV1 {
                since_micros: 0,
                until_micros: i64::MAX,
            },
            after_watermark: None,
            limit: 256,
        };
    let index_page = tracedecay_contracts::ObservabilityQueryPort::query(
        &port,
        observability_query(vec!["index.measurement.observed.v1".to_owned()]),
    )
    .await
    .expect("index lifecycle events");
    assert!(
        index_page.events.iter().any(|event| matches!(
            &event.payload,
            tracedecay_domain::ObservabilityPayloadV1::Index(observation)
                if observation.outcome == tracedecay_domain::IndexOutcomeV1::Published
                    && observation.kind
                        == tracedecay_domain::IndexObservationKindV1::Publication
        )),
        "a published reconcile must leave a canonical publication observation"
    );

    let retrieval_page = tracedecay_contracts::ObservabilityQueryPort::query(
        &port,
        observability_query(vec![
            "retrieval.planner.decided.v1".to_owned(),
            "retrieval.synthesis.completed.v1".to_owned(),
            "retrieval.source.observed.v1".to_owned(),
        ]),
    )
    .await
    .expect("retrieval pipeline events");
    let planner = retrieval_page
        .events
        .iter()
        .find_map(|event| match &event.payload {
            tracedecay_domain::ObservabilityPayloadV1::RetrievalPlanner(planner) => {
                Some(planner.clone())
            }
            _ => None,
        })
        .expect("one planner observation per composition");
    assert_eq!(
        planner.requested_lanes,
        vec!["exact_literal", "lexical", "graph"],
        "the observation reflects the lanes the composition actually ran"
    );
    assert!(
        retrieval_page.events.iter().any(|event| matches!(
            &event.payload,
            tracedecay_domain::ObservabilityPayloadV1::RetrievalSynthesis(_)
        )),
        "the composition's synthesis observation must be persisted"
    );
}

/// A checkout that never holds still must still get graph serving.
///
/// The live failure this covers: graph preparation demanded a `Noop` reconcile
/// outcome, which is only reachable when the tree is unchanged between a
/// publication and the next worker pass. On a shared checkout with peers
/// editing continuously that window never arrives - three full seals produced
/// zero seat attempts, `graph_statistics` stayed
/// `exact_scope_generation_not_ready`, and every exact graph read answered
/// "not ready" while a complete sealed generation sat on disk. Nothing logged,
/// because a missing prepare is not a refusal.
///
/// The assertion deliberately reads the serving slot, not the prepare step:
/// that slot is written only by the serving swap, which runs after graph
/// activation returns. A generation that prepares and activates but never
/// swaps fails this test exactly like one that never prepares.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn continuously_edited_tree_still_seats_the_sealed_graph_generation() {
    let fixture = GitFixture::new(ALPHA_LIB_V1);
    let store = TempDir::new().expect("store root");
    let scoped_store = super::super::scoped_code_index_store_root(
        store.path(),
        &fixture.path().canonicalize().expect("canonical fixture"),
    );
    let scope = {
        let mut scheduler = scheduler(
            &fixture,
            scoped_store,
            Arc::new(SharedCodeIndexBytePoolV1::default()),
        );
        published(scheduler.reconcile_now().expect("seed generation"));
        let latest = scheduler.latest_complete().expect("seeded generation");
        let snapshot = latest.generation.snapshot();
        ResolvedScope::new(
            test_project_id(),
            snapshot.repository.clone(),
            snapshot.worktree.clone().expect("seeded worktree id"),
            snapshot.reference.clone(),
        )
        .expect("resolved scope")
    };

    let registry = Arc::new(CodeIndexSchedulerRegistryV1::with_background_reconcile_permits(1, 1));
    registry
        .mount_worktree(
            test_project_id(),
            fixture.path(),
            store.path().to_path_buf(),
            None,
        )
        .await
        .expect("mount retained generation");

    // Never let the tree settle: every reconcile pass from here on sees a
    // changed checkout, so no pass can report `Noop`. This is the shared-repo
    // condition, reproduced deterministically. A small rotating set of files
    // keeps each seal bounded while every pass still sees different bytes, and
    // the periodic hook overflow is the production wake - nothing watches the
    // filesystem, so without it the worker simply parks.
    let churn_root = fixture.path().to_path_buf();
    let churn_registry = Arc::clone(&registry);
    let churn_stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let churn_flag = Arc::clone(&churn_stop);
    let churn = tokio::spawn(async move {
        let mut revision = 0_u64;
        while !churn_flag.load(std::sync::atomic::Ordering::Acquire) {
            let slot = revision % 4;
            let _ = std::fs::write(
                churn_root.join(format!("src/churn_{slot}.rs")),
                format!("pub fn churn_{slot}() -> u64 {{ {revision} }}\n"),
            );
            revision += 1;
            if revision.is_multiple_of(10) {
                churn_registry.notify_hook_overflow(&churn_root).await;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    });

    let deadline = Instant::now() + Duration::from_mins(1);
    let seated = loop {
        if let Some(seated) = registry.latest_complete_serving_for_scope(&scope).await {
            break Some(seated.generation().manifest().generation_id.clone());
        }
        if Instant::now() > deadline {
            let passes = registry
                .event_to_ready_receipts()
                .iter()
                .map(|receipt| match &receipt.outcome {
                    CodeIndexCadenceOutcomeV1::Published { generation_id, .. } => {
                        format!("published({})", generation_id.as_str())
                    }
                    CodeIndexCadenceOutcomeV1::Noop { .. } => "noop".to_owned(),
                })
                .collect::<Vec<_>>();
            if passes.is_empty() {
                // The background worker never completed a single reconcile, so
                // nothing here is a statement about graph seating. That happens
                // when this host refuses the worker's resident-memory admission
                // (a small memory cgroup against a large core count), and it
                // turns away every worker-driven scheduler test alike.
                eprintln!(
                    "skipping: this host completed no background reconcile pass, so graph \
                     seating cannot be observed (worker admission refused)"
                );
                break None;
            }
            let text = registry.latest_text_serving_for_scope(&scope).await;
            panic!(
                "a sealed generation never seated: graph serving waited for a quiet tree that a \
                 live checkout never provides; passes={passes:?} latest_generation={:?} \
                 text_open={} text_warm={:?}",
                registry.latest_generation_id(fixture.path()).await,
                text.is_some(),
                text.map(|text| text.query_owners_are_warm()),
            );
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };

    // The seat is stale by construction - the tree moved on while it sealed -
    // and the next sealed generation must still supersede it.
    if let Some(seated) = seated {
        let deadline = Instant::now() + Duration::from_mins(1);
        loop {
            let current = registry
                .latest_complete_serving_for_scope(&scope)
                .await
                .map(|latest| latest.generation().manifest().generation_id.clone());
            if current.is_some_and(|current| current != seated) {
                break;
            }
            assert!(
                Instant::now() <= deadline,
                "a stale seat was never superseded by the generation that sealed after it"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    churn_stop.store(true, std::sync::atomic::Ordering::Release);
    let _ = churn.await;
    registry.shutdown().await;
}

/// The seat gate is the text owner, never the tree standing still.
///
/// This is the whole regression in one table. Before the fix the gate demanded
/// a `Noop` reconcile outcome, so the `Published` rows below - the only rows a
/// continuously edited shared checkout ever produces - could not seat, and a
/// complete sealed generation stayed unserved with nothing logged. Every
/// non-seating row now names its reason.
#[test]
fn a_publication_seats_its_own_generation_without_waiting_for_a_quiet_tree() {
    use super::super::registry::GraphSeatGateV1;

    assert_eq!(
        GraphSeatGateV1::decide(true, false, true, true, true),
        GraphSeatGateV1::Prepare,
        "a publication prepares once its own text owner has reopened, however busy the \
         checkout is; the owner's projection runs alongside and the seat joins it"
    );
    assert_eq!(
        GraphSeatGateV1::decide(true, false, true, true, false),
        GraphSeatGateV1::PublishedTextOwnerUnavailable,
        "a publication whose replacement text owner did not reopen must not start graph work"
    );
    assert_eq!(
        GraphSeatGateV1::decide(true, false, true, false, true),
        GraphSeatGateV1::Prepare,
        "an unchanged pass prepares as soon as a retained owner exists to recover a head \
         from: the seat reads that owner's sealed manifest, never its lexical artifact, so a \
         restart resuming an unfinished ngram index serves the graph while text still warms"
    );
    assert_eq!(
        GraphSeatGateV1::decide(true, false, true, false, false),
        GraphSeatGateV1::RetainedGenerationUnavailable,
        "an unchanged pass with no retained owner has no head to recover"
    );
    assert_eq!(
        GraphSeatGateV1::decide(true, true, true, true, true),
        GraphSeatGateV1::ActivationDeferred,
        "a scheduled activation retry owns the next seat attempt"
    );
    assert_eq!(
        GraphSeatGateV1::decide(true, false, false, true, true),
        GraphSeatGateV1::ReconcileUnfinished,
        "a pass with no terminal outcome has nothing to seat"
    );
    assert_eq!(
        GraphSeatGateV1::decide(false, false, true, true, true),
        GraphSeatGateV1::Disabled,
        "graph activation off means no seat and no skip to report"
    );
    assert!(
        GraphSeatGateV1::decide(false, false, true, true, true)
            .skip_reason()
            .is_none(),
        "a worktree with graph activation off is not waiting for a seat"
    );
    assert!(
        [
            GraphSeatGateV1::ReconcileUnfinished,
            GraphSeatGateV1::ActivationDeferred,
            GraphSeatGateV1::PublishedTextOwnerUnavailable,
            GraphSeatGateV1::RetainedGenerationUnavailable,
        ]
        .into_iter()
        .all(|gate| gate.skip_reason().is_some()),
        "every arm that seats nothing must name itself in the log"
    );
}

/// Refusing a redundant activation must never refuse the seat.
///
/// Preparation and activation shared one gate, so an owner whose native graph
/// was already Ready cleared `prepare_graph` and skipped the bind and the
/// serving swap along with the activation call. A restart that restored such
/// an owner therefore left `serving_generation` empty forever: every
/// complete-generation demand answered unavailable while the dashboard read
/// the same owner and reported Ready/fresh/complete. This table pins the
/// separation - every refusal here is activation-only.
#[test]
fn a_redundant_activation_is_refused_without_refusing_the_seat() {
    use super::super::registry::GraphActivationGateV1;

    assert_eq!(
        GraphActivationGateV1::decide(true, true, true, false),
        GraphActivationGateV1::AlreadyServing,
        "a restored owner that already serves a native graph is never activated again, not \
         even when it takes an empty serving slot"
    );
    assert_eq!(
        GraphActivationGateV1::decide(false, true, false, true),
        GraphActivationGateV1::Activate,
        "a generation entering the serving slot installs its native graph"
    );
    assert_eq!(
        GraphActivationGateV1::decide(false, false, false, false),
        GraphActivationGateV1::UnchangedGraph,
        "an unchanged pass over a terminal graph has no effect to install"
    );
    assert_eq!(
        GraphActivationGateV1::decide(false, false, true, false),
        GraphActivationGateV1::Activate,
        "a generation serving text with a still-Pending graph gets one further attempt"
    );
    assert_eq!(
        GraphActivationGateV1::decide(false, false, true, true),
        GraphActivationGateV1::PendingAttemptSpent,
        "that attempt is bounded to one per generation per worker"
    );
    assert!(
        [
            GraphActivationGateV1::AlreadyServing,
            GraphActivationGateV1::UnchangedGraph,
            GraphActivationGateV1::PendingAttemptSpent,
        ]
        .into_iter()
        .all(|gate| !gate.activates()),
        "only the Activate arm may call native graph activation"
    );
}

/// Activation that completes against a moved durable pointer must still seat.
///
/// Graph activation of a large generation is minutes of real work, and the
/// checkout it sealed from keeps moving underneath it. The serving swap
/// answered that by refusing outright - `PublicationConflict` - so a
/// generation that decoded, activated, and was ready to serve was dropped on
/// the floor while the graph route kept answering "not ready" with nothing
/// seated at all. That is the one arm this table pins: an empty serving slot
/// takes the stale seat, and only a slot that already serves may refuse it,
/// because a superseded generation must never move the slot backwards.
#[test]
fn serving_swap_seats_a_generation_whose_publication_moved_while_it_activated() {
    use super::super::registry::ServingSwapOutcomeV1;

    assert_eq!(
        ServingSwapOutcomeV1::decide(false, false, true),
        ServingSwapOutcomeV1::SeatedStale,
        "an activated generation whose pointer moved must seat when no active \
         publication holds the slot — empty, or an incumbent the store \
         superseded as well"
    );
    assert_eq!(
        ServingSwapOutcomeV1::decide(false, true, true),
        ServingSwapOutcomeV1::Superseded,
        "a superseded generation must not displace the active durable publication"
    );
    assert_eq!(
        ServingSwapOutcomeV1::decide(true, false, true),
        ServingSwapOutcomeV1::Seated,
        "the active durable publication seats"
    );
    assert_eq!(
        ServingSwapOutcomeV1::decide(true, true, false),
        ServingSwapOutcomeV1::Offered,
        "an unchanged pass over the serving generation only re-offers semantic admission"
    );
    assert!(
        ServingSwapOutcomeV1::decide(false, false, true).installs()
            && ServingSwapOutcomeV1::decide(true, false, true).installs(),
        "both seating arms write the serving slot"
    );
    assert!(
        !ServingSwapOutcomeV1::decide(false, true, true).installs()
            && !ServingSwapOutcomeV1::decide(true, true, false).installs(),
        "neither refusing arm writes the serving slot"
    );
}
