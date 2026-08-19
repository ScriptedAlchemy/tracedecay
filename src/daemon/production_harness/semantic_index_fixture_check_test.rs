//! Isolated semantic embed/index fixture check.
//!
//! Proves the callable Plan 31 machinery — SHA-256-verified local model
//! bytes, in-process FastEmbed sessions, chunk projection, and atomic vector
//! generation publication — against the small checked-in demo codebase in
//! `tests/fixtures/semantic_index`, inside one isolated
//! `TRACEDECAY_DATA_DIR`. The live `~/.tracedecay` profile is never read or
//! written, and no `.tracedecay/` directory is created in this repository's
//! working tree.
//!
//! The check is hermetic with two truthful outcomes:
//!
//! - **pass** from pinned local bytes: every catalog member under the
//!   dedicated model cache matches its SHA-256 and length pin, the fixture
//!   embeds and indexes, and a complete vector generation publishes.
//! - **pending** when any member is absent or fails its pin: the check
//!   prints a `pending` line and returns. It never contacts the model hub —
//!   the seeded lifecycle cache satisfies every member before acquisition
//!   runs, and mismatched bytes are discarded, not re-downloaded.
//!
//! Callable is not activated: a successful embed/index run grants no
//! semantic activation. Activation stays the Plan 20 compare-and-swap after
//! a passing Plan 15 evaluation, so this check asserts the semantic runtime
//! is not `ready`, strict-semantic requests report typed unavailability, and
//! exact/lexical/graph retrieval answer normally throughout.

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

/// Dedicated, reusable model-byte cache for this check. Overridable so CI
/// can point it at a restored cache volume; the default sits under the
/// gitignored `target/` so warm local runs skip the 641 MB copy source.
const MODEL_CACHE_ENV: &str = "TRACEDECAY_FASTEMBED_MODEL_CACHE";
const DEFAULT_MODEL_CACHE: &str = "target/fastembed-model-cache";

fn repository_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn model_cache_dir() -> PathBuf {
    std::env::var_os(MODEL_CACHE_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|| repository_root().join(DEFAULT_MODEL_CACHE))
}

fn streamed_sha256_hex(path: &Path) -> Option<String> {
    let mut file = fs::File::open(path).ok()?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 1024 * 1024];
    loop {
        let read = file.read(&mut buffer).ok()?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Some(hex::encode(hasher.finalize()))
}

/// Every catalog member must be a regular file whose length and SHA-256
/// match the production pins. Anything else is a `pending` reason; bytes
/// that fail the pin are never used and never re-fetched.
fn pending_reason_for_cache(
    cache: &Path,
    model: &crate::semantic_code::CatalogedFastEmbedModelV1,
) -> Option<String> {
    for (role, member) in &model.members {
        let path = cache.join(&member.path);
        let Ok(metadata) = fs::symlink_metadata(&path) else {
            return Some(format!("member '{role}' ({}) is absent", member.path));
        };
        if !metadata.file_type().is_file() {
            return Some(format!(
                "member '{role}' ({}) is not a regular file",
                member.path
            ));
        }
        if metadata.len() != member.length {
            return Some(format!(
                "member '{role}' ({}) is {} bytes, pinned length is {}",
                member.path,
                metadata.len(),
                member.length
            ));
        }
        match streamed_sha256_hex(&path) {
            Some(digest) if digest == member.sha256 => {}
            Some(_) => {
                return Some(format!(
                    "member '{role}' ({}) does not match its SHA-256 pin",
                    member.path
                ));
            }
            None => {
                return Some(format!(
                    "member '{role}' ({}) cannot be read",
                    member.path
                ));
            }
        }
    }
    None
}

/// Copies the checked-in demo tree into the throwaway checkout. The source
/// is read-only for this check and must never carry repository or
/// enrollment state of its own.
fn copy_fixture_tree(source: &Path, destination: &Path) {
    fs::create_dir_all(destination).expect("fixture checkout directory");
    for entry in fs::read_dir(source).expect("readable checked-in fixture tree") {
        let entry = entry.expect("fixture tree entry");
        let name = entry.file_name();
        assert!(
            name != ".tracedecay" && name != ".git",
            "the checked-in fixture tree must not carry repository or enrollment state: {}",
            entry.path().display()
        );
        let target = destination.join(&name);
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
    let fixture_source = repository_root().join("tests/fixtures/semantic_index");
    assert!(
        fixture_source.join("src/inventory.rs").is_file(),
        "the checked-in demo fixture codebase is missing: {}",
        fixture_source.display()
    );

    let cache = model_cache_dir();
    let catalog = crate::semantic_code::production_fastembed_catalog();
    let model = catalog
        .get(crate::semantic_code::DEFAULT_FASTEMBED_MODEL_ID)
        .expect("production catalog contains the default model");
    if let Some(reason) = pending_reason_for_cache(&cache, model) {
        eprintln!(
            "semantic index fixture check: pending — model cache '{}' has no verified bytes \
             ({reason}); the check never downloads. Warm the cache as documented in \
             tests/fixtures/semantic_index/README.md",
            cache.display()
        );
        return;
    }

    // The pin flips every storage-affecting env var (data dir, HOME) to a
    // throwaway directory while holding the process-wide env lock, so the
    // live profile cannot be resolved anywhere below.
    let live_profile = crate::config::user_data_dir();
    let _profile = crate::config::PinnedUserDataDir::new();
    let pinned_profile = crate::config::user_data_dir().expect("pinned isolated data dir");
    assert_ne!(
        live_profile.as_ref(),
        Some(&pinned_profile),
        "the check must run against an isolated TRACEDECAY_DATA_DIR, never the live profile"
    );
    let lifecycle_root =
        crate::semantic_code::default_lifecycle_root().expect("isolated lifecycle root");
    assert!(
        lifecycle_root.starts_with(&pinned_profile),
        "the model lifecycle must live inside the isolated profile: {}",
        lifecycle_root.display()
    );

    // Seed the isolated lifecycle cache from the verified local bytes. Every
    // member is a local cache hit, so acquisition resolves without the hub;
    // the production install path then re-verifies each SHA-256 pin before
    // the atomic install.
    let lifecycle =
        crate::semantic_code::shared_lifecycle_owner().expect("production lifecycle owner");
    seed_distribution_fixture(&lifecycle_root, &cache, &lifecycle);
    lifecycle
        .select_model(Some(crate::semantic_code::DEFAULT_FASTEMBED_MODEL_ID), true)
        .expect("select the default semantic model");
    lifecycle
        .acquire_blocking_for_tests()
        .expect("install the verified local model bytes");
    let (artifact_digest, install_path) = installed_selection_material(&lifecycle);
    assert!(
        install_path.starts_with(&lifecycle_root),
        "the verified install must stay inside the isolated lifecycle root: {}",
        install_path.display()
    );

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
    // current code generation, produced by in-process FastEmbed from the
    // exact verified artifact.
    let (code, vector) = wait_for_semantic_generation(&harness, &project, &code_id).await;
    assert!(
        !vector.vectors().is_empty(),
        "indexing the fixture must embed at least one chunk"
    );
    assert_eq!(
        vector.embedding_key().embedding_key().model_artifact_digest.as_str(),
        format!("sha256:{artifact_digest}"),
        "the published vectors must be bound to the verified installed artifact"
    );
    assert_eq!(vector.source_generation(), &code_id);

    // Callable is not activated: no evaluation ran and no Plan 20
    // compare-and-swap was issued, so the runtime must not be ready and
    // strict semantic requests must stay typed-unavailable.
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

    let strict = answered(
        &harness,
        &project,
        "tracedecay_search",
        json!({
            "query": PROBE_SYMBOL,
            "limit": 10,
            "format": "json",
            "semantic_mode": "strict_semantic"
        }),
    )
    .await;
    assert_eq!(
        strict["status"],
        json!("unavailable"),
        "strict semantic must stay typed-unavailable without activation: {strict}"
    );
    assert_ne!(strict["semantic"]["status"], json!("complete"));

    let lexical = answered(
        &harness,
        &project,
        "tracedecay_grep",
        json!({"pattern": PROBE_SYMBOL, "format": "json"}),
    )
    .await;
    assert!(
        lexical["match_count"].as_u64().is_some_and(|count| count > 0),
        "lexical retrieval must answer non-vacuously: {lexical}"
    );
    let graph = answered(
        &harness,
        &project,
        "tracedecay_body",
        json!({"symbol": PROBE_SYMBOL, "format": "json"}),
    )
    .await;
    assert!(
        graph["match_count"].as_u64().is_some_and(|count| count > 0),
        "graph retrieval must answer non-vacuously: {graph}"
    );

    harness.shutdown().await;

    // The checked-in fixture tree was only read; enrollment and repository
    // state belong exclusively to the throwaway checkout.
    assert!(
        !fixture_source.join(".tracedecay").exists() && !fixture_source.join(".git").exists(),
        "the check must not create repository or enrollment state in the source tree"
    );
}
