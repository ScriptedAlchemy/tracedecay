//! Cold-start activation contract for sealed code generations.
//!
//! Decoding a sealed generation is O(store): it re-mints every file's
//! exact-extraction authority (a canonical SHA-256 over every chunk) and
//! repeats the full canonical validation sweep. These tests pin the three
//! properties that keep that work off the request path:
//!
//! - activation decodes and warms once, and the first query re-decodes nothing;
//! - concurrent cold readers share the one in-flight decode;
//! - the active generation stays pinned while superseded generations churn
//!   through the LRU.
//!
//! Plus the invariant none of the above may weaken: a corrupt sealed store
//! still fails every request, with no memoized verdict.

use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use tempfile::TempDir;
use tracedecay_application::ResolvedScope;
use tracedecay_domain::{
    AuthorizationRevision, CodeGenerationId, ComponentRevision, ExactAdmissionRuleRevision,
    PrincipalId, ProjectId, QueryNormalizationRevision, RelationEdgeKindV1, RetrievalCursorKeyId,
    SanitizerRevision, ScoreDomainId,
};
use tracedecay_query::retrieval::QueryAuthorityV1;
use tracedecay_query::retrieval::fusion::RetrievalCursorKeyringV1;

use super::{
    CodeIndexReconcileOutcomeV1, CodeIndexSchedulerRegistryV1, CodeIndexWorktreeSchedulerV1,
    DaemonCodeIndexPublicationStoreV1, SharedCodeIndexBytePoolV1,
};
use crate::privacy::CODE_SOURCE_SANITIZER_VERSION_V1;

fn git(root: &Path, args: &[&str]) {
    let output = Command::new("git")
        .current_dir(root)
        .args(args)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn fixture() -> TempDir {
    let root = TempDir::new().expect("fixture root");
    git(root.path(), &["init", "-q"]);
    git(
        root.path(),
        &["config", "user.email", "activation@test.invalid"],
    );
    git(root.path(), &["config", "user.name", "Activation Test"]);
    write(root.path(), "src/lib.rs", 0);
    git(root.path(), &["add", "src/lib.rs"]);
    git(root.path(), &["commit", "-q", "-m", "fixture"]);
    root
}

fn write(root: &Path, path: &str, revision: usize) {
    let target = root.join(path);
    fs::create_dir_all(target.parent().expect("source parent")).expect("create source parent");
    fs::write(
        target,
        format!("pub fn activation_revision() -> u32 {{ {revision} }}\n"),
    )
    .expect("write fixture source");
}

fn project_id() -> ProjectId {
    ProjectId::new("project.code-index-activation").expect("valid project identity")
}

fn open(project: &Path, store_root: &Path) -> CodeIndexWorktreeSchedulerV1 {
    CodeIndexWorktreeSchedulerV1::open(
        project_id(),
        project,
        store_root.to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    )
    .expect("open worktree scheduler")
}

fn publish(scheduler: &mut CodeIndexWorktreeSchedulerV1) -> CodeGenerationId {
    match scheduler.reconcile_now().expect("reconcile") {
        CodeIndexReconcileOutcomeV1::Published(evidence) => evidence.generation_id,
        CodeIndexReconcileOutcomeV1::Noop(_) => panic!("expected a published generation"),
    }
}

fn publication_store(store_root: &Path) -> DaemonCodeIndexPublicationStoreV1 {
    DaemonCodeIndexPublicationStoreV1::new(
        store_root,
        SanitizerRevision::new(CODE_SOURCE_SANITIZER_VERSION_V1).expect("sanitizer revision"),
    )
    .expect("open publication store")
}

fn search_request(query: &str) -> super::query_runtime::QuerySearchExecutionRequestV1 {
    super::query_runtime::QuerySearchExecutionRequestV1::new(
        query,
        super::query_runtime::QuerySearchExecutionPolicyV1 {
            principal: PrincipalId::new("principal.activation-journey").expect("principal"),
            authorization_revision: AuthorizationRevision::new("authorization.activation-journey")
                .expect("authorization"),
            sanitizer_revision: SanitizerRevision::new(
                tracedecay_query::retrieval::QUERY_SANITIZER_REVISION_V1,
            )
            .expect("query sanitizer"),
            normalization_revision: QueryNormalizationRevision::new(
                tracedecay_query::retrieval::QUERY_NORMALIZATION_REVISION_V1,
            )
            .expect("query normalization"),
            exact_rule_revision: ExactAdmissionRuleRevision::new(
                tracedecay_query::retrieval::QUERY_EXACT_RULE_REVISION_V1,
            )
            .expect("exact rule"),
            lexical_profile_revision: ComponentRevision::new(
                tracedecay_query::retrieval::QUERY_LEXICAL_PROFILE_REVISION_V1,
            )
            .expect("lexical profile"),
            lexical_score_domain: ScoreDomainId::new(
                tracedecay_query::retrieval::QUERY_LEXICAL_SCORE_DOMAIN_V1,
            )
            .expect("lexical score domain"),
            fuzzy_budget: tracedecay_query::retrieval::lexical::MAX_FUZZY_TERM_EXPANSIONS_V1,
            graph_edge_kinds: vec![RelationEdgeKindV1::Calls],
            graph_max_depth: 1,
            page_size: 10,
            cursor: None,
        },
    )
}

async fn mount_query_authority(
    registry: &CodeIndexSchedulerRegistryV1,
    project: &Path,
    scope: &ResolvedScope,
    latest: &super::LatestCompleteCodeIndexV1,
) {
    let (_, accepted, _) =
        crate::application::semantic_runtime::bundled_query_authority().expect("bundled authority");
    let keyring = RetrievalCursorKeyringV1::new(
        latest.generation().manifest().privacy_domain.clone(),
        RetrievalCursorKeyId::new("retrieval-key.activation-journey").expect("cursor key"),
        1,
        vec![11_u8; 32],
        60_000_000,
    )
    .expect("cursor keyring");
    let authority = Arc::new(
        QueryAuthorityV1::new(
            accepted.profile().clone(),
            accepted.diversity().clone(),
            ComponentRevision::new(tracedecay_query::retrieval::QUERY_RANKING_REVISION_V1)
                .expect("ranking revision"),
            keyring,
        )
        .expect("query authority"),
    );
    registry
        .mount_query_authority(project, scope, authority)
        .await
        .expect("mount query authority");
}

#[tokio::test]
async fn production_query_journey_activates_cold_and_serves_warm_edits_stale_until_publish() {
    let project = fixture();
    let store = TempDir::new().expect("store root");
    let registry = CodeIndexSchedulerRegistryV1::new(1);
    let initial_hold = registry
        .background_reconcile_admission()
        .acquire_owned()
        .await
        .expect("hold initial reconcile");
    registry
        .mount_worktree(
            project_id(),
            project.path(),
            store.path().to_path_buf(),
            None,
        )
        .await
        .expect("mount worktree");
    let identity =
        super::identity::IndexingIdentityV1::resolve(project.path()).expect("indexing identity");
    let scope = ResolvedScope::new(
        project_id(),
        identity.repository_id().clone(),
        identity.worktree_id().clone(),
        identity.head_ref().cloned(),
    )
    .expect("resolved scope");

    assert!(matches!(
        registry
            .execute_query_search(&scope, search_request("activation_revision"))
            .await,
        Err(super::query_runtime::QuerySearchExecutionErrorV1::GenerationUnavailable)
    ));
    drop(initial_hold);

    let initial = tokio::time::timeout(std::time::Duration::from_secs(30), async {
        loop {
            if let Some(latest) = registry.latest_complete_ready_for_scope(&scope).await {
                break latest;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("initial background publication");
    let initial_generation = initial.generation().manifest().generation_id.clone();
    mount_query_authority(&registry, project.path(), &scope, &initial).await;
    let retry = tokio::time::timeout(std::time::Duration::from_secs(30), async {
        loop {
            if let Ok(retry) = registry
                .execute_query_search(&scope, search_request("activation_revision"))
                .await
                && !retry.served_stale
            {
                break retry;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("fresh retry after demand activation");
    assert_eq!(retry.generation, initial_generation);

    let edit_hold = registry
        .background_reconcile_admission()
        .acquire_owned()
        .await
        .expect("hold edit reconcile");
    write(project.path(), "src/lib.rs", 2);
    git(project.path(), &["add", "src/lib.rs"]);
    git(project.path(), &["commit", "-q", "-m", "edit"]);
    assert!(
        registry
            .notify_hook_paths(project.path(), &["src/lib.rs".to_owned()])
            .await
    );
    let stale = registry
        .execute_query_search(&scope, search_request("activation_revision"))
        .await
        .expect("warm edit serves prior generation");
    assert!(stale.served_stale);
    assert_eq!(stale.generation, initial_generation);
    drop(edit_hold);

    let published = tokio::time::timeout(std::time::Duration::from_secs(30), async {
        loop {
            if let Some(latest) = registry.latest_complete_ready_for_scope(&scope).await
                && latest.generation().manifest().generation_id != initial_generation
            {
                break latest;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("edited generation publication");
    let retry = tokio::time::timeout(std::time::Duration::from_secs(30), async {
        loop {
            if let Ok(retry) = registry
                .execute_query_search(&scope, search_request("activation_revision"))
                .await
                && !retry.served_stale
            {
                break retry;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("fresh retry after edit publication");
    assert_eq!(
        retry.generation,
        published.generation().manifest().generation_id
    );
    registry.shutdown().await;
}

/// Activation pays the sealed decode and every per-generation derivation. The
/// first query that follows must find all of it already built — no second
/// decode, no lazily deferred O(store) sweep charged to the request.
#[test]
fn activation_warms_the_generation_so_the_first_query_never_redecodes() {
    let project = fixture();
    let store = TempDir::new().expect("store root");
    {
        let mut scheduler = open(project.path(), store.path());
        let _ = publish(&mut scheduler);
    }

    // Cold process view: a brand-new scheduler over an existing sealed store.
    let scheduler = open(project.path(), store.path());
    scheduler.prime_serving_caches();
    let decodes_after_activation = scheduler.sealed_decode_count();
    assert_eq!(
        decodes_after_activation, 1,
        "activation must decode the active generation exactly once"
    );

    // Every per-generation derivation an unpinned query would otherwise build
    // lazily must already exist before the first request is served.
    let latest = scheduler.latest_complete().expect("restored generation");
    assert!(
        latest.generation().is_validated(),
        "activation must run the canonical validation sweep"
    );
    assert!(
        latest.generation().is_exact_admission_warm(),
        "activation must run the exact-admission sweep, not the first query"
    );
    assert!(
        latest.record_index_is_warm(),
        "activation must build the record lookup indices"
    );
    assert!(
        latest.query_owners_are_warm(),
        "activation must build the exact/lexical/graph lane owners"
    );

    // Everything the serving path touches for an unpinned query.
    assert!(!latest.exact().expect("exact admission").is_empty());
    let _ = latest.record_index();
    latest
        .production_query_owners()
        .expect("production lane owners");
    latest
        .test_attribution_authority()
        .expect("test attribution authority");
    // A second request repeats the whole sequence.
    let repeat = scheduler.latest_complete().expect("repeat generation read");
    assert!(!repeat.exact().expect("repeat exact admission").is_empty());

    assert_eq!(
        scheduler.sealed_decode_count(),
        decodes_after_activation,
        "serving a warmed generation must not re-read or re-decode sealed bytes"
    );
    assert!(
        std::ptr::eq(latest.generation(), repeat.generation()),
        "every reader must share the one decoded generation allocation"
    );
}

/// A query that arrives while activation is still decoding must join the
/// in-flight decode, not start a competing O(store) sweep of its own.
#[test]
fn concurrent_cold_readers_share_one_in_flight_decode() {
    let project = fixture();
    let store = TempDir::new().expect("store root");
    let scoped_root = {
        let mut scheduler = open(project.path(), store.path());
        let _ = publish(&mut scheduler);
        store.path().to_path_buf()
    };

    // A fresh publication store is the cold state the daemon starts from: the
    // sealed pointer exists on disk and nothing is decoded yet.
    let publication = publication_store(&scoped_root);
    assert_eq!(publication.sealed_decode_count(), 0);

    let readers = 8;
    let barrier = Arc::new(std::sync::Barrier::new(readers));
    let loaded = std::thread::scope(|scope| {
        let handles = (0..readers)
            .map(|_| {
                let publication = publication.clone();
                let barrier = Arc::clone(&barrier);
                scope.spawn(move || {
                    barrier.wait();
                    publication
                        .load_active_shared()
                        .expect("load active generation")
                        .expect("active generation is present")
                })
            })
            .collect::<Vec<_>>();
        handles
            .into_iter()
            .map(|handle| handle.join().expect("reader thread"))
            .collect::<Vec<_>>()
    });

    assert_eq!(
        publication.sealed_decode_count(),
        1,
        "concurrent cold readers must share one decode instead of duplicating it"
    );
    let first = &loaded[0];
    assert!(
        loaded
            .iter()
            .all(|generation| Arc::ptr_eq(generation, first)),
        "every parked reader must be handed the same decoded generation"
    );
}

/// The decode-free admission is what lets a query with something servable in
/// hand resolve freshness without ever entering the O(store) decode. It must
/// abstain while nothing is decoded, never read sealed bytes, and serve the
/// identical shared allocation once the generation is warm.
#[test]
fn the_decode_free_admission_abstains_instead_of_reading_sealed_bytes() {
    let project = fixture();
    let store = TempDir::new().expect("store root");
    {
        let mut scheduler = open(project.path(), store.path());
        let _ = publish(&mut scheduler);
    }

    let publication = publication_store(store.path());
    assert_eq!(publication.sealed_decode_count(), 0);
    assert!(
        publication
            .active_already_decoded()
            .expect("decode-free probe")
            .is_none(),
        "nothing is decoded yet, so the decode-free admission abstains"
    );
    assert_eq!(
        publication.sealed_decode_count(),
        0,
        "the decode-free admission must never read or decode sealed bytes"
    );

    // Nothing servable: the awaiting admission still pays the one decode.
    let active = publication
        .load_active_shared()
        .expect("load active generation")
        .expect("active generation is present");
    assert_eq!(publication.sealed_decode_count(), 1);

    // Warm: the probe now serves, and serves the same allocation.
    let probed = publication
        .active_already_decoded()
        .expect("decode-free probe")
        .expect("warm active generation");
    assert!(
        Arc::ptr_eq(&active, &probed),
        "the decode-free admission must serve the one decoded generation"
    );
    assert_eq!(
        publication.sealed_decode_count(),
        1,
        "probing a warm generation must not re-decode"
    );
}

/// The other side of the same rule: when nothing is servable, awaiting the
/// in-flight decode is still correct and still single-flight. A reader that
/// arrives mid-decode parks and is handed the generation the holder installs,
/// without starting a second sweep.
#[test]
fn the_awaiting_admission_still_joins_an_in_flight_decode() {
    let project = fixture();
    let store = TempDir::new().expect("store root");
    {
        let mut scheduler = open(project.path(), store.path());
        let _ = publish(&mut scheduler);
    }

    let publication = publication_store(store.path());
    let expected = publication
        .load_active_shared()
        .expect("load active generation")
        .expect("active generation is present");
    let decodes = publication.sealed_decode_count();

    // Occupy the barrier as an activation decode does.
    let held = publication.hold_active_decode();
    assert!(
        publication
            .active_already_decoded()
            .expect("decode-free probe")
            .is_none(),
        "the decode-free admission abstains for the whole activation window"
    );

    let served = std::thread::scope(|scope| {
        let reader = scope.spawn(|| {
            publication
                .load_active_shared()
                .expect("load active generation")
                .expect("active generation is present")
        });
        std::thread::sleep(std::time::Duration::from_millis(200));
        drop(held);
        reader.join().expect("parked reader")
    });

    assert!(
        Arc::ptr_eq(&served, &expected),
        "a parked reader must be handed the generation the in-flight decode installs"
    );
    assert_eq!(
        publication.sealed_decode_count(),
        decodes,
        "parking must join the in-flight decode, never start a second sweep"
    );
}

/// Pinned and cursor-paged reads churn superseded generations through the LRU.
/// The active generation lives in its own pinned slot, so that churn must never
/// evict it and force a re-decode on an unpinned query.
#[test]
fn superseded_generation_churn_never_evicts_the_pinned_active_generation() {
    let project = fixture();
    let store = TempDir::new().expect("store root");
    // One more revision than the decoded-generation LRU can hold, so the
    // superseded set alone would overflow it.
    let revisions = super::DECODED_GENERATION_CACHE_CAPACITY + 2;
    let mut superseded = Vec::with_capacity(revisions);
    {
        let mut scheduler = open(project.path(), store.path());
        for revision in 0..revisions {
            write(project.path(), "src/lib.rs", revision);
            superseded.push(publish(&mut scheduler));
        }
    }

    let scheduler = open(project.path(), store.path());
    scheduler.prime_serving_caches();
    let active = scheduler
        .latest_complete()
        .expect("restored generation")
        .generation()
        .manifest()
        .generation_id
        .clone();
    let after_activation = scheduler.sealed_decode_count();
    assert_eq!(after_activation, 1);

    // Read every superseded generation, overflowing the LRU several times over.
    let mut pinned_decodes = 0;
    for generation_id in superseded.iter().filter(|id| **id != active) {
        let before = scheduler.sealed_decode_count();
        scheduler
            .generation(generation_id)
            .expect("pinned generation read")
            .expect("pinned generation is present");
        assert!(
            scheduler.sealed_decode_count() > before,
            "a cold pinned generation is decoded on first read"
        );
        pinned_decodes += scheduler.sealed_decode_count() - before;
        // Re-reading the same pinned generation is served from the LRU.
        let warm = scheduler.sealed_decode_count();
        scheduler
            .generation(generation_id)
            .expect("repeat pinned read")
            .expect("pinned generation is present");
        assert_eq!(
            scheduler.sealed_decode_count(),
            warm,
            "a decoded pinned generation must be served from the cache"
        );
    }
    assert!(pinned_decodes > 0, "the fixture must exercise pinned reads");

    let served = scheduler
        .latest_complete()
        .expect("active generation after churn");
    assert_eq!(served.generation().manifest().generation_id, active);
    assert!(!served.exact().expect("exact admission").is_empty());
    assert_eq!(
        scheduler.sealed_decode_count(),
        after_activation + pinned_decodes,
        "LRU pressure from superseded generations must never evict the active one"
    );
}

/// The amortization above is only sound because nothing is memoized on failure.
/// A corrupt sealed store must fail the first request and every request after
/// it, with the full check re-run each time.
#[test]
fn a_corrupt_sealed_generation_fails_closed_on_every_request() {
    let project = fixture();
    let store = TempDir::new().expect("store root");
    {
        let mut scheduler = open(project.path(), store.path());
        let _ = publish(&mut scheduler);
    }

    // Tamper with the sealed payload and restamp the durable pointer digest so
    // the corruption is caught by the generation's own canonical state digest
    // rather than by the pointer check in front of it.
    let generations_root = store.path().join("code-generations-v1");
    let sealed_path = fs::read_dir(&generations_root)
        .expect("read generations root")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("generation-") && name.ends_with(".json"))
        })
        .expect("sealed generation file");
    let sealed = fs::read_to_string(&sealed_path).expect("read sealed generation");
    let corrupted = sealed.replace("activation_revision", "activation_tampered");
    assert_ne!(corrupted, sealed, "the fixture must actually be tampered");
    fs::write(&sealed_path, &corrupted).expect("write corrupted generation");

    let pointer_path = store.path().join("active-code-generation-v1.json");
    let mut pointer: serde_json::Value =
        serde_json::from_slice(&fs::read(&pointer_path).expect("read pointer"))
            .expect("parse pointer");
    pointer["state_digest"] = serde_json::Value::String(format!(
        "sha256:{}",
        super::sha256_hex(corrupted.as_bytes())
    ));
    fs::write(
        &pointer_path,
        serde_json::to_vec(&pointer).expect("encode pointer"),
    )
    .expect("write pointer");

    let publication = publication_store(store.path());
    let first = publication.load_active_shared();
    assert!(first.is_err(), "a corrupt generation must not serve");
    let decodes_after_first = publication.sealed_decode_count();
    assert_eq!(
        decodes_after_first, 1,
        "the failing request must have run the real decode"
    );

    let second = publication.load_active_shared();
    assert!(
        second.is_err(),
        "a failed decode must never be memoized as a served generation"
    );
    assert_eq!(
        publication.sealed_decode_count(),
        2,
        "the next request must repeat the full check rather than trust a verdict"
    );
}
