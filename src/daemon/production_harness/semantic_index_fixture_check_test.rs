//! Isolated semantic embed/index fixture check.
//!
//! Copies the demo codebase in `tests/fixtures/semantic_index` into a
//! throwaway checkout, installs SHA-256-verified local model bytes into an
//! isolated `TRACEDECAY_DATA_DIR`, and proves in-process FastEmbed embeds
//! and indexes it: a complete vector generation publishes while semantic
//! activation stays off (Plan 20 compare-and-swap after a passing Plan 15
//! evaluation) and exact/lexical/graph retrieval keep answering.
//!
//! Hermetic, with two truthful outcomes: pass from pinned local bytes, or a
//! `pending` line when the dedicated model cache has no verified bytes. The
//! model hub is never contacted; bytes that fail their pin are never used
//! and never re-fetched. See `tests/fixtures/semantic_index/README.md`.

#![cfg(feature = "semantic-fastembed")]

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde_json::json;
use sha2::{Digest, Sha256};

use super::journey_test_support::git;
use super::semantic_activation_journey_test::{
    installed_selection_material, seed_distribution_fixture, wait_for_semantic_generation,
};
use super::semantic_availability_journey_test::{
    answered, assert_lane_complete, assert_semantic_pending,
};
use super::*;

/// Distinctive symbol from `tests/fixtures/semantic_index/src/inventory.rs`.
const PROBE_SYMBOL: &str = "reserve_inventory_for_checkout";

fn model_cache_dir() -> PathBuf {
    std::env::var_os("TRACEDECAY_FASTEMBED_MODEL_CACHE")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            Path::new(env!("CARGO_MANIFEST_DIR")).join("target/fastembed-model-cache")
        })
}

/// A member is reusable only as a regular file whose length and SHA-256
/// match its catalog pin.
fn member_matches_pin(path: &Path, length: u64, sha256: &str) -> bool {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return false;
    };
    if !metadata.file_type().is_file() || metadata.len() != length {
        return false;
    }
    let Ok(mut file) = fs::File::open(path) else {
        return false;
    };
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 1024 * 1024];
    loop {
        match file.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => hasher.update(&buffer[..read]),
            Err(_) => return false,
        }
    }
    hex::encode(hasher.finalize()) == sha256
}

fn pending_reason(
    cache: &Path,
    model: &crate::semantic_code::CatalogedFastEmbedModelV1,
) -> Option<String> {
    model.members.iter().find_map(|(role, member)| {
        (!member_matches_pin(&cache.join(&member.path), member.length, &member.sha256)).then(
            || {
                format!(
                    "member '{role}' ({}) is absent or fails its SHA-256/length pin",
                    member.path
                )
            },
        )
    })
}

fn copy_fixture_tree(source: &Path, destination: &Path) {
    fs::create_dir_all(destination).expect("fixture checkout directory");
    for entry in fs::read_dir(source).expect("readable checked-in fixture tree") {
        let entry = entry.expect("fixture tree entry");
        let target = destination.join(entry.file_name());
        if entry.file_type().expect("fixture entry type").is_dir() {
            copy_fixture_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), &target).unwrap_or_else(|error| {
                panic!(
                    "failed to copy fixture member '{}': {error}",
                    entry.path().display()
                )
            });
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn isolated_fixture_repo_embeds_and_indexes_without_activation() {
    let fixture_source =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/semantic_index");
    let cache = model_cache_dir();
    let catalog = crate::semantic_code::production_fastembed_catalog();
    let model = catalog
        .get(crate::semantic_code::DEFAULT_FASTEMBED_MODEL_ID)
        .expect("production catalog contains the default model");
    if let Some(reason) = pending_reason(&cache, model) {
        eprintln!(
            "semantic index fixture check: pending — model cache '{}' has no verified bytes \
             ({reason}); the check never downloads. Warm the cache as documented in \
             tests/fixtures/semantic_index/README.md",
            cache.display()
        );
        return;
    }

    let live_profile = crate::config::user_data_dir();
    let _profile = crate::config::PinnedUserDataDir::new();
    assert_ne!(
        live_profile,
        crate::config::user_data_dir(),
        "the check must run against an isolated TRACEDECAY_DATA_DIR, never the live profile"
    );

    // Every member is a local cache hit, so acquisition resolves without the
    // hub; the production install path re-verifies each SHA-256 pin before
    // the atomic install.
    let lifecycle_root =
        crate::semantic_code::default_lifecycle_root().expect("isolated lifecycle root");
    let lifecycle =
        crate::semantic_code::shared_lifecycle_owner().expect("production lifecycle owner");
    seed_distribution_fixture(&lifecycle_root, &cache, &lifecycle);
    lifecycle
        .select_model(Some(crate::semantic_code::DEFAULT_FASTEMBED_MODEL_ID), true)
        .expect("select the default semantic model");
    lifecycle
        .acquire_blocking_for_tests()
        .expect("install the verified local model bytes");
    let (artifact_digest, _install_path) = installed_selection_material(&lifecycle);

    let isolation = tempfile::TempDir::new().expect("fixture isolation root");
    let project = isolation.path().join("project");
    copy_fixture_tree(&fixture_source, &project);
    git(&project, &["init", "--quiet"]);
    git(&project, &["add", "."]);
    git(
        &project,
        &[
            "-c",
            "user.name=TraceDecay Test",
            "-c",
            "user.email=tracedecay@example.invalid",
            "commit",
            "--quiet",
            "-m",
            "test: seed semantic index fixture",
        ],
    );

    let harness = ProductionProjectCompositionHarnessV1::open(isolation.path(), [project.clone()])
        .await
        .expect("production composition");
    let resources = harness.resources.as_ref().expect("live harness");
    let code_id = resources
        .invocation
        .code_index_schedulers
        .latest_generation_id(&project)
        .await
        .expect("published code generation");

    // Embed/index proof: a complete vector generation publishes for the
    // current code generation from the verified installed artifact.
    let (code, vector) = wait_for_semantic_generation(&harness, &project, &code_id).await;
    assert!(
        !vector.vectors().is_empty(),
        "indexing the fixture must embed at least one chunk"
    );
    assert_eq!(
        vector
            .embedding_key()
            .embedding_key()
            .model_artifact_digest
            .as_str(),
        format!("sha256:{artifact_digest}"),
        "the published vectors must be bound to the verified installed artifact"
    );

    // Callable is not activated: no evaluation ran and no Plan 20
    // compare-and-swap was issued.
    let runtime_state = answered(
        &harness,
        &project,
        "tracedecay_runtime",
        json!({"format": "json"}),
    )
    .await["semantic_runtime"]
        .clone();
    assert_ne!(
        runtime_state["state"],
        json!("ready"),
        "an embed/index proof must not activate semantic retrieval: {runtime_state}"
    );

    let core = answered(
        &harness,
        &project,
        "tracedecay_search",
        json!({"query": PROBE_SYMBOL, "limit": 10, "format": "json"}),
    )
    .await;
    assert_semantic_pending(&core);
    assert!(
        core["results"]
            .as_array()
            .is_some_and(|results| !results.is_empty()),
        "exact/lexical/graph fusion must answer with semantic unactivated: {core}"
    );
    assert_eq!(core["code_generation"], json!(code.manifest().generation_id));
    for lane in ["exact", "lexical", "graph"] {
        assert_lane_complete(&core["coverage"], lane);
    }

    // Strict-semantic requests fail closed before activation as a typed tool
    // failure, so decode the payload directly.
    let strict_response = harness
        .call_tool(
            &project,
            "tracedecay_search",
            json!({
                "query": PROBE_SYMBOL,
                "limit": 10,
                "format": "json",
                "semantic_mode": "strict_semantic"
            }),
        )
        .await
        .expect("strict semantic search answers with a typed payload");
    assert!(
        strict_response.error.is_none(),
        "strict semantic unavailability is a typed result, not a transport error: \
         {strict_response:?}"
    );
    let strict_result = strict_response.result.as_ref().expect("strict tool result");
    let strict: serde_json::Value = serde_json::from_str(
        strict_result["content"][0]["text"]
            .as_str()
            .expect("strict tool text"),
    )
    .expect("strict tool payload is JSON");
    assert_eq!(
        strict["status"],
        json!("unavailable"),
        "strict semantic must stay typed-unavailable without activation: {strict}"
    );
    // A current vector generation without an accepted calibration authority
    // is exactly the "callable, not activated" state this check proves.
    assert_eq!(
        strict["semantic"]["reason"],
        json!("calibration_unavailable"),
        "strict unavailability must name the missing activation authority: {strict}"
    );

    harness.shutdown().await;

    assert!(
        !fixture_source.join(".tracedecay").exists() && !fixture_source.join(".git").exists(),
        "the check must not create repository or enrollment state in the source tree"
    );
}
